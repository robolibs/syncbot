//! Zone/route claim granularity — the matrix from PLAN Milestone 1.
//!
//! A route claim takes the nodes and edges it uses, and implicitly takes
//! *intent* on every zone containing them. A zone claim takes the zone
//! outright. These tests pin how the two interact.

use std::collections::BTreeMap as OMap;
use std::sync::Arc;

use datapod::{Geo, Point, Polygon};
use graphix::vertex::EdgeType;
use syncbot::{
    ClaimAccessMode, ClaimDecision, ClaimEvaluation, ClaimId, ClaimManager, ClaimRequest,
    ClaimTarget, ClaimTargetKind, MissionId, RobotId, WorkspaceIndex,
};
use uuid::Uuid;
use zoneout::{Workspace, ZoneBuilder};

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Polygon {
    Polygon {
        vertices: vec![
            Point::new(x0, y0, 0.0),
            Point::new(x1, y0, 0.0),
            Point::new(x1, y1, 0.0),
            Point::new(x0, y1, 0.0),
        ],
    }
}

/// Zone `Z` inside root, holding `routes` disjoint 2-node routes.
struct Fx {
    index: Arc<WorkspaceIndex>,
    zone: Uuid,
    node: Vec<Uuid>,
    edge: Vec<Uuid>,
}

impl Fx {
    /// The targets a robot claims to drive route `i`: its two nodes and the
    /// edge between them.
    fn route(&self, i: usize) -> Vec<ClaimTarget> {
        vec![
            ClaimTarget {
                kind: ClaimTargetKind::Node,
                resource_id: self.node[i * 2],
            },
            ClaimTarget {
                kind: ClaimTargetKind::Node,
                resource_id: self.node[i * 2 + 1],
            },
            ClaimTarget {
                kind: ClaimTargetKind::Edge,
                resource_id: self.edge[i],
            },
        ]
    }
    fn whole_zone(&self) -> Vec<ClaimTarget> {
        vec![ClaimTarget {
            kind: ClaimTargetKind::Zone,
            resource_id: self.zone,
        }]
    }
}

fn fixture(routes: usize, zone_props: &[(&str, &str)]) -> Fx {
    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rect(0.0, 0.0, 500.0, 500.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root");
    let mut builder = ZoneBuilder::new()
        .with_name("Z")
        .with_kind("zone")
        .with_boundary(rect(5.0, 5.0, 495.0, 495.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0));
    for (k, v) in zone_props {
        builder = builder.with_property(*k, *v);
    }
    let z = builder.build().expect("zone Z");
    let zone = z.id();
    root.add_child(z).expect("add Z");

    let mut ws = Workspace::new(root);
    let mut vids = Vec::new();
    for i in 0..routes * 2 {
        vids.push(ws.add_node(Point::new(10.0 + i as f64 * 10.0, 10.0, 0.0), OMap::new()));
    }
    for i in 0..routes {
        ws.add_edge(
            vids[i * 2],
            vids[i * 2 + 1],
            1.0,
            EdgeType::Undirected,
            OMap::new(),
        );
    }

    let node: Vec<Uuid> = vids
        .iter()
        .map(|v| ws.graph().get_vertex(*v).unwrap().id)
        .collect();
    for nid in &node {
        let vid = ws.find_node(*nid).unwrap();
        ws.graph_mut()
            .get_vertex_mut(vid)
            .unwrap()
            .zone_ids
            .push(zone);
    }
    let eids: Vec<_> = ws.graph().edges().iter().map(|e| e.id).collect();
    let mut edge = Vec::new();
    for eid in eids {
        let prop = ws.graph_mut().edge_property_mut(eid).unwrap();
        prop.zone_ids.push(zone);
        edge.push(prop.id);
    }

    Fx {
        index: Arc::new(WorkspaceIndex::new(Arc::new(ws))),
        zone,
        node,
        edge,
    }
}

fn request(claim: u64, robot: u64, targets: Vec<ClaimTarget>) -> ClaimRequest {
    ClaimRequest {
        id: ClaimId::new(claim),
        robot_id: RobotId::new(robot),
        mission_id: MissionId::default(),
        access_mode: ClaimAccessMode::Exclusive,
        priority: 0,
        requested_at_tick: None,
        window: Default::default(),
        targets,
    }
}

/// Grant `targets` to `robot`, asserting it succeeds.
fn hold(manager: &mut ClaimManager, claim: u64, robot: u64, targets: Vec<ClaimTarget>) {
    let r = request(claim, robot, targets);
    assert_eq!(
        manager.evaluate_request(&r).decision,
        ClaimDecision::Grant,
        "setup claim {claim} for robot {robot} should be granted"
    );
    manager.upsert_request_for_robot(r);
}

fn ask(
    manager: &ClaimManager,
    claim: u64,
    robot: u64,
    targets: Vec<ClaimTarget>,
) -> ClaimEvaluation {
    manager.evaluate_request(&request(claim, robot, targets))
}

// -- the matrix ------------------------------------------------------------

#[test]
fn a_route_blocks_a_whole_zone_claim() {
    let fx = fixture(2, &[]);
    let mut m = ClaimManager::with_index(Arc::clone(&fx.index));
    hold(&mut m, 1, 7, fx.route(0));

    assert_eq!(
        ask(&m, 2, 8, fx.whole_zone()).decision,
        ClaimDecision::Deny,
        "someone is driving through Z, so Z cannot be taken outright"
    );
}

#[test]
fn a_whole_zone_claim_blocks_a_route() {
    let fx = fixture(2, &[]);
    let mut m = ClaimManager::with_index(Arc::clone(&fx.index));
    hold(&mut m, 1, 7, fx.whole_zone());

    assert_eq!(
        ask(&m, 2, 8, fx.route(0)).decision,
        ClaimDecision::Deny,
        "robot 7 owns Z outright, so nothing inside it is available"
    );
}

/// The case that motivates the whole model: passing through a zone must not
/// reserve the zone.
#[test]
fn disjoint_routes_through_an_unconstrained_zone_coexist() {
    let fx = fixture(2, &[]);
    let mut m = ClaimManager::with_index(Arc::clone(&fx.index));
    hold(&mut m, 1, 7, fx.route(0));

    assert_eq!(
        ask(&m, 2, 8, fx.route(1)).decision,
        ClaimDecision::Grant,
        "two robots on non-interfering paths through Z must both proceed"
    );
}

#[test]
fn overlapping_routes_conflict() {
    let fx = fixture(2, &[]);
    let mut m = ClaimManager::with_index(Arc::clone(&fx.index));
    hold(&mut m, 1, 7, fx.route(0));

    let mut overlapping = fx.route(1);
    overlapping.push(ClaimTarget {
        kind: ClaimTargetKind::Node,
        resource_id: fx.node[0],
    });
    assert_eq!(
        ask(&m, 2, 8, overlapping).decision,
        ClaimDecision::Deny,
        "the routes share a node"
    );
}

#[test]
fn an_exclusive_zone_admits_one_route_at_a_time() {
    let fx = fixture(2, &[("traffic.policy", "exclusive")]);
    let mut m = ClaimManager::with_index(Arc::clone(&fx.index));
    hold(&mut m, 1, 7, fx.route(0));

    assert_eq!(
        ask(&m, 2, 8, fx.route(1)).decision,
        ClaimDecision::Deny,
        "an exclusive zone admits one occupant, disjoint paths or not"
    );
}

/// The capacity dimension: a zone that admits N occupants admits N routes,
/// and refuses the N+1th.
#[test]
fn a_capacity_zone_admits_exactly_capacity_routes() {
    let fx = fixture(
        3,
        &[("traffic.policy", "shared"), ("traffic.capacity", "2")],
    );
    let mut m = ClaimManager::with_index(Arc::clone(&fx.index));

    hold(&mut m, 1, 7, fx.route(0));
    hold(&mut m, 2, 8, fx.route(1));

    let third = ask(&m, 3, 9, fx.route(2));
    assert_eq!(
        third.decision,
        ClaimDecision::Deny,
        "Z admits 2 occupants; a third route must be refused (got: {})",
        third.reason
    );
}

/// Occupancy counts robots, not claims: one robot holding several routes in a
/// capacity-2 zone still fills one slot, leaving room for exactly one other.
#[test]
fn zone_occupancy_counts_robots_not_claims() {
    let fx = fixture(
        4,
        &[("traffic.policy", "shared"), ("traffic.capacity", "2")],
    );
    let mut m = ClaimManager::with_index(Arc::clone(&fx.index));

    hold(&mut m, 1, 7, fx.route(0));
    hold(&mut m, 2, 7, fx.route(1));

    assert_eq!(
        ask(&m, 3, 8, fx.route(2)).decision,
        ClaimDecision::Grant,
        "robot 7's two routes are one occupant, so a slot is still free"
    );
    hold(&mut m, 3, 8, fx.route(2));
    assert_eq!(
        ask(&m, 4, 9, fx.route(3)).decision,
        ClaimDecision::Deny,
        "robots 7 and 8 fill both slots"
    );
}

/// Intent reaches the ancestors of the zone actually entered: a robot on a
/// path inside a child zone occupies the constrained parent too.
#[test]
fn intent_propagates_to_ancestor_zones() {
    // root > outer(capacity 1) > inner, with the routes in `inner`.
    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rect(0.0, 0.0, 500.0, 500.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root");
    let inner = ZoneBuilder::new()
        .with_name("inner")
        .with_kind("zone")
        .with_boundary(rect(20.0, 20.0, 400.0, 400.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("inner");
    let inner_id = inner.id();
    let mut outer = ZoneBuilder::new()
        .with_name("outer")
        .with_kind("zone")
        .with_boundary(rect(10.0, 10.0, 450.0, 450.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .with_property("traffic.policy", "exclusive")
        .build()
        .expect("outer");
    outer.add_child(inner).expect("nest inner");
    root.add_child(outer).expect("nest outer");

    let mut ws = Workspace::new(root);
    let vids: Vec<_> = (0..4)
        .map(|i| ws.add_node(Point::new(30.0 + i as f64 * 10.0, 30.0, 0.0), OMap::new()))
        .collect();
    ws.add_edge(vids[0], vids[1], 1.0, EdgeType::Undirected, OMap::new());
    ws.add_edge(vids[2], vids[3], 1.0, EdgeType::Undirected, OMap::new());
    let node: Vec<Uuid> = vids
        .iter()
        .map(|v| ws.graph().get_vertex(*v).unwrap().id)
        .collect();
    for nid in &node {
        let vid = ws.find_node(*nid).unwrap();
        ws.graph_mut()
            .get_vertex_mut(vid)
            .unwrap()
            .zone_ids
            .push(inner_id);
    }
    let eids: Vec<_> = ws.graph().edges().iter().map(|e| e.id).collect();
    let mut edge = Vec::new();
    for eid in eids {
        let prop = ws.graph_mut().edge_property_mut(eid).unwrap();
        prop.zone_ids.push(inner_id);
        edge.push(prop.id);
    }
    let fx = Fx {
        index: Arc::new(WorkspaceIndex::new(Arc::new(ws))),
        zone: inner_id,
        node,
        edge,
    };

    let mut m = ClaimManager::with_index(Arc::clone(&fx.index));
    hold(&mut m, 1, 7, fx.route(0));

    assert_eq!(
        ask(&m, 2, 8, fx.route(1)).decision,
        ClaimDecision::Deny,
        "the exclusive parent admits one occupant, even though the paths are \
         disjoint and the child zone itself is unconstrained"
    );
}

// ---------------------------------------------------------------------------
// The route -> claim translation (PLAN Milestone 1.3).
// ---------------------------------------------------------------------------

/// A route claim names the nodes and edges driven, never the zones passed
/// through. Claiming the zones would reserve the whole area.
#[test]
fn route_claim_targets_name_no_zones() {
    let fx = fixture(2, &[]);
    let plan = syncbot::plan_route(&fx.index, fx.node[0], fx.node[1], false)
        .plan
        .expect("a → b is one edge");

    assert!(
        !plan.traversed_zone_ids.is_empty(),
        "the route does pass through Z — the plan still reports that"
    );

    let targets = syncbot::claim_targets_from_route(&plan);
    assert!(
        targets.iter().all(|t| t.kind != ClaimTargetKind::Zone),
        "passing through a zone is intent, not ownership: {targets:?}"
    );
    assert_eq!(targets.len(), 3, "two nodes and the edge between them");
}

/// End to end through the real helper: two robots planning disjoint paths
/// across one open zone must both proceed.
#[test]
fn two_planned_routes_across_one_open_zone_both_proceed() {
    let fx = fixture(2, &[]);
    let mut m = ClaimManager::with_index(Arc::clone(&fx.index));

    let first = syncbot::plan_route(&fx.index, fx.node[0], fx.node[1], false)
        .plan
        .expect("route 0");
    let second = syncbot::plan_route(&fx.index, fx.node[2], fx.node[3], false)
        .plan
        .expect("route 1");

    let a = syncbot::claim_request_from_route(
        ClaimId::new(1),
        RobotId::new(7),
        MissionId::default(),
        &first,
        None,
        1.0,
        ClaimAccessMode::Exclusive,
    );
    assert_eq!(m.evaluate_request(&a).decision, ClaimDecision::Grant);
    m.upsert_request_for_robot(a);

    let b = syncbot::claim_request_from_route(
        ClaimId::new(2),
        RobotId::new(8),
        MissionId::default(),
        &second,
        None,
        1.0,
        ClaimAccessMode::Exclusive,
    );
    assert_eq!(
        m.evaluate_request(&b).decision,
        ClaimDecision::Grant,
        "disjoint planned routes through an unconstrained zone must coexist"
    );
}

/// ...but the zone's own policy still governs. An exclusive zone admits one.
#[test]
fn two_planned_routes_across_an_exclusive_zone_do_not() {
    let fx = fixture(2, &[("traffic.policy", "exclusive")]);
    let mut m = ClaimManager::with_index(Arc::clone(&fx.index));

    let first = syncbot::plan_route(&fx.index, fx.node[0], fx.node[1], false)
        .plan
        .expect("route 0");
    let second = syncbot::plan_route(&fx.index, fx.node[2], fx.node[3], false)
        .plan
        .expect("route 1");

    let a = syncbot::claim_request_from_route(
        ClaimId::new(1),
        RobotId::new(7),
        MissionId::default(),
        &first,
        None,
        1.0,
        ClaimAccessMode::Exclusive,
    );
    assert_eq!(m.evaluate_request(&a).decision, ClaimDecision::Grant);
    m.upsert_request_for_robot(a);

    let b = syncbot::claim_request_from_route(
        ClaimId::new(2),
        RobotId::new(8),
        MissionId::default(),
        &second,
        None,
        1.0,
        ClaimAccessMode::Exclusive,
    );
    assert_eq!(
        m.evaluate_request(&b).decision,
        ClaimDecision::Deny,
        "the zone is exclusive, so one occupant at a time"
    );
}
