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
    local log=$2
    local server=${SERVER_BIN:-target/debug/examples/serve_workspace}
    local workspace=${USECASE_WORKSPACE:-examples/fixed}

    SYNCBOT_PEERBUS_IDENTITY="ares-core-usecase-$$-${RANDOM}" \
    SYNCBOT_ALLOW_DEFAULT_KEY=1 \
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
