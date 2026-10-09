//! `ClaimManager` — request and lease lifecycle plus conflict / capacity
//! evaluation. Port of `include/syncbot/claim_manager.hpp`.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use uuid::Uuid;

use crate::core::ids::{ClaimId, LeaseId, RobotId};
use crate::index::WorkspaceIndex;
use crate::policy::{ZonePolicy, ZonePolicyKind};

use super::{
    ClaimAccessMode, ClaimDecision, ClaimEvaluation, ClaimRequest, ClaimTarget, ClaimTargetKind,
    ClaimWindow, Lease, LeaseDisposition,
};

pub struct ClaimManager {
    index: Option<Arc<WorkspaceIndex>>,
    active_requests: Vec<ClaimRequest>,
    active_leases: Vec<Lease>,
    released_leases: Vec<Lease>,
    /// Monotonic source for server-minted claim ids. Never goes backward, so a
    /// minted id can never collide with a live id even if attacker-influenced
    /// lease claim_ids sit near `u64::MAX`. See `next_request_id`.
    next_id: AtomicU64,
}

impl Default for ClaimManager {
    fn default() -> Self {
        Self {
            index: None,
            active_requests: Vec::new(),
            active_leases: Vec::new(),
            released_leases: Vec::new(),
            next_id: AtomicU64::new(1),
        }
    }
}

impl ClaimManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_index(index: Arc<WorkspaceIndex>) -> Self {
        Self {
            index: Some(index),
            ..Self::default()
        }
    }

    pub fn empty(&self) -> bool {
        self.active_requests.is_empty() && self.active_leases.is_empty()
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

    pub fn request_count(&self) -> usize {
        self.active_requests.len()
    }
    pub fn lease_count(&self) -> usize {
        self.active_leases.len()
    }

    pub fn requests(&self) -> &[ClaimRequest] {
        &self.active_requests
    }
    pub fn leases(&self) -> &[Lease] {
        &self.active_leases
    }
    pub fn released_leases(&self) -> &[Lease] {
        &self.released_leases
    }

    pub fn bind_index(&mut self, index: Arc<WorkspaceIndex>) {
        self.index = Some(index);
    }

    pub fn clear(&mut self) {
        self.active_requests.clear();
        self.active_leases.clear();
        self.released_leases.clear();
    }

    // -- persistence ------------------------------------------------------

    /// Capture the owned ledger (requests, leases, released leases) plus the
    /// current value of the monotonic `next_id` counter into a serializable
    /// snapshot. The bound `index` is NOT serialized. See [`crate::persist`].
    pub fn snapshot(&self) -> crate::persist::ClaimManagerSnapshot {
        crate::persist::ClaimManagerSnapshot {
            active_requests: self.active_requests.clone(),
            active_leases: self.active_leases.clone(),
            released_leases: self.released_leases.clone(),
            next_id: self.next_id.load(Ordering::Relaxed),
        }
    }

    /// Rebuild a manager from a snapshot, re-attaching the freshly-loaded
    /// `index`. The `next_id` atomic is reconstructed from the stored value
    /// (floored at 1) so server-minted claim ids keep increasing across a
    /// restart.
    pub fn restore(
        snapshot: crate::persist::ClaimManagerSnapshot,
        index: Option<Arc<WorkspaceIndex>>,
    ) -> Self {
        Self {
            index,
            active_requests: snapshot.active_requests,
            active_leases: snapshot.active_leases,
            released_leases: snapshot.released_leases,
            next_id: AtomicU64::new(snapshot.next_id.max(1)),
        }
    }

    // -- request lifecycle ------------------------------------------------

    pub fn add_request(&mut self, request: ClaimRequest) {
        self.upsert_request(request);
    }

    pub fn upsert_request(&mut self, request: ClaimRequest) {
        if let Some(slot) = self.active_requests.iter_mut().find(|r| r.id == request.id) {
            *slot = request;
            return;
        }
        self.active_requests.push(request);
    }

    /// Insert `request`, replacing the same robot's existing claim over the
    /// same targets rather than appending a second ledger entry.
    ///
    /// The flat wire mints a fresh claim id per call, so a robot re-claiming
    /// on a timer would otherwise grow `active_requests` without bound — and
    /// every later evaluation pays for those duplicates. Returns whether an
    /// existing claim was replaced rather than a new one added.
    pub fn upsert_request_for_robot(&mut self, request: ClaimRequest) -> bool {
        if let Some(slot) = self.active_requests.iter_mut().find(|active| {
            active.robot_id == request.robot_id
                && same_target_set(&active.targets, &request.targets)
        }) {
            *slot = request;
            return true;
        }
        self.active_requests.push(request);
        false
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

    /// A claim id not currently used by any active request or lease. Used by
    /// the flat wire, where the server (not the client) mints claim ids.
    ///
    /// Backed by a monotonic counter that never goes backward, so it cannot
    /// overflow-panic or wrap into a live id even when attacker-influenced lease
    /// `claim_id`s sit near `u64::MAX`. The counter is also floored above any
    /// existing id so a minted id is unique against current requests/leases.
    /// (Takes `&self`; the counter is atomic so the signature is unchanged and
    /// callers holding an immutable manager reference still compile.)
    pub fn next_request_id(&self) -> ClaimId {
        let max_req = self.active_requests.iter().map(|r| r.id.raw()).max();
        let max_lease = self.active_leases.iter().map(|l| l.claim_id.raw()).max();
        // One past the largest existing id, saturating so an id at u64::MAX
        // cannot overflow. Zero is reserved for "no ids yet" → floor of 1.
        let floor = max_req
            .max(max_lease)
            .map_or(1, |m| m.saturating_add(1))
            .max(1);
        // Advance the monotonic counter to at least `floor` and take that value,
        // in one compare-exchange so two concurrent callers cannot mint the same
        // id. (Callers hold the coordinator write lock today, but the `&self`
        // signature promises more than that arrangement does.)
        let candidate = self
            .next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.max(floor).saturating_add(1))
            })
            .map(|previous| previous.max(floor))
            .unwrap_or(floor);
        ClaimId::new(candidate)
    }

    /// Release (remove) the first active request held by `robot_id` whose
    /// targets include `resource_id`. Returns whether one was removed. Used by
    /// the flat release-by-robot+resource path.
    pub fn release_request_for_robot_target(
        &mut self,
        robot_id: RobotId,
        resource_id: Uuid,
    ) -> bool {
        if let Some(pos) = self.active_requests.iter().position(|r| {
            r.robot_id == robot_id && r.targets.iter().any(|t| t.resource_id == resource_id)
        }) {
            self.active_requests.remove(pos);
            return true;
        }
        false
    }

    // -- lease lifecycle --------------------------------------------------

    pub fn add_lease(&mut self, lease: Lease) {
        self.upsert_lease(lease);
    }

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
            if self.active_leases[i].robot_id != robot_id {
                i += 1;
                continue;
            }
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

    /// Drop active requests whose claim window has closed at `current_tick`.
    ///
    /// The flat protocol's `lease_time` produces a bounded window on a
    /// *request* (it never mints a `Lease`), so without this the window only
    /// narrowed conflict detection and the claim itself lived forever. A
    /// request with an open-ended window is untouched.
    pub fn expire_requests(&mut self, current_tick: u64) -> u64 {
        let before = self.active_requests.len();
        self.active_requests
            .retain(|request| !matches!(request.window.end_tick, Some(end) if end <= current_tick));
        (before - self.active_requests.len()) as u64
    }

    pub fn expire_leases(&mut self, current_tick: u64) -> u64 {
        let mut expired = 0u64;
        let mut i = 0;
        while i < self.active_leases.len() {
            let exp = self.active_leases[i].expires_at_tick;
            if !matches!(exp, Some(t) if t <= current_tick) {
                i += 1;
                continue;
            }
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
            if expires_at_tick.is_some() {
                slot.expires_at_tick = expires_at_tick;
            }
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
    pub fn has_request(&self, id: ClaimId) -> bool {
        self.find_request(id).is_some()
    }

    pub fn find_lease(&self, id: LeaseId) -> Option<&Lease> {
        self.active_leases.iter().find(|l| l.id == id)
    }
    pub fn has_lease(&self, id: LeaseId) -> bool {
        self.find_lease(id).is_some()
    }

    pub fn find_released_lease(&self, id: LeaseId) -> Option<&Lease> {
        self.released_leases.iter().find(|l| l.id == id)
    }

    pub fn leases_for_robot(&self, robot_id: RobotId) -> Vec<&Lease> {
        self.active_leases
            .iter()
            .filter(|l| l.robot_id == robot_id)
            .collect()
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
                diagnostics: vec![
                    "request target does not exist in current workspace index".into(),
                ],
                ..ClaimEvaluation::default()
            };
        }

        for active in &self.active_requests {
            // A robot does not compete with itself. Its own earlier claim is
            // ground it already holds, so re-asserting it is a re-acquisition,
            // not a conflict — without this a rolling-horizon robot is refused
            // by its own previous slice.
            if active.id == request.id || active.robot_id == request.robot_id {
                continue;
            }
            if !self.claims_compatible_for_index(request, active) {
                let conflicts = self.conflicts_including_cross_level(request, active);
                return ClaimEvaluation {
                    decision: ClaimDecision::Deny,
                    reason: describe_conflict_reason("active request", &conflicts),
                    conflicting_claim_id: Some(active.id),
                    conflicting_lease_id: None,
                    conflicting_targets: conflicts.clone(),
                    blocking_target: conflicts.first().copied(),
                    diagnostics: self.build_conflict_diagnostics("active request", &conflicts),
                    denied_by_capacity: false,
                };
            }
        }

        for lease in &self.active_leases {
            if !lease.active || lease.robot_id == request.robot_id {
                continue;
            }
            if !self.claims_compatible_for_index_with_lease(request, lease) {
                let conflicts = self.conflicts_including_cross_level_with_lease(request, lease);
                return ClaimEvaluation {
                    decision: ClaimDecision::Deny,
                    reason: describe_conflict_reason("granted lease", &conflicts),
                    conflicting_claim_id: None,
                    conflicting_lease_id: Some(lease.id),
                    conflicting_targets: conflicts.clone(),
                    blocking_target: conflicts.first().copied(),
                    diagnostics: self.build_conflict_diagnostics("granted lease", &conflicts),
                    denied_by_capacity: false,
                };
            }
        }

        if let Some(v) = self.first_edge_capacity_violation(request) {
            return capacity_eval(v);
        }
        if let Some(v) = self.first_occupancy_violation(request) {
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

    fn first_edge_capacity_violation(&self, request: &ClaimRequest) -> Option<CapacityViolation> {
        let index = self.index.as_deref()?;
        if request.access_mode != ClaimAccessMode::Shared {
            return None;
        }

        for target in &request.targets {
            if target.kind != ClaimTargetKind::Edge {
                continue;
            }
            let semantics = index.edge_semantics(target.resource_id)?;
            if !semantics.capacity_is_explicit || semantics.capacity.unwrap_or(0) <= 1 {
                continue;
            }
            let cap = semantics.capacity.unwrap();

            let mut occupants: BTreeSet<RobotId> = BTreeSet::new();
            occupants.insert(request.robot_id);
            let mut blocking_claim_id: Option<ClaimId> = None;
            let mut blocking_lease_id: Option<LeaseId> = None;

            for active in &self.active_requests {
                if active.id == request.id
                    || active.robot_id == request.robot_id
                    || active.access_mode != ClaimAccessMode::Shared
                    || !claim_windows_overlap(request.window, active.window)
                    || !request_contains_target(active, *target)
                {
                    continue;
                }
                if occupants.insert(active.robot_id) && blocking_claim_id.is_none() {
                    blocking_claim_id = Some(active.id);
                }
            }
            for lease in &self.active_leases {
                if !lease.active
                    || lease.robot_id == request.robot_id
                    || lease.access_mode != ClaimAccessMode::Shared
                    || !claim_windows_overlap(request.window, lease_window(lease))
                    || !lease_contains_target(lease, *target)
                {
                    continue;
                }
                if occupants.insert(lease.robot_id) && blocking_lease_id.is_none() {
                    blocking_lease_id = Some(lease.id);
                }
            }

            if occupants.len() as u64 > cap {
                let mut diagnostics = vec!["edge capacity limit reached".into()];
                self.append_target_diagnostics(&mut diagnostics, *target);
                diagnostics.push(format!("configured capacity={}", cap));
                diagnostics.push(format!("observed occupancy={}", occupants.len()));
                return Some(CapacityViolation {
                    target: *target,
                    reason: "shared edge capacity exceeded".into(),
                    conflicting_claim_id: blocking_claim_id,
                    conflicting_lease_id: blocking_lease_id,
                    diagnostics,
                    denied_by_capacity: true,
                });
            }
        }
        None
    }

    /// Every zone this request puts the robot inside, with ancestors.
    ///
    /// A node or edge target contributes the zones containing it — driving
    /// through a zone is presence, not ownership, which is what lets a second
    /// non-interfering path through the same zone proceed while still blocking
    /// anyone who wants the zone itself. A zone target contributes that zone
    /// directly. Both forms are occupancy and are counted the same way, so a
    /// robot claiming a zone and a robot merely crossing it compete for the
    /// same slots. See PLAN Milestone 1.2.
    fn occupied_zones(&self, request: &ClaimRequest) -> Vec<Uuid> {
        let Some(index) = self.index.as_deref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut seen: BTreeSet<Uuid> = BTreeSet::new();
        let add = |zone_id: Uuid, out: &mut Vec<Uuid>, seen: &mut BTreeSet<Uuid>| {
            if seen.insert(zone_id) {
                out.push(zone_id);
            }
            for ancestor in index.ancestor_zones(zone_id) {
                if seen.insert(ancestor.id()) {
                    out.push(ancestor.id());
                }
            }
        };
        for target in &request.targets {
            match target.kind {
                ClaimTargetKind::Zone => add(target.resource_id, &mut out, &mut seen),
                ClaimTargetKind::Node => {
                    for zone in index.zones_of_node(target.resource_id) {
                        add(zone.id(), &mut out, &mut seen);
                    }
                }
                ClaimTargetKind::Edge => {
                    for zone in index.zones_of_edge(target.resource_id) {
                        add(zone.id(), &mut out, &mut seen);
                    }
                }
            }
        }
        out
    }

    /// Deny when a zone already holds as many distinct robots as it admits.
    ///
    /// Occupancy counts *robots*, not claims, so a robot re-claiming or holding
    /// several targets in one zone still fills one slot. This is the single
    /// place zone occupancy is decided: whether a robot is present by claiming
    /// the zone, a node in it, or an edge through it makes no difference to the
    /// count.
    fn first_occupancy_violation(&self, request: &ClaimRequest) -> Option<CapacityViolation> {
        let index = self.index.as_deref()?;
        for zone_id in self.occupied_zones(request) {
            let Some(policy) = index.zone_policy(zone_id) else {
                continue;
            };
            let Some(limit) = zone_occupancy_limit(policy) else {
                continue;
            };

            let mut occupants: BTreeSet<RobotId> = BTreeSet::new();
            occupants.insert(request.robot_id);
            let mut blocking_claim_id: Option<ClaimId> = None;
            let mut blocking_lease_id: Option<LeaseId> = None;

            for active in &self.active_requests {
                if active.id == request.id
                    || active.robot_id == request.robot_id
                    || !claim_windows_overlap(request.window, active.window)
                    || !targets_occupy_zone(index, &active.targets, zone_id)
                {
                    continue;
                }
                if occupants.insert(active.robot_id) && blocking_claim_id.is_none() {
                    blocking_claim_id = Some(active.id);
                }
            }
            for lease in &self.active_leases {
                if !lease.active
                    || lease.robot_id == request.robot_id
                    || !claim_windows_overlap(request.window, lease_window(lease))
                    || !targets_occupy_zone(index, &lease.targets, zone_id)
                {
                    continue;
                }
                if occupants.insert(lease.robot_id) && blocking_lease_id.is_none() {
                    blocking_lease_id = Some(lease.id);
                }
            }

            if occupants.len() as u64 > limit {
                let zone_target = ClaimTarget {
                    kind: ClaimTargetKind::Zone,
                    resource_id: zone_id,
                };
                // A single-occupant zone is a conflict, not a capacity
                // overflow — that is how it has always been reported.
                let single = limit <= 1;
                let mut diagnostics = vec![if single {
                    "zone admits one occupant at a time".to_string()
                } else {
                    "zone occupancy limit reached".to_string()
                }];
                self.append_target_diagnostics(&mut diagnostics, zone_target);
                diagnostics.push(format!("configured capacity={limit}"));
                diagnostics.push(format!("observed occupancy={}", occupants.len()));
                return Some(CapacityViolation {
                    target: zone_target,
                    reason: if single {
                        "conflicts with an occupant of an exclusive zone".into()
                    } else {
                        "zone capacity exceeded".into()
                    },
                    conflicting_claim_id: blocking_claim_id,
                    conflicting_lease_id: blocking_lease_id,
                    diagnostics,
                    denied_by_capacity: !single,
                });
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
            if missing {
                return Some(*target);
            }
        }
        None
    }

    fn append_target_diagnostics(&self, diagnostics: &mut Vec<String>, target: ClaimTarget) {
        let Some(index) = self.index.as_deref() else {
            return;
        };
        if target.kind == ClaimTargetKind::Zone {
            let Some(policy) = index.zone_policy(target.resource_id) else {
                return;
            };
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
            let Some(edge) = index.edge(target.resource_id) else {
                return;
            };
            let zone_policies: Vec<_> = index
                .edge_zone_policies(target.resource_id)
                .into_iter()
                .cloned()
                .collect();
            let semantics = crate::policy::derive_effective_edge_semantics(
                &edge.properties,
                false,
                &zone_policies,
            );
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

    fn build_conflict_diagnostics(&self, source: &str, conflicts: &[ClaimTarget]) -> Vec<String> {
        let mut diagnostics = vec![format!("collision detected with {source}")];
        if let Some(first) = conflicts.first() {
            diagnostics.push(format!(
                "blocking {} id={}",
                target_kind_name(first.kind),
                first.resource_id,
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
            && self.cross_level_compatible_with_index(lhs, rhs)
    }

    /// Targets of `request` that conflict with `other`, INCLUDING cross-level
    /// conflicts (a requested zone whose contained node/edge `other` holds, or a
    /// requested node/edge that lies inside a zone `other` holds). Returns
    /// request-side targets so callers can map them back to what was asked for.
    fn conflicts_including_cross_level(
        &self,
        request: &ClaimRequest,
        other: &ClaimRequest,
    ) -> Vec<ClaimTarget> {
        let mut out = conflicting_targets(request, other);
        if let Some(index) = self.index.as_deref() {
            for rt in &request.targets {
                let already = out
                    .iter()
                    .any(|t| t.kind == rt.kind && t.resource_id == rt.resource_id);
                if already {
                    continue;
                }
                if other
                    .targets
                    .iter()
                    .any(|ot| targets_conflict_cross_level(index, rt, ot))
                {
                    out.push(*rt);
                }
            }
        }
        out
    }

    fn conflicts_including_cross_level_with_lease(
        &self,
        request: &ClaimRequest,
        lease: &Lease,
    ) -> Vec<ClaimTarget> {
        let view = lease_as_request_view(lease);
        self.conflicts_including_cross_level(request, &view)
    }

    /// Cross-level exclusion (the two-level bridge rule): claiming a zone
    /// reserves every node/edge inside it, so a zone claim conflicts with a
    /// node/edge claim that lives within that zone (or any descendant), and
    /// vice versa. Checked both directions.
    fn cross_level_compatible_with_index(&self, lhs: &ClaimRequest, rhs: &ClaimRequest) -> bool {
        let Some(index) = self.index.as_deref() else {
            return true;
        };
        self.zone_vs_spatial_compatible(index, lhs, rhs)
            && self.zone_vs_spatial_compatible(index, rhs, lhs)
    }

    /// Whether the zone targets of `zside` are compatible with the node/edge
    /// targets of `sside` — i.e. no node/edge of `sside` lies inside a zone of
    /// `zside` in a way that conflicts.
    fn zone_vs_spatial_compatible(
        &self,
        index: &WorkspaceIndex,
        zside: &ClaimRequest,
        sside: &ClaimRequest,
    ) -> bool {
        for zone_t in zside
            .targets
            .iter()
            .filter(|t| t.kind == ClaimTargetKind::Zone)
        {
            for res_t in &sside.targets {
                let res_zones = match res_t.kind {
                    ClaimTargetKind::Node => index.zones_of_node(res_t.resource_id),
                    ClaimTargetKind::Edge => index.zones_of_edge(res_t.resource_id),
                    ClaimTargetKind::Zone => continue,
                };
                // node/edge is inside the zone if one of its containing zones
                // IS the zone or has the zone as an ancestor.
                let inside = res_zones
                    .iter()
                    .any(|z| index.zone_contains(zone_t.resource_id, z.id()));
                if !inside {
                    continue;
                }
                if !claim_windows_overlap(zside.window, sside.window) {
                    continue;
                }
                // Both shared on a zone with explicit capacity > 1 may coexist.
                let policy = index
                    .zone_policy(zone_t.resource_id)
                    .cloned()
                    .unwrap_or_default();
                let both_shared = zside.access_mode == ClaimAccessMode::Shared
                    && sside.access_mode == ClaimAccessMode::Shared;
                if both_shared && policy.capacity > 1 {
                    continue;
                }
                return false;
            }
        }
        true
    }

    fn claims_compatible_for_index_with_lease(
        &self,
        request: &ClaimRequest,
        lease: &Lease,
    ) -> bool {
        let view = lease_as_request_view(lease);
        self.claims_compatible_for_index(request, &view)
    }

    fn zone_claims_compatible_with_index(&self, lhs: &ClaimRequest, rhs: &ClaimRequest) -> bool {
        let Some(index) = self.index.as_deref() else {
            return true;
        };
        for lhs_t in &lhs.targets {
            if lhs_t.kind != ClaimTargetKind::Zone {
                continue;
            }
            for rhs_t in &rhs.targets {
                if rhs_t.kind != ClaimTargetKind::Zone {
                    continue;
                }
                if !zones_overlap(index, lhs_t.resource_id, rhs_t.resource_id) {
                    continue;
                }
                if !claim_windows_overlap(lhs.window, rhs.window) {
                    continue;
                }
                let policy = overlapping_zone_policy(index, lhs_t.resource_id, rhs_t.resource_id);
                let both_shared = lhs.access_mode == ClaimAccessMode::Shared
                    && rhs.access_mode == ClaimAccessMode::Shared;
                if both_shared && policy.capacity > 1 {
                    continue;
                }
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
            if lhs_t.kind != kind {
                continue;
            }
            for rhs_t in &rhs.targets {
                if rhs_t.kind != kind {
                    continue;
                }
                if !claim_windows_overlap(lhs.window, rhs.window) {
                    continue;
                }

                if lhs_t.resource_id == rhs_t.resource_id {
                    if lhs.access_mode == ClaimAccessMode::Exclusive
                        || rhs.access_mode == ClaimAccessMode::Exclusive
                    {
                        return false;
                    }
                    continue;
                }

                if shared_constrained_zone(index, kind, lhs_t.resource_id, rhs_t.resource_id) {
                    return false;
                }
            }
        }
        true
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
    /// Report this as reason 3 (CAPACITY) rather than 2 (CONFLICT). A zone
    /// admitting a single occupant is a plain conflict on the wire — it has
    /// always been reported that way, and callers key off the code.
    denied_by_capacity: bool,
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
        denied_by_capacity: v.denied_by_capacity,
    }
}

/// How many distinct robots a zone admits at once. `None` is unlimited.
///
/// This is the capacity dimension of the intent model: an `exclusive` zone
/// admits one occupant, a zone with an explicit capacity admits that many, and
/// an unconstrained zone admits any number — arbitration then happens purely
/// on the nodes and edges they actually use.
fn zone_occupancy_limit(policy: &ZonePolicy) -> Option<u64> {
    if policy.kind == ZonePolicyKind::ExclusiveAccess {
        return Some(1);
    }
    if policy.capacity_is_explicit {
        return Some(policy.capacity);
    }
    None
}

/// Whether `targets` put a robot anywhere inside `zone_id` — a node or edge
/// within it, or a zone target overlapping it.
fn targets_occupy_zone(index: &WorkspaceIndex, targets: &[ClaimTarget], zone_id: Uuid) -> bool {
    targets.iter().any(|target| match target.kind {
        ClaimTargetKind::Zone => zones_overlap(index, target.resource_id, zone_id),
        ClaimTargetKind::Node => index
            .zones_of_node(target.resource_id)
            .iter()
            .any(|zone| index.zone_contains(zone_id, zone.id())),
        ClaimTargetKind::Edge => index
            .zones_of_edge(target.resource_id)
            .iter()
            .any(|zone| index.zone_contains(zone_id, zone.id())),
    })
}

fn target_kind_compatible(lhs: &ClaimRequest, rhs: &ClaimRequest, kind: ClaimTargetKind) -> bool {
    for lhs_t in &lhs.targets {
        if lhs_t.kind != kind {
            continue;
        }
        for rhs_t in &rhs.targets {
            if rhs_t.kind != kind || lhs_t.resource_id != rhs_t.resource_id {
                continue;
            }
            if lhs.access_mode == ClaimAccessMode::Exclusive
                || rhs.access_mode == ClaimAccessMode::Exclusive
            {
                return false;
            }
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

/// Whether targets `a` and `b` conflict across levels: one is a zone and the
/// other a node/edge that lies inside that zone (the zone IS, or is an ancestor
/// of, one of the resource's containing zones).
fn targets_conflict_cross_level(index: &WorkspaceIndex, a: &ClaimTarget, b: &ClaimTarget) -> bool {
    let (zone_t, res_t) = match (a.kind, b.kind) {
        (ClaimTargetKind::Zone, ClaimTargetKind::Node | ClaimTargetKind::Edge) => (a, b),
        (ClaimTargetKind::Node | ClaimTargetKind::Edge, ClaimTargetKind::Zone) => (b, a),
        _ => return false,
    };
    let res_zones = match res_t.kind {
        ClaimTargetKind::Node => index.zones_of_node(res_t.resource_id),
        ClaimTargetKind::Edge => index.zones_of_edge(res_t.resource_id),
        ClaimTargetKind::Zone => return false,
    };
    res_zones
        .iter()
        .any(|z| index.zone_contains(zone_t.resource_id, z.id()))
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
    format!(
        "conflicts with {source} on {} target",
        target_kind_name(conflicts[0].kind)
    )
}

/// Whether two target lists name the same set of resources, order and
/// duplicates aside.
fn same_target_set(lhs: &[ClaimTarget], rhs: &[ClaimTarget]) -> bool {
    lhs.iter().all(|t| rhs.contains(t)) && rhs.iter().all(|t| lhs.contains(t))
}

fn request_contains_target(request: &ClaimRequest, target: ClaimTarget) -> bool {
    request
        .targets
        .iter()
        .any(|t| t.kind == target.kind && t.resource_id == target.resource_id)
}

fn lease_contains_target(lease: &Lease, target: ClaimTarget) -> bool {
    lease
        .targets
        .iter()
        .any(|t| t.kind == target.kind && t.resource_id == target.resource_id)
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
    index.zone_contains(rhs, lhs) || index.zone_contains(lhs, rhs)
}

fn overlapping_zone_policy(
    index: &WorkspaceIndex,
    lhs: Uuid,
    rhs: Uuid,
) -> crate::policy::ZonePolicy {
    if index.zone(lhs).is_some() {
        if lhs == rhs {
            return index.zone_policy(lhs).cloned().unwrap_or_default();
        }
        if let Some(parent) = index.parent_zone(lhs) {
            if parent.id() == rhs {
                return index.zone_policy(lhs).cloned().unwrap_or_default();
            }
        }
    }
    if index.zone(rhs).is_some() {
        if let Some(parent) = index.parent_zone(rhs) {
            if parent.id() == lhs {
                return index.zone_policy(rhs).cloned().unwrap_or_default();
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
        let Some(lhs_policy) = index.zone_policy(lhs_zone.id()) else {
            continue;
        };
        for rhs_zone in &rhs_zones {
            if lhs_zone.id() != rhs_zone.id() {
                continue;
            }
            let Some(rhs_policy) = index.zone_policy(rhs_zone.id()) else {
                continue;
            };
            let effective_capacity = lhs_policy.capacity.min(rhs_policy.capacity);
            let explicitly_constrained =
                lhs_policy.capacity_is_explicit || rhs_policy.capacity_is_explicit;
            if lhs_policy.kind == ZonePolicyKind::ExclusiveAccess
                || rhs_policy.kind == ZonePolicyKind::ExclusiveAccess
                || (explicitly_constrained && effective_capacity <= 1)
            {
                return true;
            }
        }
    }
    false
}
