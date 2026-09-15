//! Canonical ARES datapod contract and peerbus req/res core service.
//!
//! External transports never receive a [`ServeState`]. They hold a [`Client`]
//! and translate their wire format to these messages. The coordinator is owned
//! by [`CoreService`], behind one polling loop, so peerbus is the only transport
//! boundary that can execute the flat protocol operations.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use datapod::{DataPod, DataPodDecode, DataPodValidate, LeWireHeader};
use peerbus::{DatapodMsg, Node};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::claim::ClaimTargetKind;
use crate::index::ResourceRef;
use crate::wire::{ApiError, ApiResult, FlatReply, ServeState};

pub const CORE_IDENTITY: &str = "ares-core";
const LOCAL_MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

/// Bytes of workspace JSON carried per [`WorkspaceChunk`]. Sits well under
/// [`LOCAL_MAX_PAYLOAD_BYTES`] so the datapod header and section framing still
/// fit; a workspace of any size is just more chunks. The payload cap itself
/// stays where it is deliberately — it is a *per-node* setting, so raising it
/// would enlarge the preallocated shared-memory slots of every topic in order
/// to serve this one.
const WORKSPACE_CHUNK_BYTES: usize = 512 * 1024;

/// Ceiling on the bytes one in-flight push may buffer before the core rejects
/// it, so a client that dies mid-transfer cannot pin memory indefinitely.
const MAX_WORKSPACE_BYTES: usize = 256 * 1024 * 1024;

pub const REGISTER_TOPIC: &str = "ares/v1/robots/register";
pub const DEREGISTER_TOPIC: &str = "ares/v1/robots/deregister";
pub const HEARTBEAT_TOPIC: &str = "ares/v1/robots/heartbeat";
pub const CLAIM_ZONE_TOPIC: &str = "ares/v1/claims/zone";
pub const CLAIM_NODE_TOPIC: &str = "ares/v1/claims/node";
pub const CLAIM_EDGE_TOPIC: &str = "ares/v1/claims/edge";
pub const CLAIM_ROUTE_TOPIC: &str = "ares/v1/claims/route";
pub const RELEASE_ZONE_TOPIC: &str = "ares/v1/leases/release/zone";
pub const RELEASE_NODE_TOPIC: &str = "ares/v1/leases/release/node";
pub const RELEASE_EDGE_TOPIC: &str = "ares/v1/leases/release/edge";
pub const ROUTES_PLAN_TOPIC: &str = "ares/v1/routes/plan";
pub const ZONES_LIST_TOPIC: &str = "ares/v1/zones/list";
pub const ZONE_GET_TOPIC: &str = "ares/v1/zones/get";
pub const FLEET_SNAPSHOT_TOPIC: &str = "ares/v1/fleet/snapshot";
pub const WORKSPACE_SET_TOPIC: &str = "ares/v1/workspace/set";
pub const HEALTH_TOPIC: &str = "ares/v1/health";
pub const AUTH_CHALLENGE_TOPIC: &str = "ares/v1/auth/challenge";
pub const AUTH_PROVE_TOPIC: &str = "ares/v1/auth/prove";

/// Canonical robot registration request. String fields are UTF-8 payload
/// sections; an empty `key` means the shared default key and `has_alive == 0`
/// means the core default heartbeat interval.
#[datapod::datapod(name = "ares.v1.register")]
pub struct Register {
    pub alive: u64,
    pub has_alive: u8,
    pub _pad: [u8; 7],
    #[dp(bytes, section = "robot")]
    pub robot: Vec<u8>,
    #[dp(bytes, section = "key")]
    pub key: Vec<u8>,
}

/// Canonical deregistration: the robot gives up its id, and every claim and
/// lease it holds goes with it. Only the robot's own key may do this. The
/// header has nothing to say, so it is one padding word.
#[datapod::datapod(name = "ares.v1.deregister")]
pub struct Deregister {
    pub _pad: [u8; 8],
    #[dp(bytes, section = "robot")]
    pub robot: Vec<u8>,
    #[dp(bytes, section = "key")]
    pub key: Vec<u8>,
}

/// Canonical heartbeat request. Presence flags distinguish an absent value
/// from zero. A negative zone remains the flat protocol's unknown sentinel.
///
/// Position rides on the heartbeat rather than a topic of its own: a robot
/// already sends one on a timer, and a pose without the liveness that makes it
/// current is worth little. `pos_frame` says which frame `pos_a/b/c` are in —
/// `0` none, `1` lat/lon/alt, `2` x/y/z east/north/up of the datum — so the
/// same three fields carry either without paying for both. `yaw` is REP-103:
/// radians counter-clockwise from east; `roll` and `pitch` complete the
/// attitude for a robot that reports one, in the same REP-103 sense.
#[datapod::datapod(name = "ares.v2.heartbeat")]
pub struct Heartbeat {
    pub zone: i64,
    pub node: u64,
    pub edge: u64,
    pub pos_a: f64,
    pub pos_b: f64,
    pub pos_c: f64,
    pub roll: f64,
    pub pitch: f64,
    pub yaw: f64,
    pub has_zone: u8,
    pub has_node: u8,
    pub has_edge: u8,
    pub pos_frame: u8,
    pub has_roll: u8,
    pub has_pitch: u8,
    pub has_yaw: u8,
    pub _pad: [u8; 1],
    #[dp(bytes, section = "robot")]
    pub robot: Vec<u8>,
    #[dp(bytes, section = "key")]
    pub key: Vec<u8>,
}

/// `Heartbeat::pos_frame` values.
pub const POS_FRAME_NONE: u8 = 0;
pub const POS_FRAME_GLOBAL: u8 = 1;
pub const POS_FRAME_LOCAL: u8 = 2;

/// Canonical atomic claim request. `id` is a payload section of little-endian
/// `u64` values, preserving the flat protocol's all-or-nothing multi-claim.
#[datapod::datapod(name = "ares.v1.claim")]
pub struct Claim {
    pub lease_time: u64,
    pub access_mode: u8,
    pub has_access_mode: u8,
    pub has_lease_time: u8,
    pub _pad: [u8; 5],
    #[dp(bytes, section = "robot")]
    pub robot: Vec<u8>,
    #[dp(bytes, section = "key")]
    pub key: Vec<u8>,
    #[dp(bytes, section = "id")]
    pub id: Vec<u64>,
}

/// Canonical atomic route claim: the nodes a robot stops at and the edges it
/// crosses, in one all-or-nothing request. Two `u64` sections rather than one,
/// because a route spans both kinds and claiming them separately is not
/// atomic.
#[datapod::datapod(name = "ares.v1.claim.route")]
pub struct ClaimRoute {
    pub lease_time: u64,
    pub access_mode: u8,
    pub has_access_mode: u8,
    pub has_lease_time: u8,
    pub _pad: [u8; 5],
    #[dp(bytes, section = "robot")]
    pub robot: Vec<u8>,
    #[dp(bytes, section = "key")]
    pub key: Vec<u8>,
    #[dp(bytes, section = "node")]
    pub node: Vec<u64>,
    #[dp(bytes, section = "edge")]
    pub edge: Vec<u64>,
}

/// Canonical lease release request.
#[datapod::datapod(name = "ares.v1.release")]
pub struct Release {
    pub id: u64,
    #[dp(bytes, section = "robot")]
    pub robot: Vec<u8>,
    #[dp(bytes, section = "key")]
    pub key: Vec<u8>,
}

/// Canonical reply shared by every operation.
#[datapod::datapod(name = "ares.v1.reply")]
pub struct Reply {
    pub blocked: u64,
    pub decision: u8,
    pub reason: u8,
    pub has_blocked: u8,
    pub _pad: [u8; 5],
}

/// Empty request used by canonical read operations that take no arguments.
#[datapod::datapod(name = "ares.v1.read.empty")]
pub struct ReadEmpty {
    pub _reserved: u8,
}

/// Resource lookup request. The UTF-8 token is either a UUID or a numeric
/// workspace alias, matching the public REST address forms.
#[datapod::datapod(name = "ares.v1.resource.get")]
pub struct ResourceGet {
    #[dp(bytes, section = "id")]
    pub id: Vec<u8>,
}

/// Route planning request. `start` and `goal` are UTF-8 tokens, each either a
/// UUID or a numeric workspace alias, matching every other address form.
#[datapod::datapod(name = "ares.v1.route.plan")]
pub struct RoutePlanRequest {
    pub use_penalties: u8,
    pub _pad: [u8; 7],
    #[dp(bytes, section = "start")]
    pub start: Vec<u8>,
    #[dp(bytes, section = "goal")]
    pub goal: Vec<u8>,
}

/// Ask for a nonce to sign, proving a `did:key` identity.
#[datapod::datapod(name = "ares.v1.auth.challenge")]
pub struct AuthChallenge {
    #[dp(bytes, section = "robot")]
    pub robot: Vec<u8>,
}

/// Answer a challenge. `did` is the robot's `did:key:…`; `signature` is the
/// raw Ed25519 signature over the nonce.
#[datapod::datapod(name = "ares.v1.auth.prove")]
pub struct AuthProve {
    #[dp(bytes, section = "robot")]
    pub robot: Vec<u8>,
    #[dp(bytes, section = "did")]
    pub did: Vec<u8>,
    #[dp(bytes, section = "signature")]
    pub signature: Vec<u8>,
}

/// Canonical envelope for the existing structured read models.
///
/// The datapod header carries success/failure and the payload carries the
/// versioned ARES read-model document. Mutations continue to use their fully
/// fielded datapods above; this envelope exists because fleet/workspace views
/// are recursive, variable-size serde models.
#[datapod::datapod(name = "ares.v1.read.reply")]
pub struct ReadReply {
    pub success: u8,
    #[dp(bytes, section = "body")]
    pub body: Vec<u8>,
}

/// One slice of a pushed workspace.
///
/// A zoneout `WorkspaceJson` is unbounded — a site with detailed boundaries
/// runs to megabytes — while a peerbus message is bounded by the node's
/// payload cap. So the client splits the document and the core reassembles it,
/// rather than the cap dictating how large a workspace may be.
///
/// `transfer` is chosen by the client and only has to be unique among pushes
/// in flight at once; it exists so two clients pushing concurrently cannot
/// interleave into one corrupt document. The core applies the workspace when
/// it holds all `total` chunks.
/// `key` is the operator key authorising the replacement, carried on every
/// chunk so the core can refuse the transfer without buffering it.
#[datapod::datapod(name = "ares.v1.workspace.chunk")]
pub struct WorkspaceChunk {
    pub transfer: u64,
    pub index: u32,
    pub total: u32,
    #[dp(bytes, section = "body")]
    pub body: Vec<u8>,
    #[dp(bytes, section = "key")]
    pub key: Vec<u8>,
}

impl From<FlatReply> for Reply {
    fn from(value: FlatReply) -> Self {
        Self {
            blocked: value.blocked.unwrap_or_default(),
            decision: value.decision,
            reason: value.reason,
            has_blocked: u8::from(value.blocked.is_some()),
            _pad: [0; 5],
        }
    }
}

impl From<Reply> for FlatReply {
    fn from(value: Reply) -> Self {
        Self {
            decision: value.decision,
            reason: value.reason,
            blocked: (value.has_blocked != 0).then_some(value.blocked),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Operation {
    Register,
    Deregister,
    Heartbeat,
    Claim(ClaimTargetKind),
    ClaimRoute,
    Release(ClaimTargetKind),
    RoutePlan,
    AuthChallenge,
    AuthProve,
    ZonesList,
    ZoneGet,
    FleetSnapshot,
    WorkspaceSet,
    Health,
}

/// Reassembles chunked workspace pushes.
///
/// Lives on the core's polling thread rather than in [`ServeState`], because
/// a half-delivered workspace is transport bookkeeping — the coordinator
/// should only ever see a whole one.
#[derive(Default)]
struct WorkspaceAssembler {
    transfers: std::collections::BTreeMap<u64, PendingWorkspace>,
}

struct PendingWorkspace {
    total: u32,
    chunks: std::collections::BTreeMap<u32, Vec<u8>>,
    bytes: usize,
    key: Vec<u8>,
}

impl WorkspaceAssembler {
    /// Returns the whole document once the final missing chunk arrives, or
    /// `None` while it's still incomplete.
    fn accept(&mut self, chunk: WorkspaceChunk) -> ApiResult<Option<(Vec<u8>, Vec<u8>)>> {
        if chunk.total == 0 {
            return Err(ApiError::new("workspace push declares zero chunks"));
        }
        if chunk.index >= chunk.total {
            return Err(ApiError::new(format!(
                "workspace chunk {} out of range for a {}-chunk push",
                chunk.index, chunk.total
            )));
        }

        let pending = self
            .transfers
            .entry(chunk.transfer)
            .or_insert_with(|| PendingWorkspace {
                total: chunk.total,
                chunks: std::collections::BTreeMap::new(),
                bytes: 0,
                key: chunk.key.clone(),
            });

        // A client that changes its mind mid-push would silently produce a
        // spliced document; treat it as a fresh transfer instead.
        if pending.total != chunk.total {
            self.transfers.remove(&chunk.transfer);
            return Err(ApiError::new(
                "workspace push changed its chunk count mid-transfer",
            ));
        }

        pending.bytes += chunk.body.len();
        if pending.bytes > MAX_WORKSPACE_BYTES {
            self.transfers.remove(&chunk.transfer);
            return Err(ApiError::new(format!(
                "workspace push exceeds the {MAX_WORKSPACE_BYTES}-byte ceiling"
            )));
        }
        pending.chunks.insert(chunk.index, chunk.body);

        if pending.chunks.len() as u32 != pending.total {
            return Ok(None);
        }

        let pending = self
            .transfers
            .remove(&chunk.transfer)
            .expect("just entered");
        let key = pending.key.clone();
        Ok(Some((
            pending.chunks.into_values().flatten().collect(),
            key,
        )))
    }
}

/// Running canonical core service.
pub struct CoreService {
    _node: Node,
    running: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl CoreService {
    pub fn start(state: ServeState) -> peerbus::Result<Self> {
        Self::with_identity(state, CORE_IDENTITY)
    }

    pub fn with_identity(state: ServeState, identity: &str) -> peerbus::Result<Self> {
        let node = Node::builder()
            .identity(identity)
            .local_config(local_config())
            .no_relay()
            .bind()?;
        let mut servers = vec![
            (Operation::Register, node.req_server(REGISTER_TOPIC)?),
            (Operation::Deregister, node.req_server(DEREGISTER_TOPIC)?),
            (Operation::Heartbeat, node.req_server(HEARTBEAT_TOPIC)?),
            (
                Operation::Claim(ClaimTargetKind::Zone),
                node.req_server(CLAIM_ZONE_TOPIC)?,
            ),
            (
                Operation::Claim(ClaimTargetKind::Node),
                node.req_server(CLAIM_NODE_TOPIC)?,
            ),
            (
                Operation::Claim(ClaimTargetKind::Edge),
                node.req_server(CLAIM_EDGE_TOPIC)?,
            ),
            (Operation::ClaimRoute, node.req_server(CLAIM_ROUTE_TOPIC)?),
            (
                Operation::Release(ClaimTargetKind::Zone),
                node.req_server(RELEASE_ZONE_TOPIC)?,
            ),
            (
                Operation::Release(ClaimTargetKind::Node),
                node.req_server(RELEASE_NODE_TOPIC)?,
            ),
            (
                Operation::Release(ClaimTargetKind::Edge),
                node.req_server(RELEASE_EDGE_TOPIC)?,
            ),
            (
                Operation::AuthChallenge,
                node.req_server(AUTH_CHALLENGE_TOPIC)?,
            ),
            (Operation::AuthProve, node.req_server(AUTH_PROVE_TOPIC)?),
            (Operation::RoutePlan, node.req_server(ROUTES_PLAN_TOPIC)?),
            (Operation::ZonesList, node.req_server(ZONES_LIST_TOPIC)?),
            (Operation::ZoneGet, node.req_server(ZONE_GET_TOPIC)?),
            (
                Operation::FleetSnapshot,
                node.req_server(FLEET_SNAPSHOT_TOPIC)?,
            ),
            (
                Operation::WorkspaceSet,
                node.req_server(WORKSPACE_SET_TOPIC)?,
            ),
            (Operation::Health, node.req_server(HEALTH_TOPIC)?),
        ];

        discard_requests_predating_this_core(&mut servers);

        let running = Arc::new(AtomicBool::new(true));
        let worker_running = Arc::clone(&running);
        let worker = thread::Builder::new()
            .name("ares-peerbus-core".into())
            .spawn(move || {
                // Owned by the loop: partial pushes never reach the coordinator.
                let mut assembler = WorkspaceAssembler::default();
                while worker_running.load(Ordering::Acquire) {
                    let mut handled = false;
                    for (operation, server) in &mut servers {
                        match server.take() {
                            Ok(Some((request, responder))) => {
                                handled = true;
                                let message =
                                    handle_request(&state, *operation, &request, &mut assembler);
                                let _ = responder.respond(&message);
                            }
                            Ok(None) => {}
                            Err(_) => {}
                        }
                    }
                    if !handled {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
            })
            .map_err(|err| peerbus::Error::invalid_argument(err.to_string()))?;

        Ok(Self {
            _node: node,
            running,
            worker: Some(worker),
        })
    }
}

impl Drop for CoreService {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Throw away anything already queued on the topics when the core starts.
///
/// peerbus consumers begin reading `history_depth` messages behind the latest
/// write, and that depth is clamped to a minimum of one — so attaching to a
/// segment a previous core left behind re-delivers that core's *last request*.
/// The segments outlive the process whenever it does not exit cleanly, which a
/// plain `kill` guarantees.
///
/// For a pub/sub topic replaying the last sample is the point: it is state, and
/// a late joiner wants it. These topics carry commands. Replaying one silently
/// re-executes it against a core that never received it — a dead session's
/// final `register` resurrects a robot nobody asked for, out of an empty
/// server, with no client running.
///
/// Nothing can legitimately be waiting here: the core is not serving yet and no
/// adapter has connected. So anything present belongs to a session that is
/// already gone, and the responder is dropped without a reply.
fn discard_requests_predating_this_core(
    servers: &mut [(Operation, peerbus::ReqServer<DatapodMsg, DatapodMsg>)],
) {
    for (operation, server) in servers.iter_mut() {
        let mut discarded = 0;
        while let Ok(Some(_)) = server.take() {
            discarded += 1;
        }
        if discarded > 0 {
            tracing::warn!(
                ?operation,
                discarded,
                "discarded request(s) replayed from a previous core"
            );
        }
    }
}

fn handle_request(
    state: &ServeState,
    operation: Operation,
    request: &peerbus::ReqSample<DatapodMsg>,
    assembler: &mut WorkspaceAssembler,
) -> DatapodMsg {
    let message = DatapodMsg::new(request.type_hash(), request.wire().to_vec());
    match operation {
        Operation::Register => decode::<Register>(&message)
            .map(|req| {
                crate::wire::flat_register(
                    state,
                    utf8(&req.robot).unwrap_or(""),
                    key(&req.key),
                    (req.has_alive != 0).then_some(req.alive),
                )
            })
            .unwrap_or_else(|error| bad_request(operation, error))
            .into_message(),
        Operation::Deregister => decode::<Deregister>(&message)
            .map(|req| {
                crate::wire::flat_deregister(state, utf8(&req.robot).unwrap_or(""), key(&req.key))
            })
            .unwrap_or_else(|error| bad_request(operation, error))
            .into_message(),
        Operation::Heartbeat => decode::<Heartbeat>(&message)
            .map(|req| {
                let position = match req.pos_frame {
                    POS_FRAME_GLOBAL => Some(crate::wire::ReportedPosition::Global {
                        lat: req.pos_a,
                        lon: req.pos_b,
                        alt: req.pos_c,
                    }),
                    POS_FRAME_LOCAL => Some(crate::wire::ReportedPosition::Local {
                        x: req.pos_a,
                        y: req.pos_b,
                        z: req.pos_c,
                    }),
                    _ => None,
                };
                crate::wire::flat_heartbeat(
                    state,
                    utf8(&req.robot).unwrap_or(""),
                    key(&req.key),
                    (req.has_zone != 0).then_some(req.zone),
                    (req.has_node != 0).then_some(req.node),
                    (req.has_edge != 0).then_some(req.edge),
                    position,
                    crate::wire::ReportedAttitude {
                        roll: (req.has_roll != 0).then_some(req.roll),
                        pitch: (req.has_pitch != 0).then_some(req.pitch),
                        yaw: (req.has_yaw != 0).then_some(req.yaw),
                    },
                )
            })
            .unwrap_or_else(|error| bad_request(operation, error))
            .into_message(),
        Operation::Claim(kind) => decode::<Claim>(&message)
            .map(|req| {
                crate::wire::flat_claim(
                    state,
                    kind,
                    key(&req.key),
                    utf8(&req.robot).unwrap_or(""),
                    &req.id,
                    (req.has_access_mode != 0).then_some(req.access_mode),
                    (req.has_lease_time != 0).then_some(req.lease_time),
                )
            })
            .unwrap_or_else(|error| bad_request(operation, error))
            .into_message(),
        Operation::ClaimRoute => decode::<ClaimRoute>(&message)
            .map(|req| {
                crate::wire::flat_claim_route(
                    state,
                    key(&req.key),
                    utf8(&req.robot).unwrap_or(""),
                    &req.node,
                    &req.edge,
                    (req.has_access_mode != 0).then_some(req.access_mode),
                    (req.has_lease_time != 0).then_some(req.lease_time),
                )
            })
            .unwrap_or_else(|error| bad_request(operation, error))
            .into_message(),
        Operation::Release(kind) => decode::<Release>(&message)
            .map(|req| {
                crate::wire::flat_release(
                    state,
                    kind,
                    key(&req.key),
                    utf8(&req.robot).unwrap_or(""),
                    req.id,
                )
            })
            .unwrap_or_else(|error| bad_request(operation, error))
            .into_message(),
        Operation::AuthChallenge => decode::<AuthChallenge>(&message)
            .map_err(|err| ApiError::new(format!("invalid auth/challenge request: {err}")))
            .and_then(|req| {
                let robot =
                    utf8(&req.robot).map_err(|err| ApiError::new(format!("robot id: {err}")))?;
                crate::wire::flat_challenge(state, robot)
            })
            .into_read_message(),
        Operation::AuthProve => decode::<AuthProve>(&message)
            .map_err(|err| ApiError::new(format!("invalid auth/prove request: {err}")))
            .and_then(|req| {
                let robot =
                    utf8(&req.robot).map_err(|err| ApiError::new(format!("robot id: {err}")))?;
                let did = utf8(&req.did).map_err(|err| ApiError::new(format!("did: {err}")))?;
                let signature: String = req.signature.iter().map(|b| format!("{b:02x}")).collect();
                crate::wire::flat_prove(state, robot, did, &signature)
            })
            .into_read_message(),
        Operation::RoutePlan => decode::<RoutePlanRequest>(&message)
            .map_err(|err| ApiError::new(format!("invalid routes/plan request: {err}")))
            .and_then(|req| {
                let start = parse_resource_ref(
                    utf8(&req.start).map_err(|err| ApiError::new(format!("start node: {err}")))?,
                )?;
                let goal = parse_resource_ref(
                    utf8(&req.goal).map_err(|err| ApiError::new(format!("goal node: {err}")))?,
                )?;
                crate::wire::plan_route_request(
                    state,
                    crate::wire::PlanRouteRequest {
                        start_node_id: start,
                        goal_node_id: goal,
                        use_penalties: req.use_penalties != 0,
                    },
                )
            })
            .into_read_message(),
        Operation::ZonesList => decode::<ReadEmpty>(&message)
            .map_err(|err| ApiError::new(format!("invalid zones/list request: {err}")))
            .and_then(|_| crate::wire::list_zones(state))
            .into_read_message(),
        Operation::ZoneGet => decode::<ResourceGet>(&message)
            .map_err(|err| ApiError::new(format!("invalid zones/get request: {err}")))
            .and_then(|req| {
                parse_resource_ref(
                    utf8(&req.id).map_err(|err| ApiError::new(format!("zone id: {err}")))?,
                )
            })
            .and_then(|id| crate::wire::find_zone(state, id))
            .into_read_message(),
        Operation::FleetSnapshot => decode::<ReadEmpty>(&message)
            .map_err(|err| ApiError::new(format!("invalid fleet/snapshot request: {err}")))
            .and_then(|_| crate::wire::fleet_snapshot(state))
            .into_read_message(),
        Operation::Health => decode::<ReadEmpty>(&message)
            .map_err(|err| ApiError::new(format!("invalid health request: {err}")))
            .map(|_| crate::wire::health_with_datum(state))
            .into_read_message(),
        // Replies `null` for every chunk but the last, so the client can tell
        // "buffered, keep going" from "applied".
        Operation::WorkspaceSet => decode::<WorkspaceChunk>(&message)
            .map_err(|err| ApiError::new(format!("invalid workspace/set request: {err}")))
            .and_then(|chunk| {
                // Authorise before buffering, so an unauthorised client cannot
                // pin memory by starting a transfer it may not finish.
                let key = utf8(&chunk.key)
                    .map_err(|err| ApiError::new(format!("operator key: {err}")))?
                    .to_string();
                state.check_workspace_key((!key.is_empty()).then_some(key.as_str()))?;
                assembler.accept(chunk)
            })
            .and_then(|document| match document {
                Some((bytes, key)) => {
                    let key = String::from_utf8_lossy(&key).into_owned();
                    crate::wire::set_workspace(
                        state,
                        bytes.as_slice(),
                        (!key.is_empty()).then_some(key.as_str()),
                    )
                    .map(Some)
                }
                None => Ok(None),
            })
            .into_read_message(),
    }
}

trait IntoReplyMessage {
    fn into_message(self) -> DatapodMsg;
}

impl IntoReplyMessage for FlatReply {
    fn into_message(self) -> DatapodMsg {
        DatapodMsg::from_datapod(&Reply::from(self))
    }
}

trait IntoReadMessage {
    fn into_read_message(self) -> DatapodMsg;
}

impl<T: Serialize> IntoReadMessage for Result<T, ApiError> {
    fn into_read_message(self) -> DatapodMsg {
        let reply = match self {
            Ok(value) => match serde_json::to_vec(&value) {
                Ok(body) => ReadReply { success: 1, body },
                Err(err) => ReadReply {
                    success: 0,
                    body: format!("serialize canonical read reply: {err}").into_bytes(),
                },
            },
            Err(err) => ReadReply {
                success: 0,
                body: err.message.into_bytes(),
            },
        };
        DatapodMsg::from_datapod(&reply)
    }
}

/// Attempt every canonical decode against raw bytes, returning the first
/// canonical name that accepts them. See [`crate::wire::fuzz_decode_canonical`].
///
/// Each type is tried under *its own* type hash. Handing every decoder a
/// placeholder hash instead would have them reject on identity before reading
/// a single wire byte — which fuzzes nothing, and looks like success.
pub fn decode_any(bytes: &[u8]) -> Option<&'static str> {
    macro_rules! try_decode {
        ($name:literal, $ty:ty, $probe:expr) => {
            // The hash is a property of the type, taken from an encoded
            // instance of it; the fuzzer's bytes then stand in for the wire.
            let hash = DatapodMsg::from_datapod(&$probe).type_hash();
            if DatapodMsg::new(hash, bytes.to_vec())
                .to_datapod::<$ty>()
                .is_ok()
            {
                return Some($name);
            }
        };
    }

    try_decode!(
        "ares.v1.deregister",
        Deregister,
        Deregister {
            _pad: [0; 8],
            robot: Vec::new(),
            key: Vec::new(),
        }
    );
    try_decode!(
        "ares.v1.register",
        Register,
        Register {
            alive: 0,
            has_alive: 0,
            _pad: [0; 7],
            robot: Vec::new(),
            key: Vec::new(),
        }
    );
    try_decode!(
        "ares.v2.heartbeat",
        Heartbeat,
        Heartbeat {
            zone: 0,
            node: 0,
            edge: 0,
            pos_a: 0.0,
            pos_b: 0.0,
            pos_c: 0.0,
            roll: 0.0,
            pitch: 0.0,
            yaw: 0.0,
            has_zone: 0,
            has_node: 0,
            has_edge: 0,
            pos_frame: 0,
            has_roll: 0,
            has_pitch: 0,
            has_yaw: 0,
            _pad: [0; 1],
            robot: Vec::new(),
            key: Vec::new(),
        }
    );
    try_decode!(
        "ares.v1.claim",
        Claim,
        Claim {
            lease_time: 0,
            access_mode: 0,
            has_access_mode: 0,
            has_lease_time: 0,
            _pad: [0; 5],
            robot: Vec::new(),
            key: Vec::new(),
            id: Vec::new(),
        }
    );
    try_decode!(
        "ares.v1.claim.route",
        ClaimRoute,
        ClaimRoute {
            lease_time: 0,
            access_mode: 0,
            has_access_mode: 0,
            has_lease_time: 0,
            _pad: [0; 5],
            robot: Vec::new(),
            key: Vec::new(),
            node: Vec::new(),
            edge: Vec::new(),
        }
    );
    try_decode!(
        "ares.v1.release",
        Release,
        Release {
            id: 0,
            robot: Vec::new(),
            key: Vec::new(),
        }
    );
    try_decode!(
        "ares.v1.route.plan",
        RoutePlanRequest,
        RoutePlanRequest {
            use_penalties: 0,
            _pad: [0; 7],
            start: Vec::new(),
            goal: Vec::new(),
        }
    );
    try_decode!(
        "ares.v1.resource.get",
        ResourceGet,
        ResourceGet { id: Vec::new() }
    );
    try_decode!(
        "ares.v1.workspace.chunk",
        WorkspaceChunk,
        WorkspaceChunk {
            transfer: 0,
            index: 0,
            total: 0,
            body: Vec::new(),
            key: Vec::new(),
        }
    );
    None
}

fn decode<T>(message: &DatapodMsg) -> Result<T, datapod::WireError>
where
    T: DataPodDecode + DataPodValidate,
    T::Header: LeWireHeader,
{
    message.to_datapod::<T>()
}

fn utf8(bytes: &[u8]) -> Result<&str, std::str::Utf8Error> {
    std::str::from_utf8(bytes)
}

fn key(bytes: &[u8]) -> &str {
    utf8(bytes)
        .ok()
        .filter(|key| !key.is_empty())
        .unwrap_or("0")
}

fn parse_resource_ref(raw: &str) -> Result<ResourceRef, ApiError> {
    if let Ok(uuid) = uuid::Uuid::parse_str(raw.trim()) {
        return Ok(ResourceRef::Uuid(uuid));
    }
    if let Ok(numeric_id) = raw.trim().parse::<u64>() {
        return Ok(ResourceRef::Numeric(numeric_id));
    }
    Err(ApiError::new(format!(
        "resource id {raw:?} is neither a UUID nor an unsigned integer"
    )))
}

fn bad_request(operation: Operation, _: datapod::WireError) -> FlatReply {
    let reason = match operation {
        Operation::Register => crate::wire::reason::register::BAD_ID,
        Operation::Deregister | Operation::Heartbeat => {
            crate::wire::reason::heartbeat::NOT_REGISTERED
        }
        Operation::Claim(_) | Operation::ClaimRoute => crate::wire::reason::claim::BAD_REQUEST,
        Operation::Release(_) => crate::wire::reason::release::UNKNOWN_OR_BAD,
        // These reply through ReadReply, never FlatReply, so they only appear
        // here to keep the match exhaustive.
        Operation::AuthChallenge
        | Operation::AuthProve
        | Operation::RoutePlan
        | Operation::ZonesList
        | Operation::ZoneGet
        | Operation::FleetSnapshot
        | Operation::WorkspaceSet
        | Operation::Health => crate::wire::reason::claim::BAD_REQUEST,
    };
    FlatReply::deny(reason)
}

/// Cloneable peerbus-native client used by all bundled adapters.
#[derive(Clone)]
pub struct Client {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    _node: Node,
    /// One lock per topic, not one lock over the map. A `call` blocks until the
    /// core replies, so a shared lock would serialize every adapter's traffic
    /// through whichever request happened to be in flight — across unrelated
    /// topics. The map itself is built once at connect and never mutated.
    requests:
        std::collections::BTreeMap<&'static str, Mutex<peerbus::ReqClient<DatapodMsg, DatapodMsg>>>,
}

impl Client {
    pub fn connect(core_peer: impl Into<String>) -> peerbus::Result<Self> {
        let node = Node::builder()
            .local_config(local_config())
            .no_relay()
            .bind()?;
        let core_peer = core_peer.into();
        let mut requests = std::collections::BTreeMap::new();
        for topic in [
            REGISTER_TOPIC,
            DEREGISTER_TOPIC,
            HEARTBEAT_TOPIC,
            CLAIM_ZONE_TOPIC,
            CLAIM_NODE_TOPIC,
            CLAIM_EDGE_TOPIC,
            CLAIM_ROUTE_TOPIC,
            RELEASE_ZONE_TOPIC,
            RELEASE_NODE_TOPIC,
            RELEASE_EDGE_TOPIC,
            AUTH_CHALLENGE_TOPIC,
            AUTH_PROVE_TOPIC,
            ROUTES_PLAN_TOPIC,
            ZONES_LIST_TOPIC,
            ZONE_GET_TOPIC,
            FLEET_SNAPSHOT_TOPIC,
            WORKSPACE_SET_TOPIC,
            HEALTH_TOPIC,
        ] {
            requests.insert(
                topic,
                Mutex::new(node.req_client::<DatapodMsg, DatapodMsg>(core_peer.as_str(), topic)?),
            );
        }
        Ok(Self {
            inner: Arc::new(ClientInner {
                _node: node,
                requests,
            }),
        })
    }

    pub fn deregister(&self, robot: &str, key: &str) -> Result<FlatReply, ApiError> {
        self.call(
            DEREGISTER_TOPIC,
            &Deregister {
                _pad: [0; 8],
                robot: robot.as_bytes().to_vec(),
                key: key.as_bytes().to_vec(),
            },
        )
    }

    pub fn register(
        &self,
        robot: &str,
        key: &str,
        alive: Option<u64>,
    ) -> Result<FlatReply, ApiError> {
        self.call(
            REGISTER_TOPIC,
            &Register {
                alive: alive.unwrap_or_default(),
                has_alive: u8::from(alive.is_some()),
                _pad: [0; 7],
                robot: robot.as_bytes().to_vec(),
                key: key.as_bytes().to_vec(),
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn heartbeat(
        &self,
        robot: &str,
        key: &str,
        zone: Option<i64>,
        node: Option<u64>,
        edge: Option<u64>,
        position: Option<crate::wire::ReportedPosition>,
        attitude: impl Into<crate::wire::ReportedAttitude>,
    ) -> Result<FlatReply, ApiError> {
        let attitude = attitude.into();
        let (pos_frame, pos_a, pos_b, pos_c) = match position {
            Some(crate::wire::ReportedPosition::Global { lat, lon, alt }) => {
                (POS_FRAME_GLOBAL, lat, lon, alt)
            }
            Some(crate::wire::ReportedPosition::Local { x, y, z }) => (POS_FRAME_LOCAL, x, y, z),
            None => (POS_FRAME_NONE, 0.0, 0.0, 0.0),
        };
        self.call(
            HEARTBEAT_TOPIC,
            &Heartbeat {
                zone: zone.unwrap_or_default(),
                node: node.unwrap_or_default(),
                edge: edge.unwrap_or_default(),
                pos_a,
                pos_b,
                pos_c,
                roll: attitude.roll.unwrap_or_default(),
                pitch: attitude.pitch.unwrap_or_default(),
                yaw: attitude.yaw.unwrap_or_default(),
                has_zone: u8::from(zone.is_some()),
                has_node: u8::from(node.is_some()),
                has_edge: u8::from(edge.is_some()),
                pos_frame,
                has_roll: u8::from(attitude.roll.is_some()),
                has_pitch: u8::from(attitude.pitch.is_some()),
                has_yaw: u8::from(attitude.yaw.is_some()),
                _pad: [0; 1],
                robot: robot.as_bytes().to_vec(),
                key: key.as_bytes().to_vec(),
            },
        )
    }

    pub fn claim(
        &self,
        kind: ClaimTargetKind,
        key: &str,
        robot: &str,
        id: &[u64],
        access_mode: Option<u8>,
        lease_time: Option<u64>,
    ) -> Result<FlatReply, ApiError> {
        self.call(
            claim_topic(kind),
            &Claim {
                lease_time: lease_time.unwrap_or_default(),
                access_mode: access_mode.unwrap_or_default(),
                has_access_mode: u8::from(access_mode.is_some()),
                has_lease_time: u8::from(lease_time.is_some()),
                _pad: [0; 5],
                robot: robot.as_bytes().to_vec(),
                key: key.as_bytes().to_vec(),
                id: id.to_vec(),
            },
        )
    }

    /// Claim a whole route — nodes and edges together — atomically.
    pub fn claim_route(
        &self,
        key: &str,
        robot: &str,
        node: &[u64],
        edge: &[u64],
        access_mode: Option<u8>,
        lease_time: Option<u64>,
    ) -> Result<FlatReply, ApiError> {
        self.call(
            CLAIM_ROUTE_TOPIC,
            &ClaimRoute {
                lease_time: lease_time.unwrap_or_default(),
                access_mode: access_mode.unwrap_or_default(),
                has_access_mode: u8::from(access_mode.is_some()),
                has_lease_time: u8::from(lease_time.is_some()),
                _pad: [0; 5],
                robot: robot.as_bytes().to_vec(),
                key: key.as_bytes().to_vec(),
                node: node.to_vec(),
                edge: edge.to_vec(),
            },
        )
    }

    pub fn release(
        &self,
        kind: ClaimTargetKind,
        key: &str,
        robot: &str,
        id: u64,
    ) -> Result<FlatReply, ApiError> {
        self.call(
            release_topic(kind),
            &Release {
                id,
                robot: robot.as_bytes().to_vec(),
                key: key.as_bytes().to_vec(),
            },
        )
    }

    /// Ask for a nonce to sign, proving a `did:key` identity.
    pub fn challenge(&self, robot: &str) -> Result<crate::wire::ChallengeView, ApiError> {
        self.call_read(
            AUTH_CHALLENGE_TOPIC,
            &AuthChallenge {
                robot: robot.as_bytes().to_vec(),
            },
        )
    }

    /// Answer a challenge and receive a bearer token.
    pub fn prove(
        &self,
        robot: &str,
        did: &str,
        signature: &[u8],
    ) -> Result<crate::wire::TokenView, ApiError> {
        self.call_read(
            AUTH_PROVE_TOPIC,
            &AuthProve {
                robot: robot.as_bytes().to_vec(),
                did: did.as_bytes().to_vec(),
                signature: signature.to_vec(),
            },
        )
    }

    /// Plan a route between two node tokens (UUID or numeric alias).
    pub fn plan_route(
        &self,
        start: &str,
        goal: &str,
        use_penalties: bool,
    ) -> Result<crate::wire::PlanRouteResponse, ApiError> {
        self.call_read(
            ROUTES_PLAN_TOPIC,
            &RoutePlanRequest {
                use_penalties: u8::from(use_penalties),
                _pad: [0; 7],
                start: start.as_bytes().to_vec(),
                goal: goal.as_bytes().to_vec(),
            },
        )
    }

    pub fn list_zones(&self) -> Result<Vec<crate::wire::ZoneView>, ApiError> {
        self.call_read(ZONES_LIST_TOPIC, &ReadEmpty { _reserved: 0 })
    }

    pub fn zone(&self, id: &str) -> Result<crate::wire::ZoneView, ApiError> {
        self.call_read(
            ZONE_GET_TOPIC,
            &ResourceGet {
                id: id.as_bytes().to_vec(),
            },
        )
    }

    pub fn fleet_snapshot(&self) -> Result<crate::wire::FleetSnapshot, ApiError> {
        self.call_read(FLEET_SNAPSHOT_TOPIC, &ReadEmpty { _reserved: 0 })
    }

    /// Health, including the datum once a workspace is bound.
    pub fn health(&self) -> Result<crate::wire::Health, ApiError> {
        self.call_read(HEALTH_TOPIC, &ReadEmpty { _reserved: 0 })
    }

    /// Push a whole zoneout workspace, splitting it across as many chunks as
    /// its size needs. `document` is a serialized `zoneout::WorkspaceJson` —
    /// the same flat wire format the editors read and write.
    ///
    /// The core applies it only once every chunk has landed, so a transfer
    /// that dies partway leaves the previous workspace serving.
    pub fn set_workspace(
        &self,
        document: &[u8],
        key: &str,
    ) -> Result<crate::wire::WorkspaceAccepted, ApiError> {
        if document.is_empty() {
            return Err(ApiError::new("workspace push is empty"));
        }
        let transfer = next_transfer_id();
        let chunks: Vec<&[u8]> = document.chunks(WORKSPACE_CHUNK_BYTES).collect();
        let total = u32::try_from(chunks.len())
            .map_err(|_| ApiError::new("workspace is too large to address in one push"))?;

        let mut applied = None;
        for (index, body) in chunks.into_iter().enumerate() {
            let accepted: Option<crate::wire::WorkspaceAccepted> = self.call_read(
                WORKSPACE_SET_TOPIC,
                &WorkspaceChunk {
                    transfer,
                    index: index as u32,
                    total,
                    body: body.to_vec(),
                    key: key.as_bytes().to_vec(),
                },
            )?;
            applied = accepted;
        }

        applied.ok_or_else(|| {
            ApiError::new("core buffered every workspace chunk but never applied the workspace")
        })
    }

    fn call<T>(&self, topic: &str, request: &T) -> Result<FlatReply, ApiError>
    where
        T: DataPod,
        T::Header: LeWireHeader,
    {
        let response = self.call_message(topic, request)?;
        response
            .to_datapod::<Reply>()
            .map(FlatReply::from)
            .map_err(|err| ApiError::new(format!("invalid peerbus reply: {err}")))
    }

    fn call_read<T, R>(&self, topic: &str, request: &T) -> Result<R, ApiError>
    where
        T: DataPod,
        T::Header: LeWireHeader,
        R: DeserializeOwned,
    {
        let response = self.call_message(topic, request)?;
        let reply = response
            .to_datapod::<ReadReply>()
            .map_err(|err| ApiError::new(format!("invalid peerbus read reply: {err}")))?;
        if reply.success == 0 {
            return Err(ApiError::new(
                String::from_utf8_lossy(&reply.body).into_owned(),
            ));
        }
        serde_json::from_slice(&reply.body)
            .map_err(|err| ApiError::new(format!("decode canonical read reply: {err}")))
    }

    fn call_message<T>(&self, topic: &str, request: &T) -> Result<DatapodMsg, ApiError>
    where
        T: DataPod,
        T::Header: LeWireHeader,
    {
        let slot = self
            .inner
            .requests
            .get(topic)
            .ok_or_else(|| ApiError::new(format!("unknown canonical peerbus topic {topic}")))?;
        let mut client = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let request = DatapodMsg::from_datapod(request);
        let response = client.call(&request).map_err(peerbus_error)?;
        Ok(DatapodMsg::new(
            response.type_hash(),
            response.wire().to_vec(),
        ))
    }
}

fn peerbus_error(error: peerbus::Error) -> ApiError {
    ApiError::new(format!("peerbus request failed: {error}"))
}

/// Identifies one workspace push. Only has to be unique among transfers in
/// flight at the same time, so a process id and a counter are enough to keep
/// two clients from interleaving into one document.
fn next_transfer_id() -> u64 {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    ((std::process::id() as u64) << 32) | (counter & 0xffff_ffff)
}

fn local_config() -> peerbus::LocalConfig {
    peerbus::LocalConfig {
        max_payload_bytes: LOCAL_MAX_PAYLOAD_BYTES,
        ..peerbus::LocalConfig::default()
    }
}

pub fn claim_topic(kind: ClaimTargetKind) -> &'static str {
    match kind {
        ClaimTargetKind::Zone => CLAIM_ZONE_TOPIC,
        ClaimTargetKind::Node => CLAIM_NODE_TOPIC,
        ClaimTargetKind::Edge => CLAIM_EDGE_TOPIC,
    }
}

pub fn release_topic(kind: ClaimTargetKind) -> &'static str {
    match kind {
        ClaimTargetKind::Zone => RELEASE_ZONE_TOPIC,
        ClaimTargetKind::Node => RELEASE_NODE_TOPIC,
        ClaimTargetKind::Edge => RELEASE_EDGE_TOPIC,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Coordinator;

    fn chunk(transfer: u64, index: u32, total: u32, body: &[u8]) -> WorkspaceChunk {
        WorkspaceChunk {
            transfer,
            index,
            total,
            body: body.to_vec(),
            key: b"admin".to_vec(),
        }
    }

    #[test]
    fn a_single_chunk_push_completes_immediately() {
        let mut assembler = WorkspaceAssembler::default();

        let done = assembler.accept(chunk(1, 0, 1, b"{}")).expect("accepted");

        assert_eq!(
            done.map(|(body, _)| body).as_deref(),
            Some(b"{}".as_slice())
        );
    }

    #[test]
    fn a_split_document_is_reassembled_in_order() {
        let mut assembler = WorkspaceAssembler::default();

        assert!(assembler.accept(chunk(1, 0, 3, b"abc")).unwrap().is_none());
        assert!(assembler.accept(chunk(1, 1, 3, b"def")).unwrap().is_none());
        let done = assembler.accept(chunk(1, 2, 3, b"ghi")).unwrap();

        assert_eq!(
            done.map(|(body, _)| body).as_deref(),
            Some(b"abcdefghi".as_slice())
        );
    }

    /// Nothing promises chunks arrive in order, so the index decides where a
    /// slice belongs, not its arrival time.
    #[test]
    fn chunks_arriving_out_of_order_still_reassemble_correctly() {
        let mut assembler = WorkspaceAssembler::default();

        assert!(assembler.accept(chunk(1, 2, 3, b"ghi")).unwrap().is_none());
        assert!(assembler.accept(chunk(1, 0, 3, b"abc")).unwrap().is_none());
        let done = assembler.accept(chunk(1, 1, 3, b"def")).unwrap();

        assert_eq!(
            done.map(|(body, _)| body).as_deref(),
            Some(b"abcdefghi".as_slice())
        );
    }

    /// The whole point of the transfer id: two clients pushing at once must
    /// not splice into one corrupt document.
    #[test]
    fn concurrent_transfers_do_not_interleave() {
        let mut assembler = WorkspaceAssembler::default();

        assert!(assembler.accept(chunk(1, 0, 2, b"aa")).unwrap().is_none());
        assert!(assembler.accept(chunk(2, 0, 2, b"xx")).unwrap().is_none());
        let first = assembler.accept(chunk(1, 1, 2, b"bb")).unwrap();
        let second = assembler.accept(chunk(2, 1, 2, b"yy")).unwrap();

        assert_eq!(
            first.map(|(body, _)| body).as_deref(),
            Some(b"aabb".as_slice())
        );
        assert_eq!(
            second.map(|(body, _)| body).as_deref(),
            Some(b"xxyy".as_slice())
        );
    }

    #[test]
    fn an_out_of_range_index_is_rejected() {
        let mut assembler = WorkspaceAssembler::default();

        assert!(assembler.accept(chunk(1, 3, 3, b"x")).is_err());
        assert!(assembler.accept(chunk(1, 0, 0, b"x")).is_err());
    }

    #[test]
    fn a_transfer_that_changes_its_chunk_count_is_rejected() {
        let mut assembler = WorkspaceAssembler::default();
        assembler.accept(chunk(1, 0, 3, b"abc")).expect("first");

        assert!(assembler.accept(chunk(1, 1, 9, b"def")).is_err());
        // The bad transfer is dropped rather than left half-built.
        assert!(assembler.transfers.is_empty());
    }

    #[test]
    fn an_abandoned_transfer_cannot_pin_memory_forever() {
        let mut assembler = WorkspaceAssembler::default();
        let huge = vec![0u8; 1024];
        let total = (MAX_WORKSPACE_BYTES / huge.len()) as u32 + 2;

        let mut result = Ok(None);
        for index in 0..total {
            result = assembler.accept(chunk(1, index, total, &huge));
            if result.is_err() {
                break;
            }
        }

        assert!(result.is_err(), "should refuse past the ceiling");
        assert!(assembler.transfers.is_empty(), "and drop what it buffered");
    }

    /// Header byte counts for every canonical datapod, measured with empty
    /// payload sections. Out-of-process adapters pack these headers by hand
    /// (see `examples/python_adapter/adapter.py`), and datapod identity is
    /// size+alignment hashed, so a field added here silently rejects every
    /// such adapter until it is updated. Keep this in step with the schema
    /// table in `docs/WRITING_ADAPTER.md`.
    #[test]
    fn canonical_header_sizes_are_frozen() {
        let sizes = [
            (
                "ares.v1.register",
                DatapodMsg::from_datapod(&Register {
                    alive: 0,
                    has_alive: 0,
                    _pad: [0; 7],
                    robot: Vec::new(),
                    key: Vec::new(),
                })
                .wire()
                .len(),
                32,
            ),
            (
                "ares.v2.heartbeat",
                DatapodMsg::from_datapod(&Heartbeat {
                    zone: 0,
                    node: 0,
                    edge: 0,
                    pos_a: 0.0,
                    pos_b: 0.0,
                    pos_c: 0.0,
                    roll: 0.0,
                    pitch: 0.0,
                    yaw: 0.0,
                    has_zone: 0,
                    has_node: 0,
                    has_edge: 0,
                    pos_frame: 0,
                    has_roll: 0,
                    has_pitch: 0,
                    has_yaw: 0,
                    _pad: [0; 1],
                    robot: Vec::new(),
                    key: Vec::new(),
                })
                .wire()
                .len(),
                96,
            ),
            (
                "ares.v1.claim",
                DatapodMsg::from_datapod(&Claim {
                    lease_time: 0,
                    access_mode: 0,
                    has_access_mode: 0,
                    has_lease_time: 0,
                    _pad: [0; 5],
                    robot: Vec::new(),
                    key: Vec::new(),
                    id: Vec::new(),
                })
                .wire()
                .len(),
                40,
            ),
            (
                "ares.v1.claim.route",
                DatapodMsg::from_datapod(&ClaimRoute {
                    lease_time: 0,
                    access_mode: 0,
                    has_access_mode: 0,
                    has_lease_time: 0,
                    _pad: [0; 5],
                    robot: Vec::new(),
                    key: Vec::new(),
                    node: Vec::new(),
                    edge: Vec::new(),
                })
                .wire()
                .len(),
                48,
            ),
            (
                "ares.v1.release",
                DatapodMsg::from_datapod(&Release {
                    id: 0,
                    robot: Vec::new(),
                    key: Vec::new(),
                })
                .wire()
                .len(),
                24,
            ),
            (
                "ares.v1.workspace.chunk",
                DatapodMsg::from_datapod(&WorkspaceChunk {
                    transfer: 0,
                    index: 0,
                    total: 0,
                    body: Vec::new(),
                    key: Vec::new(),
                })
                .wire()
                .len(),
                32,
            ),
            (
                "ares.v1.reply",
                DatapodMsg::from_datapod(&Reply {
                    blocked: 0,
                    decision: 0,
                    reason: 0,
                    has_blocked: 0,
                    _pad: [0; 5],
                })
                .wire()
                .len(),
                16,
            ),
        ];
        for (name, actual, expected) in sizes {
            assert_eq!(
                actual, expected,
                "{name} header is {actual} bytes, not {expected}; \
                 update examples/python_adapter/adapter.py and docs/WRITING_ADAPTER.md"
            );
        }
    }

    /// Exact encoded bytes and type hash of one fully-populated instance of
    /// every canonical datapod.
    ///
    /// `canonical_header_sizes_are_frozen` catches a field being *added*. It
    /// cannot catch two same-size fields being *swapped*: size and alignment
    /// are unchanged, so the type hash still matches and every message decodes
    /// silently into the wrong fields — a claim on the wrong zone, which is
    /// worse than a rejection. These bytes catch it.
    ///
    /// Regenerate ONLY alongside a deliberate schema revision, with a new
    /// canonical name per the compatibility rules in
    /// `docs/WRITING_ADAPTER.md`:
    ///
    /// ```text
    /// cargo test --features peerbus --lib print_golden_wire_bytes -- --ignored --nocapture
    /// ```
    #[test]
    fn canonical_wire_bytes_are_frozen() {
        let expected: &[(&str, u64, &str)] = &[
            (
                "ares.v1.register",
                4489651317633298084,
                "8877665544332211010000000000000000000000070000000700000002000000726f626f742d376b31",
            ),
            (
                "ares.v2.heartbeat",
                4052155222172215506,
                "fdffffffffffffff080706050403020118171615141312110000000000204a400000000000001640000000000000f43f7b14ae47e17a943f9a9999999999b9bf000000000000e8bf010100010101010000000000070000000700000002000000726f626f742d376b31",
            ),
            (
                "ares.v1.claim",
                8721409874987841883,
                "1e000000000000000201010000000000000000000700000007000000020000000900000010000000726f626f742d376b312a000000000000002b00000000000000",
            ),
            (
                "ares.v1.claim.route",
                10315010630888837044,
                "1e0000000000000001010100000000000000000007000000070000000200000009000000100000001900000008000000726f626f742d376b31e903000000000000ea03000000000000d107000000000000",
            ),
            (
                "ares.v1.release",
                12275913315249212202,
                "282726252423222100000000070000000700000002000000726f626f742d376b31",
            ),
            (
                "ares.v1.reply",
                15837292231368537539,
                "2b000000000000000002010000000000",
            ),
            (
                "ares.v1.route.plan",
                12486718087882156207,
                "0100000000000000000000000400000004000000040000003130303131303033",
            ),
            (
                "ares.v1.resource.get",
                7228468034526520903,
                "0000000003000000323035",
            ),
            (
                "ares.v1.workspace.chunk",
                16336082357629830881,
                "38373635343332310100000003000000000000000200000002000000050000007b7d61646d696e",
            ),
        ];

        let actual = golden_fixtures();
        assert_eq!(
            actual.len(),
            expected.len(),
            "a canonical datapod was added or removed without updating the goldens"
        );
        for ((name, message), (expected_name, expected_hash, expected_hex)) in
            actual.iter().zip(expected)
        {
            assert_eq!(name, expected_name, "golden fixtures are out of order");
            assert_eq!(
                message.type_hash(),
                *expected_hash,
                "{name}: type identity changed — adapters compiled against the old \
                 schema will have their requests rejected"
            );
            let hex: String = message.wire().iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(
                hex, *expected_hex,
                "{name}: encoding changed. If two same-size fields were swapped the \
                 type hash would NOT have caught it and every adapter would decode \
                 into the wrong fields. Give the revised schema a new canonical name."
            );
        }
    }

    /// Print goldens for `canonical_wire_bytes_are_frozen`. Run with
    /// `--ignored --nocapture` after a deliberate schema change.
    #[test]
    #[ignore]
    fn print_golden_wire_bytes() {
        for (name, message) in golden_fixtures() {
            let hex: String = message.wire().iter().map(|b| format!("{b:02x}")).collect();
            println!("(\"{name}\", {}, \"{hex}\"),", message.type_hash());
        }
    }

    /// One fully-populated instance of every canonical datapod, with
    /// distinctive field values so a reorder shows up as different bytes.
    fn golden_fixtures() -> Vec<(&'static str, DatapodMsg)> {
        vec![
            (
                "ares.v1.register",
                DatapodMsg::from_datapod(&Register {
                    alive: 0x1122_3344_5566_7788,
                    has_alive: 1,
                    _pad: [0; 7],
                    robot: b"robot-7".to_vec(),
                    key: b"k1".to_vec(),
                }),
            ),
            (
                "ares.v2.heartbeat",
                DatapodMsg::from_datapod(&Heartbeat {
                    zone: -3,
                    node: 0x0102_0304_0506_0708,
                    edge: 0x1112_1314_1516_1718,
                    pos_a: 52.25,
                    pos_b: 5.5,
                    pos_c: 1.25,
                    roll: 0.02,
                    pitch: -0.1,
                    yaw: -0.75,
                    has_zone: 1,
                    has_node: 1,
                    has_edge: 0,
                    pos_frame: POS_FRAME_GLOBAL,
                    has_roll: 1,
                    has_pitch: 1,
                    has_yaw: 1,
                    _pad: [0; 1],
                    robot: b"robot-7".to_vec(),
                    key: b"k1".to_vec(),
                }),
            ),
            (
                "ares.v1.claim",
                DatapodMsg::from_datapod(&Claim {
                    lease_time: 30,
                    access_mode: 2,
                    has_access_mode: 1,
                    has_lease_time: 1,
                    _pad: [0; 5],
                    robot: b"robot-7".to_vec(),
                    key: b"k1".to_vec(),
                    id: vec![42, 43],
                }),
            ),
            (
                "ares.v1.claim.route",
                DatapodMsg::from_datapod(&ClaimRoute {
                    lease_time: 30,
                    access_mode: 1,
                    has_access_mode: 1,
                    has_lease_time: 1,
                    _pad: [0; 5],
                    robot: b"robot-7".to_vec(),
                    key: b"k1".to_vec(),
                    node: vec![1001, 1002],
                    edge: vec![2001],
                }),
            ),
            (
                "ares.v1.release",
                DatapodMsg::from_datapod(&Release {
                    id: 0x2122_2324_2526_2728,
                    robot: b"robot-7".to_vec(),
                    key: b"k1".to_vec(),
                }),
            ),
            (
                "ares.v1.reply",
                DatapodMsg::from_datapod(&Reply {
                    blocked: 43,
                    decision: 0,
                    reason: 2,
                    has_blocked: 1,
                    _pad: [0; 5],
                }),
            ),
            (
                "ares.v1.route.plan",
                DatapodMsg::from_datapod(&RoutePlanRequest {
                    use_penalties: 1,
                    _pad: [0; 7],
                    start: b"1001".to_vec(),
                    goal: b"1003".to_vec(),
                }),
            ),
            (
                "ares.v1.resource.get",
                DatapodMsg::from_datapod(&ResourceGet {
                    id: b"205".to_vec(),
                }),
            ),
            (
                "ares.v1.workspace.chunk",
                DatapodMsg::from_datapod(&WorkspaceChunk {
                    transfer: 0x3132_3334_3536_3738,
                    index: 1,
                    total: 3,
                    body: b"{}".to_vec(),
                    key: b"admin".to_vec(),
                }),
            ),
        ]
    }

    fn unique_identity() -> String {
        format!(
            "ares-core-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        )
    }

    #[test]
    fn register_round_trips_through_peerbus_core() {
        let identity = unique_identity();
        let _core = CoreService::with_identity(ServeState::new(Coordinator::new()), &identity)
            .expect("start core");
        let client = Client::connect(identity).expect("connect adapter");

        let first = client.register("7", "1234", Some(2)).expect("register");
        assert_eq!((first.decision, first.reason), (1, 0));
        let duplicate = client.register("7", "1234", None).expect("duplicate");
        assert_eq!((duplicate.decision, duplicate.reason), (0, 2));
    }
}
