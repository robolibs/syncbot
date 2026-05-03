# timenav

Rust port of the C++ [`timenav`](https://github.com/robolibs/timenav) library
— multi-robot navigation, claims, and scheduling on top of `zoneout`.

## Build

```sh
make build          # cargo build --lib --examples
make test           # cargo test --all-targets
make run EXAMPLE=route_planning
```

## Status

Work in progress — see [`PLAN.md`](./PLAN.md) for the conversion roadmap.

## Dependencies

| crate     | role                                                  |
|-----------|-------------------------------------------------------|
| `zoneout` | sibling Rust port — `Workspace`, `Zone`, graph        |
| `datapod` | POD geometry + `OMap`                                 |
| `concord` | coordinate transforms (WGS ↔ ENU)                     |
| `graphix` | vertex graph types reused via `zoneout::Workspace`    |
