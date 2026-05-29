# Concepts

Every transport speaks in terms of the same handful of resources and decisions.
This page is the shared vocabulary.

## Workspace

The map. A tree of **zones** plus a graph of **nodes** and **edges**. Zones carry
arbitrary `traffic.*` properties that drive policy:

```text
traffic.policy          = exclusive | shared | slow | ...
traffic.capacity        = 2
traffic.blocked         = true
traffic.speed_limit     = 0.5
traffic.preferred_direction = forward
```

The workspace is loaded once and wrapped in a `WorkspaceIndex` — a by-UUID lookup
layer over zones / nodes / edges with parent/child walks and coordinate
transforms. The index is shared (`Arc`) by the `ClaimManager` and `Coordinator`.

## Routes

`plan_route(index, start, goal, use_penalties)` runs Dijkstra over the graph and
returns a `RoutePlan`:

- `traversed_node_ids`, `traversed_edge_ids`, `traversed_zone_ids`
- per-step costs and `total_cost`

`use_penalties = false` only hard-blocks (e.g. `traffic.blocked`); `true` also
biases away from slow/restricted resources. Failure is a `RouteFailure` with a
diagnostic category (missing start/goal, policy-blocked, unreachable).

Routes matter to claims because later stages reserve **the zones/edges/nodes the
route traverses**, not just the endpoints.

## Claims and leases

- A **`ClaimRequest`** is an intent: "robot R wants these targets, in this access
  mode, optionally within this time window."
- The **`ClaimManager`** evaluates it against current state and returns a
  **`ClaimEvaluation`**: `Grant` or `Deny`, with the conflicting claim/lease and
  the blocking target on denial.
- A granted request becomes a **`Lease`** — an actual reservation with a
  lifecycle: `granted → refreshed → released | expired | revoked`.

**Targets** are `{ kind, resource_id }` where `kind` is `Zone`, `Node`, or
`Edge`. **Access modes** are `Exclusive` (one robot owns the target) or `Shared`
(multiple, up to the zone/edge `traffic.capacity`).

## Coordination

The **`Coordinator`** owns per-robot state plus a `ClaimManager`. It can:

- build **rolling-horizon** claims (reserve only the next *N* route steps),
- **schedule** a route into tick windows and decide `Proceed | Queue | Replan`,
- **arbitrate** right-of-way between robots,
- release leases **behind** a robot's progress,
- handle a **missed** schedule slot.

A schedule decision:

| Decision  | Meaning                                                  |
|-----------|----------------------------------------------------------|
| `Proceed` | route is clear, go                                       |
| `Queue`   | wait until the blocking window ends (carries a position) |
| `Replan`  | conflict is not safely queueable (corridor / no-stop)    |

## Layering

```text
Coordinator           owns RobotState[], scheduling, arbitration
   └─ ClaimManager    requests + leases, capacity / conflict checks
        └─ WorkspaceIndex   zones/nodes/edges by UUID, policy lookup
             └─ Workspace    zone tree + graph
```

A transport adapter sits *above* the `Coordinator`, never beside it.
