//! Refusals that keep one request from taking the fleet down (PLAN 2.1).

#![cfg(feature = "rest")]

use std::collections::BTreeMap as OMap;
use std::sync::Arc;

use datapod::{Geo, Point, Polygon};
use syncbot::wire::{
    MAX_CLAIM_TARGETS, MAX_ZONE_DEPTH, ServeState, flat_claim, flat_register, set_workspace,
};
use syncbot::{ClaimTargetKind, Coordinator, NUMERIC_ID_PROPERTY, WorkspaceIndex};
use zoneout::{Workspace, ZoneBuilder};

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

fn state_with_zones(count: u64) -> ServeState {
    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rect(0.0, 0.0, 10_000.0, 1_000.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root");
    for i in 0..count {
        let x0 = 10.0 + i as f64 * 20.0;
        root.add_child(
            ZoneBuilder::new()
                .with_name(format!("z{i}"))
                .with_kind("zone")
                .with_boundary(rect(x0, 10.0, x0 + 15.0, 500.0))
                .with_datum(Geo::new(52.0, 5.0, 0.0))
                .with_property(NUMERIC_ID_PROPERTY, i.to_string())
                .build()
                .expect("zone"),
        )
        .expect("add zone");
    }
    let index = Arc::new(WorkspaceIndex::new(Arc::new(Workspace::new(root))));
    ServeState::new(Coordinator::with_index(index))
}

/// A serialized workspace nested `depth` zones deep, built through the real
/// API and round-tripped through zoneout's own wire type so it is exactly what
/// a genuine push would carry.
fn nested_document(depth: usize) -> Vec<u8> {
    fn nest(level: usize, depth: usize, inset: f64) -> zoneout::Zone {
        let span = 1000.0 - inset * 2.0;
        let mut zone = ZoneBuilder::new()
            .with_name(format!("z{level}"))
            .with_kind(if level == 0 { "workspace" } else { "zone" })
            .with_boundary(rect(inset, inset, inset + span, inset + span))
            .with_datum(Geo::new(52.0, 5.0, 0.0))
            .build()
            .expect("zone");
        if level + 1 < depth {
            zone.add_child(nest(level + 1, depth, inset + 1.0))
                .expect("nest");
        }
        zone
    }
    let workspace = Workspace::new(nest(0, depth, 0.0));
    serde_json::to_vec(&workspace.to_wire()).expect("serialize")
}

// -- 2.1.1 workspace push authorisation ------------------------------------

/// With no operator key configured the endpoint is closed, not open. A robot
/// key must never grant map control, and neither must its absence.
#[test]
fn workspace_push_is_refused_when_no_operator_key_is_configured() {
    let state = state_with_zones(2);
    let err = set_workspace(&state, b"{}", None).expect_err("must refuse");
    assert!(
        err.message.contains("disabled"),
        "expected a 'disabled' explanation, got: {}",
        err.message
    );
    assert!(!state.workspace_push_enabled());
}

#[test]
fn workspace_push_requires_the_operator_key() {
    let state = state_with_zones(2)
        .with_admin_key(Some("4242"))
        .expect("admin key");
    assert!(state.workspace_push_enabled());

    let missing = set_workspace(&state, b"{}", None).expect_err("no key");
    assert!(missing.message.contains("requires the operator key"));

    let wrong = set_workspace(&state, b"{}", Some("9999")).expect_err("wrong key");
    assert!(wrong.message.contains("does not match"));

    // The right key gets past authorisation and fails on the document instead.
    let right = set_workspace(&state, b"{}", Some("4242")).expect_err("bad document");
    assert!(
        right.message.contains("not valid zoneout JSON") || right.message.contains("not loadable"),
        "should have reached parsing, got: {}",
        right.message
    );
}

/// A robot's key is not an operator key.
#[test]
fn a_robot_key_does_not_authorise_a_workspace_push() {
    let state = state_with_zones(2)
        .with_admin_key(Some("4242"))
        .expect("admin key");
    flat_register(&state, "7", "1234", None);

    let err = set_workspace(&state, b"{}", Some("1234")).expect_err("robot key must not work");
    assert!(err.message.contains("does not match"));
}

// -- 2.1.2 nesting depth ---------------------------------------------------

/// Deep nesting used to recurse through `index_zone_tree`, overflowing the
/// stack — an abort, not an error. It is refused at the door now.
#[test]
fn an_over_nested_workspace_is_refused_not_fatal() {
    let state = state_with_zones(1)
        .with_admin_key(Some("4242"))
        .expect("admin key");

    // A workspace at the limit is fine...
    let ok = set_workspace(&state, &nested_document(MAX_ZONE_DEPTH), Some("4242"));
    assert!(ok.is_ok(), "at the limit should load: {ok:?}");

    // ...one past it is refused, and the process is still here to say so.
    let err = set_workspace(&state, &nested_document(MAX_ZONE_DEPTH + 1), Some("4242"))
        .expect_err("too deep");
    assert!(
        err.message.contains("deep"),
        "should name the depth limit, got: {}",
        err.message
    );
}

/// The index reports depth without recursing, so it can be asked before
/// anything else walks the tree.
#[test]
fn zone_depth_is_measured_iteratively() {
    let state = state_with_zones(3);
    let coord = state.coordinator();
    let guard = coord.read().unwrap();
    let index = guard.index().expect("index");
    assert_eq!(index.max_zone_depth(), 2, "root plus one level of children");
}

// -- 2.1.3 claim target cap ------------------------------------------------

/// Claim evaluation is quadratic in the target count and runs under the
/// coordinator write lock, so an unbounded list is a denial of service.
#[test]
fn an_oversized_claim_is_refused() {
    let state = state_with_zones(4);
    flat_register(&state, "7", "1234", None);

    let too_many: Vec<u64> = (0..MAX_CLAIM_TARGETS as u64 + 1).collect();
    let reply = flat_claim(
        &state,
        ClaimTargetKind::Zone,
        "1234",
        "7",
        &too_many,
        None,
        None,
    );
    assert_eq!(
        (reply.decision, reply.reason),
        (0, 5),
        "past the cap must be a bad request"
    );
}

/// ...and a claim right at the cap is still served.
#[test]
fn a_claim_at_the_cap_is_accepted() {
    let state = state_with_zones(MAX_CLAIM_TARGETS as u64);
    flat_register(&state, "7", "1234", None);

    let at_cap: Vec<u64> = (0..MAX_CLAIM_TARGETS as u64).collect();
    let reply = flat_claim(
        &state,
        ClaimTargetKind::Zone,
        "1234",
        "7",
        &at_cap,
        None,
        None,
    );
    assert_eq!((reply.decision, reply.reason), (1, 0));
}

// -- unused import guard ---------------------------------------------------

#[test]
fn fixture_builds() {
    let _ = OMap::<String, String>::new();
    assert!(state_with_zones(1).coordinator().read().is_ok());
}

// -- 2.2 identity ----------------------------------------------------------

/// Registration was first-come: an attacker could take an id before the real
/// robot booted and hold it forever with a key only they knew. Provisioning
/// closes that.
#[test]
fn an_unprovisioned_robot_cannot_register() {
    use syncbot::RobotId;

    let state = state_with_zones(2);
    {
        let coord = state.coordinator();
        let mut guard = coord.write().unwrap();
        guard.set_provisioned_robots(Some([RobotId::new(7)].into_iter().collect()));
    }

    let allowed = flat_register(&state, "7", "1234", None);
    assert_eq!((allowed.decision, allowed.reason), (1, 0));

    let squatter = flat_register(&state, "8", "9999", None);
    assert_eq!(
        (squatter.decision, squatter.reason),
        (0, 5),
        "an id the operator never provisioned must be refused"
    );
}

/// The real robot must always be able to claim its own id, whatever order
/// clients arrive in — which is the whole point of provisioning.
#[test]
fn provisioning_survives_a_squatter_arriving_first() {
    use syncbot::RobotId;

    let state = state_with_zones(2);
    {
        let coord = state.coordinator();
        let mut guard = coord.write().unwrap();
        guard.set_provisioned_robots(Some([RobotId::new(7)].into_iter().collect()));
    }

    // The squatter tries first, with several ids.
    for id in ["8", "9", "10"] {
        assert_eq!(flat_register(&state, id, "9999", None).decision, 0);
    }
    // Robot 7 still gets its id.
    assert_eq!(flat_register(&state, "7", "1234", None).decision, 1);
}

/// Registration never expires by design, so it is bounded instead.
#[test]
fn the_fleet_has_a_registration_ceiling() {
    use syncbot::coordinator::MAX_REGISTERED_ROBOTS;

    let state = state_with_zones(1);
    for id in 0..MAX_REGISTERED_ROBOTS {
        let reply = flat_register(&state, &id.to_string(), "1234", None);
        assert_eq!(reply.decision, 1, "robot {id} should have registered");
    }
    let overflow = flat_register(&state, "999999", "1234", None);
    assert_eq!(
        (overflow.decision, overflow.reason),
        (0, 6),
        "past the ceiling must report a full fleet, not a generic refusal"
    );
}

/// The fuzz targets in `fuzz/` drive these two entry points. Exercise them in
/// the normal suite too, so the paths stay reachable and obviously total even
/// for someone who never runs cargo-fuzz.
#[test]
fn untrusted_bytes_are_rejected_not_fatal() {
    // Canonical decode: garbage, truncated headers, and a valid header with a
    // section length that runs off the end.
    for bytes in [vec![], vec![0u8; 1], vec![0xffu8; 16], vec![0u8; 32], {
        let mut wire = vec![0u8; 32];
        wire[24..28].copy_from_slice(&0u32.to_le_bytes());
        wire[28..32].copy_from_slice(&u32::MAX.to_le_bytes());
        wire
    }] {
        // Any answer is fine; not returning one is not.
        let _ = syncbot::wire::fuzz_decode_canonical(&bytes);
    }

    // Workspace documents: not JSON, JSON of the wrong shape, and an empty one.
    let state = state_with_zones(1)
        .with_admin_key(Some("4242"))
        .expect("admin key");
    for document in [
        b"".as_slice(),
        b"not json",
        b"[]",
        b"{}",
        br#"{"format_version":2}"#,
        br#"{"format_version":2,"zones":{},"nodes":[],"edges":[]}"#,
    ] {
        let _ = set_workspace(&state, document, Some("4242"));
    }
}

/// Claims survive a workspace swap on purpose — the usual push is an edit,
/// where dropping the fleet's claims would be the greater harm. But a claim
/// whose resource the new workspace does not have silently stops meaning
/// anything, so the push reports how many it orphaned.
#[test]
fn a_push_reports_the_claims_it_orphaned() {
    let state = state_with_zones(3)
        .with_admin_key(Some("4242"))
        .expect("admin key");
    flat_register(&state, "7", "1234", None);
    for zone in 0..2u64 {
        assert_eq!(
            flat_claim(
                &state,
                ClaimTargetKind::Zone,
                "1234",
                "7",
                &[zone],
                None,
                None
            )
            .decision,
            1
        );
    }

    // A workspace with the same shape but freshly minted uuids: every held
    // claim names a resource it does not contain.
    let replacement = {
        let mut root = ZoneBuilder::new()
            .with_name("replacement")
            .with_kind("workspace")
            .with_boundary(rect(0.0, 0.0, 10_000.0, 1_000.0))
            .with_datum(Geo::new(52.0, 5.0, 0.0))
            .build()
            .expect("root");
        for i in 0..3u64 {
            let x0 = 10.0 + i as f64 * 20.0;
            root.add_child(
                ZoneBuilder::new()
                    .with_name(format!("z{i}"))
                    .with_kind("zone")
                    .with_boundary(rect(x0, 10.0, x0 + 15.0, 500.0))
                    .with_datum(Geo::new(52.0, 5.0, 0.0))
                    .with_property(NUMERIC_ID_PROPERTY, i.to_string())
                    .build()
                    .expect("zone"),
            )
            .expect("add");
        }
        serde_json::to_vec(&Workspace::new(root).to_wire()).expect("serialize")
    };

    let accepted = set_workspace(&state, &replacement, Some("4242")).expect("push");
    assert_eq!(
        accepted.stale_claims, 2,
        "both held claims name uuids the new workspace does not have"
    );
    assert_eq!(accepted.zones, 4, "root plus three zones");
}
