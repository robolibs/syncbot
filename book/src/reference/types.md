# Shared message types

These live in `syncbot::wire` and are shared by the REST and Zenoh adapters. Each
adapter just chooses an encoding (JSON or XML) for them. JSON field names are
shown; XML uses the same names as elements.

## State and errors

```rust
ServeState                 // wraps Arc<RwLock<Coordinator>>; construct every adapter from it
ApiError  { message }      // returned on failure (HTTP 400 / Zenoh error reply)
Health    { status, version }
```

## Snapshot

```rust
FleetSnapshot {
    robots:   Vec<RobotState>,
    requests: Vec<ClaimRequest>,
    leases:   Vec<Lease>,
}
```

## Requests

```rust
PlanRouteRequest {
    start_node_id: ResourceRef,   // UUID or numeric, as text
    goal_node_id:  ResourceRef,
    use_penalties: bool,          // default false
}

ScheduleRobotRouteRequest {
    claim_id:            ClaimId,
    start_tick:          u64,
    ticks_per_cost_unit: f64,
    access_mode:         ClaimAccessMode,   // "Exclusive" | "Shared"
    key:                 Option<String>,    // required (state-changing)
}

AssignRouteRequest { route_plan, horizon, updated_at_tick, key: Option<String> }

ReleaseLeaseRequest { lease_id: LeaseId, released_at_tick: Option<u64> }
```

## Flat (tier-1) types

The PLC-facing flows use flat request envelopes and one reply type. `key` is an
integer password or `did:pass=<secret>`; `id` carries one or more ids.

```rust
FlatRegister  { robot: String, key: String, alive: Option<u64> }  // key+alive optional
FlatHeartbeat { key: String, zone: Option<i64>, node: Option<u64>, edge: Option<u64> }
FlatClaim     { key: String, robot: String, id: Vec<u64>,
                access_mode: Option<u8>, lease_time: Option<u64> }
FlatRelease   { key: String, robot: String, id: u64 }

// key omitted -> shared default password (insecure)
// access_mode: 0/absent/1 -> exclusive; 2+ reserved (rejected)
// lease_time: seconds; 0/absent -> unlimited

FlatReply { decision: u8, reason: u8, blocked: Option<u64> }   // 1/0, enum, offending id
```

## Claim wire types

The wire claim types carry `ResourceRef` (UUID-or-integer) and resolve to core
`ClaimTarget` / `ClaimRequest` against the `WorkspaceIndex`:

```rust
ClaimTargetWire  { kind: ClaimTargetKind, resource_id: ResourceRef }
ClaimRequestWire {
    id, robot_id, mission_id,
    access_mode, priority,
    requested_at_tick: Option<u64>,    // omitted when None
    window: ClaimWindow,               // { start_tick?, end_tick? }
    targets: Vec<ClaimTargetWire>,
    key: Option<String>,               // required on submit; ignored by evaluate
}
```

`ClaimRequestWire`, `RobotState`, and `Lease` all deserialise with `#[serde(default)]`,
so a client only sends the fields it cares about.

## Core response types

Returned as-is (their core JSON shape):

```rust
ClaimEvaluation {
    decision: "Grant" | "Deny",
    reason: String,
    conflicting_claim_id: Option<ClaimId>,
    conflicting_lease_id: Option<LeaseId>,
    conflicting_targets:  Vec<ClaimTarget>,
    blocking_target:      Option<ClaimTarget>,
    diagnostics:          Vec<String>,
}

ScheduleDecision { kind: "Proceed"|"Queue"|"Replan", start_tick, queue_position, conflicts, diagnostics }

PlanRouteResponse { found: bool, distance: f64, plan: Option<RoutePlan>, failure: Option<RouteFailure> }
```

See [Resource IDs](../resource-ids.md) for the `ResourceRef` encoding.
