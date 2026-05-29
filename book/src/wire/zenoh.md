# Zenoh

[Zenoh](https://zenoh.io) queryables and pub/sub for robot-to-core, bridges, and
wireless / edge deployments. Zenoh handles discovery, reconnect, and multicast
natively, which fits on-vehicle clients better than HTTP.

- **Feature:** `robo` (robotics transport)
- **Module:** `timenav::wire::robo`
- **Key prefix:** `ares/v1`
- **Payload:** JSON

## Enable and serve

```toml
[dependencies]
timenav = { version = "0.0.1", features = ["robo"] }
```

```rust
use timenav::wire::{ServeState, robo};

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

## Envelopes for per-robot calls

Where the REST path puts the robot id in the URL (`/robots/{id}/heartbeat`), the
Zenoh key is flat (`ares/v1/robots/heartbeat`), so the robot id travels **in the
body** via a small envelope:

```json
{
  "robot_id": 1,
  "heartbeat": {
    "current_node_id": "139",
    "current_edge_id": null,
    "updated_at_tick": 42
  }
}
```

The envelopes are `RobotHeartbeatEnvelope`, `RobotAssignRouteEnvelope`, and
`RobotScheduleEnvelope`, wrapping the same `HeartbeatRequest` /
`AssignRouteRequest` / `ScheduleRobotRouteRequest` the REST handlers use.

## Worked example (concept)

```text
query  ares/v1/claims/evaluate
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
