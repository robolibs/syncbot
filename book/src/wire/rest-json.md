# REST / JSON

HTTP over [axum](https://github.com/tokio-rs/axum), JSON bodies. The default door
for dashboards, tooling, integration tests, and any non-robot client.

- **Feature:** `rest`
- **Module:** `timenav::wire::rest`
- **Prefix:** `/ares/v1`
- **Content type:** `application/json`

## Enable and serve

```toml
[dependencies]
timenav = { version = "0.0.1", features = ["rest"] }
```

```rust
use std::sync::Arc;
use timenav::wire::{ServeState, rest};
use timenav::{Coordinator, WorkspaceIndex};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let index = Arc::new(WorkspaceIndex::from_workspace(/* … */));
    let state = ServeState::new(Coordinator::with_index(index));

    let app = rest::router(state);                 // axum::Router
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

`rest::router(state)` returns a plain `axum::Router`, so you can nest it, add
middleware, or run it next to your own routes.

> A runnable server with a 3-zone workspace lives in
> `examples/rest_server.rs` — `cargo run --example rest_server --features rest`.

## Endpoints

```text
GET     /ares/v1/health
GET     /ares/v1/fleet/snapshot

POST    /ares/v1/routes/plan

GET     /ares/v1/robots
POST    /ares/v1/robots                       register
GET     /ares/v1/robots/{id}
DELETE  /ares/v1/robots/{id}                   unregister
POST    /ares/v1/robots/{id}/heartbeat
POST    /ares/v1/robots/{id}/route             assign route plan
POST    /ares/v1/robots/{id}/schedule

GET     /ares/v1/claims
POST    /ares/v1/claims                        submit (evaluate, then store if granted)
POST    /ares/v1/claims/evaluate               evaluate only (read-only)

GET     /ares/v1/leases
POST    /ares/v1/leases                        add a lease
POST    /ares/v1/leases/release                release by body
DELETE  /ares/v1/leases/{id}                   release by path
```

`{id}` is the numeric robot/lease id (a `u64`).

## Worked example: claim a zone

`claims/evaluate` is read-only — safe to poll. Submit/store with `POST /claims`.

```sh
curl -X POST http://127.0.0.1:8080/ares/v1/claims/evaluate \
     -H "Content-Type: application/json" \
     -d '{
       "id": 1,
       "robot_id": 1,
       "access_mode": "Exclusive",
       "targets": [{ "kind": "Zone", "resource_id": "205" }]
     }'
```

```json
{
  "decision": "Grant",
  "reason": "claim is compatible with current state",
  "conflicting_claim_id": null,
  "conflicting_lease_id": null,
  "conflicting_targets": [],
  "blocking_target": null,
  "diagnostics": ["request passed conflict and capacity checks"]
}
```

On conflict you get `"decision": "Deny"`, a `reason`, the `conflicting_*` ids, and
the `blocking_target`.

`resource_id` accepts a numeric alias (`"205"`) or a UUID string — see
[Resource IDs](../resource-ids.md). Numeric IDs are **quoted**.

## Minimal-field requests

`RobotState`, `Lease`, and `ClaimRequestWire` all deserialise with serde defaults,
so a client only sends what it cares about:

```sh
# register a robot
curl -X POST .../ares/v1/robots -H 'Content-Type: application/json' \
     -d '{"robot_id": 1}'
```

## Errors

Handlers return `ApiError { message }` with HTTP `400` on bad input (unknown
resource id, missing workspace index, lock poisoned). The body is JSON:

```json
{ "message": "unknown Zone resource id Numeric(9999)" }
```
