//! Flat (tier-1) wire flow: register → heartbeat → claim → release, with the
//! mandatory key and `decision`/`reason` enum replies. Exercises the
//! transport-neutral `flat_*` functions directly.

#![cfg(feature = "rest")]

use std::collections::BTreeMap as OMap;
use std::sync::Arc;

use datapod::{Geo, Point, Polygon};
use syncbot::wire::{
    ClaimRequestWire, ClaimTargetWire, ServeState, flat_claim, flat_heartbeat, flat_register,
    flat_release, submit_claim,
};
use syncbot::{
    ClaimAccessMode, ClaimId, ClaimTargetKind, ClaimWindow, Coordinator, MissionId,
    NUMERIC_ID_PROPERTY, ResourceRef, RobotId, WorkspaceIndex,
};
use zoneout::{Workspace, ZoneBuilder};

fn rectangle(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Polygon {
    Polygon {
        vertices: vec![
            Point::new(min_x, min_y, 0.0),
            Point::new(max_x, min_y, 0.0),
            Point::new(max_x, max_y, 0.0),
            Point::new(min_x, max_y, 0.0),
        ],
    }
}

/// Root + two exclusive numeric-aliased zones (42, 43) and one node (139).
fn build_state() -> ServeState {
    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 100.0, 100.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root zone");
    for (name, num) in [("a", "42"), ("b", "43")] {
        let zone = ZoneBuilder::new()
            .with_name(name)
            .with_kind("zone")
            .with_boundary(rectangle(10.0, 10.0, 50.0, 50.0))
            .with_datum(Geo::new(52.0, 5.0, 0.0))
            .with_property(NUMERIC_ID_PROPERTY, num)
            .build()
            .expect("zone");
        root.add_child(zone).expect("add zone");
    }
    let mut ws = Workspace::new(root);
    let mut node_props = OMap::new();
    node_props.insert(NUMERIC_ID_PROPERTY.into(), "139".into());
    let _ = ws.add_node(Point::new(15.0, 15.0, 0.0), node_props);

    let idx = Arc::new(WorkspaceIndex::new(Arc::new(ws)));
    ServeState::new(Coordinator::with_index(idx))
}

#[test]
fn register_is_idempotent_deny_on_reuse() {
    let s = build_state();
    // first registration succeeds
    let r = flat_register(&s, "7", "1234");
    assert_eq!((r.decision, r.reason), (1, 0));
    // same id again -> deny "already registered" (reason 2)
    let r = flat_register(&s, "7", "1234");
    assert_eq!((r.decision, r.reason), (0, 2));
    // bad id (not int/uuid) -> reason 3
    let r = flat_register(&s, "notanid", "1234");
    assert_eq!((r.decision, r.reason), (0, 3));
    // unsupported key scheme -> reason 4
    let r = flat_register(&s, "8", "did:key=abc");
    assert_eq!((r.decision, r.reason), (0, 4));
}

#[test]
fn heartbeat_requires_registration_and_key() {
    let s = build_state();
    // not registered -> reason 2
    let r = flat_heartbeat(&s, "7", "1234", Some(42), None, None);
    assert_eq!((r.decision, r.reason), (0, 2));
    flat_register(&s, "7", "1234");
    // wrong key -> mismatched key (reason 1)
    let r = flat_heartbeat(&s, "7", "9999", Some(42), None, None);
    assert_eq!((r.decision, r.reason), (0, 1));
    // correct key -> ack ok
    let r = flat_heartbeat(&s, "7", "1234", Some(42), None, None);
    assert_eq!((r.decision, r.reason), (1, 0));
}

#[test]
fn claim_conflict_then_release() {
    let s = build_state();
    flat_register(&s, "7", "1234");
    flat_register(&s, "8", "5678");

    // robot 7 claims zone 42 (exclusive) -> grant
    let r = flat_claim(&s, ClaimTargetKind::Zone, "1234", "7", &[42], None, None);
    assert_eq!((r.decision, r.reason), (1, 0));

    // robot 8 claims zone 42 -> conflict (reason 2), blocked names 42
    let r = flat_claim(&s, ClaimTargetKind::Zone, "5678", "8", &[42], None, None);
    assert_eq!((r.decision, r.reason), (0, 2));
    assert_eq!(r.blocked, Some(42));

    // robot 8 with wrong key -> mismatched key
    let r = flat_claim(&s, ClaimTargetKind::Zone, "0000", "8", &[43], None, None);
    assert_eq!((r.decision, r.reason), (0, 1));

    // unknown resource -> reason 4, blocked names the id
    let r = flat_claim(&s, ClaimTargetKind::Zone, "1234", "7", &[999], None, None);
    assert_eq!((r.decision, r.reason), (0, 4));
    assert_eq!(r.blocked, Some(999));

    // robot 7 releases zone 42 -> ok; now robot 8 can claim it
    let r = flat_release(&s, ClaimTargetKind::Zone, "1234", "7", 42);
    assert_eq!((r.decision, r.reason), (1, 0));
    let r = flat_claim(&s, ClaimTargetKind::Zone, "5678", "8", &[42], None, None);
    assert_eq!((r.decision, r.reason), (1, 0));

    // releasing something not held -> reason 2 (no such lease)
    let r = flat_release(&s, ClaimTargetKind::Zone, "1234", "7", 43);
    assert_eq!((r.decision, r.reason), (0, 2));
}

/// Root + one child zone (numeric 50) that geometrically contains node 139.
fn build_nested_state() -> ServeState {
    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 100.0, 100.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root zone");
    let zone = ZoneBuilder::new()
        .with_name("c")
        .with_kind("zone")
        .with_boundary(rectangle(10.0, 10.0, 60.0, 60.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .with_property(NUMERIC_ID_PROPERTY, "50")
        .build()
        .expect("zone");
    root.add_child(zone).expect("add zone");

    let mut ws = Workspace::new(root);
    let mut node_props = OMap::new();
    node_props.insert(NUMERIC_ID_PROPERTY.into(), "139".into());
    ws.add_node(Point::new(20.0, 20.0, 0.0), node_props);

    let idx = Arc::new(WorkspaceIndex::new(Arc::new(ws)));
    ServeState::new(Coordinator::with_index(idx))
}

#[test]
fn claiming_zone_blocks_node_inside_it() {
    let s = build_nested_state();
    flat_register(&s, "7", "1234");
    flat_register(&s, "8", "5678");

    // robot 7 claims zone 50 exclusively -> grant
    assert_eq!(
        flat_claim(&s, ClaimTargetKind::Zone, "1234", "7", &[50], None, None).decision,
        1
    );

    // robot 8 claims node 139 (inside zone 50) -> DENIED by cross-level rule,
    // and the offending node id is reported
    let r = flat_claim(&s, ClaimTargetKind::Node, "5678", "8", &[139], None, None);
    assert_eq!((r.decision, r.reason), (0, 2));
    assert_eq!(r.blocked, Some(139));

    // release the zone -> the node becomes claimable
    assert_eq!(
        flat_release(&s, ClaimTargetKind::Zone, "1234", "7", 50).decision,
        1
    );
    assert_eq!(
        flat_claim(&s, ClaimTargetKind::Node, "5678", "8", &[139], None, None).decision,
        1
    );
}

#[test]
fn claiming_node_blocks_zone_around_it() {
    let s = build_nested_state();
    flat_register(&s, "7", "1234");
    flat_register(&s, "8", "5678");

    // robot 7 claims node 139 exclusively -> grant
    assert_eq!(
        flat_claim(&s, ClaimTargetKind::Node, "1234", "7", &[139], None, None).decision,
        1
    );

    // robot 8 claims the surrounding zone 50 -> DENIED (it contains node 139),
    // and the offending zone id is reported
    let r = flat_claim(&s, ClaimTargetKind::Zone, "5678", "8", &[50], None, None);
    assert_eq!((r.decision, r.reason), (0, 2));
    assert_eq!(r.blocked, Some(50));
}

#[test]
fn multi_zone_claim_is_atomic() {
    let s = build_state();
    flat_register(&s, "7", "1234");
    flat_register(&s, "8", "5678");

    // robot 8 grabs zone 43 first
    assert_eq!(
        flat_claim(&s, ClaimTargetKind::Zone, "5678", "8", &[43], None, None).decision,
        1
    );

    // robot 7 asks for 42 AND 43 atomically -> denied because 43 is taken;
    // 42 must NOT be granted (all-or-nothing)
    let r = flat_claim(
        &s,
        ClaimTargetKind::Zone,
        "1234",
        "7",
        &[42, 43],
        None,
        None,
    );
    assert_eq!((r.decision, r.reason), (0, 2));
    assert_eq!(r.blocked, Some(43));

    // so 42 is still free: robot 7 can take it alone
    assert_eq!(
        flat_claim(&s, ClaimTargetKind::Zone, "1234", "7", &[42], None, None).decision,
        1
    );
}

#[test]
fn tier2_submit_claim_requires_key() {
    let s = build_state();
    flat_register(&s, "7", "1234");

    let make = |key: Option<&str>| ClaimRequestWire {
        id: ClaimId::new(0),
        robot_id: RobotId::new(7),
        mission_id: MissionId::new(0),
        access_mode: ClaimAccessMode::Exclusive,
        priority: 0,
        requested_at_tick: None,
        window: ClaimWindow::default(),
        targets: vec![ClaimTargetWire {
            kind: ClaimTargetKind::Zone,
            resource_id: ResourceRef::Numeric(42),
        }],
        key: key.map(|k| k.to_string()),
    };

    // missing key -> rejected
    assert!(submit_claim(&s, make(None)).is_err());
    // wrong key -> rejected
    assert!(submit_claim(&s, make(Some("9999"))).is_err());
    // correct key -> accepted (grant)
    let eval = submit_claim(&s, make(Some("1234"))).expect("submit ok");
    assert_eq!(eval.decision, syncbot::ClaimDecision::Grant);
}

#[test]
fn uuid_robot_id_full_flow() {
    let s = build_state();
    let uuid = "11111111-1111-1111-1111-111111111111";

    // register with a UUID id
    assert_eq!(flat_register(&s, uuid, "1234").decision, 1);
    // re-registering the same UUID -> already registered (reason 2)
    let r = flat_register(&s, uuid, "1234");
    assert_eq!((r.decision, r.reason), (0, 2));

    // heartbeat by UUID, correct key -> ack
    assert_eq!(
        flat_heartbeat(&s, uuid, "1234", Some(42), None, None).decision,
        1
    );
    // wrong key -> mismatched key
    assert_eq!(
        flat_heartbeat(&s, uuid, "9999", Some(42), None, None).reason,
        1
    );

    // claim + release a zone as the UUID robot
    assert_eq!(
        flat_claim(&s, ClaimTargetKind::Zone, "1234", uuid, &[42], None, None).decision,
        1
    );
    assert_eq!(
        flat_release(&s, ClaimTargetKind::Zone, "1234", uuid, 42).decision,
        1
    );

    // an unregistered UUID heartbeats -> not registered (reason 2)
    let other = "22222222-2222-2222-2222-222222222222";
    assert_eq!(
        flat_heartbeat(&s, other, "1234", Some(42), None, None).reason,
        2
    );

    // a garbage id is rejected at registration (bad id, reason 3)
    assert_eq!(flat_register(&s, "not-an-id", "1234").reason, 3);
}

#[test]
fn key_is_optional_defaults_to_shared_password() {
    let s = build_state();
    // register with NO key -> uses the default password
    let r = flat_register(&s, "7", "0");
    assert_eq!((r.decision, r.reason), (1, 0));
    // a second robot registered the same way also works (each bound to default)
    assert_eq!(flat_register(&s, "8", "0").decision, 1);
    // claim with the default key succeeds for the default-registered robot
    assert_eq!(
        flat_claim(&s, ClaimTargetKind::Zone, "0", "7", &[42], None, None).decision,
        1
    );
    // a robot that registered WITH a real key is NOT satisfied by the default
    flat_register(&s, "9", "1234");
    let r = flat_claim(&s, ClaimTargetKind::Zone, "0", "9", &[43], None, None);
    assert_eq!((r.decision, r.reason), (0, 1)); // mismatched key
}

#[test]
fn claim_access_mode_and_lease_time() {
    let s = build_state();
    flat_register(&s, "7", "1234");
    // access_mode 0 (undef) -> exclusive, lease 0 -> unlimited: grant
    assert_eq!(
        flat_claim(
            &s,
            ClaimTargetKind::Zone,
            "1234",
            "7",
            &[42],
            Some(0),
            Some(0)
        )
        .decision,
        1
    );
    // access_mode 1 (exclusive) explicit, with a 30-min lease: grant on a free zone
    assert_eq!(
        flat_claim(
            &s,
            ClaimTargetKind::Zone,
            "1234",
            "7",
            &[43],
            Some(1),
            Some(30)
        )
        .decision,
        1
    );
    // access_mode 2 (reserved/future) -> rejected as bad request (reason 5)
    flat_register(&s, "8", "5678");
    let r = flat_claim(
        &s,
        ClaimTargetKind::Node,
        "5678",
        "8",
        &[139],
        Some(2),
        None,
    );
    assert_eq!((r.decision, r.reason), (0, 5));
}
