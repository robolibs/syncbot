//! Serve the timenav REST API over a workspace loaded from disk.
//!
//! Unlike `rest_server.rs` (which builds a fixed in-memory workspace), this
//! takes a zoneout workspace **directory** as input and serves whatever zones
//! it contains. If no directory is given, it loads `examples/fixed`. Zones
//! carrying an `external.numeric_id` property are reachable by that integer as
//! well as by UUID.
//!
//! ```sh
//! cargo run --example serve_workspace --features rest -- [workspace_dir] [bind_addr]
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

use timenav::wire::{ServeState, rest};
use timenav::{Coordinator, NUMERIC_ID_PROPERTY, ValidationSeverity, WorkspaceIndex};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use zoneout::Workspace;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    init_logging();

    let mut args = std::env::args().skip(1);
    let dir = args.next().unwrap_or_else(|| "examples/fixed".to_string());
    let addr = args.next().unwrap_or_else(|| "127.0.0.1:8080".to_string());

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
    let app = rest::router(state);

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            error!(addr = %addr, error = %e, "failed to bind REST listener");
            return ExitCode::FAILURE;
        }
    };

    println!("\ntimenav serving workspace '{dir}' on http://{addr}");
    println!("try: curl http://{addr}/ares/v1/health");
    info!(addr = %addr, workspace = %dir, "REST server listening");

    if let Err(e) = axum::serve(listener, app).await {
        error!(error = %e, "REST server error");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
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
