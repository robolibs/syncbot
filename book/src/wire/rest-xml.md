# REST / XML

The same HTTP API as [REST / JSON](./rest-json.md), with XML request and response
bodies. Built for PLCs and XML-native controllers that can't easily emit JSON.

- **Feature:** `xmlt` (XML transport)
- **Module:** `syncbot::wire::xmlt`
- **Prefix:** `/ares/v1`
- **Content type:** `application/xml`
- **Encoder:** [quick-xml](https://github.com/tafia/quick-xml)

Same paths, same fields, same semantics as JSON — only the encoding differs. The
two routers are independent; mount whichever you need (or both, on separate
ports), sharing one `ServeState`. With the `rest` feature, the JSON router also
answers XML when the request sets `Content-Type`/`Accept: application/xml`.

## Enable and serve

```toml
[dependencies]
syncbot = { version = "0.0.2", features = ["xmlt"] }
```

```rust
use syncbot::wire::{ServeState, xmlt};

let app = xmlt::router(state);     // same route table as rest::router
let listener = tokio::net::TcpListener::bind("127.0.0.1:8081").await?;
axum::serve(listener, app).await?;
```

## Flat (tier-1) calls — for PLCs

The four robot-facing flows are **flat**: one level of scalar elements, the
resource *type* in the path, an auth **key** in the body, and a small
`decision`/`reason` reply. No nested structures.

Each call may carry a `<key>` — an integer password or `did:pass=<secret>`. The
key is **optional**: omit it and the server uses a shared default password
(insecure but convenient). Reserved reasons: `0` = OK, `1` = mismatched key.

**Register** — `POST /ares/v1/robots`. The `<robot>` id is an integer **or** a
UUID string (per-robot routes accept either).

```xml
<reg><robot>7</robot><key>1234</key></reg>
<reply><decision>1</decision><reason>0</reason></reply>
```
Reasons: 0 ok · 1 key · 2 already registered · 3 bad id · 4 unsupported key.

**Heartbeat** — `POST /ares/v1/robots/{id}/heartbeat` (liveness + position; ack only)

```xml
<hb><key>1234</key><zone>42</zone></hb>   <!-- or <node>/<edge> -->
<reply><decision>1</decision><reason>0</reason></reply>
```
Reasons: 0 ok · 1 key · 2 not registered.

**Claim** — `POST /ares/v1/claims/{zone,node,edge}` (type in path; repeat `<id>`
for an atomic multi-resource claim). Optional `<AccessMode>` (1 = exclusive
default; 0 = unspecified; 2+ reserved → rejected) and `<LeaseTime>` (minutes;
0 = unlimited).

```xml
<claim><key>1234</key><robot>7</robot><id>42</id><id>43</id>
       <AccessMode>1</AccessMode><LeaseTime>30</LeaseTime></claim>
<reply><decision>0</decision><reason>2</reason><blocked>43</blocked></reply>
```
Reasons: 0 granted · 1 key · 2 conflict · 3 capacity · 4 unknown · 5 bad request
(no id / unsupported AccessMode).
On denial `<blocked>` names the offending id.

**Release** — `POST /ares/v1/leases/release/{zone,node,edge}`

```xml
<rel><key>1234</key><robot>7</robot><id>42</id></rel>
<reply><decision>1</decision><reason>0</reason></reply>
```
Reasons: 0 released · 1 key · 2 no such lease · 3 unknown/bad.

## Tier-2 (fine-level) calls

Richer endpoints (snapshot, route planning, assign-route, schedule, lists) keep
their fuller shapes for graph-aware robots. Read-only endpoints are open;
**state-changing** ones (assign-route, schedule, the nested `/claims` submit)
require a `<key>` element validated against the acting robot. The dry-run
`/claims/evaluate` is read-only and open.

The nested `<ClaimRequestWire>` submit still works for fine-level clients if it
includes a `<key>`:

```xml
<ClaimRequestWire>
  <robot_id>1</robot_id>
  <key>1234</key>
  <access_mode>Exclusive</access_mode>
  <targets><kind>Zone</kind><resource_id>205</resource_id></targets>
</ClaimRequestWire>
```

Two targets repeat the `<targets>` element.

## Design notes

- **Scalars are text either way.** XML has no number type, so ids and keys are
  text; JSON matches by quoting numeric ids/keys. See
  [Resource IDs](../resource-ids.md).
- **Optional fields are omitted, not null** (`skip_serializing_if`), so an
  absent value round-trips back to `None`.
- **Flat scalars over `deserialize_string`.** The flat envelopes coerce a
  string-or-number scalar via a `Visitor`, which quick-xml supports (an
  `#[serde(untagged)]` enum does not).

## Implementation

`xmlt::Xml<T>` is the axum extractor/responder: it reads the body as UTF-8 and
runs `quick_xml::de::from_str`, and on the way out serialises with
`quick_xml::se::to_string`, setting `Content-Type: application/xml`.
