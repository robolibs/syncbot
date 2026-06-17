# syncbot REST tutorial

Start the server:

```sh
make run
```

Or as a daemon:

```sh
make run DAEMON=1
```

Use this base URL in the examples below:

```sh
BASE=http://127.0.0.1:8080/ares/v1
```

## Health

```sh
curl $BASE/health
```

## Query the map

List zones:

```sh
curl $BASE/zones
```

Get zone `100`:

```sh
curl $BASE/zones/100
```

List nodes:

```sh
curl $BASE/nodes
```

Get node `1001`:

```sh
curl $BASE/nodes/1001
```

List edges:

```sh
curl $BASE/edges
```

Get edge `2001`:

```sh
curl $BASE/edges/2001
```

## Register robots

Registration is flat — just an id and an auth `key` (an integer password or
`did:pass=<secret>`). The reply is `{"decision":1,"reason":0}` on success.

Register robot `1` with key `1234`:

```sh
curl -X POST $BASE/robots -H 'content-type: application/json' -d '{"robot":"1","key":"1234"}'
```

Register robot `2` with key `5678`:

```sh
curl -X POST $BASE/robots -H 'content-type: application/json' -d '{"robot":"2","key":"5678"}'
```

List robots:

```sh
curl $BASE/robots
```

## Claim a zone

Claims are flat — the type is in the path (`/claims/zone`), the body is the key,
the robot, and the id(s). The reply is `decision` + `reason` (`2` = conflict),
plus `blocked` naming the offending id on denial.

Robot `1` claims zone `100` exclusively:

```sh
curl -X POST $BASE/claims/zone -H 'content-type: application/json' -d '{"key":"1234","robot":"1","id":[100]}'
```

List active claims:

```sh
curl $BASE/claims
```

Robot `2` tries the same zone — denied while robot `1` holds it
(`{"decision":0,"reason":2,"blocked":100}`):

```sh
curl -X POST $BASE/claims/zone -H 'content-type: application/json' -d '{"key":"5678","robot":"2","id":[100]}'
```

Robot `1` releases zone `100`:

```sh
curl -X POST $BASE/leases/release/zone -H 'content-type: application/json' -d '{"key":"1234","robot":"1","id":100}'
```

Now robot `2` can claim zone `100`:

```sh
curl -X POST $BASE/claims/zone -H 'content-type: application/json' -d '{"key":"5678","robot":"2","id":[100]}'
```

## Shared zone capacity

Zone `101` is shared and has capacity `2`.

The flat `/claims/zone` uses an exclusive claim by default. To use shared
capacity, fine-level clients submit the nested `POST /claims` body with
`"access_mode":"Shared"` and a `"key"` (state-changing, so the key is required):

```sh
curl -X POST $BASE/claims -H 'content-type: application/json' -d '{"id":20,"robot_id":1,"key":"1234","mission_id":9101,"access_mode":"Shared","priority":1,"requested_at_tick":20,"window":{"start_tick":20,"end_tick":120},"targets":[{"kind":"Zone","resource_id":"101"}]}'
curl -X POST $BASE/claims -H 'content-type: application/json' -d '{"id":21,"robot_id":2,"key":"5678","mission_id":9102,"access_mode":"Shared","priority":1,"requested_at_tick":21,"window":{"start_tick":20,"end_tick":120},"targets":[{"kind":"Zone","resource_id":"101"}]}'
```

A third overlapping shared claim is denied once capacity is full. The dry-run
`evaluate` is read-only and needs no key:

```sh
curl -X POST $BASE/claims/evaluate -H 'content-type: application/json' -d '{"id":22,"robot_id":3,"mission_id":9103,"access_mode":"Shared","priority":1,"requested_at_tick":22,"window":{"start_tick":20,"end_tick":120},"targets":[{"kind":"Zone","resource_id":"101"}]}'
```

## Heartbeat

Heartbeat is flat: key + position (zone, node, or edge). The server stamps the
tick; the reply is just an ack. Move robot `1` to node `1002`:

```sh
curl -X POST $BASE/robots/1/heartbeat -H 'content-type: application/json' -d '{"key":"1234","node":"1002"}'
```

Read robot `1`:

```sh
curl $BASE/robots/1
```

## Leases

The current API separates active claim requests from leases. `POST /claims` records a granted request; it does not automatically create a lease.

Add lease `501` for claim `10` on zone `100`:

```sh
curl -X POST $BASE/leases -H 'content-type: application/json' -d '{"id":501,"claim_id":10,"robot_id":1,"access_mode":"Exclusive","targets":[{"kind":"Zone","resource_id":"00000000-0000-0000-0000-000000000100"}],"granted_at_tick":12,"expires_at_tick":200,"disposition":"Active","active":true}'
```

List leases:

```sh
curl $BASE/leases
```

Release lease `501`:

```sh
curl -X POST $BASE/leases/release -H 'content-type: application/json' -d '{"lease_id":501,"released_at_tick":50}'
```

## Cleanup

Release robot `2`'s zone `100` (flat, by robot + resource):

```sh
curl -X POST $BASE/leases/release/zone -H 'content-type: application/json' -d '{"key":"5678","robot":"2","id":100}'
```

The shared claims were submitted with explicit ids `20`/`21`, so a fine-level
client can also remove them by id:

```sh
curl -X DELETE $BASE/claims/20
curl -X DELETE $BASE/claims/21
```

Final state:

```sh
curl $BASE/fleet/snapshot
```
