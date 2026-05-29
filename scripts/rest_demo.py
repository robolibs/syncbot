#!/usr/bin/env python3
"""Two-robot zone-claim simulation against the timenav REST adapter.

The companion Rust server (`examples/rest_server.rs`) boots a workspace
with three exclusive zones (numeric IDs 100/101/102). This script drives
two robots through two phases:

  Phase 1 — robots target different zones: both granted, in parallel.
  Phase 2 — robots target the same zone: one is granted, the other
            waits, then succeeds after the first releases.

Run the server first (different terminal):
    cargo run --example rest_server --features rest

Then:
    python3 scripts/rest_demo.py

No Python dependencies — uses only the standard library.
"""

import json
import sys
import threading
import time
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:8080/ares/v1"

# UUIDs assigned by examples/rest_server.rs. The script could also use the
# numeric_id form (e.g. "100") for evaluate / submit calls, but leases need
# the resolved UUID since core types don't accept ResourceRef in storage.
DOCK_A = "00000000-0000-0000-0000-000000000100"
DOCK_B = "00000000-0000-0000-0000-000000000101"
JUNCTION = "00000000-0000-0000-0000-000000000102"


def http(method, path, body=None):
    """Tiny HTTP client. Returns (status, parsed_json_or_raw_text_or_None)."""
    data = None if body is None else json.dumps(body).encode("utf-8")
    headers = {"Content-Type": "application/json"} if data is not None else {}
    request = urllib.request.Request(
        f"{BASE}{path}", data=data, headers=headers, method=method
    )
    try:
        with urllib.request.urlopen(request) as resp:
            text = resp.read().decode("utf-8")
            return resp.status, _parse(text)
    except urllib.error.HTTPError as e:
        text = e.read().decode("utf-8")
        raise RuntimeError(f"{method} {path} -> HTTP {e.code}: {text}") from None


def _parse(text):
    if not text:
        return None
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return text


def register(robot_id, label):
    status, _ = http("POST", "/robots", {"robot_id": robot_id})
    print(f"  registered {label} (robot_id={robot_id})  HTTP {status}")


def claim_body(claim_id, robot_id, resource_id):
    return {
        "id": claim_id,
        "robot_id": robot_id,
        "access_mode": "Exclusive",
        "targets": [{"kind": "Zone", "resource_id": str(resource_id)}],
    }


def lease_body(lease_id, claim_id, robot_id, zone_uuid):
    return {
        "id": lease_id,
        "claim_id": claim_id,
        "robot_id": robot_id,
        "access_mode": "Exclusive",
        "targets": [{"kind": "Zone", "resource_id": zone_uuid}],
    }


def evaluate(claim_id, robot_id, resource_id):
    _, body = http("POST", "/claims/evaluate", claim_body(claim_id, robot_id, resource_id))
    return body


def acquire(label, robot_id, claim_id, lease_id, zone_uuid, zone_name):
    result = evaluate(claim_id, robot_id, zone_uuid)
    if result["decision"] != "Grant":
        print(f"  {label}: DENIED on {zone_name} — {result.get('reason')}")
        return False
    http("POST", "/leases", lease_body(lease_id, claim_id, robot_id, zone_uuid))
    print(f"  {label}: GRANTED on {zone_name} (lease {lease_id})")
    return True


def release(label, lease_id):
    http("POST", "/leases/release", {"lease_id": lease_id})
    print(f"  {label}: released lease {lease_id}")


def wait_for(label, claim_id, robot_id, zone_uuid, zone_name, tries=10, delay=0.4):
    for attempt in range(1, tries + 1):
        result = evaluate(claim_id, robot_id, zone_uuid)
        if result["decision"] == "Grant":
            print(f"  {label}: {zone_name} is free (after {attempt} polls)")
            return True
        reason = result.get("reason") or "blocked"
        print(f"  {label}: still waiting ({reason})  poll {attempt}/{tries}")
        time.sleep(delay)
    return False


def phase_one():
    print()
    print("=" * 64)
    print(" Phase 1 — different zones, no conflict")
    print("=" * 64)
    a = acquire("robot 1", 1, claim_id=11, lease_id=111,
                zone_uuid=DOCK_A, zone_name="dock_a")
    b = acquire("robot 2", 2, claim_id=22, lease_id=222,
                zone_uuid=DOCK_B, zone_name="dock_b")
    if not (a and b):
        print("  unexpected: both claims should have been granted")
    release("robot 1", 111)
    release("robot 2", 222)


def phase_two():
    print()
    print("=" * 64)
    print(" Phase 2 — same zone, one waits")
    print("=" * 64)
    acquire("robot 1", 1, claim_id=33, lease_id=333,
            zone_uuid=JUNCTION, zone_name="junction")

    # Robot 2 evaluates once first to show the initial Deny.
    result = evaluate(44, 2, JUNCTION)
    print(f"  robot 2: DENIED on junction — {result.get('reason')}")

    # Robot 1 will release its lease after a short delay, simulating
    # finishing its work. Meanwhile robot 2 polls until the zone frees.
    def robot1_finishes():
        time.sleep(1.5)
        print("  robot 1: work complete, releasing junction")
        release("robot 1", 333)

    threading.Thread(target=robot1_finishes, daemon=True).start()

    print("  robot 2: waiting for junction to free up...")
    if wait_for("robot 2", claim_id=44, robot_id=2,
                zone_uuid=JUNCTION, zone_name="junction"):
        http("POST", "/leases", lease_body(444, 44, 2, JUNCTION))
        print("  robot 2: acquired junction (lease 444)")
        release("robot 2", 444)
    else:
        print("  robot 2: gave up after retries")


def main():
    try:
        status, _ = http("GET", "/health")
        if status != 200:
            raise urllib.error.URLError(f"health returned {status}")
    except (urllib.error.URLError, ConnectionRefusedError):
        print(f"could not reach timenav server at {BASE}")
        print("start it first:")
        print("    cargo run --example rest_server --features rest")
        sys.exit(1)

    print(f"connected to timenav at {BASE}")
    print("registering robots...")
    register(1, "robot 1")
    register(2, "robot 2")
    phase_one()
    phase_two()
    print()
    print("simulation complete.")


if __name__ == "__main__":
    main()
