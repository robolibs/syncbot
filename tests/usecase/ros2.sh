#!/usr/bin/env bash
set -eo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$ROOT"

source "${ROS_SETUP:-/opt/ros/jazzy/setup.bash}"
source "${ROS2_INTERFACE_SETUP:-target/usecase-ros2/install/share/ares_interfaces/local_setup.bash}"
set -u
source tests/usecase/common.sh

HTTP_PORT=${USECASE_HTTP_PORT:-18081}
ZENOH_PORT=${USECASE_ZENOH_PORT:-17448}
ROS_DOMAIN_ID=${ROS_DOMAIN_ID:-87}
export ROS_DOMAIN_ID RMW_IMPLEMENTATION=${USECASE_RMW_IMPLEMENTATION:-rmw_cyclonedds_cpp}
export ROS_LOCALHOST_ONLY=1
SERVER_LOG=${USECASE_SERVER_LOG:-target/usecase-ros2-server.log}
BRIDGE_LOG=${USECASE_BRIDGE_LOG:-target/usecase-ros2-bridge.log}
SERVER_PID=
BRIDGE_PID=
trap 'stop_process "$BRIDGE_PID"; stop_process "$SERVER_PID"' EXIT

start_server "$HTTP_PORT" "$ZENOH_PORT" "$SERVER_LOG"
start_bridge "$ZENOH_PORT" "$ROS_DOMAIN_ID" "$BRIDGE_LOG"

out=$(ros_call /ares/v1/health '{}')
assert_contains 'success=True' "$out" 'ROS2 health success'
response=$(ros_response_json "$out")
assert_json '.status' 'ok' "$response" 'ROS2 health status'
assert_json '.version' '0.1.3' "$response" 'ROS2 health version'

out=$(ros_call /ares/v1/zones/get '{"id":"3"}')
assert_contains 'success=True' "$out" 'ROS2 zone 3 success'
response=$(ros_response_json "$out")
assert_json '.numeric_id' '3' "$response" 'ROS2 zone 3 numeric id'
assert_json '.name' 'dock_c' "$response" 'ROS2 zone 3 name'
out=$(ros_call /ares/v1/zones/get '{"id":"99"}')
assert_contains 'success=False' "$out" 'ROS2 unknown zone failure'
assert_contains 'unknown zone id Numeric(99)' "$out" 'ROS2 unknown zone message'
out=$(ros_call /ares/v1/fleet/snapshot '{}')
assert_contains 'success=True' "$out" 'ROS2 fleet snapshot success'
response=$(ros_response_json "$out")
assert_json '.robots | length' '0' "$response" 'ROS2 empty fleet robots'
assert_json '.requests | length' '0' "$response" 'ROS2 empty fleet requests'
assert_json '.leases | length' '0' "$response" 'ROS2 empty fleet leases'

run_register_heartbeat_claim_release_ros2

out=$(ros_call /ares/v1/robots/register '{"robot":"95","key":"5","alive":5}')
assert_ros_reply 1 0 "$out" 'ROS2 auto-release register 95'
out=$(ros_call /ares/v1/claims/zone '{"key":"5","robot":"95","id":[3]}')
assert_ros_reply 1 0 "$out" 'ROS2 auto-release claim zone 3 by 95'
out=$(ros_call /ares/v1/robots/register '{"robot":"96","key":"6","alive":9999}')
assert_ros_reply 1 0 "$out" 'ROS2 auto-release register 96'
out=$(ros_call /ares/v1/claims/zone '{"key":"6","robot":"96","id":[3]}')
assert_ros_reply 0 2 "$out" 'ROS2 zone 3 blocked before timeout'
assert_json '.blocked' '3' "$(ros_response_json "$out")" 'ROS2 zone 3 blocker'
sleep 15
out=$(ros_call /ares/v1/claims/zone '{"key":"6","robot":"96","id":[3]}')
assert_ros_reply 1 0 "$out" 'ROS2 zone 3 granted after timeout'

printf 'USECASE ROS2: PASS\n'
