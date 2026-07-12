//! Small REST server for the `scripts/rest_demo.py` simulation.
//!
//! Boots a workspace with three exclusive zones (numeric IDs 100/101/102)
//! and serves the syncbot REST API on 127.0.0.1:8080. Run with:
//!
//! ```sh
//! cargo run --example rest_server --features rest
//! ```

use std::sync::Arc;

use datapod::{Geo, Point, Polygon};
use syncbot::wire::{ServeState, rest};
use syncbot::{Coordinator, NUMERIC_ID_PROPERTY, WorkspaceIndex};
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
    // OPT-IN persistence: only when `SYNCBOT_STATE` is set. Unset/empty →
    // fully in-memory, no file touched, behaviour identical to before.
    let persist_path = std::env::var("SYNCBOT_STATE")
        .ok()
        .filter(|s| !s.is_empty());
    let coord = match persist_path.as_deref() {
        Some(path) => match syncbot::persist::load(std::path::Path::new(path)) {
            Ok(Some(snapshot)) => {
                info!(
                    state_file = %path,
                    robots = snapshot.robot_states.len(),
                    "restored syncbot state from disk"
                );
                Coordinator::restore(snapshot, Some(Arc::clone(&idx)))
            }
            Ok(None) => {
                info!(state_file = %path, "no existing state file; starting fresh");
                Coordinator::with_index(Arc::clone(&idx))
            }
            Err(e) => {
                tracing::warn!(state_file = %path, error = %e, "failed to load state file; starting fresh");
                Coordinator::with_index(Arc::clone(&idx))
            }
        },
        None => Coordinator::with_index(Arc::clone(&idx)),
    };
    // OPT-IN admin auth: only when `SYNCBOT_ADMIN_AUTH` is truthy. Unset/other →
    // admin endpoints stay OPEN, behaviour identical to before.
    let admin_auth = env_truthy("SYNCBOT_ADMIN_AUTH");
    if admin_auth {
        info!("admin auth ENABLED: mutation endpoints require the owning robot's key");
    }
    let state = ServeState::new(coord).with_admin_auth(admin_auth);
    let _sweeper =
        syncbot::wire::spawn_inactive_sweeper(state.clone(), std::time::Duration::from_secs(1));
    // Periodic state flush (only when persistence is enabled), every 2s.
    let _flusher = persist_path.clone().map(|path| {
        let coord_handle = state.coordinator();
        let path_buf = std::path::PathBuf::from(path);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                ticker.tick().await;
                if let Err(e) = syncbot::persist::flush(&coord_handle, &path_buf) {
                    tracing::warn!(error = %e, "periodic state flush failed");
                }
            }
        })
    });
    let app = rest::router(state.clone());

    let addr = "127.0.0.1:8080";
    let listener = tokio::net::TcpListener::bind(addr).await?;

    println!("syncbot REST demo server");
    println!("  listening on http://{addr}");
    println!();
    println!("zones (all exclusive):");
    println!("  dock_a    numeric_id=100  uuid={ZONE_DOCK_A}");
    println!("  dock_b    numeric_id=101  uuid={ZONE_DOCK_B}");
    println!("  junction  numeric_id=102  uuid={ZONE_JUNCTION}");
    println!();
    println!("try: curl http://{addr}/ares/v1/health");
    info!(addr = %addr, "REST demo server listening");

    // When persistence is enabled, flush a final snapshot on Ctrl-C via
    // graceful shutdown. When it is not, keep the exact prior serve path.
    match persist_path.clone() {
        Some(path) => {
            let coord_handle = state.coordinator();
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = tokio::signal::ctrl_c().await;
                    if let Err(e) =
                        syncbot::persist::flush(&coord_handle, std::path::Path::new(&path))
                    {
                        tracing::warn!(error = %e, "final state flush on shutdown failed");
                    } else {
                        info!("flushed final syncbot state on shutdown");
                    }
                })
                .await?;
        }
        None => axum::serve(listener, app).await?,
    }
    Ok(())
}

/// Truthy env flag: `1`/`true`/`yes` (case-insensitive). Anything else, or
/// unset, is false.
fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes"
        })
        .unwrap_or(false)
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,syncbot=debug,tower_http=debug"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(true)
        .pretty()
        .init();
}
