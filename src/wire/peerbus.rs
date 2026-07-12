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
use crate::wire::{ApiError, FlatReply, ServeState};

pub const CORE_IDENTITY: &str = "ares-core";
const LOCAL_MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

pub const REGISTER_TOPIC: &str = "ares/v1/robots/register";
pub const HEARTBEAT_TOPIC: &str = "ares/v1/robots/heartbeat";
pub const CLAIM_ZONE_TOPIC: &str = "ares/v1/claims/zone";
pub const CLAIM_NODE_TOPIC: &str = "ares/v1/claims/node";
pub const CLAIM_EDGE_TOPIC: &str = "ares/v1/claims/edge";
pub const RELEASE_ZONE_TOPIC: &str = "ares/v1/leases/release/zone";
pub const RELEASE_NODE_TOPIC: &str = "ares/v1/leases/release/node";
pub const RELEASE_EDGE_TOPIC: &str = "ares/v1/leases/release/edge";
pub const ZONES_LIST_TOPIC: &str = "ares/v1/zones/list";
pub const ZONE_GET_TOPIC: &str = "ares/v1/zones/get";
pub const FLEET_SNAPSHOT_TOPIC: &str = "ares/v1/fleet/snapshot";

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

/// Canonical heartbeat request. Presence flags distinguish an absent value
/// from zero. A negative zone remains the flat protocol's unknown sentinel.
#[datapod::datapod(name = "ares.v1.heartbeat")]
pub struct Heartbeat {
    pub zone: i64,
    pub node: u64,
    pub edge: u64,
    pub has_zone: u8,
    pub has_node: u8,
    pub has_edge: u8,
    pub _pad: [u8; 5],
    #[dp(bytes, section = "robot")]
    pub robot: Vec<u8>,
    #[dp(bytes, section = "key")]
    pub key: Vec<u8>,
}

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
/// workspace alias, matching the public REST and ROS2 address forms.
#[datapod::datapod(name = "ares.v1.resource.get")]
pub struct ResourceGet {
    #[dp(bytes, section = "id")]
    pub id: Vec<u8>,
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
    Heartbeat,
    Claim(ClaimTargetKind),
    Release(ClaimTargetKind),
    ZonesList,
    ZoneGet,
    FleetSnapshot,
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
            (Operation::ZonesList, node.req_server(ZONES_LIST_TOPIC)?),
            (Operation::ZoneGet, node.req_server(ZONE_GET_TOPIC)?),
            (
                Operation::FleetSnapshot,
                node.req_server(FLEET_SNAPSHOT_TOPIC)?,
            ),
        ];

        let running = Arc::new(AtomicBool::new(true));
        let worker_running = Arc::clone(&running);
        let worker = thread::Builder::new()
            .name("ares-peerbus-core".into())
            .spawn(move || {
                while worker_running.load(Ordering::Acquire) {
                    let mut handled = false;
                    for (operation, server) in &mut servers {
                        match server.take() {
                            Ok(Some((request, responder))) => {
                                handled = true;
                                let message = handle_request(&state, *operation, &request);
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

fn handle_request(
    state: &ServeState,
    operation: Operation,
    request: &peerbus::ReqSample<DatapodMsg>,
) -> DatapodMsg {
    let message = DatapodMsg::new(request.type_hash(), request.wire().to_vec());
    let reply = match operation {
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
        Operation::Heartbeat => decode::<Heartbeat>(&message)
            .map(|req| {
                crate::wire::flat_heartbeat(
                    state,
                    utf8(&req.robot).unwrap_or(""),
                    key(&req.key),
                    (req.has_zone != 0).then_some(req.zone),
                    (req.has_node != 0).then_some(req.node),
                    (req.has_edge != 0).then_some(req.edge),
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
    };
    reply
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
        Operation::Heartbeat => crate::wire::reason::heartbeat::NOT_REGISTERED,
        Operation::Claim(_) => crate::wire::reason::claim::BAD_REQUEST,
        Operation::Release(_) => crate::wire::reason::release::UNKNOWN_OR_BAD,
        Operation::ZonesList | Operation::ZoneGet | Operation::FleetSnapshot => {
            crate::wire::reason::claim::BAD_REQUEST
        }
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
    requests:
        Mutex<std::collections::BTreeMap<&'static str, peerbus::ReqClient<DatapodMsg, DatapodMsg>>>,
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
            HEARTBEAT_TOPIC,
            CLAIM_ZONE_TOPIC,
            CLAIM_NODE_TOPIC,
            CLAIM_EDGE_TOPIC,
            RELEASE_ZONE_TOPIC,
            RELEASE_NODE_TOPIC,
            RELEASE_EDGE_TOPIC,
            ZONES_LIST_TOPIC,
            ZONE_GET_TOPIC,
            FLEET_SNAPSHOT_TOPIC,
        ] {
            requests.insert(
                topic,
                node.req_client::<DatapodMsg, DatapodMsg>(core_peer.as_str(), topic)?,
            );
        }
        Ok(Self {
            inner: Arc::new(ClientInner {
                _node: node,
                requests: Mutex::new(requests),
            }),
        })
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

    pub fn heartbeat(
        &self,
        robot: &str,
        key: &str,
        zone: Option<i64>,
        node: Option<u64>,
        edge: Option<u64>,
    ) -> Result<FlatReply, ApiError> {
        self.call(
            HEARTBEAT_TOPIC,
            &Heartbeat {
                zone: zone.unwrap_or_default(),
                node: node.unwrap_or_default(),
                edge: edge.unwrap_or_default(),
                has_zone: u8::from(zone.is_some()),
                has_node: u8::from(node.is_some()),
                has_edge: u8::from(edge.is_some()),
                _pad: [0; 5],
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
        let mut requests = self
            .inner
            .requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let client = requests
            .get_mut(topic)
            .ok_or_else(|| ApiError::new(format!("unknown canonical peerbus topic {topic}")))?;
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
