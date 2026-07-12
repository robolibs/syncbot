#let accent = rgb("#2563eb")
#let dark = rgb("#0f172a")
#let muted = rgb("#64748b")
#let pale = rgb("#eff6ff")
#let warn = rgb("#f59e0b")

#set document(title: "ARES Protocol — Quick Start", author: "robolibs / ARES")
#set page(
  paper: "a4",
  margin: (x: 18mm, y: 17mm),
  header: align(right, text(size: 8pt, fill: muted)[ARES protocol — quick start]),
  footer: context align(center, text(size: 8pt, fill: muted)[page #counter(page).display("1")]),
)
#set text(size: 9.5pt, fill: dark)
#set heading(numbering: "1.1")
#set par(justify: true, leading: 0.62em)
#show heading: it => [ #v(0.6em) #it #v(0.2em) ]
#show raw.where(block: true): it => block(
  fill: rgb("#f8fafc"), stroke: rgb("#e2e8f0"), radius: 4pt, inset: 6pt, width: 100%, it,
)
#show raw.where(block: false): it => box(
  fill: rgb("#f8fafc"), stroke: rgb("#dbe3ef"), radius: 2pt,
  inset: (x: 2.5pt, y: 1pt), outset: (y: 0.5pt), it,
)
#let tag(body, color: accent) = box(
  fill: color.lighten(78%), stroke: color.lighten(25%), radius: 3pt,
  inset: (x: 5pt, y: 2pt), text(size: 7.5pt, weight: "bold", fill: color.darken(20%), body),
)

#align(center)[
  #text(size: 24pt, weight: "bold", fill: accent)[ARES Protocol — Quick Start]
  #v(0.4em)
  #text(size: 11pt, fill: muted)[How a robot connects to ARES: register, heartbeat, claim, release]
  #v(1em)
  #tag[REST/XML] #h(0.5em) #tag[ROS2DDS]
]

#v(1em)
#block(fill: pale, stroke: accent.lighten(45%), radius: 6pt, inset: 9pt)[
  A robot does exactly four things: *register* once, *heartbeat* while it runs,
  *claim* the space it needs, and *release* when done. Each call carries a *key*
  and gets back a tiny *decision + reason*. That is the whole protocol — this
  page is all most integrations need.

  REST/XML and ROS2DDS are shown here. JSON, native Zenoh, route planning,
  scheduling, snapshots and the fine-level "tier-2" calls live in
  *`PROTOCOL-advanced.typ`*.
]

= Connect

#table(
  columns: (30mm, 1fr),
  inset: 6pt, stroke: rgb("#e2e8f0"),
  table.header([Transport], [Address]),
  [REST / XML], [`http://<host>:8080/ares/v1/...` — send/accept `application/xml`.],
  [ROS2DDS], [ROS2 service `/ares/v1/...`, type `ares_interfaces/srv/Json` (a JSON string in, a JSON string out), via `zenoh-bridge-ros2dds`.],
)

Check the server is up (no key needed):

```sh
curl http://<host>:8080/ares/v1/health
# {"status":"ok","version":"0.1.0"}
```

= The key

Each call may carry a `key`. Two forms:

#table(
  columns: (34mm, 1fr),
  inset: 5pt, stroke: rgb("#e2e8f0"),
  table.header([Key], [Meaning]),
  [an integer, e.g. `1234`], [a simple numeric password],
  [`did:pass=<secret>`], [a password in DID form],
)

The key is set at *register* and bound to the robot. Every later call resends it;
a wrong key is rejected with reason `1` (*mismatched key*).

*The key is optional.* If you omit it the server uses a shared default password
— convenient but insecure (anyone can act as a robot that skipped the key). If a
robot registers *with* a key, it must keep sending that same key.

= The reply

Every call answers with the same two fields:

- `decision` — `1` = ok / granted, `0` = denied.
- `reason` — `0` = ok and `1` = mismatched key on *every* call; other values are
  listed per call below.

In XML the reply root is always `<reply>`. (On input the request's outer tag
name is ignored — only the inner fields matter.)

= The four calls

Each call shows the XML body and the equivalent ROS2 service call. The robot id
may be an *integer or a UUID string*.

== Register

Bind a robot id to a key. Do this once. Optional `<alive>` is the heartbeat
interval in seconds (default 2); if no heartbeat arrives for `2×` that, the
server marks the robot inactive *and auto-releases all its claims* (so a crashed
robot never leaves a zone stuck).

```xml
POST /ares/v1/robots
  <reg><robot>7</robot><key>1234</key><alive>2</alive></reg>
  <reply><decision>1</decision><reason>0</reason></reply>
```

```sh
ros2 service call /ares/v1/robots/register ares_interfaces/srv/Json \
  "{request: '{\"robot\":\"7\",\"key\":\"1234\",\"alive\":2}'}"
# response.success=true  response.response='{"decision":1,"reason":0}'
```

#table(columns: (14mm, 1fr), inset: 4pt, stroke: rgb("#e2e8f0"),
  table.header([reason], [meaning]),
  [0], [registered], [1], [mismatched key], [2], [already registered],
  [3], [bad id (not an integer or UUID)], [4], [unsupported key scheme],
)

== Heartbeat

Liveness + where the robot is. The reply is just an ack; the server timestamps
it. Send one of `zone` / `node` / `edge`. Use `zone = -1` when the robot holds
no zone and its location is unknown.

```xml
POST /ares/v1/robots/7/heartbeat
  <hb><key>1234</key><zone>42</zone></hb>     <!-- or <zone>-1</zone> if unknown -->
  <reply><decision>1</decision><reason>0</reason></reply>
```

```sh
ros2 service call /ares/v1/robots/heartbeat ares_interfaces/srv/Json \
  "{request: '{\"robot\":\"7\",\"key\":\"1234\",\"zone\":42}'}"   # or "zone":-1 if unknown
# response.success=true  response.response='{"decision":1,"reason":0}'
```

#table(columns: (14mm, 1fr), inset: 4pt, stroke: rgb("#e2e8f0"),
  table.header([reason], [meaning]),
  [0], [ok], [1], [mismatched key], [2], [not registered],
)

== Claim

Reserve a zone (or node/edge — the type is in the path). To reserve several at
once, repeat `<id>`; it is *all-or-nothing*. On denial, `<blocked>` names the id
that stopped it. Two optional fields:

#table(columns: (24mm, 1fr), inset: 4pt, stroke: rgb("#e2e8f0"),
  table.header([Field], [Meaning]),
  [`<access_mode>`], [`0` = unspecified (→ exclusive), `1` = exclusive (default), `2` = shared; `3`+ reserved and rejected.],
  [`<lease_time>`], [seconds the claim should hold: `0` (default) = unlimited, `X` = X seconds.],
)

*Shared zones.* On a zone whose capacity is greater than 1, several robots may
hold it at once with `access_mode=2` (shared), up to that capacity; once full, a
further shared claim is denied with reason `3` (*capacity exceeded*). An
*exclusive* claim (`access_mode=1`) on a shared zone still *conflicts* with the
current holders (reason `2`).

*Lease time, today.* `<lease_time>` is accepted but timed wall-clock expiry is
not enforced yet: only `0` (unlimited) is currently effective, and a non-zero
value is accepted but the claim is *not* auto-expired on a timer today. The
expiry that does work is auto-release when a robot stops heartbeating (after
`2×` its `alive` interval).

```xml
POST /ares/v1/claims/zone
  <claim><key>1234</key><robot>7</robot><id>42</id></claim>
  <reply><decision>1</decision><reason>0</reason></reply>

  <!-- several zones, atomic, exclusive, 30-second lease -->
  <claim><key>1234</key><robot>7</robot><id>42</id><id>43</id>
         <access_mode>1</access_mode><lease_time>30</lease_time></claim>
  <reply><decision>0</decision><reason>2</reason><blocked>43</blocked></reply>
```

```sh
ros2 service call /ares/v1/claims/zone ares_interfaces/srv/Json \
  "{request: '{\"key\":\"1234\",\"robot\":\"7\",\"id\":[42]}'}"
# response.success=true  response.response='{"decision":1,"reason":0}'

# several zones, atomic, exclusive, 30-second lease — id is an ARRAY even for one
ros2 service call /ares/v1/claims/zone ares_interfaces/srv/Json \
  "{request: '{\"key\":\"1234\",\"robot\":\"7\",\"id\":[42,43],\"access_mode\":1,\"lease_time\":30}'}"
# response.success=true  response.response='{"decision":0,"reason":2,"blocked":43}'
```

#table(columns: (14mm, 1fr), inset: 4pt, stroke: rgb("#e2e8f0"),
  table.header([reason], [meaning]),
  [0], [granted], [1], [mismatched key], [2], [conflict — someone holds it],
  [3], [capacity exceeded (shared zone full)], [4], [unknown id],
  [5], [bad request (no id, or unsupported `access_mode`)],
)

Claiming a zone reserves everything inside it — so a zone claim and a claim on a
node/edge within that zone conflict with each other.

== Release

Give back what this robot holds on a resource.

```xml
POST /ares/v1/leases/release/zone
  <rel><key>1234</key><robot>7</robot><id>42</id></rel>
  <reply><decision>1</decision><reason>0</reason></reply>
```

```sh
ros2 service call /ares/v1/leases/release/zone ares_interfaces/srv/Json \
  "{request: '{\"key\":\"1234\",\"robot\":\"7\",\"id\":42}'}"
# response.success=true  response.response='{"decision":1,"reason":0}'
```

#table(columns: (14mm, 1fr), inset: 4pt, stroke: rgb("#e2e8f0"),
  table.header([reason], [meaning]),
  [0], [released], [1], [mismatched key], [2], [no such lease (robot holds nothing there)],
  [3], [unknown id],
)

= A full run

```sh
B=http://<host>:8080/ares/v1
xml() { curl -s -X POST "$B$1" -H 'content-type: application/xml' -d "$2"; echo; }

xml /robots            '<reg><robot>7</robot><key>1234</key></reg>'
xml /claims/zone       '<claim><key>1234</key><robot>7</robot><id>42</id></claim>'
xml /robots/7/heartbeat '<hb><key>1234</key><zone>42</zone></hb>'
xml /leases/release/zone '<rel><key>1234</key><robot>7</robot><id>42</id></rel>'
```

The same run over ROS2 — register, claim, heartbeat, release:

```sh
J=ares_interfaces/srv/Json

ros2 service call /ares/v1/robots/register     $J "{request: '{\"robot\":\"7\",\"key\":\"1234\"}'}"
ros2 service call /ares/v1/claims/zone         $J "{request: '{\"key\":\"1234\",\"robot\":\"7\",\"id\":[42]}'}"
ros2 service call /ares/v1/robots/heartbeat    $J "{request: '{\"robot\":\"7\",\"key\":\"1234\",\"zone\":42}'}"
ros2 service call /ares/v1/leases/release/zone $J "{request: '{\"key\":\"1234\",\"robot\":\"7\",\"id\":42}'}"
```

#block(fill: rgb("#fffbeb"), stroke: warn.lighten(20%), radius: 5pt, inset: 8pt)[
  *JSON note.* On JSON the claim `id` is an array — `{"id":[42]}` for one,
  `{"id":[42,43]}` for several. (XML just repeats `<id>`.) Numeric `robot`/`key`
  may be quoted strings. See `PROTOCOL-advanced.typ` for the JSON transport.
]
