//! Cross-transport differential (PLAN 2.3.3).
//!
//! The architecture's central claim is that no wire format is privileged: JSON,
//! XML, Zenoh and ROS2/DDS are all adapters onto one canonical datapod service.
//! Nothing enforced that. Each transport had its own acceptance script, but
//! none asserted the transports *agree* — which is how three of four adapters
//! came to report `datum: null` on `/health` while JSON reported the real one.
//!
//! This runs one identical sequence through each transport against its own
//! fresh core, then compares the answers step for step. Adding an adapter
//! means adding it to `DRIVERS`.
//!
//! Zenoh and ROS2/DDS are exercised live by `tests/usecase/{ros2,mixed}.sh`
//! instead: they need a Zenoh session and a running `zenoh-bridge-ros2dds`,
//! which do not belong in a unit test. They reach the core through the same
//! `peerbus::Client` methods the native driver below calls directly, so the
//! encoding boundary — where the divergences live — is what is covered here.

#![cfg(all(feature = "peerbus", feature = "rest", feature = "xmlt"))]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, header};
use datapod::{Geo, Point, Polygon};
use graphix::vertex::EdgeType;
use syncbot::wire::peerbus::{Client, CoreService};
use syncbot::wire::{FlatReply, ServeState};
use syncbot::{ClaimTargetKind, Coordinator, NUMERIC_ID_PROPERTY, WorkspaceIndex};
use tower::ServiceExt;
use zoneout::{Workspace, ZoneBuilder};

// ---------------------------------------------------------------------------
// fixture
// ---------------------------------------------------------------------------

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

/// Two exclusive zones (42, 43) and a two-node path (1001-1002 over edge 2001).
fn state() -> ServeState {
    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rect(0.0, 0.0, 1000.0, 1000.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root");
    for numeric in [42u64, 43] {
        let x0 = 10.0 + numeric as f64;
        root.add_child(
            ZoneBuilder::new()
                .with_name(format!("z{numeric}"))
                .with_kind("zone")
                .with_boundary(rect(x0, 10.0, x0 + 20.0, 500.0))
                .with_datum(Geo::new(52.0, 5.0, 0.0))
                .with_property(NUMERIC_ID_PROPERTY, numeric.to_string())
                .with_property("traffic.policy", "exclusive")
                .build()
                .expect("zone"),
        )
        .expect("add zone");
    }

    let mut ws = Workspace::new(root);
    let mut a_props = BTreeMap::new();
    a_props.insert(NUMERIC_ID_PROPERTY.to_string(), "1001".to_string());
    let mut b_props = BTreeMap::new();
    b_props.insert(NUMERIC_ID_PROPERTY.to_string(), "1002".to_string());
    let a = ws.add_node(Point::new(500.0, 500.0, 0.0), a_props);
    let b = ws.add_node(Point::new(600.0, 500.0, 0.0), b_props);
    let mut edge_props = BTreeMap::new();
    edge_props.insert(NUMERIC_ID_PROPERTY.to_string(), "2001".to_string());
    ws.add_edge(a, b, 1.0, EdgeType::Undirected, edge_props);

    let index = Arc::new(WorkspaceIndex::new(Arc::new(ws)));
    ServeState::new(Coordinator::with_index(index))
        .with_kdf_params(syncbot::core::key::insecure_test_cost())
}

fn identity() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "ares-parity-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

// ---------------------------------------------------------------------------
// the sequence
// ---------------------------------------------------------------------------

/// One request, described independently of any encoding.
#[derive(Debug, Clone)]
enum Step {
    Register {
        robot: &'static str,
        key: &'static str,
        alive: Option<u64>,
    },
    Heartbeat {
        robot: &'static str,
        key: &'static str,
        zone: i64,
    },
    ClaimZone {
        robot: &'static str,
        key: &'static str,
        ids: &'static [u64],
    },
    ClaimRoute {
        robot: &'static str,
        key: &'static str,
        nodes: &'static [u64],
        edges: &'static [u64],
    },
    Release {
        robot: &'static str,
        key: &'static str,
        id: u64,
    },
}

/// Exercises every reason code the flat protocol can answer with.
fn sequence() -> Vec<Step> {
    vec![
        Step::Register {
            robot: "7",
            key: "1234",
            alive: Some(9999),
        },
        // ...again: already registered.
        Step::Register {
            robot: "7",
            key: "1234",
            alive: None,
        },
        Step::Register {
            robot: "8",
            key: "5678",
            alive: Some(9999),
        },
        // Bad id.
        Step::Register {
            robot: "nope",
            key: "1",
            alive: None,
        },
        Step::Heartbeat {
            robot: "7",
            key: "1234",
            zone: -1,
        },
        // Wrong key.
        Step::Heartbeat {
            robot: "7",
            key: "9999",
            zone: -1,
        },
        // Not registered.
        Step::Heartbeat {
            robot: "999",
            key: "1",
            zone: 3,
        },
        Step::ClaimZone {
            robot: "7",
            key: "1234",
            ids: &[42],
        },
        // Conflict, and `blocked` must name the same id everywhere.
        Step::ClaimZone {
            robot: "8",
            key: "5678",
            ids: &[42],
        },
        // Atomic multi-claim, denied by the held one.
        Step::ClaimZone {
            robot: "8",
            key: "5678",
            ids: &[43, 42],
        },
        Step::ClaimZone {
            robot: "8",
            key: "5678",
            ids: &[43],
        },
        // Unknown resource.
        Step::ClaimZone {
            robot: "7",
            key: "1234",
            ids: &[9999],
        },
        // Wrong key on a claim.
        Step::ClaimZone {
            robot: "7",
            key: "0000",
            ids: &[43],
        },
        Step::ClaimRoute {
            robot: "7",
            key: "1234",
            nodes: &[1001, 1002],
            edges: &[2001],
        },
        // Overlapping route.
        Step::ClaimRoute {
            robot: "8",
            key: "5678",
            nodes: &[1002],
            edges: &[],
        },
        // Re-acquisition by the holder.
        Step::ClaimRoute {
            robot: "7",
            key: "1234",
            nodes: &[1001, 1002],
            edges: &[2001],
        },
        // Unknown id inside a route.
        Step::ClaimRoute {
            robot: "8",
            key: "5678",
            nodes: &[4242],
            edges: &[],
        },
        Step::Release {
            robot: "7",
            key: "1234",
            id: 42,
        },
        // Nothing held now.
        Step::Release {
            robot: "7",
            key: "1234",
            id: 42,
        },
    ]
}

/// What a transport answered, stripped of encoding.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    decision: u8,
    reason: u8,
    blocked: Option<u64>,
}

impl From<FlatReply> for Outcome {
    fn from(reply: FlatReply) -> Self {
        Self {
            decision: reply.decision,
            reason: reply.reason,
            blocked: reply.blocked,
        }
    }
}

// ---------------------------------------------------------------------------
// drivers
// ---------------------------------------------------------------------------

/// Speak the canonical service directly — the shape every adapter compiles to.
fn drive_native(client: &Client, step: &Step) -> Outcome {
    let reply = match step {
        Step::Register { robot, key, alive } => client.register(robot, key, *alive),
        Step::Heartbeat { robot, key, zone } => {
            client.heartbeat(robot, key, Some(*zone), None, None, None, None)
        }
        Step::ClaimZone { robot, key, ids } => {
            client.claim(ClaimTargetKind::Zone, key, robot, ids, None, None)
        }
        Step::ClaimRoute {
            robot,
            key,
            nodes,
            edges,
        } => client.claim_route(key, robot, nodes, edges, None, None),
        Step::Release { robot, key, id } => client.release(ClaimTargetKind::Zone, key, robot, *id),
    };
    reply.expect("native call").into()
}

fn json_body(step: &Step) -> (String, String) {
    let list = |ids: &[u64]| ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
    match step {
        Step::Register { robot, key, alive } => (
            "/ares/v1/robots".into(),
            match alive {
                Some(a) => format!(r#"{{"robot":"{robot}","key":"{key}","alive":{a}}}"#),
                None => format!(r#"{{"robot":"{robot}","key":"{key}"}}"#),
            },
        ),
        Step::Heartbeat { robot, key, zone } => (
            format!("/ares/v1/robots/{robot}/heartbeat"),
            format!(r#"{{"key":"{key}","zone":{zone}}}"#),
        ),
        Step::ClaimZone { robot, key, ids } => (
            "/ares/v1/claims/zone".into(),
            format!(
                r#"{{"key":"{key}","robot":"{robot}","id":[{}]}}"#,
                list(ids)
            ),
        ),
        Step::ClaimRoute {
            robot,
            key,
            nodes,
            edges,
        } => (
            "/ares/v1/claims/route".into(),
            format!(
                r#"{{"key":"{key}","robot":"{robot}","node":[{}],"edge":[{}]}}"#,
                list(nodes),
                list(edges)
            ),
        ),
        Step::Release { robot, key, id } => (
            "/ares/v1/leases/release/zone".into(),
            format!(r#"{{"key":"{key}","robot":"{robot}","id":{id}}}"#),
        ),
    }
}

fn xml_body(step: &Step) -> (String, String) {
    let repeat = |tag: &str, ids: &[u64]| {
        ids.iter()
            .map(|id| format!("<{tag}>{id}</{tag}>"))
            .collect::<Vec<_>>()
            .concat()
    };
    match step {
        Step::Register { robot, key, alive } => (
            "/ares/v1/robots".into(),
            match alive {
                Some(a) => {
                    format!("<m><robot>{robot}</robot><key>{key}</key><alive>{a}</alive></m>")
                }
                None => format!("<m><robot>{robot}</robot><key>{key}</key></m>"),
            },
        ),
        Step::Heartbeat { robot, key, zone } => (
            format!("/ares/v1/robots/{robot}/heartbeat"),
            format!("<m><key>{key}</key><zone>{zone}</zone></m>"),
        ),
        Step::ClaimZone { robot, key, ids } => (
            "/ares/v1/claims/zone".into(),
            format!(
                "<m><key>{key}</key><robot>{robot}</robot>{}</m>",
                repeat("id", ids)
            ),
        ),
        Step::ClaimRoute {
            robot,
            key,
            nodes,
            edges,
        } => (
            "/ares/v1/claims/route".into(),
            format!(
                "<m><key>{key}</key><robot>{robot}</robot>{}{}</m>",
                repeat("node", nodes),
                repeat("edge", edges)
            ),
        ),
        Step::Release { robot, key, id } => (
            "/ares/v1/leases/release/zone".into(),
            format!("<m><key>{key}</key><robot>{robot}</robot><id>{id}</id></m>"),
        ),
    }
}

async fn drive_http(app: &Router, step: &Step, xml: bool) -> Outcome {
    let (path, body) = if xml { xml_body(step) } else { json_body(step) };
    let content = if xml {
        "application/xml"
    } else {
        "application/json"
    };
    let response = app
        .clone()
        .oneshot(
            Request::post(path)
                .header(header::CONTENT_TYPE, content)
                .header(header::ACCEPT, content)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .expect("http response");
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = std::str::from_utf8(&bytes).expect("utf8");
    let reply: FlatReply = if xml {
        quick_xml::de::from_str(text).unwrap_or_else(|e| panic!("XML reply {text:?}: {e}"))
    } else {
        serde_json::from_str(text).unwrap_or_else(|e| panic!("JSON reply {text:?}: {e}"))
    };
    reply.into()
}

/// Run the whole sequence against a fresh core, through one transport.
fn run(transport: &str) -> Vec<Outcome> {
    let identity = identity();
    let _core = CoreService::with_identity(state(), &identity).expect("core");
    let client = Client::connect(identity).expect("client");
    let steps = sequence();

    match transport {
        "native" => steps.iter().map(|s| drive_native(&client, s)).collect(),
        "json" | "xml" => {
            let app = if transport == "json" {
                syncbot::wire::rest::router(client)
            } else {
                syncbot::wire::xmlt::router(client)
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            let out = runtime.block_on(async {
                let mut out = Vec::new();
                for step in &steps {
                    out.push(drive_http(&app, step, transport == "xml").await);
                }
                out
            });
            drop(runtime);
            out
        }
        other => panic!("unknown transport {other}"),
    }
}

/// Every transport must answer identically, step for step. A divergence here
/// means one adapter is privileged over another — which the design says none
/// is.
#[test]
fn every_transport_answers_identically() {
    const DRIVERS: [&str; 3] = ["native", "json", "xml"];

    let steps = sequence();
    let baseline = run(DRIVERS[0]);
    assert_eq!(baseline.len(), steps.len());

    for transport in &DRIVERS[1..] {
        let actual = run(transport);
        for (index, (expected, got)) in baseline.iter().zip(&actual).enumerate() {
            assert_eq!(
                got, expected,
                "step {index} ({:?}) diverged: {} answered {:?}, {} answered {:?}",
                steps[index], transport, got, DRIVERS[0], expected
            );
        }
    }
}

/// The differential is only worth running if the sequence actually reaches
/// interesting answers — a run of all-grants would compare equal everywhere
/// and prove nothing.
///
/// Note reason codes are *endpoint-scoped* past `0` (ok) and `1` (mismatched
/// key): `3` means "bad id" on register and "capacity" on a claim. What
/// matters here is the spread, and that `blocked` is carried on the paths that
/// produce it — it is the field most likely to be dropped by an encoding.
#[test]
fn the_sequence_exercises_a_real_spread_of_answers() {
    let steps = sequence();
    let outcomes = run("native");

    let granted = outcomes.iter().filter(|o| o.decision == 1).count();
    let denied = outcomes.len() - granted;
    assert!(
        granted >= 5,
        "only {granted} grants; the sequence is too hostile"
    );
    assert!(
        denied >= 8,
        "only {denied} denials; the sequence is too easy"
    );

    let reasons: std::collections::BTreeSet<u8> = outcomes
        .iter()
        .filter(|o| o.decision == 0)
        .map(|o| o.reason)
        .collect();
    for expected in [1u8, 2, 3, 4] {
        assert!(
            reasons.contains(&expected),
            "sequence never provokes reason {expected}; saw {reasons:?}"
        );
    }

    let blocked: Vec<u64> = outcomes.iter().filter_map(|o| o.blocked).collect();
    assert!(
        blocked.len() >= 3,
        "only {} answers carry a `blocked` id ({blocked:?}); that field is the \
         one most likely to be lost in an encoding, so the differential needs \
         several",
        blocked.len()
    );

    // Milestone 1.1: a holder re-asserting its own route is granted, and every
    // transport must agree on that.
    let is_robot_7_route = |step: &Step| {
        matches!(
            step,
            Step::ClaimRoute {
                robot: "7",
                nodes: [1001, 1002],
                ..
            }
        )
    };
    let claims: Vec<usize> = steps
        .iter()
        .enumerate()
        .filter(|(_, step)| is_robot_7_route(step))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        claims.len(),
        2,
        "the sequence should claim the same route twice"
    );
    assert_eq!(
        outcomes[claims[1]].decision, 1,
        "re-acquiring your own route must be granted"
    );
}
