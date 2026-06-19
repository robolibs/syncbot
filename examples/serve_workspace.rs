//! Serve the syncbot REST API over a workspace loaded from disk.
//!
//! Unlike `rest_server.rs` (which builds a fixed in-memory workspace), this
//! takes a zoneout workspace **directory** as input and serves whatever zones
//! it contains. If no directory is given, it loads `examples/fixed`. Zones
//! carrying an `external.numeric_id` property are reachable by that integer as
//! well as by UUID.
//!
//! ```sh
//! cargo run --example serve_workspace --features "rest robo" -- [workspace_dir] [bind_addr]
//! ```
//!
//! With the `robo` feature enabled this same process also exposes ARES
//! Zenoh/ROS2DDS service endpoints for `zenoh-bridge-ros2dds`:
//!
//! - ROS2 services: `/ares/v1/...`
//! - ROS2 type: `ares_interfaces/srv/Json`
//! - Zenoh keys: `ares/v1/...`
//!
//! Optional Zenoh environment:
//!
//! ```sh
//! # by default the server listens on tcp/0.0.0.0:7447
//! SYNCBOT_ZENOH_LISTEN=tcp/0.0.0.0:7448
//! ```
//!
//! A workspace directory is what `zoneout::Workspace::save(dir)` writes:
//!
//! ```text
//! <dir>/workspace.json        manifest (version, root zone id, coord mode, datum)
//! <dir>/zones/                recursive zone tree
//! <dir>/graph/graph.json      nodes + edges
//! ```

use std::process::ExitCode;
use std::sync::Arc;

#[cfg(feature = "robo")]
use syncbot::wire::ros2dds::{Ros2DdsAresJsonHandle, serve_ares_json_services};
use syncbot::wire::{ServeState, rest};
use syncbot::{Coordinator, NUMERIC_ID_PROPERTY, ValidationSeverity, WorkspaceIndex};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use zoneout::Workspace;

#[cfg(feature = "robo")]
const DEFAULT_ZENOH_LISTEN: &str = "tcp/0.0.0.0:7447";

#[tokio::main(flavor = "multi_thread", worker_threads = 1)]
async fn main() -> ExitCode {
    init_logging();

    let mut args = std::env::args().skip(1);
    let dir = args.next().unwrap_or_else(|| "examples/fixed".to_string());
    let addr = args.next().unwrap_or_else(|| "0.0.0.0:8080".to_string());

    // 1. Load the workspace from disk.
    let ws = match Workspace::load(&dir) {
        Ok(ws) => ws,
        Err(e) => {
            error!(workspace = %dir, error = %e, "failed to load workspace");
            return ExitCode::FAILURE;
        }
    };
    info!(workspace = %dir, "loaded workspace");

    // 2. Build the index and report any structural issues.
    let idx = Arc::new(WorkspaceIndex::new(Arc::new(ws)));
    let issues = idx.validation_issues();
    let errors = issues
        .iter()
        .filter(|i| i.severity == ValidationSeverity::Error)
        .count();
    if !issues.is_empty() {
        warn!(
            issues = issues.len(),
            errors, "workspace validation reported issues"
        );
        for i in &issues {
            warn!(
                severity = ?i.severity,
                resource_kind = %i.resource_kind,
                category = %i.category,
                message = %i.message,
                "workspace validation issue"
            );
        }
        if errors > 0 {
            error!("refusing to serve a workspace with validation errors");
            return ExitCode::FAILURE;
        }
    }

    // 3. Banner: list zones and their numeric aliases (if any).
    print_zones(&idx);

    // 4. Serve.
    let state = ServeState::new(Coordinator::with_index(idx));
    #[cfg(feature = "robo")]
    let _ros2dds = start_ros2dds(state.clone()).await;
    // Auto-release the claims of robots that stop heartbeating (per their
    // <alive> interval). Checks once a second.
    let _sweeper =
        syncbot::wire::spawn_inactive_sweeper(state.clone(), std::time::Duration::from_secs(1));
    let app = rest::router(state);

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            error!(addr = %addr, error = %e, "failed to bind REST listener");
            return ExitCode::FAILURE;
        }
    };

    println!("\nsyncbot serving workspace '{dir}' on http://{addr}");
    println!("try: curl http://{addr}/ares/v1/health");
    info!(addr = %addr, workspace = %dir, "REST server listening");

    if let Err(e) = axum::serve(listener, app).await {
        error!(error = %e, "REST server error");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(feature = "robo")]
async fn start_ros2dds(state: ServeState) -> Option<(zenoh::Session, Ros2DdsAresJsonHandle)> {
    let config = match zenoh_config_from_env() {
        Ok(config) => config,
        Err(err) => {
            warn!(
                error = %err,
                "ROS2DDS ARES JSON services disabled: invalid Zenoh config"
            );
            return None;
        }
    };

    let session = match zenoh::open(config).await {
        Ok(session) => session,
        Err(err) => {
            warn!(
                error = %err,
                "ROS2DDS ARES JSON services disabled: failed to open Zenoh session"
            );
            return None;
        }
    };

    let ares_json_handle = match serve_ares_json_services(&session, state).await {
        Ok(handle) => handle,
        Err(err) => {
            warn!(
                error = %err,
                "ROS2DDS ARES JSON services disabled: failed to declare queryables"
            );
            return None;
        }
    };

    info!(
        services = ares_json_handle.task_count(),
        ros_type = "ares_interfaces/srv/Json",
        "ROS2DDS ARES JSON services ready"
    );
    println!(
        "ROS2DDS ARES JSON services ready: {} services using ares_interfaces/srv/Json",
        ares_json_handle.task_count()
    );
    let listen =
        std::env::var("SYNCBOT_ZENOH_LISTEN").unwrap_or_else(|_| DEFAULT_ZENOH_LISTEN.to_string());
    println!("Zenoh listening for bridge/peer connections on {listen}");
    println!(
        "ROS2 test: ros2 service call /ares/v1/health ares_interfaces/srv/Json \"{{request: '{{}}'}}\""
    );

    Some((session, ares_json_handle))
}

#[cfg(feature = "robo")]
fn zenoh_config_from_env() -> Result<zenoh::Config, String> {
    let mut config = zenoh::Config::default();

    if let Ok(raw) = std::env::var("SYNCBOT_ZENOH_CONNECT") {
        let endpoints = split_env_list(&raw);
        if !endpoints.is_empty() {
            config
                .insert_json5("connect/endpoints", &json_array(&endpoints))
                .map_err(|err| format!("SYNCBOT_ZENOH_CONNECT: {err}"))?;
        }
    }

    let listen_raw =
        std::env::var("SYNCBOT_ZENOH_LISTEN").unwrap_or_else(|_| DEFAULT_ZENOH_LISTEN.to_string());
    let listen_endpoints = split_env_list(&listen_raw);
    if !listen_endpoints.is_empty() {
        config
            .insert_json5("listen/endpoints", &json_array(&listen_endpoints))
            .map_err(|err| format!("SYNCBOT_ZENOH_LISTEN: {err}"))?;
    }

    Ok(config)
}

#[cfg(feature = "robo")]
fn split_env_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(feature = "robo")]
fn json_array(values: &[String]) -> String {
    let quoted = values
        .iter()
        .map(|value| format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect::<Vec<_>>();
    format!("[{}]", quoted.join(","))
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

fn print_zones(idx: &WorkspaceIndex) {
    let Some(root) = idx.root_zone_id() else {
        println!("(workspace has no root zone)");
        return;
    };
    let mut zones = vec![root];
    zones.extend(idx.descendant_zones(root).iter().map(|z| z.id()));

    println!("zones ({}):", zones.len());
    for zid in zones {
        let Some(z) = idx.zone(zid) else { continue };
        let policy = z
            .property("traffic.policy")
            .map(String::as_str)
            .unwrap_or("-");
        match z.property(NUMERIC_ID_PROPERTY) {
            Some(n) => println!(
                "  {:<20} numeric_id={:<6} policy={:<10} uuid={}",
                z.name(),
                n,
                policy,
                zid
            ),
            None => println!(
                "  {:<20} (no numeric id)  policy={:<10} uuid={}",
                z.name(),
                policy,
                zid
            ),
        }
    }
}
