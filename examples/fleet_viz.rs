//! Live fleet visualizer — a rerun view of who is where and who holds what.
//!
//! This is an **adapter**, not a core feature. It holds a peerbus client and
//! polls `ares/v1/fleet/snapshot`; the coordinator does not know it exists and
//! nothing was added to the wire to serve it. `FleetSnapshot` already carries
//! the datum, the core's clock, each robot's position in both frames, its
//! heading, the claim ledger, and the recent decisions.
//!
//! Geometry comes from the workspace directory rather than the wire: zone
//! boundaries and node coordinates are static, and the read models
//! deliberately carry identity and policy rather than polygons. Point this at
//! the same directory the core is serving.
//!
//! ```sh
//! # terminal 1
//! make run
//! # terminal 2
//! cargo run --example fleet_viz --features "rerun-viz peerbus" -- examples/fixed
//! ```
//!
//! One caveat worth knowing: `/routes/<robot>` only appears for a robot that
//! has a route plan, and a plan is assigned through `Coordinator` directly
//! (library, C ABI, or Python) — the flat wire has no "assign route" call. A
//! fleet driven purely over the wire claims and releases without ever naming
//! a plan, so that view stays empty. Everything else here comes off the wire.
//!
//! `SYNCBOT_VIZ_RRD=<path>` records to a file and opens no window — for a
//! headless box, or for keeping the evidence from a run worth studying.
//! `RERUN_URL` attaches to an already-running viewer; otherwise one is
//! spawned. `SYNCBOT_PEERBUS_IDENTITY` selects the core, matching the server.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use concord::{Enu, to_wgs_from_enu};
use datapod::{Geo, Point};
use rerun::{
    Arrows3D, Color, GeoLineStrings, GeoPoints, LineStrips3D, Points3D, RecordingStream,
    RecordingStreamBuilder, Scalars, TextLog,
};
use syncbot::wire::peerbus::{CORE_IDENTITY, Client};
use syncbot::wire::{FleetEventKind, FleetSnapshot};
use syncbot::{ClaimTargetKind, WorkspaceIndex};
use uuid::Uuid;
use zoneout::Workspace;

/// Same palette zoneout draws workspaces with, so the two agree on colour.
const PALETTE_RGB: [[u8; 3]; 10] = [
    [255, 100, 100],
    [100, 255, 100],
    [100, 100, 255],
    [255, 255, 100],
    [100, 255, 255],
    [255, 100, 255],
    [255, 150, 100],
    [150, 100, 255],
    [100, 255, 150],
    [255, 100, 150],
];

/// Zones nobody is in.
fn idle_zone_color() -> Color {
    Color::from_rgb(90, 100, 115)
}

fn palette_rgb(index: usize) -> [u8; 3] {
    PALETTE_RGB[index % PALETTE_RGB.len()]
}

fn palette_color(index: usize) -> Color {
    let [r, g, b] = palette_rgb(index);
    Color::from_rgb(r, g, b)
}

/// Stable colour per robot, so a robot keeps its colour across ticks.
fn robot_color(robot: u64) -> Color {
    palette_color(robot as usize)
}

/// The same colour, dimmed — used for a robot that has gone quiet.
fn dimmed(robot: u64) -> Color {
    let [r, g, b] = palette_rgb(robot as usize);
    Color::from_rgb(r / 3, g / 3, b / 3)
}

/// Where the recording goes, in order of preference:
///
/// - `SYNCBOT_VIZ_RRD` — write to that file and never open a window. This is
///   what a headless box or a failing test wants.
/// - `RERUN_URL` — attach to a viewer someone already has open.
/// - otherwise spawn a viewer.
///
/// Empty values count as unset, so `RERUN_URL=` in a script means "spawn"
/// rather than "connect to the empty string".
fn connect(app_id: &str) -> Result<RecordingStream, Box<dyn Error>> {
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    if let Some(path) = env("SYNCBOT_VIZ_RRD") {
        return Ok(RecordingStreamBuilder::new(app_id).save(path)?);
    }
    if let Some(url) = env("RERUN_URL") {
        return Ok(RecordingStreamBuilder::new(app_id).connect_grpc_opts(url)?);
    }
    Ok(RecordingStreamBuilder::new(app_id).spawn()?)
}

fn enu_to_latlon(p: Point, datum: Geo) -> [f64; 2] {
    let wgs = to_wgs_from_enu(Enu::new(p.x, p.y, p.z, datum));
    [wgs.latitude, wgs.longitude]
}

fn close<T: Copy>(mut strip: Vec<T>) -> Vec<T> {
    if let Some(&first) = strip.first() {
        strip.push(first);
    }
    strip
}

// ---------------------------------------------------------------------------
// static geometry
// ---------------------------------------------------------------------------

/// Zone boundaries, graph nodes and graph edges. Logged once — they do not
/// move; only who occupies them does.
fn log_geometry(rec: &RecordingStream, index: &WorkspaceIndex) -> Result<(), Box<dyn Error>> {
    let datum = index.datum();

    let graph = index.workspace().graph();
    let mut node_points = Vec::new();
    let mut node_labels = Vec::new();
    let mut node_geo = Vec::new();
    for vid in graph.vertices() {
        let Some(node) = graph.get_vertex(vid) else {
            continue;
        };
        let position = Point::new(node.position.x, node.position.y, node.position.z);
        node_points.push([position.x as f32, position.y as f32, position.z as f32]);
        node_labels.push(node.name.clone());
        if let Some(datum) = datum {
            node_geo.push(enu_to_latlon(position, datum));
        }
    }
    rec.log_static(
        "/graph/nodes",
        &Points3D::new(node_points)
            .with_labels(node_labels)
            .with_colors([Color::from_rgb(210, 215, 225)])
            .with_radii([0.6f32]),
    )?;
    if !node_geo.is_empty() {
        rec.log_static("/map/nodes", &GeoPoints::from_lat_lon(node_geo))?;
    }

    let mut edge_strips = Vec::new();
    for edge in graph.edges() {
        let (Some(source), Some(target)) = (graph.source(edge.id), graph.target(edge.id)) else {
            continue;
        };
        let (Some(a), Some(b)) = (graph.get_vertex(source), graph.get_vertex(target)) else {
            continue;
        };
        edge_strips.push(vec![
            [
                a.position.x as f32,
                a.position.y as f32,
                a.position.z as f32,
            ],
            [
                b.position.x as f32,
                b.position.y as f32,
                b.position.z as f32,
            ],
        ]);
    }
    rec.log_static(
        "/graph/edges",
        &LineStrips3D::new(edge_strips)
            .with_colors([Color::from_rgb(120, 130, 145)])
            .with_radii([0.25f32]),
    )?;

    Ok(())
}

/// Every zone in the workspace, root first.
fn all_zones(index: &WorkspaceIndex) -> Vec<Uuid> {
    let Some(root) = index.root_zone_id() else {
        return Vec::new();
    };
    let mut zones = vec![root];
    zones.extend(index.descendant_zones(root).iter().map(|zone| zone.id()));
    zones
}

// ---------------------------------------------------------------------------
// per-tick state
// ---------------------------------------------------------------------------

/// Who is in a zone, and how.
#[derive(Default)]
struct Occupancy {
    /// Robot holding the zone outright — nothing else may be inside it.
    owner: Option<u64>,
    /// Robots merely passing through, holding nodes or edges within it.
    passing: BTreeSet<u64>,
}

/// Read the claim ledger into per-zone occupancy.
///
/// This is the distinction the coordination model turns on, made visible: a
/// zone claim reserves the whole area, while a route claim reserves only the
/// nodes and edges used and lets a non-interfering route share the zone.
fn occupancy(index: &WorkspaceIndex, snapshot: &FleetSnapshot) -> BTreeMap<Uuid, Occupancy> {
    let mut map: BTreeMap<Uuid, Occupancy> = BTreeMap::new();
    for request in &snapshot.requests {
        let robot = request.robot_id.raw();
        for target in &request.targets {
            match target.kind {
                ClaimTargetKind::Zone => {
                    map.entry(target.resource_id).or_default().owner = Some(robot);
                }
                ClaimTargetKind::Node | ClaimTargetKind::Edge => {
                    let zones = match target.kind {
                        ClaimTargetKind::Node => index.zones_of_node(target.resource_id),
                        _ => index.zones_of_edge(target.resource_id),
                    };
                    for zone in zones {
                        map.entry(zone.id()).or_default().passing.insert(robot);
                        for ancestor in index.ancestor_zones(zone.id()) {
                            map.entry(ancestor.id()).or_default().passing.insert(robot);
                        }
                    }
                }
            }
        }
    }
    map
}

/// Draw each zone in the colour of whoever holds it: thick for a zone held
/// outright, thinner for one merely being driven through, grey for empty.
fn log_zones(
    rec: &RecordingStream,
    index: &WorkspaceIndex,
    occupied: &BTreeMap<Uuid, Occupancy>,
) -> Result<(), Box<dyn Error>> {
    let datum = index.datum();
    for zone_id in all_zones(index) {
        let Some(zone) = index.zone(zone_id) else {
            continue;
        };
        if !zone.poly().has_field_boundary() {
            continue;
        }
        let boundary = zone.poly().field_boundary();
        if boundary.vertices.is_empty() {
            continue;
        }

        let state = occupied.get(&zone_id);
        let (color, radius, label) = match state {
            Some(o) if o.owner.is_some() => {
                let robot = o.owner.unwrap();
                (
                    robot_color(robot),
                    1.6f32,
                    format!("{} — held by {robot}", zone.name()),
                )
            }
            Some(o) if !o.passing.is_empty() => {
                let first = *o.passing.iter().next().unwrap();
                let who: Vec<String> = o.passing.iter().map(|r| r.to_string()).collect();
                (
                    robot_color(first),
                    0.8f32,
                    format!("{} — {} passing through", zone.name(), who.join(", ")),
                )
            }
            _ => (idle_zone_color(), 0.4f32, zone.name().to_string()),
        };

        let strip = close(
            boundary
                .vertices
                .iter()
                .map(|v| [v.x as f32, v.y as f32, 0.0f32])
                .collect::<Vec<_>>(),
        );
        rec.log(
            format!("/zones/{}", zone.name()),
            &LineStrips3D::new([strip])
                .with_colors([color])
                .with_radii([radius])
                .with_labels([label]),
        )?;

        if let Some(datum) = datum {
            let geo = close(
                boundary
                    .vertices
                    .iter()
                    .map(|v| enu_to_latlon(Point::new(v.x, v.y, v.z), datum))
                    .collect::<Vec<_>>(),
            );
            rec.log(
                format!("/map/zones/{}", zone.name()),
                &GeoLineStrings::from_lat_lon([geo])
                    .with_colors([color])
                    .with_radii([radius * 2.0]),
            )?;
        }
    }
    Ok(())
}

/// Robot poses, headings, and planned routes.
fn log_robots(
    rec: &RecordingStream,
    index: &WorkspaceIndex,
    snapshot: &FleetSnapshot,
) -> Result<(), Box<dyn Error>> {
    let inactive: BTreeSet<u64> = snapshot
        .inactive_robot_ids
        .iter()
        .map(|id| id.raw())
        .collect();

    let mut points = Vec::new();
    let mut colors = Vec::new();
    let mut labels = Vec::new();
    let mut geo = Vec::new();
    let mut arrow_origins = Vec::new();
    let mut arrow_vectors = Vec::new();

    for robot in &snapshot.robots {
        let id = robot.robot_id.raw();
        let Some(position) = robot.position else {
            continue;
        };
        points.push([position.x as f32, position.y as f32, position.z as f32]);
        // A robot that has gone quiet is dimmed rather than removed: absence
        // and silence are different things and should not look the same.
        let base = robot_color(id);
        colors.push(if inactive.contains(&id) {
            dimmed(id)
        } else {
            base
        });
        labels.push(if inactive.contains(&id) {
            format!("{id} (quiet)")
        } else {
            format!("{id}")
        });
        if position.converted || snapshot.datum.is_some() {
            geo.push([position.lat, position.lon]);
        }

        if let Some(heading) = robot.heading {
            let (sin, cos) = heading.yaw_rad.sin_cos();
            arrow_origins.push([position.x as f32, position.y as f32, position.z as f32]);
            arrow_vectors.push([(cos * 4.0) as f32, (sin * 4.0) as f32, 0.0f32]);
        }

        // The planned route, with the part already claimed drawn brighter.
        if let Some(plan) = robot.route_plan.as_ref() {
            let strip: Vec<[f32; 3]> = plan
                .traversed_node_ids
                .iter()
                .filter_map(|node_id| index.node(*node_id))
                .map(|node| {
                    [
                        node.position.x as f32,
                        node.position.y as f32,
                        node.position.z as f32,
                    ]
                })
                .collect();
            if strip.len() >= 2 {
                rec.log(
                    format!("/routes/{id}"),
                    &LineStrips3D::new([strip])
                        .with_colors([base])
                        .with_radii([0.45f32]),
                )?;
            }
        }
    }

    rec.log(
        "/robots",
        &Points3D::new(points)
            .with_colors(colors)
            .with_labels(labels)
            .with_radii([1.4f32]),
    )?;
    if !geo.is_empty() {
        rec.log("/map/robots", &GeoPoints::from_lat_lon(geo))?;
    }
    if !arrow_origins.is_empty() {
        rec.log(
            "/robots/heading",
            &Arrows3D::from_vectors(arrow_vectors).with_origins(arrow_origins),
        )?;
    }
    Ok(())
}

/// Decisions, on the same timeline as the geometry that caused them.
///
/// This is what makes a denial explicable: scrub to the moment and the zone
/// that blocked it is lit up beside the log line.
fn log_events(
    rec: &RecordingStream,
    snapshot: &FleetSnapshot,
    already_seen: &mut BTreeSet<(u64, String)>,
) -> Result<(), Box<dyn Error>> {
    for event in &snapshot.events {
        let robot = event
            .robot_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "-".into());
        let text = format!(
            "{:?} robot={robot} reason={} {}{}",
            event.kind,
            event.reason,
            event.zone_names.join(", "),
            event
                .blocked
                .map(|b| format!(" blocked={b}"))
                .unwrap_or_default(),
        );
        if !already_seen.insert((event.at_ms, text.clone())) {
            continue;
        }
        let level = match event.kind {
            FleetEventKind::Denied => "WARN",
            FleetEventKind::Swept | FleetEventKind::Expired => "INFO",
            _ => "INFO",
        };
        rec.log("/events", &TextLog::new(text).with_level(level))?;
    }
    Ok(())
}

fn log_scalars(rec: &RecordingStream, snapshot: &FleetSnapshot) -> Result<(), Box<dyn Error>> {
    let active =
        snapshot.robots.len() - snapshot.inactive_robot_ids.len().min(snapshot.robots.len());
    rec.log("/stats/robots_active", &Scalars::single(active as f64))?;
    rec.log(
        "/stats/robots_quiet",
        &Scalars::single(snapshot.inactive_robot_ids.len() as f64),
    )?;
    rec.log(
        "/stats/claims_held",
        &Scalars::single(snapshot.requests.len() as f64),
    )?;
    rec.log(
        "/stats/denials_recent",
        &Scalars::single(
            snapshot
                .events
                .iter()
                .filter(|e| e.kind == FleetEventKind::Denied)
                .count() as f64,
        ),
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let dir = args.next().unwrap_or_else(|| "examples/fixed".to_string());
    let identity = args.next().unwrap_or_else(|| {
        std::env::var("SYNCBOT_PEERBUS_IDENTITY").unwrap_or_else(|_| CORE_IDENTITY.to_string())
    });

    let workspace = Workspace::load(&dir)?;
    let index = Arc::new(WorkspaceIndex::new(Arc::new(workspace)));
    println!("fleet_viz: geometry from {dir}, live data from peerbus core '{identity}'");

    let client = Client::connect(identity)?;
    let rec = connect("syncbot-fleet")?;
    log_geometry(&rec, &index)?;

    let mut seen = BTreeSet::new();
    let mut first_tick_ms: Option<u64> = None;
    loop {
        match client.fleet_snapshot() {
            Ok(snapshot) => {
                // The core's clock drives the timeline, not this process's —
                // ages stay right even when the viewer runs on another host.
                let origin = *first_tick_ms.get_or_insert(snapshot.now_ms);
                let elapsed = snapshot.now_ms.saturating_sub(origin) as f64 / 1000.0;
                rec.set_duration_secs("fleet", elapsed);

                let occupied = occupancy(&index, &snapshot);
                log_zones(&rec, &index, &occupied)?;
                log_robots(&rec, &index, &snapshot)?;
                log_events(&rec, &snapshot, &mut seen)?;
                log_scalars(&rec, &snapshot)?;
            }
            Err(err) => eprintln!("fleet_viz: snapshot failed: {}", err.message),
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
