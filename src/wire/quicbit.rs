//! quicbit transport adapter — publish fleet events and state over
//! [`quicbit`](https://codeberg.org/robolibs/quicbit).
//!
//! Enabled with `--features quicbit`.
//!
//! quicbit chooses its substrate per subscriber: same-host subscribers
//! attach over iceoryx2 shared memory (zero copy); off-host subscribers
//! dial in over iroh QUIC. This adapter does not pick — it just publishes.
//! The topic names mirror the Zenoh streams in `PRESENTATION.md`.
//!
//! Two payload shapes:
//!
//! * **POD events** ([`LeaseEvent`], [`ScheduleEvent`]) — flat fixed-size
//!   structs carrying numeric IDs. Ride entirely in the iceoryx2
//!   user-header / iroh frame prefix, no serialisation.
//! * **Bytes snapshot** ([`FleetStateMsg`]) — a fixed header plus a
//!   `#[dp(bytes)]` blob holding a JSON-encoded [`FleetSnapshot`], for
//!   consumers that want the whole picture rather than a delta.

use quicbit::{Node, Publisher};

use crate::claim::{Lease, LeaseDisposition};
use crate::coordinator::{ScheduleDecision, ScheduleDecisionKind};
use crate::core::ids::RobotId;
use crate::wire::FleetSnapshot;

/// Topic for lease lifecycle events.
pub const TOPIC_LEASE: &str = "ares/v1/events/lease";
/// Topic for schedule decisions.
pub const TOPIC_SCHEDULE: &str = "ares/v1/events/schedule";
/// Topic for full fleet-state snapshots.
pub const TOPIC_FLEET_STATE: &str = "ares/v1/fleet/state";

/// Lease lifecycle kind, flattened to a `u8` for the POD wire form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LeaseEventKind {
    Granted = 0,
    Released = 1,
    Expired = 2,
    Revoked = 3,
}

impl LeaseEventKind {
    /// Map a lease's current disposition to an event kind.
    pub fn from_disposition(d: LeaseDisposition) -> Self {
        match d {
            LeaseDisposition::Active => Self::Granted,
            LeaseDisposition::Released => Self::Released,
            LeaseDisposition::Expired => Self::Expired,
            LeaseDisposition::Revoked => Self::Revoked,
        }
    }
}

/// Flat, fixed-size lease event. All scalar — true zero-copy on SHM.
///
/// Every field is `u64` so the generated header is padding-free, which
/// `bytemuck::Pod` (required by `datapod::DataPod::Header`) demands.
///
/// `zone_numeric_id` is the first target's `external.numeric_id` when the
/// caller resolved one, else `0`. The full target set lives in the core
/// `Lease`; this event is a notification, not a replacement for it.
#[datapod::datapod]
pub struct LeaseEvent {
    /// One of [`LeaseEventKind`] as `u64`.
    pub kind: u64,
    pub robot_id: u64,
    pub claim_id: u64,
    pub lease_id: u64,
    /// Numeric alias of the first target zone, or `0` if none.
    pub zone_numeric_id: u64,
    pub tick: u64,
}

/// Flat, fixed-size schedule decision event. All `u64` — padding-free.
#[datapod::datapod]
pub struct ScheduleEvent {
    pub robot_id: u64,
    pub claim_id: u64,
    /// 0 = Proceed, 1 = Queue, 2 = Replan.
    pub decision: u64,
    pub start_tick: u64,
    pub queue_position: u64,
    pub conflict_count: u64,
    pub tick: u64,
}

/// Full fleet snapshot. Fixed (padding-free, all-`u64`) header plus a JSON
/// blob in the `#[dp(bytes)]` payload. Decode with [`decode_fleet_state`].
#[datapod::datapod]
pub struct FleetStateMsg {
    pub tick: u64,
    pub robot_count: u64,
    pub request_count: u64,
    pub lease_count: u64,
    #[dp(bytes)]
    pub payload: Vec<u8>,
}

fn decision_code(kind: ScheduleDecisionKind) -> u64 {
    match kind {
        ScheduleDecisionKind::Proceed => 0,
        ScheduleDecisionKind::Queue => 1,
        ScheduleDecisionKind::Replan => 2,
    }
}

/// Publishes timenav fleet events and state over quicbit.
///
/// Owns a quicbit [`Node`] and one publisher per topic. Construct once,
/// then call the `publish_*` methods as the coordinator's state changes.
pub struct FleetPublisher {
    _node: Node,
    lease: Publisher<LeaseEvent>,
    schedule: Publisher<ScheduleEvent>,
    fleet: Publisher<FleetStateMsg>,
}

impl FleetPublisher {
    /// Build a publisher identified by `identity` (a stable name; on a
    /// trusted LAN it hashes to a key, see quicbit's identity docs).
    /// `no_relay` keeps discovery on the local network only.
    pub fn new(identity: &str) -> quicbit::Result<Self> {
        let node = Node::builder().identity(identity).no_relay().bind()?;
        let lease = node.publisher::<LeaseEvent>(TOPIC_LEASE)?;
        let schedule = node.publisher::<ScheduleEvent>(TOPIC_SCHEDULE)?;
        let fleet = node.publisher::<FleetStateMsg>(TOPIC_FLEET_STATE)?;
        Ok(Self {
            _node: node,
            lease,
            schedule,
            fleet,
        })
    }

    /// Publish a lease lifecycle event. `zone_numeric_id` is the resolved
    /// numeric alias of the relevant zone, or `0` if not applicable.
    pub fn publish_lease(
        &mut self,
        lease: &Lease,
        kind: LeaseEventKind,
        zone_numeric_id: u64,
        tick: u64,
    ) -> quicbit::Result<()> {
        self.lease.send(&LeaseEvent {
            kind: kind as u64,
            robot_id: lease.robot_id.raw(),
            claim_id: lease.claim_id.raw(),
            lease_id: lease.id.raw(),
            zone_numeric_id,
            tick,
        })?;
        Ok(())
    }

    /// Publish a schedule decision for a robot.
    pub fn publish_schedule(
        &mut self,
        robot_id: RobotId,
        claim_id: u64,
        decision: &ScheduleDecision,
        tick: u64,
    ) -> quicbit::Result<()> {
        self.schedule.send(&ScheduleEvent {
            robot_id: robot_id.raw(),
            claim_id,
            decision: decision_code(decision.kind),
            start_tick: decision.start_tick,
            queue_position: decision.queue_position,
            conflict_count: decision.conflicts.len() as u64,
            tick,
        })?;
        Ok(())
    }

    /// Publish a full fleet snapshot. The snapshot is JSON-encoded into
    /// the variable-length payload; counts are mirrored in the header so
    /// subscribers can triage without decoding the blob.
    pub fn publish_fleet_state(
        &mut self,
        snapshot: &FleetSnapshot,
        tick: u64,
    ) -> quicbit::Result<()> {
        let payload = serde_json::to_vec(snapshot)
            .map_err(|e| quicbit::Error::invalid_argument(format!("encode snapshot: {e}")))?;
        self.fleet.send(&FleetStateMsg {
            tick,
            robot_count: snapshot.robots.len() as u64,
            request_count: snapshot.requests.len() as u64,
            lease_count: snapshot.leases.len() as u64,
            payload,
        })?;
        Ok(())
    }
}

/// Decode a [`FleetStateMsg`] payload back into a [`FleetSnapshot`].
///
/// `payload` is the variable-length blob from a received sample
/// (`sample.payload()`), not the fixed header.
pub fn decode_fleet_state(payload: &[u8]) -> serde_json::Result<FleetSnapshot> {
    serde_json::from_slice(payload)
}
