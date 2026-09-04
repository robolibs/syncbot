"""Exercise the syncbot Python surface end to end.

Run with `make check` in this directory; `make ci` at the repo root runs it
too. It is a test, not a demo — every assertion is something a binding could
plausibly get wrong, and several of them have.

No rerun and no on-disk fixture: the workspace is built through the bindings,
which is the thing most worth proving.
"""
from __future__ import annotations

import sys

import syncbot


def box(min_x, min_y, max_x, max_y):
    return [(min_x, min_y), (max_x, min_y), (max_x, max_y), (min_x, max_y)]


def build_yard():
    """Two nodes either side of an exclusive single-lane bridge."""
    b = syncbot.WorkspaceBuilder(
        "yard", box(0, 0, 150, 185), (52.6619, 5.7482, 0.0),
        properties={"traffic.policy": "shared"},
    )
    b.add_zone("bridge", box(60, 58, 90, 92), numeric_id=3,
               properties={"traffic.policy": "exclusive",
                           "traffic.claim_required": "true"})
    b.add_zone("apron", box(10, 95, 140, 135), numeric_id=4,
               properties={"traffic.policy": "shared", "traffic.capacity": "3"})
    for name, numeric, x, y in [("buf", 1001, 35.0, 30.0),
                                ("weigh_in", 1002, 75.0, 62.0),
                                ("weigh_out", 1003, 75.0, 88.0),
                                ("apron_mid", 1004, 75.0, 115.0)]:
        b.add_node(name, x, y, numeric_id=numeric)
    for i, (s, t, w) in enumerate([("buf", "weigh_in", 34.0),
                                   ("weigh_in", "weigh_out", 26.0),
                                   ("weigh_out", "apron_mid", 27.0)]):
        b.add_edge(s, t, weight=w, numeric_id=2001 + i,
                   properties={"traffic.lane_type": "corridor"})
    return b.build()


def check_workspace(index):
    zones = {z["name"]: z for z in index.zones()}
    assert set(zones) == {"yard", "bridge", "apron"}, zones.keys()
    assert zones["bridge"]["numeric_id"] == 3
    assert zones["bridge"]["policy"]["kind"] == "ExclusiveAccess"
    assert zones["apron"]["policy"]["capacity"] == 3
    assert len(zones["bridge"]["boundary"]) == 4

    nodes = {n["name"]: n for n in index.nodes()}
    assert len(nodes) == 4, nodes.keys()
    assert nodes["weigh_in"]["numeric_id"] == 1002
    assert (nodes["weigh_in"]["x"], nodes["weigh_in"]["y"]) == (75.0, 62.0)

    edges = index.edges()
    assert len(edges) == 3
    deck = next(e for e in edges if e["numeric_id"] == 2002)
    assert deck["source"] == nodes["weigh_in"]["id"]
    assert deck["target"] == nodes["weigh_out"]["id"]

    # Regression: `add_edge` alone does not compute zone membership. Without
    # the builder's refresh an edge claim implies intent on no zone at all,
    # and the exclusive bridge would never register as held.
    assert zones["bridge"]["id"] in index.zones_of_edge(deck["id"]), \
        "the bridge deck must belong to the bridge zone"
    assert zones["bridge"]["id"] in index.zones_of_node(nodes["weigh_in"]["id"])

    assert index.node_by_numeric_id(1002) == nodes["weigh_in"]["id"]
    assert index.zone_by_numeric_id(3) == zones["bridge"]["id"]
    assert index.edge_by_numeric_id(2002) == deck["id"]
    assert index.edge_between(nodes["weigh_in"]["id"],
                              nodes["weigh_out"]["id"]) == deck["id"]

    # A round-trip through the datum lands back where it started.
    geo = index.local_to_global(75.0, 62.0, 0.0)
    back = index.global_to_local(geo["lat"], geo["lon"], geo["alt"])
    assert abs(back["x"] - 75.0) < 1e-6 and abs(back["y"] - 62.0) < 1e-6, back
    return nodes, zones


def check_routing(index, nodes):
    # Unpenalised, the cost is exactly the graph weight.
    raw = syncbot.plan_route(index, nodes["buf"]["id"], nodes["apron_mid"]["id"],
                             use_penalties=False)
    assert raw["found"]
    assert raw["plan"]["total_cost"] == 34.0 + 26.0 + 27.0, raw["plan"]["total_cost"]

    # Penalised — the default — costs more, because the route crosses an
    # exclusive claim-required zone and a capacity-limited one.
    result = syncbot.plan_route(index, nodes["buf"]["id"], nodes["apron_mid"]["id"])
    plan = result["plan"]
    assert plan["total_cost"] > raw["plan"]["total_cost"], plan["total_cost"]
    assert len(plan["traversed_node_ids"]) == 4
    assert len(plan["traversed_edge_ids"]) == 3
    assert plan["steps"][0]["cumulative_cost"] == 0.0
    assert plan["steps"][-1]["cumulative_cost"] == plan["total_cost"]

    same = syncbot.plan_route(index, nodes["buf"]["id"], nodes["buf"]["id"])
    assert same["found"], "a zero-length route to itself still resolves"
    return plan


def check_claims(index, plan, nodes, zones):
    """The rolling horizon: claim ahead, conflict, move on, release behind."""
    coord = syncbot.Coordinator(index)
    for robot in (1, 2):
        coord.register_robot({"robot_id": robot, "horizon": 2})
        assert coord.assign_route_plan(robot, plan, 2, 0)
    assert coord.robot_count() == 2

    first = coord.claim_request_for_robot(1, 1, "exclusive")
    kinds = [t["kind"] for t in first["targets"]]
    assert "Edge" in kinds and "Node" in kinds
    assert "Zone" not in kinds, "zones are derived from node/edge intent, never claimed"
    assert coord.evaluate_claim(first)["decision"] == "Grant"
    coord.upsert_claim_request_for_robot(first)
    assert coord.claim_request_count() == 1

    # Robot 2 wants the same ground and is refused — this is the bridge.
    second = coord.claim_request_for_robot(2, 2, "exclusive")
    denial = coord.evaluate_claim(second)
    assert denial["decision"] == "Deny", denial
    assert denial["blocking_target"] is not None
    decision = coord.schedule_robot_route(2, 2, 0, 1.0, "exclusive")
    assert decision["kind"] in ("Queue", "Replan"), decision

    # The claim ledger says the bridge is held, derived from the edge claim.
    held = set()
    for request in coord.claim_manager_requests():
        for target in request["targets"]:
            if target["kind"] == "Edge":
                held.update(index.zones_of_edge(target["resource_id"]))
            elif target["kind"] == "Node":
                held.update(index.zones_of_node(target["resource_id"]))
    assert zones["bridge"]["id"] in held, "an edge claim must imply zone intent"

    # Robot 1 moves on and gives the ground back; robot 2 gets it.
    coord.update_robot_progress(1, plan["traversed_node_ids"][2], None, 5)
    coord.release_behind_progress(1)
    assert coord.remove_claim_requests_for_robot(1) == 1
    assert coord.claim_request_count() == 0
    assert coord.evaluate_claim(coord.claim_request_for_robot(2, 2))["decision"] == "Grant"
    return coord


def check_poses_and_liveness(coord, nodes):
    assert coord.update_robot_pose(1, x=75.0, y=62.0, yaw=1.57, now_ms=1000)
    state = coord.robot_state(1)
    assert state["position"]["x"] == 75.0
    assert state["position"]["converted"], "a datum is bound, so lat/lon is derived"
    assert abs(state["position"]["lat"] - 52.66) < 0.01, state["position"]
    assert abs(state["heading"]["bearing_deg"] - 0.05) < 0.5, state["heading"]

    coord.set_alive(1, 1, 1000)
    assert coord.robot_active_at(1, 1500)
    assert not coord.robot_active_at(1, 60_000), "two missed intervals is inactive"
    assert coord.inactive_robots_at(60_000) == [1]

    # A robot that has gone quiet must not keep holding ground. The sweep
    # frees what it held and reports it; it does *not* unregister the robot,
    # which stays known so it can come back.
    coord.upsert_claim_request_for_robot(coord.claim_request_for_robot(1, 1))
    assert coord.claim_request_count() == 1
    assert coord.sweep_inactive(60_000) == [1]
    assert coord.claim_request_count() == 0, "the sweep released the claim"
    assert coord.robot_count() == 2, "the robot itself is still registered"
    assert coord.sweep_inactive(60_000) == [], "nothing left to free"

    snapshot = coord.snapshot()
    assert "robot_states" in snapshot, snapshot.keys()


def check_arbitration():
    assert syncbot.arbitrate_right_of_way(self_is_emergency=True) == "proceed"
    assert syncbot.arbitrate_right_of_way(other_is_emergency=True) == "yield"
    assert syncbot.arbitrate_right_of_way(self_holds_lease=True) == "proceed"
    assert syncbot.arbitrate_right_of_way(
        self_priority=1.0, other_priority=9.0) == "yield"
    # Nothing to separate them.
    assert syncbot.arbitrate_right_of_way() == "replan"


def check_claim_manager(index):
    mgr = syncbot.ClaimManager(index)
    assert mgr.empty()
    node = index.nodes()[0]["id"]
    request = {
        "id": 7, "robot_id": 3, "mission_id": 0,
        "access_mode": "Exclusive", "priority": 0,
        "targets": [{"kind": "Node", "resource_id": node}],
    }
    assert mgr.evaluate_request(request)["decision"] == "Grant"
    mgr.upsert_request_for_robot(request)
    assert mgr.request_count() == 1
    assert mgr.next_request_id() > 0
    assert mgr.leases_for_robot(3) == []
    assert mgr.remove_requests_for_robot(3) == 1
    assert mgr.empty()


def check_traffic_parsers():
    assert syncbot.parse_traffic_bool("yes") is True
    assert syncbot.parse_traffic_u64("3") == 3
    assert syncbot.parse_traffic_f64("0.5") == 0.5
    policy = syncbot.parse_zone_policy({"traffic.policy": "exclusive",
                                        "traffic.capacity": "2"})
    assert policy["kind"] == "ExclusiveAccess" and policy["capacity"] == 2
    issues = syncbot.validate_zone_traffic_properties({"traffic.bogus": "x"})
    assert len(issues) == 1 and issues[0]["severity"] == "Warning"


def main():
    workspace = build_yard()
    assert workspace.node_count() == 4 and workspace.edge_count() == 3
    assert workspace.datum()["lat"] == 52.6619

    index = syncbot.WorkspaceIndex(workspace)
    assert index.is_valid(), index.validation_issues()

    nodes, zones = check_workspace(index)
    plan = check_routing(index, nodes)
    coord = check_claims(index, plan, nodes, zones)
    check_poses_and_liveness(coord, nodes)
    check_arbitration()
    check_claim_manager(index)
    check_traffic_parsers()

    order = syncbot.vda_order_from_route(plan)
    assert order["nodes"], "a VDA order carries the route's nodes"

    print(f"python bindings ok: syncbot {syncbot.version()}, "
          f"{len(index.zones())} zones, {len(index.nodes())} nodes, "
          f"{len(index.edges())} edges")
    return 0


if __name__ == "__main__":
    sys.exit(main())
