#!/usr/bin/env bash
set -euo pipefail

fail() {
    printf 'USECASE FAIL: %s\n' "$*" >&2
    exit 1
}

assert_eq() {
    local expected=$1
    local actual=$2
    local label=$3
    [[ "$actual" == "$expected" ]] || fail "$label: expected '$expected', got '$actual'"
}

assert_contains() {
    local needle=$1
    local actual=$2
    local label=$3
    [[ "$actual" == *"$needle"* ]] || fail "$label: missing '$needle' in '$actual'"
}

assert_json() {
    local expression=$1
    local expected=$2
    local actual=$3
    local label=$4
    local value
    value=$(jq -cer "$expression" <<<"$actual") || fail "$label: invalid/unexpected JSON: $actual"
    assert_eq "$expected" "$value" "$label"
}

assert_reply_xml() {
    local decision=$1
    local reason=$2
    local actual=$3
    local label=$4
    assert_contains "<decision>${decision}</decision>" "$actual" "$label decision"
    assert_contains "<reason>${reason}</reason>" "$actual" "$label reason"
}

assert_reply_json() {
    local decision=$1
    local reason=$2
    local actual=$3
    local label=$4
    assert_json '.decision' "$decision" "$actual" "$label decision"
    assert_json '.reason' "$reason" "$actual" "$label reason"
}

start_server() {
    local http_port=$1
    local zenoh_port=$2
    local log=$3
    local server=${SERVER_BIN:-target/debug/examples/serve_workspace}
    local workspace=${USECASE_WORKSPACE:-examples/fixed}

    SYNCBOT_PEERBUS_IDENTITY="ares-core-usecase-$$-${RANDOM}" \
    SYNCBOT_ZENOH_LISTEN="tcp/127.0.0.1:${zenoh_port}" \
        "$server" "$workspace" "127.0.0.1:${http_port}" >"$log" 2>&1 &
    SERVER_PID=$!

    local base="http://127.0.0.1:${http_port}/ares/v1"
    for _ in $(seq 1 120); do
        if curl -fsS "${base}/health" >/dev/null 2>&1; then
            return 0
        fi
        if ! kill -0 "$SERVER_PID" 2>/dev/null; then
            cat "$log" >&2
            fail "server exited before becoming ready"
        fi
        sleep 0.25
    done
    cat "$log" >&2
    fail "server did not become ready"
}

stop_process() {
    local pid=${1:-}
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        kill -INT "$pid" 2>/dev/null || true
        for _ in $(seq 1 40); do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.1
        done
        if kill -0 "$pid" 2>/dev/null; then
            kill -TERM "$pid" 2>/dev/null || true
        fi
        wait "$pid" 2>/dev/null || true
    fi
}

xml_post() {
    local path=$1
    local body=$2
    curl -sS -X POST "${B}${path}" -H 'content-type: application/xml' -d "$body"
}

run_register_heartbeat_claim_release_xml() {
    local out

    out=$(xml_post /robots '<reg><robot>91</robot><key>1234</key><alive>9999</alive></reg>')
    assert_reply_xml 1 0 "$out" 'register 91'
    out=$(xml_post /robots '<reg><robot>91</robot><key>1234</key></reg>')
    assert_reply_xml 0 2 "$out" 'duplicate register 91'
    out=$(xml_post /robots '<reg><robot>92</robot><alive>9999</alive></reg>')
    assert_reply_xml 1 0 "$out" 'register 92 with default key'
    out=$(xml_post /robots '<reg><robot>abc</robot></reg>')
    assert_reply_xml 0 3 "$out" 'reject invalid robot id'

    out=$(xml_post /robots/91/heartbeat '<hb><key>1234</key><zone>3</zone></hb>')
    assert_reply_xml 1 0 "$out" 'heartbeat 91'
    out=$(xml_post /robots/91/heartbeat '<hb><key>9999</key><zone>3</zone></hb>')
    assert_reply_xml 0 1 "$out" 'heartbeat wrong key'
    out=$(xml_post /robots/999/heartbeat '<hb><zone>3</zone></hb>')
    assert_reply_xml 0 2 "$out" 'heartbeat unregistered robot'
    out=$(xml_post /robots/91/heartbeat '<hb><key>1234</key><zone>-1</zone></hb>')
    assert_reply_xml 1 0 "$out" 'heartbeat unknown location'

    out=$(xml_post /robots '<reg><robot>94</robot><key>4444</key><alive>9999</alive></reg>')
    assert_reply_xml 1 0 "$out" 'register 94 claim setup'
    out=$(xml_post /claims/zone '<claim><key>1234</key><robot>91</robot><id>5</id></claim>')
    assert_reply_xml 1 0 "$out" 'claim zone 5 by 91'
    out=$(xml_post /claims/zone '<claim><robot>92</robot><id>5</id></claim>')
    assert_reply_xml 0 2 "$out" 'claim conflict on zone 5'
    assert_contains '<blocked>5</blocked>' "$out" 'claim conflict blocked zone'
    out=$(xml_post /claims/zone '<claim><robot>92</robot><id>6</id><id>5</id></claim>')
    assert_reply_xml 0 2 "$out" 'atomic multi-claim conflict'
    assert_contains '<blocked>5</blocked>' "$out" 'atomic multi-claim blocked zone'
    out=$(xml_post /claims/zone '<claim><robot>92</robot><id>6</id></claim>')
    assert_reply_xml 1 0 "$out" 'claim zone 6 by 92'
    out=$(xml_post /claims/zone '<claim><key>4444</key><robot>94</robot><id>7</id><access_mode>1</access_mode><lease_time>30</lease_time></claim>')
    assert_reply_xml 1 0 "$out" 'exclusive timed claim'
    out=$(xml_post /claims/zone '<claim><key>1234</key><robot>91</robot><id>1</id><access_mode>2</access_mode></claim>')
    assert_reply_xml 1 0 "$out" 'shared access mode on free exclusive zone'
    out=$(xml_post /claims/zone '<claim><key>1234</key><robot>91</robot><id>2</id><access_mode>3</access_mode></claim>')
    assert_reply_xml 0 5 "$out" 'reserved access mode'
    out=$(xml_post /claims/zone '<claim><key>1234</key><robot>91</robot><id>99</id></claim>')
    assert_reply_xml 0 4 "$out" 'unknown zone claim'
    assert_contains '<blocked>99</blocked>' "$out" 'unknown zone blocked id'
    out=$(xml_post /claims/zone '<claim><key>0000</key><robot>91</robot><id>2</id></claim>')
    assert_reply_xml 0 1 "$out" 'claim wrong key'

    out=$(xml_post /leases/release/zone '<rel><key>1234</key><robot>91</robot><id>5</id></rel>')
    assert_reply_xml 1 0 "$out" 'release zone 5'
    out=$(xml_post /leases/release/zone '<rel><key>1234</key><robot>91</robot><id>5</id></rel>')
    assert_reply_xml 0 2 "$out" 'release missing lease'
    out=$(xml_post /claims/zone '<claim><robot>92</robot><id>5</id></claim>')
    assert_reply_xml 1 0 "$out" 'claim released zone 5'
}

ros_call() {
    local service=$1
    local request=$2
    local type=${ROS2_SERVICE_TYPE:-ares_interfaces/srv/Json}
    local timeout_seconds=${ROS2_CALL_TIMEOUT:-20}
    local -a command
    if [[ -n "${ROS2_PYTHON:-}" ]]; then
        command=("$ROS2_PYTHON" "${ROS2_BIN:-/opt/ros/jazzy/bin/ros2}")
    else
        command=("${ROS2_BIN:-ros2}")
    fi
    timeout "${timeout_seconds}s" "${command[@]}" service call "$service" "$type" \
        "{request: '${request}'}"
}

ros_response_json() {
    sed -n "s/^.*response='\(.*\)'.*$/\1/p" <<<"$1" | tail -n 1
}

assert_ros_reply() {
    local decision=$1
    local reason=$2
    local actual=$3
    local label=$4
    assert_contains 'success=True' "$actual" "$label success"
    local response
    response=$(ros_response_json "$actual")
    [[ -n "$response" ]] || fail "$label: could not extract response JSON from '$actual'"
    assert_reply_json "$decision" "$reason" "$response" "$label"
}

start_bridge() {
    local zenoh_port=$1
    local domain=$2
    local log=$3
    local bridge=${ROS2_BRIDGE:-zenoh-bridge-ros2dds}
    "$bridge" -e "tcp/127.0.0.1:${zenoh_port}" --domain "$domain" \
        --ros-localhost-only >"$log" 2>&1 &
    BRIDGE_PID=$!
    sleep 2
    if ! kill -0 "$BRIDGE_PID" 2>/dev/null; then
        cat "$log" >&2
        fail "zenoh-bridge-ros2dds exited during startup"
    fi
}

run_register_heartbeat_claim_release_ros2() {
    local out

    out=$(ros_call /ares/v1/robots/register '{"robot":"91","key":"1234","alive":9999}')
    assert_ros_reply 1 0 "$out" 'ROS2 register 91'
    out=$(ros_call /ares/v1/robots/register '{"robot":"91","key":"1234"}')
    assert_ros_reply 0 2 "$out" 'ROS2 duplicate register 91'
    out=$(ros_call /ares/v1/robots/register '{"robot":"92","alive":9999}')
    assert_ros_reply 1 0 "$out" 'ROS2 register 92 with default key'
    out=$(ros_call /ares/v1/robots/register '{"robot":"abc"}')
    assert_ros_reply 0 3 "$out" 'ROS2 reject invalid robot id'

    out=$(ros_call /ares/v1/robots/heartbeat '{"robot":"91","key":"1234","zone":3}')
    assert_ros_reply 1 0 "$out" 'ROS2 heartbeat 91'
    out=$(ros_call /ares/v1/robots/heartbeat '{"robot":"91","key":"9999","zone":3}')
    assert_ros_reply 0 1 "$out" 'ROS2 heartbeat wrong key'
    out=$(ros_call /ares/v1/robots/heartbeat '{"robot":"999","zone":3}')
    assert_ros_reply 0 2 "$out" 'ROS2 heartbeat unregistered robot'
    out=$(ros_call /ares/v1/robots/heartbeat '{"robot":"91","key":"1234","zone":-1}')
    assert_ros_reply 1 0 "$out" 'ROS2 heartbeat unknown location'

    out=$(ros_call /ares/v1/robots/register '{"robot":"94","key":"4444","alive":9999}')
    assert_ros_reply 1 0 "$out" 'ROS2 register 94 claim setup'
    out=$(ros_call /ares/v1/claims/zone '{"key":"1234","robot":"91","id":[5]}')
    assert_ros_reply 1 0 "$out" 'ROS2 claim zone 5 by 91'
    out=$(ros_call /ares/v1/claims/zone '{"robot":"92","id":[5]}')
    assert_ros_reply 0 2 "$out" 'ROS2 claim conflict on zone 5'
    assert_json '.blocked' '5' "$(ros_response_json "$out")" 'ROS2 claim conflict blocked zone'
    out=$(ros_call /ares/v1/claims/zone '{"robot":"92","id":[6,5]}')
    assert_ros_reply 0 2 "$out" 'ROS2 atomic multi-claim conflict'
    assert_json '.blocked' '5' "$(ros_response_json "$out")" 'ROS2 atomic blocked zone'
    out=$(ros_call /ares/v1/claims/zone '{"robot":"92","id":[6]}')
    assert_ros_reply 1 0 "$out" 'ROS2 claim zone 6 by 92'
    out=$(ros_call /ares/v1/claims/zone '{"key":"4444","robot":"94","id":[7],"access_mode":1,"lease_time":30}')
    assert_ros_reply 1 0 "$out" 'ROS2 exclusive timed claim'
    out=$(ros_call /ares/v1/claims/zone '{"key":"1234","robot":"91","id":[1],"access_mode":2}')
    assert_ros_reply 1 0 "$out" 'ROS2 shared access mode'
    out=$(ros_call /ares/v1/claims/zone '{"key":"1234","robot":"91","id":[2],"access_mode":3}')
    assert_ros_reply 0 5 "$out" 'ROS2 reserved access mode'
    out=$(ros_call /ares/v1/claims/zone '{"key":"1234","robot":"91","id":[99]}')
    assert_ros_reply 0 4 "$out" 'ROS2 unknown zone claim'
    assert_json '.blocked' '99' "$(ros_response_json "$out")" 'ROS2 unknown blocked id'
    out=$(ros_call /ares/v1/claims/zone '{"key":"0000","robot":"91","id":[2]}')
    assert_ros_reply 0 1 "$out" 'ROS2 claim wrong key'

    out=$(ros_call /ares/v1/leases/release/zone '{"key":"1234","robot":"91","id":5}')
    assert_ros_reply 1 0 "$out" 'ROS2 release zone 5'
    out=$(ros_call /ares/v1/leases/release/zone '{"key":"1234","robot":"91","id":5}')
    assert_ros_reply 0 2 "$out" 'ROS2 release missing lease'
    out=$(ros_call /ares/v1/claims/zone '{"robot":"92","id":[5]}')
    assert_ros_reply 1 0 "$out" 'ROS2 claim released zone 5'
}
