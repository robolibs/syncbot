#let accent = rgb("#2563eb")
#let dark = rgb("#0f172a")
#let muted = rgb("#64748b")
#let pale = rgb("#eff6ff")
#let warn = rgb("#f59e0b")

#set document(title: "ARES Protocol — Use Cases", author: "robolibs / ARES")
#set page(
  paper: "a4",
  margin: (x: 18mm, y: 17mm),
  header: align(right, text(size: 8pt, fill: muted)[ARES protocol — use cases]),
  footer: context align(center, text(size: 8pt, fill: muted)[page #counter(page).display("1")]),
)
#set text(size: 9.5pt, fill: dark)
#set heading(numbering: "1.1")
#set par(justify: true, leading: 0.62em)
#show heading: it => [ #v(0.6em) #it #v(0.2em) ]
#show raw.where(block: true): it => block(
  fill: rgb("#f8fafc"), stroke: rgb("#e2e8f0"), radius: 4pt, inset: 6pt, width: 100%,
  text(size: 8pt, it),
)
#show raw.where(block: false): it => box(
  fill: rgb("#f8fafc"), stroke: rgb("#dbe3ef"), radius: 2pt,
  inset: (x: 2.5pt, y: 1pt), outset: (y: 0.5pt), it,
)
#let tbl(..args) = table(inset: 5pt, stroke: rgb("#e2e8f0"), ..args)
#let note(body) = block(fill: pale, stroke: accent.lighten(45%), radius: 6pt, inset: 9pt, body)
#let caution(body) = block(fill: rgb("#fffbeb"), stroke: warn.lighten(20%), radius: 5pt, inset: 8pt, body)

#align(center)[
  #text(size: 24pt, weight: "bold", fill: accent)[ARES Protocol — Use Cases]
  #v(0.4em)
  #text(size: 11pt, fill: muted)[The same battery, real commands: JSON, XML, and mixed]
]

#v(1em)
#note[
  One protocol, several encodings — every command shown in full. The
  register / heartbeat / claim / release battery runs over *REST/JSON* (`curl`),
  over *REST/XML* (`curl`), and *mixed*
  across both, against one coordinator and one 7-zone workspace
  (`dock_a=1 … dock_c=3`, `zone_4=4 … zone_7=7`). Zones 1–4 and 6 are exclusive;
  zones 5 and 7 are shared with capacity 2. Server version `0.1.3`. Every reply
  is captured from a live run.

  *Auto-release:* a robot loses its zones when it stops heart-beating — no manual
  reset. Register with a high `<alive>` (e.g. `9999`) to avoid constant
  heartbeats; the server reclaims a zone only after `2×` the `alive` interval
  passes with no heartbeat (section F uses a short `alive` to show it).

  *Leases:* `<lease_time>` is in *seconds of wall clock*. A claim carrying one is
  dropped that many seconds after it is granted, whether or not the robot keeps
  heart-beating; omitting it (or sending `0`) holds the zone until it is
  released or the robot goes quiet.
]

Reason codes (after `0` ok / `1` mismatched key): register `2` already
registered, `3` bad id, `5` not permitted (id not provisioned, or keyless
registration disabled), `6` fleet full. Heartbeat `2` not registered. Claim `2`
conflict, `3` capacity, `4` unknown id, `5` bad request. Release `2` no such
lease.

= JSON (REST) — `curl`

Set `B=http://<host>:8080/ares/v1`. JSON is the default wire format: the
`content-type` header below is explicit for clarity, but a body with no
`content-type` is read as JSON and a request with no `Accept` is answered in
JSON. Replies shown after `# ->`.

== A. Read (no key)

```sh
curl -s "$B/health"
# -> {"status":"ok","version":"0.1.3","datum":{"lat":52.0,"lon":5.0,"alt":0.0}}
curl -s "$B/zones"          # -> [ … 8 zones: fixed_yard, dock_a … dock_c, zone_4 … zone_7 ]
curl -s "$B/zones/3"
# -> {"id":"…0103","numeric_id":3,"name":"dock_c","kind":"zone",…,
#     "properties":{"traffic.policy":"exclusive",…}}
curl -s "$B/zones/99"       # -> {"message":"unknown zone id Numeric(99)"}
curl -s "$B/fleet/snapshot"
# -> {"robots":[],"requests":[],"leases":[],"inactive_robot_ids":[],"events":[],…}
```

== B. Register

```sh
curl -s -X POST "$B/robots" -H 'content-type: application/json' \
  -d '{"robot":"91","key":"1234","alive":9999}'
# -> {"decision":1,"reason":0}
curl -s -X POST "$B/robots" -H 'content-type: application/json' \
  -d '{"robot":"91","key":"1234"}'                    # again -> already registered
# -> {"decision":0,"reason":2}
curl -s -X POST "$B/robots" -H 'content-type: application/json' \
  -d '{"robot":"92"}'                                 # no key -> default
# -> {"decision":1,"reason":0}
curl -s -X POST "$B/robots" -H 'content-type: application/json' \
  -d '{"robot":"abc"}'                                # bad id
# -> {"decision":0,"reason":3}
```

== C. Heartbeat

```sh
curl -s -X POST "$B/robots/91/heartbeat" -H 'content-type: application/json' \
  -d '{"key":"1234","zone":3}'                        # ok
# -> {"decision":1,"reason":0}
curl -s -X POST "$B/robots/91/heartbeat" -H 'content-type: application/json' \
  -d '{"key":"9999","zone":3}'                        # wrong key
# -> {"decision":0,"reason":1}
curl -s -X POST "$B/robots/999/heartbeat" -H 'content-type: application/json' \
  -d '{"zone":3}'                                     # not registered
# -> {"decision":0,"reason":2}
curl -s -X POST "$B/robots/91/heartbeat" -H 'content-type: application/json' \
  -d '{"key":"1234","zone":-1}'                       # location unknown
# -> {"decision":1,"reason":0}
```

== D. Claim

Setup: `curl -s -X POST "$B/robots" -H 'content-type: application/json' -d '{"robot":"94","key":"4444","alive":9999}'`.

`id` is an *array* even for a single resource — `[5]`, not `5`.

```sh
curl -s -X POST "$B/claims/zone" -H 'content-type: application/json' \
  -d '{"key":"1234","robot":"91","id":[5]}'                    # granted
# -> {"decision":1,"reason":0}
curl -s -X POST "$B/claims/zone" -H 'content-type: application/json' \
  -d '{"robot":"92","id":[5]}'                                 # 5 held by 91
# -> {"decision":0,"reason":2,"blocked":5}
curl -s -X POST "$B/claims/zone" -H 'content-type: application/json' \
  -d '{"robot":"92","id":[6,5]}'                               # atomic; 5 held
# -> {"decision":0,"reason":2,"blocked":5}
curl -s -X POST "$B/claims/zone" -H 'content-type: application/json' \
  -d '{"robot":"92","id":[6]}'                                 # granted
# -> {"decision":1,"reason":0}
curl -s -X POST "$B/claims/zone" -H 'content-type: application/json' \
  -d '{"key":"4444","robot":"94","id":[7],"access_mode":1,"lease_time":30}'
# -> {"decision":1,"reason":0}                                 # exclusive + lease
curl -s -X POST "$B/claims/zone" -H 'content-type: application/json' \
  -d '{"key":"1234","robot":"91","id":[1],"access_mode":2}'    # access_mode 2 = shared
# -> {"decision":1,"reason":0}
curl -s -X POST "$B/claims/zone" -H 'content-type: application/json' \
  -d '{"key":"1234","robot":"91","id":[2],"access_mode":3}'    # 3+ reserved
# -> {"decision":0,"reason":5}
curl -s -X POST "$B/claims/zone" -H 'content-type: application/json' \
  -d '{"key":"1234","robot":"91","id":[99]}'                   # unknown zone
# -> {"decision":0,"reason":4,"blocked":99}
curl -s -X POST "$B/claims/zone" -H 'content-type: application/json' \
  -d '{"key":"0000","robot":"91","id":[2]}'                    # wrong key
# -> {"decision":0,"reason":1}
```

== E. Release

`id` here is a *single* value, not an array — one resource is released at a time.

```sh
curl -s -X POST "$B/leases/release/zone" -H 'content-type: application/json' \
  -d '{"key":"1234","robot":"91","id":5}'                      # released
# -> {"decision":1,"reason":0}
curl -s -X POST "$B/leases/release/zone" -H 'content-type: application/json' \
  -d '{"key":"1234","robot":"91","id":5}'                      # nothing held now
# -> {"decision":0,"reason":2}
curl -s -X POST "$B/claims/zone" -H 'content-type: application/json' \
  -d '{"robot":"92","id":[5]}'                                 # now free -> granted
# -> {"decision":1,"reason":0}
```

== F. Auto-release on heartbeat timeout

```sh
curl -s -X POST "$B/robots" -H 'content-type: application/json' \
  -d '{"robot":"95","key":"5","alive":1}'                      # short alive
# -> {"decision":1,"reason":0}
curl -s -X POST "$B/claims/zone" -H 'content-type: application/json' \
  -d '{"key":"5","robot":"95","id":[3]}'                       # 95 takes zone 3
# -> {"decision":1,"reason":0}
curl -s -X POST "$B/robots" -H 'content-type: application/json' \
  -d '{"robot":"96","key":"6"}'
# -> {"decision":1,"reason":0}
curl -s -X POST "$B/claims/zone" -H 'content-type: application/json' \
  -d '{"key":"6","robot":"96","id":[3]}'                       # 95 still holds it
# -> {"decision":0,"reason":2,"blocked":3}
sleep 3   # 95 (alive 1) misses 2x its interval -> inactive, zone 3 auto-released
curl -s -X POST "$B/claims/zone" -H 'content-type: application/json' \
  -d '{"key":"6","robot":"96","id":[3]}'                       # now granted
# -> {"decision":1,"reason":0}
```

#note[*Same protocol, different clothes.* Every decision and reason above is
identical to the XML chapter that follows — only the encoding differs. Pick a
format per client; the coordinator does not care which one a robot speaks.]

= XML (REST) — `curl`

Set `B=http://<host>:8080/ares/v1`. Send `content-type: application/xml` and,
for the read endpoints, ask for `Accept: application/xml`. Replies shown after
`# ->`.

== A. Read (no key)

```sh
curl -s "$B/health"
# -> {"status":"ok","version":"0.1.3"}
curl -s "$B/zones"          # -> [ … zones: dock_a, dock_b, dock_c, zone_4 … zone_7 ]
curl -s "$B/zones/3"        # -> {"numeric_id":3,"name":"dock_c",…,"traffic.policy":"exclusive"}
curl -s "$B/zones/99"       # -> {"message":"unknown zone id Numeric(99)"}
curl -s "$B/fleet/snapshot" # -> {"robots":[],"requests":[],"leases":[]}
```

== B. Register

```sh
curl -s -X POST "$B/robots" -H 'content-type: application/xml' \
  -d '<reg><robot>91</robot><key>1234</key><alive>9999</alive></reg>'
# -> <reply><decision>1</decision><reason>0</reason></reply>
curl -s -X POST "$B/robots" -H 'content-type: application/xml' \
  -d '<reg><robot>91</robot><key>1234</key></reg>'          # again -> already registered
# -> <reply><decision>0</decision><reason>2</reason></reply>
curl -s -X POST "$B/robots" -H 'content-type: application/xml' \
  -d '<reg><robot>92</robot></reg>'                          # no key -> default
# -> <reply><decision>1</decision><reason>0</reason></reply>
curl -s -X POST "$B/robots" -H 'content-type: application/xml' \
  -d '<reg><robot>abc</robot></reg>'                         # bad id
# -> <reply><decision>0</decision><reason>3</reason></reply>
```

== C. Heartbeat

```sh
curl -s -X POST "$B/robots/91/heartbeat" -H 'content-type: application/xml' \
  -d '<hb><key>1234</key><zone>3</zone></hb>'                # ok
# -> <reply><decision>1</decision><reason>0</reason></reply>
curl -s -X POST "$B/robots/91/heartbeat" -H 'content-type: application/xml' \
  -d '<hb><key>9999</key><zone>3</zone></hb>'                # wrong key
# -> <reply><decision>0</decision><reason>1</reason></reply>
curl -s -X POST "$B/robots/999/heartbeat" -H 'content-type: application/xml' \
  -d '<hb><zone>3</zone></hb>'                               # not registered
# -> <reply><decision>0</decision><reason>2</reason></reply>
curl -s -X POST "$B/robots/91/heartbeat" -H 'content-type: application/xml' \
  -d '<hb><key>1234</key><zone>-1</zone></hb>'               # location unknown
# -> <reply><decision>1</decision><reason>0</reason></reply>
```

== D. Claim

Setup: `curl -s -X POST "$B/robots" -H 'content-type: application/xml' -d '<reg><robot>94</robot><key>4444</key><alive>9999</alive></reg>'`.

```sh
curl -s -X POST "$B/claims/zone" -H 'content-type: application/xml' \
  -d '<claim><key>1234</key><robot>91</robot><id>5</id></claim>'         # granted
# -> <reply><decision>1</decision><reason>0</reason></reply>
curl -s -X POST "$B/claims/zone" -H 'content-type: application/xml' \
  -d '<claim><robot>92</robot><id>5</id></claim>'                        # 5 held by 91
# -> <reply><decision>0</decision><reason>2</reason><blocked>5</blocked></reply>
curl -s -X POST "$B/claims/zone" -H 'content-type: application/xml' \
  -d '<claim><robot>92</robot><id>6</id><id>5</id></claim>'              # atomic; 5 held
# -> <reply><decision>0</decision><reason>2</reason><blocked>5</blocked></reply>
curl -s -X POST "$B/claims/zone" -H 'content-type: application/xml' \
  -d '<claim><robot>92</robot><id>6</id></claim>'                        # granted
# -> <reply><decision>1</decision><reason>0</reason></reply>
curl -s -X POST "$B/claims/zone" -H 'content-type: application/xml' \
  -d '<claim><key>4444</key><robot>94</robot><id>7</id>
      <access_mode>1</access_mode><lease_time>30</lease_time></claim>'   # exclusive + lease
# -> <reply><decision>1</decision><reason>0</reason></reply>
curl -s -X POST "$B/claims/zone" -H 'content-type: application/xml' \
  -d '<claim><key>1234</key><robot>91</robot><id>1</id>
      <access_mode>2</access_mode></claim>'                             # access_mode 2 = shared
# -> <reply><decision>1</decision><reason>0</reason></reply>
curl -s -X POST "$B/claims/zone" -H 'content-type: application/xml' \
  -d '<claim><key>1234</key><robot>91</robot><id>2</id>
      <access_mode>3</access_mode></claim>'                             # 3+ reserved
# -> <reply><decision>0</decision><reason>5</reason></reply>
curl -s -X POST "$B/claims/zone" -H 'content-type: application/xml' \
  -d '<claim><key>1234</key><robot>91</robot><id>99</id></claim>'        # unknown zone
# -> <reply><decision>0</decision><reason>4</reason><blocked>99</blocked></reply>
curl -s -X POST "$B/claims/zone" -H 'content-type: application/xml' \
  -d '<claim><key>0000</key><robot>91</robot><id>2</id></claim>'         # wrong key
# -> <reply><decision>0</decision><reason>1</reason></reply>
```

== E. Release

```sh
curl -s -X POST "$B/leases/release/zone" -H 'content-type: application/xml' \
  -d '<rel><key>1234</key><robot>91</robot><id>5</id></rel>'             # released
# -> <reply><decision>1</decision><reason>0</reason></reply>
curl -s -X POST "$B/leases/release/zone" -H 'content-type: application/xml' \
  -d '<rel><key>1234</key><robot>91</robot><id>5</id></rel>'             # nothing held now
# -> <reply><decision>0</decision><reason>2</reason></reply>
curl -s -X POST "$B/claims/zone" -H 'content-type: application/xml' \
  -d '<claim><robot>92</robot><id>5</id></claim>'                        # now free -> granted
# -> <reply><decision>1</decision><reason>0</reason></reply>
```

== F. Auto-release on heartbeat timeout

```sh
curl -s -X POST "$B/robots" -H 'content-type: application/xml' \
  -d '<reg><robot>95</robot><key>5</key><alive>1</alive></reg>'          # short alive
# -> <reply><decision>1</decision><reason>0</reason></reply>
curl -s -X POST "$B/claims/zone" -H 'content-type: application/xml' \
  -d '<claim><key>5</key><robot>95</robot><id>3</id></claim>'            # 95 takes zone 3
# -> <reply><decision>1</decision><reason>0</reason></reply>
curl -s -X POST "$B/robots" -H 'content-type: application/xml' \
  -d '<reg><robot>96</robot><key>6</key></reg>'
# -> <reply><decision>1</decision><reason>0</reason></reply>
curl -s -X POST "$B/claims/zone" -H 'content-type: application/xml' \
  -d '<claim><key>6</key><robot>96</robot><id>3</id></claim>'            # 95 still holds it
# -> <reply><decision>0</decision><reason>2</reason><blocked>3</blocked></reply>
sleep 3   # 95 (alive 1) misses 2x its interval -> inactive, zone 3 auto-released
curl -s -X POST "$B/claims/zone" -H 'content-type: application/xml' \
  -d '<claim><key>6</key><robot>96</robot><id>3</id></claim>'            # now granted
# -> <reply><decision>1</decision><reason>0</reason></reply>
```

#caution[*`access_mode 2`* now means *shared*, not "reserved" — on a free zone it
is granted, and only lets robots coexist on a `shared`-policy zone with capacity
> 1. On a free exclusive zone it behaves like a normal claim. `access_mode 3`+ is
still reserved (→ reason `5`).]

= Mixed (REST/JSON + REST/XML, one coordinator)

Two robots on *different encodings* against the *same* server: robot `91` speaks
REST/XML (`curl`) and robot `93` speaks REST/JSON (`curl`). We register one on
each, then run the experiments — a claim on either blocks the other, in both
directions, and a release on one frees the zone for the other.

```sh
# --- register: 91 over XML, 93 over JSON ---
curl -s -X POST "$B/robots" -H 'content-type: application/xml' \
  -d '<reg><robot>91</robot><key>1234</key><alive>9999</alive></reg>'
# -> <reply><decision>1</decision><reason>0</reason></reply>
curl -s -X POST "$B/robots" -H 'content-type: application/json' \
  -d '{"robot":"93","key":"3333","alive":9999}'
# -> {"decision":1,"reason":0}

# --- experiment 1: XML robot 91 takes zone 4, JSON robot 93 is blocked ---
curl -s -X POST "$B/claims/zone" -H 'content-type: application/xml' \
  -d '<claim><key>1234</key><robot>91</robot><id>4</id></claim>'
# -> <reply><decision>1</decision><reason>0</reason></reply>          (91 holds zone 4)
curl -s -X POST "$B/claims/zone" -H 'content-type: application/json' \
  -d '{"key":"3333","robot":"93","id":[4]}'
# -> {"decision":0,"reason":2,"blocked":4}                            (blocked by XML 91)

# --- experiment 2: JSON robot 93 takes zone 6, XML robot 91 is blocked ---
curl -s -X POST "$B/claims/zone" -H 'content-type: application/json' \
  -d '{"key":"3333","robot":"93","id":[6]}'
# -> {"decision":1,"reason":0}                                        (93 holds zone 6)
curl -s -X POST "$B/claims/zone" -H 'content-type: application/xml' \
  -d '<claim><key>1234</key><robot>91</robot><id>6</id></claim>'
# -> <reply><decision>0</decision><reason>2</reason><blocked>6</blocked></reply>  (blocked by JSON 93)

# --- experiment 3: XML robot 91 releases zone 4, JSON robot 93 can now take it ---
curl -s -X POST "$B/leases/release/zone" -H 'content-type: application/xml' \
  -d '<rel><key>1234</key><robot>91</robot><id>4</id></rel>'
# -> <reply><decision>1</decision><reason>0</reason></reply>
curl -s -X POST "$B/claims/zone" -H 'content-type: application/json' \
  -d '{"key":"3333","robot":"93","id":[4]}'
# -> {"decision":1,"reason":0}                                        (now free -> granted)

# --- experiment 4: JSON robot 93 takes zone 2, XML robot 91 is blocked ---
curl -s -X POST "$B/claims/zone" -H 'content-type: application/json' \
  -d '{"key":"3333","robot":"93","id":[2]}'
# -> {"decision":1,"reason":0}                                        (93 holds zone 2)
curl -s -X POST "$B/claims/zone" -H 'content-type: application/xml' \
  -d '<claim><key>1234</key><robot>91</robot><id>2</id></claim>'
# -> <reply><decision>0</decision><reason>2</reason><blocked>2</blocked></reply>  (blocked by JSON 93)

# --- experiment 5: JSON robot 93 releases zone 2, XML robot 91 takes it ---
curl -s -X POST "$B/leases/release/zone" -H 'content-type: application/json' \
  -d '{"key":"3333","robot":"93","id":2}'
# -> {"decision":1,"reason":0}
curl -s -X POST "$B/claims/zone" -H 'content-type: application/xml' \
  -d '<claim><key>1234</key><robot>91</robot><id>2</id></claim>'
# -> <reply><decision>1</decision><reason>0</reason></reply>          (now free -> granted)
```

#note[Captured live: both encodings via `curl` against one coordinator on the
same seven-zone workspace. The encoding is a client's own choice — the
coordinator sees one fleet.]
