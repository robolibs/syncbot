# Feature flags

All transports are optional. The default build is core only — dependency-light,
no HTTP/Zenoh/SHM.

| Feature            | Pulls in            | Enables                                              |
|--------------------|---------------------|------------------------------------------------------|
| *(default)*        | —                   | core: index, policy, route, claims, coordinator, VDA |
| `rest`             | `axum`, `tokio`     | [REST / JSON](../wire/rest-json.md) adapter          |
| `xmlt`             | `axum`, `quick-xml` | [REST / XML](../wire/rest-xml.md) adapter            |
| `robo`             | `tokio`, `zenoh`    | [Zenoh](../wire/zenoh.md) adapter                    |
| `python`           | `pyo3`              | [Python](../embedding/python.md) bindings            |
| `python-extension` | `pyo3` (extension)  | build as a Python extension module (maturin)         |

The `wire` module is compiled when any of `rest`, `xmlt`, or `robo` is on. The
shared types and `ServeState` live there regardless of which adapter you pick.

## Build lanes

```sh
cargo check                              # core only
cargo check --features rest              # REST / JSON
cargo check --features xmlt              # REST / XML
cargo check --features robo              # Zenoh
cargo check --features "rest robo xmlt"  # all transports
```

## Notes

- `rest` and `xmlt` can be enabled together; they expose the same route table on
  two routers — mount on separate ports or pick one.
- The crate is also a `cdylib`, so the [C ABI](../embedding/c-abi.md) is always
  built.
