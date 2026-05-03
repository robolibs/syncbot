//! `ClaimManager` — request and lease lifecycle plus conflict / capacity
//! evaluation. Port of `include/timenav/claim_manager.hpp`.

use std::sync::Arc;

use uuid::Uuid;

use crate::core::ids::{ClaimId, LeaseId, RobotId};
use crate::index::WorkspaceIndex;
use crate::policy::{ZonePolicyKind, parse_edge_traffic_semantics, parse_zone_policy};

use super::{
    ClaimAccessMode, ClaimDecision, ClaimEvaluation, ClaimRequest, ClaimTarget,
    ClaimTargetKind, ClaimWindow, Lease, LeaseDisposition,
};

pub struct ClaimManager {
    index: Option<Arc<WorkspaceIndex>>,
    active_requests: Vec<ClaimRequest>,
    active_leases: Vec<Lease>,
    released_leases: Vec<Lease>,
}

impl Default for ClaimManager {
    fn default() -> Self {
        Self {
            index: None,
            active_requests: Vec::new(),
            active_leases: Vec::new(),
            released_leases: Vec::new(),
        }
    }
}

impl ClaimManager {
    pub fn new() -> Self { Self::default() }

    pub fn with_index(index: Arc<WorkspaceIndex>) -> Self {
        Self { index: Some(index), ..Self::default() }
    }

    pub fn empty(&self) -> bool {
        self.active_requests.is_empty() && self.active_leases.is_empty()
    }

    pub fn index(&self) -> Option<&WorkspaceIndex> { self.index.as_deref() }
    pub fn index_arc(&self) -> Option<Arc<WorkspaceIndex>> { self.index.clone() }
    pub fn has_index(&self) -> bool { self.index.is_some() }

    pub fn request_count(&self) -> usize { self.active_requests.len() }
    pub fn lease_count(&self) -> usize { self.active_leases.len() }

    pub fn requests(&self) -> &[ClaimRequest] { &self.active_requests }
    pub fn leases(&self) -> &[Lease] { &self.active_leases }
    pub fn released_leases(&self) -> &[Lease] { &self.released_leases }

    pub fn bind_index(&mut self, index: Arc<WorkspaceIndex>) {
        self.index = Some(index);
    }

    pub fn clear(&mut self) {
        self.active_requests.clear();
        self.active_leases.clear();
        self.released_leases.clear();
    }

    // -- request lifecycle ------------------------------------------------

    pub fn add_request(&mut self, request: ClaimRequest) { self.upsert_request(request); }

    pub fn upsert_request(&mut self, request: ClaimRequest) {
        if let Some(slot) = self.active_requests.iter_mut().find(|r| r.id == request.id) {
            *slot = request;
            return;
        }
        self.active_requests.push(request);
    }

    pub fn remove_request(&mut self, id: ClaimId) -> bool {
        if let Some(pos) = self.active_requests.iter().position(|r| r.id == id) {
            self.active_requests.remove(pos);
            return true;
        }
        false
    }

    pub fn remove_requests_for_robot(&mut self, robot_id: RobotId) -> u64 {
        let before = self.active_requests.len();
        self.active_requests.retain(|r| r.robot_id != robot_id);
        (before - self.active_requests.len()) as u64
    }

    // -- lease lifecycle --------------------------------------------------

    pub fn add_lease(&mut self, lease: Lease) { self.upsert_lease(lease); }

    pub fn upsert_lease(&mut self, lease: Lease) {
        if let Some(slot) = self.active_leases.iter_mut().find(|l| l.id == lease.id) {
            *slot = lease;
            return;
        }
        self.active_leases.push(lease);
    }

    pub fn remove_lease(&mut self, id: LeaseId) -> bool {
        if let Some(pos) = self.active_leases.iter().position(|l| l.id == id) {
            self.active_leases.remove(pos);
            return true;
        }
        false
    }

    pub fn release_leases_for_robot(
        &mut self,
        robot_id: RobotId,
        released_at_tick: Option<u64>,
    ) -> u64 {
        let mut released = 0u64;
        let mut i = 0;
        while i < self.active_leases.len() {
            if self.active_leases[i].robot_id != robot_id { i += 1; continue; }
            let mut archived = self.active_leases.remove(i);
            archived.active = false;
            archived.disposition = LeaseDisposition::Released;
            archived.released_at_tick = released_at_tick;
            self.released_leases.push(archived);
            released += 1;
        }
        released
    }

    pub fn release_lease(&mut self, id: LeaseId, released_at_tick: Option<u64>) -> bool {
        if let Some(pos) = self.active_leases.iter().position(|l| l.id == id) {
            let mut archived = self.active_leases.remove(pos);
            archived.active = false;
            archived.disposition = LeaseDisposition::Released;
            archived.released_at_tick = released_at_tick;
            self.released_leases.push(archived);
            return true;
        }
        false
    }

    pub fn expire_leases(&mut self, current_tick: u64) -> u64 {
        let mut expired = 0u64;
        let mut i = 0;
        while i < self.active_leases.len() {
            let exp = self.active_leases[i].expires_at_tick;
            if !matches!(exp, Some(t) if t <= current_tick) { i += 1; continue; }
            let mut archived = self.active_leases.remove(i);
            archived.active = false;
            archived.disposition = LeaseDisposition::Expired;
            archived.released_at_tick = Some(current_tick);
            self.released_leases.push(archived);
            expired += 1;
        }
        expired
    }

    pub fn refresh_lease(
        &mut self,
        id: LeaseId,
        refreshed_at_tick: u64,
        expires_at_tick: Option<u64>,
    ) -> bool {
        if let Some(slot) = self.active_leases.iter_mut().find(|l| l.id == id) {
            slot.refreshed_at_tick = Some(refreshed_at_tick);
            if expires_at_tick.is_some() { slot.expires_at_tick = expires_at_tick; }
            return true;
        }
        false
    }

    pub fn revoke_lease(&mut self, id: LeaseId, reason: String, revoked_at_tick: u64) -> bool {
        if let Some(pos) = self.active_leases.iter().position(|l| l.id == id) {
            let mut archived = self.active_leases.remove(pos);
            archived.active = false;
            archived.disposition = LeaseDisposition::Revoked;
            archived.revoked_at_tick = Some(revoked_at_tick);
            archived.released_at_tick = Some(revoked_at_tick);
            archived.revoke_reason = Some(reason);
            self.released_leases.push(archived);
            return true;
        }
        false
    }

    // -- find -------------------------------------------------------------

    pub fn find_request(&self, id: ClaimId) -> Option<&ClaimRequest> {
        self.active_requests.iter().find(|r| r.id == id)
    }
    pub fn has_request(&self, id: ClaimId) -> bool { self.find_request(id).is_some() }

    pub fn find_lease(&self, id: LeaseId) -> Option<&Lease> {
        self.active_leases.iter().find(|l| l.id == id)
    }
    pub fn has_lease(&self, id: LeaseId) -> bool { self.find_lease(id).is_some() }

    pub fn find_released_lease(&self, id: LeaseId) -> Option<&Lease> {
        self.released_leases.iter().find(|l| l.id == id)
    }

    pub fn leases_for_robot(&self, robot_id: RobotId) -> Vec<&Lease> {
        self.active_leases.iter().filter(|l| l.robot_id == robot_id).collect()
    }

    pub fn lease_for_claim(&self, claim_id: ClaimId) -> Option<&Lease> {
        self.active_leases.iter().find(|l| l.claim_id == claim_id)
    }

    // -- static (no-index) compatibility ----------------------------------

    pub fn zone_claims_compatible(lhs: &ClaimRequest, rhs: &ClaimRequest) -> bool {
        target_kind_compatible(lhs, rhs, ClaimTargetKind::Zone)
    }
    pub fn node_claims_compatible(lhs: &ClaimRequest, rhs: &ClaimRequest) -> bool {
        target_kind_compatible(lhs, rhs, ClaimTargetKind::Node)
    }
    pub fn edge_claims_compatible(lhs: &ClaimRequest, rhs: &ClaimRequest) -> bool {
        target_kind_compatible(lhs, rhs, ClaimTargetKind::Edge)
    }
    pub fn claims_compatible(lhs: &ClaimRequest, rhs: &ClaimRequest) -> bool {
        Self::zone_claims_compatible(lhs, rhs)
            && Self::node_claims_compatible(lhs, rhs)
            && Self::edge_claims_compatible(lhs, rhs)
    }
    pub fn claims_compatible_with_lease(request: &ClaimRequest, lease: &Lease) -> bool {
        let view = lease_as_request_view(lease);
        Self::claims_compatible(request, &view)
    }

    // -- evaluation -------------------------------------------------------

    pub fn evaluate_request(&self, request: &ClaimRequest) -> ClaimEvaluation {
        if request.targets.is_empty() {
            return ClaimEvaluation {
                decision: ClaimDecision::Deny,
                reason: "claim request does not contain any targets".into(),
                diagnostics: vec!["request has no claim targets".into()],
                ..ClaimEvaluation::default()
            };
        }

        if let Some(invalid_target) = self.first_invalid_target(request) {
            return ClaimEvaluation {
                decision: ClaimDecision::Deny,
                reason: "claim request references a missing workspace resource".into(),
                conflicting_targets: vec![invalid_target],
                blocking_target: Some(invalid_target),
                diagnostics: vec!["request target does not exist in current workspace index".into()],
                ..ClaimEvaluation::default()
            };
        }

        for active in &self.active_requests {
            if active.id == request.id { continue; }
            if !self.claims_compatible_for_index(request, active) {
                let conflicts = conflicting_targets(request, active);
                return ClaimEvaluation {
                    decision: ClaimDecision::Deny,
                    reason: describe_conflict_reason("active request", &conflicts),
                    conflicting_claim_id: Some(active.id),
                    conflicting_lease_id: None,
                    conflicting_targets: conflicts.clone(),
                    blocking_target: conflicts.first().copied(),
                    diagnostics: self.build_conflict_diagnostics("active request", &conflicts),
                };
            }
        }

        for lease in &self.active_leases {
            if !lease.active { continue; }
            if !self.claims_compatible_for_index_with_lease(request, lease) {
                let conflicts = conflicting_targets_with_lease(request, lease);
                return ClaimEvaluation {
                    decision: ClaimDecision::Deny,
                    reason: describe_conflict_reason("granted lease", &conflicts),
                    conflicting_claim_id: None,
                    conflicting_lease_id: Some(lease.id),
                    conflicting_targets: conflicts.clone(),
                    blocking_target: conflicts.first().copied(),
                    diagnostics: self.build_conflict_diagnostics("granted lease", &conflicts),
                };
            }
        }

        if let Some(v) = self.first_capacity_violation(request) {
            return capacity_eval(v);
        }
        if let Some(v) = self.first_membership_capacity_violation(request, ClaimTargetKind::Node) {
            return capacity_eval(v);
        }
        if let Some(v) = self.first_edge_capacity_violation(request) {
            return capacity_eval(v);
        }
        if let Some(v) = self.first_membership_capacity_violation(request, ClaimTargetKind::Edge) {
            return capacity_eval(v);
        }

        ClaimEvaluation {
            decision: ClaimDecision::Grant,
            reason: "claim is compatible with current state".into(),
            diagnostics: vec!["request passed conflict and capacity checks".into()],
            ..ClaimEvaluation::default()
        }
    }

    // -- capacity checks --------------------------------------------------

    fn first_capacity_violation(&self, request: &ClaimRequest) -> Option<CapacityViolation> {
        let index = self.index.as_deref()?;
        if request.access_mode != ClaimAccessMode::Shared { return None; }

        for target in &request.targets {
            if target.kind != ClaimTargetKind::Zone { continue; }
            let zone = index.zone(target.resource_id)?;
            let policy = parse_zone_policy(zone.properties());
            if !policy.capacity_is_explicit || policy.capacity <= 1 { continue; }

            let mut occupant_count: u32 = 1;
            let mut blocking_claim_id: Option<ClaimId> = None;
            let mut blocking_lease_id: Option<LeaseId> = None;

            for active in &self.active_requests {
                if active.id == request.id
                    || active.access_mode != ClaimAccessMode::Shared
                    || !claim_windows_overlap(request.window, active.window)
                    || !self.request_overlaps_zone(active, target.resource_id)
                { continue; }
                occupant_count += 1;
                if blocking_claim_id.is_none() {
                    blocking_claim_id = Some(active.id);
                }
            }

            for lease in &self.active_leases {
                if !lease.active
                    || lease.access_mode != ClaimAccessMode::Shared
                    || !claim_windows_overlap(request.window, lease_window(lease))
                    || !self.lease_overlaps_zone(lease, target.resource_id)
                { continue; }
                occupant_count += 1;
                if blocking_lease_id.is_none() {
                    blocking_lease_id = Some(lease.id);
                }
            }

            if occupant_count as u64 > policy.capacity {
                let mut diagnostics = vec!["zone capacity limit reached".into()];
                self.append_target_diagnostics(&mut diagnostics, *target);
                diagnostics.push(format!("configured capacity={}", policy.capacity));
                diagnostics.push(format!("observed occupancy={}", occupant_count));
                return Some(CapacityViolation {
                    target: *target,
                    reason: "shared zone capacity exceeded".into(),
                    conflicting_claim_id: blocking_claim_id,
                    conflicting_lease_id: blocking_lease_id,
                    diagnostics,
                });
            }
        }
        None
    }

    fn first_edge_capacity_violation(&self, request: &ClaimRequest) -> Option<CapacityViolation> {
        let index = self.index.as_deref()?;
        if request.access_mode != ClaimAccessMode::Shared { return None; }

        for target in &request.targets {
            if target.kind != ClaimTargetKind::Edge { continue; }
            let edge = index.edge(target.resource_id)?;
            let semantics = parse_edge_traffic_semantics(&edge.properties, false);
            if !semantics.capacity_is_explicit
                || semantics.capacity.unwrap_or(0) <= 1
            { continue; }
            let cap = semantics.capacity.unwrap();

            let mut occupant_count: u32 = 1;
            let mut blocking_claim_id: Option<ClaimId> = None;
            let mut blocking_lease_id: Option<LeaseId> = None;

            for active in &self.active_requests {
                if active.id == request.id
                    || active.access_mode != ClaimAccessMode::Shared
                    || !claim_windows_overlap(request.window, active.window)
                    || !request_contains_target(active, *target)
                { continue; }
                occupant_count += 1;
                if blocking_claim_id.is_none() { blocking_claim_id = Some(active.id); }
            }
            for lease in &self.active_leases {
                if !lease.active
                    || lease.access_mode != ClaimAccessMode::Shared
                    || !claim_windows_overlap(request.window, lease_window(lease))
                    || !lease_contains_target(lease, *target)
                { continue; }
                occupant_count += 1;
                if blocking_lease_id.is_none() { blocking_lease_id = Some(lease.id); }
            }

            if occupant_count as u64 > cap {
                let mut diagnostics = vec!["edge capacity limit reached".into()];
                self.append_target_diagnostics(&mut diagnostics, *target);
                diagnostics.push(format!("configured capacity={}", cap));
                diagnostics.push(format!("observed occupancy={}", occupant_count));
                return Some(CapacityViolation {
                    target: *target,
                    reason: "shared edge capacity exceeded".into(),
                    conflicting_claim_id: blocking_claim_id,
                    conflicting_lease_id: blocking_lease_id,
                    diagnostics,
                });
            }
        }
        None
    }

    fn first_membership_capacity_violation(
        &self,
        request: &ClaimRequest,
        kind: ClaimTargetKind,
    ) -> Option<CapacityViolation> {
        let index = self.index.as_deref()?;
        if request.access_mode != ClaimAccessMode::Shared { return None; }

        for target in &request.targets {
            if target.kind != kind { continue; }
            let target_zones = match kind {
                ClaimTargetKind::Node => index.zones_of_node(target.resource_id),
                ClaimTargetKind::Edge => index.zones_of_edge(target.resource_id),
                _ => continue,
            };
            for zone in target_zones {
                let policy = parse_zone_policy(zone.properties());
                if !policy.capacity_is_explicit || policy.capacity <= 1 { continue; }

                let mut occupant_count: u32 = 1;
                let mut blocking_claim_id: Option<ClaimId> = None;
                let mut blocking_lease_id: Option<LeaseId> = None;

                for active in &self.active_requests {
                    if active.id == request.id
                        || active.access_mode != ClaimAccessMode::Shared
                        || !claim_windows_overlap(request.window, active.window)
                        || !self.request_contains_resource_in_zone(active, kind, zone.id())
                    { continue; }
                    occupant_count += 1;
                    if blocking_claim_id.is_none() { blocking_claim_id = Some(active.id); }
                }
                for lease in &self.active_leases {
                    if !lease.active
                        || lease.access_mode != ClaimAccessMode::Shared
                        || !claim_windows_overlap(request.window, lease_window(lease))
                        || !self.lease_contains_resource_in_zone(lease, kind, zone.id())
                    { continue; }
                    occupant_count += 1;
                    if blocking_lease_id.is_none() { blocking_lease_id = Some(lease.id); }
                }

                if occupant_count as u64 > policy.capacity {
                    let zone_target = ClaimTarget {
                        kind: ClaimTargetKind::Zone,
                        resource_id: zone.id(),
                    };
                    let reason = match kind {
                        ClaimTargetKind::Node => "shared node-zone capacity exceeded",
                        ClaimTargetKind::Edge => "shared edge-zone capacity exceeded",
                        _ => "shared capacity exceeded",
                    };
                    let head_diag = match kind {
                        ClaimTargetKind::Node => "node claims exceed containing zone capacity",
                        ClaimTargetKind::Edge => "edge claims exceed containing zone capacity",
                        _ => "claims exceed containing zone capacity",
                    };
                    let mut diagnostics = vec![head_diag.into()];
                    self.append_target_diagnostics(&mut diagnostics, zone_target);
                    diagnostics.push(format!("configured capacity={}", policy.capacity));
                    diagnostics.push(format!("observed occupancy={}", occupant_count));
                    return Some(CapacityViolation {
                        target: zone_target,
                        reason: reason.into(),
                        conflicting_claim_id: blocking_claim_id,
                        conflicting_lease_id: blocking_lease_id,
                        diagnostics,
                    });
                }
            }
        }
        None
    }

    fn first_invalid_target(&self, request: &ClaimRequest) -> Option<ClaimTarget> {
        let index = self.index.as_deref()?;
        for target in &request.targets {
            let missing = match target.kind {
                ClaimTargetKind::Zone => index.zone(target.resource_id).is_none(),
                ClaimTargetKind::Node => index.node(target.resource_id).is_none(),
                ClaimTargetKind::Edge => index.edge(target.resource_id).is_none(),
            };
            if missing { return Some(*target); }
        }
        None
    }

    fn append_target_diagnostics(&self, diagnostics: &mut Vec<String>, target: ClaimTarget) {
        let Some(index) = self.index.as_deref() else { return };
        if target.kind == ClaimTargetKind::Zone {
            let Some(zone) = index.zone(target.resource_id) else { return };
            let policy = parse_zone_policy(zone.properties());
            if policy.blocks_entry_without_grant
                || policy.blocks_traversal_without_grant
                || policy.blocked.unwrap_or(false)
            {
                diagnostics.push("zone blocks traversal without a grant".into());
            }
            if policy.kind == ZonePolicyKind::Corridor {
                diagnostics.push("zone is treated as a corridor".into());
            }
            if policy.waiting_allowed == Some(false) {
                diagnostics.push("zone does not allow waiting".into());
            }
            if policy.stop_allowed == Some(false) {
                diagnostics.push("zone does not allow stopping".into());
            }
            if let Some(sw) = policy.schedule_window.as_ref() {
                diagnostics.push(format!("zone schedule window={sw}"));
            }
            if let Some(s) = policy.speed_limit {
                diagnostics.push(format!("zone speed limit={s}"));
            }
            return;
        }
        if target.kind == ClaimTargetKind::Edge {
            let Some(edge) = index.edge(target.resource_id) else { return };
            let zone_policies: Vec<_> = index
                .zones_of_edge(target.resource_id)
                .into_iter()
                .map(|z| parse_zone_policy(z.properties()))
                .collect();
            let semantics = crate::policy::derive_effective_edge_semantics(
                &edge.properties, false, &zone_policies);
            if semantics.blocked.unwrap_or(false) {
                diagnostics.push("edge is blocked by traffic policy".into());
            }
            if semantics.lane_type.as_deref() == Some("corridor") {
                diagnostics.push("edge is treated as a corridor".into());
            }
            if semantics.waiting_allowed == Some(false) {
                diagnostics.push("edge does not allow waiting".into());
            }
            if semantics.stop_allowed == Some(false) {
                diagnostics.push("edge does not allow stopping".into());
            }
            if let Some(sw) = semantics.schedule_window.as_ref() {
                diagnostics.push(format!("edge schedule window={sw}"));
            }
            if let Some(s) = semantics.speed_limit {
                diagnostics.push(format!("edge speed limit={s}"));
            }
        }
    }

    fn build_conflict_diagnostics(
        &self,
        source: &str,
        conflicts: &[ClaimTarget],
    ) -> Vec<String> {
        let mut diagnostics = vec![format!("collision detected with {source}")];
        if let Some(first) = conflicts.first() {
            diagnostics.push(format!(
                "blocking {} id={}", target_kind_name(first.kind), first.resource_id,
            ));
            self.append_target_diagnostics(&mut diagnostics, *first);
        }
        diagnostics
    }

    fn claims_compatible_for_index(&self, lhs: &ClaimRequest, rhs: &ClaimRequest) -> bool {
        if self.index.is_none() {
            return Self::claims_compatible(lhs, rhs);
        }
        self.zone_claims_compatible_with_index(lhs, rhs)
            && self.spatial_claims_compatible_with_index(lhs, rhs, ClaimTargetKind::Node)
            && self.spatial_claims_compatible_with_index(lhs, rhs, ClaimTargetKind::Edge)
    }

    fn claims_compatible_for_index_with_lease(
        &self,
        request: &ClaimRequest,
        lease: &Lease,
    ) -> bool {
        let view = lease_as_request_view(lease);
        self.claims_compatible_for_index(request, &view)
    }

    fn zone_claims_compatible_with_index(
        &self,
        lhs: &ClaimRequest,
        rhs: &ClaimRequest,
    ) -> bool {
        let Some(index) = self.index.as_deref() else { return true };
        for lhs_t in &lhs.targets {
            if lhs_t.kind != ClaimTargetKind::Zone { continue; }
            for rhs_t in &rhs.targets {
                if rhs_t.kind != ClaimTargetKind::Zone { continue; }
                if !zones_overlap(index, lhs_t.resource_id, rhs_t.resource_id) { continue; }
                if !claim_windows_overlap(lhs.window, rhs.window) { continue; }
                let policy = overlapping_zone_policy(index, lhs_t.resource_id, rhs_t.resource_id);
                let both_shared = lhs.access_mode == ClaimAccessMode::Shared
                    && rhs.access_mode == ClaimAccessMode::Shared;
                if both_shared && policy.capacity > 1 { continue; }
                return false;
            }
        }
        true
    }

    fn spatial_claims_compatible_with_index(
        &self,
        lhs: &ClaimRequest,
        rhs: &ClaimRequest,
        kind: ClaimTargetKind,
    ) -> bool {
        let Some(index) = self.index.as_deref() else {
            return target_kind_compatible(lhs, rhs, kind);
        };
        for lhs_t in &lhs.targets {
            if lhs_t.kind != kind { continue; }
            for rhs_t in &rhs.targets {
                if rhs_t.kind != kind { continue; }
                if !claim_windows_overlap(lhs.window, rhs.window) { continue; }

                if lhs_t.resource_id == rhs_t.resource_id {
                    if lhs.access_mode == ClaimAccessMode::Exclusive
                        || rhs.access_mode == ClaimAccessMode::Exclusive
                    { return false; }
                    continue;
                }

                if shared_constrained_zone(index, kind, lhs_t.resource_id, rhs_t.resource_id) {
                    return false;
                }
            }
        }
        true
    }

    fn request_contains_resource_in_zone(
        &self,
        request: &ClaimRequest,
        kind: ClaimTargetKind,
        zone_id: Uuid,
    ) -> bool {
        let Some(index) = self.index.as_deref() else { return false };
        for candidate in &request.targets {
            if candidate.kind != kind { continue; }
            let zones = match kind {
                ClaimTargetKind::Node => index.zones_of_node(candidate.resource_id),
                ClaimTargetKind::Edge => index.zones_of_edge(candidate.resource_id),
                _ => continue,
            };
            if zones.iter().any(|z| z.id() == zone_id) { return true; }
        }
        false
    }

    fn lease_contains_resource_in_zone(
        &self,
        lease: &Lease,
        kind: ClaimTargetKind,
        zone_id: Uuid,
    ) -> bool {
        let Some(index) = self.index.as_deref() else { return false };
        for candidate in &lease.targets {
            if candidate.kind != kind { continue; }
            let zones = match kind {
                ClaimTargetKind::Node => index.zones_of_node(candidate.resource_id),
                ClaimTargetKind::Edge => index.zones_of_edge(candidate.resource_id),
                _ => continue,
            };
            if zones.iter().any(|z| z.id() == zone_id) { return true; }
        }
        false
    }

    fn request_overlaps_zone(&self, request: &ClaimRequest, zone_id: Uuid) -> bool {
        let Some(index) = self.index.as_deref() else { return false };
        for target in &request.targets {
            if target.kind == ClaimTargetKind::Zone
                && zones_overlap(index, target.resource_id, zone_id)
            { return true; }
        }
        false
    }

    fn lease_overlaps_zone(&self, lease: &Lease, zone_id: Uuid) -> bool {
        let Some(index) = self.index.as_deref() else { return false };
        for target in &lease.targets {
            if target.kind == ClaimTargetKind::Zone
                && zones_overlap(index, target.resource_id, zone_id)
            { return true; }
        }
        false
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

struct CapacityViolation {
    target: ClaimTarget,
    reason: String,
    conflicting_claim_id: Option<ClaimId>,
    conflicting_lease_id: Option<LeaseId>,
    diagnostics: Vec<String>,
}

fn capacity_eval(v: CapacityViolation) -> ClaimEvaluation {
    ClaimEvaluation {
        decision: ClaimDecision::Deny,
        reason: v.reason,
        conflicting_claim_id: v.conflicting_claim_id,
        conflicting_lease_id: v.conflicting_lease_id,
        conflicting_targets: vec![v.target],
        blocking_target: Some(v.target),
        diagnostics: v.diagnostics,
    }
}

fn target_kind_compatible(
    lhs: &ClaimRequest,
    rhs: &ClaimRequest,
    kind: ClaimTargetKind,
) -> bool {
    for lhs_t in &lhs.targets {
        if lhs_t.kind != kind { continue; }
        for rhs_t in &rhs.targets {
            if rhs_t.kind != kind || lhs_t.resource_id != rhs_t.resource_id { continue; }
            if lhs.access_mode == ClaimAccessMode::Exclusive
                || rhs.access_mode == ClaimAccessMode::Exclusive
            { return false; }
        }
    }
    true
}

fn lease_as_request_view(lease: &Lease) -> ClaimRequest {
    let mut window = ClaimWindow::default();
    window.end_tick = lease.expires_at_tick;
    ClaimRequest {
        access_mode: lease.access_mode,
        window,
        targets: lease.targets.clone(),
        ..ClaimRequest::default()
    }
}

fn conflicting_targets(lhs: &ClaimRequest, rhs: &ClaimRequest) -> Vec<ClaimTarget> {
    let mut out = Vec::new();
    for lhs_t in &lhs.targets {
        for rhs_t in &rhs.targets {
            if lhs_t.kind == rhs_t.kind && lhs_t.resource_id == rhs_t.resource_id {
                out.push(*lhs_t);
            }
        }
    }
    out
}

fn conflicting_targets_with_lease(request: &ClaimRequest, lease: &Lease) -> Vec<ClaimTarget> {
    let view = lease_as_request_view(lease);
    conflicting_targets(request, &view)
}

fn target_kind_name(kind: ClaimTargetKind) -> &'static str {
    match kind {
        ClaimTargetKind::Zone => "zone",
        ClaimTargetKind::Node => "node",
        ClaimTargetKind::Edge => "edge",
    }
}

fn describe_conflict_reason(source: &str, conflicts: &[ClaimTarget]) -> String {
    if conflicts.is_empty() {
        return format!("conflicts with {source}");
    }
    format!("conflicts with {source} on {} target", target_kind_name(conflicts[0].kind))
}

fn request_contains_target(request: &ClaimRequest, target: ClaimTarget) -> bool {
    request.targets.iter().any(|t| t.kind == target.kind && t.resource_id == target.resource_id)
}

fn lease_contains_target(lease: &Lease, target: ClaimTarget) -> bool {
    lease.targets.iter().any(|t| t.kind == target.kind && t.resource_id == target.resource_id)
}

fn lease_window(lease: &Lease) -> ClaimWindow {
    ClaimWindow {
        start_tick: lease.granted_at_tick,
        end_tick: lease.expires_at_tick,
    }
}

fn claim_windows_overlap(lhs: ClaimWindow, rhs: ClaimWindow) -> bool {
    let lhs_start = lhs.start_tick.unwrap_or(0);
    let rhs_start = rhs.start_tick.unwrap_or(0);
    let lhs_end = lhs.end_tick.unwrap_or(u64::MAX);
    let rhs_end = rhs.end_tick.unwrap_or(u64::MAX);
    lhs_start <= rhs_end && rhs_start <= lhs_end
}

fn zones_overlap(index: &WorkspaceIndex, lhs: Uuid, rhs: Uuid) -> bool {
    if lhs == rhs { return true; }
    if index.ancestor_zones(lhs).iter().any(|a| a.id() == rhs) { return true; }
    if index.ancestor_zones(rhs).iter().any(|a| a.id() == lhs) { return true; }
    false
}

fn overlapping_zone_policy(
    index: &WorkspaceIndex,
    lhs: Uuid,
    rhs: Uuid,
) -> crate::policy::ZonePolicy {
    if let Some(lhs_zone) = index.zone(lhs) {
        if lhs == rhs {
            return parse_zone_policy(lhs_zone.properties());
        }
        if let Some(parent) = index.parent_zone(lhs) {
            if parent.id() == rhs {
                return parse_zone_policy(lhs_zone.properties());
            }
        }
    }
    if let Some(rhs_zone) = index.zone(rhs) {
        if let Some(parent) = index.parent_zone(rhs) {
            if parent.id() == lhs {
                return parse_zone_policy(rhs_zone.properties());
            }
        }
    }
    crate::policy::ZonePolicy::empty()
}

fn shared_constrained_zone(
    index: &WorkspaceIndex,
    kind: ClaimTargetKind,
    lhs_resource: Uuid,
    rhs_resource: Uuid,
) -> bool {
    let lhs_zones = match kind {
        ClaimTargetKind::Node => index.zones_of_node(lhs_resource),
        ClaimTargetKind::Edge => index.zones_of_edge(lhs_resource),
        _ => return false,
    };
    let rhs_zones = match kind {
        ClaimTargetKind::Node => index.zones_of_node(rhs_resource),
        ClaimTargetKind::Edge => index.zones_of_edge(rhs_resource),
        _ => return false,
    };

    for lhs_zone in &lhs_zones {
        let lhs_policy = parse_zone_policy(lhs_zone.properties());
        for rhs_zone in &rhs_zones {
            if lhs_zone.id() != rhs_zone.id() { continue; }
            let rhs_policy = parse_zone_policy(rhs_zone.properties());
            let effective_capacity = lhs_policy.capacity.min(rhs_policy.capacity);
            let explicitly_constrained =
                lhs_policy.capacity_is_explicit || rhs_policy.capacity_is_explicit;
            if lhs_policy.kind == ZonePolicyKind::ExclusiveAccess
                || rhs_policy.kind == ZonePolicyKind::ExclusiveAccess
                || (explicitly_constrained && effective_capacity <= 1)
            { return true; }
        }
    }
    false
}
