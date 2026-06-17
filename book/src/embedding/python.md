# Python

The same surface as the C ABI, idiomatic Python, via [PyO3](https://pyo3.rs).
In-process — no network. Pass dicts, get dicts.

- **Feature:** `python` / `python-extension`
- **Module:** `src/python.rs`
- **Build:** [maturin](https://github.com/PyO3/maturin)

## Build and import

```sh
pip install maturin
cd syncbot
maturin develop --features python-extension
python -c "import syncbot; print(syncbot.version())"
```

## Example

```python
import syncbot

# Policy parsing
policy = syncbot.parse_zone_policy({
    "traffic.policy": "exclusive",
    "traffic.capacity": "2",
})
assert policy["kind"] == "ExclusiveAccess"

# ClaimManager
mgr = syncbot.ClaimManager()
req = {
    "id": 1, "robot_id": 1, "mission_id": 0,
    "access_mode": "Exclusive", "priority": 0,
    "window": {"start_tick": None, "end_tick": None},
    "targets": [{"kind": "Zone",
                 "resource_id": "00000000-0000-0000-0000-000000000001"}],
}
mgr.add_request(req)
print(mgr.evaluate_request(req)["decision"])     # "Grant"

# Coordinator + workspace
coord = syncbot.Coordinator()
coord.register_robot({"robot_id": 7})

ws  = syncbot.Workspace.load("/path/to/workspace_dir")
idx = syncbot.WorkspaceIndex(ws)
result = syncbot.plan_route(idx, start_uuid, goal_uuid, use_penalties=True)
```

## Class surface

| Class            | Methods (selected)                                                                 |
|------------------|------------------------------------------------------------------------------------|
| `Workspace`      | `Workspace.load(path)`, `root_zone_id()`                                           |
| `WorkspaceIndex` | `WorkspaceIndex(ws)`, `root_zone_id()`, `validation_issues()`, `is_valid()`, `zones_of_node(uuid)`, `refresh()` |
| `ClaimManager`   | `add/upsert/remove_request`, `add/release/refresh/revoke/expire_lease(s)`, `evaluate_request`, `requests()`, `leases()` |
| `Coordinator`    | `register/unregister_robot`, `robot_state`, `assign_route_plan`, `update_robot_progress`, `schedule_robot_route`, `handle_missed_schedule_slot`, `release_behind_progress` |

Free functions: `plan_route`, `arbitrate_right_of_way`, `parse_zone_policy`,
`parse_edge_traffic_semantics`, `validate_zone/edge_traffic_properties`,
`vda_order_from_route`, `vda_state_from_robot`, `version`.

## Python vs the wire transports

This binding is for **embedding** syncbot in a Python process. If you instead want
to talk to a *running* syncbot server from Python, use the
[REST / JSON](../wire/rest-json.md) transport over `urllib`/`requests` — see
`scripts/rest_demo.py` for a dependency-free two-robot simulation.
