# Changelog

## [0.0.2] - 2026-06-06

### <!-- 0 -->⛰️  Features

- Add Zenoh/ROS2DDS service endpoint for listing zones
- Improve C ABI with cbindgen and update Python bindings
- Implement REST endpoint for workspace details
- Add quicbit transport for fleet events
- Add REST server example and Python demo script
- Introduce HTTP REST and Zenoh robotics wire protocols
- CPP-PARITY

### <!-- 1 -->🐛 Bug Fixes

- Update Makefile for improved example running

### <!-- 3 -->📚 Documentation

- Add initial project documentation
- Update `CHANGELOG.md` and `README.md` for 0.0.1 release

### <!-- 7 -->⚙️ Miscellaneous Tasks

- Add daemon mode for running examples
- Update example addresses to bind to all interfaces
- Harmonize example and documentation references

### Deps

- Pin all sibling crates to new revs

## 0.0.1 — initial Rust port

First release of the Rust port of the C++
[`timenav`](https://github.com/robolibs/timenav) library, sitting on top of
the sibling Rust [`zoneout`](../zoneout) port.

### Added

- `core::ids` — strong-typed `RobotId`, `MissionId`, `ClaimId`, `LeaseId`
  newtypes (newtype `u64` with `serde(transparent)` round-tripping).
- `core::error` — `Error` / `Result` mapping `dp::Error::*` variants
  (`invalid_argument`, `not_found`, `parse_error`).
- `policy` — `ZonePolicy`, `EdgeTrafficSemantics`, traffic property parsing
  (camelCase aliases included), validation (`validate_zone_traffic_properties`,
  `validate_edge_traffic_properties`), `merge_zone_policy`,
  `derive_effective_edge_semantics`.
- `index` — `WorkspaceIndex` over `Arc<zoneout::Workspace>`: by-UUID lookup
  for zones / nodes / edges, parent / child / ancestor / descendant zone
  walks, `nodes_in_zone`, `zones_of_node`, `zones_of_edge`, `edge_between`,
  `local_to_global` / `global_to_local` (via `concord`), and full
  `validation_issues()` reporting.
- `route` — three Dijkstra variants (unconstrained, hard-blocking,
  policy-penalised) sharing one inner engine; `RoutePlan`, `RouteStep`,
  `RouteFailure` with detailed diagnosis (`MissingStartNode`,
  `MissingGoalNode`, `PolicyBlocked`, `Unreachable`); plan
  reconstruction, validation, and the top-level `plan_route` entry point.
- `claim` — `ClaimRequest`, `Lease`, `ClaimEvaluation` data model plus
  `ClaimManager` with full lifecycle (add / upsert / remove / release /
  expire / refresh / revoke), conflict detection, and capacity enforcement
  for zone, edge, node-membership and edge-membership cases.
- `robot` — `RobotState`, `RobotProgressState`.
- `coordinator` — per-robot `Coordinator` with rolling-horizon claim
  builder, route → time-window scheduling (`schedule_route_request` →
  `Proceed | Queue | Replan`), right-of-way arbitration, missed-slot
  handling, schedule-window matching.
- `vda` — partial VDA 5050 transport structs (`Order`, `OrderNode`,
  `OrderEdge`, `State`, `Connection`, `Factsheet`, `InstantAction`,
  `Response`) + `Adapter` mapping `RoutePlan` / `RobotState` to VDA.
- `ffi` — full C ABI with opaque handles (`SbWorkspace`,
  `SbWorkspaceIndex`, `SbClaimManager`, `SbCoordinator`) and JSON
  marshaling for complex inputs/outputs. Covers workspace load, route
  planning, claim lifecycle, coordinator scheduling, VDA mapping,
  arbitration, traffic parsing/validation.
- `python` — full PyO3 bindings (gated behind `python` /
  `python-extension` features) exposing every public surface as native
  Python types. `Workspace`, `WorkspaceIndex`, `ClaimManager`,
  `Coordinator` are `#[pyclass]`; data types cross the boundary as native
  `dict` / `list`.

### Architecture

- All long-lived types (`WorkspaceIndex`, `ClaimManager`, `Coordinator`)
  hold `Arc<Workspace>` / `Arc<WorkspaceIndex>` ownership rather than a
  borrow lifetime — required for FFI/Python wrapping. Per-call cost is
  identical to a borrow (Arc deref ≡ `&` deref); only one heap allocation
  per workspace.

### Tests

- 16 lib unit tests in `core::ids`, `policy`, `ffi`.
- 7 integration suites in `tests/`: `index`, `route`, `claim_manager`,
  `coordinator`, `vda`, `ffi` — 31 integration tests.
- 3 Rust examples: `simple`, `route_planning`, `claim_lifecycle`.
- C ABI demo: `examples/c_abi/demo.c` (links against `libsyncbot.so`).
- Python smoke script: `examples/python_binding/example.py` (built via
  `maturin develop --features python-extension`).

### Tooling

- `cargo clippy --all-targets -- -D warnings` clean.
- `cargo check --features python` clean.
- `make build`, `make test`, `make run EXAMPLE=route_planning` all green.
