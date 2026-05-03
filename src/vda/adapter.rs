//! VDA adapter — `RoutePlan` / `RobotState` → VDA-shaped messages.

use uuid::Uuid;

use crate::claim::{ClaimManager, ClaimTargetKind};
use crate::core::error::Result;
use crate::index::WorkspaceIndex;
use crate::policy::{derive_effective_edge_semantics, parse_zone_policy};
use crate::robot::{RobotProgressState, RobotState};
use crate::route::{RoutePlan, validate_route_plan_shape};

use super::{
    ActionStatus, Connection, ConnectionStatus, Factsheet, InstantAction,
    OperatingMode, Order, OrderEdge, OrderNode, ConnectionState,
    ReservationState, ResourceReservation, Response, State,
};

fn uuid_string(id: Uuid) -> String { id.to_string() }

fn claim_target_kind_string(kind: ClaimTargetKind) -> &'static str {
    match kind {
        ClaimTargetKind::Zone => "zone",
        ClaimTargetKind::Node => "node",
        ClaimTargetKind::Edge => "edge",
    }
}

pub fn try_map_route_plan(route_plan: &RoutePlan) -> Result<Order> {
    validate_route_plan_shape(route_plan)?;

    let mut order = Order {
        header_id: uuid_string(route_plan.start_node_id),
        order_id: uuid_string(route_plan.goal_node_id),
        order_update_id: route_plan.traversed_edge_ids.len() as u32,
        ..Order::default()
    };

    for (i, &node_id) in route_plan.traversed_node_ids.iter().enumerate() {
        let mut node = OrderNode {
            node_id: uuid_string(node_id),
            sequence_id: i.to_string(),
            node_position_hint: Some(i.to_string()),
            ..OrderNode::default()
        };
        if i < route_plan.traversed_node_zone_ids.len() {
            for &zid in &route_plan.traversed_node_zone_ids[i] {
                node.reservations.push(ResourceReservation {
                    target_id: uuid_string(zid),
                    target_kind: "zone".into(),
                    ..ResourceReservation::default()
                });
            }
        }
        order.nodes.push(node);
    }

    for (i, &edge_id) in route_plan.traversed_edge_ids.iter().enumerate() {
        let mut edge = OrderEdge {
            edge_id: uuid_string(edge_id),
            start_node_id: uuid_string(route_plan.traversed_node_ids[i]),
            end_node_id: uuid_string(route_plan.traversed_node_ids[i + 1]),
            ..OrderEdge::default()
        };
        if i < route_plan.traversed_edge_zone_ids.len() {
            for &zid in &route_plan.traversed_edge_zone_ids[i] {
                edge.reservations.push(ResourceReservation {
                    target_id: uuid_string(zid),
                    target_kind: "zone".into(),
                    ..ResourceReservation::default()
                });
            }
        }
        order.edges.push(edge);
    }

    Ok(order)
}

pub fn try_map_route_plan_with_index(
    index: &WorkspaceIndex,
    route_plan: &RoutePlan,
) -> Result<Order> {
    let mut order = try_map_route_plan(route_plan)?;

    for (i, &node_id) in route_plan.traversed_node_ids.iter().enumerate() {
        if i >= order.nodes.len() { break; }
        let zones = index.zones_of_node(node_id);
        if let Some(first) = zones.first() {
            order.nodes[i].zone_id = Some(uuid_string(first.id()));
        }
        if let Some(node) = index.node(node_id) {
            order.nodes[i].node_position_hint =
                Some(format!("{},{}", node.position.x, node.position.y));
        }
    }

    for (i, &edge_id) in route_plan.traversed_edge_ids.iter().enumerate() {
        if i >= order.edges.len() { break; }
        let zones = index.zones_of_edge(edge_id);
        if let Some(first) = zones.first() {
            order.edges[i].zone_id = Some(uuid_string(first.id()));
        }
        if let Some(edge) = index.edge(edge_id) {
            let zone_policies: Vec<_> = zones.iter()
                .map(|z| parse_zone_policy(z.properties()))
                .collect();
            let semantics = derive_effective_edge_semantics(&edge.properties, false, &zone_policies);
            if let Some(s) = semantics.speed_limit { order.edges[i].max_speed = Some(s); }
            order.edges[i].bidirectional = !semantics.directed
                || semantics.reversible.unwrap_or(false);
            if semantics.requires_claim.unwrap_or(false) {
                order.edges[i].reservations.push(ResourceReservation {
                    target_id: uuid_string(edge.id),
                    target_kind: "edge".into(),
                    requires_claim: true,
                    access_group: semantics.access_group.clone(),
                    schedule_window: semantics.schedule_window.clone(),
                });
            }
            if semantics.access_group.is_some() {
                if let Some(last) = order.edges[i].reservations.last_mut() {
                    last.access_group = semantics.access_group.clone();
                }
            }
            if semantics.schedule_window.is_some() {
                if let Some(last) = order.edges[i].reservations.last_mut() {
                    last.schedule_window = semantics.schedule_window.clone();
                }
            }
        }
    }

    for (i, &node_id) in route_plan.traversed_node_ids.iter().enumerate() {
        if i >= order.nodes.len() { break; }
        for zone in index.zones_of_node(node_id) {
            let policy = parse_zone_policy(zone.properties());
            if policy.requires_claim
                || policy.access_group.is_some()
                || policy.schedule_window.is_some()
            {
                order.nodes[i].reservations.push(ResourceReservation {
                    target_id: uuid_string(zone.id()),
                    target_kind: "zone".into(),
                    requires_claim: policy.requires_claim,
                    access_group: policy.access_group.clone(),
                    schedule_window: policy.schedule_window.clone(),
                });
            }
        }
    }

    Ok(order)
}

pub fn map_route_plan(route_plan: &RoutePlan) -> Order {
    try_map_route_plan(route_plan).unwrap_or_default()
}

pub fn map_route_plan_with_index(
    index: &WorkspaceIndex,
    route_plan: &RoutePlan,
) -> Order {
    try_map_route_plan_with_index(index, route_plan).unwrap_or_default()
}

pub fn map_robot_state(state: &RobotState) -> State {
    let mut s = State {
        agv_id: state.robot_id.raw().to_string(),
        operating_mode: if state.route_plan.is_some() {
            OperatingMode::Automatic
        } else { OperatingMode::Manual },
        connection_state: ConnectionState::Online,
        ..State::default()
    };
    if let Some(n) = state.current_node_id { s.last_node_id = Some(uuid_string(n)); }
    if let Some(e) = state.current_edge_id { s.last_edge_id = Some(uuid_string(e)); }
    if let Some(plan) = state.route_plan.as_ref() {
        s.order_id = Some(uuid_string(plan.goal_node_id));
        s.order_update_id = plan.traversed_edge_ids.len() as u32;
    }
    s.driving_state = Some(
        if state.progress_state == RobotProgressState::FollowingRoute {
            "DRIVING"
        } else { "STOPPED" }
        .into()
    );
    s.paused = state.progress_state == RobotProgressState::Waiting
        || state.progress_state == RobotProgressState::Blocked;
    if !state.pending_claim_ids.is_empty() {
        s.errors.push("pending_claims".into());
        s.information.push("claims awaiting arbitration".into());
    }
    s
}

pub fn map_robot_state_with_claims(
    state: &RobotState,
    claim_manager: &ClaimManager,
) -> State {
    let mut s = map_robot_state(state);
    if !state.active_lease_ids.is_empty() {
        s.action_states.push("holding_leases".into());
    }
    for &lease_id in &state.active_lease_ids {
        let Some(lease) = claim_manager.find_lease(lease_id) else {
            s.errors.push("missing_lease".into());
            continue;
        };
        for target in &lease.targets {
            s.reservation_states.push(ReservationState {
                target_id: uuid_string(target.resource_id),
                target_kind: claim_target_kind_string(target.kind).into(),
                state: "ACTIVE".into(),
            });
        }
    }
    for &claim_id in &state.pending_claim_ids {
        if let Some(req) = claim_manager.find_request(claim_id) {
            for target in &req.targets {
                s.reservation_states.push(ReservationState {
                    target_id: uuid_string(target.resource_id),
                    target_kind: claim_target_kind_string(target.kind).into(),
                    state: "PENDING".into(),
                });
            }
        }
    }
    if let Some(reason) = state.hold_reason.as_ref() {
        s.action_states.push(reason.clone());
    }
    s
}

pub struct Adapter;

impl Adapter {
    pub fn new() -> Self { Self }

    pub fn order_from_route(&self, route_plan: &RoutePlan) -> Order {
        map_route_plan(route_plan)
    }

    pub fn order_from_route_with_index(
        &self, index: &WorkspaceIndex, route_plan: &RoutePlan,
    ) -> Order {
        map_route_plan_with_index(index, route_plan)
    }

    pub fn state_from_robot(&self, robot_state: &RobotState) -> State {
        map_robot_state(robot_state)
    }

    pub fn state_from_robot_with_claims(
        &self, robot_state: &RobotState, claim_manager: &ClaimManager,
    ) -> State {
        map_robot_state_with_claims(robot_state, claim_manager)
    }

    pub fn connection_from_factsheet(&self, factsheet: &Factsheet) -> Connection {
        Connection {
            manufacturer: factsheet.manufacturer.clone(),
            serial_number: factsheet.serial_number.clone(),
            version: factsheet.protocol_version.clone(),
            status: ConnectionStatus::Online,
            ..Connection::default()
        }
    }

    pub fn response_for_action(
        &self,
        action: &InstantAction,
        status: ActionStatus,
        description: Option<String>,
        result_code: Option<String>,
    ) -> Response {
        Response {
            action_id: action.action_id.clone(),
            status,
            description,
            result_code,
        }
    }
}

impl Default for Adapter {
    fn default() -> Self { Self::new() }
}
