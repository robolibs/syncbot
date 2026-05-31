//! Small REST server for the `scripts/rest_demo.py` simulation.
//!
//! Boots a workspace with three exclusive zones (numeric IDs 100/101/102)
//! and serves the timenav REST API on 127.0.0.1:8080. Run with:
//!
//! ```sh
//! cargo run --example rest_server --features rest
//! ```

use std::sync::Arc;

use datapod::{Geo, Point, Polygon};
use timenav::wire::{ServeState, rest};
use timenav::{Coordinator, NUMERIC_ID_PROPERTY, WorkspaceIndex};
use tracing::info;
use tracing_subscriber::EnvFilter;
use uuid::{Uuid, uuid};
use zoneout::{Workspace, ZoneBuilder};

const ZONE_DOCK_A: Uuid = uuid!("00000000-0000-0000-0000-000000000100");
const ZONE_DOCK_B: Uuid = uuid!("00000000-0000-0000-0000-000000000101");
const ZONE_JUNCTION: Uuid = uuid!("00000000-0000-0000-0000-000000000102");

fn rectangle(x0: f64, y0: f64, x1: f64, y1: f64) -> Polygon {
    Polygon {
        vertices: vec![
            Point::new(x0, y0, 0.0),
            Point::new(x1, y0, 0.0),
            Point::new(x1, y1, 0.0),
            Point::new(x0, y1, 0.0),
        ],
    }
}

fn exclusive_zone(
    name: &str,
    uuid: Uuid,
    numeric_id: u64,
    bbox: (f64, f64, f64, f64),
) -> zoneout::Zone {
    let (x0, y0, x1, y1) = bbox;
    let mut zone = ZoneBuilder::new()
        .with_name(name)
        .with_kind("zone")
        .with_boundary(rectangle(x0, y0, x1, y1))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .with_property("traffic.policy", "exclusive")
        .with_property(NUMERIC_ID_PROPERTY, numeric_id.to_string())
        .build()
        .expect("zone");
    zone.set_id(uuid);
    zone
}

fn build_workspace() -> Workspace {
    let mut root = ZoneBuilder::new()
        .with_name("yard")
        .with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 200.0, 200.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root zone");

    let zones = [
        ("dock_a", ZONE_DOCK_A, 100u64, (10.0, 10.0, 60.0, 60.0)),
        ("dock_b", ZONE_DOCK_B, 101u64, (80.0, 10.0, 130.0, 60.0)),
        (
            "junction",
            ZONE_JUNCTION,
            102u64,
            (50.0, 80.0, 100.0, 130.0),
        ),
    ];
    for (name, uuid, num, bbox) in zones {
        root.add_child(exclusive_zone(name, uuid, num, bbox))
            .expect("add zone");
    }
    Workspace::new(root)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();

    let ws = build_workspace();
    let idx = Arc::new(WorkspaceIndex::new(Arc::new(ws)));
    let coord = Coordinator::with_index(idx);
    let state = ServeState::new(coord);
    let app = rest::router(state);

    let addr = "127.0.0.1:8080";
    let listener = tokio::net::TcpListener::bind(addr).await?;

    println!("timenav REST demo server");
    println!("  listening on http://{addr}");
    println!();
    println!("zones (all exclusive):");
    println!("  dock_a    numeric_id=100  uuid={ZONE_DOCK_A}");
    println!("  dock_b    numeric_id=101  uuid={ZONE_DOCK_B}");
    println!("  junction  numeric_id=102  uuid={ZONE_JUNCTION}");
    println!();
    println!("try: curl http://{addr}/ares/v1/health");
    info!(addr = %addr, "REST demo server listening");

    axum::serve(listener, app).await?;
    Ok(())
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,timenav=debug,tower_http=debug"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(true)
        .pretty()
        .init();
}
