//! Wire transport adapters for `timenav`.
//!
//! Each submodule is a thin adapter that translates an external wire
//! protocol (REST/JSON, REST/XML, Zenoh) into calls on the real timenav
//! core (`Coordinator`, `ClaimManager`, `plan_route`) and serialises core
//! results back to the wire.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::claim::{
    ClaimAccessMode, ClaimDecision, ClaimEvaluation, ClaimId, ClaimRequest, ClaimTarget,
    ClaimTargetKind, ClaimWindow, Lease, LeaseId, MissionId,
};
use crate::coordinator::{Coordinator, ScheduleDecision};
use crate::core::ids::RobotId;
use crate::index::{NUMERIC_ID_PROPERTY, ResourceRef, WorkspaceIndex};
use crate::robot::RobotState;
use crate::route::{RouteFailure, RoutePlan, plan_route};

#[cfg(feature = "rest")]
pub mod rest;

#[cfg(feature = "robo")]
pub mod robo;

#[cfg(feature = "xmlt")]
pub mod xmlt;

/// Shared state used by all serving adapters.
#[derive(Clone)]
pub struct ServeState {
    coordinator: Arc<RwLock<Coordinator>>,
}

impl ServeState {
    pub fn new(coordinator: Coordinator) -> Self {
        Self {
            coordinator: Arc::new(RwLock::new(coordinator)),
        }
    }

    pub fn shared(coordinator: Arc<RwLock<Coordinator>>) -> Self {
        Self { coordinator }
    }

    pub fn coordinator(&self) -> Arc<RwLock<Coordinator>> {
        Arc::clone(&self.coordinator)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub message: String,
}

impl ApiError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    pub status: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetSnapshot {
    pub robots: Vec<RobotState>,
    pub requests: Vec<ClaimRequest>,
    pub leases: Vec<Lease>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneView {
    pub id: uuid::Uuid,
    pub numeric_id: Option<u64>,
    pub name: String,
    pub kind: String,
    pub parent_id: Option<uuid::Uuid>,
    pub child_ids: Vec<uuid::Uuid>,
    pub node_ids: Vec<uuid::Uuid>,
    pub properties: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeView {
    pub id: uuid::Uuid,
    pub numeric_id: Option<u64>,
    pub name: String,
    pub position: zoneout::NodePosition,
    pub zone_ids: Vec<uuid::Uuid>,
    pub properties: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeView {
    pub id: uuid::Uuid,
    pub numeric_id: Option<u64>,
    pub source_node_id: uuid::Uuid,
    pub target_node_id: uuid::Uuid,
    pub directed: bool,
    pub weight: f64,
    pub zone_ids: Vec<uuid::Uuid>,
    pub properties: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRouteRequest {
    pub start_node_id: ResourceRef,
    pub goal_node_id: ResourceRef,
    #[serde(default)]
    pub use_penalties: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRouteResponse {
    pub found: bool,
    pub distance: f64,
    pub plan: Option<RoutePlan>,
    pub failure: Option<RouteFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatRequest {
    pub current_node_id: Option<ResourceRef>,
    pub current_edge_id: Option<ResourceRef>,
    pub updated_at_tick: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleRobotRouteRequest {
    pub claim_id: ClaimId,
    pub start_tick: u64,
    pub ticks_per_cost_unit: f64,
    pub access_mode: ClaimAccessMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignRouteRequest {
    pub route_plan: RoutePlan,
    pub horizon: u64,
    pub updated_at_tick: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseLeaseRequest {
    pub lease_id: LeaseId,
    pub released_at_tick: Option<u64>,
}

/// Wire form of [`ClaimTarget`] — `resource_id` accepts either a UUID
/// string or a numeric alias resolved against [`WorkspaceIndex`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ClaimTargetWire {
    pub kind: ClaimTargetKind,
    pub resource_id: ResourceRef,
}

impl ClaimTargetWire {
    pub fn into_target(self, idx: &WorkspaceIndex) -> ApiResult<ClaimTarget> {
        let resolved = match self.kind {
            ClaimTargetKind::Zone => self.resource_id.resolve_zone(idx),
            ClaimTargetKind::Node => self.resource_id.resolve_node(idx),
            ClaimTargetKind::Edge => self.resource_id.resolve_edge(idx),
        };
        let resource_id = resolved.ok_or_else(|| {
            ApiError::new(format!(
                "unknown {:?} resource id {:?}",
                self.kind, self.resource_id
            ))
        })?;
        Ok(ClaimTarget {
            kind: self.kind,
            resource_id,
        })
    }
}

/// Wire form of [`ClaimRequest`] — mirrors the core struct but accepts
/// `ResourceRef` for each target's `resource_id`. Convert with
/// [`ClaimRequestWire::into_request`] before handing to `ClaimManager`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ClaimRequestWire {
    pub id: ClaimId,
    pub robot_id: RobotId,
    pub mission_id: MissionId,
    pub access_mode: ClaimAccessMode,
    pub priority: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_at_tick: Option<u64>,
    pub window: ClaimWindow,
    pub targets: Vec<ClaimTargetWire>,
}

impl ClaimRequestWire {
    pub fn into_request(self, idx: &WorkspaceIndex) -> ApiResult<ClaimRequest> {
        let targets = self
            .targets
            .into_iter()
            .map(|t| t.into_target(idx))
            .collect::<ApiResult<Vec<_>>>()?;
        Ok(ClaimRequest {
            id: self.id,
            robot_id: self.robot_id,
            mission_id: self.mission_id,
            access_mode: self.access_mode,
            priority: self.priority,
            requested_at_tick: self.requested_at_tick,
            window: self.window,
            targets,
        })
    }
}

pub fn health() -> Health {
    Health {
        status: "ok".into(),
        version: crate::version().into(),
    }
}

pub fn fleet_snapshot(state: &ServeState) -> ApiResult<FleetSnapshot> {
    let coord = read_coord(state)?;
    Ok(FleetSnapshot {
        robots: coord.robot_states().to_vec(),
        requests: coord.claim_manager().requests().to_vec(),
        leases: coord.claim_manager().leases().to_vec(),
    })
}

pub fn list_zones(state: &ServeState) -> ApiResult<Vec<ZoneView>> {
    let coord = read_coord(state)?;
    let idx = coord
        .index()
        .ok_or_else(|| ApiError::new("coordinator has no WorkspaceIndex bound"))?;
    let mut zones = Vec::new();
    let root_id = idx
        .root_zone_id()
        .ok_or_else(|| ApiError::new("workspace has no root zone"))?;
    let root = idx
        .zone(root_id)
        .ok_or_else(|| ApiError::new("root zone is missing from index"))?;
    zones.push(zone_view(idx, root));
    for zone in idx.descendant_zones(root_id) {
        zones.push(zone_view(idx, zone));
    }
    Ok(zones)
}

pub fn find_zone(state: &ServeState, id: ResourceRef) -> ApiResult<ZoneView> {
    let coord = read_coord(state)?;
    let idx = coord
        .index()
        .ok_or_else(|| ApiError::new("coordinator has no WorkspaceIndex bound"))?;
    let zone_id = id
        .resolve_zone(idx)
        .ok_or_else(|| ApiError::new(format!("unknown zone id {:?}", id)))?;
    let zone = idx
        .zone(zone_id)
        .ok_or_else(|| ApiError::new(format!("unknown zone id {zone_id}")))?;
    Ok(zone_view(idx, zone))
}

pub fn list_nodes(state: &ServeState) -> ApiResult<Vec<NodeView>> {
    let coord = read_coord(state)?;
    let idx = coord
        .index()
        .ok_or_else(|| ApiError::new("coordinator has no WorkspaceIndex bound"))?;
    let graph = idx.workspace().graph();
    Ok(graph
        .vertices()
        .into_iter()
        .filter_map(|vid| graph.get_vertex(vid))
        .map(node_view)
        .collect())
}

pub fn find_node(state: &ServeState, id: ResourceRef) -> ApiResult<NodeView> {
    let coord = read_coord(state)?;
    let idx = coord
        .index()
        .ok_or_else(|| ApiError::new("coordinator has no WorkspaceIndex bound"))?;
    let node_id = id
        .resolve_node(idx)
        .ok_or_else(|| ApiError::new(format!("unknown node id {:?}", id)))?;
    let node = idx
        .node(node_id)
        .ok_or_else(|| ApiError::new(format!("unknown node id {node_id}")))?;
    Ok(node_view(node))
}

pub fn list_edges(state: &ServeState) -> ApiResult<Vec<EdgeView>> {
    let coord = read_coord(state)?;
    let idx = coord
        .index()
        .ok_or_else(|| ApiError::new("coordinator has no WorkspaceIndex bound"))?;
    let graph = idx.workspace().graph();
    let mut edges = Vec::new();
    for edge in graph.edges() {
        let Some(data) = graph.edge_property(edge.id) else {
            continue;
        };
        let Some(source) = graph.source(edge.id).and_then(|vid| graph.get_vertex(vid)) else {
            continue;
        };
        let Some(target) = graph.target(edge.id).and_then(|vid| graph.get_vertex(vid)) else {
            continue;
        };
        edges.push(edge_view(
            data,
            source.id,
            target.id,
            matches!(
                graph.get_edge_type(edge.id),
                Some(graphix::vertex::EdgeType::Directed)
            ),
            graph.get_weight(edge.id).unwrap_or(edge.weight),
        ));
    }
    Ok(edges)
}

pub fn find_edge(state: &ServeState, id: ResourceRef) -> ApiResult<EdgeView> {
    let coord = read_coord(state)?;
    let idx = coord
        .index()
        .ok_or_else(|| ApiError::new("coordinator has no WorkspaceIndex bound"))?;
    let edge_uuid = id
        .resolve_edge(idx)
        .ok_or_else(|| ApiError::new(format!("unknown edge id {:?}", id)))?;
    let edge_id = idx
        .edge_id(edge_uuid)
        .ok_or_else(|| ApiError::new(format!("unknown edge id {edge_uuid}")))?;
    let graph = idx.workspace().graph();
    let data = graph
        .edge_property(edge_id)
        .ok_or_else(|| ApiError::new(format!("unknown edge id {edge_uuid}")))?;
    let source = graph
        .source(edge_id)
        .and_then(|vid| graph.get_vertex(vid))
        .ok_or_else(|| ApiError::new(format!("edge {edge_uuid} has no source node")))?;
    let target = graph
        .target(edge_id)
        .and_then(|vid| graph.get_vertex(vid))
        .ok_or_else(|| ApiError::new(format!("edge {edge_uuid} has no target node")))?;
    Ok(edge_view(
        data,
        source.id,
        target.id,
        matches!(
            graph.get_edge_type(edge_id),
            Some(graphix::vertex::EdgeType::Directed)
        ),
        graph.get_weight(edge_id).unwrap_or_default(),
    ))
}

pub fn register_robot(state: &ServeState, robot: RobotState) -> ApiResult<RobotState> {
    let mut coord = write_coord(state)?;
    coord.register_robot(robot.clone());
    Ok(robot)
}

pub fn unregister_robot(state: &ServeState, robot_id: RobotId) -> ApiResult<bool> {
    Ok(write_coord(state)?.unregister_robot(robot_id))
}

pub fn robot_state(state: &ServeState, robot_id: RobotId) -> ApiResult<RobotState> {
    read_coord(state)?
        .find_robot_state(robot_id)
        .cloned()
        .ok_or_else(|| ApiError::new(format!("robot {robot_id} is not registered")))
}

pub fn list_robots(state: &ServeState) -> ApiResult<Vec<RobotState>> {
    Ok(read_coord(state)?.robot_states().to_vec())
}

pub fn heartbeat(
    state: &ServeState,
    robot_id: RobotId,
    request: HeartbeatRequest,
) -> ApiResult<RobotState> {
    let mut coord = write_coord(state)?;
    let (current_node_id, current_edge_id) = {
        let idx = coord
            .index()
            .ok_or_else(|| ApiError::new("coordinator has no WorkspaceIndex bound"))?;
        let node = match request.current_node_id {
            Some(r) => Some(
                r.resolve_node(idx)
                    .ok_or_else(|| ApiError::new(format!("unknown node id {:?}", r)))?,
            ),
            None => None,
        };
        let edge = match request.current_edge_id {
            Some(r) => Some(
                r.resolve_edge(idx)
                    .ok_or_else(|| ApiError::new(format!("unknown edge id {:?}", r)))?,
            ),
            None => None,
        };
        (node, edge)
    };
    if !coord.update_robot_progress(
        robot_id,
        current_node_id,
        current_edge_id,
        request.updated_at_tick,
    ) {
        return Err(ApiError::new(format!("robot {robot_id} is not registered")));
    }
    coord
        .find_robot_state(robot_id)
        .cloned()
        .ok_or_else(|| ApiError::new(format!("robot {robot_id} is not registered")))
}

pub fn assign_route(
    state: &ServeState,
    robot_id: RobotId,
    request: AssignRouteRequest,
) -> ApiResult<RobotState> {
    let mut coord = write_coord(state)?;
    if !coord.assign_route_plan(
        robot_id,
        request.route_plan,
        request.horizon,
        request.updated_at_tick,
    ) {
        return Err(ApiError::new(format!("robot {robot_id} is not registered")));
    }
    coord
        .find_robot_state(robot_id)
        .cloned()
        .ok_or_else(|| ApiError::new(format!("robot {robot_id} is not registered")))
}

pub fn schedule_robot_route(
    state: &ServeState,
    robot_id: RobotId,
    request: ScheduleRobotRouteRequest,
) -> ApiResult<ScheduleDecision> {
    let mut coord = write_coord(state)?;
    if coord.find_robot_state(robot_id).is_none() {
        return Err(ApiError::new(format!("robot {robot_id} is not registered")));
    }
    Ok(coord.schedule_robot_route(
        robot_id,
        request.claim_id,
        request.start_tick,
        request.ticks_per_cost_unit,
        request.access_mode,
    ))
}

pub fn plan_route_request(
    state: &ServeState,
    request: PlanRouteRequest,
) -> ApiResult<PlanRouteResponse> {
    let coord = read_coord(state)?;
    let index = coord
        .index()
        .ok_or_else(|| ApiError::new("coordinator has no WorkspaceIndex bound"))?;
    let start = request.start_node_id.resolve_node(index).ok_or_else(|| {
        ApiError::new(format!("unknown start node id {:?}", request.start_node_id))
    })?;
    let goal = request
        .goal_node_id
        .resolve_node(index)
        .ok_or_else(|| ApiError::new(format!("unknown goal node id {:?}", request.goal_node_id)))?;
    let result = plan_route(index, start, goal, request.use_penalties);
    Ok(PlanRouteResponse {
        found: result.search.found,
        distance: result.search.distance,
        plan: result.plan,
        failure: result.failure,
    })
}

pub fn list_claims(state: &ServeState) -> ApiResult<Vec<ClaimRequest>> {
    Ok(read_coord(state)?.claim_manager().requests().to_vec())
}

pub fn find_claim(state: &ServeState, claim_id: ClaimId) -> ApiResult<ClaimRequest> {
    read_coord(state)?
        .claim_manager()
        .requests()
        .iter()
        .find(|request| request.id == claim_id)
        .cloned()
        .ok_or_else(|| ApiError::new(format!("claim {claim_id} is not active")))
}

pub fn remove_claim(state: &ServeState, claim_id: ClaimId) -> ApiResult<bool> {
    Ok(write_coord(state)?
        .claim_manager_mut()
        .remove_request(claim_id))
}

pub fn evaluate_claim(state: &ServeState, request: ClaimRequestWire) -> ApiResult<ClaimEvaluation> {
    let coord = read_coord(state)?;
    let idx = coord
        .index()
        .ok_or_else(|| ApiError::new("coordinator has no WorkspaceIndex bound"))?;
    let resolved = request.into_request(idx)?;
    Ok(coord.claim_manager().evaluate_request(&resolved))
}

pub fn submit_claim(state: &ServeState, request: ClaimRequestWire) -> ApiResult<ClaimEvaluation> {
    let mut coord = write_coord(state)?;
    let resolved = {
        let idx = coord
            .index()
            .ok_or_else(|| ApiError::new("coordinator has no WorkspaceIndex bound"))?;
        request.into_request(idx)?
    };
    let evaluation = coord.claim_manager().evaluate_request(&resolved);
    if evaluation.decision == ClaimDecision::Grant {
        coord.claim_manager_mut().add_request(resolved);
    }
    Ok(evaluation)
}

pub fn list_leases(state: &ServeState) -> ApiResult<Vec<Lease>> {
    Ok(read_coord(state)?.claim_manager().leases().to_vec())
}

pub fn add_lease(state: &ServeState, lease: Lease) -> ApiResult<Lease> {
    let mut coord = write_coord(state)?;
    coord.claim_manager_mut().add_lease(lease.clone());
    Ok(lease)
}

pub fn release_lease(state: &ServeState, request: ReleaseLeaseRequest) -> ApiResult<bool> {
    Ok(write_coord(state)?
        .claim_manager_mut()
        .release_lease(request.lease_id, request.released_at_tick))
}

fn read_coord(state: &ServeState) -> ApiResult<std::sync::RwLockReadGuard<'_, Coordinator>> {
    state
        .coordinator
        .read()
        .map_err(|_| ApiError::new("coordinator lock is poisoned"))
}

fn write_coord(state: &ServeState) -> ApiResult<std::sync::RwLockWriteGuard<'_, Coordinator>> {
    state
        .coordinator
        .write()
        .map_err(|_| ApiError::new("coordinator lock is poisoned"))
}

fn zone_view(idx: &WorkspaceIndex, zone: &zoneout::Zone) -> ZoneView {
    ZoneView {
        id: zone.id(),
        numeric_id: zone
            .property(NUMERIC_ID_PROPERTY)
            .and_then(|raw| raw.trim().parse::<u64>().ok()),
        name: zone.name().into(),
        kind: zone.kind().into(),
        parent_id: idx.parent_zone(zone.id()).map(|parent| parent.id()),
        child_ids: idx
            .child_zones(zone.id())
            .into_iter()
            .map(|child| child.id())
            .collect(),
        node_ids: zone.node_ids().to_vec(),
        properties: zone.properties().clone(),
    }
}

fn node_view(node: &zoneout::NodeData) -> NodeView {
    NodeView {
        id: node.id,
        numeric_id: numeric_id(&node.properties),
        name: node.name.clone(),
        position: node.position,
        zone_ids: node.zone_ids.clone(),
        properties: node.properties.clone(),
    }
}

fn edge_view(
    edge: &zoneout::EdgeData,
    source_node_id: uuid::Uuid,
    target_node_id: uuid::Uuid,
    directed: bool,
    weight: f64,
) -> EdgeView {
    EdgeView {
        id: edge.id,
        numeric_id: numeric_id(&edge.properties),
        source_node_id,
        target_node_id,
        directed,
        weight,
        zone_ids: edge.zone_ids.clone(),
        properties: edge.properties.clone(),
    }
}

fn numeric_id(properties: &BTreeMap<String, String>) -> Option<u64> {
    properties
        .get(NUMERIC_ID_PROPERTY)
        .and_then(|raw| raw.trim().parse::<u64>().ok())
}
