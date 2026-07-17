//! Wire transport adapters for `syncbot`.
//!
//! Each submodule is a thin adapter that translates an external wire
//! protocol (REST/JSON, REST/XML, Zenoh) into calls on the real syncbot
//! core (`Coordinator`, `ClaimManager`, `plan_route`) and serialises core
//! results back to the wire.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};

use crate::claim::{
    ClaimAccessMode, ClaimDecision, ClaimEvaluation, ClaimId, ClaimRequest, ClaimTarget,
    ClaimTargetKind, ClaimWindow, Lease, LeaseId, MissionId,
};
use crate::coordinator::{Coordinator, ScheduleDecision};
use crate::core::ids::RobotId;
use crate::core::key::{Key, KeyError};
use crate::index::{NUMERIC_ID_PROPERTY, ResourceRef, ValidationSeverity, WorkspaceIndex};
use crate::robot::RobotState;
use crate::route::{RouteFailure, RoutePlan, plan_route};

#[cfg(feature = "rest")]
pub mod rest;

#[cfg(feature = "peerbus")]
pub mod peerbus;

#[cfg(feature = "robo")]
pub mod robo;

#[cfg(feature = "robo")]
pub mod ros2dds;

#[cfg(feature = "xmlt")]
pub mod xmlt;

/// Shared state used by all serving adapters.
#[derive(Clone)]
pub struct ServeState {
    coordinator: Arc<RwLock<Coordinator>>,
    /// OPT-IN auth on the admin/mutation endpoints (unregister robot, remove
    /// claim, add/release lease). Defaults to `false`, which keeps those
    /// endpoints OPEN exactly as before. Flip on with [`ServeState::with_admin_auth`].
    /// Never read from the environment here — operators wire that in the binary
    /// (see the examples) so this stays testable and race-free.
    admin_auth: bool,
    /// Recent decisions, newest last. See [`FleetEvent`].
    ///
    /// Lives here rather than in the coordinator because it is a record of what
    /// the wire was asked and answered, not part of the coordination state: a
    /// refusal changes nothing, so the coordinator rightly forgets it the
    /// instant it replies.
    events: Arc<Mutex<VecDeque<FleetEvent>>>,
}

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
    WorkspaceReplaced,
}

impl ServeState {
    pub fn new(coordinator: Coordinator) -> Self {
        Self {
            coordinator: Arc::new(RwLock::new(coordinator)),
            admin_auth: false,
            events: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub fn shared(coordinator: Arc<RwLock<Coordinator>>) -> Self {
        Self {
            coordinator,
            admin_auth: false,
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

    /// Enable (or disable) opt-in auth on the admin/mutation endpoints. Off by
    /// default; leaving it off keeps those endpoints byte-identically open.
    pub fn with_admin_auth(mut self, on: bool) -> Self {
        self.admin_auth = on;
        self
    }

    /// Whether opt-in admin auth is enabled.
    pub fn admin_auth(&self) -> bool {
        self.admin_auth
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
    /// Mandatory auth key (state-changing endpoint). See [`require_key`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignRouteRequest {
    pub route_plan: RoutePlan,
    pub horizon: u64,
    pub updated_at_tick: u64,
    /// Mandatory auth key (state-changing endpoint). See [`require_key`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseLeaseRequest {
    pub lease_id: LeaseId,
    pub released_at_tick: Option<u64>,
    /// OPTIONAL admin key. Ignored unless `ServeState::admin_auth` is on; when
    /// on, it is validated against the lease's owning robot (a missing key
    /// falls back to the shared [`DEFAULT_KEY`]). See [`require_key_or_default`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
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
    /// Mandatory auth key when SUBMITTING (state-changing). Ignored by the
    /// read-only `evaluate` (dry-run) path. See [`require_key`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
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
pub fn set_workspace(state: &ServeState, document: &[u8]) -> ApiResult<WorkspaceAccepted> {
    let wire: zoneout::WorkspaceJson = serde_json::from_slice(document)
        .map_err(|err| ApiError::new(format!("workspace is not valid zoneout JSON: {err}")))?;
    let name = wire.name.clone();

    // from_wire is where a merely-well-formed document meets the rules a
    // served workspace has to satisfy: one root, no orphans, real geometry.
    let workspace = zoneout::Workspace::from_wire(wire)
        .map_err(|err| ApiError::new(format!("workspace is not loadable: {err}")))?;
    let index = WorkspaceIndex::from_workspace(workspace);

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
    };

    write_coord(state)?.bind_index(Arc::new(index));
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

pub fn unregister_robot(
    state: &ServeState,
    robot_id: RobotId,
    key: Option<String>,
) -> ApiResult<bool> {
    let mut coord = write_coord(state)?;
    // Opt-in auth: protect a registered robot bound to a real key. An unknown
    // robot has nothing to protect — fall through to the unchanged `false`.
    if state.admin_auth && coord.has_robot(robot_id) {
        require_key_or_default(&coord, robot_id, &key)?;
    }
    Ok(coord.unregister_robot(robot_id))
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
    require_key(&coord, robot_id, &request.key)?;
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
    require_key(&coord, robot_id, &request.key)?;
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

pub fn remove_claim(state: &ServeState, claim_id: ClaimId, key: Option<String>) -> ApiResult<bool> {
    let mut coord = write_coord(state)?;
    // Opt-in auth: enforce the owning robot's key. If the claim is unknown there
    // is no owner to protect — fall through to the unchanged `false`.
    if state.admin_auth {
        if let Some(owner) = coord
            .claim_manager()
            .find_request(claim_id)
            .map(|r| r.robot_id)
        {
            require_key_or_default(&coord, owner, &key)?;
        }
    }
    Ok(coord.claim_manager_mut().remove_request(claim_id))
}

pub fn evaluate_claim(state: &ServeState, request: ClaimRequestWire) -> ApiResult<ClaimEvaluation> {
    let coord = read_coord(state)?;
    let idx = coord
        .index()
        .ok_or_else(|| ApiError::new("coordinator has no WorkspaceIndex bound"))?;
    let resolved = request.into_request(idx)?;
    Ok(coord.claim_manager().evaluate_request(&resolved))
}

/// Release the claims of any robot that has gone inactive (no heartbeat for
/// `2 ×` its registered `alive` interval). Returns the robots that were freed.
/// Call this periodically — see [`spawn_inactive_sweeper`].
pub fn sweep_inactive(state: &ServeState) -> Vec<RobotId> {
    let swept = match write_coord(state) {
        Ok(mut coord) => coord.sweep_inactive(now_ms()),
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

/// Resolve a raw robot identifier (integer or UUID string) from a per-robot URL
/// route to its internal [`RobotId`]. Errors if the robot is unknown.
pub fn resolve_robot(state: &ServeState, raw: &str) -> ApiResult<RobotId> {
    read_coord(state)?
        .resolve_robot_id(raw)
        .ok_or_else(|| ApiError::new(format!("unknown robot id {raw:?}")))
}

/// Validate the mandatory auth key on a state-changing tier-2 request against
/// the acting robot. Returns a `mismatched key` error if missing/wrong.
/// Read-only endpoints (snapshot, lists, plan, evaluate) do NOT call this —
/// they stay open by design (see `PLAN.md`).
fn require_key(coord: &Coordinator, robot_id: RobotId, key: &Option<String>) -> ApiResult<()> {
    let raw = key
        .as_deref()
        .ok_or_else(|| ApiError::new("mismatched key: missing key"))?;
    let parsed = Key::parse(raw).map_err(|_| ApiError::new("mismatched key: bad key"))?;
    if coord.validate_key(robot_id, &parsed) {
        Ok(())
    } else {
        Err(ApiError::new("mismatched key"))
    }
}

/// Validate an OPTIONAL admin key against `robot_id`, defaulting a MISSING key
/// to the shared [`DEFAULT_KEY`] before validating. Semantics mirror the flat
/// register/heartbeat/claim convention: a robot bound to the default key `"0"`
/// (i.e. registered without a real key) stays openly manageable even with auth
/// on, while a robot bound to a real key is protected (omitted/wrong key is
/// rejected). Unlike the strict tier-2 [`require_key`], a missing key is NOT a
/// hard error — it becomes the default. Only invoked by the admin handlers when
/// `ServeState::admin_auth` is enabled.
fn require_key_or_default(
    coord: &Coordinator,
    robot_id: RobotId,
    key: &Option<String>,
) -> ApiResult<()> {
    let raw = key.as_deref().unwrap_or(DEFAULT_KEY);
    let parsed = Key::parse(raw).map_err(|_| ApiError::new("mismatched key: bad key"))?;
    if coord.validate_key(robot_id, &parsed) {
        Ok(())
    } else {
        Err(ApiError::new("mismatched key"))
    }
}

pub fn submit_claim(state: &ServeState, request: ClaimRequestWire) -> ApiResult<ClaimEvaluation> {
    let mut coord = write_coord(state)?;
    require_key(&coord, request.robot_id, &request.key)?;
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

pub fn add_lease(state: &ServeState, lease: Lease, key: Option<String>) -> ApiResult<Lease> {
    let mut coord = write_coord(state)?;
    // Opt-in auth: the lease names its own owning robot; enforce that robot's key.
    if state.admin_auth {
        require_key_or_default(&coord, lease.robot_id, &key)?;
    }
    coord.claim_manager_mut().add_lease(lease.clone());
    Ok(lease)
}

pub fn release_lease(state: &ServeState, request: ReleaseLeaseRequest) -> ApiResult<bool> {
    let mut coord = write_coord(state)?;
    // Opt-in auth: enforce the owning robot's key. If the lease is unknown there
    // is no owner to protect — fall through to the unchanged `false`.
    if state.admin_auth {
        if let Some(owner) = coord
            .claim_manager()
            .find_lease(request.lease_id)
            .map(|l| l.robot_id)
        {
            require_key_or_default(&coord, owner, &request.key)?;
        }
    }
    Ok(coord
        .claim_manager_mut()
        .release_lease(request.lease_id, request.released_at_tick))
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
// REST/XML/Zenoh adapters call these; the resource TYPE comes from the address
// (path / key-expr), never the body. See PLAN.md.
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
fn key_ok(coord: &Coordinator, robot_id: RobotId, key_raw: &str) -> bool {
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
    let robot_id = match coord.resolve_or_mint_robot_id(robot_raw) {
        Some(id) => id,
        None => return FlatReply::deny(reason::register::BAD_ID),
    };
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
fn resource_names(
    index: &WorkspaceIndex,
    kind: ClaimTargetKind,
    ids: &[uuid::Uuid],
) -> Vec<String> {
    ids.iter()
        .map(|id| match kind {
            ClaimTargetKind::Zone => index
                .zone(*id)
                .map(|zone| zone.name().to_string())
                .unwrap_or_else(|| format!("zone {id}")),
            ClaimTargetKind::Node => format!("node {id}"),
            ClaimTargetKind::Edge => format!("edge {id}"),
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
    use crate::robot::{PositionFrame, RobotPosition};
    match reported {
        ReportedPosition::Global { lat, lon, alt } => {
            let local = index
                .and_then(|index| index.pose_global_to_local(concord::Geo::new(lat, lon, alt)));
            RobotPosition {
                reported: PositionFrame::Global,
                lat,
                lon,
                alt,
                x: local.map_or(0.0, |p| p.x),
                y: local.map_or(0.0, |p| p.y),
                z: local.map_or(0.0, |p| p.z),
                converted: local.is_some(),
            }
        }
        ReportedPosition::Local { x, y, z } => {
            let global =
                index.and_then(|index| index.pose_local_to_global(datapod::Point::new(x, y, z)));
            RobotPosition {
                reported: PositionFrame::Local,
                lat: global.map_or(0.0, |g| g.latitude),
                lon: global.map_or(0.0, |g| g.longitude),
                alt: global.map_or(0.0, |g| g.altitude),
                x,
                y,
                z,
                converted: global.is_some(),
            }
        }
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
    if !key_ok(&coord, robot_id, key_raw) {
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
/// rejected. `lease_seconds`: `None`/0 → unlimited; X → the claim window ends
/// X units out (currently the system's tick unit; wall-clock expiry needs a
/// scheduler — see PLAN.md).
pub fn flat_claim(
    state: &ServeState,
    kind: ClaimTargetKind,
    key_raw: &str,
    robot_raw: &str,
    ids: &[u64],
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
    let window = match lease_seconds.unwrap_or(0) {
        0 => ClaimWindow::default(),
        seconds => ClaimWindow {
            start_tick: None,
            end_tick: Some(seconds),
        },
    };
    let robot_id = match coord.resolve_robot_id(robot_raw) {
        Some(id) => id,
        None => return FlatReply::deny(reason::MISMATCHED_KEY),
    };
    if !key_ok(&coord, robot_id, key_raw) {
        return FlatReply::deny(reason::MISMATCHED_KEY);
    }
    if ids.is_empty() {
        return FlatReply::deny(reason::claim::BAD_REQUEST);
    }
    let Some(index) = coord.index_arc() else {
        return FlatReply::deny(reason::claim::BAD_REQUEST);
    };
    // Resolve every id up front; keep uuid -> original numeric for `blocked`.
    let mut targets = Vec::with_capacity(ids.len());
    let mut numeric_by_uuid: BTreeMap<uuid::Uuid, u64> = BTreeMap::new();
    for &id in ids {
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
    let zone_names = resource_names(&index, kind, &zone_ids);
    if evaluation.decision == ClaimDecision::Grant {
        coord.claim_manager_mut().add_request(request);
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
    if !key_ok(&coord, robot_id, key_raw) {
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
        let zone_names = resource_names(&index, kind, &[resource_id]);
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

// --- Shared flat request envelopes (used by REST/XML and Zenoh/ROS) --------

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
/// skips the key shares this password. See `PLAN.md`.
pub(crate) const DEFAULT_KEY: &str = "0";

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

/// Flat release request: key + robot + resource id (type from the address).
#[derive(Debug, Clone, Deserialize)]
pub struct FlatRelease {
    #[serde(default = "default_key", deserialize_with = "de_scalar_string")]
    pub key: String,
    #[serde(deserialize_with = "de_scalar_string")]
    pub robot: String,
    pub id: u64,
}
