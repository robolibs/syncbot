//! Four yard shuttles, one weighbridge, stepped in real time into Rerun.
//!
//! A produce packhouse in the Noordoostpolder at harvest. Autonomous shuttles
//! haul full bins from the two field-side buffers to the packhouse intakes and
//! come back empty. The yard's two halves are joined by a single-lane
//! weighbridge — every loaded shuttle must cross it, it fits one machine, and
//! traffic runs both ways over it. That one span is the whole reason this
//! yard needs a coordinator: without it the shuttles deadlock nose to nose on
//! the deck.
//!
//! The interesting claim is not the weighbridge *zone*. A shuttle claims the
//! nodes it stops at and the edges it crosses; the manager derives what that
//! implies for the zones those sit in. So two shuttles heading for different
//! intakes share the apron happily, while the bridge deck — one edge inside an
//! exclusive zone — admits exactly one.
//!
//! From the library: the workspace index, policy-aware route planning, the
//! rolling-horizon claim request, claim evaluation, schedule decisions
//! (*Proceed | Queue | Replan*) and right-of-way arbitration. The shuttle
//! state machine is application logic and lives here.
//!
//! Start a viewer, then run:
//!
//!   rerun
//!   cargo run --example packhouse_yard --features rerun-viz
//!
//! `SPEEDUP=20` to slow it down, `SYNCBOT_VIZ_RRD=x.rrd` to record headless.

#[path = "support/rerun_viz.rs"]
mod rerun_viz;

use std::collections::BTreeMap as OMap;
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use datapod::{Geo, Point, Polygon};
use graphix::vertex::EdgeType;
use rerun::{RecordingStream, Scalars, TextLog};
use syncbot::{
    ArbitrationContext, ArbitrationDecision, ClaimAccessMode, ClaimDecision, ClaimId,
    ClaimTargetKind, Coordinator, NUMERIC_ID_PROPERTY, RobotId, RobotState, RoutePlan,
    ScheduleDecisionKind, WorkspaceIndex, arbitrate_right_of_way, plan_route,
};
use uuid::Uuid;
use zoneout::{CoordMode, EdgeData, NodeData, Workspace, ZoneBuilder};

/// Packhouse yard, Noordoostpolder. Local ENU is metres off this point.
const DATUM_LAT: f64 = 52.661_9;
const DATUM_LON: f64 = 5.748_2;

const SHUTTLE_SPEED: f64 = 2.6;
const SHUTTLE_LENGTH: f64 = 6.5;
const SHUTTLE_BODY: f64 = 2.4;
const HORIZON: u64 = 2;

const LOAD_SECONDS: f64 = 40.0;
const TIP_SECONDS: f64 = 55.0;

const TICK: f64 = 1.0;
const RUN_FOR: f64 = 2.0 * 3600.0;

const C_YARD: (u8, u8, u8) = (0xe6, 0xe9, 0xdd);
const C_FREE: (u8, u8, u8) = (0x5a, 0x64, 0x73);
const C_BRIDGE: (u8, u8, u8) = (0xf2, 0xc1, 0x4e);
const C_GRAPH: (u8, u8, u8) = (0x78, 0x82, 0x96);
const C_PLAN: (u8, u8, u8) = (0xf2, 0xc1, 0x4e);
const SHUTTLE_RGB: [(u8, u8, u8); 4] = [
    (0x4f, 0xa3, 0xd1),
    (0xe0, 0x6c, 0x5f),
    (0x6f, 0xc2, 0x8b),
    (0xc2, 0x8b, 0xe0),
];

// --------------------------------------------------------------------- yard

/// Everything the example needs to look a resource up by name afterwards.
struct Yard {
    index: Arc<WorkspaceIndex>,
    datum: Geo,
    nodes: OMap<&'static str, Uuid>,
    zones: Vec<ZoneView>,
    graph_lines: Vec<Vec<Point>>,
}

/// A zone as the visualiser needs it: what to draw, and the id to look its
/// claim state up by.
struct ZoneView {
    name: String,
    id: Uuid,
    outline: Vec<Point>,
    exclusive: bool,
}

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

fn zone(
    name: &str,
    numeric: u64,
    bbox: (f64, f64, f64, f64),
    policy: &str,
    extra: &[(&str, &str)],
) -> zoneout::Zone {
    let (min_x, min_y, max_x, max_y) = bbox;
    let mut builder = ZoneBuilder::new()
        .with_name(name)
        .with_kind("zone")
        .with_boundary(rectangle(min_x, min_y, max_x, max_y))
        .with_datum(Geo::new(DATUM_LAT, DATUM_LON, 0.0))
        .with_resolution(0.0)
        .with_property(NUMERIC_ID_PROPERTY, numeric.to_string())
        .with_property("traffic.policy", policy);
    for (key, value) in extra {
        builder = builder.with_property(*key, *value);
    }
    builder.build().expect("zone")
}

fn node_data(name: &str, numeric: u64, x: f64, y: f64) -> NodeData {
    let mut data = NodeData::new(Point::new(x, y, 0.0));
    data.name = name.into();
    data.properties = [
        (NUMERIC_ID_PROPERTY.to_string(), numeric.to_string()),
        ("label".to_string(), name.to_string()),
    ]
    .into_iter()
    .collect();
    data
}

fn edge_data(numeric: u64, name: &str) -> EdgeData {
    EdgeData {
        id: Uuid::new_v4(),
        zone_ids: Vec::new(),
        properties: [
            (NUMERIC_ID_PROPERTY.to_string(), numeric.to_string()),
            ("label".to_string(), name.to_string()),
            ("traffic.lane_type".to_string(), "corridor".to_string()),
        ]
        .into_iter()
        .collect(),
    }
}

/// The yard: two buffers, a single-lane weighbridge, an apron, two intakes.
///
/// The weighbridge is the only span between the south half and the north
/// half, and its deck is one edge inside an exclusive zone — so the graph
/// itself, not a special case in the coordinator, is what serialises traffic.
fn build_yard() -> Result<Yard, Box<dyn Error>> {
    let datum = Geo::new(DATUM_LAT, DATUM_LON, 0.0);
    let mut root = ZoneBuilder::new()
        .with_name("packhouse_yard")
        .with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 150.0, 185.0))
        .with_datum(datum)
        .with_resolution(0.0)
        .with_property("traffic.policy", "shared")
        .build()?;

    let plan: [(&str, u64, (f64, f64, f64, f64), &str, &[(&str, &str)]); 6] = [
        (
            "buffer_south",
            1,
            (10.0, 10.0, 60.0, 50.0),
            "shared",
            &[("traffic.capacity", "2")],
        ),
        (
            "buffer_north",
            2,
            (90.0, 10.0, 140.0, 50.0),
            "shared",
            &[("traffic.capacity", "2")],
        ),
        (
            "weighbridge",
            3,
            (60.0, 58.0, 90.0, 92.0),
            "exclusive",
            &[("traffic.claim_required", "true")],
        ),
        (
            "apron",
            4,
            (10.0, 95.0, 140.0, 135.0),
            "shared",
            &[("traffic.capacity", "3")],
        ),
        (
            "intake_1",
            5,
            (10.0, 140.0, 60.0, 175.0),
            "exclusive",
            &[("traffic.claim_required", "true")],
        ),
        (
            "intake_2",
            6,
            (90.0, 140.0, 140.0, 175.0),
            "exclusive",
            &[("traffic.claim_required", "true")],
        ),
    ];

    let mut zones = Vec::new();
    for (name, numeric, bbox, policy, extra) in plan {
        let z = zone(name, numeric, bbox, policy, extra);
        zones.push(ZoneView {
            name: name.to_string(),
            id: z.id(),
            outline: z.poly().field_boundary().vertices.clone(),
            exclusive: policy == "exclusive",
        });
        root.add_child(z)?;
    }

    let mut ws = Workspace::new(root);
    ws.set_coord_mode(CoordMode::Local);
    ws.set_datum(datum);

    let places: [(&str, u64, f64, f64); 7] = [
        ("buf_s", 1001, 35.0, 30.0),
        ("buf_n", 1002, 115.0, 30.0),
        ("weigh_in", 1003, 75.0, 62.0),
        ("weigh_out", 1004, 75.0, 88.0),
        ("apron_mid", 1005, 75.0, 115.0),
        ("intake_1", 1006, 35.0, 157.0),
        ("intake_2", 1007, 115.0, 157.0),
    ];
    let mut vertex = OMap::new();
    let mut nodes = OMap::new();
    for (name, numeric, x, y) in places {
        let vid = ws.add_node_data(node_data(name, numeric, x, y));
        let uuid = ws.graph().get_vertex(vid).expect("vertex").id;
        vertex.insert(name, vid);
        nodes.insert(name, uuid);
    }

    // The bridge deck weighs the same as any other link. It is scarce because
    // of the zone it sits in, not because it is expensive to cross.
    let links: [(&str, &str, f64, u64, &str); 6] = [
        ("buf_s", "weigh_in", 34.0, 2001, "south_buffer_to_bridge"),
        ("buf_n", "weigh_in", 51.0, 2002, "north_buffer_to_bridge"),
        ("weigh_in", "weigh_out", 26.0, 2003, "bridge_deck"),
        ("weigh_out", "apron_mid", 27.0, 2004, "bridge_to_apron"),
        ("apron_mid", "intake_1", 55.0, 2005, "apron_to_intake_1"),
        ("apron_mid", "intake_2", 55.0, 2006, "apron_to_intake_2"),
    ];
    let mut graph_lines = Vec::new();
    for (from, to, weight, numeric, name) in links {
        ws.add_edge_data(
            vertex[from],
            vertex[to],
            weight,
            EdgeType::Undirected,
            edge_data(numeric, name),
        );
        graph_lines.push(vec![place_of(&ws, nodes[from]), place_of(&ws, nodes[to])]);
    }

    Ok(Yard {
        index: Arc::new(WorkspaceIndex::new(Arc::new(ws))),
        datum,
        nodes,
        zones,
        graph_lines,
    })
}

fn place_of(ws: &Workspace, node_id: Uuid) -> Point {
    for vid in ws.graph().vertices() {
        if let Some(v) = ws.graph().get_vertex(vid)
            && v.id == node_id
        {
            return Point::new(v.position.x, v.position.y, 0.0);
        }
    }
    Point::new(0.0, 0.0, 0.0)
}

// ------------------------------------------------------------------ shuttles

#[derive(Clone, Copy, PartialEq, Eq)]
enum Job {
    Loading,
    Hauling,
    Tipping,
    Returning,
}

impl Job {
    fn label(self) -> &'static str {
        match self {
            Job::Loading => "loading at the buffer",
            Job::Hauling => "hauling to the intake",
            Job::Tipping => "tipping",
            Job::Returning => "returning empty",
        }
    }
}

/// A shuttle's position along the polyline its route plan traces.
#[derive(Default, Clone)]
struct Path {
    points: Vec<Point>,
    node_ids: Vec<Uuid>,
    edge_ids: Vec<Uuid>,
    travelled: f64,
}

impl Path {
    fn from_plan(plan: &RoutePlan, yard: &Yard) -> Self {
        let points = plan
            .traversed_node_ids
            .iter()
            .map(|id| place_of(yard.index.workspace(), *id))
            .collect();
        Self {
            points,
            node_ids: plan.traversed_node_ids.clone(),
            edge_ids: plan.traversed_edge_ids.clone(),
            travelled: 0.0,
        }
    }

    fn leg_lengths(&self) -> Vec<f64> {
        self.points
            .windows(2)
            .map(|w| (w[1].x - w[0].x).hypot(w[1].y - w[0].y))
            .collect()
    }

    fn total(&self) -> f64 {
        self.leg_lengths().iter().sum()
    }

    /// How far the shuttle may advance before it would enter the leg after
    /// `legs` — the horizon it actually holds a claim on.
    fn distance_through(&self, legs: usize) -> f64 {
        self.leg_lengths().iter().take(legs).sum()
    }

    fn done(&self) -> bool {
        self.points.len() < 2 || self.travelled >= self.total() - 1e-6
    }

    /// Index of the last node the shuttle has reached or passed.
    fn node_index(&self) -> usize {
        let mut walked = 0.0;
        for (i, leg) in self.leg_lengths().iter().enumerate() {
            if self.travelled < walked + leg - 1e-6 {
                return i;
            }
            walked += leg;
        }
        self.points.len().saturating_sub(1)
    }

    fn pose(&self) -> (Point, f64) {
        if self.points.is_empty() {
            return (Point::new(0.0, 0.0, 0.0), 0.0);
        }
        if self.points.len() == 1 {
            return (self.points[0], 0.0);
        }
        let mut walked = 0.0;
        for (i, leg) in self.leg_lengths().iter().enumerate() {
            if self.travelled <= walked + leg || i + 2 == self.points.len() {
                let (a, b) = (self.points[i], self.points[i + 1]);
                let t = if *leg > 1e-9 {
                    ((self.travelled - walked) / leg).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let at = Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t, 0.0);
                return (at, (b.y - a.y).atan2(b.x - a.x));
            }
            walked += leg;
        }
        (*self.points.last().unwrap(), 0.0)
    }
}

struct Shuttle {
    id: RobotId,
    name: &'static str,
    home: &'static str,
    job: Job,
    countdown: f64,
    path: Path,
    /// How many legs ahead the coordinator has granted. Zero means held.
    granted_legs: usize,
    bins: u64,
    waited: f64,
    trail: Vec<Point>,
    note: String,
}

impl Shuttle {
    fn new(id: u64, name: &'static str, home: &'static str, yard: &Yard) -> Self {
        let at = place_of(yard.index.workspace(), yard.nodes[home]);
        Self {
            id: RobotId::new(id),
            name,
            home,
            job: Job::Loading,
            countdown: LOAD_SECONDS * (0.4 + 0.3 * id as f64),
            path: Path {
                points: vec![at],
                ..Path::default()
            },
            granted_legs: 0,
            bins: 0,
            waited: 0.0,
            trail: Vec::new(),
            note: "waiting on a bin".into(),
        }
    }

    fn at(&self) -> Point {
        self.path.pose().0
    }

    fn colour(&self) -> (u8, u8, u8) {
        SHUTTLE_RGB[(self.id.raw() as usize - 1) % SHUTTLE_RGB.len()]
    }
}

// ---------------------------------------------------------------------- main

fn main() -> Result<(), Box<dyn Error>> {
    let speedup: f64 = std::env::var("SPEEDUP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120.0);

    let yard = build_yard()?;
    let rec = rerun_viz::connect("syncbot_packhouse_yard")?;

    let mut coord = Coordinator::with_index(Arc::clone(&yard.index));
    let mut shuttles = vec![
        Shuttle::new(1, "shuttle_1", "buf_s", &yard),
        Shuttle::new(2, "shuttle_2", "buf_n", &yard),
        Shuttle::new(3, "shuttle_3", "buf_s", &yard),
        Shuttle::new(4, "shuttle_4", "buf_n", &yard),
    ];
    for shuttle in &shuttles {
        coord.register_robot(RobotState {
            robot_id: shuttle.id,
            horizon: HORIZON,
            ..RobotState::default()
        });
    }

    println!(
        "packhouse yard, Noordoostpolder — {} shuttles",
        shuttles.len()
    );
    println!("  one single-lane weighbridge joins the two halves of the yard");
    println!("  claims are on nodes and edges; zone intent is derived from them");
    println!("  running at {speedup:.0}x real time\n");

    log_static_yard(&rec, &yard)?;

    let bridge = yard
        .zones
        .iter()
        .find(|z| z.name == "weighbridge")
        .map(|z| z.id)
        .expect("weighbridge");

    let mut clock = 0.0_f64;
    let mut delivered = 0u64;
    let mut bridge_busy = 0.0_f64;
    let mut blocked_now;

    while clock < RUN_FOR {
        let tick = clock as u64;
        blocked_now = 0;

        for i in 0..shuttles.len() {
            match shuttles[i].job {
                Job::Loading | Job::Tipping => {
                    shuttles[i].countdown -= TICK;
                    if shuttles[i].countdown <= 0.0 {
                        let heading_out = shuttles[i].job == Job::Loading;
                        if heading_out {
                            let goal = free_intake(&shuttles, i, &yard);
                            dispatch(&mut coord, &mut shuttles[i], &yard, goal, tick, &rec)?;
                            shuttles[i].job = Job::Hauling;
                        } else {
                            let home = shuttles[i].home;
                            dispatch(&mut coord, &mut shuttles[i], &yard, home, tick, &rec)?;
                            shuttles[i].job = Job::Returning;
                        }
                    }
                }
                Job::Hauling | Job::Returning => {
                    let allowed = shuttles[i].path.distance_through(shuttles[i].granted_legs);
                    let wants_more = shuttles[i].path.travelled >= allowed - 1e-6;

                    if wants_more
                        && !shuttles[i].path.done()
                        && let Extend::Denied = try_extend(&mut coord, &mut shuttles[i], tick, &rec)
                    {
                        blocked_now += 1;
                        shuttles[i].waited += TICK;
                        let yielded = yield_check(&coord, &shuttles, i);
                        shuttles[i].note = if yielded {
                            "yielding — another machine holds the ground ahead".into()
                        } else {
                            "held at the claim boundary".into()
                        };
                    }

                    let allowed = shuttles[i].path.distance_through(shuttles[i].granted_legs);
                    let before = shuttles[i].path.node_index();
                    let step = (SHUTTLE_SPEED * TICK).min(allowed - shuttles[i].path.travelled);
                    if step > 0.0 {
                        shuttles[i].path.travelled += step;
                        let at = shuttles[i].at();
                        shuttles[i].trail.push(at);
                    }
                    let after = shuttles[i].path.node_index();
                    if after != before {
                        report_progress(&mut coord, &shuttles[i], after, tick);
                    }

                    if shuttles[i].path.done() {
                        arrive(&mut coord, &mut shuttles[i], tick, &mut delivered, &rec)?;
                    }
                }
            }
        }

        // --- log the tick
        let claimed = claimed_zones(&yard, &coord);
        if claimed.contains_key(&bridge) {
            bridge_busy += TICK;
        }
        rec.set_duration_secs("sim", clock);
        log_zone_state(&rec, &yard, &claimed)?;
        for shuttle in &shuttles {
            log_shuttle(&rec, &yard, shuttle)?;
        }
        rec.log("stats/bins_delivered", &Scalars::new([delivered as f64]))?;
        rec.log("stats/shuttles_held", &Scalars::new([blocked_now as f64]))?;
        rec.log(
            "stats/claims_active",
            &Scalars::new([coord.claim_manager().request_count() as f64]),
        )?;

        if (clock as u64).is_multiple_of(600) {
            println!(
                "  {}  {delivered} bins in   {} held   {} claims live",
                hms(clock),
                blocked_now,
                coord.claim_manager().request_count()
            );
        }

        clock += TICK;
        if speedup > 0.0 {
            std::thread::sleep(Duration::from_secs_f64(TICK / speedup));
        }
    }

    println!("\n{} simulated, {delivered} bins delivered", hms(clock));
    for shuttle in &shuttles {
        println!(
            "  {:<10} {:>3} bins, {:>5.0}s held at a claim boundary",
            shuttle.name, shuttle.bins, shuttle.waited
        );
    }
    println!(
        "\nweighbridge claimed {:.0}% of the run — the yard is bridge-limited, and",
        100.0 * bridge_busy / clock
    );
    println!("a fifth shuttle would add waiting, not bins. That is the finding: the");
    println!("coordinator does not create the bottleneck, it makes it measurable.");
    Ok(())
}

// ------------------------------------------------------------ the library bits

/// Plan a route and hand it to the coordinator, then ask whether the schedule
/// clears. `Queue` and `Replan` are not failures — they are the coordinator
/// saying *not yet* and *not this way*, which the shuttle obeys by starting
/// with nothing granted.
fn dispatch(
    coord: &mut Coordinator,
    shuttle: &mut Shuttle,
    yard: &Yard,
    goal: &str,
    tick: u64,
    rec: &RecordingStream,
) -> Result<(), Box<dyn Error>> {
    let from = nearest_node(yard, shuttle.at());
    let to = yard.nodes[goal];
    let result = plan_route(&yard.index, from, to, true);
    let Some(plan) = result.plan else {
        shuttle.note = "no route — waiting".into();
        return Ok(());
    };

    shuttle.path = Path::from_plan(&plan, yard);
    shuttle.granted_legs = 0;
    coord.assign_route_plan(shuttle.id, plan, HORIZON, tick);

    let decision = coord.schedule_robot_route(
        shuttle.id,
        ClaimId::new(shuttle.id.raw()),
        tick,
        1.0,
        ClaimAccessMode::Exclusive,
    );
    shuttle.note = match decision.kind {
        ScheduleDecisionKind::Proceed => format!("cleared for {goal}"),
        ScheduleDecisionKind::Queue => {
            format!("queued for {goal}, position {}", decision.queue_position)
        }
        ScheduleDecisionKind::Replan => {
            format!("replan for {goal}, {} conflicts", decision.conflicts.len())
        }
    };
    log_note(rec, shuttle);
    Ok(())
}

/// What came back from asking for the next slice of route.
enum Extend {
    /// The coordinator granted it; the shuttle may drive on.
    Granted,
    /// Somebody else holds ground the shuttle needs. It waits where it is.
    Denied,
    /// Nothing left to claim — the shuttle is on its final approach.
    Complete,
}

/// Ask for the next slice of the route. This is the rolling horizon: the
/// request only ever covers the next `HORIZON` steps, and `upsert` replaces
/// the previous slice — so moving forward is what releases the ground behind.
fn try_extend(
    coord: &mut Coordinator,
    shuttle: &mut Shuttle,
    tick: u64,
    rec: &RecordingStream,
) -> Extend {
    let request = coord.claim_request_for_robot(
        shuttle.id,
        ClaimId::new(shuttle.id.raw()),
        ClaimAccessMode::Exclusive,
    );
    if request.targets.is_empty() {
        shuttle.granted_legs = shuttle.path.points.len().saturating_sub(1);
        return Extend::Complete;
    }

    if coord.claim_manager().evaluate_request(&request).decision != ClaimDecision::Grant {
        return Extend::Denied;
    }

    let edges = request
        .targets
        .iter()
        .filter(|t| t.kind == ClaimTargetKind::Edge)
        .count();
    let claim_id = request.id;
    coord.claim_manager_mut().upsert_request_for_robot(request);
    coord.update_robot_claim_state(shuttle.id, vec![claim_id], Vec::new(), Some(tick));

    let reached = shuttle.path.node_index();
    let was = shuttle.granted_legs;
    shuttle.granted_legs = (reached + edges).min(shuttle.path.points.len() - 1);
    if shuttle.granted_legs > was {
        shuttle.note = format!("granted {} more leg(s)", shuttle.granted_legs - was);
        log_note(rec, shuttle);
    }
    Extend::Granted
}

/// Tell the coordinator which node the shuttle has just reached, then let it
/// drop whatever the shuttle has already driven past.
fn report_progress(coord: &mut Coordinator, shuttle: &Shuttle, node_index: usize, tick: u64) {
    let node = shuttle.path.node_ids.get(node_index).copied();
    let edge = node_index
        .checked_sub(1)
        .and_then(|i| shuttle.path.edge_ids.get(i).copied());
    coord.update_robot_progress(shuttle.id, node, edge, tick);
    coord.release_behind_progress(shuttle.id);
}

/// End of a run: tip a bin, or park at the buffer for the next one.
fn arrive(
    coord: &mut Coordinator,
    shuttle: &mut Shuttle,
    tick: u64,
    delivered: &mut u64,
    rec: &RecordingStream,
) -> Result<(), Box<dyn Error>> {
    match shuttle.job {
        Job::Hauling => {
            shuttle.job = Job::Tipping;
            shuttle.countdown = TIP_SECONDS;
            shuttle.bins += 1;
            *delivered += 1;
            shuttle.note = "tipping into the intake".into();
        }
        Job::Returning => {
            shuttle.job = Job::Loading;
            shuttle.countdown = LOAD_SECONDS;
            shuttle.note = "waiting on a bin".into();
        }
        _ => return Ok(()),
    }
    // Nothing is being driven any more, so the claim goes back whole.
    coord
        .claim_manager_mut()
        .remove_requests_for_robot(shuttle.id);
    coord.update_robot_progress(shuttle.id, None, None, tick);
    log_note(rec, shuttle);
    Ok(())
}

/// Would this shuttle lose a right-of-way call against whoever is ahead of it?
///
/// The coordinator has already refused the claim; this only decides how the
/// hold is described. A shuttle that yields on merit reads differently from
/// one that simply arrived second.
fn yield_check(coord: &Coordinator, shuttles: &[Shuttle], index: usize) -> bool {
    let me = &shuttles[index];
    let Some(mine) = coord.find_robot_state(me.id) else {
        return false;
    };
    shuttles.iter().enumerate().any(|(j, other)| {
        if j == index {
            return false;
        }
        let Some(theirs) = coord.find_robot_state(other.id) else {
            return false;
        };
        let ctx = ArbitrationContext {
            self_priority: 0.0,
            other_priority: 0.0,
            self_holds_lease: false,
            other_holds_lease: !coord.claim_manager().leases_for_robot(other.id).is_empty(),
            self_state: mine.progress_state,
            other_state: theirs.progress_state,
            self_wait_ticks: mine.wait_ticks,
            other_wait_ticks: theirs.wait_ticks,
            self_remaining_steps: remaining(me),
            other_remaining_steps: remaining(other),
            ..ArbitrationContext::default()
        };
        arbitrate_right_of_way(&ctx) == ArbitrationDecision::Yield
    })
}

fn remaining(shuttle: &Shuttle) -> u64 {
    shuttle
        .path
        .points
        .len()
        .saturating_sub(shuttle.path.node_index() + 1) as u64
}

/// Pick the intake fewest other shuttles are already committed to, so the
/// fleet spreads over both instead of queueing on one.
fn free_intake(shuttles: &[Shuttle], index: usize, yard: &Yard) -> &'static str {
    let committed = |name: &str| {
        let goal = yard.nodes[name];
        shuttles
            .iter()
            .enumerate()
            .filter(|(j, s)| *j != index && matches!(s.job, Job::Hauling | Job::Tipping))
            .filter(|(_, s)| s.path.node_ids.last() == Some(&goal))
            .count()
    };
    if committed("intake_1") <= committed("intake_2") {
        "intake_1"
    } else {
        "intake_2"
    }
}

fn nearest_node(yard: &Yard, at: Point) -> Uuid {
    let mut best = (f64::INFINITY, Uuid::nil());
    for id in yard.nodes.values() {
        let p = place_of(yard.index.workspace(), *id);
        let d = (p.x - at.x).hypot(p.y - at.y);
        if d < best.0 {
            best = (d, *id);
        }
    }
    best.1
}

// --------------------------------------------------------------------- rerun

/// Yard outline and the road graph — logged once, they do not move.
fn log_static_yard(rec: &RecordingStream, yard: &Yard) -> Result<(), Box<dyn Error>> {
    let outline = rectangle(0.0, 0.0, 150.0, 185.0).vertices;
    rerun_viz::log_polygon_3d(
        rec,
        "enu/yard/outline",
        &outline,
        rerun_viz::rgb(C_YARD),
        0.6,
    )?;
    rerun_viz::log_polygon_geo(
        rec,
        "wgs/yard/outline",
        &outline,
        yard.datum,
        rerun_viz::rgb(C_YARD),
    )?;

    rerun_viz::log_polylines_3d(
        rec,
        "enu/yard/graph",
        &yard.graph_lines,
        rerun_viz::rgb(C_GRAPH),
        0.35,
    )?;
    rerun_viz::log_polylines_geo(
        rec,
        "wgs/yard/graph",
        &yard.graph_lines,
        yard.datum,
        rerun_viz::rgb(C_GRAPH),
    )?;

    let mut places = Vec::new();
    let mut labels = Vec::new();
    for (name, id) in &yard.nodes {
        places.push(place_of(yard.index.workspace(), *id));
        labels.push((*name).to_string());
    }
    rerun_viz::log_points_3d(
        rec,
        "enu/yard/nodes",
        &places,
        &labels,
        rerun_viz::rgb(C_GRAPH),
        1.1,
    )?;
    rerun_viz::log_points_geo(
        rec,
        "wgs/yard/nodes",
        &places,
        yard.datum,
        rerun_viz::rgb(C_GRAPH),
        2.5,
    )?;
    Ok(())
}

/// Zones tinted by who has intent on them. The colour comes from the claim
/// ledger, not from where the shuttles happen to be — that is the point of
/// deriving zone intent from node and edge claims.
fn log_zone_state(
    rec: &RecordingStream,
    yard: &Yard,
    claimed: &OMap<Uuid, u64>,
) -> Result<(), Box<dyn Error>> {
    for zone in &yard.zones {
        let name = &zone.name;
        let (color, radius) = match claimed.get(&zone.id).copied() {
            Some(robot) => (
                rerun_viz::rgb(SHUTTLE_RGB[(robot as usize - 1) % SHUTTLE_RGB.len()]),
                1.5,
            ),
            None if zone.exclusive => (rerun_viz::scaled(C_BRIDGE, 0.45), 0.6),
            None => (rerun_viz::rgb(C_FREE), 0.5),
        };
        rerun_viz::log_polygon_3d(
            rec,
            &format!("enu/yard/zones/{name}"),
            &zone.outline,
            color,
            radius,
        )?;
        rerun_viz::log_polygon_geo(
            rec,
            &format!("wgs/yard/zones/{name}"),
            &zone.outline,
            yard.datum,
            color,
        )?;
    }
    Ok(())
}

/// Which zone each live claim implies intent on, read back out of the ledger.
fn claimed_zones(yard: &Yard, coord: &Coordinator) -> OMap<Uuid, u64> {
    let mut held = OMap::new();
    for request in coord.claim_manager().requests() {
        for target in &request.targets {
            let zones = match target.kind {
                ClaimTargetKind::Zone => vec![target.resource_id],
                ClaimTargetKind::Node => yard
                    .index
                    .zones_of_node(target.resource_id)
                    .iter()
                    .map(|z| z.id())
                    .collect(),
                ClaimTargetKind::Edge => yard
                    .index
                    .zones_of_edge(target.resource_id)
                    .iter()
                    .map(|z| z.id())
                    .collect(),
            };
            for zone in zones {
                held.entry(zone).or_insert(request.robot_id.raw());
            }
        }
    }
    held
}

fn log_shuttle(
    rec: &RecordingStream,
    yard: &Yard,
    shuttle: &Shuttle,
) -> Result<(), Box<dyn Error>> {
    let (at, yaw) = shuttle.path.pose();
    let color = rerun_viz::rgb(shuttle.colour());
    let base = format!("enu/shuttle/{}", shuttle.name);

    rerun_viz::log_machine_3d(rec, &base, at, yaw, color, SHUTTLE_LENGTH, SHUTTLE_BODY)?;
    rerun_viz::log_points_geo(
        rec,
        &format!("wgs/shuttle/{}", shuttle.name),
        &[at],
        yard.datum,
        color,
        3.0,
    )?;

    // The granted part of the plan is drawn bright, the ungranted tail dim —
    // so a shuttle held at its claim boundary is visible as a short bright
    // stub with a long dark road ahead of it.
    if shuttle.path.points.len() >= 2 {
        let cut = (shuttle.granted_legs + 1).min(shuttle.path.points.len());
        let granted = shuttle.path.points[..cut].to_vec();
        let pending = shuttle.path.points[cut.saturating_sub(1)..].to_vec();
        rerun_viz::log_polylines_3d(
            rec,
            &format!("{base}/granted"),
            &[granted.clone()],
            rerun_viz::rgb(C_PLAN),
            0.5,
        )?;
        rerun_viz::log_polylines_3d(
            rec,
            &format!("{base}/pending"),
            &[pending.clone()],
            rerun_viz::scaled(C_PLAN, 0.35),
            0.3,
        )?;
        rerun_viz::log_polylines_geo(
            rec,
            &format!("wgs/shuttle/{}/plan", shuttle.name),
            &[granted],
            yard.datum,
            rerun_viz::rgb(C_PLAN),
        )?;
    }

    if shuttle.trail.len() >= 2 {
        rerun_viz::log_polylines_3d(
            rec,
            &format!("{base}/trail"),
            std::slice::from_ref(&shuttle.trail),
            rerun_viz::scaled(shuttle.colour(), 0.5),
            0.18,
        )?;
    }
    Ok(())
}

fn log_note(rec: &RecordingStream, shuttle: &Shuttle) {
    let _ = rec.log(
        format!("log/{}", shuttle.name),
        &TextLog::new(format!(
            "{} — {} ({})",
            shuttle.name,
            shuttle.note,
            shuttle.job.label()
        )),
    );
}

fn hms(seconds: f64) -> String {
    let total = seconds as u64;
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        (total % 3600) / 60,
        total % 60
    )
}
