---
title: "timenav Transports"
sub_title: "How external systems talk to the coordinator"
author: "Trim Bresilla"
---

# One Core, Several Doors

```text
                    +--------------------------------+
                    |          timenav core          |
                    |  route, policy, claims,        |
                    |  schedule, arbitration         |
                    +--------------------------------+
                       ^         ^         ^
                       |         |         |
                   REST/JSON  REST/XML   Zenoh
                       |         |         |
                  dashboards   PLCs    robots / bridges
                  & tooling           (wireless / edge)
```

All doors hit the same coordinator, the same `ClaimManager`, the
same scheduling logic. Only the encoding and transport differ.

<!-- end_slide -->

# Pick Your Door

| Caller                                  | Recommended transport |
|-----------------------------------------|-----------------------|
| Dashboard, web tool, integration test   | **REST + JSON**       |
| PLC, legacy controller, XML-native peer | **REST + XML**        |
| AGV / mobile robot, on-vehicle bridge   | **Zenoh**             |
| In-process embedding                    | Python or C bindings  |

Every operation in the table on the *Shared Semantics* slide is
available on every transport. Pick the one your environment is
already speaking.

<!-- end_slide -->

# Common namespace

```text
/ares/v1/...            REST (JSON or XML)
ares/v1/...             Zenoh (key expression)
```

Same path under both, with the `/` for HTTP and the slash-separated
key form for Zenoh.

Endpoints (selection):

```text
GET     /ares/v1/health
GET     /ares/v1/fleet/snapshot

POST    /ares/v1/routes/plan
POST    /ares/v1/claims/evaluate
POST    /ares/v1/claims
GET     /ares/v1/claims
GET     /ares/v1/leases
DELETE  /ares/v1/leases/{id}

POST    /ares/v1/robots
GET     /ares/v1/robots
POST    /ares/v1/robots/{id}/heartbeat
POST    /ares/v1/robots/{id}/route
POST    /ares/v1/robots/{id}/schedule
```

<!-- end_slide -->

# Resource model

What an integrator needs to know:

- **Workspace** — the map (zones, nodes, edges) shared by all robots.
- **Zone** — a polygonal area, possibly nested. Carries traffic policy
  (`exclusive`, `shared`, `blocked`, `speed_limit`, …).
- **Node** / **Edge** — graph for routing.
- **Robot** — registered participant. Has an integer `robot_id`.
- **ClaimRequest** — "I want to reserve these targets." Returns a
  `ClaimEvaluation` of `Grant` or `Deny`.
- **Lease** — a granted reservation. Has a lifecycle:
  `granted → refreshed → released | expired | revoked`.
- **Schedule decision** — `Proceed | Queue | Replan` once a claim is
  evaluated against a time horizon.

<!-- end_slide -->

# Resource IDs — UUID or integer

Every workspace resource has a UUID. A resource can additionally
carry an integer alias via the workspace property:

```text
external.numeric_id = "205"
```

On the wire, both forms are accepted on any field that takes a
`resource_id`:

```json
{ "kind": "Zone", "resource_id": "00000000-0000-0000-0000-000000000001" }
{ "kind": "Zone", "resource_id": "205" }
```

Numeric IDs in JSON are strings (`"205"`) for parity with XML, which
encodes everything as text. The server resolves either form against
the workspace before doing any work.

<!-- end_slide -->

# REST + JSON

`Content-Type: application/json`.

Claim a zone:

```sh
curl -X POST http://<host>/ares/v1/claims/evaluate \
     -H "Content-Type: application/json" \
     -d '{
       "id": 1,
       "robot_id": 1,
       "access_mode": "Exclusive",
       "targets": [{ "kind": "Zone", "resource_id": "205" }]
     }'
```

Response:

```json
{
  "decision": "Grant",
  "reason": "",
  "conflicting_claim_id": null,
  "diagnostics": []
}
```

…or `"decision": "Deny"` with a reason and a `blocking_target`.

<!-- end_slide -->

# REST + XML

`Content-Type: application/xml`. Same paths, same fields, same
semantics — just the encoding differs.

Claim a zone:

```xml
<ClaimRequestWire>
  <id>1</id>
  <robot_id>1</robot_id>
  <mission_id>0</mission_id>
  <access_mode>Exclusive</access_mode>
  <priority>0</priority>
  <targets>
    <kind>Zone</kind>
    <resource_id>205</resource_id>
  </targets>
</ClaimRequestWire>
```

Response:

```xml
<ClaimEvaluation>
  <decision>Grant</decision>
  <reason></reason>
</ClaimEvaluation>
```

Multiple targets repeat the `<targets>` element.

<!-- end_slide -->

# Zenoh

Zenoh queryables under `ares/v1/`. Payload is JSON. Same shape, same
field names.

```text
ares/v1/health
ares/v1/fleet/snapshot
ares/v1/routes/plan
ares/v1/claims/evaluate
ares/v1/claims/request
ares/v1/robots/register
ares/v1/robots/heartbeat
ares/v1/robots/schedule
ares/v1/leases/release
```

Plus pub/sub streams:

```text
ares/v1/events/lease
ares/v1/events/zone
ares/v1/events/alert
ares/v1/fleet/state
```

Recommended for wireless / on-vehicle clients: Zenoh handles
discovery, reconnect, and multicast natively.

<!-- end_slide -->

# Shared semantics

Same operation, three transports:

| Capability        | REST (JSON or XML)                    | Zenoh                             |
|-------------------|---------------------------------------|-----------------------------------|
| register robot    | `POST /ares/v1/robots`                | `ares/v1/robots/register`         |
| heartbeat         | `POST /ares/v1/robots/{id}/heartbeat` | `ares/v1/robots/heartbeat`        |
| plan route        | `POST /ares/v1/routes/plan`           | `ares/v1/routes/plan`             |
| evaluate claim    | `POST /ares/v1/claims/evaluate`       | `ares/v1/claims/evaluate`         |
| submit claim      | `POST /ares/v1/claims`                | `ares/v1/claims/request`          |
| release lease     | `DELETE /ares/v1/leases/{id}`         | `ares/v1/leases/release`          |
| fleet snapshot    | `GET /ares/v1/fleet/snapshot`         | `ares/v1/fleet/snapshot`          |
| events            | `GET /ares/v1/events` (SSE later)     | `ares/v1/events/**`               |

Only the wire protocol differs. The state machine, conflict checks,
and scheduling decisions are identical.

<!-- end_slide -->

# What the server guarantees

- Same `ClaimManager` evaluates every claim, no matter the transport.
- Decisions are reproducible: same workspace + same claims +
  same tick = same answer.
- Numeric IDs always resolve through the same `external.numeric_id`
  property — a claim by integer and a claim by UUID hit the same
  resource.
- No transport invents its own semantics. A `Grant` on REST/JSON is
  the same `Grant` on REST/XML and on Zenoh.

What it does **not** do:

- It does not move robots. The client is still responsible for
  driving once a claim/lease is granted.
- It does not authenticate (yet). Run behind a trusted boundary.
- It does not store history beyond active and recently archived
  leases.

<!-- end_slide -->

# Embedding (no network)

When the caller lives in the same process as the coordinator, two
in-process integrations skip the wire entirely:

- **Python** — `import timenav` (PyO3 binding). Pass dicts; the same
  wire shapes apply.
- **C ABI** — opaque handles + JSON strings via `libtimenav.so`.
  Suitable for non-Rust embedding.

Both call directly into `Coordinator` / `ClaimManager` — no
serialisation cost, but no isolation either.
