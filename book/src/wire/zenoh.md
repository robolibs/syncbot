# Zenoh

[Zenoh](https://zenoh.io) queryables and pub/sub for robot-to-core, bridges, and
wireless / edge deployments. Zenoh handles discovery, reconnect, and multicast
natively, which fits on-vehicle clients better than HTTP.

- **Feature:** `robo` (robotics transport)
- **Module:** `syncbot::wire::robo`
- **Key prefix:** `ares/v1`
- **Payload:** JSON

## Enable and serve

```toml
[dependencies]
syncbot = { version = "0.1.0", features = ["robo"] }
```

```rust
use syncbot::wire::{ServeState, robo};

// `session` is an open `zenoh::Session` you own.
let handle = robo::serve(&session, state).await?;   // installs queryables
// Dropping `handle` aborts the queryable tasks.
```

`robo::serve` declares one queryable per capability and spawns a task pumping each.
The returned `ZenohServeHandle` owns those tasks; drop it to tear them down.
`robo::key_expr("claims/evaluate")` builds a normalised key under the prefix.

## Queryables

```text
ares/v1/health
ares/v1/fleet/snapshot

ares/v1/routes/plan

ares/v1/robots/register
ares/v1/robots/list
ares/v1/robots/heartbeat
ares/v1/robots/assign_route
ares/v1/robots/schedule

ares/v1/claims/list
ares/v1/claims/evaluate
ares/v1/claims/request          submit (evaluate, then store if granted)

ares/v1/leases/list
ares/v1/leases/add
ares/v1/leases/release
```

A queryable that needs a body expects a JSON payload; query it with the payload
attached. The reply is JSON (or a Zenoh error reply carrying `ApiError`).

## Flat (tier-1) services

The four robot flows are flat and key-authenticated, same contract as REST —
the resource *type* is a key-expression segment, the robot id travels in the
body (no URL here), and the reply is `decision`/`reason`:

```text
query  ares/v1/robots/register     {"robot":"7","key":"1234"}     -> {"decision":1,"reason":0}
query  ares/v1/robots/heartbeat    {"robot":"7","key":"1234","node":"139"}
query  ares/v1/claims/zone         {"key":"1234","robot":"7","id":[42,43]}
query  ares/v1/leases/release/zone {"key":"1234","robot":"7","id":42}
```

## Envelopes for fine-level per-robot calls

The tier-2 per-robot endpoints (`assign_route`, `schedule`) put the robot id
**in the body** via a small envelope, and carry the `key` on the inner
request (they are state-changing):

```json
{
  "robot_id": 1,
  "assignment": { "route_plan": { /* ... */ }, "horizon": 100, "updated_at_tick": 10, "key": "1234" }
}
```

The envelopes are `RobotAssignRouteEnvelope` and `RobotScheduleEnvelope`,
wrapping `AssignRouteRequest` / `ScheduleRobotRouteRequest`.

## Worked example (concept)

```text
query  ares/v1/claims/evaluate   (read-only, open — no key)
       payload: {"id":1,"robot_id":1,"access_mode":"Exclusive",
                 "targets":[{"kind":"Zone","resource_id":"205"}]}
reply  {"decision":"Grant", ...}
```

`resource_id` accepts numeric alias or UUID, same as everywhere — see
[Resource IDs](../resource-ids.md).

## Publishing fleet state

Beyond the request/response queryables, the adapter can push a snapshot:

```rust
robo::publish_fleet_state(&session, &state).await?;   // -> ares/v1/fleet/state
```

The intended event streams (publisher side) mirror the names sketched for the
fleet:

```text
ares/v1/fleet/state
ares/v1/events/lease
ares/v1/events/zone
ares/v1/events/alert
```
