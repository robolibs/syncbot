# Wire transports — overview

A transport is a thin adapter: parse a request, resolve resource IDs, call the
core, serialise the result. All transports live under `timenav::wire` and share
one piece of state.

## Shared state: `ServeState`

Every adapter is constructed from a `ServeState`, which wraps an
`Arc<RwLock<Coordinator>>`:

```rust
use timenav::wire::ServeState;
let state = ServeState::new(coordinator);          // owns it
let state = ServeState::shared(arc_rwlock_coord);  // shares an existing one
```

Mount more than one transport on the same `ServeState` and they operate on the
same fleet — a claim granted over REST is visible over Zenoh immediately.

## Pick a transport

| Transport     | Feature   | Encoding   | Substrate         | Use for                              |
|---------------|-----------|------------|-------------------|--------------------------------------|
| [REST / JSON](./rest-json.md) | `rest`   | JSON   | HTTP (axum)        | dashboards, tools, tests, web clients |
| [REST / XML](./rest-xml.md)   | `xmlt`   | XML    | HTTP (axum)        | PLCs, XML-native / legacy controllers |
| [Zenoh](./zenoh.md)           | `robo`   | JSON   | Zenoh queryables   | robots, bridges, wireless / edge      |

All three are **request/response** over the same API surface.

## Common address space

REST and Zenoh share the same paths under a version prefix:

```text
/ares/v1/...     REST   (HTTP path)
ares/v1/...      Zenoh  (key expression)
```

The capability set is identical; only the wrapping differs.

| Capability       | REST (JSON or XML)                    | Zenoh                       |
|------------------|---------------------------------------|-----------------------------|
| health           | `GET /ares/v1/health`                 | `ares/v1/health`            |
| fleet snapshot   | `GET /ares/v1/fleet/snapshot`         | `ares/v1/fleet/snapshot`    |
| plan route       | `POST /ares/v1/routes/plan`           | `ares/v1/routes/plan`       |
| register robot   | `POST /ares/v1/robots`                | `ares/v1/robots/register`   |
| list robots      | `GET /ares/v1/robots`                 | `ares/v1/robots/list`       |
| heartbeat        | `POST /ares/v1/robots/{id}/heartbeat` | `ares/v1/robots/heartbeat`  |
| assign route     | `POST /ares/v1/robots/{id}/route`     | `ares/v1/robots/assign_route` |
| schedule         | `POST /ares/v1/robots/{id}/schedule`  | `ares/v1/robots/schedule`   |
| evaluate claim   | `POST /ares/v1/claims/evaluate`       | `ares/v1/claims/evaluate`   |
| submit claim     | `POST /ares/v1/claims`                | `ares/v1/claims/request`    |
| list claims      | `GET /ares/v1/claims`                 | `ares/v1/claims/list`       |
| list leases      | `GET /ares/v1/leases`                 | `ares/v1/leases/list`       |
| add lease        | `POST /ares/v1/leases`                | `ares/v1/leases/add`        |
| release lease    | `POST /ares/v1/leases/release`        | `ares/v1/leases/release`    |
| release by id    | `DELETE /ares/v1/leases/{id}`         | —                           |

## The adapter rule

Adapters may parse requests, serialise responses, authenticate, publish events,
and call core functions. They may **not** own independent fleet state, bypass the
`ClaimManager`, make route/schedule decisions outside the `Coordinator`, or invent
different semantics per transport. That is what keeps REST, XML, Zenoh,
Python, and the C ABI aligned.
