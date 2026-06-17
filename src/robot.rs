//! Per-robot navigation state. Port of `include/syncbot/robot_state.hpp`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::claim::{ClaimId, LeaseId};
use crate::core::ids::{MissionId, RobotId};
use crate::route::RoutePlan;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RobotProgressState {
    Idle,
    FollowingRoute,
    Waiting,
    Queued,
    Blocked,
    Replanning,
}

impl Default for RobotProgressState {
    fn default() -> Self {
        Self::Idle
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RobotState {
    pub robot_id: RobotId,
    pub mission_id: MissionId,
    pub current_node_id: Option<Uuid>,
    pub current_edge_id: Option<Uuid>,
    pub route_plan: Option<RoutePlan>,
    pub pending_claim_ids: Vec<ClaimId>,
    pub active_lease_ids: Vec<LeaseId>,
    pub progress_state: RobotProgressState,
    pub next_route_step_index: u64,
    pub hold_reason: Option<String>,
    pub last_claim_tick: Option<u64>,
    pub scheduled_start_tick: Option<u64>,
    pub reserved_until_tick: Option<u64>,
    pub wait_ticks: u64,
    pub needs_replan: bool,
    pub horizon: u64,
    pub updated_at_tick: u64,
}
