# Writing an ARES adapter

An adapter has exactly two sides:

1. its own external wire (HTTP/JSON, XML, Zenoh, DDS, a serial line, ...);
2. a peerbus `DatapodMsg` req/res client connected to the ARES core.

It does not link `syncbot`, receive a `Coordinator`, or call a `flat_*`
function. Adding an adapter therefore does not rebuild the core.

## Frozen topics

| Topic | Inner request datapod | Inner reply datapod |
|---|---|---|
| `ares/v1/robots/register` | `ares.v1.register` | `ares.v1.reply` |
| `ares/v1/robots/heartbeat` | `ares.v1.heartbeat` | `ares.v1.reply` |
| `ares/v1/claims/zone` | `ares.v1.claim` | `ares.v1.reply` |
| `ares/v1/claims/node` | `ares.v1.claim` | `ares.v1.reply` |
| `ares/v1/claims/edge` | `ares.v1.claim` | `ares.v1.reply` |
| `ares/v1/leases/release/zone` | `ares.v1.release` | `ares.v1.reply` |
| `ares/v1/leases/release/node` | `ares.v1.release` | `ares.v1.reply` |
| `ares/v1/leases/release/edge` | `ares.v1.release` | `ares.v1.reply` |
| `ares/v1/zones/list` | `ares.v1.read.empty` | `ares.v1.read.reply` |
| `ares/v1/zones/get` | `ares.v1.resource.get` | `ares.v1.read.reply` |
| `ares/v1/fleet/snapshot` | `ares.v1.read.empty` | `ares.v1.read.reply` |

The outer peerbus request and response type is `peerbus::DatapodMsg`. Its
`type_hash` identifies the inner canonical datapod and its `wire` is the
datapod little-endian `header || payload`. This type-erased envelope is what
lets Rust, C, and Python clients use the same service.

## Canonical schema

The source of truth is `src/wire/peerbus.rs`. String fields are UTF-8 payload
sections. `id[]` is a payload section of little-endian `u64` values.

| Canonical name | Fields |
|---|---|
| `ares.v1.register` | `alive: u64`, `has_alive: u8`, `robot: bytes`, `key: bytes` |
| `ares.v1.heartbeat` | `zone: i64`, `node: u64`, `edge: u64`, three presence flags, `robot: bytes`, `key: bytes` |
| `ares.v1.claim` | `lease_time: u64`, `access_mode: u8`, two presence flags, `robot: bytes`, `key: bytes`, `id: u64[]` |
| `ares.v1.release` | `id: u64`, `robot: bytes`, `key: bytes` |
| `ares.v1.reply` | `blocked: u64`, `decision: u8`, `reason: u8`, `has_blocked: u8` |
| `ares.v1.read.empty` | one reserved zero byte |
| `ares.v1.resource.get` | `id: bytes` containing a UTF-8 UUID or numeric alias |
| `ares.v1.read.reply` | `success: u8`, `body: bytes` containing the versioned ARES JSON read model or UTF-8 error text |

Empty `key` means the flat protocol's shared default key (`"0"`). Presence
flags distinguish an omitted optional value from numeric zero. All explicit
padding bytes are zero.

The core caps each local ARES peerbus payload at 1 MiB. This is deliberately
smaller than peerbus's general-purpose 16 MiB default: coordination messages
are small, and every req/res topic owns bounded SHM rings.

## Rust client

Bundled Rust adapters normally use `syncbot::wire::peerbus::Client`:

```rust
use syncbot::wire::peerbus::{Client, CORE_IDENTITY};

let client = Client::connect(CORE_IDENTITY)?;
let reply = client.register("robot-7", "1234", Some(2))?;
assert_eq!(reply.decision, 1);
# Ok::<(), Box<dyn std::error::Error>>(())
```

A standalone Rust adapter can instead use
`peerbus::Node::req_client::<DatapodMsg, DatapodMsg>` and wrap its canonical
request with `DatapodMsg::from_datapod`.

## Python client

[`examples/python_adapter/adapter.py`](../examples/python_adapter/adapter.py)
is a complete out-of-process example. It accepts a toy newline-JSON wire,
packs the canonical datapod bytes with `struct`, and calls:

```python
node = peerbus.Node(identity="my-adapter", no_relay=True)
client = node.datapod_req_client("ares-core", "ares/v1/robots/register")
reply = client.call_wire(datapod.type_hash_name("ares.v1.register"), wire)
```

The example deliberately does not import `syncbot`: it is the living proof
that a new protocol can be attached from another language without changing
the core binary.

## Compatibility rules

- Do not rename a canonical datapod or reorder/change its header fields in v1.
- Add a new canonical name/topic for an incompatible schema revision.
- Every process must use the same peerbus version. Peerbus is pre-1.0 and its
  transport handshake is not stable between versions.
- The core uses the type hash to reject a request encoded with the wrong inner
  datapod schema.
- peerbus topic names are limited to `[A-Za-z0-9._/-]+` and 200 bytes.
