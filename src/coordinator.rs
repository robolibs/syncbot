//! Per-robot scheduling, arbitration, and rolling-horizon claim builder.
//!
//! Port of `include/syncbot/coordinator.hpp`. Free helpers map a `RoutePlan`
//! into time-windowed reservations and decide `Proceed | Queue | Replan`.
//! `Coordinator` ties everything together with a list of `RobotState`s plus
//! its own `ClaimManager`.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::claim::{
    ClaimAccessMode, ClaimId, ClaimManager, ClaimRequest, ClaimTarget, ClaimTargetKind,
    ClaimWindow, LeaseId,
};
use crate::core::ids::{MissionId, RobotId};
use crate::core::key::Key;
use crate::index::WorkspaceIndex;
use crate::policy::{ZonePolicyKind, derive_effective_edge_semantics, parse_zone_policy};
use crate::robot::{RobotProgressState, RobotState};
use crate::route::{RoutePlan, validate_route_plan_shape};

// ---------------------------------------------------------------------------
// target semantics
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClaimTargetSemantics {
    pub target: ClaimTarget,
    pub requires_claim: bool,
    pub waiting_allowed: bool,
    pub stop_allowed: bool,
    pub blocked: bool,
    pub corridor: bool,
    pub slowdown: bool,
    pub schedule_window: Option<String>,
    pub access_group: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScheduledTargetWindow {
    pub semantics: ClaimTargetSemantics,
    pub start_tick: u64,
    pub end_tick: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScheduleConflict {
    pub target: ClaimTarget,
    pub blocking_until_tick: u64,
    pub conflicting_claim_id: Option<ClaimId>,
    pub conflicting_lease_id: Option<LeaseId>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScheduleDecisionKind {
    Proceed,
    Queue,
    Replan,
}

impl Default for ScheduleDecisionKind {
    fn default() -> Self {
        Self::Proceed
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScheduleDecision {
    pub kind: ScheduleDecisionKind,
    pub start_tick: u64,
    pub queue_position: u64,
    pub conflicts: Vec<ScheduleConflict>,
    pub diagnostics: Vec<String>,
}

pub fn claim_target_semantics(index: &WorkspaceIndex, target: ClaimTarget) -> ClaimTargetSemantics {
    let mut semantics = ClaimTargetSemantics {
        target,
        waiting_allowed: true,
        stop_allowed: true,
        ..ClaimTargetSemantics::default()
    };

    if target.kind == ClaimTargetKind::Zone {
        let Some(zone) = index.zone(target.resource_id) else {
            return semantics;
        };
        let policy = parse_zone_policy(zone.properties());
        semantics.requires_claim = policy.requires_claim;
        semantics.waiting_allowed = policy.waiting_allowed.unwrap_or(true);
        semantics.stop_allowed = policy
            .stop_allowed
            .unwrap_or(!policy.blocked.unwrap_or(false));
        semantics.blocked = policy.blocked.unwrap_or(false)
            || policy.blocks_entry_without_grant
            || policy.blocks_traversal_without_grant;
        semantics.corridor = policy.kind == ZonePolicyKind::Corridor;
        semantics.slowdown = policy.kind == ZonePolicyKind::Slowdown
            || (policy.speed_limit.map(|v| v < 1.0).unwrap_or(false));
        semantics.schedule_window = policy.schedule_window;
        semantics.access_group = policy.access_group;
        return semantics;
    }

    if target.kind == ClaimTargetKind::Edge {
        let Some(edge) = index.edge(target.resource_id) else {
            return semantics;
        };
        let zone_policies: Vec<_> = index
            .zones_of_edge(target.resource_id)
            .into_iter()
            .map(|z| parse_zone_policy(z.properties()))
            .collect();
        let edge_semantics =
            derive_effective_edge_semantics(&edge.properties, false, &zone_policies);
        semantics.requires_claim = edge_semantics.requires_claim.unwrap_or(false);
        semantics.waiting_allowed = edge_semantics.waiting_allowed.unwrap_or(true);
        semantics.stop_allowed = edge_semantics
            .stop_allowed
            .unwrap_or(!edge_semantics.blocked.unwrap_or(false));
        semantics.blocked = edge_semantics.blocked.unwrap_or(false);
        semantics.corridor = edge_semantics.lane_type.as_deref() == Some("corridor");
        semantics.slowdown = edge_semantics.speed_limit.map(|v| v < 1.0).unwrap_or(false);
        semantics.schedule_window = edge_semantics.schedule_window;
        semantics.access_group = edge_semantics.access_group;
        return semantics;
    }

    if target.kind == ClaimTargetKind::Node {
        for zone in index.zones_of_node(target.resource_id) {
            let policy = parse_zone_policy(zone.properties());
            semantics.requires_claim = semantics.requires_claim || policy.requires_claim;
            semantics.waiting_allowed =
                semantics.waiting_allowed && policy.waiting_allowed.unwrap_or(true);
            semantics.stop_allowed = semantics.stop_allowed && policy.stop_allowed.unwrap_or(true);
            semantics.blocked = semantics.blocked
                || policy.blocked.unwrap_or(false)
                || policy.blocks_entry_without_grant
                || policy.blocks_traversal_without_grant;
            semantics.corridor = semantics.corridor || policy.kind == ZonePolicyKind::Corridor;
            semantics.slowdown = semantics.slowdown
                || policy.kind == ZonePolicyKind::Slowdown
                || policy.speed_limit.map(|v| v < 1.0).unwrap_or(false);
            if semantics.schedule_window.is_none() && policy.schedule_window.is_some() {
                semantics.schedule_window = policy.schedule_window;
            }
            if semantics.access_group.is_none() && policy.access_group.is_some() {
                semantics.access_group = policy.access_group;
            }
        }
    }
    semantics
}

pub fn scheduled_target_windows_from_route(
    index: &WorkspaceIndex,
    route_plan: &RoutePlan,
    start_tick: u64,
    ticks_per_cost_unit: f64,
) -> Vec<ScheduledTargetWindow> {
    let mut windows = Vec::new();
    if validate_route_plan_shape(route_plan).is_err() {
        return windows;
    }

    let tick_at_step = |step_index: usize| -> u64 {
        if step_index >= route_plan.steps.len() {
            return start_tick;
        }
        let offset_f = (route_plan.steps[step_index].cumulative_cost * ticks_per_cost_unit)
            .max(0.0)
            .ceil();
        start_tick + offset_f as u64
    };

    for (i, &node_id) in route_plan.traversed_node_ids.iter().enumerate() {
        let window_start = tick_at_step(i);
        let next_tick = if i + 1 < route_plan.steps.len() {
            tick_at_step(i + 1)
        } else {
            window_start
        };
        let window_end = window_start.max(next_tick);

        windows.push(ScheduledTargetWindow {
            semantics: claim_target_semantics(
                index,
                ClaimTarget {
                    kind: ClaimTargetKind::Node,
                    resource_id: node_id,
                },
            ),
            start_tick: window_start,
            end_tick: window_end,
        });

        if i < route_plan.traversed_node_zone_ids.len() {
            for &zone_id in &route_plan.traversed_node_zone_ids[i] {
                windows.push(ScheduledTargetWindow {
                    semantics: claim_target_semantics(
                        index,
                        ClaimTarget {
                            kind: ClaimTargetKind::Zone,
                            resource_id: zone_id,
                        },
                    ),
                    start_tick: window_start,
                    end_tick: window_end,
                });
            }
        }

        if i < route_plan.traversed_edge_ids.len() {
            let edge_window_end = window_start.max(tick_at_step(i + 1));
            let edge_id = route_plan.traversed_edge_ids[i];
            windows.push(ScheduledTargetWindow {
                semantics: claim_target_semantics(
                    index,
                    ClaimTarget {
                        kind: ClaimTargetKind::Edge,
                        resource_id: edge_id,
                    },
                ),
                start_tick: window_start,
                end_tick: edge_window_end,
            });
            if i < route_plan.traversed_edge_zone_ids.len() {
                for &zone_id in &route_plan.traversed_edge_zone_ids[i] {
                    windows.push(ScheduledTargetWindow {
                        semantics: claim_target_semantics(
                            index,
                            ClaimTarget {
                                kind: ClaimTargetKind::Zone,
                                resource_id: zone_id,
                            },
                        ),
                        start_tick: window_start,
                        end_tick: edge_window_end,
                    });
                }
            }
        }
    }
    windows
}

pub fn claim_target_windows_overlap(
    lhs: ClaimTarget,
    lhs_window: ClaimWindow,
    rhs: ClaimTarget,
    rhs_window: ClaimWindow,
    index: Option<&WorkspaceIndex>,
) -> bool {
    let lhs_start = lhs_window.start_tick.unwrap_or(0);
    let rhs_start = rhs_window.start_tick.unwrap_or(0);
    let lhs_end = lhs_window.end_tick.unwrap_or(u64::MAX);
    let rhs_end = rhs_window.end_tick.unwrap_or(u64::MAX);
    if !(lhs_start <= rhs_end && rhs_start <= lhs_end) {
        return false;
    }

    if lhs.kind != rhs.kind {
        return false;
    }
    if lhs.resource_id == rhs.resource_id {
        return true;
    }
    if lhs.kind != ClaimTargetKind::Zone {
        return false;
    }
    let Some(index) = index else {
        return false;
    };

    if index.zone(lhs.resource_id).is_some() {
        if index
            .ancestor_zones(lhs.resource_id)
            .iter()
            .any(|a| a.id() == rhs.resource_id)
        {
            return true;
        }
    }
    if index.zone(rhs.resource_id).is_some() {
        if index
            .ancestor_zones(rhs.resource_id)
            .iter()
            .any(|a| a.id() == lhs.resource_id)
        {
            return true;
        }
    }
    false
}

pub fn schedule_route_request(
    index: &WorkspaceIndex,
    claim_manager: &ClaimManager,
    request: &ClaimRequest,
    route_plan: &RoutePlan,
    start_tick: u64,
    ticks_per_cost_unit: f64,
) -> ScheduleDecision {
    let mut decision = ScheduleDecision {
        start_tick,
        ..ScheduleDecision::default()
    };
    let windows =
        scheduled_target_windows_from_route(index, route_plan, start_tick, ticks_per_cost_unit);
    let mut latest_blocking_tick = start_tick;

    for window in &windows {
        let requested_window = ClaimWindow {
            start_tick: Some(window.start_tick),
            end_tick: Some(window.end_tick),
        };

        for active in claim_manager.requests() {
            if active.id == request.id {
                continue;
            }
            for target in &active.targets {
                if !claim_target_windows_overlap(
                    window.semantics.target,
                    requested_window,
                    *target,
                    active.window,
                    claim_manager.index(),
                ) {
                    continue;
                }
                let blocking = active.window.end_tick.unwrap_or(window.end_tick);
                add_conflict(
                    &mut decision,
                    &mut latest_blocking_tick,
                    window.semantics.target,
                    blocking,
                    Some(active.id),
                    None,
                    &window.semantics,
                    "active request",
                );
                break;
            }
        }

        for lease in claim_manager.leases() {
            if !lease.active {
                continue;
            }
            let lease_window = ClaimWindow {
                start_tick: lease.granted_at_tick,
                end_tick: lease.expires_at_tick,
            };
            for target in &lease.targets {
                if !claim_target_windows_overlap(
                    window.semantics.target,
                    requested_window,
                    *target,
                    lease_window,
                    claim_manager.index(),
                ) {
                    continue;
                }
                let blocking = lease.expires_at_tick.unwrap_or(window.end_tick);
                add_conflict(
                    &mut decision,
                    &mut latest_blocking_tick,
                    window.semantics.target,
                    blocking,
                    None,
                    Some(lease.id),
                    &window.semantics,
                    "active lease",
                );
                break;
            }
        }
    }

    if decision.conflicts.is_empty() {
        decision.kind = ScheduleDecisionKind::Proceed;
        decision
            .diagnostics
            .push("route can proceed within the requested reservation window".into());
        return decision;
    }

    let mut queueable = true;
    for conflict in &decision.conflicts {
        let semantics = claim_target_semantics(index, conflict.target);
        if semantics.blocked
            || semantics.corridor
            || !semantics.waiting_allowed
            || !semantics.stop_allowed
        {
            queueable = false;
            break;
        }
    }

    if !queueable {
        decision.kind = ScheduleDecisionKind::Replan;
        decision
            .diagnostics
            .push("schedule conflicts require replanning instead of queuing".into());
        return decision;
    }

    decision.kind = ScheduleDecisionKind::Queue;
    decision.start_tick = latest_blocking_tick.saturating_add(1);
    decision.queue_position = decision.conflicts.len() as u64 + 1;
    decision
        .diagnostics
        .push("route should wait for an available reservation window".into());
    decision
}

fn add_conflict(
    decision: &mut ScheduleDecision,
    latest_blocking_tick: &mut u64,
    target: ClaimTarget,
    blocking_until_tick: u64,
    conflicting_claim_id: Option<ClaimId>,
    conflicting_lease_id: Option<LeaseId>,
    semantics: &ClaimTargetSemantics,
    source: &str,
) {
    let mut conflict = ScheduleConflict {
        target,
        blocking_until_tick,
        conflicting_claim_id,
        conflicting_lease_id,
        diagnostics: vec![format!("schedule conflict with {source}")],
    };
    if semantics.corridor {
        conflict
            .diagnostics
            .push("corridor resources cannot be used for side waiting".into());
    }
    if !semantics.waiting_allowed {
        conflict
            .diagnostics
            .push("waiting is not allowed on the blocking resource".into());
    }
    if !semantics.stop_allowed {
        conflict
            .diagnostics
            .push("stopping is not allowed on the blocking resource".into());
    }
    if semantics.blocked {
        conflict
            .diagnostics
            .push("blocking resource is hard-restricted".into());
    }
    if semantics.slowdown {
        conflict
            .diagnostics
            .push("blocking resource applies slowdown semantics".into());
    }
    if let Some(sw) = semantics.schedule_window.as_ref() {
        conflict
            .diagnostics
            .push(format!("blocking schedule window={sw}"));
    }
    decision.conflicts.push(conflict);
    if blocking_until_tick > *latest_blocking_tick {
        *latest_blocking_tick = blocking_until_tick;
    }
}

pub fn robot_missed_schedule_slot(state: &RobotState, current_tick: u64, grace_ticks: u64) -> bool {
    let Some(scheduled) = state.scheduled_start_tick else {
        return false;
    };
    current_tick > scheduled + grace_ticks
        && state.progress_state != RobotProgressState::FollowingRoute
        && state.progress_state != RobotProgressState::Idle
}

pub fn apply_schedule_decision(
    state: &mut RobotState,
    decision: &ScheduleDecision,
    updated_at_tick: u64,
) {
    state.updated_at_tick = updated_at_tick;
    state.scheduled_start_tick = Some(decision.start_tick);
    state.wait_ticks = decision.start_tick.saturating_sub(updated_at_tick);
    state.needs_replan = decision.kind == ScheduleDecisionKind::Replan;
    match decision.kind {
        ScheduleDecisionKind::Proceed => {
            state.progress_state = if state.route_plan.is_some() {
                RobotProgressState::FollowingRoute
            } else {
                RobotProgressState::Idle
            };
            state.hold_reason = None;
        }
        ScheduleDecisionKind::Queue => {
            state.progress_state = RobotProgressState::Queued;
            state.hold_reason = Some("queued_for_reservation_window".into());
        }
        ScheduleDecisionKind::Replan => {
            state.progress_state = RobotProgressState::Replanning;
            state.hold_reason = Some("schedule_conflict_requires_replan".into());
        }
    }
}

pub fn route_progress_index(state: &RobotState) -> u64 {
    let Some(plan) = state.route_plan.as_ref() else {
        return state.next_route_step_index;
    };
    let mut start_node_index = state.next_route_step_index;
    if let Some(current) = state.current_node_id {
        if let Some(pos) = plan.traversed_node_ids.iter().position(|id| *id == current) {
            start_node_index = pos as u64;
        }
    }
    start_node_index
}

pub fn route_zone_targets_from_progress(
    route_plan: &RoutePlan,
    start_node_index: u64,
    horizon: u64,
) -> Vec<Uuid> {
    let mut seen: HashSet<Uuid> = HashSet::new();
    let mut zone_ids = Vec::new();

    let node_limit = route_plan
        .traversed_node_zone_ids
        .len()
        .min((start_node_index + horizon + 1) as usize);
    for i in (start_node_index as usize)..node_limit {
        for &zid in &route_plan.traversed_node_zone_ids[i] {
            if seen.insert(zid) {
                zone_ids.push(zid);
            }
        }
    }

    let edge_limit = route_plan
        .traversed_edge_zone_ids
        .len()
        .min((start_node_index + horizon) as usize);
    for i in (start_node_index as usize)..edge_limit {
        for &zid in &route_plan.traversed_edge_zone_ids[i] {
            if seen.insert(zid) {
                zone_ids.push(zid);
            }
        }
    }

    zone_ids
}

pub fn claim_targets_from_route(route_plan: &RoutePlan) -> Vec<ClaimTarget> {
    let mut targets = Vec::new();
    for &z in &route_plan.traversed_zone_ids {
        targets.push(ClaimTarget {
            kind: ClaimTargetKind::Zone,
            resource_id: z,
        });
    }
    for &e in &route_plan.traversed_edge_ids {
        targets.push(ClaimTarget {
            kind: ClaimTargetKind::Edge,
            resource_id: e,
        });
    }
    for &n in &route_plan.traversed_node_ids {
        targets.push(ClaimTarget {
            kind: ClaimTargetKind::Node,
            resource_id: n,
        });
    }
    targets
}

pub fn claim_window_from_route(
    route_plan: &RoutePlan,
    start_tick: u64,
    ticks_per_cost_unit: f64,
) -> ClaimWindow {
    let duration = (route_plan.total_cost * ticks_per_cost_unit)
        .max(0.0)
        .ceil() as u64;
    ClaimWindow {
        start_tick: Some(start_tick),
        end_tick: Some(start_tick.saturating_add(duration)),
    }
}

pub fn claim_request_from_route(
    claim_id: ClaimId,
    robot_id: RobotId,
    mission_id: MissionId,
    route_plan: &RoutePlan,
    start_tick: Option<u64>,
    ticks_per_cost_unit: f64,
    access_mode: ClaimAccessMode,
) -> ClaimRequest {
    let mut request = ClaimRequest {
        id: claim_id,
        robot_id,
        mission_id,
        access_mode,
        targets: claim_targets_from_route(route_plan),
        ..ClaimRequest::default()
    };
    if let Some(t) = start_tick {
        request.requested_at_tick = Some(t);
        request.window = claim_window_from_route(route_plan, t, ticks_per_cost_unit);
    }
    request
}

pub fn rolling_horizon_claim_request(
    claim_id: ClaimId,
    state: &RobotState,
    access_mode: ClaimAccessMode,
) -> ClaimRequest {
    let mut request = ClaimRequest {
        id: claim_id,
        robot_id: state.robot_id,
        mission_id: state.mission_id,
        access_mode,
        ..ClaimRequest::default()
    };

    let Some(plan) = state.route_plan.as_ref() else {
        return request;
    };
    if validate_route_plan_shape(plan).is_err() {
        return request;
    }

    let start_node_index = route_progress_index(state);
    request.requested_at_tick = Some(state.updated_at_tick);
    request.window.start_tick = Some(state.updated_at_tick);
    if start_node_index as usize >= plan.traversed_node_ids.len() {
        request.window.end_tick = Some(state.updated_at_tick);
        return request;
    }

    let available_nodes = plan.traversed_node_ids.len() - start_node_index as usize;
    let available_edges = plan
        .traversed_edge_ids
        .len()
        .saturating_sub(start_node_index as usize);
    let node_limit = available_nodes.min((state.horizon + 1) as usize);
    let edge_limit = available_edges.min(state.horizon as usize);
    let zone_targets = route_zone_targets_from_progress(plan, start_node_index, state.horizon);

    for zid in zone_targets {
        request.targets.push(ClaimTarget {
            kind: ClaimTargetKind::Zone,
            resource_id: zid,
        });
    }
    for i in 0..edge_limit {
        let eid = plan.traversed_edge_ids[start_node_index as usize + i];
        request.targets.push(ClaimTarget {
            kind: ClaimTargetKind::Edge,
            resource_id: eid,
        });
    }
    for i in 0..node_limit {
        let nid = plan.traversed_node_ids[start_node_index as usize + i];
        request.targets.push(ClaimTarget {
            kind: ClaimTargetKind::Node,
            resource_id: nid,
        });
    }

    if (start_node_index as usize) < plan.steps.len() {
        let traversed_cost = if start_node_index == 0 {
            0.0
        } else {
            plan.steps[start_node_index as usize].cumulative_cost
        };
        let remaining_cost = (plan.total_cost - traversed_cost).max(0.0);
        request.window.end_tick = Some(
            state
                .updated_at_tick
                .saturating_add(remaining_cost.ceil() as u64),
        );
    } else {
        request.window.end_tick = Some(state.updated_at_tick);
    }

    request
}

pub fn release_targets_behind_progress(
    state: &mut RobotState,
    claim_manager: &mut ClaimManager,
) -> u64 {
    let Some(plan) = state.route_plan.as_ref() else {
        return 0;
    };
    if validate_route_plan_shape(plan).is_err() {
        return 0;
    }
    let Some(current) = state.current_node_id else {
        return 0;
    };
    let Some(current_index) = plan.traversed_node_ids.iter().position(|id| *id == current) else {
        return 0;
    };

    let mut remaining_node_ids: HashSet<Uuid> = HashSet::new();
    let mut remaining_edge_ids: HashSet<Uuid> = HashSet::new();
    let mut remaining_zone_ids: HashSet<Uuid> = HashSet::new();
    for i in current_index..plan.traversed_node_ids.len() {
        remaining_node_ids.insert(plan.traversed_node_ids[i]);
    }
    for i in current_index..plan.traversed_edge_ids.len() {
        remaining_edge_ids.insert(plan.traversed_edge_ids[i]);
    }
    for i in current_index..plan.traversed_node_zone_ids.len() {
        for &zid in &plan.traversed_node_zone_ids[i] {
            remaining_zone_ids.insert(zid);
        }
    }
    for i in current_index..plan.traversed_edge_zone_ids.len() {
        for &zid in &plan.traversed_edge_zone_ids[i] {
            remaining_zone_ids.insert(zid);
        }
    }

    let mut retained: Vec<LeaseId> = Vec::new();
    let mut released: u64 = 0;
    for lease_id in state.active_lease_ids.clone() {
        let Some(lease) = claim_manager.find_lease(lease_id) else {
            continue;
        };
        let keep = lease.targets.iter().any(|t| match t.kind {
            ClaimTargetKind::Node => remaining_node_ids.contains(&t.resource_id),
            ClaimTargetKind::Edge => remaining_edge_ids.contains(&t.resource_id),
            ClaimTargetKind::Zone => remaining_zone_ids.contains(&t.resource_id),
        });
        if keep {
            retained.push(lease_id);
        } else if claim_manager.release_lease(lease_id, None) {
            released += 1;
        }
    }
    state.active_lease_ids = retained;
    released
}

pub fn route_schedule_window_conflicts(
    index: &WorkspaceIndex,
    route_plan: &RoutePlan,
    active_window: &str,
) -> Vec<Uuid> {
    let mut out = Vec::new();
    for &zid in &route_plan.traversed_zone_ids {
        let Some(window_value) = index.zone_property(zid, "traffic.schedule_window") else {
            continue;
        };
        let matches_window = window_value
            .split(',')
            .map(|t| t.trim())
            .any(|t| t == active_window);
        if !matches_window {
            out.push(zid);
        }
    }
    out
}

pub fn route_matches_schedule_window(
    index: &WorkspaceIndex,
    route_plan: &RoutePlan,
    active_window: &str,
) -> bool {
    route_schedule_window_conflicts(index, route_plan, active_window).is_empty()
}

// ---------------------------------------------------------------------------
// arbitration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArbitrationDecision {
    Proceed,
    Yield,
    Replan,
}

#[derive(Debug, Clone, Default)]
pub struct ArbitrationContext {
    pub self_priority: f64,
    pub other_priority: f64,
    pub self_holds_lease: bool,
    pub other_holds_lease: bool,
    pub self_is_emergency: bool,
    pub other_is_emergency: bool,
    pub self_state: RobotProgressState,
    pub other_state: RobotProgressState,
    pub self_wait_ticks: u64,
    pub other_wait_ticks: u64,
    pub self_remaining_steps: u64,
    pub other_remaining_steps: u64,
}

pub fn arbitrate_right_of_way(ctx: &ArbitrationContext) -> ArbitrationDecision {
    use ArbitrationDecision::*;
    use RobotProgressState::*;

    if ctx.self_is_emergency && !ctx.other_is_emergency {
        return Proceed;
    }
    if ctx.other_is_emergency && !ctx.self_is_emergency {
        return Yield;
    }
    if ctx.other_holds_lease && !ctx.self_holds_lease {
        return Yield;
    }
    if ctx.self_holds_lease && !ctx.other_holds_lease {
        return Proceed;
    }
    if ctx.self_priority > ctx.other_priority {
        return Proceed;
    }
    if ctx.self_priority < ctx.other_priority {
        return Yield;
    }
    if ctx.self_state == Blocked && ctx.other_state != Blocked {
        return Yield;
    }
    if ctx.other_state == Blocked && ctx.self_state != Blocked {
        return Proceed;
    }
    if ctx.self_state == FollowingRoute && ctx.other_state == Waiting {
        return Proceed;
    }
    if ctx.self_state == Waiting && ctx.other_state == FollowingRoute {
        return Yield;
    }
    if ctx.self_state == Waiting && ctx.other_state == Waiting {
        if ctx.self_wait_ticks > ctx.other_wait_ticks {
            return Proceed;
        }
        if ctx.self_wait_ticks < ctx.other_wait_ticks {
            return Yield;
        }
    }
    if ctx.self_remaining_steps > 0 || ctx.other_remaining_steps > 0 {
        if ctx.self_remaining_steps < ctx.other_remaining_steps {
            return Proceed;
        }
        if ctx.self_remaining_steps > ctx.other_remaining_steps {
            return Yield;
        }
    }
    Replan
}

// ---------------------------------------------------------------------------
// Coordinator
// ---------------------------------------------------------------------------

pub struct Coordinator {
    index: Option<Arc<WorkspaceIndex>>,
    claim_manager: ClaimManager,
    robot_states: Vec<RobotState>,
    /// Auth key bound to each robot at registration. Checked on every later
    /// call. See `src/core/key.rs` and `PLAN.md`.
    robot_keys: BTreeMap<RobotId, Key>,
    /// Maps a registered UUID robot identifier (canonical string) to its
    /// internal numeric `RobotId`. Integer ids map to themselves and are not
    /// stored here. See `resolve_or_mint_robot_id`.
    robot_id_by_uuid: BTreeMap<String, RobotId>,
    /// Next id minted for a previously-unseen UUID robot. Starts high to avoid
    /// colliding with client-supplied numeric ids.
    next_synthetic_robot_id: u64,
    /// UUID robots whose id was minted but whose registration has not yet
    /// succeeded. Maps the tentative `RobotId` back to its canonical UUID
    /// string. The `robot_id_by_uuid` binding is only committed once
    /// `register_with_key` succeeds, so a failed/duplicate register cannot
    /// poison the mapping. See `resolve_or_mint_robot_id`.
    pending_uuid_bindings: BTreeMap<RobotId, String>,
    /// Heartbeat liveness tracking per robot: expected interval + last seen.
    robot_alive: BTreeMap<RobotId, AliveInfo>,
}

/// Base for synthetic ids minted for UUID robots (2^56).
const SYNTHETIC_ROBOT_ID_BASE: u64 = 1 << 56;

/// Per-robot heartbeat liveness: the expected interval (seconds) the robot
/// promised at registration, and the wall-clock time (epoch millis) of its last
/// heartbeat. A robot is "inactive" once `2 × interval` has elapsed.
///
/// Public and `serde`-derivable so it can round-trip through a
/// [`crate::persist::CoordinatorSnapshot`]. Fields are otherwise only touched
/// through the coordinator's alive/heartbeat methods.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AliveInfo {
    pub interval_secs: u64,
    pub last_seen_ms: u64,
}

impl Default for Coordinator {
    fn default() -> Self {
        Self {
            index: None,
            claim_manager: ClaimManager::new(),
            robot_states: Vec::new(),
            robot_keys: BTreeMap::new(),
            robot_id_by_uuid: BTreeMap::new(),
            next_synthetic_robot_id: SYNTHETIC_ROBOT_ID_BASE,
            pending_uuid_bindings: BTreeMap::new(),
            robot_alive: BTreeMap::new(),
        }
    }
}

impl Coordinator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_index(index: Arc<WorkspaceIndex>) -> Self {
        Self {
            index: Some(Arc::clone(&index)),
            claim_manager: ClaimManager::with_index(index),
            robot_states: Vec::new(),
            robot_keys: BTreeMap::new(),
            robot_id_by_uuid: BTreeMap::new(),
            next_synthetic_robot_id: SYNTHETIC_ROBOT_ID_BASE,
            pending_uuid_bindings: BTreeMap::new(),
            robot_alive: BTreeMap::new(),
        }
    }

    /// Default heartbeat interval (seconds) a robot is assumed to use when it
    /// does not specify one at registration.
    pub const DEFAULT_ALIVE_SECS: u64 = 2;

    /// Record the heartbeat interval a robot promised at registration and stamp
    /// it as just-seen. `interval_secs == 0` falls back to the default.
    pub fn set_alive(&mut self, robot_id: RobotId, interval_secs: u64, now_ms: u64) {
        let interval_secs = if interval_secs == 0 {
            Self::DEFAULT_ALIVE_SECS
        } else {
            interval_secs
        };
        self.robot_alive.insert(
            robot_id,
            AliveInfo {
                interval_secs,
                last_seen_ms: now_ms,
            },
        );
    }

    /// Refresh a robot's last-seen time (call on every heartbeat).
    pub fn touch_robot(&mut self, robot_id: RobotId, now_ms: u64) {
        if let Some(a) = self.robot_alive.get_mut(&robot_id) {
            a.last_seen_ms = now_ms;
        }
    }

    /// Whether a robot is still active at `now_ms`: less than `2 × interval` has
    /// elapsed since its last heartbeat. Robots with no liveness info (never set
    /// an interval) are treated as active.
    pub fn robot_active_at(&self, robot_id: RobotId, now_ms: u64) -> bool {
        match self.robot_alive.get(&robot_id) {
            Some(a) => {
                now_ms.saturating_sub(a.last_seen_ms) <= a.interval_secs.saturating_mul(2_000)
            }
            None => true,
        }
    }

    /// Robots whose last heartbeat is older than `2 × interval` at `now_ms`.
    pub fn inactive_robots_at(&self, now_ms: u64) -> Vec<RobotId> {
        self.robot_alive
            .iter()
            .filter(|(_, a)| {
                now_ms.saturating_sub(a.last_seen_ms) > a.interval_secs.saturating_mul(2_000)
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// Release the claims/leases of every robot that has gone inactive (no
    /// heartbeat for `2 × interval`). The robot stays registered, so it resumes
    /// once it heartbeats again. Returns the robots whose claims were actually
    /// freed (so callers can log them). Idempotent — robots already freed are
    /// not reported again.
    pub fn sweep_inactive(&mut self, now_ms: u64) -> Vec<RobotId> {
        let mut freed = Vec::new();
        for robot_id in self.inactive_robots_at(now_ms) {
            let removed = self.claim_manager.remove_requests_for_robot(robot_id)
                + self
                    .claim_manager
                    .release_leases_for_robot(robot_id, Some(now_ms));
            if removed > 0 {
                freed.push(robot_id);
            }
        }
        freed
    }

    pub fn index(&self) -> Option<&WorkspaceIndex> {
        self.index.as_deref()
    }
    pub fn index_arc(&self) -> Option<Arc<WorkspaceIndex>> {
        self.index.clone()
    }
    pub fn has_index(&self) -> bool {
        self.index.is_some()
    }
    pub fn claim_manager(&self) -> &ClaimManager {
        &self.claim_manager
    }
    pub fn claim_manager_mut(&mut self) -> &mut ClaimManager {
        &mut self.claim_manager
    }
    pub fn empty(&self) -> bool {
        self.robot_states.is_empty()
    }
    pub fn robot_count(&self) -> usize {
        self.robot_states.len()
    }
    pub fn robot_states(&self) -> &[RobotState] {
        &self.robot_states
    }

    pub fn bind_index(&mut self, index: Arc<WorkspaceIndex>) {
        self.index = Some(Arc::clone(&index));
        self.claim_manager.bind_index(index);
    }

    pub fn clear(&mut self) {
        self.robot_states.clear();
        self.robot_keys.clear();
        self.robot_id_by_uuid.clear();
        self.pending_uuid_bindings.clear();
        self.robot_alive.clear();
        self.next_synthetic_robot_id = SYNTHETIC_ROBOT_ID_BASE;
        self.claim_manager.clear();
    }

    /// Resolve a raw robot identifier (an integer or a UUID string) to an
    /// internal [`RobotId`], MINTING a stable id for a previously-unseen UUID.
    /// Integer ids map to themselves. Returns `None` if `raw` is neither an
    /// integer nor a UUID. Use this at registration time.
    pub fn resolve_or_mint_robot_id(&mut self, raw: &str) -> Option<RobotId> {
        let raw = raw.trim();
        if let Ok(n) = raw.parse::<u64>() {
            return Some(RobotId::new(n));
        }
        let canon = Uuid::parse_str(raw).ok()?.to_string();
        if let Some(id) = self.robot_id_by_uuid.get(&canon) {
            return Some(*id);
        }
        // Mint a tentative id but DO NOT commit the UUID->id binding yet: it is
        // only committed once `register_with_key` succeeds, so a failed or
        // duplicate registration cannot poison `robot_id_by_uuid`.
        let id = RobotId::new(self.next_synthetic_robot_id);
        self.next_synthetic_robot_id = self.next_synthetic_robot_id.saturating_add(1);
        self.pending_uuid_bindings.insert(id, canon);
        Some(id)
    }

    /// Resolve a raw robot identifier to an internal [`RobotId`] WITHOUT
    /// minting: integer ids map to themselves; a UUID resolves only if it was
    /// already registered. Use this on every non-registration call.
    pub fn resolve_robot_id(&self, raw: &str) -> Option<RobotId> {
        let raw = raw.trim();
        if let Ok(n) = raw.parse::<u64>() {
            return Some(RobotId::new(n));
        }
        let canon = Uuid::parse_str(raw).ok()?.to_string();
        self.robot_id_by_uuid.get(&canon).copied()
    }

    /// Flat registration: bind `key` to a fresh robot identified only by
    /// `robot_id`. The coordinator owns all other state. Returns `false` if the
    /// id is already registered (caller maps that to the "already registered"
    /// reason); the existing robot and its key are left untouched.
    pub fn register_with_key(&mut self, robot_id: RobotId, key: Key) -> bool {
        if self.robot_keys.contains_key(&robot_id) || self.find_robot_state(robot_id).is_some() {
            // Registration did not take: drop any tentative UUID->id binding
            // minted for this id so the mapping is not poisoned. (A no-op for
            // integer ids and for already-committed UUID robots.)
            self.pending_uuid_bindings.remove(&robot_id);
            return false;
        }
        let state = RobotState {
            robot_id,
            ..RobotState::default()
        };
        self.robot_states.push(state);
        self.robot_keys.insert(robot_id, key);
        // Registration succeeded: commit the tentative UUID->id binding, if any.
        if let Some(canon) = self.pending_uuid_bindings.remove(&robot_id) {
            self.robot_id_by_uuid.insert(canon, robot_id);
        }
        true
    }

    /// Whether `key` authenticates as `robot_id`'s registered key. Returns
    /// `false` if the robot is not registered or has no bound key.
    pub fn validate_key(&self, robot_id: RobotId, key: &Key) -> bool {
        match self.robot_keys.get(&robot_id) {
            Some(stored) => stored.matches(key),
            None => false,
        }
    }

    /// Whether a robot with this id is registered.
    pub fn has_robot(&self, robot_id: RobotId) -> bool {
        self.robot_keys.contains_key(&robot_id) || self.find_robot_state(robot_id).is_some()
    }

    pub fn register_robot(&mut self, state: RobotState) {
        if let Some(slot) = self
            .robot_states
            .iter_mut()
            .find(|s| s.robot_id == state.robot_id)
        {
            *slot = state;
            return;
        }
        self.robot_states.push(state);
    }

    pub fn unregister_robot(&mut self, robot_id: RobotId) -> bool {
        if let Some(pos) = self
            .robot_states
            .iter()
            .position(|s| s.robot_id == robot_id)
        {
            let updated_at_tick = self.robot_states[pos].updated_at_tick;
            self.claim_manager.remove_requests_for_robot(robot_id);
            self.claim_manager
                .release_leases_for_robot(robot_id, Some(updated_at_tick));
            self.robot_states.remove(pos);
            self.robot_keys.remove(&robot_id);
            self.robot_id_by_uuid.retain(|_, v| *v != robot_id);
            self.robot_alive.remove(&robot_id);
            return true;
        }
        false
    }

    pub fn find_robot_state(&self, robot_id: RobotId) -> Option<&RobotState> {
        self.robot_states.iter().find(|s| s.robot_id == robot_id)
    }

    pub fn find_robot_state_mut(&mut self, robot_id: RobotId) -> Option<&mut RobotState> {
        self.robot_states
            .iter_mut()
            .find(|s| s.robot_id == robot_id)
    }

    pub fn claim_request_for_robot(
        &self,
        robot_id: RobotId,
        claim_id: ClaimId,
        access_mode: ClaimAccessMode,
    ) -> ClaimRequest {
        match self.find_robot_state(robot_id) {
            None => ClaimRequest::default(),
            Some(state) => rolling_horizon_claim_request(claim_id, state, access_mode),
        }
    }

    /// Record a robot's reported pose.
    ///
    /// `position` and `heading` are independent: a robot may send either, both,
    /// or neither. `None` means "not reported in this heartbeat", never "moved
    /// to nowhere" — so a heading-only report leaves the last known position
    /// standing instead of erasing it.
    pub fn update_robot_pose(
        &mut self,
        robot_id: RobotId,
        position: Option<crate::robot::RobotPosition>,
        heading: Option<crate::robot::RobotHeading>,
        at_ms: u64,
    ) -> bool {
        if position.is_none() && heading.is_none() {
            return false;
        }
        let Some(state) = self.find_robot_state_mut(robot_id) else {
            return false;
        };
        if position.is_some() {
            state.position = position;
        }
        if heading.is_some() {
            state.heading = heading;
        }
        state.pose_at_ms = Some(at_ms);
        true
    }

    pub fn update_robot_progress(
        &mut self,
        robot_id: RobotId,
        current_node_id: Option<Uuid>,
        current_edge_id: Option<Uuid>,
        updated_at_tick: u64,
    ) -> bool {
        let Some(state) = self.find_robot_state_mut(robot_id) else {
            return false;
        };
        state.current_node_id = current_node_id;
        state.current_edge_id = current_edge_id;
        if let (Some(plan), Some(node_id)) = (state.route_plan.as_ref(), current_node_id) {
            if let Some(pos) = plan.traversed_node_ids.iter().position(|id| *id == node_id) {
                state.next_route_step_index = pos as u64;
                state.progress_state = if pos + 1 >= plan.traversed_node_ids.len() {
                    RobotProgressState::Idle
                } else {
                    RobotProgressState::FollowingRoute
                };
                state.hold_reason = None;
                state.needs_replan = false;
                state.wait_ticks = 0;
            }
        } else if current_edge_id.is_some() {
            state.progress_state = RobotProgressState::FollowingRoute;
            state.needs_replan = false;
            state.wait_ticks = 0;
        } else {
            state.progress_state = RobotProgressState::Waiting;
        }
        state.updated_at_tick = updated_at_tick;
        true
    }

    pub fn assign_route_plan(
        &mut self,
        robot_id: RobotId,
        route_plan: RoutePlan,
        horizon: u64,
        updated_at_tick: u64,
    ) -> bool {
        let Some(state) = self.find_robot_state_mut(robot_id) else {
            return false;
        };
        let total_cost = route_plan.total_cost;
        let is_empty = route_plan.traversed_node_ids.is_empty();
        state.route_plan = Some(route_plan);
        state.horizon = horizon;
        state.next_route_step_index = 0;
        state.scheduled_start_tick = Some(updated_at_tick);
        state.reserved_until_tick =
            Some(updated_at_tick.saturating_add(total_cost.max(0.0).ceil() as u64));
        state.wait_ticks = 0;
        state.needs_replan = false;
        state.progress_state = if is_empty {
            RobotProgressState::Idle
        } else {
            RobotProgressState::FollowingRoute
        };
        state.updated_at_tick = updated_at_tick;
        true
    }

    pub fn update_robot_claim_state(
        &mut self,
        robot_id: RobotId,
        pending_claim_ids: Vec<ClaimId>,
        active_lease_ids: Vec<LeaseId>,
        last_claim_tick: Option<u64>,
    ) -> bool {
        let Some(state) = self.find_robot_state_mut(robot_id) else {
            return false;
        };
        state.pending_claim_ids = pending_claim_ids;
        state.active_lease_ids = active_lease_ids;
        state.last_claim_tick = last_claim_tick;
        true
    }

    pub fn release_behind_progress(&mut self, robot_id: RobotId) -> u64 {
        let Some(pos) = self
            .robot_states
            .iter()
            .position(|s| s.robot_id == robot_id)
        else {
            return 0;
        };
        let mut state = std::mem::take(&mut self.robot_states[pos]);
        let released = release_targets_behind_progress(&mut state, &mut self.claim_manager);
        if released > 0 && state.progress_state == RobotProgressState::FollowingRoute {
            state.last_claim_tick = Some(state.updated_at_tick);
        }
        self.robot_states[pos] = state;
        released
    }

    pub fn schedule_robot_route(
        &mut self,
        robot_id: RobotId,
        claim_id: ClaimId,
        start_tick: u64,
        ticks_per_cost_unit: f64,
        access_mode: ClaimAccessMode,
    ) -> ScheduleDecision {
        let (Some(index), Some(state)) = (self.index.as_ref(), self.find_robot_state(robot_id))
        else {
            return ScheduleDecision {
                kind: ScheduleDecisionKind::Replan,
                start_tick,
                diagnostics: vec!["robot does not have a schedulable route".into()],
                ..ScheduleDecision::default()
            };
        };
        let Some(plan) = state.route_plan.clone() else {
            return ScheduleDecision {
                kind: ScheduleDecisionKind::Replan,
                start_tick,
                diagnostics: vec!["robot does not have a schedulable route".into()],
                ..ScheduleDecision::default()
            };
        };
        let mission_id = state.mission_id;
        let request = claim_request_from_route(
            claim_id,
            robot_id,
            mission_id,
            &plan,
            Some(start_tick),
            ticks_per_cost_unit,
            access_mode,
        );
        let decision = schedule_route_request(
            index,
            &self.claim_manager,
            &request,
            &plan,
            start_tick,
            ticks_per_cost_unit,
        );
        if let Some(state) = self.find_robot_state_mut(robot_id) {
            state.reserved_until_tick = request.window.end_tick;
            apply_schedule_decision(state, &decision, start_tick);
        }
        decision
    }

    pub fn refresh_robot_leases(
        &mut self,
        robot_id: RobotId,
        refreshed_at_tick: u64,
        extension_ticks: u64,
    ) -> u64 {
        let Some(state) = self.find_robot_state_mut(robot_id) else {
            return 0;
        };
        let lease_ids = state.active_lease_ids.clone();
        state.last_claim_tick = Some(refreshed_at_tick);

        let mut refreshed: u64 = 0;
        for lease_id in lease_ids {
            let new_expiry = self
                .claim_manager
                .find_lease(lease_id)
                .and_then(|l| l.expires_at_tick)
                .map(|t| t + extension_ticks);
            if self
                .claim_manager
                .refresh_lease(lease_id, refreshed_at_tick, new_expiry)
            {
                refreshed += 1;
            }
        }
        refreshed
    }

    pub fn revoke_robot_leases(
        &mut self,
        robot_id: RobotId,
        reason: String,
        revoked_at_tick: u64,
    ) -> u64 {
        let Some(state) = self.find_robot_state_mut(robot_id) else {
            return 0;
        };
        let lease_ids = std::mem::take(&mut state.active_lease_ids);
        state.progress_state = RobotProgressState::Replanning;
        state.hold_reason = Some(reason.clone());
        state.updated_at_tick = revoked_at_tick;

        let mut revoked: u64 = 0;
        for lease_id in lease_ids {
            if self
                .claim_manager
                .revoke_lease(lease_id, reason.clone(), revoked_at_tick)
            {
                revoked += 1;
            }
        }
        // re-borrow to set needs_replan now that we're done with claim_manager
        if let Some(state) = self.find_robot_state_mut(robot_id) {
            state.needs_replan = revoked > 0;
        }
        revoked
    }

    pub fn handle_missed_schedule_slot(
        &mut self,
        robot_id: RobotId,
        current_tick: u64,
        grace_ticks: u64,
    ) -> bool {
        let Some(state) = self.find_robot_state_mut(robot_id) else {
            return false;
        };
        if !robot_missed_schedule_slot(state, current_tick, grace_ticks) {
            return false;
        }
        state.progress_state = RobotProgressState::Replanning;
        state.hold_reason = Some("missed_reservation_window".into());
        state.needs_replan = true;
        state.updated_at_tick = current_tick;
        true
    }

    // -- persistence ------------------------------------------------------

    /// Capture the coordinator's owned state (registrations, keys, UUID→id
    /// map, synthetic-id counter, liveness, and the embedded claim manager)
    /// into a serializable snapshot. The bound [`WorkspaceIndex`] and the
    /// transient `pending_uuid_bindings` are intentionally excluded. See
    /// [`crate::persist`].
    pub fn snapshot(&self) -> crate::persist::CoordinatorSnapshot {
        crate::persist::CoordinatorSnapshot {
            robot_states: self.robot_states.clone(),
            robot_keys: self.robot_keys.clone(),
            robot_id_by_uuid: self.robot_id_by_uuid.clone(),
            next_synthetic_robot_id: self.next_synthetic_robot_id,
            robot_alive: self.robot_alive.clone(),
            claims: self.claim_manager.snapshot(),
        }
    }

    /// Rebuild a coordinator from a snapshot, re-attaching the freshly-loaded
    /// `index` (which was never serialized). `pending_uuid_bindings` starts
    /// empty. The claim manager's `next_id` atomic is rebuilt so minted ids
    /// keep increasing after a restart.
    pub fn restore(
        snapshot: crate::persist::CoordinatorSnapshot,
        index: Option<Arc<WorkspaceIndex>>,
    ) -> Self {
        let claim_manager = ClaimManager::restore(snapshot.claims, index.clone());
        Self {
            index,
            claim_manager,
            robot_states: snapshot.robot_states,
            robot_keys: snapshot.robot_keys,
            robot_id_by_uuid: snapshot.robot_id_by_uuid,
            next_synthetic_robot_id: snapshot.next_synthetic_robot_id,
            pending_uuid_bindings: BTreeMap::new(),
            robot_alive: snapshot.robot_alive,
        }
    }
}
