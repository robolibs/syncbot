#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$ROOT"
source tests/usecase/common.sh

HTTP_PORT=${USECASE_HTTP_PORT:-18080}
ZENOH_PORT=${USECASE_ZENOH_PORT:-17447}
LOG=${USECASE_LOG:-target/usecase-rest-server.log}
SERVER_PID=
trap 'stop_process "$SERVER_PID"' EXIT

start_server "$HTTP_PORT" "$ZENOH_PORT" "$LOG"
B="http://127.0.0.1:${HTTP_PORT}/ares/v1"

out=$(curl -sS "$B/health")
assert_json '.status' 'ok' "$out" 'health status'
assert_json '.version' '0.1.3' "$out" 'health version'
assert_json '.datum.lat' '52.0' "$out" 'health carries the workspace datum'
out=$(curl -sS -H 'accept: application/xml' "$B/health")
assert_contains '<lat>52</lat>' "$out" 'XML health carries the same datum'

out=$(curl -sS "$B/zones")
assert_json 'map(select(.numeric_id != null)) | length' '7' "$out" 'seven numeric zones'
assert_json 'map(select(.numeric_id == 3))[0].name' 'dock_c' "$out" 'zone 3 name in listing'
out=$(curl -sS "$B/zones/3")
assert_json '.numeric_id' '3' "$out" 'zone 3 numeric id'
assert_json '.name' 'dock_c' "$out" 'zone 3 name'
assert_json '.properties["traffic.policy"]' 'exclusive' "$out" 'zone 3 policy'
out=$(curl -sS "$B/zones/99")
assert_json '.message' 'unknown zone id Numeric(99)' "$out" 'unknown zone error'
out=$(curl -sS -X POST "$B/routes/plan" -H 'content-type: application/json' \
    -d '{"start_node_id":"1001","goal_node_id":"1003"}')
assert_json '.found' 'true' "$out" 'route 1001 -> 1003 found'
assert_json '.plan.traversed_node_ids | length' '3' "$out" 'route visits three nodes'
assert_json '.plan.traversed_edge_ids | length' '2' "$out" 'route crosses two edges'
out=$(curl -sS -X POST "$B/routes/plan" -H 'content-type: application/json' \
    -d '{"start_node_id":"1001","goal_node_id":"9999"}')
assert_json '.message' 'unknown goal node id Numeric(9999)' "$out" 'unknown goal node error'
out=$(curl -sS -X POST "$B/routes/plan" -H 'content-type: application/xml' \
    -H 'accept: application/xml' \
    -d '<plan><start_node_id>1001</start_node_id><goal_node_id>1003</goal_node_id></plan>')
assert_contains '<found>true</found>' "$out" 'route plan over XML'

out=$(curl -sS "$B/fleet/snapshot")
assert_json '.robots | length' '0' "$out" 'empty fleet robots'
assert_json '.requests | length' '0' "$out" 'empty fleet requests'
assert_json '.leases | length' '0' "$out" 'empty fleet leases'

# Atomic route claim: nodes + edges in one request (PLAN Milestone 1.4).
out=$(curl -sS -X POST "$B/robots/register" -H 'content-type: application/json' \
    -d '{"robot":"70","key":"7070","alive":9999}')
assert_reply_json 1 0 "$out" 'route-claim register 70'
out=$(curl -sS -X POST "$B/robots/register" -H 'content-type: application/json' \
    -d '{"robot":"71","key":"7171","alive":9999}')
assert_reply_json 1 0 "$out" 'route-claim register 71'
out=$(curl -sS -X POST "$B/claims/route" -H 'content-type: application/json' \
    -d '{"key":"7070","robot":"70","node":[1001,1002],"edge":[2001]}')
assert_reply_json 1 0 "$out" 'robot 70 claims route 1001-1002'
# A route sharing node 1002 is refused whole.
out=$(curl -sS -X POST "$B/claims/route" -H 'content-type: application/json' \
    -d '{"key":"7171","robot":"71","node":[1002,1003],"edge":[2002]}')
assert_reply_json 0 2 "$out" 'overlapping route refused'
# Re-claiming your own route is a re-acquisition, not a conflict.
out=$(curl -sS -X POST "$B/claims/route" -H 'content-type: application/json' \
    -d '{"key":"7070","robot":"70","node":[1001,1002],"edge":[2001]}')
assert_reply_json 1 0 "$out" 'robot 70 re-claims its own route'
# An unknown id refuses the whole route and names it.
out=$(curl -sS -X POST "$B/claims/route" -H 'content-type: application/json' \
    -d '{"key":"7171","robot":"71","node":[9999],"edge":[]}')
assert_reply_json 0 4 "$out" 'unknown node in route'
# Same op over XML.
out=$(xml_post /claims/route '<claim><key>7171</key><robot>71</robot><node>1003</node></claim>')
assert_reply_xml 1 0 "$out" 'route claim over XML'
out=$(xml_post /leases/release/node '<rel><key>7070</key><robot>70</robot><id>1001</id></rel>')
assert_reply_xml 1 0 "$out" 'release route node'

run_register_heartbeat_claim_release_xml

out=$(xml_post /robots '<reg><robot>95</robot><key>5</key><alive>1</alive></reg>')
assert_reply_xml 1 0 "$out" 'auto-release register 95'
out=$(xml_post /claims/zone '<claim><key>5</key><robot>95</robot><id>3</id></claim>')
assert_reply_xml 1 0 "$out" 'auto-release claim zone 3 by 95'
out=$(xml_post /robots '<reg><robot>96</robot><key>6</key></reg>')
assert_reply_xml 1 0 "$out" 'auto-release register 96'
out=$(xml_post /claims/zone '<claim><key>6</key><robot>96</robot><id>3</id></claim>')
assert_reply_xml 0 2 "$out" 'zone 3 blocked before timeout'
assert_contains '<blocked>3</blocked>' "$out" 'zone 3 blocker before timeout'
sleep 3
out=$(xml_post /claims/zone '<claim><key>6</key><robot>96</robot><id>3</id></claim>')
assert_reply_xml 1 0 "$out" 'zone 3 granted after timeout'

printf 'USECASE REST/XML: PASS\n'
