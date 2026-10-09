# syncbot

Multi-robot navigation, claims, and scheduling on top of
[`zoneout`](../zoneout). Rust port of the C++
[`timenav`](https://github.com/robolibs/timenav) library.

`syncbot` answers four questions for a fleet of robots moving through a
shared workspace:

1. **Where can I go?** — route planning over a workspace graph with
   policy-aware Dijkstra (hard blocks, slowdown penalties, direction locks).
2. **Can I claim it?** — exclusive / shared claims on zones, nodes, and
   edges with capacity enforcement and lease lifecycle.
3. **When can I go?** — schedule a route into time-windowed reservations and
   decide *Proceed | Queue | Replan*.
4. **Who goes first?** — right-of-way arbitration when robots conflict.

## Status

Functional parity with the C++ reference plus **C ABI** and **PyO3**
bindings. The primary validation lanes are the Makefile targets below.

## Build

```sh
make build                           # build the library
make check-all                       # check every transport adapter
make test-all                        # run every Rust target with all adapters
make check-python-adapter            # syntax-check the Python adapter
make test-usecase-rest               # live REST/XML curl battery
make check-python-binding            # build the PyO3 extension and test it
```

## Transport adapters

`serve_workspace` starts one canonical peerbus req/res core. HTTP/JSON and
HTTP/XML are adapters: they parse their external wire, make a typed datapod
call through peerbus, and encode the reply. They never receive a coordinator
handle.

```sh
make run                              # REST/JSON + REST/XML adapters
curl -X POST http://127.0.0.1:8080/ares/v1/robots/register \
  -H 'content-type: application/json' \
  -d '{"robot":"7","key":"1234"}'
```

The frozen topics/schema and Rust/Python adapter examples are documented in
[`docs/WRITING_ADAPTER.md`](docs/WRITING_ADAPTER.md).

The live acceptance scripts under `tests/usecase/` execute every command class
in [`misc/USECASE.typ`](misc/USECASE.typ), including heartbeat auto-release and
cross-transport conflict/release.

## Dependencies

| crate     | role                                                       |
|-----------|------------------------------------------------------------|
| `zoneout` | sibling Rust port — `Workspace`, `Zone`, graph, JSON I/O   |
| `datapod` | POD geometry types                                          |
| `concord` | coordinate transforms (WGS ↔ ENU)                          |
| `graphix` | vertex graph types (reused via `zoneout::Workspace`)       |
| `pyo3`    | Python bindings (optional, gated)                          |

## Concepts at a glance

- **Workspace** — a tree of zones plus a graph of nodes/edges with
  arbitrary `traffic.*` properties.
- **WorkspaceIndex** — an `Arc<Workspace>`-backed lookup layer. By-UUID
  zone/node/edge access, parent/child/ancestor walks, validation issues,
  coord transforms.
- **Policy** — typed parse of `traffic.*` strings into `ZonePolicy` and
  `EdgeTrafficSemantics`.
- **Route** — Dijkstra in three flavours: unconstrained, hard-blocking,
  policy-penalised. Output is a `RoutePlan` (nodes + edges + zones + per-step
  costs); failure is a `RouteFailure` with diagnostic categories.
- **Claim/Lease** — `ClaimRequest` is an intent; `Lease` is a granted
  reservation. `ClaimManager` runs the lifecycle and conflict checks
  (capacity at zone, edge, and membership levels).
- **RobotState + Coordinator** — per-robot progress; rolling-horizon claim
  builder; `schedule_route_request → Proceed | Queue | Replan`; arbitration;
  release-behind-progress; missed-slot handling.
- **VDA 5050** — partial-shape transport structs (`Order`, `State`, etc.)
  for AGV interop.

---

## Quick start (Rust)

```rust
use std::sync::Arc;

use std::collections::BTreeMap as OMap;

use datapod::{Geo, Point, Polygon};
use graphix::vertex::EdgeType;
use syncbot::{WorkspaceIndex, plan_route};
use zoneout::{NodeData, Workspace, ZoneBuilder};

fn rectangle(x0: f64, y0: f64, x1: f64, y1: f64) -> Polygon {
    Polygon { vertices: vec![
        Point::new(x0, y0, 0.0),
        Point::new(x1, y0, 0.0),
        Point::new(x1, y1, 0.0),
        Point::new(x0, y1, 0.0),
    ].into() }
}

fn main() {
    // 1. Build a workspace with a root zone
    let root = ZoneBuilder::new()
        .with_name("yard").with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 100.0, 100.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build().unwrap();

    let mut ws = Workspace::new(root);

    // 2. Add nodes + edges
    let a = ws.add_node_data(NodeData::new(Point::new(10.0, 10.0, 0.0)));
    let b = ws.add_node_data(NodeData::new(Point::new(50.0, 10.0, 0.0)));
    let c = ws.add_node_data(NodeData::new(Point::new(90.0, 10.0, 0.0)));
    let _ = ws.add_edge(a, b, 4.0, EdgeType::Undirected, OMap::new());
    let _ = ws.add_edge(b, c, 4.0, EdgeType::Undirected, OMap::new());

    let a_id = ws.graph().get_vertex(a).unwrap().id;
    let c_id = ws.graph().get_vertex(c).unwrap().id;

    // 3. Build the index and plan a route
    let idx = WorkspaceIndex::new(Arc::new(ws));
    let result = plan_route(&idx, a_id, c_id, false);

    let plan = result.plan.expect("found");
    println!("cost = {}, nodes = {}", plan.total_cost, plan.traversed_node_ids.len());
}
```

The same flow runs as `make run EXAMPLE=route_planning`.

---

## Scenarios

### 1. Block a zone from being routed through

Setting `traffic.blocked=true` on a zone hard-blocks every edge that touches
it.

```rust
let blocked = ZoneBuilder::new()
    .with_name("dock_closed").with_kind("lane")
    .with_boundary(rectangle(40.0, 0.0, 60.0, 20.0))
    .with_datum(Geo::new(52.0, 5.0, 0.0))
    .with_property("traffic.blocked", "true")
    .build().unwrap();
root.add_child(blocked).unwrap();

// plan_route(..., use_penalties=false) will route around the blocked zone.
// If no detour exists, RouteFailure { kind: PolicyBlocked, .. } describes
// exactly which edges and zones blocked the search, plus diagnostics like
// "blocked or restricted resources must be claimed; slowdown only
// increases cost".
```

### 2. Slow zone biases costs without blocking

```rust
let slow = ZoneBuilder::new()
    .with_name("warehouse_aisle")
    .with_property("traffic.policy", "slow")
    .with_property("traffic.speed_limit", "0.5")  // m/s — < 1.0 is "slow"
    .with_boundary(...).with_datum(...).build().unwrap();
```

`plan_route(idx, start, goal, true)` (use_penalties=true) routes around it
when there's a faster path; `use_penalties=false` ignores the bias.

### 3. Direction-locked edges

```rust
let mut edge_props = OMap::new();
edge_props.insert("traffic.preferred_direction".into(), "forward".into());
edge_props.insert("traffic.reversible".into(), "false".into());
ws.add_edge(a, b, 1.0, EdgeType::Directed, edge_props);
```

The Dijkstra engine drops illegal traversals (`forward` on a back-edge);
`diagnose_route_failure` reports `directionally_blocked_edge_ids`.

### 4. Exclusive claim conflict

```rust
use syncbot::{ClaimManager, ClaimRequest, ClaimTarget, ClaimTargetKind,
              ClaimAccessMode, ClaimDecision, ClaimId, RobotId};

let idx = std::sync::Arc::new(WorkspaceIndex::new(Arc::new(ws)));
let mut mgr = ClaimManager::with_index(Arc::clone(&idx));

let r1 = ClaimRequest {
    id: ClaimId::new(1), robot_id: RobotId::new(1),
    access_mode: ClaimAccessMode::Exclusive,
    targets: vec![ClaimTarget {
        kind: ClaimTargetKind::Zone,
        resource_id: dock_zone_id,
    }],
    ..ClaimRequest::default()
};
assert_eq!(mgr.evaluate_request(&r1).decision, ClaimDecision::Grant);
mgr.add_request(r1);

let r2 = ClaimRequest {
    id: ClaimId::new(2), robot_id: RobotId::new(2),
    access_mode: ClaimAccessMode::Exclusive,
    targets: vec![ClaimTarget {
        kind: ClaimTargetKind::Zone,
        resource_id: dock_zone_id,   // same zone!
    }],
    ..ClaimRequest::default()
};
let eval = mgr.evaluate_request(&r2);
assert_eq!(eval.decision, ClaimDecision::Deny);
println!("blocked by: {:?}", eval.conflicting_claim_id);
println!("reason: {}", eval.reason);
for d in &eval.diagnostics { println!("  - {d}"); }
```

### 5. Shared zone with explicit capacity

```rust
let parking = ZoneBuilder::new()
    .with_property("traffic.policy", "shared")
    .with_property("traffic.capacity", "2")
    ...
```

Three robots all asking for `Shared` on this zone: the first two grant, the
third hits "shared zone capacity exceeded" with the offending zone reported
in `eval.blocking_target`.

### 6. Lease lifecycle: grant → refresh → expire

```rust
use syncbot::{Lease, LeaseId};

let lease = Lease {
    id: LeaseId::new(11), claim_id: ClaimId::new(1),
    robot_id: RobotId::new(1),
    access_mode: ClaimAccessMode::Exclusive,
    targets: r1.targets.clone(),
    granted_at_tick: Some(0),
    expires_at_tick: Some(100),
    ..Lease::default()
};
mgr.add_lease(lease);

mgr.refresh_lease(LeaseId::new(11), 80, Some(200));   // extend to t=200
assert_eq!(mgr.expire_leases(150), 0);                 // not yet
assert_eq!(mgr.expire_leases(250), 1);                 // archived as Expired
```

Other endpoints: `release_lease(id, tick)`, `revoke_lease(id, reason, tick)`,
`release_leases_for_robot(id, tick)`.

### 7. Coordinator + rolling-horizon claims

```rust
use syncbot::{Coordinator, RobotState, RobotProgressState, RouteCostModel};

let mut coord = Coordinator::with_index(Arc::clone(&idx));
coord.register_robot(RobotState {
    robot_id: RobotId::new(1),
    horizon: 3,
    ..RobotState::default()
});
coord.assign_route_plan(RobotId::new(1), plan, /* horizon */ 3, /* tick */ 0);

// Each tick, ask for a claim covering only the next `horizon` steps:
let req = coord.claim_request_for_robot(
    RobotId::new(1), ClaimId::new(42), ClaimAccessMode::Exclusive,
);
let eval = coord.claim_manager().evaluate_request(&req);
```

The horizon shrinks the claim's targets to a sliding window so the manager
isn't holding the whole route locked.

### 8. Schedule a route — *Proceed | Queue | Replan*

```rust
let decision = coord.schedule_robot_route(
    RobotId::new(1), ClaimId::new(42),
    /* start_tick */ 0,
    /* ticks_per_cost_unit */ 1.0,
    ClaimAccessMode::Exclusive,
);
match decision.kind {
    syncbot::ScheduleDecisionKind::Proceed => println!("clear to go"),
    syncbot::ScheduleDecisionKind::Queue   => println!(
        "wait until tick {}; queue position {}",
        decision.start_tick, decision.queue_position
    ),
    syncbot::ScheduleDecisionKind::Replan  => println!(
        "must replan: {} conflicts on corridors / blocked / no-stop resources",
        decision.conflicts.len()
    ),
}
```

The decision automatically applies to the robot state (sets `wait_ticks`,
`hold_reason`, `progress_state`).

### 9. Right-of-way arbitration

When two robots simultaneously want the same intersection, decide who waits:

```rust
use syncbot::{ArbitrationContext, ArbitrationDecision, arbitrate_right_of_way};

let ctx = ArbitrationContext {
    self_priority: 5.0, other_priority: 3.0,
    self_holds_lease: true,
    self_state: RobotProgressState::FollowingRoute,
    other_state: RobotProgressState::Waiting,
    self_remaining_steps: 4, other_remaining_steps: 12,
    ..ArbitrationContext::default()
};
match arbitrate_right_of_way(&ctx) {
    ArbitrationDecision::Proceed => /* go */ (),
    ArbitrationDecision::Yield   => /* wait */ (),
    ArbitrationDecision::Replan  => /* tied; replan */ (),
}
```

Tie-break order: emergency → lease holder → priority → blocked-state →
following-vs-waiting → wait-ticks → remaining-steps → Replan.

### 10. Missed schedule slot

```rust
if coord.handle_missed_schedule_slot(
    RobotId::new(1),
    /* current_tick */ 50,
    /* grace_ticks */ 5,
) {
    // robot.progress_state == Replanning; hold_reason = "missed_reservation_window"
}
```

### 11. Release leases behind progress

As a robot crosses each node, leases on already-traversed resources are no
longer needed:

```rust
coord.update_robot_progress(
    RobotId::new(1),
    Some(node_id_we_just_reached),
    None,
    /* updated_at_tick */ 12,
);
let released = coord.release_behind_progress(RobotId::new(1));
println!("released {} stale leases", released);
```

### 12. VDA 5050 mapping

```rust
let order = syncbot::vda::map_route_plan(&plan);
// order.nodes.len() == plan.traversed_node_ids.len()
// order.version == "3.0.0"

let agv_state = syncbot::vda::map_robot_state(&robot_state);
// agv_state.driving_state == "DRIVING" while FollowingRoute
```

The full VDA struct set is partial-by-design (compatibility surface, not
schema clone): `Order`, `OrderNode`, `OrderEdge`, `State`, `Connection`,
`Factsheet`, `InstantAction`, `Response`.

### 13. Zone policy validation

```rust
let issues = syncbot::validate_zone_traffic_properties(&zone.properties());
for i in &issues {
    println!("[{:?}] {}: {}", i.severity, i.key, i.message);
}
```

Catches unknown traffic keys, malformed bool/u64/f64 values, conflicts like
`stop_allowed=true` with `no_stop=true`.

---

## C ABI

The crate builds a `cdylib` (`libsyncbot.so` / `.dylib` / `.dll`) plus a
header-free C ABI. Pattern: opaque handles for stateful types, JSON strings
for everything else.

```c
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>

extern const char *sb_version(void);
extern const char *sb_last_error(void);
extern void sb_string_free(char *s);

typedef struct SbClaimManager SbClaimManager;
extern SbClaimManager *sb_claim_manager_new(void);
extern void sb_claim_manager_free(SbClaimManager *);
extern int  sb_claim_manager_add_request(SbClaimManager *, const char *json);
extern char *sb_claim_manager_evaluate(const SbClaimManager *, const char *json);

int main(void) {
    printf("syncbot %s\n", sb_version());

    SbClaimManager *m = sb_claim_manager_new();
    const char *req =
        "{\"id\":1,\"robot_id\":1,\"mission_id\":0,"
        "\"access_mode\":\"Exclusive\",\"priority\":0,"
        "\"requested_at_tick\":null,"
        "\"window\":{\"start_tick\":null,\"end_tick\":null},"
        "\"targets\":[{\"kind\":\"Zone\","
        "\"resource_id\":\"00000000-0000-0000-0000-000000000001\"}]}";
    sb_claim_manager_add_request(m, req);
    char *eval = sb_claim_manager_evaluate(m, req);
    printf("eval: %s\n", eval);
    sb_string_free(eval);
    sb_claim_manager_free(m);
    return 0;
}
```

A complete demo lives in `examples/c_abi/demo.c` with a `Makefile`:

```sh
cd examples/c_abi && make run
```

### C ABI surface (selected)

```
sb_version(), sb_last_error(), sb_string_free(*)

# Workspace + Index (opaque handles)
sb_workspace_load(path) -> *SbWorkspace
sb_workspace_free(*)
sb_workspace_index_new(*) -> *SbWorkspaceIndex
sb_workspace_index_validation_issues(*) -> json
sb_workspace_index_root_zone_id(*) -> uuid string

# Route planning
sb_plan_route(idx, start_uuid, goal_uuid, use_penalties) -> json

# ClaimManager
sb_claim_manager_new() / _with_index(idx) / _free(*)
sb_claim_manager_add_request / _remove_request
sb_claim_manager_add_lease / _release_lease / _expire_leases
sb_claim_manager_refresh_lease / _revoke_lease
sb_claim_manager_evaluate(*, request_json) -> json
sb_claim_manager_requests / _leases (json)

# Coordinator
sb_coordinator_new() / _with_index(idx) / _free(*)
sb_coordinator_register_robot / _unregister_robot
sb_coordinator_assign_route_plan
sb_coordinator_schedule_robot_route -> json (ScheduleDecision)
sb_coordinator_handle_missed_schedule_slot
sb_coordinator_robot_state(robot_id) -> json

# VDA
sb_vda_order_from_route(plan_json) -> json
sb_vda_state_from_robot(state_json) -> json

# Helpers
sb_arbitrate_right_of_way(*ctx) -> 0/1/2
sb_parse_traffic_bool/u64/f64
sb_parse_zone_policy(props_json) -> json
sb_validate_zone_traffic / _validate_edge_traffic
```

All `char *` returns must be freed with `sb_string_free`. On error, a
function returns `NULL` / `-1` and `sb_last_error()` describes the cause
(thread-local).

---

## Python bindings

The same surface, idiomatic Python, gated behind the `python` /
`python` feature. Built with [maturin](https://github.com/PyO3/maturin):

```sh
pip install maturin
cd syncbot
maturin develop --features python
python -c "import syncbot; print(syncbot.version())"
```

A complete smoke script in `examples/python_binding/example.py`:

```python
import syncbot

# version + constants
print(syncbot.version())                    # "0.1.0"
print(syncbot.ZONE_POLICY_KINDS)
print(syncbot.SCHEDULE_DECISION_KINDS)

# Policy
policy = syncbot.parse_zone_policy({
    "traffic.policy": "exclusive",
    "traffic.capacity": "2",
})
assert policy["kind"] == "ExclusiveAccess"

# Validation
issues = syncbot.validate_zone_traffic_properties({"traffic.bogus": "x"})
print(issues)  # [{"severity": "Warning", "key": "traffic.bogus", ...}]

# Arbitration
print(syncbot.arbitrate_right_of_way(self_is_emergency=True))   # "proceed"

# ClaimManager
mgr = syncbot.ClaimManager()
mgr.add_request({
    "id": 1, "robot_id": 1, "mission_id": 0,
    "access_mode": "Exclusive", "priority": 0,
    "requested_at_tick": None,
    "window": {"start_tick": None, "end_tick": None},
    "targets": [{"kind": "Zone",
                 "resource_id": "00000000-0000-0000-0000-000000000001"}],
})
eval_result = mgr.evaluate_request(req)
print(eval_result["decision"])               # "Grant"

# Coordinator
coord = syncbot.Coordinator()
coord.register_robot({
    "robot_id": 7, "mission_id": 0, ...
})
coord.assign_route_plan(7, route_plan_dict, horizon=3, updated_at_tick=0)
decision = coord.schedule_robot_route(7, claim_id=1, start_tick=0,
                                       ticks_per_cost_unit=1.0,
                                       access_mode="exclusive")

# Workspace from disk
ws = syncbot.Workspace.load("/path/to/workspace_dir")
idx = syncbot.WorkspaceIndex(ws)
result = syncbot.plan_route(idx, start_uuid, goal_uuid, use_penalties=True)

# VDA
order = syncbot.vda_order_from_route(route_plan_dict)
state = syncbot.vda_state_from_robot(robot_state_dict)
```

### Python class surface

| Class              | Methods (selected)                                          |
|--------------------|-------------------------------------------------------------|
| `WorkspaceBuilder` | `WorkspaceBuilder(name, boundary, datum)`, `add_zone`, `add_node`, `add_edge`, `build()` |
| `Workspace`        | `Workspace.load(path)`, `save(path)`, `root_zone_id()`, `datum()`, `node/edge_count()` |
| `WorkspaceIndex`   | `WorkspaceIndex(ws)`, `zones()`, `nodes()`, `edges()`, `zone/node(uuid)`, `zone_policy`, `edge_semantics`, `zones_of_node/edge`, `zone/node/edge_by_numeric_id`, `edge_between`, `local_to_global`, `global_to_local`, `datum()`, `validation_issues()`, `is_valid()`, `refresh()` |
| `ClaimManager`     | `ClaimManager(index=None)`, `add/upsert/remove_request`, `upsert_request_for_robot`, `remove_requests_for_robot`, `add/release/refresh/revoke/expire_lease(s)`, `release_leases_for_robot`, `leases_for_robot`, `evaluate_request`, `next_request_id`, `requests()`, `leases()` |
| `Coordinator`      | `Coordinator(index=None)`, `register/unregister_robot`, `robot_state`, `assign_route_plan`, `update_robot_progress`, `update_robot_pose`, `schedule_robot_route`, `claim_request_for_robot`, `evaluate_claim`, `upsert_claim_request_for_robot`, `remove_claim_requests_for_robot`, `update_robot_claim_state`, `handle_missed_schedule_slot`, `release_behind_progress`, `refresh/revoke_robot_leases`, `set_alive`, `touch_robot`, `robot_active_at`, `inactive_robots_at`, `sweep_inactive`, `expire_claims`, `snapshot()` |

Free functions: `plan_route`, `arbitrate_right_of_way`, `parse_zone_policy`,
`parse_edge_traffic_semantics`, `validate_zone/edge_traffic_properties`,
`parse_traffic_{bool,u64,f64,string}`, `vda_order_from_route`,
`vda_state_from_robot`, `version`.

The surface is enough to build a workspace, plan over it and run the whole
claim loop without dropping to Rust — `examples/python_binding/` has a
[binding test](examples/python_binding/test_bindings.py) that proves it and a
[packhouse-yard scenario](examples/python_binding/packhouse_yard.py) that is a
line-for-line twin of the Rust `packhouse_yard` example. Both produce the same
numbers, tick for tick.

```sh
make check-python-binding                 # build the extension, run the test
cd examples/python_binding && make yard    # the scenario, in rerun
```

---

## Architecture

```
                    ┌──────────────────────────────────────┐
                    │              Coordinator             │
                    │  • RobotState[] • Schedule • Arbit.  │
                    └──────────────┬───────────────────────┘
                                   │ owns
                                   ▼
                    ┌──────────────────────────────────────┐
                    │            ClaimManager              │
                    │  • requests   • leases               │
                    │  • capacity / conflict evaluation    │
                    └──────────────┬───────────────────────┘
                                   │ Arc
                                   ▼
                    ┌──────────────────────────────────────┐
                    │           WorkspaceIndex             │
                    │  zones / nodes / edges by UUID       │
                    │  policy lookup, traversal helpers    │
                    └──────────────┬───────────────────────┘
                                   │ Arc<Workspace>
                                   ▼
                    ┌──────────────────────────────────────┐
                    │       zoneout::Workspace             │
                    │  Zone tree + graphix vertex graph    │
                    └──────────────────────────────────────┘
```

### Module map

```
src/
├── core/{error.rs, ids.rs}     errors + RobotId/MissionId/ClaimId/LeaseId
├── policy.rs                   ZonePolicy / EdgeTrafficSemantics + parsing
├── index.rs                    WorkspaceIndex (Arc<Workspace> ownership)
├── route.rs                    Dijkstra ×3 + RoutePlan + RouteFailure
├── claim/{mod.rs, manager.rs}  ClaimRequest/Lease/Eval + ClaimManager
├── robot.rs                    RobotState, RobotProgressState
├── coordinator.rs              Coordinator + scheduling + arbitration
├── vda/                        VDA 5050 transport + Adapter
├── wire/peerbus.rs             canonical datapods + req/res core/client
├── wire/{rest,xmlt}.rs         HTTP JSON/XML peerbus adapters
├── ffi.rs                      C ABI (opaque handles + JSON marshaling)
└── python/mod.rs               PyO3 (gated, full surface)
```

## See also

- [`docs/WRITING_ADAPTER.md`](./docs/WRITING_ADAPTER.md) — the frozen wire contract
- [`CHANGELOG.md`](./CHANGELOG.md) — release notes
- C++ source: [`../../robolibs_cpp/syncbot`](../../robolibs_cpp/syncbot)
- Sibling Rust port: [`../zoneout`](../zoneout)

## License

MIT.
