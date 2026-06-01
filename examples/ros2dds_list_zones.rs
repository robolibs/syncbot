//! One-service proof for ROS2 DDS clients through `zenoh-bridge-ros2dds`.
//!
//! No ROS2 libraries are used here. This is a native Zenoh queryable that
//! replies with a CDR-encoded `std_srvs/srv/Trigger` response.
//!
//! Default:
//!
//! ```sh
//! cargo run --features robo --example ros2dds_list_zones
//! ```
//!
//! Connect to a bridge/router:
//!
//! ```sh
//! cargo run --features robo --example ros2dds_list_zones -- --connect tcp/BRIDGE_HOST:7447
//! ```

use std::process::ExitCode;
use std::sync::Arc;

use timenav::wire::ServeState;
use timenav::wire::ros2dds::{LIST_ZONES_KEY, serve_list_zones_trigger};
use timenav::{Coordinator, ValidationSeverity, WorkspaceIndex};
use zoneout::Workspace;

#[derive(Debug)]
struct Args {
    workspace: String,
    key: String,
    connect: Vec<String>,
    listen: Vec<String>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            workspace: "examples/fixed".into(),
            key: LIST_ZONES_KEY.into(),
            connect: Vec::new(),
            listen: Vec::new(),
        }
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 1)]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            usage();
            return ExitCode::from(2);
        }
    };

    let ws = match Workspace::load(&args.workspace) {
        Ok(ws) => ws,
        Err(err) => {
            eprintln!("failed to load workspace '{}': {err}", args.workspace);
            return ExitCode::FAILURE;
        }
    };
    let idx = Arc::new(WorkspaceIndex::new(Arc::new(ws)));
    let issues = idx.validation_issues();
    let errors = issues
        .iter()
        .filter(|issue| issue.severity == ValidationSeverity::Error)
        .count();
    if errors > 0 {
        eprintln!("workspace has {errors} validation error(s); refusing to serve");
        for issue in issues {
            eprintln!(
                "  [{:?}] {}/{}: {}",
                issue.severity, issue.resource_kind, issue.category, issue.message
            );
        }
        return ExitCode::FAILURE;
    }

    let mut config = zenoh::Config::default();
    if !args.connect.is_empty() {
        if let Err(err) = config.insert_json5("connect/endpoints", &json_array(&args.connect)) {
            eprintln!("invalid --connect endpoint config: {err}");
            return ExitCode::from(2);
        }
    }
    if !args.listen.is_empty() {
        if let Err(err) = config.insert_json5("listen/endpoints", &json_array(&args.listen)) {
            eprintln!("invalid --listen endpoint config: {err}");
            return ExitCode::from(2);
        }
    }

    let session = match zenoh::open(config).await {
        Ok(session) => session,
        Err(err) => {
            eprintln!("failed to open Zenoh session: {err}");
            return ExitCode::FAILURE;
        }
    };

    let state = ServeState::new(Coordinator::with_index(idx));
    let _handle = match serve_list_zones_trigger(&session, state, args.key.clone()).await {
        Ok(handle) => handle,
        Err(err) => {
            eprintln!("failed to declare queryable '{}': {err}", args.key);
            return ExitCode::FAILURE;
        }
    };

    println!("ROS2DDS Trigger queryable ready");
    println!("  workspace: {}", args.workspace);
    println!("  zenoh key: {}", args.key);
    println!("  ROS2 call: ros2 service call /timenav/list_zones std_srvs/srv/Trigger");
    println!(
        "  bridge should log: Route Service Client (ROS:/timenav/list_zones <-> Zenoh:{})",
        args.key
    );

    std::future::pending::<()>().await;
    ExitCode::SUCCESS
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--workspace" | "-w" => {
                args.workspace = iter
                    .next()
                    .ok_or_else(|| format!("{arg} requires a value"))?;
            }
            "--key" | "-k" => {
                args.key = iter
                    .next()
                    .ok_or_else(|| format!("{arg} requires a value"))?;
            }
            "--connect" | "-e" => {
                args.connect.push(
                    iter.next()
                        .ok_or_else(|| format!("{arg} requires a value"))?,
                );
            }
            "--listen" | "-l" => {
                args.listen.push(
                    iter.next()
                        .ok_or_else(|| format!("{arg} requires a value"))?,
                );
            }
            "--help" | "-h" => {
                usage();
                std::process::exit(0);
            }
            other => {
                return Err(format!("unknown argument: {other}"));
            }
        }
    }
    Ok(args)
}

fn json_array(values: &[String]) -> String {
    let quoted = values
        .iter()
        .map(|value| format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect::<Vec<_>>();
    format!("[{}]", quoted.join(","))
}

fn usage() {
    eprintln!(
        "usage: ros2dds_list_zones [--workspace DIR] [--key KEY] [--connect ENDPOINT] [--listen ENDPOINT]"
    );
}
