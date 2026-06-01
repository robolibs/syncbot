# timenav REST tutorial

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

Register robot `1` at node `1001`:

```sh
curl -X POST $BASE/robots -H 'content-type: application/json' -d '{"robot_id":1,"mission_id":9001,"current_node_id":"00000000-0000-0000-0000-000000001001","progress_state":"Idle","updated_at_tick":1}'
```

Register robot `2` at node `1003`:

```sh
curl -X POST $BASE/robots -H 'content-type: application/json' -d '{"robot_id":2,"mission_id":9002,"current_node_id":"00000000-0000-0000-0000-000000001003","progress_state":"Idle","updated_at_tick":1}'
```

List robots:

```sh
curl $BASE/robots
```

## Claim a zone

Robot `1` claims zone `100` exclusively:

```sh
curl -X POST $BASE/claims -H 'content-type: application/json' -d '{"id":10,"robot_id":1,"mission_id":9001,"access_mode":"Exclusive","priority":10,"requested_at_tick":10,"window":{"start_tick":10,"end_tick":100},"targets":[{"kind":"Zone","resource_id":"100"}]}'
```

List active claims:

```sh
curl $BASE/claims
```

Check if robot `2` can claim the same zone. This should be denied while claim `10` is active:

```sh
curl -X POST $BASE/claims/evaluate -H 'content-type: application/json' -d '{"id":11,"robot_id":2,"mission_id":9002,"access_mode":"Exclusive","priority":5,"requested_at_tick":11,"window":{"start_tick":10,"end_tick":100},"targets":[{"kind":"Zone","resource_id":"100"}]}'
```

Remove/release claim `10`:

```sh
curl -X DELETE $BASE/claims/10
```

Now robot `2` can claim zone `100`:

```sh
curl -X POST $BASE/claims -H 'content-type: application/json' -d '{"id":11,"robot_id":2,"mission_id":9002,"access_mode":"Exclusive","priority":5,"requested_at_tick":12,"window":{"start_tick":12,"end_tick":100},"targets":[{"kind":"Zone","resource_id":"100"}]}'
```

## Shared zone capacity

Zone `101` is shared and has capacity `2`.

Robot `1` shared claim:

```sh
curl -X POST $BASE/claims -H 'content-type: application/json' -d '{"id":20,"robot_id":1,"mission_id":9101,"access_mode":"Shared","priority":1,"requested_at_tick":20,"window":{"start_tick":20,"end_tick":120},"targets":[{"kind":"Zone","resource_id":"101"}]}'
```

Robot `2` shared claim:

```sh
curl -X POST $BASE/claims -H 'content-type: application/json' -d '{"id":21,"robot_id":2,"mission_id":9102,"access_mode":"Shared","priority":1,"requested_at_tick":21,"window":{"start_tick":20,"end_tick":120},"targets":[{"kind":"Zone","resource_id":"101"}]}'
```

A third overlapping shared claim should be denied because capacity is already full:

```sh
curl -X POST $BASE/claims/evaluate -H 'content-type: application/json' -d '{"id":22,"robot_id":3,"mission_id":9103,"access_mode":"Shared","priority":1,"requested_at_tick":22,"window":{"start_tick":20,"end_tick":120},"targets":[{"kind":"Zone","resource_id":"101"}]}'
```

## Heartbeat

Move robot `1` to node `1002`:

```sh
curl -X POST $BASE/robots/1/heartbeat -H 'content-type: application/json' -d '{"current_node_id":"1002","current_edge_id":null,"updated_at_tick":70}'
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

Delete active claims:

```sh
curl -X DELETE $BASE/claims/10
```

```sh
curl -X DELETE $BASE/claims/11
```

```sh
curl -X DELETE $BASE/claims/20
```

```sh
curl -X DELETE $BASE/claims/21
```

Final state:

```sh
curl $BASE/fleet/snapshot
```
