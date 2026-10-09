//! Property tests on the claim algebra (PLAN 2.3.1) and on routing (2.3.2).
//!
//! The claim matrix is combinatorial — target kind × access mode × capacity ×
//! nesting depth × overlapping windows — and hand-written cases only reach the
//! corners someone thought of. These generate random workspaces and random
//! claim sequences, then assert what must hold no matter what came before.
//!
//! The invariants are the reason the system exists: if `no_two_robots_hold_the
//! _same_resource_exclusively` can be violated, two robots collide.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use datapod::{Geo, Point, Polygon};
use graphix::vertex::EdgeType;
use proptest::prelude::*;
use syncbot::{
    ClaimAccessMode, ClaimDecision, ClaimId, ClaimManager, ClaimRequest, ClaimTarget,
    ClaimTargetKind, MissionId, RobotId, WorkspaceIndex,
};
use uuid::Uuid;
use zoneout::{Workspace, ZoneBuilder};

// ---------------------------------------------------------------------------
// generated workspace
// ---------------------------------------------------------------------------

/// What a generated zone is configured to allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ZoneKind {
    /// No traffic policy: unlimited occupants, arbitration at node/edge level.
    Open,
    /// `traffic.policy=exclusive`: one occupant.
    Exclusive,
    /// `traffic.capacity=n`: n occupants.
    Capacity(u64),
}

impl ZoneKind {
    fn properties(self) -> Vec<(String, String)> {
        match self {
            ZoneKind::Open => Vec::new(),
            ZoneKind::Exclusive => vec![("traffic.policy".into(), "exclusive".into())],
            ZoneKind::Capacity(n) => vec![
                ("traffic.policy".into(), "shared".into()),
                ("traffic.capacity".into(), n.to_string()),
            ],
        }
    }

    /// How many distinct robots the zone admits, or `None` for unlimited.
    fn occupancy_limit(self) -> Option<u64> {
        match self {
            ZoneKind::Open => None,
            ZoneKind::Exclusive => Some(1),
            ZoneKind::Capacity(n) => Some(n),
        }
    }
}

fn zone_kind() -> impl Strategy<Value = ZoneKind> {
    prop_oneof![
        2 => Just(ZoneKind::Open),
        2 => Just(ZoneKind::Exclusive),
        1 => (2u64..4).prop_map(ZoneKind::Capacity),
    ]
}

/// A generated workspace plus the lookups the invariants need.
struct World {
    index: Arc<WorkspaceIndex>,
    /// Leaf zones, in generation order.
    zones: Vec<Uuid>,
    /// Configured limit per zone, including inherited parent limits.
    limits: BTreeMap<Uuid, Option<u64>>,
    nodes: Vec<Uuid>,
    edges: Vec<Uuid>,
    /// Which zones contain each node/edge, ancestors included.
    containing: BTreeMap<Uuid, BTreeSet<Uuid>>,
}

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

/// Build a workspace of `kinds.len()` sibling zones under one root, each
/// holding `nodes_per_zone` nodes chained by edges.
fn build_world(kinds: &[ZoneKind], nodes_per_zone: usize) -> World {
    let datum = Geo::new(52.0, 5.0, 0.0);
    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rect(0.0, 0.0, 10_000.0, 1_000.0))
        .with_datum(datum)
        .build()
        .expect("root");

    let mut zones = Vec::new();
    let mut limits = BTreeMap::new();
    for (i, kind) in kinds.iter().enumerate() {
        let x0 = 10.0 + i as f64 * 200.0;
        let mut builder = ZoneBuilder::new()
            .with_name(format!("z{i}"))
            .with_kind("zone")
            .with_boundary(rect(x0, 10.0, x0 + 150.0, 500.0))
            .with_datum(datum);
        for (k, v) in kind.properties() {
            builder = builder.with_property(k, v);
        }
        let zone = builder.build().expect("zone");
        zones.push(zone.id());
        limits.insert(zone.id(), kind.occupancy_limit());
        root.add_child(zone).expect("add zone");
    }

    let mut ws = Workspace::new(root);
    let mut vids = Vec::new();
    let mut node_zone = Vec::new();
    for (i, _) in kinds.iter().enumerate() {
        let x0 = 10.0 + i as f64 * 200.0;
        for n in 0..nodes_per_zone {
            vids.push(ws.add_node(
                Point::new(x0 + 20.0 + n as f64 * 20.0, 100.0, 0.0),
                Default::default(),
            ));
            node_zone.push(zones[i]);
        }
    }
    // Chain nodes within each zone so every edge lies in exactly one zone.
    for zone_index in 0..kinds.len() {
        for n in 0..nodes_per_zone.saturating_sub(1) {
            let a = vids[zone_index * nodes_per_zone + n];
            let b = vids[zone_index * nodes_per_zone + n + 1];
            ws.add_edge(a, b, 1.0, EdgeType::Undirected, Default::default());
        }
    }

    let nodes: Vec<Uuid> = vids
        .iter()
        .map(|v| ws.graph().get_vertex(*v).unwrap().id)
        .collect();
    for (node_id, zone_id) in nodes.iter().zip(node_zone.iter()) {
        let vid = ws.find_node(*node_id).unwrap();
        ws.graph_mut()
            .get_vertex_mut(vid)
            .unwrap()
            .zone_ids
            .push(*zone_id);
    }
    let eids: Vec<_> = ws.graph().edges().iter().map(|e| e.id).collect();
    let mut edges = Vec::new();
    let mut edge_zone = Vec::new();
    for (i, eid) in eids.iter().enumerate() {
        // Edges were added zone by zone, so index maps back to its zone.
        let per = nodes_per_zone.saturating_sub(1).max(1);
        let zone_id = zones[(i / per).min(zones.len() - 1)];
        let prop = ws.graph_mut().edge_property_mut(*eid).unwrap();
        prop.zone_ids.push(zone_id);
        edges.push(prop.id);
        edge_zone.push(zone_id);
    }

    let index = Arc::new(WorkspaceIndex::new(Arc::new(ws)));
    let mut containing: BTreeMap<Uuid, BTreeSet<Uuid>> = BTreeMap::new();
    for node_id in &nodes {
        let mut set = BTreeSet::new();
        for zone in index.zones_of_node(*node_id) {
            set.insert(zone.id());
            for ancestor in index.ancestor_zones(zone.id()) {
                set.insert(ancestor.id());
            }
        }
        containing.insert(*node_id, set);
    }
    for edge_id in &edges {
        let mut set = BTreeSet::new();
        for zone in index.zones_of_edge(*edge_id) {
            set.insert(zone.id());
            for ancestor in index.ancestor_zones(zone.id()) {
                set.insert(ancestor.id());
            }
        }
        containing.insert(*edge_id, set);
    }

    World {
        index,
        zones,
        limits,
        nodes,
        edges,
        containing,
    }
}

// ---------------------------------------------------------------------------
// generated operations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Op {
    /// Claim: robot, whether shared, and indices into zones/nodes/edges.
    Claim {
        robot: u64,
        shared: bool,
        zones: Vec<usize>,
        nodes: Vec<usize>,
        edges: Vec<usize>,
    },
    /// Drop everything a robot holds.
    ReleaseAll { robot: u64 },
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        8 => (
            0u64..4,
            any::<bool>(),
            prop::collection::vec(0usize..8, 0..2),
            prop::collection::vec(0usize..16, 0..3),
            prop::collection::vec(0usize..12, 0..2),
        )
            .prop_map(|(robot, shared, zones, nodes, edges)| Op::Claim {
                robot,
                shared,
                zones,
                nodes,
                edges,
            }),
        2 => (0u64..4).prop_map(|robot| Op::ReleaseAll { robot }),
    ]
}

/// Apply `op` to `manager`, returning the request when it was granted.
fn apply(world: &World, manager: &mut ClaimManager, next_id: &mut u64, op: &Op) {
    match op {
        Op::ReleaseAll { robot } => {
            manager.remove_requests_for_robot(RobotId::new(*robot));
        }
        Op::Claim {
            robot,
            shared,
            zones,
            nodes,
            edges,
        } => {
            let mut targets = Vec::new();
            let mut seen = BTreeSet::new();
            for &i in zones {
                if let Some(&id) = world.zones.get(i % world.zones.len().max(1))
                    && seen.insert(id)
                {
                    targets.push(ClaimTarget {
                        kind: ClaimTargetKind::Zone,
                        resource_id: id,
                    });
                }
            }
            for &i in nodes {
                if let Some(&id) = world.nodes.get(i % world.nodes.len().max(1))
                    && seen.insert(id)
                {
                    targets.push(ClaimTarget {
                        kind: ClaimTargetKind::Node,
                        resource_id: id,
                    });
                }
            }
            for &i in edges {
                if !world.edges.is_empty() {
                    let id = world.edges[i % world.edges.len()];
                    if seen.insert(id) {
                        targets.push(ClaimTarget {
                            kind: ClaimTargetKind::Edge,
                            resource_id: id,
                        });
                    }
                }
            }
            if targets.is_empty() {
                return;
            }
            *next_id += 1;
            let request = ClaimRequest {
                id: ClaimId::new(*next_id),
                robot_id: RobotId::new(*robot),
                mission_id: MissionId::default(),
                access_mode: if *shared {
                    ClaimAccessMode::Shared
                } else {
                    ClaimAccessMode::Exclusive
                },
                priority: 0,
                requested_at_tick: None,
                window: Default::default(),
                targets,
            };
            if manager.evaluate_request(&request).decision == ClaimDecision::Grant {
                manager.upsert_request_for_robot(request);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// failure artifact
// ---------------------------------------------------------------------------

/// Draw a counterexample as a rerun recording (PLAN 2.4.3).
///
/// A proptest counterexample in the claim matrix is a wall of shrink output.
/// As a picture it is two robots holding the same ground, with the zones drawn
/// and each holder in its own colour. Enabled by the same `rerun-viz` feature
/// the fleet visualizer uses; without it this is a no-op and the message alone
/// has to do.
#[cfg(feature = "rerun-viz")]
fn dump_failure(world: &World, manager: &ClaimManager, step: usize, why: &str) -> Option<String> {
    use rerun::{Color, LineStrips3D, RecordingStreamBuilder, TextLog};

    let path = format!("target/claim-counterexample-step{step}.rrd");
    let rec = RecordingStreamBuilder::new("syncbot-claim-counterexample")
        .save(&path)
        .ok()?;

    // Who holds each zone, by the same accounting the invariants use.
    let mut holders: BTreeMap<Uuid, BTreeSet<u64>> = BTreeMap::new();
    for request in manager.requests() {
        for target in &request.targets {
            let zones: Vec<Uuid> = match target.kind {
                ClaimTargetKind::Zone => vec![target.resource_id],
                _ => world
                    .containing
                    .get(&target.resource_id)
                    .map(|z| z.iter().copied().collect())
                    .unwrap_or_default(),
            };
            for zone in zones {
                holders
                    .entry(zone)
                    .or_default()
                    .insert(request.robot_id.raw());
            }
        }
    }

    for (i, zone_id) in world.zones.iter().enumerate() {
        let Some(zone) = world.index.zone(*zone_id) else {
            continue;
        };
        if !zone.poly().has_field_boundary() {
            continue;
        }
        let mut strip: Vec<[f32; 3]> = zone
            .poly()
            .field_boundary()
            .vertices
            .iter()
            .map(|v| [v.x as f32, v.y as f32, 0.0])
            .collect();
        if let Some(&first) = strip.first() {
            strip.push(first);
        }
        let who = holders.get(zone_id).cloned().unwrap_or_default();
        let color = match who.iter().next() {
            Some(robot) => {
                let shade = (*robot as u8).wrapping_mul(70);
                Color::from_rgb(255 - shade, 90 + shade / 2, 110)
            }
            None => Color::from_rgb(90, 100, 115),
        };
        let label = format!(
            "z{i} limit={:?} held by {:?}",
            world.limits.get(zone_id).copied().flatten(),
            who
        );
        let _ = rec.log_static(
            format!("/zones/z{i}"),
            &LineStrips3D::new([strip])
                .with_colors([color])
                .with_radii([if who.len() > 1 { 3.0f32 } else { 1.0 }])
                .with_labels([label]),
        );
    }

    let _ = rec.log_static("/why", &TextLog::new(format!("step {step}: {why}")));
    for request in manager.requests() {
        let _ = rec.log_static(
            format!("/ledger/robot{}", request.robot_id.raw()),
            &TextLog::new(format!("{:?} {:?}", request.access_mode, request.targets)),
        );
    }
    Some(path)
}

#[cfg(not(feature = "rerun-viz"))]
fn dump_failure(_: &World, _: &ClaimManager, _: usize, _: &str) -> Option<String> {
    None
}

/// Panic with the counterexample, pointing at the recording when there is one.
fn fail(world: &World, manager: &ClaimManager, step: usize, why: &str) -> ! {
    match dump_failure(world, manager, step, why) {
        Some(path) => panic!("step {step}: {why}\n  recording: {path} (open with `rerun {path}`)"),
        None => {
            panic!("step {step}: {why}\n  (re-run with --features rerun-viz for a picture of it)")
        }
    }
}

// ---------------------------------------------------------------------------
// invariants over the resulting ledger
// ---------------------------------------------------------------------------

/// SAFETY. Two robots must never both hold the same resource when either holds
/// it exclusively. A violation here is two robots in one place.
fn check_exclusive_safety(manager: &ClaimManager) -> Result<(), String> {
    let mut holder: BTreeMap<(u8, Uuid), (RobotId, ClaimAccessMode)> = BTreeMap::new();
    for request in manager.requests() {
        for target in &request.targets {
            let key = (target.kind as u8, target.resource_id);
            if let Some((other_robot, other_mode)) = holder.get(&key)
                && *other_robot != request.robot_id
                && (*other_mode == ClaimAccessMode::Exclusive
                    || request.access_mode == ClaimAccessMode::Exclusive)
            {
                return Err(format!(
                    "robots {} and {} both hold {:?} {} with an exclusive claim",
                    other_robot, request.robot_id, target.kind, target.resource_id
                ));
            }
            holder.insert(key, (request.robot_id, request.access_mode));
        }
    }
    Ok(())
}

/// CAPACITY. A zone never holds more distinct robots than it admits, counting
/// robots present via a node/edge inside it as well as by a zone claim.
fn check_capacity(world: &World, manager: &ClaimManager) -> Result<(), String> {
    for (&zone_id, &limit) in &world.limits {
        let Some(limit) = limit else { continue };
        let mut occupants: BTreeSet<RobotId> = BTreeSet::new();
        for request in manager.requests() {
            let inside = request.targets.iter().any(|t| match t.kind {
                ClaimTargetKind::Zone => t.resource_id == zone_id,
                _ => world
                    .containing
                    .get(&t.resource_id)
                    .is_some_and(|zones| zones.contains(&zone_id)),
            });
            if inside {
                occupants.insert(request.robot_id);
            }
        }
        if occupants.len() as u64 > limit {
            return Err(format!(
                "zone {zone_id} admits {limit} robots but holds {} ({:?})",
                occupants.len(),
                occupants
            ));
        }
    }
    Ok(())
}

/// CROSS-LEVEL. A zone held outright excludes everything inside it, at any
/// depth, from every other robot.
fn check_cross_level(world: &World, manager: &ClaimManager) -> Result<(), String> {
    for request in manager.requests() {
        for target in request
            .targets
            .iter()
            .filter(|t| t.kind == ClaimTargetKind::Zone)
        {
            if request.access_mode != ClaimAccessMode::Exclusive {
                continue;
            }
            for other in manager.requests() {
                if other.robot_id == request.robot_id {
                    continue;
                }
                for other_target in &other.targets {
                    let inside = match other_target.kind {
                        ClaimTargetKind::Zone => other_target.resource_id == target.resource_id,
                        _ => world
                            .containing
                            .get(&other_target.resource_id)
                            .is_some_and(|zones| zones.contains(&target.resource_id)),
                    };
                    if inside {
                        return Err(format!(
                            "robot {} holds zone {} exclusively while robot {} holds {:?} {} inside it",
                            request.robot_id,
                            target.resource_id,
                            other.robot_id,
                            other_target.kind,
                            other_target.resource_id
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn world_strategy() -> impl Strategy<Value = Vec<ZoneKind>> {
    prop::collection::vec(zone_kind(), 1..4)
}

// A modest default so the suite stays quick in CI; building a workspace per
// case dominates the runtime. Raise it when hunting:
//   PROPTEST_CASES=2000 cargo test --release --test claim_properties
proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// Whatever sequence of claims and releases has run, the ledger always
    /// satisfies the three invariants the coordination model promises.
    #[test]
    fn ledger_invariants_hold_after_any_sequence(
        kinds in world_strategy(),
        ops in prop::collection::vec(op_strategy(), 1..14),
    ) {
        let world = build_world(&kinds, 3);
        let mut manager = ClaimManager::with_index(Arc::clone(&world.index));
        let mut next_id = 0u64;

        for (step, op) in ops.iter().enumerate() {
            apply(&world, &mut manager, &mut next_id, op);
            if let Err(why) = check_exclusive_safety(&manager) {
                fail(&world, &manager, step, &format!("exclusive safety violated: {why}"));
            }
            if let Err(why) = check_capacity(&world, &manager) {
                fail(&world, &manager, step, &format!("capacity violated: {why}"));
            }
            if let Err(why) = check_cross_level(&world, &manager) {
                fail(
                    &world,
                    &manager,
                    step,
                    &format!("cross-level exclusion violated: {why}"),
                );
            }
        }
    }

    /// SELF. A robot re-asserting exactly what it already holds is always
    /// granted, and never consumes a second slot. (PLAN Milestone 1.1.)
    #[test]
    fn a_robot_can_always_reclaim_what_it_holds(
        kinds in world_strategy(),
        ops in prop::collection::vec(op_strategy(), 1..10),
    ) {
        let world = build_world(&kinds, 3);
        let mut manager = ClaimManager::with_index(Arc::clone(&world.index));
        let mut next_id = 0u64;
        for op in &ops {
            apply(&world, &mut manager, &mut next_id, op);
        }

        let held: Vec<ClaimRequest> = manager.requests().to_vec();
        for request in held {
            next_id += 1;
            let again = ClaimRequest {
                id: ClaimId::new(next_id),
                ..request.clone()
            };
            let decision = manager.evaluate_request(&again).decision;
            prop_assert_eq!(
                decision,
                ClaimDecision::Grant,
                "robot {} was refused ground it already holds",
                request.robot_id
            );
        }
    }

    /// RELEASE IS AN INVERSE. Whatever a robot was refused, it is granted once
    /// every holder of the blocking ground has let go.
    #[test]
    fn releasing_everything_makes_any_claim_grantable(
        kinds in world_strategy(),
        ops in prop::collection::vec(op_strategy(), 1..10),
        candidate in op_strategy(),
    ) {
        let world = build_world(&kinds, 3);
        let mut manager = ClaimManager::with_index(Arc::clone(&world.index));
        let mut next_id = 0u64;
        for op in &ops {
            apply(&world, &mut manager, &mut next_id, op);
        }

        // Build the candidate claim without applying it.
        let mut probe = ClaimManager::with_index(Arc::clone(&world.index));
        let mut probe_id = 10_000u64;
        apply(&world, &mut probe, &mut probe_id, &candidate);
        let Some(wanted) = probe.requests().first().cloned() else {
            return Ok(());
        };

        // Everyone lets go.
        for robot in 0..4u64 {
            manager.remove_requests_for_robot(RobotId::new(robot));
        }
        prop_assert_eq!(manager.request_count(), 0);

        let decision = manager.evaluate_request(&wanted).decision;
        prop_assert_eq!(
            decision,
            ClaimDecision::Grant,
            "an empty workspace refused a claim it had granted in isolation"
        );
    }
}

// ---------------------------------------------------------------------------
// Metamorphic properties on routing (PLAN 2.3.2).
//
// These need no known-correct answers: they relate one planning result to
// another on a modified graph, which is exactly the kind of bug a hand-written
// example never reaches.
// ---------------------------------------------------------------------------

/// A `side x side` grid with per-edge weights, 4-connected.
fn grid(side: usize, weight: f64) -> (WorkspaceIndex, Vec<Uuid>) {
    let root = ZoneBuilder::new()
        .with_name("grid")
        .with_kind("workspace")
        .with_boundary(rect(-1.0, -1.0, side as f64 + 1.0, side as f64 + 1.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root");
    let mut ws = Workspace::new(root);
    let mut vids = Vec::new();
    for y in 0..side {
        for x in 0..side {
            vids.push(ws.add_node(Point::new(x as f64, y as f64, 0.0), Default::default()));
        }
    }
    for y in 0..side {
        for x in 0..side {
            if x + 1 < side {
                ws.add_edge(
                    vids[y * side + x],
                    vids[y * side + x + 1],
                    weight,
                    EdgeType::Undirected,
                    Default::default(),
                );
            }
            if y + 1 < side {
                ws.add_edge(
                    vids[y * side + x],
                    vids[(y + 1) * side + x],
                    weight,
                    EdgeType::Undirected,
                    Default::default(),
                );
            }
        }
    }
    let ids: Vec<Uuid> = vids
        .iter()
        .map(|v| ws.graph().get_vertex(*v).unwrap().id)
        .collect();
    (WorkspaceIndex::from_workspace(ws), ids)
}

fn cost(index: &WorkspaceIndex, a: Uuid, b: Uuid) -> Option<f64> {
    let result = syncbot::plan_route(index, a, b, false);
    result.search.found.then_some(result.search.distance)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// An undirected graph costs the same in both directions.
    #[test]
    fn routing_is_symmetric_on_an_undirected_graph(
        from in 0usize..25,
        to in 0usize..25,
    ) {
        let (index, nodes) = grid(5, 1.0);
        let (a, b) = (nodes[from], nodes[to]);
        let forward = cost(&index, a, b);
        let backward = cost(&index, b, a);
        prop_assert_eq!(
            forward.is_some(),
            backward.is_some(),
            "reachability must not depend on direction"
        );
        if let (Some(f), Some(r)) = (forward, backward) {
            prop_assert!(
                (f - r).abs() < 1e-9,
                "a->b cost {f} but b->a cost {r}"
            );
        }
    }

    /// Going via an intermediate node is never cheaper than going direct.
    #[test]
    fn routing_obeys_the_triangle_inequality(
        a in 0usize..25,
        b in 0usize..25,
        c in 0usize..25,
    ) {
        let (index, nodes) = grid(5, 1.0);
        let (Some(direct), Some(first), Some(second)) = (
            cost(&index, nodes[a], nodes[c]),
            cost(&index, nodes[a], nodes[b]),
            cost(&index, nodes[b], nodes[c]),
        ) else {
            return Ok(());
        };
        prop_assert!(
            direct <= first + second + 1e-9,
            "a->c is {direct} but a->b->c is {} + {} = {}",
            first,
            second,
            first + second
        );
    }

    /// Raising every edge weight scales the optimal cost, never lowers it.
    #[test]
    fn heavier_edges_never_make_a_route_cheaper(
        from in 0usize..25,
        to in 0usize..25,
        factor in 1.0f64..8.0,
    ) {
        let (cheap, nodes) = grid(5, 1.0);
        let (dear, _) = grid(5, factor);
        let (Some(before), Some(after)) = (
            cost(&cheap, nodes[from], nodes[to]),
            cost(&dear, nodes[from], nodes[to]),
        ) else {
            return Ok(());
        };
        prop_assert!(
            after >= before - 1e-9,
            "weights rose by {factor}x but cost fell from {before} to {after}"
        );
    }

    /// The policy cost model only ever adds. A penalised plan is never cheaper
    /// than the same plan measured on raw graph weight.
    #[test]
    fn penalties_never_reduce_cost(from in 0usize..25, to in 0usize..25) {
        let (index, nodes) = grid(5, 1.0);
        let plain = syncbot::plan_route(&index, nodes[from], nodes[to], false);
        let penalised = syncbot::plan_route(&index, nodes[from], nodes[to], true);
        prop_assert_eq!(
            plain.search.found,
            penalised.search.found,
            "penalties must not change what is reachable on an unrestricted graph"
        );
        if plain.search.found {
            prop_assert!(
                penalised.search.distance >= plain.search.distance - 1e-9,
                "penalised {} is cheaper than plain {}",
                penalised.search.distance,
                plain.search.distance
            );
        }
    }

    /// Every plan handed out is structurally sound: it starts and ends where
    /// asked, and every consecutive pair of nodes has a real edge.
    #[test]
    fn every_returned_plan_is_well_formed(from in 0usize..25, to in 0usize..25) {
        let (index, nodes) = grid(5, 1.0);
        let (a, b) = (nodes[from], nodes[to]);
        let Some(plan) = syncbot::plan_route(&index, a, b, true).plan else {
            return Ok(());
        };
        prop_assert!(syncbot::validate_route_plan_shape(&plan).is_ok());
        prop_assert_eq!(plan.start_node_id, a);
        prop_assert_eq!(plan.goal_node_id, b);
        for pair in plan.traversed_node_ids.windows(2) {
            prop_assert!(
                index.edge_between(pair[0], pair[1]).is_some(),
                "plan steps from {} to {} with no edge between them",
                pair[0],
                pair[1]
            );
        }
    }
}
