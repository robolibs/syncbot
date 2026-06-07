#let accent = rgb("#2563eb")
#let dark = rgb("#0f172a")
#let muted = rgb("#64748b")
#let pale = rgb("#eff6ff")
#let warn = rgb("#f59e0b")

#set document(
  title: "ARES Protocol",
  author: "robolibs / ARES",
)
#set page(
  paper: "a4",
  margin: (x: 18mm, y: 17mm),
  header: align(right, text(size: 8pt, fill: muted)[ARES protocol]),
  footer: context align(center, text(size: 8pt, fill: muted)[page #counter(page).display("1")]),
)
#set text(size: 9.5pt, fill: dark)
#set heading(numbering: "1.1")
#set par(justify: true, leading: 0.62em)
#show heading: it => [
  #v(0.7em)
  #it
  #v(0.25em)
]
#show raw.where(block: true): it => block(
  fill: rgb("#f8fafc"),
  stroke: rgb("#e2e8f0"),
  radius: 4pt,
  inset: 6pt,
  width: 100%,
  it,
)
#show raw.where(block: false): it => box(
  fill: rgb("#f8fafc"),
  stroke: rgb("#dbe3ef"),
  radius: 2pt,
  inset: (x: 2.5pt, y: 1pt),
  outset: (y: 0.5pt),
  it,
)

#let tag(body, color: accent) = box(
  fill: color.lighten(78%),
  stroke: color.lighten(25%),
  radius: 3pt,
  inset: (x: 5pt, y: 2pt),
  text(size: 7.5pt, weight: "bold", fill: color.darken(20%), body),
)

#let call(method, path, body, response) = table(
  columns: (18mm, 45mm, 1fr, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  table.header(
    text(weight: "bold")[Method],
    text(weight: "bold")[Path / Key],
    text(weight: "bold")[Request],
    text(weight: "bold")[Response],
  ),
  method, path, body, response,
)

#align(center)[
  #text(size: 24pt, weight: "bold", fill: accent)[ARES Protocol]

  #v(0.4em)
  #text(size: 11pt, fill: muted)[Connection and operating guide for REST/JSON, XML, Zenoh, and ROS2DDS]

  #v(1em)
  #tag[REST/JSON] #h(0.5em) #tag[XML] #h(0.5em) #tag[Zenoh] #h(0.5em) #tag[ROS2DDS]
]

#v(1em)
#block(fill: pale, stroke: accent.lighten(45%), radius: 6pt, inset: 9pt)[
  This document describes how clients connect to an already-running ARES server.
  It documents the external protocol only: URLs, keys, payloads, and expected responses.
]

#outline(title: "Contents", depth: 2)
#pagebreak()

= Connection points

The operator running ARES should provide these values.

#table(
  columns: (32mm, 55mm, 1fr),
  inset: 6pt,
  stroke: rgb("#e2e8f0"),
  table.header([Name], [Example], [Used for]),
  [`REST_BASE`], [`http://84.22.103.80:8080/ares/v1`], [REST/JSON calls.],
  [`XML_BASE`], [`http://84.22.103.80:8080/ares/v1`], [XML calls, if the XML adapter is exposed.],
  [`ZENO_ENDPOINT`], [`tcp/84.22.103.80:7447`], [Zenoh bridge or peer connection.],
  [`ROS2_SERVICE`], [`/ares/v1/health`], [ARES JSON service through `zenoh-bridge-ros2dds`.],
)

#block(fill: rgb("#fffbeb"), stroke: warn.lighten(20%), radius: 5pt, inset: 8pt)[
  *Zenoh endpoint syntax:* use `tcp/84.22.103.80:7447`. Do not use `tcp://84.22.103.80:7447`.
]

= Shared data rules

== Resource identifiers

Zones, nodes, and edges are UUID-based internally, but client-facing calls can
use either a UUID or a numeric alias. Numeric aliases should be sent as strings
to keep JSON and XML compatible.

```json
"100"
```

```json
"00000000-0000-0000-0000-000000000100"
```

== Common enum values

#table(
  columns: (38mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  table.header([Field], [Allowed values]),
  [Claim target kind], [`Zone`, `Node`, `Edge`],
  [Claim access mode], [`Shared`, `Exclusive`],
  [Robot progress], [`Idle`, `FollowingRoute`, `Waiting`, `Queued`, `Blocked`, `Replanning`],
  [Lease disposition], [`Active`, `Released`, `Expired`, `Revoked`],
)

== Error response

REST and XML API errors return HTTP `400`. JSON error body:

```json
{"message":"what went wrong"}
```

= Operating story

This is the normal ARES flow: check the server, read the map, register the
robot, plan movement, ask for access to a zone, then keep the server updated as
the robot moves.

== Story overview

#table(
  columns: (12mm, 42mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  table.header([Step], [Action], [Why]),
  [1], [Health check], [Confirm that the ARES server is reachable.],
  [2], [Read map resources], [Find zone, node, and edge IDs. XML does not expose map lookup yet, so use REST for this step.],
  [3], [Register robot], [Tell ARES the robot exists and where it starts.],
  [4], [Plan route], [Ask ARES for a graph route between nodes.],
  [5], [Evaluate claim], [Dry-run zone/node/edge access before committing.],
  [6], [Submit claim], [Store the request if ARES grants it.],
  [7], [Move / heartbeat], [Update robot progress while it moves.],
  [8], [Release / cleanup], [Remove claims or release leases when finished.],
  [9], [Snapshot], [Read robots, active claims, and leases in one response.],
)

== Full REST story: robot claims dock A

Set the base URL:

```sh
REST_BASE=http://84.22.103.80:8080/ares/v1
```

1. Check health:

```sh
curl $REST_BASE/health
```

2. Read zones and nodes. In the fixed example, zone `100` is `dock_a`, node
`1001` is the dock entry, node `1003` is the other side of the map.

```sh
curl $REST_BASE/zones
```

```sh
curl $REST_BASE/nodes
```

3. Register robot `1` at node `1001`.

```sh
curl -X POST $REST_BASE/robots -H 'content-type: application/json' -d '{"robot_id":1,"mission_id":9001,"current_node_id":"00000000-0000-0000-0000-000000001001","progress_state":"Idle","updated_at_tick":1}'
```

4. Plan a route from node `1001` to node `1003`.

```sh
curl -X POST $REST_BASE/routes/plan -H 'content-type: application/json' -d '{"start_node_id":"1001","goal_node_id":"1003","use_penalties":false}'
```

5. Dry-run an exclusive claim on zone `100`.

```sh
curl -X POST $REST_BASE/claims/evaluate -H 'content-type: application/json' -d '{"id":10,"robot_id":1,"mission_id":9001,"access_mode":"Exclusive","priority":10,"requested_at_tick":10,"window":{"start_tick":10,"end_tick":100},"targets":[{"kind":"Zone","resource_id":"100"}]}'
```

If the response contains `"decision":"Grant"`, submit the claim.

6. Submit/store the granted claim.

```sh
curl -X POST $REST_BASE/claims -H 'content-type: application/json' -d '{"id":10,"robot_id":1,"mission_id":9001,"access_mode":"Exclusive","priority":10,"requested_at_tick":10,"window":{"start_tick":10,"end_tick":100},"targets":[{"kind":"Zone","resource_id":"100"}]}'
```

7. Confirm the claim is active.

```sh
curl $REST_BASE/claims
```

8. Update robot progress as it moves.

```sh
curl -X POST $REST_BASE/robots/1/heartbeat -H 'content-type: application/json' -d '{"current_node_id":"1002","current_edge_id":null,"updated_at_tick":70}'
```

9. Read the whole fleet state.

```sh
curl $REST_BASE/fleet/snapshot
```

10. When done, remove the active claim.

```sh
curl -X DELETE $REST_BASE/claims/10
```

== Contention story: second robot is denied until release

Robot `2` tries to claim the same exclusive zone while robot `1` still owns
claim `10`.

```sh
curl -X POST $REST_BASE/robots -H 'content-type: application/json' -d '{"robot_id":2,"mission_id":9002,"current_node_id":"00000000-0000-0000-0000-000000001003","progress_state":"Idle","updated_at_tick":1}'
```

```sh
curl -X POST $REST_BASE/claims/evaluate -H 'content-type: application/json' -d '{"id":11,"robot_id":2,"mission_id":9002,"access_mode":"Exclusive","priority":5,"requested_at_tick":11,"window":{"start_tick":10,"end_tick":100},"targets":[{"kind":"Zone","resource_id":"100"}]}'
```

Expected decision while claim `10` is active:

```json
{"decision":"Deny", "...":"..."}
```

Release robot `1` claim:

```sh
curl -X DELETE $REST_BASE/claims/10
```

Now robot `2` can submit:

```sh
curl -X POST $REST_BASE/claims -H 'content-type: application/json' -d '{"id":11,"robot_id":2,"mission_id":9002,"access_mode":"Exclusive","priority":5,"requested_at_tick":12,"window":{"start_tick":12,"end_tick":100},"targets":[{"kind":"Zone","resource_id":"100"}]}'
```

== Shared-zone story: capacity

For shared zones, more than one robot may be granted if the zone policy allows
capacity. In the fixed example, zone `101` is shared.

```sh
curl -X POST $REST_BASE/claims -H 'content-type: application/json' -d '{"id":20,"robot_id":1,"mission_id":9101,"access_mode":"Shared","priority":1,"requested_at_tick":20,"window":{"start_tick":20,"end_tick":120},"targets":[{"kind":"Zone","resource_id":"101"}]}'
```

```sh
curl -X POST $REST_BASE/claims -H 'content-type: application/json' -d '{"id":21,"robot_id":2,"mission_id":9102,"access_mode":"Shared","priority":1,"requested_at_tick":21,"window":{"start_tick":20,"end_tick":120},"targets":[{"kind":"Zone","resource_id":"101"}]}'
```

If capacity is full, another overlapping claim is denied:

```sh
curl -X POST $REST_BASE/claims/evaluate -H 'content-type: application/json' -d '{"id":22,"robot_id":3,"mission_id":9103,"access_mode":"Shared","priority":1,"requested_at_tick":22,"window":{"start_tick":20,"end_tick":120},"targets":[{"kind":"Zone","resource_id":"101"}]}'
```

== Full XML story: same claim flow

Set:

```sh
XML_BASE=http://84.22.103.80:8080/ares/v1
```

XML has the claim/robot/route/lease calls. For map lookup, use REST first
because XML map lookup is not exposed yet.

1. Check health:

```sh
curl -H 'accept: application/xml' $XML_BASE/health
```

2. Register robot:

```sh
curl -X POST $XML_BASE/robots -H 'content-type: application/xml' -d '<RobotState><robot_id>1</robot_id><mission_id>9001</mission_id><current_node_id>00000000-0000-0000-0000-000000001001</current_node_id><progress_state>Idle</progress_state><updated_at_tick>1</updated_at_tick></RobotState>'
```

3. Plan route:

```sh
curl -X POST $XML_BASE/routes/plan -H 'content-type: application/xml' -d '<PlanRouteRequest><start_node_id>1001</start_node_id><goal_node_id>1003</goal_node_id><use_penalties>false</use_penalties></PlanRouteRequest>'
```

4. Dry-run claim:

```sh
curl -X POST $XML_BASE/claims/evaluate -H 'content-type: application/xml' -d '<ClaimRequestWire><id>10</id><robot_id>1</robot_id><mission_id>9001</mission_id><access_mode>Exclusive</access_mode><priority>10</priority><requested_at_tick>10</requested_at_tick><window><start_tick>10</start_tick><end_tick>100</end_tick></window><targets><kind>Zone</kind><resource_id>100</resource_id></targets></ClaimRequestWire>'
```

5. Submit claim:

```sh
curl -X POST $XML_BASE/claims -H 'content-type: application/xml' -d '<ClaimRequestWire><id>10</id><robot_id>1</robot_id><mission_id>9001</mission_id><access_mode>Exclusive</access_mode><priority>10</priority><requested_at_tick>10</requested_at_tick><window><start_tick>10</start_tick><end_tick>100</end_tick></window><targets><kind>Zone</kind><resource_id>100</resource_id></targets></ClaimRequestWire>'
```

6. Heartbeat:

```sh
curl -X POST $XML_BASE/robots/1/heartbeat -H 'content-type: application/xml' -d '<HeartbeatRequest><current_node_id>1002</current_node_id><updated_at_tick>70</updated_at_tick></HeartbeatRequest>'
```

7. Snapshot:

```sh
curl -H 'accept: application/xml' $XML_BASE/fleet/snapshot
```

8. Add a lease directly if the integration uses leases:

```sh
curl -X POST $XML_BASE/leases -H 'content-type: application/xml' -d '<Lease><id>501</id><claim_id>10</claim_id><robot_id>1</robot_id><access_mode>Exclusive</access_mode><targets><kind>Zone</kind><resource_id>00000000-0000-0000-0000-000000000100</resource_id></targets><granted_at_tick>12</granted_at_tick><expires_at_tick>200</expires_at_tick><disposition>Active</disposition><active>true</active></Lease>'
```

9. Release lease:

```sh
curl -X POST $XML_BASE/leases/release -H 'content-type: application/xml' -d '<ReleaseLeaseRequest><lease_id>501</lease_id><released_at_tick>50</released_at_tick></ReleaseLeaseRequest>'
```

= REST / JSON protocol

Set the base URL:

```sh
REST_BASE=http://84.22.103.80:8080/ares/v1
```

All REST request bodies are JSON. All successful responses are JSON.

== Health

#call([GET], [`/health`], [none], [`Health`])

```sh
curl $REST_BASE/health
```

```json
{"status":"ok","version":"0.0.2"}
```

== Map lookup

#table(
  columns: (20mm, 45mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  table.header([Method], [Path], [Meaning]),
  [GET], [`/zones`], [List zones.],
  [GET], [`/zones/{id}`], [Get zone by UUID or numeric alias.],
  [GET], [`/nodes`], [List graph nodes.],
  [GET], [`/nodes/{id}`], [Get node by UUID or numeric alias.],
  [GET], [`/edges`], [List graph edges.],
  [GET], [`/edges/{id}`], [Get edge by UUID or numeric alias.],
)

```sh
curl $REST_BASE/zones
curl $REST_BASE/zones/100
curl $REST_BASE/nodes
curl $REST_BASE/nodes/1001
curl $REST_BASE/edges
curl $REST_BASE/edges/2001
```

Zone shape:

```json
{
  "id": "00000000-0000-0000-0000-000000000100",
  "numeric_id": 100,
  "name": "dock_a",
  "kind": "zone",
  "parent_id": "00000000-0000-0000-0000-000000000001",
  "child_ids": [],
  "node_ids": [],
  "properties": {"traffic.policy": "exclusive"}
}
```

== Route planning

#call([POST], [`/routes/plan`], [`PlanRouteRequest`], [`PlanRouteResponse`])

```sh
curl -X POST $REST_BASE/routes/plan -H 'content-type: application/json' -d '{"start_node_id":"1001","goal_node_id":"1003","use_penalties":false}'
```

```json
{
  "start_node_id": "1001",
  "goal_node_id": "1003",
  "use_penalties": false
}
```

Response fields:

#table(
  columns: (35mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  [`found`], [Whether a route was found.],
  [`distance`], [Graph distance / cost.],
  [`plan`], [Route plan if found.],
  [`failure`], [Failure diagnostics if not found.],
)

== Robots

#table(
  columns: (20mm, 52mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  table.header([Method], [Path], [Meaning]),
  [GET], [`/robots`], [List registered robots.],
  [POST], [`/robots`], [Register or replace robot state.],
  [GET], [`/robots/{id}`], [Get robot state.],
  [DELETE], [`/robots/{id}`], [Unregister robot.],
  [POST], [`/robots/{id}/heartbeat`], [Update current node/edge and tick.],
  [POST], [`/robots/{id}/route`], [Assign a route plan.],
  [POST], [`/robots/{id}/schedule`], [Schedule from an existing claim.],
)

Register:

```sh
curl -X POST $REST_BASE/robots -H 'content-type: application/json' -d '{"robot_id":1,"mission_id":9001,"current_node_id":"00000000-0000-0000-0000-000000001001","progress_state":"Idle","updated_at_tick":1}'
```

Heartbeat:

```sh
curl -X POST $REST_BASE/robots/1/heartbeat -H 'content-type: application/json' -d '{"current_node_id":"1002","current_edge_id":null,"updated_at_tick":70}'
```

Schedule:

```sh
curl -X POST $REST_BASE/robots/1/schedule -H 'content-type: application/json' -d '{"claim_id":10,"start_tick":20,"ticks_per_cost_unit":1.0,"access_mode":"Exclusive"}'
```

== Claims

#table(
  columns: (20mm, 52mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  table.header([Method], [Path], [Meaning]),
  [GET], [`/claims`], [List active claim requests.],
  [POST], [`/claims/evaluate`], [Dry-run claim; does not store it.],
  [POST], [`/claims`], [Evaluate and store only if granted.],
  [GET], [`/claims/{id}`], [Get active claim request.],
  [DELETE], [`/claims/{id}`], [Remove active claim request.],
)

Submit claim:

```sh
curl -X POST $REST_BASE/claims -H 'content-type: application/json' -d '{"id":10,"robot_id":1,"mission_id":9001,"access_mode":"Exclusive","priority":10,"requested_at_tick":10,"window":{"start_tick":10,"end_tick":100},"targets":[{"kind":"Zone","resource_id":"100"}]}'
```

Claim body:

```json
{
  "id": 10,
  "robot_id": 1,
  "mission_id": 9001,
  "access_mode": "Exclusive",
  "priority": 10,
  "requested_at_tick": 10,
  "window": {"start_tick": 10, "end_tick": 100},
  "targets": [{"kind": "Zone", "resource_id": "100"}]
}
```

== Leases and snapshot

#table(
  columns: (20mm, 52mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  table.header([Method], [Path], [Meaning]),
  [GET], [`/leases`], [List leases.],
  [POST], [`/leases`], [Add lease directly.],
  [POST], [`/leases/release`], [Release lease by body.],
  [DELETE], [`/leases/{id}`], [Release lease by path.],
  [GET], [`/fleet/snapshot`], [Robots, active claim requests, and leases.],
)

```sh
curl $REST_BASE/fleet/snapshot
```

= XML protocol

Set the XML base URL:

```sh
XML_BASE=http://84.22.103.80:8080/ares/v1
```

XML uses the same `/ares/v1` prefix as REST. The server selects XML when:

- a `GET` request sends `Accept: application/xml`
- a request with a body sends `Content-Type: application/xml`

Body requests return XML automatically when their body was XML.

== XML endpoint table

#table(
  columns: (20mm, 52mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  table.header([Method], [Path], [Request root]),
  [GET], [`/health`], [none],
  [GET], [`/fleet/snapshot`], [none],
  [POST], [`/routes/plan`], [`<PlanRouteRequest>`],
  [GET], [`/zones`], [none],
  [GET], [`/zones/{id}`], [none],
  [GET], [`/nodes`], [none],
  [GET], [`/nodes/{id}`], [none],
  [GET], [`/edges`], [none],
  [GET], [`/edges/{id}`], [none],
  [GET], [`/robots`], [none],
  [POST], [`/robots`], [`<RobotState>`],
  [GET], [`/robots/{id}`], [none],
  [DELETE], [`/robots/{id}`], [none],
  [POST], [`/robots/{id}/heartbeat`], [`<HeartbeatRequest>`],
  [POST], [`/robots/{id}/route`], [`<AssignRouteRequest>`],
  [POST], [`/robots/{id}/schedule`], [`<ScheduleRobotRouteRequest>`],
  [GET], [`/claims`], [none],
  [POST], [`/claims`], [`<ClaimRequestWire>`],
  [POST], [`/claims/evaluate`], [`<ClaimRequestWire>`],
  [GET], [`/claims/{id}`], [none],
  [DELETE], [`/claims/{id}`], [none],
  [GET], [`/leases`], [none],
  [POST], [`/leases`], [`<Lease>`],
  [POST], [`/leases/release`], [`<ReleaseLeaseRequest>`],
  [DELETE], [`/leases/{id}`], [none],
)

== XML calls

Health:

```sh
curl -H 'accept: application/xml' $XML_BASE/health
```

Zones:

```sh
curl -H 'accept: application/xml' $XML_BASE/zones
```

Get zone:

```sh
curl -H 'accept: application/xml' $XML_BASE/zones/100
```

Nodes:

```sh
curl -H 'accept: application/xml' $XML_BASE/nodes
```

Edges:

```sh
curl -H 'accept: application/xml' $XML_BASE/edges
```

Route:

```sh
curl -X POST $XML_BASE/routes/plan -H 'content-type: application/xml' -d '<PlanRouteRequest><start_node_id>1001</start_node_id><goal_node_id>1003</goal_node_id><use_penalties>false</use_penalties></PlanRouteRequest>'
```

Register robot:

```sh
curl -X POST $XML_BASE/robots -H 'content-type: application/xml' -d '<RobotState><robot_id>1</robot_id><mission_id>9001</mission_id><current_node_id>00000000-0000-0000-0000-000000001001</current_node_id><progress_state>Idle</progress_state><updated_at_tick>1</updated_at_tick></RobotState>'
```

Heartbeat:

```sh
curl -X POST $XML_BASE/robots/1/heartbeat -H 'content-type: application/xml' -d '<HeartbeatRequest><current_node_id>1002</current_node_id><updated_at_tick>70</updated_at_tick></HeartbeatRequest>'
```

Claim evaluate:

```sh
curl -X POST $XML_BASE/claims/evaluate -H 'content-type: application/xml' -d '<ClaimRequestWire><id>10</id><robot_id>1</robot_id><mission_id>9001</mission_id><access_mode>Exclusive</access_mode><priority>10</priority><requested_at_tick>10</requested_at_tick><window><start_tick>10</start_tick><end_tick>100</end_tick></window><targets><kind>Zone</kind><resource_id>100</resource_id></targets></ClaimRequestWire>'
```

Submit claim:

```sh
curl -X POST $XML_BASE/claims -H 'content-type: application/xml' -d '<ClaimRequestWire><id>10</id><robot_id>1</robot_id><mission_id>9001</mission_id><access_mode>Exclusive</access_mode><priority>10</priority><requested_at_tick>10</requested_at_tick><window><start_tick>10</start_tick><end_tick>100</end_tick></window><targets><kind>Zone</kind><resource_id>100</resource_id></targets></ClaimRequestWire>'
```

List claims:

```sh
curl -H 'accept: application/xml' $XML_BASE/claims
```

Lease release:

```sh
curl -X POST $XML_BASE/leases/release -H 'content-type: application/xml' -d '<ReleaseLeaseRequest><lease_id>501</lease_id><released_at_tick>50</released_at_tick></ReleaseLeaseRequest>'
```

= Zenoh and ROS2DDS

The ARES server stays pure Zenoh. It does not link ROS2. ROS2 machines call it
through `zenoh-bridge-ros2dds`, which translates a ROS2 service request into a
Zenoh query.

== ROS2 service catalog

This is the complete ROS2DDS service surface currently exposed by ARES.

#table(
  columns: (42mm, 38mm, 42mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  table.header([ROS2 service], [ROS2 type], [Zenoh key], [Meaning]),
  [`/ares/v1/health`], [`ares_interfaces/srv/Json`], [`ares/v1/health`], [Health/version as JSON.],
  [`/ares/v1/fleet/snapshot`], [`ares_interfaces/srv/Json`], [`ares/v1/fleet/snapshot`], [Robots, claims, and leases.],
  [`/ares/v1/routes/plan`], [`ares_interfaces/srv/Json`], [`ares/v1/routes/plan`], [Plan route from JSON request.],
  [`/ares/v1/robots/register`], [`ares_interfaces/srv/Json`], [`ares/v1/robots/register`], [Register robot from JSON request.],
  [`/ares/v1/robots/list`], [`ares_interfaces/srv/Json`], [`ares/v1/robots/list`], [List robots.],
  [`/ares/v1/robots/heartbeat`], [`ares_interfaces/srv/Json`], [`ares/v1/robots/heartbeat`], [Update robot progress.],
  [`/ares/v1/robots/assign_route`], [`ares_interfaces/srv/Json`], [`ares/v1/robots/assign_route`], [Assign route plan.],
  [`/ares/v1/robots/schedule`], [`ares_interfaces/srv/Json`], [`ares/v1/robots/schedule`], [Schedule robot from claim.],
  [`/ares/v1/claims/list`], [`ares_interfaces/srv/Json`], [`ares/v1/claims/list`], [List active claims.],
  [`/ares/v1/claims/evaluate`], [`ares_interfaces/srv/Json`], [`ares/v1/claims/evaluate`], [Dry-run claim.],
  [`/ares/v1/claims/request`], [`ares_interfaces/srv/Json`], [`ares/v1/claims/request`], [Submit claim if granted.],
  [`/ares/v1/leases/list`], [`ares_interfaces/srv/Json`], [`ares/v1/leases/list`], [List leases.],
  [`/ares/v1/leases/add`], [`ares_interfaces/srv/Json`], [`ares/v1/leases/add`], [Add lease.],
  [`/ares/v1/leases/release`], [`ares_interfaces/srv/Json`], [`ares/v1/leases/release`], [Release lease.],
)

The `/ares/v1/...` services are the real ARES ROS2DDS service surface.

== `ares_interfaces/srv/Json`

The full ARES ROS2 service surface uses one generic JSON service type:

```text
string request
---
bool success
string response
```

The `.srv` file is included in this repository at:

```text
ros/ares_interfaces/srv/Json.srv
```

Rules:

#table(
  columns: (35mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  [`request`], [JSON request object encoded as a string. Use `{}` for no-input services.],
  [`success`], [`true` when the call succeeded; `false` when ARES returned an error.],
  [`response`], [On success: JSON response encoded as a string. On error: error text.],
)

Example call:

```sh
ros2 service call /ares/v1/health ares_interfaces/srv/Json "{request: '{}'}"
```

== Full ROS2 JSON services

Health:

```sh
ros2 service call /ares/v1/health ares_interfaces/srv/Json "{request: '{}'}"
```

Fleet snapshot:

```sh
ros2 service call /ares/v1/fleet/snapshot ares_interfaces/srv/Json "{request: '{}'}"
```

Route planning:

```sh
ros2 service call /ares/v1/routes/plan ares_interfaces/srv/Json "{request: '{\"start_node_id\":\"1001\",\"goal_node_id\":\"1003\",\"use_penalties\":false}'}"
```

Register robot:

```sh
ros2 service call /ares/v1/robots/register ares_interfaces/srv/Json "{request: '{\"robot_id\":1,\"mission_id\":9001,\"current_node_id\":\"00000000-0000-0000-0000-000000001001\",\"progress_state\":\"Idle\",\"updated_at_tick\":1}'}"
```

List robots:

```sh
ros2 service call /ares/v1/robots/list ares_interfaces/srv/Json "{request: '{}'}"
```

Heartbeat:

```sh
ros2 service call /ares/v1/robots/heartbeat ares_interfaces/srv/Json "{request: '{\"robot_id\":1,\"heartbeat\":{\"current_node_id\":\"1002\",\"current_edge_id\":null,\"updated_at_tick\":70}}'}"
```

Assign route:

```sh
ros2 service call /ares/v1/robots/assign_route ares_interfaces/srv/Json "{request: '{\"robot_id\":1,\"assignment\":{\"route_plan\":{\"start_node_id\":\"00000000-0000-0000-0000-000000001001\",\"goal_node_id\":\"00000000-0000-0000-0000-000000001003\",\"steps\":[],\"traversed_node_ids\":[],\"traversed_edge_ids\":[],\"traversed_zone_ids\":[],\"traversed_node_zone_ids\":[],\"traversed_edge_zone_ids\":[],\"total_cost\":0.0},\"horizon\":100,\"updated_at_tick\":10}}'}"
```

Schedule:

```sh
ros2 service call /ares/v1/robots/schedule ares_interfaces/srv/Json "{request: '{\"robot_id\":1,\"schedule\":{\"claim_id\":10,\"start_tick\":20,\"ticks_per_cost_unit\":1.0,\"access_mode\":\"Exclusive\"}}'}"
```

List claims:

```sh
ros2 service call /ares/v1/claims/list ares_interfaces/srv/Json "{request: '{}'}"
```

Evaluate claim:

```sh
ros2 service call /ares/v1/claims/evaluate ares_interfaces/srv/Json "{request: '{\"id\":10,\"robot_id\":1,\"mission_id\":9001,\"access_mode\":\"Exclusive\",\"priority\":10,\"requested_at_tick\":10,\"window\":{\"start_tick\":10,\"end_tick\":100},\"targets\":[{\"kind\":\"Zone\",\"resource_id\":\"100\"}]}'}"
```

Submit claim:

```sh
ros2 service call /ares/v1/claims/request ares_interfaces/srv/Json "{request: '{\"id\":10,\"robot_id\":1,\"mission_id\":9001,\"access_mode\":\"Exclusive\",\"priority\":10,\"requested_at_tick\":10,\"window\":{\"start_tick\":10,\"end_tick\":100},\"targets\":[{\"kind\":\"Zone\",\"resource_id\":\"100\"}]}'}"
```

List leases:

```sh
ros2 service call /ares/v1/leases/list ares_interfaces/srv/Json "{request: '{}'}"
```

Add lease:

```sh
ros2 service call /ares/v1/leases/add ares_interfaces/srv/Json "{request: '{\"id\":501,\"claim_id\":10,\"robot_id\":1,\"access_mode\":\"Exclusive\",\"targets\":[{\"kind\":\"Zone\",\"resource_id\":\"00000000-0000-0000-0000-000000000100\"}],\"granted_at_tick\":12,\"expires_at_tick\":200,\"disposition\":\"Active\",\"active\":true}'}"
```

Release lease:

```sh
ros2 service call /ares/v1/leases/release ares_interfaces/srv/Json "{request: '{\"lease_id\":501,\"released_at_tick\":50}'}"
```

== ROS2 connection story

On the ARES machine, the server listens for Zenoh peers on `tcp/0.0.0.0:7447`.
On the ROS2 machine, connect the bridge to the ARES machine:

```sh
zenoh-bridge-ros2dds -e tcp/TIMENAV_IP:7447
```

Use `tcp/TIMENAV_IP:7447`, not `tcp://TIMENAV_IP:7447`.

In another ROS2 terminal, source ROS and call the service:

```sh
ros2 service call /ares/v1/health ares_interfaces/srv/Json "{request: '{}'}"
```

Expected CLI shape:

```text
requester: making request: ares_interfaces.srv.Json_Request(request='{}')

response:
ares_interfaces.srv.Json_Response(
  success=true,
  response='{"status":"ok","version":"0.0.2"}'
)
```

If the bridge logs a route like this, the ROS2-to-Zenoh path is working:

```text
Route Service Client (ROS:/ares/v1/health <-> Zenoh:ares/v1/health) created
```

#block(fill: rgb("#f8fafc"), stroke: rgb("#cbd5e1"), radius: 5pt, inset: 8pt)[
  `ros2 service list` may not show the ARES services. That is expected in
  the pure-Zenoh setup. The call works because the bridge creates the route
  dynamically when the request is made.
]

== ROS2 troubleshooting

#table(
  columns: (55mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  table.header([Symptom], [Fix / explanation]),
  [`Unicast not supported for tcp: protocol`], [The endpoint used `tcp://...`. Use `tcp/...` instead.],
  [`ros2 service list` does not show the service], [Expected for this pure-Zenoh setup. Call the `/ares/v1/...` service directly.],
  [`waiting for service to become available...`], [Keep the bridge running, then call directly. The bridge creates the route when it sees the client.],
  [`received invalid request ... less than 20 bytes`], [Use CycloneDDS on the ROS2 side: `export RMW_IMPLEMENTATION=rmw_cyclonedds_cpp`.],
  [No response], [Check firewall/NAT and confirm the bridge can reach `tcp/TIMENAV_IP:7447`.],
)

== Internal ROS2DDS response encoding

ARES replies with a CDR-encoded `ares_interfaces/srv/Json_Response`:

```text
00 01 00 00                 CDR little-endian header
01                           bool success
00 00 00                     padding to 4 bytes
<u32 len including NUL>       string length
<UTF-8 JSON bytes>
00                           trailing NUL
<padding to 4 bytes>
```

= ARES native Zenoh services

These are the ARES native Zenoh services from the `robo` adapter. They are JSON
queryables under the `ares/v1` prefix. Request payloads are JSON strings where
the table says a payload is required. Replies are JSON. Errors are Zenoh error
replies with `{"message":"..."}`.

== Complete Zenoh service catalog

#table(
  columns: (58mm, 1fr, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  table.header([Key], [Payload], [Response]),
  [`ares/v1/health`], [none], [`Health`],
  [`ares/v1/fleet/snapshot`], [none], [`FleetSnapshot`],
  [`ares/v1/routes/plan`], [`PlanRouteRequest` JSON], [`PlanRouteResponse`],
  [`ares/v1/robots/register`], [`RobotState` JSON], [`RobotState`],
  [`ares/v1/robots/list`], [none], [`RobotState[]`],
  [`ares/v1/robots/heartbeat`], [`{"robot_id":1,"heartbeat":...}`], [`RobotState`],
  [`ares/v1/robots/assign_route`], [`{"robot_id":1,"assignment":...}`], [`RobotState`],
  [`ares/v1/robots/schedule`], [`{"robot_id":1,"schedule":...}`], [`ScheduleDecision`],
  [`ares/v1/claims/list`], [none], [`ClaimRequest[]`],
  [`ares/v1/claims/evaluate`], [`ClaimRequestWire` JSON], [`ClaimEvaluation`],
  [`ares/v1/claims/request`], [`ClaimRequestWire` JSON], [`ClaimEvaluation`],
  [`ares/v1/leases/list`], [none], [`Lease[]`],
  [`ares/v1/leases/add`], [`Lease` JSON], [`Lease`],
  [`ares/v1/leases/release`], [`ReleaseLeaseRequest` JSON], [`bool`],
)

=== `ares/v1/health`

Checks that the ARES service is alive.

#table(
  columns: (35mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  [Payload], [none],
  [Response], [`Health`: `{"status":"ok","version":"0.0.2"}`],
)

=== `ares/v1/fleet/snapshot`

Returns the whole live coordination state: robots, active claim requests, and
leases.

#table(
  columns: (35mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  [Payload], [none],
  [Response], [`FleetSnapshot`: `{"robots":[],"requests":[],"leases":[]}`],
)

=== `ares/v1/routes/plan`

Plans a route between two graph nodes.

#table(
  columns: (35mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  [Payload], [`PlanRouteRequest` JSON],
  [Response], [`PlanRouteResponse` JSON],
)

```json
{"start_node_id":"1001","goal_node_id":"1003","use_penalties":false}
```

=== `ares/v1/robots/register`

Registers or replaces a robot state.

#table(
  columns: (35mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  [Payload], [`RobotState` JSON],
  [Response], [`RobotState` JSON],
)

```json
{"robot_id":1,"mission_id":9001,"current_node_id":"00000000-0000-0000-0000-000000001001","progress_state":"Idle","updated_at_tick":1}
```

=== `ares/v1/robots/list`

Lists all registered robots.

#table(
  columns: (35mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  [Payload], [none],
  [Response], [`RobotState[]` JSON],
)

=== `ares/v1/robots/heartbeat`

Updates the robot's current node/edge and timestamp.

#table(
  columns: (35mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  [Payload], [`RobotHeartbeatEnvelope` JSON],
  [Response], [`RobotState` JSON],
)

```json
{"robot_id":1,"heartbeat":{"current_node_id":"1002","current_edge_id":null,"updated_at_tick":70}}
```

=== `ares/v1/robots/assign_route`

Assigns an already computed route plan to a robot.

#table(
  columns: (35mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  [Payload], [`RobotAssignRouteEnvelope` JSON],
  [Response], [`RobotState` JSON],
)

```json
{
  "robot_id": 1,
  "assignment": {
    "route_plan": {
      "start_node_id": "00000000-0000-0000-0000-000000001001",
      "goal_node_id": "00000000-0000-0000-0000-000000001003",
      "steps": [],
      "traversed_node_ids": [],
      "traversed_edge_ids": [],
      "traversed_zone_ids": [],
      "traversed_node_zone_ids": [],
      "traversed_edge_zone_ids": [],
      "total_cost": 0.0
    },
    "horizon": 100,
    "updated_at_tick": 10
  }
}
```

=== `ares/v1/robots/schedule`

Schedules a robot from an existing claim.

#table(
  columns: (35mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  [Payload], [`RobotScheduleEnvelope` JSON],
  [Response], [`ScheduleDecision` JSON],
)

```json
{"robot_id":1,"schedule":{"claim_id":10,"start_tick":20,"ticks_per_cost_unit":1.0,"access_mode":"Exclusive"}}
```

=== `ares/v1/claims/list`

Lists active claim requests.

#table(
  columns: (35mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  [Payload], [none],
  [Response], [`ClaimRequest[]` JSON],
)

=== `ares/v1/claims/evaluate`

Dry-runs a claim without storing it.

#table(
  columns: (35mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  [Payload], [`ClaimRequestWire` JSON],
  [Response], [`ClaimEvaluation` JSON],
)

```json
{"id":10,"robot_id":1,"mission_id":9001,"access_mode":"Exclusive","priority":10,"requested_at_tick":10,"window":{"start_tick":10,"end_tick":100},"targets":[{"kind":"Zone","resource_id":"100"}]}
```

=== `ares/v1/claims/request`

Evaluates a claim and stores it if the decision is `Grant`.

#table(
  columns: (35mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  [Payload], [`ClaimRequestWire` JSON],
  [Response], [`ClaimEvaluation` JSON],
)

```json
{"id":10,"robot_id":1,"mission_id":9001,"access_mode":"Exclusive","priority":10,"requested_at_tick":10,"window":{"start_tick":10,"end_tick":100},"targets":[{"kind":"Zone","resource_id":"100"}]}
```

=== `ares/v1/leases/list`

Lists leases.

#table(
  columns: (35mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  [Payload], [none],
  [Response], [`Lease[]` JSON],
)

=== `ares/v1/leases/add`

Adds a lease directly.

#table(
  columns: (35mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  [Payload], [`Lease` JSON],
  [Response], [`Lease` JSON],
)

```json
{"id":501,"claim_id":10,"robot_id":1,"access_mode":"Exclusive","targets":[{"kind":"Zone","resource_id":"00000000-0000-0000-0000-000000000100"}],"granted_at_tick":12,"expires_at_tick":200,"disposition":"Active","active":true}
```

=== `ares/v1/leases/release`

Releases a lease.

#table(
  columns: (35mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  [Payload], [`ReleaseLeaseRequest` JSON],
  [Response], [`bool`],
)

```json
{"lease_id":501,"released_at_tick":50}
```

== Native Zenoh operating sequence

The native Zenoh story mirrors REST:

#table(
  columns: (12mm, 55mm, 1fr),
  inset: 5pt,
  stroke: rgb("#e2e8f0"),
  table.header([Step], [Zenoh key], [Payload]),
  [1], [`ares/v1/health`], [none],
  [2], [`ares/v1/robots/register`], [`RobotState`],
  [3], [`ares/v1/routes/plan`], [`PlanRouteRequest`],
  [4], [`ares/v1/claims/evaluate`], [`ClaimRequestWire` dry-run],
  [5], [`ares/v1/claims/request`], [`ClaimRequestWire` store-if-granted],
  [6], [`ares/v1/robots/heartbeat`], [`RobotHeartbeatEnvelope`],
  [7], [`ares/v1/fleet/snapshot`], [none],
  [8], [`ares/v1/leases/release`], [`ReleaseLeaseRequest`, if leases are used],
)
