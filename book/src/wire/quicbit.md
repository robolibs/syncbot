# quicbit

[quicbit](https://codeberg.org/robolibs/quicbit) is a typed zero-copy messaging
layer: **iceoryx2 shared memory** when both ends are on the same host, **iroh
QUIC** when they aren't. The substrate is chosen per subscriber, not per call —
the publishing code is identical either way.

timenav uses it to **publish** fleet events and snapshots. Unlike REST/Zenoh,
this is not a control API — it's an outbound event bus for dashboards, loggers,
and other robots.

- **Feature:** `quicbit`
- **Module:** `timenav::wire::quicbit`
- **Topics:** `ares/v1/events/lease`, `ares/v1/events/schedule`, `ares/v1/fleet/state`

## Two payload shapes

| Shape          | Types                          | Wire form                          | Zero-copy |
|----------------|--------------------------------|------------------------------------|-----------|
| **POD events** | `LeaseEvent`, `ScheduleEvent`  | flat fixed-size struct (all `u64`) | yes, on SHM |
| **Snapshot**   | `FleetStateMsg`                | fixed header + JSON blob payload   | no (serialises) |

POD events ride entirely in the iceoryx2 user-header / iroh frame prefix — no
serialisation. They carry **numeric IDs** (see [Resource IDs](../resource-ids.md)),
which is exactly why they stay small and fixed-size. The snapshot wraps a
JSON-encoded `FleetSnapshot` in a `#[dp(bytes)]` payload for consumers that want
the whole picture.

> **Why all-`u64`?** A datapod POD header must be padding-free (`bytemuck::Pod`).
> Mixing `u8`/`u32` with `u64` inserts padding and is rejected, so every event
> field is a `u64`.

## Enable and publish

```toml
[dependencies]
timenav = { version = "0.0.1", features = ["quicbit"] }
```

```rust
use timenav::wire::quicbit::{FleetPublisher, LeaseEventKind};

// Identity is a stable name; on a trusted LAN it hashes to a key.
let mut fleet = FleetPublisher::new("timenav-core")?;

// As leases change:
fleet.publish_lease(&lease, LeaseEventKind::Granted, /* zone_numeric_id */ 205, tick)?;
fleet.publish_lease(&lease, LeaseEventKind::Released, 205, tick)?;

// On a schedule decision:
fleet.publish_schedule(robot_id, claim_id, &decision, tick)?;

// Periodic full snapshot:
fleet.publish_fleet_state(&snapshot, tick)?;
```

`FleetPublisher` owns a quicbit `Node` and one publisher per topic. Construct once,
call as state changes.

> A runnable publisher + subscriber in one process is in
> `examples/quicbit_fleet.rs` —
> `cargo run --example quicbit_fleet --features quicbit`.

## Subscribing

A consumer is a quicbit `Node` subscribing to the publisher's identity + topic.
Same-host → it attaches over shared memory; remote → it dials over QUIC.

```rust
use quicbit::Node;
use timenav::wire::quicbit::{LeaseEvent, TOPIC_LEASE};

let node = Node::builder().identity("dashboard").no_relay().bind()?;
let mut sub = node.subscriber::<LeaseEvent>("timenav-core", TOPIC_LEASE)?;

while let Some(sample) = sub.take()? {
    let e = sample.header();          // &LeaseEvent
    println!("robot={} lease={} zone#{} kind={}",
             e.robot_id, e.lease_id, e.zone_numeric_id, e.kind);
}
```

For the snapshot topic, read the header then decode the payload:

```rust
use timenav::wire::quicbit::{FleetStateMsg, decode_fleet_state, TOPIC_FLEET_STATE};

let mut sub = node.subscriber::<FleetStateMsg>("timenav-core", TOPIC_FLEET_STATE)?;
if let Some(sample) = sub.take()? {
    let snapshot = decode_fleet_state(sample.payload())?;   // FleetSnapshot
}
```

## Event reference

`LeaseEvent` — `kind` is a `LeaseEventKind` as `u64`:

| `kind` | meaning   |
|--------|-----------|
| 0      | Granted   |
| 1      | Released  |
| 2      | Expired   |
| 3      | Revoked   |

Fields: `kind, robot_id, claim_id, lease_id, zone_numeric_id, tick`
(`zone_numeric_id` is `0` when no numeric alias applies).

`ScheduleEvent` — `decision` is `0=Proceed, 1=Queue, 2=Replan`. Fields:
`robot_id, claim_id, decision, start_tick, queue_position, conflict_count, tick`.

`FleetStateMsg` — header `tick, robot_count, request_count, lease_count` + a
`#[dp(bytes)]` JSON payload of the full `FleetSnapshot`.

## Dependency note

quicbit pulls iceoryx2 (needs `libclang` for bindgen) and iroh, both behind the
`quicbit` feature — the default build is untouched. Payload types are
`#[datapod::datapod]`; thanks to datapod re-exporting its marker traits, timenav
does **not** need a direct `bytemuck` dependency.
