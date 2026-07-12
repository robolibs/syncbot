#!/usr/bin/env bash
set -eo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$ROOT"

source "${ROS_SETUP:-/opt/ros/jazzy/setup.bash}"
source "${ROS2_INTERFACE_SETUP:-target/usecase-ros2/install/share/ares_interfaces/local_setup.bash}"
set -u
source tests/usecase/common.sh

HTTP_PORT=${USECASE_HTTP_PORT:-18082}
ZENOH_PORT=${USECASE_ZENOH_PORT:-17449}
ROS_DOMAIN_ID=${ROS_DOMAIN_ID:-88}
export ROS_DOMAIN_ID RMW_IMPLEMENTATION=${USECASE_RMW_IMPLEMENTATION:-rmw_cyclonedds_cpp}
export ROS_LOCALHOST_ONLY=1
SERVER_LOG=${USECASE_SERVER_LOG:-target/usecase-mixed-server.log}
BRIDGE_LOG=${USECASE_BRIDGE_LOG:-target/usecase-mixed-bridge.log}
SERVER_PID=
BRIDGE_PID=
trap 'stop_process "$BRIDGE_PID"; stop_process "$SERVER_PID"' EXIT

start_server "$HTTP_PORT" "$ZENOH_PORT" "$SERVER_LOG"
start_bridge "$ZENOH_PORT" "$ROS_DOMAIN_ID" "$BRIDGE_LOG"
B="http://127.0.0.1:${HTTP_PORT}/ares/v1"

out=$(xml_post /robots '<reg><robot>91</robot><key>1234</key><alive>9999</alive></reg>')
assert_reply_xml 1 0 "$out" 'mixed XML register 91'
out=$(ros_call /ares/v1/robots/register '{"robot":"92","key":"2222","alive":9999}')
assert_ros_reply 1 0 "$out" 'mixed ROS2 register 92'

out=$(xml_post /claims/zone '<claim><key>1234</key><robot>91</robot><id>4</id></claim>')
assert_reply_xml 1 0 "$out" 'mixed XML claim zone 4'
out=$(ros_call /ares/v1/claims/zone '{"key":"2222","robot":"92","id":[4]}')
assert_ros_reply 0 2 "$out" 'mixed ROS2 blocked by XML claim'
assert_json '.blocked' '4' "$(ros_response_json "$out")" 'mixed ROS2 blocked zone 4'

out=$(ros_call /ares/v1/claims/zone '{"key":"2222","robot":"92","id":[6]}')
assert_ros_reply 1 0 "$out" 'mixed ROS2 claim zone 6'
out=$(xml_post /claims/zone '<claim><key>1234</key><robot>91</robot><id>6</id></claim>')
assert_reply_xml 0 2 "$out" 'mixed XML blocked by ROS2 claim'
assert_contains '<blocked>6</blocked>' "$out" 'mixed XML blocked zone 6'

out=$(xml_post /leases/release/zone '<rel><key>1234</key><robot>91</robot><id>4</id></rel>')
assert_reply_xml 1 0 "$out" 'mixed XML release zone 4'
out=$(ros_call /ares/v1/claims/zone '{"key":"2222","robot":"92","id":[4]}')
assert_ros_reply 1 0 "$out" 'mixed ROS2 claims released zone 4'

printf 'USECASE MIXED: PASS\n'
