//! Serve the syncbot REST API over a workspace.
//!
//! This
//! takes a zoneout workspace **directory** as input and serves whatever zones
//! it contains. If no directory is given, it loads `examples/fixed`. Zones
//! carrying an `external.numeric_id` property are reachable by that integer as
//! well as by UUID.
//!
//! ```sh
//! cargo run --features rest --bin syncbot -- [workspace_dir] [bind_addr]
//! ```
//!
//! With `--no-map` it starts with nothing to serve and waits for a workspace to
//! be pushed to `POST /ares/v1/workspace`, so an editor can be the source of
//! truth instead of a directory both sides have to agree on:
//!
//! ```sh
//! cargo run --example serve_workspace --features rest -- --no-map [bind_addr]
//! curl -X POST http://127.0.0.1:8080/ares/v1/workspace --data-binary @workspace.json
//! ```
//!
//! A pushed workspace can be replaced at any time; robots keep the claims they
//! hold, since claims are keyed by resource uuid.
//!
//! Optional state persistence: set `SYNCBOT_STATE` to a file path and the
//! server restores from it on boot, flushes every 2s, and flushes again on
//! shutdown. Unset, the server is fully in-memory and no file is touched.
//! Keys are stored as Argon2id hashes, never in the clear, but the file is
//! still created `0600` and deserves care.
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

use syncbot::wire::{ServeState, rest};
use syncbot::{Coordinator, NUMERIC_ID_PROPERTY, ValidationSeverity, WorkspaceIndex};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use zoneout::Workspace;

/// Wait for Ctrl-C **or** SIGTERM.
///
/// Only Ctrl-C used to be handled, so every other way a server dies — `kill`,
/// `systemctl stop`, a container stop — skipped the graceful path entirely and
/// dropped the final state flush on the floor.
///
/// It does *not* guarantee peerbus' shared memory is released: only the handle
/// that created a segment unlinks it, so a core that attaches to one an earlier
/// core left behind leaves the name in place however cleanly it exits. Stale
/// segments are therefore expected, and are made harmless at the other end —
/// see `discard_requests_predating_this_core`.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(e) => {
                warn!(error = %e, "cannot listen for SIGTERM; Ctrl-C only");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => info!("interrupted; shutting down"),
            _ = terminate.recv() => info!("terminated; shutting down"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn new_coordinator(idx: Option<Arc<WorkspaceIndex>>) -> Coordinator {
    match idx {
        Some(idx) => Coordinator::with_index(idx),
        None => Coordinator::new(),
    }
}

/// Load a workspace directory into an index, refusing anything with
/// validation errors. `Err(())` means the failure has already been logged.
fn load_index(dir: &str) -> Result<Arc<WorkspaceIndex>, ()> {
    let ws = match Workspace::load(dir) {
        Ok(ws) => ws,
        Err(e) => {
            error!(workspace = %dir, error = %e, "failed to load workspace");
            return Err(());
        }
    };
    info!(workspace = %dir, "loaded workspace");

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
            return Err(());
        }
    }
    Ok(idx)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 1)]
async fn main() -> ExitCode {
    init_logging();

    let mut args = std::env::args().skip(1).peekable();
    // `--no-map` starts with nothing to serve and waits for a workspace to be
    // pushed to POST /ares/v1/workspace, which lets an editor be the source of
    // truth instead of a directory both sides have to agree on.
    let no_map = args.peek().is_some_and(|arg| arg == "--no-map");
    if no_map {
        args.next();
    }
    let dir = (!no_map).then(|| args.next().unwrap_or_else(|| "examples/fixed".to_string()));
    let addr = args.next().unwrap_or_else(|| "0.0.0.0:8080".to_string());

    // 1..3. Load the workspace from disk, unless we're waiting for a push.
    let idx = match &dir {
        Some(dir) => match load_index(dir) {
            Ok(idx) => Some(idx),
            Err(()) => return ExitCode::FAILURE,
        },
        None => None,
    };
    match &idx {
        Some(idx) => print_zones(idx),
        None => println!("\nno workspace loaded; waiting for one to be pushed"),
    }

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
                Coordinator::restore(snapshot, idx.clone())
            }
            Ok(None) => {
                info!(state_file = %path, "no existing state file; starting fresh");
                new_coordinator(idx.clone())
            }
            Err(e) => {
                warn!(state_file = %path, error = %e, "failed to load state file; starting fresh");
                new_coordinator(idx.clone())
            }
        },
        None => new_coordinator(idx.clone()),
    };
    // OPT-IN workspace replacement: only a client holding this operator key
    // may redraw the map. Unset, POST /ares/v1/workspace is refused outright
    // rather than left open — a robot key must never grant map control.
    let admin_key = std::env::var("SYNCBOT_ADMIN_KEY")
        .ok()
        .filter(|key| !key.is_empty());
    // OPT-IN keyless registration: every robot that omits a key shares one
    // password, so anyone can act as any of them. Off unless asked for.
    let allow_default_key = std::env::var("SYNCBOT_ALLOW_DEFAULT_KEY")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    if allow_default_key {
        warn!("keyless registration ENABLED: robots without a key share one password");
    }
    let state = match ServeState::new(coord)
        .with_default_key_allowed(allow_default_key)
        .with_admin_key(admin_key.as_deref())
    {
        Ok(state) => state,
        Err(_) => {
            error!("SYNCBOT_ADMIN_KEY is not a valid key (an integer, or did:pass=...)");
            return ExitCode::FAILURE;
        }
    };
    if admin_key.is_some() {
        info!("workspace replacement ENABLED for clients presenting the operator key");
    } else {
        info!("workspace replacement disabled; set SYNCBOT_ADMIN_KEY to enable it");
    }
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

    let serving = dir.clone().unwrap_or_else(|| "(awaiting push)".to_string());
    println!("\nsyncbot serving workspace '{serving}' on http://{addr}");
    println!("try: curl http://{addr}/ares/v1/health");
    if dir.is_none() {
        println!(
            "push one: curl -X POST http://{addr}/ares/v1/workspace --data-binary @workspace.json"
        );
    }
    info!(addr = %addr, workspace = %serving, "REST server listening");

    // Always shut down gracefully so peerbus can unlink its SHM segments.
    // Persistence additionally flushes the last snapshot on the way out.
    let coord_handle = state.coordinator();
    let serve_result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
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
