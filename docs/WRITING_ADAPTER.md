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
| `ares/v1/claims/route` | `ares.v1.claim.route` | `ares.v1.reply` |
| `ares/v1/leases/release/zone` | `ares.v1.release` | `ares.v1.reply` |
| `ares/v1/leases/release/node` | `ares.v1.release` | `ares.v1.reply` |
| `ares/v1/leases/release/edge` | `ares.v1.release` | `ares.v1.reply` |
| `ares/v1/routes/plan` | `ares.v1.route.plan` | `ares.v1.read.reply` |
| `ares/v1/zones/list` | `ares.v1.read.empty` | `ares.v1.read.reply` |
| `ares/v1/zones/get` | `ares.v1.resource.get` | `ares.v1.read.reply` |
| `ares/v1/fleet/snapshot` | `ares.v1.read.empty` | `ares.v1.read.reply` |
| `ares/v1/workspace/set` | `ares.v1.workspace.chunk` | `ares.v1.read.reply` |
| `ares/v1/health` | `ares.v1.read.empty` | `ares.v1.read.reply` |

The outer peerbus request and response type is `peerbus::DatapodMsg`. Its
`type_hash` identifies the inner canonical datapod and its `wire` is the
datapod little-endian `header || payload`. This type-erased envelope is what
lets Rust, C, and Python clients use the same service.

## Canonical schema

The source of truth is `src/wire/peerbus.rs`. String fields are UTF-8 payload
sections. `id[]` is a payload section of little-endian `u64` values.

Fields are listed in declaration order. `_pad` bytes are explicit in the
struct and are always zero. "Header bytes" is the fixed part, measured with
empty payload sections — an out-of-process adapter packing these by hand must
match it exactly, because datapod identity is size+alignment hashed.

| Canonical name | Fields | Header bytes |
|---|---|---|
| `ares.v1.register` | `alive: u64`, `has_alive: u8`, `_pad: [u8;7]`, `robot: bytes`, `key: bytes` | 32 |
| `ares.v1.heartbeat` | `zone: i64`, `node: u64`, `edge: u64`, `pos_a: f64`, `pos_b: f64`, `pos_c: f64`, `yaw: f64`, `has_zone: u8`, `has_node: u8`, `has_edge: u8`, `pos_frame: u8`, `has_yaw: u8`, `_pad: [u8;3]`, `robot: bytes`, `key: bytes` | 80 |
| `ares.v1.claim` | `lease_time: u64`, `access_mode: u8`, `has_access_mode: u8`, `has_lease_time: u8`, `_pad: [u8;5]`, `robot: bytes`, `key: bytes`, `id: u64[]` | 40 |
| `ares.v1.claim.route` | `lease_time: u64`, `access_mode: u8`, `has_access_mode: u8`, `has_lease_time: u8`, `_pad: [u8;5]`, `robot: bytes`, `key: bytes`, `node: u64[]`, `edge: u64[]` | 48 |
| `ares.v1.release` | `id: u64`, `robot: bytes`, `key: bytes` | 24 |
| `ares.v1.reply` | `blocked: u64`, `decision: u8`, `reason: u8`, `has_blocked: u8`, `_pad: [u8;5]` | 16 |
| `ares.v1.read.empty` | `_reserved: u8` | — |
| `ares.v1.resource.get` | `id: bytes` containing a UTF-8 UUID or numeric alias | — |
| `ares.v1.route.plan` | `use_penalties: u8`, `_pad: [u8;7]`, `start: bytes`, `goal: bytes` (each a UTF-8 UUID or numeric alias) | 24 |
| `ares.v1.read.reply` | `success: u8`, `body: bytes` containing the versioned ARES JSON read model or UTF-8 error text | — |
| `ares.v1.workspace.chunk` | `transfer: u64`, `index: u32`, `total: u32`, `body: bytes`, `key: bytes` | 32 |

Empty `key` means the flat protocol's shared default key (`"0"`). Presence
flags distinguish an omitted optional value from numeric zero.

`pos_frame` says which frame `pos_a`/`pos_b`/`pos_c` are in:

| Value | Meaning | `pos_a` | `pos_b` | `pos_c` |
|---|---|---|---|---|
| `0` | no position reported | — | — | — |
| `1` | global (WGS84) | `lat` | `lon` | `alt` |
| `2` | local, metres from the datum | `x` east | `y` north | `z` up |

`yaw` is REP-103: radians counter-clockwise from east, valid only when
`has_yaw` is set. A robot sends one frame or neither, never both.

## Claiming a route

`ares/v1/claims/route` takes the nodes a robot stops at and the edges it
crosses as one atomic request: all of it is granted or none of it is. The
single-kind endpoints cannot express this — a route spans both kinds, and
claiming them separately leaves a robot holding half a path when the second
call is denied. Either section may be empty; both empty is `BAD_REQUEST`.

The zones a route passes through are **not** claimed. The core derives
*intent* on every containing zone and its ancestors from the node and edge
targets, which is what makes these two things different:

- a claim on a **zone** reserves the whole area — it conflicts with anything
  inside it, and with any route through it;
- a claim on a **route** reserves only the nodes and edges used — it blocks a
  claim on the surrounding zone, but a second, non-interfering route through
  that same zone still proceeds.

How many robots may be inside one zone at once comes from the zone's own
policy: `traffic.policy=exclusive` admits one, `traffic.capacity=N` admits N,
and an unconstrained zone admits any number, with conflicts then decided on
the nodes and edges actually shared. Occupancy counts robots, not claims, so
one robot holding several routes in a zone still fills one slot.

One claim may name at most **64** resources. Evaluation is quadratic in that
count and runs under the coordinator's write lock, so the cap keeps one client
from stalling every other robot; a rolling horizon never approaches it. Past
the cap the claim is refused with `BAD_REQUEST`.

`lease_time` is in seconds of wall clock. A claim carrying a non-zero one is
dropped that many seconds after it is granted — the core expires lapsed claims
both on the periodic sweep and before evaluating any new claim, so an adapter
never has to poll for the release. `0` (or an absent value) holds the claim
until it is released or its robot stops heartbeating.

The core caps each local ARES peerbus payload at 1 MiB. This is deliberately
smaller than peerbus's general-purpose 16 MiB default: coordination messages
are small, and every req/res topic owns bounded SHM rings.

## Pushing a workspace

**Replacing the workspace needs an operator key, and is refused outright when
none is configured.** This is deliberately *not* a robot key: a robot key
authorises claiming one zone, never redrawing the map every robot is claiming
against. The key rides on every chunk (`key` section) so the core can refuse a
transfer before buffering any of it; over HTTP the adapter takes it from the
`x-ares-admin-key` header, leaving the body as the caller's document. Operators
set it with `SYNCBOT_ADMIN_KEY`.

Claims survive a swap — they are keyed by resource uuid, and the usual push is
an edit where dropping the fleet's claims would be the greater harm. A claim
whose resource no longer exists simply stops resolving, so the reply reports
`stale_claims`: how many live claims the new workspace orphaned.

The zone tree of a pushed document may nest at most 64 deep. It arrives
untrusted and gets walked, and the walks are iterative precisely so a hostile
document cannot overflow the stack — the depth cap is the belt to that braces.

`ares/v1/workspace/set` carries a serialized `zoneout::WorkspaceJson` split
across `ares.v1.workspace.chunk` messages, because a workspace is unbounded
while a peerbus payload is not. The client picks a `transfer` id unique among
its in-flight pushes, sends `total` chunks of at most 512 KiB each, and the
core applies the document only once it holds them all — so a push that dies
partway leaves the previous workspace serving. Every chunk but the last
replies `null`; the last replies the accepted-workspace read model. The core
refuses a transfer that exceeds 256 MiB buffered, that changes its chunk
count mid-push, or that indexes past `total`.

## Keeping a hand-packed adapter honest

The header sizes above are asserted by `canonical_header_sizes_are_frozen` in
`src/wire/peerbus.rs`, so adding a field to a canonical struct fails the Rust
test suite with a message naming what else to update.
`examples/python_adapter/adapter.py --self-check` asserts the same numbers
from the adapter side, and runs without the peerbus bindings installed.

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

## Trust boundary

**The host is the trust boundary.** Any process that can open the shared-memory
segment can be an adapter, and an adapter can speak every operation here —
including workspace replacement, if it holds the operator key. There is no
per-adapter identity check and no ACL.

That is a deliberate position, not an oversight, and it has a consequence worth
stating plainly: the key checks on the flat protocol authenticate *robots to
the core*, not *adapters to the core*. An adapter is trusted the moment it can
reach the bus. Do not read the reason-1 (`mismatched key`) path as protection
against a hostile local process — it is not, and was never meant to be. Run the
core where you would run anything else holding fleet control.

## Keys

A robot presents its key as a scalar, in one of three forms:

| Presented | Meaning |
|---|---|
| `1234` | a numeric password |
| `pass:<secret>` | a password; anything after the prefix, verbatim |
| `did:key:<multibase>` | an Ed25519 identity — parsed, not yet accepted |

DID forms are parsed by `authbox`, the crate that owns DID syntax. A password
is deliberately *not* a DID: an earlier form spelled it `did:pass=<secret>`,
which is not valid DID syntax at all (DIDs separate on colons), and dressing a
secret as an identifier restricted it to the DID method-id charset — no `!`, no
spaces — for no benefit.

What the core *stores* is never the key — it is `keylock::kdf::pwhash`'s
password hash, a standard PHC string:

```text
$argon2id$v=19$m=19456,t=2,p=1$<salt>$<digest>
```

So a stolen state file costs an offline Argon2 attack per guess rather than
handing over the fleet. The salt is per key, so two robots sharing a password
do not share a digest, and the cost parameters travel inside the string, so
raising them later does not invalidate keys already stored.

Two consequences an adapter author should know:

- **The first check of a key is slow on purpose** (~11 ms). Every later check
  of the same secret is a cache hit (~300 ns), so a heartbeat loop pays it
  once, not once per beat.
- **A wrong key is slow too**, which makes a stream of them a denial of
  service. After a short run of failures the core refuses further derivations
  for that robot until the streak ages out. A robot whose key is already known
  is checked from the cache first, so someone guessing at its id cannot lock
  it out.

`did:key` (Ed25519) is parsed and **rejected** with reason `4`. That is a
missing protocol, not a missing algorithm: proving possession of a private key
needs a fresh challenge to sign, and this wire carries one static scalar per
request, which would replay. It needs a challenge/response op first.

## Registration

`register` binds a key to a robot id, and two operator decisions govern it:

- **Provisioning.** If the operator supplies a set of permitted robot ids, an
  id outside it is refused with reason `5` (not permitted). Without a list,
  registration is first-come — which means whoever asks first owns an id, and
  a robot that boots late finds its id taken with a key it does not know.
- **Keyless registration.** A robot that sends no key is bound to the shared
  default (`"0"`), which every other keyless robot also holds; anyone can then
  act as any of them. It is refused with reason `5` unless the operator enables
  it (`SYNCBOT_ALLOW_DEFAULT_KEY`).

Registration never expires — a robot that goes quiet keeps its id, because it
is allowed to come back — so the total is capped instead. Past the cap,
registration is refused with reason `6` (fleet full).

## Compatibility rules

- Do not rename a canonical datapod or reorder/change its header fields in v1.
- Add a new canonical name/topic for an incompatible schema revision.
- Every process must use the same peerbus version. Peerbus is pre-1.0 and its
  transport handshake is not stable between versions.
- The core uses the type hash to reject a request encoded with the wrong inner
  datapod schema.
- peerbus topic names are limited to `[A-Za-z0-9._/-]+` and 200 bytes.
