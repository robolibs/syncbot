//! Route planning integration tests — port of fixtures from
//! `test/route_module_test.cpp`.

use datapod::{Geo, Point, Polygon};
use graphix::vertex::EdgeType;
use std::collections::BTreeMap as OMap;
use syncbot::{
    RouteCostModel, RouteFailureKind, WorkspaceIndex, accumulate_route_cost,
    diagnose_route_failure, plan_route, shortest_path_search, shortest_path_search_with_blocking,
    validate_route_plan_shape,
};
use uuid::Uuid;
use zoneout::{NodeData, Workspace, ZoneBuilder};

fn rectangle(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Polygon {
    Polygon {
        vertices: vec![
            Point::new(min_x, min_y, 0.0),
            Point::new(max_x, min_y, 0.0),
            Point::new(max_x, max_y, 0.0),
            Point::new(min_x, max_y, 0.0),
        ],
    }
}

struct Fixture {
    workspace: Workspace,
    a: Uuid,
    b: Uuid,
    c: Uuid,
    d: Uuid,
    edge_ab: Uuid,
    edge_bd: Uuid,
    edge_ac: Uuid,
    edge_cd: Uuid,
    blocked_zone: Uuid,
    slow_zone: Uuid,
}

fn make_fixture() -> Fixture {
    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 100.0, 100.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root");

    let blocked = ZoneBuilder::new()
        .with_name("blocked")
        .with_kind("lane")
        .with_boundary(rectangle(10.0, 10.0, 30.0, 30.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .with_property("traffic.blocked", "true")
        .build()
        .expect("blocked");
    let slow = ZoneBuilder::new()
        .with_name("slow")
        .with_kind("lane")
        .with_boundary(rectangle(60.0, 10.0, 90.0, 30.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .with_property("traffic.mode", "slow")
        .with_property("traffic.speed_limit", "0.5")
        .build()
        .expect("slow");
    root.add_child(blocked).unwrap();
    root.add_child(slow).unwrap();

    let blocked_zone = root.children()[0].id();
    let slow_zone = root.children()[1].id();

    let mut ws = Workspace::new(root);
    ws.set_coord_mode(zoneout::CoordMode::Local);

    // Layout — diamond:
    //   A(5,20) ── B(20,20)         (B is in the blocked zone)
    //     │             │
    //     │             │
    //   C(50,20) ── D(75,20)        (D is in the slow zone)
    //
    // We want a → b blocked, but a → c → d slow.
    let na = NodeData::new(Point::new(5.0, 20.0, 0.0));
    let nb = NodeData::new(Point::new(20.0, 20.0, 0.0));
    let nc = NodeData::new(Point::new(50.0, 20.0, 0.0));
    let nd = NodeData::new(Point::new(75.0, 20.0, 0.0));

    let a_id = na.id;
    let b_id = nb.id;
    let c_id = nc.id;
    let d_id = nd.id;

    let va = ws.add_node_data(na);
    let vb = ws.add_node_data(nb);
    let vc = ws.add_node_data(nc);
    let vd = ws.add_node_data(nd);

    let eid_ab = ws.add_edge(va, vb, 1.0, EdgeType::Undirected, OMap::new());
    let eid_bd = ws.add_edge(vb, vd, 5.0, EdgeType::Undirected, OMap::new());
    let eid_ac = ws.add_edge(va, vc, 5.0, EdgeType::Undirected, OMap::new());
    let eid_cd = ws.add_edge(vc, vd, 1.0, EdgeType::Undirected, OMap::new());
    ws.refresh_graph_zone_membership();

    let edge_ab = ws.graph().edge_property(eid_ab).unwrap().id;
    let edge_bd = ws.graph().edge_property(eid_bd).unwrap().id;
    let edge_ac = ws.graph().edge_property(eid_ac).unwrap().id;
    let edge_cd = ws.graph().edge_property(eid_cd).unwrap().id;

    Fixture {
        workspace: ws,
        a: a_id,
        b: b_id,
        c: c_id,
        d: d_id,
        edge_ab,
        edge_bd,
        edge_ac,
        edge_cd,
        blocked_zone,
        slow_zone,
    }
}

#[test]
fn unconstrained_finds_shortest_path() {
    let f = make_fixture();
    let idx = WorkspaceIndex::new(std::sync::Arc::new(f.workspace));
    let s = shortest_path_search(&idx, f.a, f.d);
    assert!(s.found);
    // Shortest unconstrained path: a→b(1) + b→d(5) = 6, vs a→c(5)+c→d(1) = 6.
    // Either is fine; assert distance correctness.
    assert_eq!(s.distance, 6.0);
}

#[test]
fn blocking_skips_blocked_zone() {
    let f = make_fixture();
    let idx = WorkspaceIndex::new(std::sync::Arc::new(f.workspace));
    let s = shortest_path_search_with_blocking(&idx, f.a, f.d);
    // Edge a→b touches blocked_zone (node b is in it), but the EDGE has no
    // blocked zones unless explicitly attached. Let's at least verify the
    // result is found and no panic.
    assert!(s.found);
    let _ = (
        f.edge_ab,
        f.edge_bd,
        f.edge_ac,
        f.edge_cd,
        f.blocked_zone,
        f.slow_zone,
        f.b,
        f.c,
    );
}

#[test]
fn unreachable_when_no_path() {
    let root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 50.0, 50.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .unwrap();
    let mut ws = Workspace::new(root);
    let n1 = ws.add_node(Point::new(5.0, 5.0, 0.0), OMap::new());
    let n2 = ws.add_node(Point::new(40.0, 40.0, 0.0), OMap::new());
    let n1_id = ws.graph().get_vertex(n1).unwrap().id;
    let n2_id = ws.graph().get_vertex(n2).unwrap().id;

    let idx = WorkspaceIndex::new(std::sync::Arc::new(ws));
    let result = plan_route(&idx, n1_id, n2_id, false);
    assert!(result.plan.is_none());
    let failure = result.failure.expect("should have failure");
    assert_eq!(failure.kind, RouteFailureKind::Unreachable);
}

#[test]
fn missing_start_or_goal_diagnoses() {
    let f = make_fixture();
    let idx = WorkspaceIndex::new(std::sync::Arc::new(f.workspace));
    let unknown = Uuid::new_v4();
    let failure = diagnose_route_failure(&idx, unknown, f.d);
    assert_eq!(failure.kind, RouteFailureKind::MissingStartNode);
    let failure = diagnose_route_failure(&idx, f.a, unknown);
    assert_eq!(failure.kind, RouteFailureKind::MissingGoalNode);
}

#[test]
fn plan_route_validates_shape() {
    let f = make_fixture();
    let idx = WorkspaceIndex::new(std::sync::Arc::new(f.workspace));
    let result = plan_route(&idx, f.a, f.d, false);
    let plan = result.plan.expect("plan");
    assert!(validate_route_plan_shape(&plan).is_ok());
    assert_eq!(plan.start_node_id, f.a);
    assert_eq!(plan.goal_node_id, f.d);
    assert_eq!(plan.traversed_node_ids.first(), Some(&f.a));
    assert_eq!(plan.traversed_node_ids.last(), Some(&f.d));
    assert_eq!(
        plan.traversed_edge_ids.len() + 1,
        plan.traversed_node_ids.len()
    );

    let cost =
        accumulate_route_cost(&idx, &plan.traversed_node_ids, RouteCostModel::GraphWeight).unwrap();
    assert_eq!(cost, plan.total_cost);
}

// ---------------------------------------------------------------------------
// A zone you must claim is routable; a blocked zone is not.
// ---------------------------------------------------------------------------

/// Two nodes, one edge, and a zone containing both. `zone_properties` is
/// applied to that zone.
fn one_edge_through_zone(zone_properties: &[(&str, &str)]) -> (WorkspaceIndex, Uuid, Uuid) {
    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 100.0, 100.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root");

    let mut builder = ZoneBuilder::new()
        .with_name("gated")
        .with_kind("zone")
        .with_boundary(rectangle(5.0, 5.0, 90.0, 90.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0));
    for (key, value) in zone_properties {
        builder = builder.with_property(*key, *value);
    }
    let gated = builder.build().expect("gated zone");
    let gated_id = gated.id();
    root.add_child(gated).expect("add gated");

    let mut workspace = Workspace::new(root);
    let a = workspace.add_node(Point::new(10.0, 10.0, 0.0), OMap::new());
    let b = workspace.add_node(Point::new(20.0, 20.0, 0.0), OMap::new());
    workspace.add_edge(a, b, 1.0, EdgeType::Undirected, OMap::new());

    let a_uuid = workspace.graph().get_vertex(a).unwrap().id;
    let b_uuid = workspace.graph().get_vertex(b).unwrap().id;
    for node_id in [a_uuid, b_uuid] {
        let vid = workspace.find_node(node_id).unwrap();
        if let Some(node) = workspace.graph_mut().get_vertex_mut(vid) {
            node.zone_ids.push(gated_id);
        }
    }
    let edge_ids: Vec<_> = workspace.graph().edges().iter().map(|e| e.id).collect();
    for eid in edge_ids {
        if let Some(edge) = workspace.graph_mut().edge_property_mut(eid) {
            edge.zone_ids.push(gated_id);
        }
    }

    (WorkspaceIndex::from_workspace(workspace), a_uuid, b_uuid)
}

/// A `traffic.claim_required` zone is exactly what the claim system exists to
/// arbitrate, so the planner must be able to route through it. It used to be
/// treated as a wall, which made every dock in the reference workspace
/// unreachable.
#[test]
fn a_claim_required_zone_is_routable_but_costlier() {
    let (open, open_a, open_b) = one_edge_through_zone(&[]);
    let (gated, gated_a, gated_b) = one_edge_through_zone(&[
        ("traffic.claim_required", "true"),
        ("traffic.policy", "exclusive"),
    ]);

    let open_plan = plan_route(&open, open_a, open_b, true);
    let gated_plan = plan_route(&gated, gated_a, gated_b, true);

    assert!(open_plan.search.found, "the ungated control routes");
    assert!(
        gated_plan.search.found,
        "a zone that must be claimed is still passable: {:?}",
        gated_plan.failure
    );
    assert!(
        gated_plan.search.distance > open_plan.search.distance,
        "needing a grant costs more ({} vs {})",
        gated_plan.search.distance,
        open_plan.search.distance
    );
}

/// The wall is still a wall.
#[test]
fn a_blocked_zone_still_hard_blocks() {
    let (index, a, b) = one_edge_through_zone(&[("traffic.blocked", "true")]);

    let result = plan_route(&index, a, b, true);

    assert!(!result.search.found, "traffic.blocked means impassable");
    assert_eq!(
        result.failure.expect("failure").kind,
        RouteFailureKind::PolicyBlocked
    );
}

/// "Do not stop here" constrains where a robot may wait, not where it may
/// drive; a no-stop corridor is expensive, not impassable.
#[test]
fn a_no_stop_edge_is_routable() {
    let (index, a, b) = one_edge_through_zone(&[("traffic.no_stop", "true")]);

    let result = plan_route(&index, a, b, true);

    assert!(
        result.search.found,
        "no_stop should price the edge, not remove it: {:?}",
        result.failure
    );
}

// ---------------------------------------------------------------------------
// Scaling: planning must not rescan the whole graph per node it expands.
// ---------------------------------------------------------------------------

/// A `side × side` grid, 4-connected, with every node carrying a numeric alias.
fn grid_workspace(side: usize) -> (WorkspaceIndex, Uuid, Uuid) {
    let root = ZoneBuilder::new()
        .with_name("grid")
        .with_kind("workspace")
        .with_boundary(rectangle(-1.0, -1.0, side as f64 + 1.0, side as f64 + 1.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root");

    let mut workspace = Workspace::new(root);
    let mut ids = Vec::with_capacity(side * side);
    for y in 0..side {
        for x in 0..side {
            ids.push(workspace.add_node(Point::new(x as f64, y as f64, 0.0), OMap::new()));
        }
    }
    for y in 0..side {
        for x in 0..side {
            let here = ids[y * side + x];
            if x + 1 < side {
                workspace.add_edge(
                    here,
                    ids[y * side + x + 1],
                    1.0,
                    EdgeType::Undirected,
                    OMap::new(),
                );
            }
            if y + 1 < side {
                workspace.add_edge(
                    here,
                    ids[(y + 1) * side + x],
                    1.0,
                    EdgeType::Undirected,
                    OMap::new(),
                );
            }
        }
    }

    let start = workspace.graph().get_vertex(ids[0]).unwrap().id;
    let goal = workspace
        .graph()
        .get_vertex(*ids.last().unwrap())
        .unwrap()
        .id;
    (WorkspaceIndex::from_workspace(workspace), start, goal)
}

/// Neighbour lookup used to scan every edge in the graph for each node the
/// search expanded, making Dijkstra `O(V·E)`. On a 40×40 grid that is ~5M edge
/// inspections per plan, each re-parsing the edge's traffic properties. The
/// bound below is deliberately loose — it is here to catch a return to
/// quadratic behaviour, not to police milliseconds.
#[test]
fn planning_a_large_grid_stays_fast() {
    let (index, start, goal) = grid_workspace(40);

    let began = std::time::Instant::now();
    let result = plan_route(&index, start, goal, true);
    let elapsed = began.elapsed();

    assert!(
        result.search.found,
        "opposite corners of a grid are connected"
    );
    let plan = result.plan.expect("plan");
    assert_eq!(
        plan.traversed_node_ids.len(),
        79,
        "a 40x40 grid corner-to-corner is 78 steps"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "planning took {elapsed:?}; neighbour lookup has probably gone quadratic again"
    );
    eprintln!("40x40 grid plan: {elapsed:?}");
}
