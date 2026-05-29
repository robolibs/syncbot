//! Claim and lease data types.
//!
//! Port of `include/timenav/claim.hpp`. The actual lifecycle and conflict
//! evaluation lives in `manager.rs`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use crate::core::ids::{ClaimId, LeaseId, MissionId, RobotId};

pub mod manager;
pub use manager::ClaimManager;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaimTargetKind {
    Zone,
    Node,
    Edge,
}

impl Default for ClaimTargetKind {
    fn default() -> Self {
        Self::Zone
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaimAccessMode {
    Shared,
    Exclusive,
}

impl Default for ClaimAccessMode {
    fn default() -> Self {
        Self::Exclusive
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaimDecision {
    Grant,
    Deny,
}

impl Default for ClaimDecision {
    fn default() -> Self {
        Self::Grant
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeaseDisposition {
    Active,
    Released,
    Expired,
    Revoked,
}

impl Default for LeaseDisposition {
    fn default() -> Self {
        Self::Active
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_tick: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_tick: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimTarget {
    pub kind: ClaimTargetKind,
    pub resource_id: Uuid,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRequest {
    pub id: ClaimId,
    pub robot_id: RobotId,
    pub mission_id: MissionId,
    pub access_mode: ClaimAccessMode,
    pub priority: u32,
    pub requested_at_tick: Option<u64>,
    pub window: ClaimWindow,
    pub targets: Vec<ClaimTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Lease {
    pub id: LeaseId,
    pub claim_id: ClaimId,
    pub robot_id: RobotId,
    pub access_mode: ClaimAccessMode,
    pub targets: Vec<ClaimTarget>,
    pub granted_at_tick: Option<u64>,
    pub expires_at_tick: Option<u64>,
    pub refreshed_at_tick: Option<u64>,
    pub released_at_tick: Option<u64>,
    pub revoked_at_tick: Option<u64>,
    pub revoke_reason: Option<String>,
    pub disposition: LeaseDisposition,
    pub active: bool,
}

impl Default for Lease {
    fn default() -> Self {
        Self {
            id: LeaseId::default(),
            claim_id: ClaimId::default(),
            robot_id: RobotId::default(),
            access_mode: ClaimAccessMode::Exclusive,
            targets: Vec::new(),
            granted_at_tick: None,
            expires_at_tick: None,
            refreshed_at_tick: None,
            released_at_tick: None,
            revoked_at_tick: None,
            revoke_reason: None,
            disposition: LeaseDisposition::Active,
            active: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimEvaluation {
    pub decision: ClaimDecision,
    pub reason: String,
    pub conflicting_claim_id: Option<ClaimId>,
    pub conflicting_lease_id: Option<LeaseId>,
    pub conflicting_targets: Vec<ClaimTarget>,
    pub blocking_target: Option<ClaimTarget>,
    pub diagnostics: Vec<String>,
}
