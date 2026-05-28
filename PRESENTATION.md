---
title: "timenav Transports"
sub_title: "One coordination core, optional REST and robotics doors"
author: "Trim Bresilla"
---

# One Core, Optional Doors

```text
                 +----------------------------------+
                 |        timenav core              |
                 | route, policy, claims, schedule  |
                 +----------------------------------+
                         ^                 ^
                         |                 |
              feature = "rest"      feature = "robo"
                         |                 |
                    axum HTTP          zenoh queries/pubsub
                         |                 |
                 REST clients         robots / bridges
```

- Core logic stays dependency-light.
- Transport crates are optional.
- REST is enabled by feature `rest`.
- Robotics transport is enabled by feature `robo`.

<!-- end_slide -->

# Cargo Features

```toml
[features]
default = []
rest = ["dep:axum"]
robo = ["dep:zenoh"]
python = ["dep:pyo3", "pyo3/abi3-py39", "pyo3/auto-initialize"]
python-extension = ["python", "pyo3/extension-module"]
```

Build lanes:

```sh
cargo check                 # core only
cargo check --features rest # REST adapter
cargo check --features robo # Zenoh adapter
cargo check --features "rest robo"
```

Note: the crate is `axum`, not `axium`.

<!-- end_slide -->

# Core API Remains the Source of Truth

Adapters should only translate wire messages into core calls:

```text
WorkspaceIndex
  -> plan_route()
  -> RoutePlan
  -> ClaimRequest
  -> ClaimManager
  -> Coordinator
  -> ScheduleDecision
```

No adapter should reimplement:

- route planning,
- traffic policy,
- claim conflict checks,
- capacity rules,
- right-of-way arbitration.

<!-- end_slide -->

# REST Adapter: feature = "rest"

Crate: `axum`

Module: `src/wire/rest.rs`

Namespace:

```text
/ares/v1
```

Suggested endpoints:

```text
GET     /ares/v1/health
POST    /ares/v1/robots
DELETE  /ares/v1/robots/{id}
POST    /ares/v1/robots/{id}/heartbeat

POST    /ares/v1/routes/plan
POST    /ares/v1/claims
GET     /ares/v1/claims/{id}
DELETE  /ares/v1/leases/{id}

GET     /ares/v1/robots
GET     /ares/v1/leases
GET     /ares/v1/fleet/snapshot
GET     /ares/v1/events        # SSE later
```

REST is for dashboards, tools, tests, and non-robot clients.

<!-- end_slide -->

# Robo Adapter: feature = "robo"

Crate: `zenoh`

Module: `src/wire/robo.rs`

Key prefix:

```text
ares/v1
```

Suggested queryables:

```text
ares/v1/robots/register
ares/v1/robots/{id}/deregister
ares/v1/robots/{id}/heartbeat

ares/v1/routes/plan
ares/v1/claims/request
ares/v1/leases/{id}/release

ares/v1/robots/**
ares/v1/leases/**
ares/v1/fleet/snapshot
```

Suggested pub/sub streams:

```text
ares/v1/events/lease
ares/v1/events/zone
ares/v1/events/alert
ares/v1/fleet/state
```

Zenoh is for robot-to-core, bridge, edge, and wireless deployments.

<!-- end_slide -->

# Shared Semantics

Same operation, different transport:

| Capability     | REST                                | Zenoh                             |
|----------------|-------------------------------------|-----------------------------------|
| register robot | `POST /ares/v1/robots`              | `ares/v1/robots/register`         |
| heartbeat      | `POST /ares/v1/robots/{id}/heartbeat` | `ares/v1/robots/{id}/heartbeat` |
| plan route     | `POST /ares/v1/routes/plan`         | `ares/v1/routes/plan`             |
| request claim  | `POST /ares/v1/claims`              | `ares/v1/claims/request`          |
| release lease  | `DELETE /ares/v1/leases/{id}`       | `ares/v1/leases/{id}/release`     |
| fleet snapshot | `GET /ares/v1/fleet/snapshot`       | `ares/v1/fleet/snapshot`          |
| events         | `GET /ares/v1/events`               | `ares/v1/events/**`               |

Only the wire protocol differs. The state machine and decisions are identical.

<!-- end_slide -->

# Adapter Rule

Adapters are allowed to:

- parse requests,
- serialize responses,
- authenticate later,
- publish events,
- call core functions.

Adapters are not allowed to:

- own independent fleet state,
- bypass `ClaimManager`,
- make route/schedule decisions outside `Coordinator`,
- invent different semantics per transport.

That keeps REST, Zenoh, Python, and C ABI behavior aligned.
