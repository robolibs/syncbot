"""End-to-end Python smoke test of the timenav module.

Build with::

    cd examples/python_binding && make

This script exercises the full public surface — workspace loading is skipped
because it requires an on-disk workspace fixture, but every other class and
free function is covered.
"""
from __future__ import annotations

import json
import sys

import timenav


def header(title: str) -> None:
    print(f"\n=== {title} ===")


def main() -> int:
    header("version + constants")
    print("version:", timenav.version())
    print("zone policy kinds:", timenav.ZONE_POLICY_KINDS)
    print("schedule decision kinds:", timenav.SCHEDULE_DECISION_KINDS)

    header("traffic value parsers")
    assert timenav.parse_traffic_bool("yes") is True
    assert timenav.parse_traffic_u64("3") == 3
    assert timenav.parse_traffic_f64("0.5") == 0.5

    header("zone policy classification")
    policy = timenav.parse_zone_policy({"traffic.policy": "exclusive",
                                        "traffic.capacity": "2"})
    print("policy:", json.dumps(policy, indent=2))
    assert policy["kind"] == "ExclusiveAccess"
    assert policy["capacity"] == 2

    issues = timenav.validate_zone_traffic_properties({"traffic.bogus": "x"})
    print("issues:", issues)
    assert len(issues) == 1
    assert issues[0]["severity"] == "Warning"

    header("right-of-way arbitration")
    decision = timenav.arbitrate_right_of_way(self_is_emergency=True)
    print("emergency self:", decision)
    assert decision == "proceed"

    header("ClaimManager")
    mgr = timenav.ClaimManager()
    request = {
        "id": 1, "robot_id": 1, "mission_id": 0,
        "access_mode": "Exclusive", "priority": 0,
        "requested_at_tick": None,
        "window": {"start_tick": None, "end_tick": None},
        "targets": [{"kind": "Zone",
                     "resource_id": "00000000-0000-0000-0000-000000000001"}],
    }
    mgr.add_request(request)
    print("requests:", mgr.request_count())
    eval_result = mgr.evaluate_request(request)
    print("decision:", eval_result["decision"])
    assert eval_result["decision"] == "Grant"

    lease = {
        "id": 11, "claim_id": 1, "robot_id": 1,
        "access_mode": "Exclusive",
        "targets": [{"kind": "Zone",
                     "resource_id": "00000000-0000-0000-0000-000000000001"}],
        "granted_at_tick": 0, "expires_at_tick": 100,
        "refreshed_at_tick": None, "released_at_tick": None,
        "revoked_at_tick": None, "revoke_reason": None,
        "disposition": "Active", "active": True,
    }
    mgr.add_lease(lease)
    assert mgr.lease_count() == 1
    expired = mgr.expire_leases(150)
    print("expired:", expired)
    assert expired == 1

    header("Coordinator")
    coord = timenav.Coordinator()
    coord.register_robot({
        "robot_id": 7, "mission_id": 0,
        "current_node_id": None, "current_edge_id": None,
        "route_plan": None,
        "pending_claim_ids": [], "active_lease_ids": [],
        "progress_state": "Idle", "next_route_step_index": 0,
        "hold_reason": None, "last_claim_tick": None,
        "scheduled_start_tick": None, "reserved_until_tick": None,
        "wait_ticks": 0, "needs_replan": False, "horizon": 0,
        "updated_at_tick": 0,
    })
    print("robots:", coord.robot_count())
    state = coord.robot_state(7)
    print("registered state:", state["robot_id"], state["progress_state"])

    header("VDA mapping")
    plan = {
        "start_node_id": "00000000-0000-0000-0000-000000000001",
        "goal_node_id": "00000000-0000-0000-0000-000000000002",
        "steps": [
            {"node_id": "00000000-0000-0000-0000-000000000001",
             "incoming_edge_id": None, "step_cost": 0.0, "cumulative_cost": 0.0},
            {"node_id": "00000000-0000-0000-0000-000000000002",
             "incoming_edge_id": "00000000-0000-0000-0000-0000000000aa",
             "step_cost": 1.0, "cumulative_cost": 1.0},
        ],
        "traversed_node_ids": [
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000002",
        ],
        "traversed_edge_ids": ["00000000-0000-0000-0000-0000000000aa"],
        "traversed_zone_ids": [],
        "traversed_node_zone_ids": [[], []],
        "traversed_edge_zone_ids": [[]],
        "total_cost": 1.0,
    }
    order = timenav.vda_order_from_route(plan)
    print("order edges:", len(order["edges"]), "version:", order["version"])
    assert order["version"] == "3.0.0"

    print("\nall checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
