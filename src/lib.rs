//! syncbot — multi-robot navigation, claims, and scheduling on top of
//! `zoneout`.
//!
//! See `PLAN.md` at the crate root for the design overview. The public
//! surface is kept flat at `syncbot::*` via re-exports below.

// Crate-wide lint policy. The explicit form of these lints is clearer for a
// straight C++ port; collapsing them obscures the original control flow.
#![allow(
    clippy::collapsible_if,
    clippy::derivable_impls,
    clippy::field_reassign_with_default,
    clippy::missing_safety_doc,
    clippy::too_many_arguments
)]

pub mod claim;
pub mod coordinator;
pub mod core;
pub mod ffi;
pub mod index;
pub mod persist;
pub mod policy;
pub mod robot;
pub mod route;
pub mod vda;

#[cfg(feature = "python")]
pub mod python;

#[cfg(any(feature = "rest", feature = "robo", feature = "xmlt"))]
pub mod wire;

pub use crate::core::error::{Error, Result};
pub use crate::core::ids::{ClaimId, LeaseId, MissionId, RobotId};
pub use crate::core::key::{Key, KeyError};

pub use crate::policy::{
    EdgeTrafficSemantics, TrafficIssueSeverity, TrafficParseIssue, ZonePolicy, ZonePolicyKind,
    derive_effective_edge_semantics, merge_zone_policy, parse_edge_traffic_semantics,
    parse_traffic_bool, parse_traffic_f64, parse_traffic_string, parse_traffic_u64,
    parse_zone_policy, validate_edge_traffic_properties, validate_zone_traffic_properties,
};

pub use crate::index::{
    NUMERIC_ID_PROPERTY, ResourceRef, ValidationIssue, ValidationSeverity, WorkspaceIndex,
};

pub use crate::route::{
    RouteCostModel, RouteFailure, RouteFailureKind, RoutePlan, RoutePlanningResult,
    RouteSearchState, RouteStep, accumulate_route_cost, build_route_plan,
    build_route_plan_from_search, diagnose_route_failure, extract_traversed_edge_ids,
    extract_traversed_node_ids, extract_traversed_zone_ids, plan_route, reconstruct_route_steps,
    shortest_path_search, shortest_path_search_with_blocking, shortest_path_search_with_penalties,
    validate_route_plan_shape,
};

pub use crate::claim::{
    ClaimAccessMode, ClaimDecision, ClaimEvaluation, ClaimManager, ClaimRequest, ClaimTarget,
    ClaimTargetKind, ClaimWindow, Lease, LeaseDisposition,
};

pub use crate::robot::{RobotProgressState, RobotState};

pub use crate::coordinator::{
    ArbitrationContext, ArbitrationDecision, ClaimTargetSemantics, Coordinator, ScheduleConflict,
    ScheduleDecision, ScheduleDecisionKind, ScheduledTargetWindow, apply_schedule_decision,
    arbitrate_right_of_way, claim_request_from_route, claim_target_semantics,
    claim_targets_from_route, claim_window_from_route, release_targets_behind_progress,
    robot_missed_schedule_slot, rolling_horizon_claim_request, route_matches_schedule_window,
    route_progress_index, route_schedule_window_conflicts, route_zone_targets_from_progress,
    schedule_route_request, scheduled_target_windows_from_route,
};

/// Crate version, read from `Cargo.toml` at compile time so it never drifts.
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
