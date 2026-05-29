# REST / XML

The same HTTP API as [REST / JSON](./rest-json.md), with XML request and response
bodies. Built for PLCs and XML-native controllers that can't easily emit JSON.

- **Feature:** `xmlt` (XML transport)
- **Module:** `timenav::wire::xmlt`
- **Prefix:** `/ares/v1`
- **Content type:** `application/xml`
- **Encoder:** [quick-xml](https://github.com/tafia/quick-xml) over the existing serde derives

Same paths, same fields, same semantics as JSON — only the encoding differs. The
two routers are independent; mount whichever you need (or both, on separate
ports), sharing one `ServeState`.

## Enable and serve

```toml
[dependencies]
timenav = { version = "0.0.1", features = ["xmlt"] }
```

```rust
use timenav::wire::{ServeState, xmlt};

let app = xmlt::router(state);     // same route table as rest::router
let listener = tokio::net::TcpListener::bind("127.0.0.1:8081").await?;
axum::serve(listener, app).await?;
```

The XML router defines the same routes as REST/JSON — see the
[endpoint list](./rest-json.md#endpoints).

## How the body maps

quick-xml serialises straight off the same structs. The element name is the
struct name; fields are child elements; a `Vec` repeats its field element. So the
JSON claim body becomes:

```xml
<ClaimRequestWire>
  <id>1</id>
  <robot_id>1</robot_id>
  <mission_id>0</mission_id>
  <access_mode>Exclusive</access_mode>
  <priority>0</priority>
  <targets>
    <kind>Zone</kind>
    <resource_id>205</resource_id>
  </targets>
</ClaimRequestWire>
```

Two targets repeat the `<targets>` element:

```xml
<targets><kind>Zone</kind><resource_id>205</resource_id></targets>
<targets><kind>Node</kind><resource_id>139</resource_id></targets>
```

## Worked example: claim a zone

```sh
curl -X POST http://127.0.0.1:8081/ares/v1/claims/evaluate \
     -H "Content-Type: application/xml" \
     --data-binary @claim.xml
```

Response:

```xml
<ClaimEvaluation>
  <decision>Grant</decision>
  <reason></reason>
</ClaimEvaluation>
```

…or `<decision>Deny</decision>` with a `<reason>` and the blocking target.

## Design notes

These two points come from making serde round-trip cleanly through a text-only
format:

- **`resource_id` is text either way.** XML has no number type, so a single text
  representation is used for both UUID and integer forms — and JSON matches it by
  quoting numeric IDs. See [Resource IDs](../resource-ids.md).
- **Optional fields are omitted, not null.** `ClaimWindow`'s `start_tick` /
  `end_tick` and other `Option` fields use `skip_serializing_if`, so an absent
  value is a missing element rather than `<start_tick/>`, which round-trips back
  to `None`.

## Implementation

`xmlt::Xml<T>` is the axum extractor/responder: it reads the body as UTF-8 and
runs `quick_xml::de::from_str`, and on the way out serialises with
`quick_xml::se::to_string` and sets `Content-Type: application/xml`. The route
handlers are otherwise identical to the JSON ones.
