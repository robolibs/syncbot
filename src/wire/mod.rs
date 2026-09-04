//! Wire transport adapters for `syncbot`.
//!
//! Each submodule is a thin adapter that translates an external wire protocol
//! (REST/JSON, REST/XML) into a canonical datapod call over peerbus, and
//! encodes the reply back to its wire. The flat operations below
//! are what the peerbus core executes; no adapter holds a coordinator handle.
//! See `docs/WRITING_ADAPTER.md` for the frozen contract.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};

use crate::claim::{
    ClaimAccessMode, ClaimDecision, ClaimRequest, ClaimTarget, ClaimTargetKind, ClaimWindow, Lease,
    MissionId,
};
use crate::coordinator::Coordinator;
use crate::core::ids::RobotId;
use crate::core::key::{Key, KeyError};
use crate::index::{NUMERIC_ID_PROPERTY, ResourceRef, ValidationSeverity, WorkspaceIndex};
use crate::robot::RobotState;
use crate::route::{RouteFailure, RoutePlan, plan_route};

#[cfg(feature = "rest")]
pub mod rest;

#[cfg(feature = "peerbus")]
pub mod peerbus;

#[cfg(feature = "xmlt")]
pub mod xmlt;

/// Shared state used by all serving adapters.
#[derive(Clone)]
pub struct ServeState {
    coordinator: Arc<RwLock<Coordinator>>,
    /// Whether a robot may register without presenting a key of its own — in
    /// which case it is bound to the shared [`DEFAULT_KEY`] that every other
    /// keyless robot also uses. Off by default: convenient is not the same as
    /// safe, and an unkeyed fleet is trivially impersonated.
    allow_default_key: bool,
    /// Operator key guarding workspace replacement. Deliberately separate from
    /// robot keys: a robot key authorises claiming one zone, never redrawing
    /// the map every robot is claiming against. `None` disables the push
    /// entirely rather than leaving it open — see [`set_workspace`].
    admin_key: Option<Key>,
    /// Recent decisions, newest last. See [`FleetEvent`].
    ///
    /// Lives here rather than in the coordinator because it is a record of what
    /// the wire was asked and answered, not part of the coordination state: a
    /// refusal changes nothing, so the coordinator rightly forgets it the
    /// instant it replies.
    events: Arc<Mutex<VecDeque<FleetEvent>>>,
}

/// Most resources one claim may name. Evaluation cost is quadratic in this,
/// and it runs under the coordinator write lock.
pub const MAX_CLAIM_TARGETS: usize = 64;

/// Deepest zone nesting a pushed workspace may have. Real workspaces are a
/// handful deep; the cap exists because the tree arrives untrusted.
pub const MAX_ZONE_DEPTH: usize = 64;

/// How many decisions to remember. Enough to cover a busy fleet's last minute
/// or two without the snapshot growing without bound — it is re-sent on every
/// poll.
const MAX_EVENTS: usize = 256;

/// Something the core was asked to do, and what it answered.
///
/// The fleet snapshot otherwise only shows claims that *succeeded*: a denial
/// leaves no trace anywhere, so a client cannot tell "nobody asked" from "three
/// robots were refused". These are the refusals, and the grants that preceded
/// them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetEvent {
    /// Wall clock, same scale as [`FleetSnapshot::now_ms`]. Compare against
    /// that rather than the reader's clock, so ages stay right across hosts.
    pub at_ms: u64,
    pub kind: FleetEventKind,
    pub robot_id: Option<RobotId>,
    /// Zones the request named, resolved to uuids so a client can match them
    /// against its own workspace.
    #[serde(default)]
    pub zone_ids: Vec<uuid::Uuid>,
    /// Zone names as the core knows them, for clients without the workspace.
    #[serde(default)]
    pub zone_names: Vec<String>,
    /// The `FlatReply` reason on a denial; `0` otherwise.
    #[serde(default)]
    pub reason: u8,
    /// Numeric id of whatever blocked a denied claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FleetEventKind {
    Registered,
    Granted,
    Denied,
    Released,
    /// Auto-released because the robot stopped heartbeating.
    Swept,
    /// Auto-released because the claim's `lease_time` ran out.
    Expired,
    WorkspaceReplaced,
}

impl ServeState {
    pub fn new(coordinator: Coordinator) -> Self {
        Self {
            coordinator: Arc::new(RwLock::new(coordinator)),
            allow_default_key: false,
            admin_key: None,
            events: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub fn shared(coordinator: Arc<RwLock<Coordinator>>) -> Self {
        Self {
            coordinator,
            allow_default_key: false,
            admin_key: None,
            events: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Record a decision, dropping the oldest once the ring is full.
    ///
    /// A poisoned lock is swallowed rather than propagated: losing an entry
    /// from an observability log must never fail the operation it describes.
    pub(crate) fn record(&self, event: FleetEvent) {
        let Ok(mut events) = self.events.lock() else {
            return;
        };
        if events.len() >= MAX_EVENTS {
            events.pop_front();
        }
        events.push_back(event);
    }

    /// Recent decisions, oldest first.
    pub fn events(&self) -> Vec<FleetEvent> {
        self.events
            .lock()
            .map(|events| events.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Set the cost of deriving stored key verifiers on the inner
    /// coordinator. See `Coordinator::set_kdf_params`.
    pub fn with_kdf_params(self, params: keylock::kdf::pwhash::Config) -> Self {
        if let Ok(mut coord) = write_coord(&self) {
            coord.set_kdf_params(params);
        }
        self
    }

    /// Allow keyless registration, binding those robots to the shared
    /// [`DEFAULT_KEY`]. Convenient for a closed bench, unsafe anywhere else.
    pub fn with_default_key_allowed(mut self, allow: bool) -> Self {
        self.allow_default_key = allow;
        self
    }

    /// Whether keyless registration is permitted.
    pub fn default_key_allowed(&self) -> bool {
        self.allow_default_key
    }

    /// Bind the operator key that authorises workspace replacement. Without
    /// one the push endpoint is closed.
    pub fn with_admin_key(mut self, raw: Option<&str>) -> Result<Self, KeyError> {
        self.admin_key = match raw {
            Some(raw) => Some(Key::parse(raw)?),
            None => None,
        };
        Ok(self)
    }

    /// Whether workspace replacement is configured at all.
    pub fn workspace_push_enabled(&self) -> bool {
        self.admin_key.is_some()
    }

    /// Check a presented operator key. Refuses when none is configured, so an
    /// operator who never set one cannot be pushed to by anybody.
    pub(crate) fn check_workspace_key(&self, presented: Option<&str>) -> ApiResult<()> {
        let Some(expected) = self.admin_key.as_ref() else {
            return Err(ApiError::new(
                "workspace replacement is disabled: set an operator key \
                 (SYNCBOT_ADMIN_KEY) to enable it",
            ));
        };
        let raw = presented
            .ok_or_else(|| ApiError::new("workspace replacement requires the operator key"))?;
        let parsed = Key::parse(raw).map_err(|_| ApiError::new("operator key is malformed"))?;
        if expected.matches(&parsed) {
            Ok(())
        } else {
            Err(ApiError::new("operator key does not match"))
        }
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
    /// The workspace datum, or `None` until one is bound.
    ///
    /// Every robot position is anchored to this, so it is published wherever a
    /// client might look — here it is reachable before any workspace call.
    #[serde(default)]
    pub datum: Option<DatumView>,
}

/// The origin every local coordinate is measured from.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DatumView {
    pub lat: f64,
    pub lon: f64,
    pub alt: f64,
}

impl From<concord::Geo> for DatumView {
    fn from(geo: concord::Geo) -> Self {
        Self {
            lat: geo.latitude,
            lon: geo.longitude,
            alt: geo.altitude,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetSnapshot {
    pub robots: Vec<RobotState>,
    pub requests: Vec<ClaimRequest>,
    pub leases: Vec<Lease>,
    /// Which workspace these claims are against, or `None` when no workspace
    /// is bound yet.
    ///
    /// Claims name resources by uuid, so a client holding a *different*
    /// workspace resolves none of them and cannot tell that apart from "no
    /// claims" — it just shows an empty map and says nothing. Naming the
    /// workspace here lets a client detect the mismatch outright.
    #[serde(default)]
    pub workspace_root_zone_id: Option<uuid::Uuid>,
    /// Robots that have not heartbeated within `2 × alive`.
    ///
    /// Registration has no expiry — a robot the core has ever seen stays in
    /// `robots` for the life of the process, because it is allowed to come
    /// back (see [`Coordinator::sweep_inactive`]). Without this a client can't
    /// tell a robot that is present and idle from one that left hours ago, and
    /// ends up listing ghosts forever.
    #[serde(default)]
    pub inactive_robot_ids: Vec<RobotId>,
    /// Recent decisions, oldest first — including the ones that changed
    /// nothing. See [`FleetEvent`].
    #[serde(default)]
    pub events: Vec<FleetEvent>,
    /// The core's clock when this snapshot was taken.
    ///
    /// Ages are `now_ms - event.at_ms`, both from here, so a client on another
    /// host doesn't subtract its own clock from the core's and show nonsense.
    #[serde(default)]
    pub now_ms: u64,
    /// The origin the robots' x/y/z are measured from.
    ///
    /// Sent with every snapshot rather than left to be looked up: it is what
    /// makes the positions here mean anything, and a reader holding positions
    /// without the frame they are in has nothing.
    #[serde(default)]
    pub datum: Option<DatumView>,
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
    /// The origin this zone's geometry is measured from. Zones carry their own
    /// datum in zoneout, so this is the zone's, not a copy of the workspace's —
    /// they normally agree, and it is per-zone here rather than hoisted to the
    /// response so `/zones` stays a plain array.
    pub datum: DatumView,
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
    /// Total search distance, or infinite when no route was found.
    #[serde(with = "unreachable_distance")]
    pub distance: f64,
    pub plan: Option<RoutePlan>,
    pub failure: Option<RouteFailure>,
}

/// An unreachable goal leaves the search distance at `f64::INFINITY`, and JSON
/// has no infinity — `serde_json` writes `null` and then refuses to read it
/// back as an `f64`, so the reply failed to decode on exactly the responses
/// that carry a failure. Map the two representations explicitly instead.
mod unreachable_distance {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(distance: &f64, serializer: S) -> Result<S::Ok, S::Error> {
        match distance.is_finite() {
            true => serializer.serialize_f64(*distance),
            false => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        Ok(Option::<f64>::deserialize(deserializer)?.unwrap_or(f64::INFINITY))
    }
}

pub fn health() -> Health {
    Health {
        status: "ok".into(),
        version: crate::version().into(),
        datum: None,
    }
}

/// Health, plus the datum once a workspace is bound.
pub fn health_with_datum(state: &ServeState) -> Health {
    Health {
        datum: read_coord(state)
            .ok()
            .and_then(|coord| coord.index().and_then(|index| index.datum()))
            .map(DatumView::from),
        ..health()
    }
}

pub fn fleet_snapshot(state: &ServeState) -> ApiResult<FleetSnapshot> {
    let coord = read_coord(state)?;
    Ok(FleetSnapshot {
        robots: coord.robot_states().to_vec(),
        requests: coord.claim_manager().requests().to_vec(),
        leases: coord.claim_manager().leases().to_vec(),
        workspace_root_zone_id: coord.index().and_then(|index| index.root_zone_id()),
        inactive_robot_ids: coord.inactive_robots_at(now_ms()),
        events: state.events(),
        now_ms: now_ms(),
        datum: coord
            .index()
            .and_then(|index| index.datum())
            .map(DatumView::from),
    })
}

/// What the core made of a pushed workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceAccepted {
    pub name: String,
    pub root_zone_id: uuid::Uuid,
    pub zones: usize,
    pub nodes: usize,
    /// Zones the flat claim API cannot address, because they carry no
    /// `external.numeric_id` property. Claims against them are denied as
    /// unknown resources, so report it once at push time instead of leaving
    /// the caller to discover it one denial at a time.
    pub zones_without_numeric_id: Vec<String>,
    /// Non-fatal validation issues; errors reject the push outright.
    pub warnings: Vec<String>,
    /// Claims that no longer resolve against the new workspace.
    ///
    /// Claims deliberately survive a swap — they are keyed by resource uuid,
    /// and the usual push is an edit where dropping the fleet's claims would
    /// be the greater harm. But a claim whose zone no longer exists silently
    /// stops meaning anything, so the count is reported here rather than left
    /// for the pusher to discover one denial at a time.
    #[serde(default)]
    pub stale_claims: usize,
}

/// Replace the served workspace with one pushed over the wire.
///
/// `document` is a serialized `zoneout::WorkspaceJson` — the flat wire format
/// the editors already read and write. This is the counterpart to booting from
/// a directory: it lets a core start with no map and be told what to serve, so
/// an editor can be the source of truth without both sides sharing a disk.
///
/// Claims deliberately survive the swap. They are keyed by resource uuid, and
/// the usual reason to push is a workspace someone just edited, where dropping
/// the fleet's claims would be worse than keeping them. A claim whose zone no
/// longer exists simply stops resolving.
pub fn set_workspace(
    state: &ServeState,
    document: &[u8],
    key: Option<&str>,
) -> ApiResult<WorkspaceAccepted> {
    state.check_workspace_key(key)?;

    let wire: zoneout::WorkspaceJson = serde_json::from_slice(document)
        .map_err(|err| ApiError::new(format!("workspace is not valid zoneout JSON: {err}")))?;
    let name = wire.name.clone();

    // from_wire is where a merely-well-formed document meets the rules a
    // served workspace has to satisfy: one root, no orphans, real geometry.
    let workspace = zoneout::Workspace::from_wire(wire)
        .map_err(|err| ApiError::new(format!("workspace is not loadable: {err}")))?;
    let index = WorkspaceIndex::from_workspace(workspace);

    // A pushed document is untrusted and the zone tree is walked recursively.
    // Refuse an unreasonable nesting depth before anything walks it.
    let depth = index.max_zone_depth();
    if depth > MAX_ZONE_DEPTH {
        return Err(ApiError::new(format!(
            "workspace zone tree is {depth} deep; the limit is {MAX_ZONE_DEPTH}"
        )));
    }

    let issues = index.validation_issues();
    let errors: Vec<String> = issues
        .iter()
        .filter(|issue| issue.severity == ValidationSeverity::Error)
        .map(|issue| issue.message.clone())
        .collect();
    if !errors.is_empty() {
        return Err(ApiError::new(format!(
            "refusing a workspace with validation errors: {}",
            errors.join("; ")
        )));
    }

    let root_zone_id = index
        .root_zone_id()
        .ok_or_else(|| ApiError::new("workspace has no root zone"))?;
    let mut zones: Vec<&zoneout::Zone> = vec![
        index
            .zone(root_zone_id)
            .ok_or_else(|| ApiError::new("root zone is missing from index"))?,
    ];
    zones.extend(index.descendant_zones(root_zone_id));

    let accepted = WorkspaceAccepted {
        name,
        root_zone_id,
        zones: zones.len(),
        nodes: index.workspace().graph().vertices().len(),
        zones_without_numeric_id: zones
            .iter()
            .filter(|zone| zone.property(NUMERIC_ID_PROPERTY).is_none())
            .map(|zone| zone.name().to_string())
            .collect(),
        warnings: issues
            .iter()
            .filter(|issue| issue.severity == ValidationSeverity::Warning)
            .map(|issue| issue.message.clone())
            .collect(),
        stale_claims: 0,
    };

    let mut accepted = accepted;
    let mut coord = write_coord(state)?;
    accepted.stale_claims = coord
        .claim_manager()
        .requests()
        .iter()
        .filter(|request| {
            request.targets.iter().any(|target| match target.kind {
                ClaimTargetKind::Zone => index.zone(target.resource_id).is_none(),
                ClaimTargetKind::Node => index.node(target.resource_id).is_none(),
                ClaimTargetKind::Edge => index.edge(target.resource_id).is_none(),
            })
        })
        .count();
    coord.bind_index(Arc::new(index));
    drop(coord);
    state.record(FleetEvent {
        at_ms: now_ms(),
        kind: FleetEventKind::WorkspaceReplaced,
        robot_id: None,
        zone_ids: Vec::new(),
        zone_names: vec![accepted.name.clone()],
        reason: reason::OK,
        blocked: None,
    });
    Ok(accepted)
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

/// Release the claims of any robot that has gone inactive (no heartbeat for
/// `2 ×` its registered `alive` interval). Returns the robots that were freed.
/// Call this periodically — see [`spawn_inactive_sweeper`].
pub fn sweep_inactive(state: &ServeState) -> Vec<RobotId> {
    let swept = match write_coord(state) {
        Ok(mut coord) => {
            let now = now_ms();
            let expired = coord.expire_claims(now);
            if expired > 0 {
                state.record(FleetEvent {
                    at_ms: now,
                    kind: FleetEventKind::Expired,
                    robot_id: None,
                    zone_ids: Vec::new(),
                    zone_names: Vec::new(),
                    reason: reason::OK,
                    blocked: None,
                });
            }
            coord.sweep_inactive(now)
        }
        Err(_) => Vec::new(),
    };
    // A zone freeing itself with nobody having asked is the least obvious thing
    // the core does, so it gets a line of its own.
    for robot_id in &swept {
        state.record(FleetEvent {
            at_ms: now_ms(),
            kind: FleetEventKind::Swept,
            robot_id: Some(*robot_id),
            zone_ids: Vec::new(),
            zone_names: Vec::new(),
            reason: reason::OK,
            blocked: None,
        });
    }
    swept
}

/// Spawn a background task that calls [`sweep_inactive`] every `period`,
/// auto-releasing the claims of robots that stopped heartbeating. Returns the
/// task handle (drop/abort to stop). Requires a Tokio runtime.
#[cfg(feature = "rest")]
pub fn spawn_inactive_sweeper(
    state: ServeState,
    period: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(period);
        loop {
            ticker.tick().await;
            let freed = sweep_inactive(&state);
            for robot in freed {
                tracing::info!(robot = robot.raw(), "auto-released inactive robot's claims");
            }
        }
    })
}

fn read_coord(state: &ServeState) -> ApiResult<std::sync::RwLockReadGuard<'_, Coordinator>> {
    // Recover the guard from a poisoned lock (defense-in-depth): a single
    // panicking request must not permanently brick every future request.
    Ok(state.coordinator.read().unwrap_or_else(|e| e.into_inner()))
}

fn write_coord(state: &ServeState) -> ApiResult<std::sync::RwLockWriteGuard<'_, Coordinator>> {
    // Recover the guard from a poisoned lock (defense-in-depth): a single
    // panicking request must not permanently brick every future request.
    Ok(state.coordinator.write().unwrap_or_else(|e| e.into_inner()))
}

fn zone_view(idx: &WorkspaceIndex, zone: &zoneout::Zone) -> ZoneView {
    ZoneView {
        datum: (*zone.datum()).into(),
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

/// Resolve a claim target's numeric alias (`NUMERIC_ID_PROPERTY`) from the
/// workspace index by its resolved UUID. Used to name a blocker that the caller
/// never requested (cross-level conflicts), the reverse of the numeric->UUID
/// resolution done elsewhere.
fn numeric_alias_for(index: &WorkspaceIndex, target: &ClaimTarget) -> Option<u64> {
    let raw = match target.kind {
        ClaimTargetKind::Zone => index.zone_property(target.resource_id, NUMERIC_ID_PROPERTY),
        ClaimTargetKind::Node => index
            .node(target.resource_id)
            .and_then(|node| node.properties.get(NUMERIC_ID_PROPERTY).cloned()),
        ClaimTargetKind::Edge => index.edge_property(target.resource_id, NUMERIC_ID_PROPERTY),
    };
    raw.and_then(|raw| raw.trim().parse::<u64>().ok())
}

// ===========================================================================
// Flat (tier-1) wire — PLC / coarse robots.
//
// Transport-neutral. Key on every call, replies are decision + reason (enum).
// REST/XML adapters call these; the resource TYPE comes from the address
// (path / key-expr), never the body.
// ===========================================================================

/// Reason codes for the flat replies. `0` = OK and `1` = mismatched key are
/// reserved with the same meaning on every endpoint; endpoint-specific codes
/// start at `2`.
pub mod reason {
    pub const OK: u8 = 0;
    pub const MISMATCHED_KEY: u8 = 1;

    pub mod register {
        pub const ALREADY_REGISTERED: u8 = 2;
        pub const BAD_ID: u8 = 3;
        pub const UNSUPPORTED_KEY: u8 = 4;
        /// The operator provisioned a list of robot ids and this is not on it,
        /// or registration without a key was not enabled.
        pub const NOT_PERMITTED: u8 = 5;
        /// The coordinator is holding as many robots as it will.
        pub const FLEET_FULL: u8 = 6;
    }
    pub mod heartbeat {
        pub const NOT_REGISTERED: u8 = 2;
        /// A position or heading that isn't a number, or a lat/lon off the
        /// globe. Refused rather than stored: NaN would poison every later
        /// conversion and comparison silently.
        pub const BAD_POSITION: u8 = 3;
    }
    pub mod claim {
        pub const CONFLICT: u8 = 2;
        pub const CAPACITY: u8 = 3;
        pub const UNKNOWN_RESOURCE: u8 = 4;
        pub const BAD_REQUEST: u8 = 5;
    }
    pub mod release {
        pub const NO_SUCH_LEASE: u8 = 2;
        pub const UNKNOWN_OR_BAD: u8 = 3;
    }
}

/// Flat reply shared by all tier-1 endpoints. `decision` is `1` (ok/grant) or
/// `0` (deny); `reason` is the per-endpoint enum (see [`reason`]); `blocked`
/// (claim only) names the offending resource id on denial.
///
/// `#[serde(rename = "reply")]` controls the XML root element: quick-xml uses
/// the type's serde name as the root, so without this the wire would leak the
/// Rust name `<FlatReply>`. JSON object output is unaffected (no type name).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename = "reply")]
pub struct FlatReply {
    pub decision: u8,
    pub reason: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<u64>,
}

impl FlatReply {
    pub fn ok() -> Self {
        Self {
            decision: 1,
            reason: reason::OK,
            blocked: None,
        }
    }
    pub fn deny(reason: u8) -> Self {
        Self {
            decision: 0,
            reason,
            blocked: None,
        }
    }
    pub fn deny_blocked(reason: u8, blocked: u64) -> Self {
        Self {
            decision: 0,
            reason,
            blocked: Some(blocked),
        }
    }
}

/// Whether `key_raw` authenticates as `robot_id`'s bound key.
fn key_ok(coord: &mut Coordinator, robot_id: RobotId, key_raw: &str) -> bool {
    match Key::parse(key_raw) {
        Ok(k) => coord.validate_key(robot_id, &k),
        Err(_) => false,
    }
}

/// Flat registration: bind a key to a robot identified only by its id.
/// `alive_secs` is the heartbeat interval the robot promises (default 2s); the
/// server marks the robot inactive after `2 ×` that without a heartbeat.
pub fn flat_register(
    state: &ServeState,
    robot_raw: &str,
    key_raw: &str,
    alive_secs: Option<u64>,
) -> FlatReply {
    let mut coord = match write_coord(state) {
        Ok(coord) => coord,
        Err(_) => return FlatReply::deny(reason::MISMATCHED_KEY),
    };
    let key = match Key::parse(key_raw) {
        Ok(k) => k,
        Err(KeyError::Unsupported(_)) => return FlatReply::deny(reason::register::UNSUPPORTED_KEY),
        Err(KeyError::Malformed) => return FlatReply::deny(reason::MISMATCHED_KEY),
    };
    // Robot id may be an integer or a UUID string; a new UUID mints a stable
    // internal id.
    // A robot that presents no key of its own would be bound to the shared
    // default, which every other keyless robot also holds. That is a decision
    // an operator makes deliberately, not a default.
    if !state.allow_default_key && key_raw.trim() == DEFAULT_KEY {
        return FlatReply::deny(reason::register::NOT_PERMITTED);
    }
    // A bare `did:key` names an identity without proving it — a public key is
    // public, so anyone could bind someone else's and deny them their id. Prove
    // it first (challenge -> signature -> token) and register with the token.
    if matches!(key, Key::DidKey(_)) {
        return FlatReply::deny(reason::register::NOT_PERMITTED);
    }
    let robot_id = match coord.resolve_or_mint_robot_id(robot_raw) {
        Some(id) => id,
        None => return FlatReply::deny(reason::register::BAD_ID),
    };
    if let Some(refusal) = coord.registration_refusal(robot_id) {
        // Drop the tentative UUID binding a refused registration minted.
        coord.register_with_key(robot_id, key);
        return FlatReply::deny(match refusal {
            crate::coordinator::RegistrationRefusal::AlreadyRegistered => {
                reason::register::ALREADY_REGISTERED
            }
            crate::coordinator::RegistrationRefusal::NotProvisioned => {
                reason::register::NOT_PERMITTED
            }
            crate::coordinator::RegistrationRefusal::Full => reason::register::FLEET_FULL,
        });
    }
    if coord.register_with_key(robot_id, key) {
        let interval = alive_secs.unwrap_or(Coordinator::DEFAULT_ALIVE_SECS);
        coord.set_alive(robot_id, interval, now_ms());
        state.record(FleetEvent {
            at_ms: now_ms(),
            kind: FleetEventKind::Registered,
            robot_id: Some(robot_id),
            zone_ids: Vec::new(),
            zone_names: Vec::new(),
            reason: reason::OK,
            blocked: None,
        });
        FlatReply::ok()
    } else {
        FlatReply::deny(reason::register::ALREADY_REGISTERED)
    }
}

/// Human names for claim targets, so an event still reads sensibly to a client
/// that does not hold this workspace.
fn resource_names(index: &WorkspaceIndex, targets: &[ClaimTarget]) -> Vec<String> {
    targets
        .iter()
        .map(|target| match target.kind {
            ClaimTargetKind::Zone => index
                .zone(target.resource_id)
                .map(|zone| zone.name().to_string())
                .unwrap_or_else(|| format!("zone {}", target.resource_id)),
            ClaimTargetKind::Node => format!("node {}", target.resource_id),
            ClaimTargetKind::Edge => format!("edge {}", target.resource_id),
        })
        .collect()
}

/// Flat heartbeat: liveness + position. Reply is just an ack (decision/reason).
/// The server stamps the tick. `zone` is the coarse position: a non-negative
/// value is a zone id; `-1` (or any negative) means "unknown / not in any
/// claimed zone" — still a valid heartbeat, just no known location. Node/edge
/// progress updates as before (zone-granular progress is deferred).
/// A position as it came off the wire, before the datum is applied.
///
/// A robot reports in whichever frame it has: lat/lon/alt straight from a GNSS
/// receiver, or x/y/z from an odometry stack zeroed at the datum. Both name the
/// same point, so the sender uses whichever it can produce and the core stores
/// both.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReportedPosition {
    /// Latitude, longitude, altitude (WGS84).
    Global { lat: f64, lon: f64, alt: f64 },
    /// Metres east/north/up of the datum.
    Local { x: f64, y: f64, z: f64 },
}

/// Fill in the frame the robot didn't send, using the workspace datum.
///
/// With no datum there is nothing to anchor against, so the reported frame is
/// kept and the other left zeroed behind `converted: false` — which says
/// "unknown", where a bare zero would claim the robot is sitting on the origin.
fn resolve_position(
    index: Option<&Arc<WorkspaceIndex>>,
    reported: ReportedPosition,
) -> crate::robot::RobotPosition {
    use crate::robot::RobotPosition;
    let index = index.map(|index| index.as_ref());
    match reported {
        ReportedPosition::Global { lat, lon, alt } => {
            RobotPosition::from_global(lat, lon, alt, index)
        }
        ReportedPosition::Local { x, y, z } => RobotPosition::from_local(x, y, z, index),
    }
}

/// `position` and `yaw_rad` are optional and independent: a robot that sends
/// neither still coordinates, it just cannot be drawn. `yaw_rad` is REP-103
/// yaw — radians counter-clockwise from east — and the compass bearing is
/// derived from it.
pub fn flat_heartbeat(
    state: &ServeState,
    robot_raw: &str,
    key_raw: &str,
    zone: Option<i64>,
    node: Option<u64>,
    edge: Option<u64>,
    position: Option<ReportedPosition>,
    yaw_rad: Option<f64>,
) -> FlatReply {
    let mut coord = match write_coord(state) {
        Ok(coord) => coord,
        Err(_) => return FlatReply::deny(reason::MISMATCHED_KEY),
    };
    let robot_id = match coord.resolve_robot_id(robot_raw) {
        Some(id) => id,
        None => return FlatReply::deny(reason::heartbeat::NOT_REGISTERED),
    };
    if !coord.has_robot(robot_id) {
        return FlatReply::deny(reason::heartbeat::NOT_REGISTERED);
    }
    if !key_ok(&mut coord, robot_id, key_raw) {
        return FlatReply::deny(reason::MISMATCHED_KEY);
    }
    // A non-finite coordinate would poison every later conversion and comparison
    // with NaN, so it is refused at the door rather than stored.
    if let Some(p) = position
        && !position_is_finite(&p)
    {
        return FlatReply::deny(reason::heartbeat::BAD_POSITION);
    }
    if yaw_rad.is_some_and(|yaw| !yaw.is_finite()) {
        return FlatReply::deny(reason::heartbeat::BAD_POSITION);
    }
    // A negative zone is the "unknown location" sentinel; a non-negative one is
    // a zone id (informational for now — progress advances from node/edge).
    let _known_zone = zone.filter(|&z| z >= 0).map(|z| z as u64);
    let tick = coord
        .find_robot_state(robot_id)
        .map_or(1, |s| s.updated_at_tick + 1);
    let index = coord.index_arc();
    let (node_uuid, edge_uuid) = match &index {
        Some(index) => (
            node.and_then(|n| ResourceRef::Numeric(n).resolve_node(index)),
            edge.and_then(|e| ResourceRef::Numeric(e).resolve_edge(index)),
        ),
        None => (None, None),
    };
    coord.update_robot_progress(robot_id, node_uuid, edge_uuid, tick);
    let resolved = position.map(|p| resolve_position(index.as_ref(), p));
    let heading = yaw_rad.map(crate::robot::RobotHeading::from_yaw_rad);
    coord.update_robot_pose(robot_id, resolved, heading, now_ms());
    coord.touch_robot(robot_id, now_ms());
    FlatReply::ok()
}

fn position_is_finite(position: &ReportedPosition) -> bool {
    match *position {
        ReportedPosition::Global { lat, lon, alt } => {
            lat.is_finite() && lon.is_finite() && alt.is_finite()
                // A GNSS fix outside these ranges is a bug, not a location.
                && (-90.0..=90.0).contains(&lat)
                && (-180.0..=180.0).contains(&lon)
        }
        ReportedPosition::Local { x, y, z } => x.is_finite() && y.is_finite() && z.is_finite(),
    }
}

/// Flat claim over one or more resources of `kind` (type from the address).
/// Atomic: all-or-nothing. On denial, `blocked` names the offending id.
///
/// `access_mode`: `None`/0/1 → Exclusive; 2+ is reserved for future modes and
/// rejected. `lease_seconds`: `None`/0 → the claim is held until released or
/// until the robot stops heartbeating; X → it is additionally dropped X
/// seconds from now.
///
/// Claim windows minted here are in epoch milliseconds: the flat path is the
/// only thing that creates claims on a served core, so its windows are
/// anchored to the same wall clock the heartbeat sweep uses rather than to an
/// abstract tick nobody advances.
pub fn flat_claim(
    state: &ServeState,
    kind: ClaimTargetKind,
    key_raw: &str,
    robot_raw: &str,
    ids: &[u64],
    access_mode: Option<u8>,
    lease_seconds: Option<u64>,
) -> FlatReply {
    let requested: Vec<(ClaimTargetKind, u64)> = ids.iter().map(|&id| (kind, id)).collect();
    flat_claim_targets(
        state,
        &requested,
        key_raw,
        robot_raw,
        access_mode,
        lease_seconds,
    )
}

/// Flat claim over a whole route — the nodes it stops at and the edges it
/// crosses — as one atomic request.
///
/// A route is nodes *and* edges, which the single-kind endpoints cannot
/// express: claiming them separately is two independent requests, and a robot
/// whose node claim lands while its edge claim is denied ends up holding half
/// a path. Here it is all-or-nothing.
///
/// The zones the route passes through are not claimed. The manager derives
/// intent on them from these targets, so a non-interfering route through the
/// same zone still proceeds while a claim on the zone itself does not.
pub fn flat_claim_route(
    state: &ServeState,
    key_raw: &str,
    robot_raw: &str,
    nodes: &[u64],
    edges: &[u64],
    access_mode: Option<u8>,
    lease_seconds: Option<u64>,
) -> FlatReply {
    let mut requested: Vec<(ClaimTargetKind, u64)> = Vec::with_capacity(nodes.len() + edges.len());
    requested.extend(nodes.iter().map(|&id| (ClaimTargetKind::Node, id)));
    requested.extend(edges.iter().map(|&id| (ClaimTargetKind::Edge, id)));
    flat_claim_targets(
        state,
        &requested,
        key_raw,
        robot_raw,
        access_mode,
        lease_seconds,
    )
}

/// Shared body of every flat claim: resolve the requested resources, evaluate
/// once, and record the decision. `requested` may mix target kinds.
fn flat_claim_targets(
    state: &ServeState,
    requested: &[(ClaimTargetKind, u64)],
    key_raw: &str,
    robot_raw: &str,
    access_mode: Option<u8>,
    lease_seconds: Option<u64>,
) -> FlatReply {
    let mut coord = match write_coord(state) {
        Ok(coord) => coord,
        Err(_) => return FlatReply::deny(reason::MISMATCHED_KEY),
    };
    let access = match access_mode.unwrap_or(1) {
        0 | 1 => ClaimAccessMode::Exclusive,
        2 => ClaimAccessMode::Shared,
        _ => return FlatReply::deny(reason::claim::BAD_REQUEST), // 3+ reserved
    };
    let now = now_ms();
    // Expire lapsed claims before evaluating, so a claim that outlived its
    // lease cannot block this one in the gap before the next sweep.
    coord.expire_claims(now);
    let window = match lease_seconds.unwrap_or(0) {
        0 => ClaimWindow::default(),
        seconds => ClaimWindow {
            start_tick: Some(now),
            end_tick: Some(now.saturating_add(seconds.saturating_mul(1_000))),
        },
    };
    let robot_id = match coord.resolve_robot_id(robot_raw) {
        Some(id) => id,
        None => return FlatReply::deny(reason::MISMATCHED_KEY),
    };
    if !key_ok(&mut coord, robot_id, key_raw) {
        return FlatReply::deny(reason::MISMATCHED_KEY);
    }
    if requested.is_empty() || requested.len() > MAX_CLAIM_TARGETS {
        // Evaluation is O(targets x holders x zones) under the coordinator's
        // write lock, so an unbounded list is a cheap denial of service
        // against every other robot. A rolling horizon never needs this many.
        return FlatReply::deny(reason::claim::BAD_REQUEST);
    }
    let Some(index) = coord.index_arc() else {
        return FlatReply::deny(reason::claim::BAD_REQUEST);
    };
    // Resolve every id up front; keep uuid -> original numeric for `blocked`.
    let mut targets = Vec::with_capacity(requested.len());
    let mut numeric_by_uuid: BTreeMap<uuid::Uuid, u64> = BTreeMap::new();
    for &(kind, id) in requested {
        let rref = ResourceRef::Numeric(id);
        let resolved = match kind {
            ClaimTargetKind::Zone => rref.resolve_zone(&index),
            ClaimTargetKind::Node => rref.resolve_node(&index),
            ClaimTargetKind::Edge => rref.resolve_edge(&index),
        };
        let Some(resource_id) = resolved else {
            return FlatReply::deny_blocked(reason::claim::UNKNOWN_RESOURCE, id);
        };
        numeric_by_uuid.insert(resource_id, id);
        targets.push(ClaimTarget { kind, resource_id });
    }
    let request = ClaimRequest {
        id: coord.claim_manager().next_request_id(),
        robot_id,
        mission_id: MissionId::default(),
        access_mode: access,
        priority: 0,
        requested_at_tick: None,
        window,
        targets,
    };
    let evaluation = coord.claim_manager().evaluate_request(&request);
    let zone_ids: Vec<uuid::Uuid> = request.targets.iter().map(|t| t.resource_id).collect();
    let zone_names = resource_names(&index, &request.targets);
    if evaluation.decision == ClaimDecision::Grant {
        // Re-claiming the same ground refreshes the existing entry instead of
        // stacking another one; the wire mints a fresh id on every call.
        coord.claim_manager_mut().upsert_request_for_robot(request);
        state.record(FleetEvent {
            at_ms: now_ms(),
            kind: FleetEventKind::Granted,
            robot_id: Some(robot_id),
            zone_ids,
            zone_names,
            reason: reason::OK,
            blocked: None,
        });
        return FlatReply::ok();
    }
    let blocked = evaluation.blocking_target.and_then(|t| {
        // Prefer the caller's own requested numeric id; otherwise (cross-level
        // conflict where the blocker is an ancestor/containing zone the caller
        // never named) fall back to the blocker's numeric alias in the index.
        numeric_by_uuid
            .get(&t.resource_id)
            .copied()
            .or_else(|| numeric_alias_for(&index, &t))
    });
    // Capacity denials (shared-vs-shared, zone/edge over capacity) report reason
    // 3; every other denial (including an exclusive claim on an occupied or
    // shared zone) stays CONFLICT (reason 2), exactly as before.
    let code = if evaluation.denied_by_capacity {
        reason::claim::CAPACITY
    } else {
        reason::claim::CONFLICT
    };
    state.record(FleetEvent {
        at_ms: now_ms(),
        kind: FleetEventKind::Denied,
        robot_id: Some(robot_id),
        zone_ids,
        zone_names,
        reason: code,
        blocked,
    });
    match blocked {
        Some(b) => FlatReply::deny_blocked(code, b),
        None => FlatReply::deny(code),
    }
}

/// Flat release by robot + resource (mirrors the flat claim). Removes the
/// robot's active claim on that resource.
pub fn flat_release(
    state: &ServeState,
    kind: ClaimTargetKind,
    key_raw: &str,
    robot_raw: &str,
    id: u64,
) -> FlatReply {
    let mut coord = match write_coord(state) {
        Ok(coord) => coord,
        Err(_) => return FlatReply::deny(reason::MISMATCHED_KEY),
    };
    let robot_id = match coord.resolve_robot_id(robot_raw) {
        Some(id) => id,
        None => return FlatReply::deny(reason::MISMATCHED_KEY),
    };
    if !key_ok(&mut coord, robot_id, key_raw) {
        return FlatReply::deny(reason::MISMATCHED_KEY);
    }
    let Some(index) = coord.index_arc() else {
        return FlatReply::deny(reason::release::UNKNOWN_OR_BAD);
    };
    let rref = ResourceRef::Numeric(id);
    let resolved = match kind {
        ClaimTargetKind::Zone => rref.resolve_zone(&index),
        ClaimTargetKind::Node => rref.resolve_node(&index),
        ClaimTargetKind::Edge => rref.resolve_edge(&index),
    };
    let Some(resource_id) = resolved else {
        return FlatReply::deny(reason::release::UNKNOWN_OR_BAD);
    };
    if coord
        .claim_manager_mut()
        .release_request_for_robot_target(robot_id, resource_id)
    {
        let zone_names = resource_names(&index, &[ClaimTarget { kind, resource_id }]);
        state.record(FleetEvent {
            at_ms: now_ms(),
            kind: FleetEventKind::Released,
            robot_id: Some(robot_id),
            zone_ids: vec![resource_id],
            zone_names,
            reason: reason::OK,
            blocked: None,
        });
        FlatReply::ok()
    } else {
        FlatReply::deny(reason::release::NO_SUCH_LEASE)
    }
}

// --- Shared flat request envelopes (used by the REST/JSON and XML adapters) -

/// Accept a scalar that may arrive as a JSON string or number (XML is always
/// text); yield it as a `String` for the transport-neutral flat functions.
pub(crate) fn de_scalar_string<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Visitor (not `#[serde(untagged)]`, which fails under quick-xml). Accepts
    // a text token (XML leaf body / JSON string) or a JSON number, yielding a
    // `String` for the transport-neutral flat functions.
    struct V;
    impl serde::de::Visitor<'_> for V {
        type Value = String;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a string or integer scalar")
        }
        fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<String, E> {
            Ok(s.trim().to_string())
        }
        fn visit_string<E: serde::de::Error>(self, s: String) -> Result<String, E> {
            Ok(s.trim().to_string())
        }
        fn visit_u64<E: serde::de::Error>(self, n: u64) -> Result<String, E> {
            Ok(n.to_string())
        }
        fn visit_i64<E: serde::de::Error>(self, n: i64) -> Result<String, E> {
            Ok(n.to_string())
        }
    }
    d.deserialize_string(V)
}

/// Default key used when a flat request omits `key`. UNSAFE — every robot that
/// skips the key shares this password, and it is what an omitted key is
/// compared against. Registering with it is refused unless the operator opts
/// in; see [`ServeState::with_default_key_allowed`].
pub const DEFAULT_KEY: &str = "0";

pub(crate) fn default_key() -> String {
    DEFAULT_KEY.to_string()
}

/// Wall-clock now as epoch milliseconds, for heartbeat-liveness tracking.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Flat registration request: robot id + optional key (defaults to [`DEFAULT_KEY`]).
#[derive(Debug, Clone, Deserialize)]
pub struct FlatRegister {
    #[serde(deserialize_with = "de_scalar_string")]
    pub robot: String,
    #[serde(default = "default_key", deserialize_with = "de_scalar_string")]
    pub key: String,
    /// Optional heartbeat interval in seconds the robot promises to keep. The
    /// server marks the robot inactive after `2 ×` this. Absent → default (2s).
    #[serde(default, alias = "Alive")]
    pub alive: Option<u64>,
}

/// Flat heartbeat request: key, optionally where the robot is.
#[derive(Debug, Clone, Deserialize)]
pub struct FlatHeartbeat {
    #[serde(default = "default_key", deserialize_with = "de_scalar_string")]
    pub key: String,
    /// Coarse position. A non-negative value is a zone id; `-1` (or any
    /// negative) means "unknown / not in any claimed zone".
    #[serde(default)]
    pub zone: Option<i64>,
    #[serde(default)]
    pub node: Option<u64>,
    #[serde(default)]
    pub edge: Option<u64>,
    /// Fine position, in either frame — `lat`/`lon` (+ optional `alt`) or
    /// `x`/`y` (+ optional `z`), never both. Optional throughout: a robot that
    /// reports no position still coordinates, it just cannot be drawn.
    #[serde(default)]
    pub lat: Option<f64>,
    #[serde(default)]
    pub lon: Option<f64>,
    #[serde(default)]
    pub alt: Option<f64>,
    /// Metres east of the datum.
    #[serde(default)]
    pub x: Option<f64>,
    /// Metres north of the datum.
    #[serde(default)]
    pub y: Option<f64>,
    /// Metres up from the datum.
    #[serde(default)]
    pub z: Option<f64>,
    /// Heading as REP-103 yaw: radians counter-clockwise from east. The compass
    /// bearing is derived from it, so send what an ENU stack already publishes
    /// rather than converting at the edge.
    #[serde(default)]
    pub yaw: Option<f64>,
}

impl FlatHeartbeat {
    /// Which frame this heartbeat reports a position in, if any.
    ///
    /// `Err` when it names both frames, or half of one: a body with `lat` and
    /// no `lon` is a mistake at the sender, and picking a half to believe would
    /// bury it.
    pub fn position(&self) -> Result<Option<ReportedPosition>, &'static str> {
        let global = self.lat.is_some() || self.lon.is_some();
        let local = self.x.is_some() || self.y.is_some();
        match (global, local) {
            (true, true) => Err("send either lat/lon or x/y, not both"),
            (false, false) => Ok(None),
            (true, false) => match (self.lat, self.lon) {
                (Some(lat), Some(lon)) => Ok(Some(ReportedPosition::Global {
                    lat,
                    lon,
                    alt: self.alt.unwrap_or(0.0),
                })),
                _ => Err("a global position needs both lat and lon"),
            },
            (false, true) => match (self.x, self.y) {
                (Some(x), Some(y)) => Ok(Some(ReportedPosition::Local {
                    x,
                    y,
                    z: self.z.unwrap_or(0.0),
                })),
                _ => Err("a local position needs both x and y"),
            },
        }
    }
}

/// Flat claim request: key + robot + one or more ids (type from the address).
/// `id` is a list: JSON sends an array (`"id":[42,43]`, or `[42]` for one);
/// XML repeats the `<id>` element (`<id>42</id><id>43</id>`, or a single
/// `<id>42</id>`). Both map to `Vec<u64>` via the format's native sequence
/// handling.
#[derive(Debug, Clone, Deserialize)]
pub struct FlatClaim {
    #[serde(default = "default_key", deserialize_with = "de_scalar_string")]
    pub key: String,
    #[serde(deserialize_with = "de_scalar_string")]
    pub robot: String,
    #[serde(default)]
    pub id: Vec<u64>,
    /// Optional access mode: 0 = undef (→ default), 1 = exclusive, 2+ = reserved
    /// for future modes (rejected for now). Absent → exclusive.
    #[serde(default, alias = "AccessMode", alias = "accessmode")]
    pub access_mode: Option<u8>,
    /// Optional lease time in seconds: 0 (or absent) = unlimited, X = X seconds.
    #[serde(default, alias = "LeaseTime", alias = "leasetime")]
    pub lease_time: Option<u64>,
}

/// What a robot must sign to prove a `did:key` identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeView {
    /// The bytes to sign, hex encoded.
    pub nonce: String,
    /// Seconds before the challenge lapses.
    pub expires_in: u64,
}

/// The bearer token a successful proof yields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenView {
    /// Present this as `tok:<token>` in the `key` field of later calls.
    pub token: String,
    /// Seconds before it stops being accepted.
    pub expires_in: u64,
}

/// Ask for a nonce to sign.
///
/// Open to unregistered robot ids on purpose: a `did:key` robot proves
/// possession *before* it registers, so an identity can never be bound by
/// someone who cannot use it.
pub fn flat_challenge(state: &ServeState, robot_raw: &str) -> ApiResult<ChallengeView> {
    let mut coord = write_coord(state)?;
    let robot_id = coord
        .resolve_or_mint_robot_id(robot_raw)
        .ok_or_else(|| ApiError::new(format!("unknown robot id {robot_raw:?}")))?;
    let nonce = coord.issue_challenge(robot_id);
    if nonce.is_empty() {
        return Err(ApiError::new("could not generate a challenge"));
    }
    Ok(ChallengeView {
        nonce: nonce.iter().map(|b| format!("{b:02x}")).collect(),
        expires_in: 30,
    })
}

/// Answer a challenge with a signature and receive a bearer token.
///
/// `did` is the robot's `did:key:…`; `signature` is hex-encoded Ed25519 over
/// the challenge nonce.
pub fn flat_prove(
    state: &ServeState,
    robot_raw: &str,
    did: &str,
    signature_hex: &str,
) -> ApiResult<TokenView> {
    let public_key = match Key::parse(did) {
        Ok(Key::DidKey(public_key)) => public_key,
        _ => return Err(ApiError::new("expected a did:key identity")),
    };
    let signature =
        decode_hex(signature_hex).ok_or_else(|| ApiError::new("signature must be hex encoded"))?;

    let mut coord = write_coord(state)?;
    let robot_id = coord
        .resolve_or_mint_robot_id(robot_raw)
        .ok_or_else(|| ApiError::new(format!("unknown robot id {robot_raw:?}")))?;
    // One message for every failure: a wrong signature, a lapsed challenge and
    // an identity that does not match the registration are all the same
    // answer, so a prober learns nothing from which one it hit.
    let token = coord
        .prove_identity(robot_id, &public_key, &signature)
        .ok_or_else(|| ApiError::new("challenge was not answered correctly"))?;
    Ok(TokenView {
        token,
        expires_in: 3600,
    })
}

/// Decode a hex string, for adapters carrying a signature as text.
pub fn decode_hex_public(hex: &str) -> Option<Vec<u8>> {
    decode_hex(hex)
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    let hex = hex.trim();
    if !hex.len().is_multiple_of(2) || hex.is_empty() {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

/// Flat challenge request: which robot is asking.
#[derive(Debug, Clone, Deserialize)]
pub struct FlatChallenge {
    #[serde(deserialize_with = "de_scalar_string")]
    pub robot: String,
}

/// Flat proof: the identity and the signature over the challenge.
#[derive(Debug, Clone, Deserialize)]
pub struct FlatProve {
    #[serde(deserialize_with = "de_scalar_string")]
    pub robot: String,
    #[serde(deserialize_with = "de_scalar_string")]
    pub did: String,
    #[serde(deserialize_with = "de_scalar_string")]
    pub signature: String,
}

/// Flat route claim: the nodes a robot stops at and the edges it crosses.
///
/// JSON sends two arrays (`{"node":[1001,1002],"edge":[2001]}`); XML repeats
/// the elements (`<node>1001</node><node>1002</node><edge>2001</edge>`). Both
/// may be empty individually, but not both at once.
#[derive(Debug, Clone, Deserialize)]
pub struct FlatClaimRoute {
    #[serde(default = "default_key", deserialize_with = "de_scalar_string")]
    pub key: String,
    #[serde(deserialize_with = "de_scalar_string")]
    pub robot: String,
    #[serde(default)]
    pub node: Vec<u64>,
    #[serde(default)]
    pub edge: Vec<u64>,
    #[serde(default, alias = "AccessMode", alias = "accessmode")]
    pub access_mode: Option<u8>,
    #[serde(default, alias = "LeaseTime", alias = "leasetime")]
    pub lease_time: Option<u64>,
}

/// Try every canonical request decoder against `bytes`, reporting which (if
/// any) accepted them.
///
/// Exists for the fuzz target in `fuzz/fuzz_targets/canonical_datapod.rs`:
/// adapters in other languages pack these headers by hand, so the decoders are
/// fed bytes no Rust caller would ever produce. Rejecting them is correct;
/// panicking on them is not.
#[cfg(feature = "peerbus")]
pub fn fuzz_decode_canonical(bytes: &[u8]) -> Option<&'static str> {
    peerbus::decode_any(bytes)
}

/// Flat route-plan request: two node tokens, each a UUID or a numeric alias.
///
/// Read-only and unauthenticated, like the other read endpoints: planning a
/// route reserves nothing and changes no state.
#[derive(Debug, Clone, Deserialize)]
pub struct FlatPlanRoute {
    #[serde(alias = "start", deserialize_with = "de_scalar_string")]
    pub start_node_id: String,
    #[serde(alias = "goal", deserialize_with = "de_scalar_string")]
    pub goal_node_id: String,
    /// Apply the policy cost model (slowdowns, corridors, claim-gated zones)
    /// instead of raw graph weight.
    #[serde(default)]
    pub use_penalties: bool,
}

/// Flat release request: key + robot + resource id (type from the address).
#[derive(Debug, Clone, Deserialize)]
pub struct FlatRelease {
    #[serde(default = "default_key", deserialize_with = "de_scalar_string")]
    pub key: String,
    #[serde(deserialize_with = "de_scalar_string")]
    pub robot: String,
    pub id: u64,
}
