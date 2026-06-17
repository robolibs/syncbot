# REST / JSON

HTTP over [axum](https://github.com/tokio-rs/axum), JSON bodies. The default door
for dashboards, tooling, integration tests, and any non-robot client.

- **Feature:** `rest`
- **Module:** `syncbot::wire::rest`
- **Prefix:** `/ares/v1`
- **Content type:** `application/json`

## Enable and serve

```toml
[dependencies]
syncbot = { version = "0.0.2", features = ["rest"] }
```

```rust
use std::sync::Arc;
use syncbot::wire::{ServeState, rest};
use syncbot::{Coordinator, WorkspaceIndex};

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
POST    /ares/v1/robots                       register (flat: robot + key)
GET     /ares/v1/robots/{id}
DELETE  /ares/v1/robots/{id}                   unregister
POST    /ares/v1/robots/{id}/heartbeat         flat: key + zone/node/edge
POST    /ares/v1/robots/{id}/route             assign route plan (+ key)
POST    /ares/v1/robots/{id}/schedule          (+ key)

GET     /ares/v1/claims
POST    /ares/v1/claims                        submit nested (+ key)
POST    /ares/v1/claims/{zone,node,edge}       flat claim: key + robot + id(s)
POST    /ares/v1/claims/evaluate               evaluate only (read-only, open)

GET     /ares/v1/leases
POST    /ares/v1/leases                        add a lease
POST    /ares/v1/leases/release                release by lease id
POST    /ares/v1/leases/release/{zone,node,edge}  flat release: key + robot + id
DELETE  /ares/v1/leases/{id}                   release by path
```

`{id}` is the numeric robot/lease id (a `u64`).

## Authentication & the flat protocol

Read-only endpoints (health, snapshot, lists, route planning, claim
*evaluate*) are open. **State-changing** endpoints require a `key` — an integer
password or `did:pass=<secret>` — validated against the acting robot.

The four robot flows have a **flat** form for simple controllers: type in the
path, scalar body, `decision` (1/0) + `reason` (enum) reply. `0` = OK and `1` =
mismatched key on every flat reply.

```sh
# register (flat)
curl -X POST .../ares/v1/robots -d '{"robot":"7","key":"1234"}'
# {"decision":1,"reason":0}

# claim one or more zones atomically (type in path)
curl -X POST .../ares/v1/claims/zone -d '{"key":"1234","robot":"7","id":[42,43]}'

# heartbeat (liveness + position; ack only)
curl -X POST .../ares/v1/robots/7/heartbeat -d '{"key":"1234","zone":42}'

# release
curl -X POST .../ares/v1/leases/release/zone -d '{"key":"1234","robot":"7","id":42}'
```

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

The nested tier-2 bodies (`ClaimRequestWire`, `Lease`) deserialise with serde
defaults, so a fine-level client only sends what it cares about (plus the `key`
on state-changing calls). The flat tier-1 bodies are already minimal — see the
flat protocol above.

## Errors

Handlers return `ApiError { message }` with HTTP `400` on bad input (unknown
resource id, missing workspace index, lock poisoned). The body is JSON:

```json
{ "message": "unknown Zone resource id Numeric(9999)" }
```
