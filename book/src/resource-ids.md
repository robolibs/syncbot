# Resource IDs (UUID or integer)

Every workspace resource has a **UUID** — its canonical identity. But many
integrators (PLCs, legacy controllers) address resources by small **integers**.
syncbot accepts both, on every transport, anywhere a `resource_id` appears.

## The numeric alias

A zone, node, or edge gets an integer alias by carrying a workspace property:

```text
external.numeric_id = "205"
```

When the `WorkspaceIndex` is built, it scans these and builds a reverse lookup
(`numeric_id → UUID`). Duplicate numeric IDs are surfaced as validation issues
(`category = "duplicate_numeric_id"`), just like duplicate UUIDs.

The UUID stays canonical. The integer is a convenience alias the edge of the
system resolves before any core call runs.

## On the wire

A `resource_id` is encoded as **text** in both JSON and XML, and accepts either
form:

```json
{ "kind": "Zone", "resource_id": "00000000-0000-0000-0000-000000000001" }
{ "kind": "Zone", "resource_id": "205" }
```

```xml
<resource_id>205</resource_id>
<resource_id>00000000-0000-0000-0000-000000000001</resource_id>
```

> **Numeric IDs are quoted strings in JSON** (`"205"`, not `205`). This keeps the
> form identical across JSON and XML — XML has no native number type, so a single
> text representation serialises and parses the same way on both. The deserialiser
> tries UUID first, then falls back to an unsigned integer.

## How resolution works

`ResourceRef` is the wire type (`Uuid | Numeric(u64)`). The transport adapter
turns each wire target into a core `ClaimTarget` before handing it to the
`ClaimManager`:

```rust
ResourceRef::Numeric(205).resolve_zone(&index)  // -> Some(uuid) | None
ResourceRef::Uuid(u).resolve_zone(&index)        // -> Some(u) if present
```

The `*Wire` request types (`ClaimRequestWire`, `PlanRouteRequest`,
`HeartbeatRequest`) carry `ResourceRef`; their `into_request` / `into_target`
methods do the resolution and return an `ApiError` naming the offending value if
an ID is unknown.

The core types (`ClaimTarget`, `RoutePlan`, …) only ever hold UUIDs. Integers
live at the wire boundary and nowhere deeper.
