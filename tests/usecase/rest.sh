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

out=$(curl -sS "$B/zones")
assert_json 'map(select(.numeric_id != null)) | length' '7' "$out" 'seven numeric zones'
assert_json 'map(select(.numeric_id == 3))[0].name' 'dock_c' "$out" 'zone 3 name in listing'
out=$(curl -sS "$B/zones/3")
assert_json '.numeric_id' '3' "$out" 'zone 3 numeric id'
assert_json '.name' 'dock_c' "$out" 'zone 3 name'
assert_json '.properties["traffic.policy"]' 'exclusive' "$out" 'zone 3 policy'
out=$(curl -sS "$B/zones/99")
assert_json '.message' 'unknown zone id Numeric(99)' "$out" 'unknown zone error'
out=$(curl -sS "$B/fleet/snapshot")
assert_json '.robots | length' '0' "$out" 'empty fleet robots'
assert_json '.requests | length' '0' "$out" 'empty fleet requests'
assert_json '.leases | length' '0' "$out" 'empty fleet leases'

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
