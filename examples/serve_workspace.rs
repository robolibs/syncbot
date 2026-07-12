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
                warn!(state_file = %path, error = %e, "failed to load state file; starting fresh");
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
    // peerbus owns an internal Tokio runtime. Construct it on a plain thread,
    // outside this example's async runtime, to avoid nested-runtime panics.
    let peerbus_state = state.clone();
    let peerbus_identity = std::env::var("SYNCBOT_PEERBUS_IDENTITY")
        .unwrap_or_else(|_| syncbot::wire::peerbus::CORE_IDENTITY.to_string());
    let started = std::thread::spawn(move || -> Result<_, String> {
        let core =
            syncbot::wire::peerbus::CoreService::with_identity(peerbus_state, &peerbus_identity)
                .map_err(|err| format!("failed to start canonical peerbus core: {err}"))?;
        let client = syncbot::wire::peerbus::Client::connect(peerbus_identity)
            .map_err(|err| format!("failed to start peerbus adapter client: {err}"))?;
        Ok((core, client))
    })
    .join();
    let (_core, peerbus) = match started {
        Ok(Ok(started)) => started,
        Ok(Err(err)) => {
            error!(error = %err, "failed to start peerbus services");
            return ExitCode::FAILURE;
        }
        Err(_) => {
            error!("peerbus startup thread panicked");
            return ExitCode::FAILURE;
        }
    };
    #[cfg(feature = "robo")]
    let _ros2dds = start_ros2dds(peerbus.clone()).await;
    // Auto-release the claims of robots that stop heartbeating (per their
    // <alive> interval). Checks once a second.
    let _sweeper =
        syncbot::wire::spawn_inactive_sweeper(state.clone(), std::time::Duration::from_secs(1));
    // Periodic state flush (only when persistence is enabled). Mirrors the
    // sweeper: read the coordinator, snapshot, atomically write, every 2s.
    let _flusher = persist_path.clone().map(|path| {
        let coord_handle = state.coordinator();
        let path_buf = std::path::PathBuf::from(path);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                ticker.tick().await;
                if let Err(e) = syncbot::persist::flush(&coord_handle, &path_buf) {
                    warn!(error = %e, "periodic state flush failed");
                }
            }
        })
    });
    let app = rest::router(peerbus);

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

    // Always shut down gracefully so peerbus can unlink its SHM segments.
    // Persistence additionally flushes the last snapshot after Ctrl-C.
    let coord_handle = state.coordinator();
    let serve_result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            if let Some(path) = persist_path {
                if let Err(e) = syncbot::persist::flush(&coord_handle, std::path::Path::new(&path))
                {
                    warn!(error = %e, "final state flush on shutdown failed");
                } else {
                    info!("flushed final syncbot state on shutdown");
                }
            }
        })
        .await;
    if let Err(e) = serve_result {
        error!(error = %e, "REST server error");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(feature = "robo")]
async fn start_ros2dds(
    peerbus: syncbot::wire::peerbus::Client,
) -> Option<(zenoh::Session, Ros2DdsAresJsonHandle)> {
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

    let ares_json_handle = match serve_ares_json_services(&session, peerbus).await {
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
