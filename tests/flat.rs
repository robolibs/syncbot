//! Flat (tier-1) wire flow: register → heartbeat → claim → release, with the
//! mandatory key and `decision`/`reason` enum replies. Exercises the
//! transport-neutral `flat_*` functions directly.

#![cfg(feature = "rest")]

use std::collections::BTreeMap as OMap;
use std::sync::Arc;

use datapod::{Geo, Point, Polygon};
use syncbot::wire::{
    ReportedPosition, ServeState, flat_claim, flat_heartbeat, flat_register, flat_release,
};
use syncbot::{ClaimTargetKind, Coordinator, NUMERIC_ID_PROPERTY, RobotId, WorkspaceIndex};
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
        .with_kdf_params(syncbot::core::key::insecure_test_cost())
}

#[test]
fn register_is_idempotent_deny_on_reuse() {
    let s = build_state();
    // first registration succeeds
    let r = flat_register(&s, "7", "1234", None);
    assert_eq!((r.decision, r.reason), (1, 0));
    // same id again -> deny "already registered" (reason 2)
    let r = flat_register(&s, "7", "1234", None);
    assert_eq!((r.decision, r.reason), (0, 2));
    // bad id (not int/uuid) -> reason 3
    let r = flat_register(&s, "notanid", "1234", None);
    assert_eq!((r.decision, r.reason), (0, 3));
    // A real but unsupported DID method -> reason 4.
    let r = flat_register(
        &s,
        "8",
        "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK",
        None,
    );
    assert_eq!((r.decision, r.reason), (0, 4));
    // Something that is not a key at all -> reason 1 (mismatched key).
    let r = flat_register(&s, "9", "did:key=not-a-did", None);
    assert_eq!((r.decision, r.reason), (0, 1));
}

#[test]
fn heartbeat_requires_registration_and_key() {
    let s = build_state();
    // not registered -> reason 2
    let r = flat_heartbeat(&s, "7", "1234", Some(42), None, None, None, None);
    assert_eq!((r.decision, r.reason), (0, 2));
    flat_register(&s, "7", "1234", None);
    // wrong key -> mismatched key (reason 1)
    let r = flat_heartbeat(&s, "7", "9999", Some(42), None, None, None, None);
    assert_eq!((r.decision, r.reason), (0, 1));
    // correct key -> ack ok
    let r = flat_heartbeat(&s, "7", "1234", Some(42), None, None, None, None);
    assert_eq!((r.decision, r.reason), (1, 0));
}

#[test]
fn claim_conflict_then_release() {
    let s = build_state();
    flat_register(&s, "7", "1234", None);
    flat_register(&s, "8", "5678", None);

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
        .with_kdf_params(syncbot::core::key::insecure_test_cost())
}

#[test]
fn claiming_zone_blocks_node_inside_it() {
    let s = build_nested_state();
    flat_register(&s, "7", "1234", None);
    flat_register(&s, "8", "5678", None);

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
    flat_register(&s, "7", "1234", None);
    flat_register(&s, "8", "5678", None);

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
    flat_register(&s, "7", "1234", None);
    flat_register(&s, "8", "5678", None);

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
fn uuid_robot_id_full_flow() {
    let s = build_state();
    let uuid = "11111111-1111-1111-1111-111111111111";

    // register with a UUID id
    assert_eq!(flat_register(&s, uuid, "1234", None).decision, 1);
    // re-registering the same UUID -> already registered (reason 2)
    let r = flat_register(&s, uuid, "1234", None);
    assert_eq!((r.decision, r.reason), (0, 2));

    // heartbeat by UUID, correct key -> ack
    assert_eq!(
        flat_heartbeat(&s, uuid, "1234", Some(42), None, None, None, None).decision,
        1
    );
    // wrong key -> mismatched key
    assert_eq!(
        flat_heartbeat(&s, uuid, "9999", Some(42), None, None, None, None).reason,
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
        flat_heartbeat(&s, other, "1234", Some(42), None, None, None, None).reason,
        2
    );

    // a garbage id is rejected at registration (bad id, reason 3)
    assert_eq!(flat_register(&s, "not-an-id", "1234", None).reason, 3);
}

#[test]
fn keyless_registration_is_refused_unless_enabled() {
    // Every keyless robot shares one password, so anyone can act as any of
    // them. That is a decision an operator makes, not a default.
    let closed = build_state();
    let refused = flat_register(&closed, "7", "0", None);
    assert_eq!(
        (refused.decision, refused.reason),
        (0, 5),
        "keyless registration must be refused as not permitted"
    );
    // A robot bringing its own key is unaffected.
    assert_eq!(flat_register(&closed, "7", "1234", None).decision, 1);
}

#[test]
fn key_is_optional_defaults_to_shared_password() {
    let s = build_state().with_default_key_allowed(true);
    // register with NO key -> uses the default password
    let r = flat_register(&s, "7", "0", None);
    assert_eq!((r.decision, r.reason), (1, 0));
    // a second robot registered the same way also works (each bound to default)
    assert_eq!(flat_register(&s, "8", "0", None).decision, 1);
    // claim with the default key succeeds for the default-registered robot
    assert_eq!(
        flat_claim(&s, ClaimTargetKind::Zone, "0", "7", &[42], None, None).decision,
        1
    );
    // a robot that registered WITH a real key is NOT satisfied by the default
    flat_register(&s, "9", "1234", None);
    let r = flat_claim(&s, ClaimTargetKind::Zone, "0", "9", &[43], None, None);
    assert_eq!((r.decision, r.reason), (0, 1)); // mismatched key
}

#[test]
fn claim_access_mode_and_lease_time() {
    let s = build_state();
    flat_register(&s, "7", "1234", None);
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
    // access_mode 1 (exclusive) explicit, with a 30-second lease: grant on a free zone
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
    // access_mode 3 (reserved/future) -> rejected as bad request (reason 5).
    // (access_mode 2 is now the approved SHARED mode; see the shared-zone tests.)
    flat_register(&s, "8", "5678", None);
    let r = flat_claim(
        &s,
        ClaimTargetKind::Node,
        "5678",
        "8",
        &[139],
        Some(3),
        None,
    );
    assert_eq!((r.decision, r.reason), (0, 5));
}

#[test]
fn heartbeat_zone_minus_one_is_unknown_location() {
    let s = build_state();
    flat_register(&s, "7", "1234", None);
    // -1 = robot hasn't claimed any zone / location unknown -> still a valid ack
    let r = flat_heartbeat(&s, "7", "1234", Some(-1), None, None, None, None);
    assert_eq!((r.decision, r.reason), (1, 0));
    // a real zone also acks
    let r = flat_heartbeat(&s, "7", "1234", Some(42), None, None, None, None);
    assert_eq!((r.decision, r.reason), (1, 0));
    // no position at all acks too
    let r = flat_heartbeat(&s, "7", "1234", None, None, None, None, None);
    assert_eq!((r.decision, r.reason), (1, 0));
}

#[test]
fn alive_interval_marks_robot_inactive_after_2x() {
    let mut c = Coordinator::new();
    // robot 7 promises a 2s heartbeat interval, last seen at t=1000ms
    c.set_alive(RobotId::new(7), 2, 1_000);

    // within 2x (4s) of last heartbeat -> active
    assert!(c.robot_active_at(RobotId::new(7), 1_000)); // same instant
    assert!(c.robot_active_at(RobotId::new(7), 5_000)); // +4s exactly
    assert!(c.inactive_robots_at(5_000).is_empty());

    // past 2x -> inactive
    assert!(!c.robot_active_at(RobotId::new(7), 5_001)); // +4.001s
    assert_eq!(c.inactive_robots_at(9_999), vec![RobotId::new(7)]);

    // a heartbeat at t=8000 refreshes -> active again
    c.touch_robot(RobotId::new(7), 8_000);
    assert!(c.robot_active_at(RobotId::new(7), 9_999));

    // default interval (2s) when 0 is given
    c.set_alive(RobotId::new(8), 0, 0);
    assert!(c.robot_active_at(RobotId::new(8), 4_000));
    assert!(!c.robot_active_at(RobotId::new(8), 4_001));

    // a robot with no alive info is treated as active
    assert!(c.robot_active_at(RobotId::new(99), 1_000_000));
}

#[test]
fn inactive_robot_claims_auto_released() {
    use std::time::{SystemTime, UNIX_EPOCH};
    let s = build_state();
    flat_register(&s, "7", "1234", Some(1)); // 1s heartbeat interval
    flat_register(&s, "8", "5678", Some(1));

    // robot 7 holds zone 42; robot 8 is blocked
    assert_eq!(
        flat_claim(&s, ClaimTargetKind::Zone, "1234", "7", &[42], None, None).decision,
        1
    );
    assert_eq!(
        flat_claim(&s, ClaimTargetKind::Zone, "5678", "8", &[42], None, None).reason,
        2
    );

    // simulate >2× alive (2s) elapsed with no heartbeat from 7, then sweep
    let future = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + 10_000;
    let freed = s.coordinator().write().unwrap().sweep_inactive(future);
    assert!(freed.contains(&RobotId::new(7)));

    // zone 42 is now free — robot 8 can take it
    assert_eq!(
        flat_claim(&s, ClaimTargetKind::Zone, "5678", "8", &[42], None, None).decision,
        1
    );
}

// ---------------------------------------------------------------------------
// Shared claims via access_mode=2 and reason 3 (CAPACITY) on the shared path.
// ---------------------------------------------------------------------------

/// Root + one shared zone (numeric 60, `traffic.policy=shared`, capacity 2).
fn build_shared_state() -> ServeState {
    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 100.0, 100.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root zone");
    let shared = ZoneBuilder::new()
        .with_name("shared")
        .with_kind("zone")
        .with_boundary(rectangle(10.0, 10.0, 50.0, 50.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .with_property(NUMERIC_ID_PROPERTY, "60")
        .with_property("traffic.policy", "shared")
        .with_property("traffic.capacity", "2")
        .build()
        .expect("shared zone");
    root.add_child(shared).expect("add zone");

    let idx = Arc::new(WorkspaceIndex::new(Arc::new(Workspace::new(root))));
    ServeState::new(Coordinator::with_index(idx))
        .with_kdf_params(syncbot::core::key::insecure_test_cost())
}

#[test]
fn shared_zone_admits_up_to_capacity_then_reason_3() {
    let s = build_shared_state();
    flat_register(&s, "7", "1234", None);
    flat_register(&s, "8", "5678", None);
    flat_register(&s, "9", "9012", None);

    // Two shared claimants (access_mode 2) fit the cap-2 zone -> both granted.
    let r = flat_claim(&s, ClaimTargetKind::Zone, "1234", "7", &[60], Some(2), None);
    assert_eq!((r.decision, r.reason), (1, 0));
    let r = flat_claim(&s, ClaimTargetKind::Zone, "5678", "8", &[60], Some(2), None);
    assert_eq!((r.decision, r.reason), (1, 0));

    // A third shared claimant exceeds capacity -> reason 3 (CAPACITY), not 2.
    let r = flat_claim(&s, ClaimTargetKind::Zone, "9012", "9", &[60], Some(2), None);
    assert_eq!((r.decision, r.reason), (0, 3));
    assert_eq!(r.blocked, Some(60));
}

#[test]
fn exclusive_on_shared_zone_is_conflict_not_capacity() {
    let s = build_shared_state();
    flat_register(&s, "7", "1234", None);
    flat_register(&s, "8", "5678", None);

    // A shared claimant holds the zone (well within capacity 2).
    let r = flat_claim(&s, ClaimTargetKind::Zone, "1234", "7", &[60], Some(2), None);
    assert_eq!((r.decision, r.reason), (1, 0));

    // An EXCLUSIVE claim (access_mode 1) on that same zone must return reason 2
    // (CONFLICT), NOT 3 — exclusive claims never route through capacity_eval.
    let r = flat_claim(&s, ClaimTargetKind::Zone, "5678", "8", &[60], Some(1), None);
    assert_eq!((r.decision, r.reason), (0, 2));
    assert_eq!(r.blocked, Some(60));
}

#[test]
fn access_mode_2_accepted_3_still_bad_request() {
    let s = build_shared_state();
    flat_register(&s, "7", "1234", None);

    // access_mode 2 (shared) no longer returns reason 5; it is granted here.
    let r = flat_claim(&s, ClaimTargetKind::Zone, "1234", "7", &[60], Some(2), None);
    assert_eq!((r.decision, r.reason), (1, 0));

    // access_mode 3 remains reserved -> reason 5 (BAD_REQUEST).
    let r = flat_claim(&s, ClaimTargetKind::Zone, "1234", "7", &[60], Some(3), None);
    assert_eq!((r.decision, r.reason), (0, 5));
}

/// Root + one numeric zone, with a datum bound and `coord_mode` left at its
/// default of Global — the shape roboviz actually pushes.
fn build_state_with_datum() -> ServeState {
    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 100.0, 100.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root zone");
    let zone = ZoneBuilder::new()
        .with_name("a")
        .with_kind("zone")
        .with_boundary(rectangle(10.0, 10.0, 50.0, 50.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .with_property(NUMERIC_ID_PROPERTY, "42")
        .build()
        .expect("zone");
    root.add_child(zone).expect("add zone");
    let mut ws = Workspace::new(root);
    ws.set_datum(Geo::new(52.0, 5.0, 0.0));
    let idx = Arc::new(WorkspaceIndex::new(Arc::new(ws)));
    ServeState::new(Coordinator::with_index(idx))
        .with_kdf_params(syncbot::core::key::insecure_test_cost())
}

fn robot_position(state: &ServeState) -> syncbot::RobotPosition {
    let coord = state.coordinator();
    let coord = coord.read().expect("coordinator");
    coord
        .robot_states()
        .first()
        .expect("one robot")
        .position
        .expect("a position was reported")
}

/// The whole point of relaxing the coord_mode gate: roboviz pushes global-mode
/// workspaces, and a robot reporting metres in one must still be placeable.
#[test]
fn a_global_mode_workspace_still_converts_robot_positions() {
    let s = build_state_with_datum();
    assert_eq!(flat_register(&s, "7", "1234", None).decision, 1);

    // 100 m east, 50 m north of the datum.
    let r = flat_heartbeat(
        &s,
        "7",
        "1234",
        None,
        None,
        None,
        Some(ReportedPosition::Local {
            x: 100.0,
            y: 50.0,
            z: 0.0,
        }),
        None,
    );
    assert_eq!((r.decision, r.reason), (1, 0));

    let position = robot_position(&s);
    assert!(position.converted, "a datum is bound, so it must convert");
    assert_eq!(position.reported, syncbot::PositionFrame::Local);
    assert_eq!((position.x, position.y), (100.0, 50.0), "kept as sent");
    // East of the datum means a larger longitude, north means a larger
    // latitude. Rough bounds: this pins the direction, not the ellipsoid.
    assert!(position.lon > 5.0, "100 m east must raise longitude");
    assert!(position.lat > 52.0, "50 m north must raise latitude");
    assert!((position.lat - 52.0).abs() < 0.01 && (position.lon - 5.0).abs() < 0.01);
}

#[test]
fn a_global_position_round_trips_back_to_where_it_started() {
    let s = build_state_with_datum();
    assert_eq!(flat_register(&s, "7", "1234", None).decision, 1);

    let r = flat_heartbeat(
        &s,
        "7",
        "1234",
        None,
        None,
        None,
        Some(ReportedPosition::Global {
            lat: 52.001,
            lon: 5.001,
            alt: 12.0,
        }),
        None,
    );
    assert_eq!((r.decision, r.reason), (1, 0));

    let position = robot_position(&s);
    assert!(position.converted);
    assert_eq!(position.reported, syncbot::PositionFrame::Global);
    assert_eq!(
        (position.lat, position.lon),
        (52.001, 5.001),
        "kept as sent"
    );
    // ~111 m north and ~68 m east at this latitude; the point is that the
    // derived metres are sane, not that they hit a specific ellipsoid value.
    assert!(
        position.y > 50.0 && position.y < 200.0,
        "y was {}",
        position.y
    );
    assert!(
        position.x > 30.0 && position.x < 150.0,
        "x was {}",
        position.x
    );
}

/// With no datum there is nothing to convert against. The reported frame is
/// kept and `converted` says the rest is unknown — rather than a zeroed
/// lat/lon reading as "somewhere off west Africa".
#[test]
fn without_a_datum_a_position_is_kept_but_not_converted() {
    let s = build_state(); // Workspace::new leaves datum None
    assert_eq!(flat_register(&s, "7", "1234", None).decision, 1);

    let r = flat_heartbeat(
        &s,
        "7",
        "1234",
        None,
        None,
        None,
        Some(ReportedPosition::Local {
            x: 10.0,
            y: 20.0,
            z: 1.0,
        }),
        None,
    );
    assert_eq!((r.decision, r.reason), (1, 0));

    let position = robot_position(&s);
    assert!(
        !position.converted,
        "no datum, so nothing to convert against"
    );
    assert_eq!((position.x, position.y, position.z), (10.0, 20.0, 1.0));
    assert_eq!(
        (position.lat, position.lon),
        (0.0, 0.0),
        "unknown, not measured"
    );
}

/// NaN would spread through every later conversion and comparison in silence.
#[test]
fn a_position_that_is_not_a_number_is_refused() {
    let s = build_state_with_datum();
    assert_eq!(flat_register(&s, "7", "1234", None).decision, 1);

    for bad in [
        ReportedPosition::Local {
            x: f64::NAN,
            y: 0.0,
            z: 0.0,
        },
        ReportedPosition::Global {
            lat: 91.0, // off the globe
            lon: 5.0,
            alt: 0.0,
        },
    ] {
        let r = flat_heartbeat(&s, "7", "1234", None, None, None, Some(bad), None);
        assert_eq!((r.decision, r.reason), (0, 3), "refused: {bad:?}");
    }
    let r = flat_heartbeat(&s, "7", "1234", None, None, None, None, Some(f64::INFINITY));
    assert_eq!(
        (r.decision, r.reason),
        (0, 3),
        "a non-finite yaw is refused"
    );
}

/// Position and heading are independent, and `None` means "not reported now",
/// never "moved to nowhere".
#[test]
fn a_heading_only_heartbeat_keeps_the_last_position() {
    let s = build_state_with_datum();
    assert_eq!(flat_register(&s, "7", "1234", None).decision, 1);
    flat_heartbeat(
        &s,
        "7",
        "1234",
        None,
        None,
        None,
        Some(ReportedPosition::Local {
            x: 7.0,
            y: 8.0,
            z: 0.0,
        }),
        None,
    );

    let r = flat_heartbeat(&s, "7", "1234", None, None, None, None, Some(0.0));
    assert_eq!((r.decision, r.reason), (1, 0));

    let position = robot_position(&s);
    assert_eq!((position.x, position.y), (7.0, 8.0), "position survived");
}

// ---------------------------------------------------------------------------
// lease_time actually expires the claim.
// ---------------------------------------------------------------------------

/// `lease_time` used to be inert: it wrote a bound into the claim window in a
/// tick space nothing advanced, so the claim was held until the robot stopped
/// heartbeating. It now expires on its own.
#[test]
fn a_leased_claim_frees_itself_when_the_lease_runs_out() {
    let s = build_state();
    flat_register(&s, "7", "1234", None);
    flat_register(&s, "8", "5678", None);

    // Robot 7 takes zone 42 for one second.
    let taken = flat_claim(
        &s,
        ClaimTargetKind::Zone,
        "1234",
        "7",
        &[42],
        Some(1),
        Some(1),
    );
    assert_eq!((taken.decision, taken.reason), (1, 0));

    // Robot 8 is refused while that lease stands.
    let blocked = flat_claim(&s, ClaimTargetKind::Zone, "5678", "8", &[42], Some(1), None);
    assert_eq!(
        (blocked.decision, blocked.reason),
        (0, 2),
        "the zone is held for another second"
    );

    std::thread::sleep(std::time::Duration::from_millis(1_200));

    // The next claim expires the lapsed one before evaluating, so it is free
    // without waiting for the periodic sweep.
    let after = flat_claim(&s, ClaimTargetKind::Zone, "5678", "8", &[42], Some(1), None);
    assert_eq!(
        (after.decision, after.reason),
        (1, 0),
        "the lease ran out, so the zone is free"
    );
}

/// An unleased claim is still held until it is released or the robot goes
/// quiet — expiry must not collect it.
#[test]
fn an_unleased_claim_is_not_expired() {
    let s = build_state();
    flat_register(&s, "7", "1234", None);
    flat_register(&s, "8", "5678", None);

    assert_eq!(
        flat_claim(&s, ClaimTargetKind::Zone, "1234", "7", &[42], None, None).decision,
        1
    );
    std::thread::sleep(std::time::Duration::from_millis(50));

    let blocked = flat_claim(&s, ClaimTargetKind::Zone, "5678", "8", &[42], None, None);
    assert_eq!(
        (blocked.decision, blocked.reason),
        (0, 2),
        "an open-ended claim stays held"
    );
}

// ---------------------------------------------------------------------------
// A robot does not compete with itself (PLAN Milestone 1.1).
// ---------------------------------------------------------------------------

/// Re-asserting a claim you already hold used to be denied with reason 2 by
/// your own ledger entry, which broke rolling-horizon claiming outright.
#[test]
fn a_robot_can_reclaim_what_it_already_holds() {
    let s = build_state();
    flat_register(&s, "7", "1234", None);

    let first = flat_claim(&s, ClaimTargetKind::Zone, "1234", "7", &[42], None, None);
    assert_eq!((first.decision, first.reason), (1, 0));

    let again = flat_claim(&s, ClaimTargetKind::Zone, "1234", "7", &[42], None, None);
    assert_eq!(
        (again.decision, again.reason),
        (1, 0),
        "a holder re-acquiring is a re-acquisition, not a conflict"
    );
}

/// Re-claiming must refresh the existing entry, not stack another one — the
/// wire mints a fresh claim id on every call.
#[test]
fn reclaiming_does_not_grow_the_ledger() {
    let s = build_state();
    flat_register(&s, "7", "1234", None);

    for _ in 0..5 {
        assert_eq!(
            flat_claim(&s, ClaimTargetKind::Zone, "1234", "7", &[42], None, None).decision,
            1
        );
    }

    let held = s
        .coordinator()
        .read()
        .unwrap()
        .claim_manager()
        .request_count();
    assert_eq!(held, 1, "five identical claims left {held} ledger entries");
}

/// Not competing with yourself must not become "not counting yourself".
/// Another robot still sees the claim.
#[test]
fn self_reclaim_does_not_release_the_ground() {
    let s = build_state();
    flat_register(&s, "7", "1234", None);
    flat_register(&s, "8", "5678", None);

    flat_claim(&s, ClaimTargetKind::Zone, "1234", "7", &[42], None, None);
    flat_claim(&s, ClaimTargetKind::Zone, "1234", "7", &[42], None, None);

    let other = flat_claim(&s, ClaimTargetKind::Zone, "5678", "8", &[42], None, None);
    assert_eq!(
        (other.decision, other.reason),
        (0, 2),
        "robot 7 still holds zone 42 after re-claiming it"
    );
}

/// A robot advancing its rolling horizon claims new ground while still
/// holding the old — the case that motivated 1.1.
#[test]
fn a_robot_can_claim_ahead_while_holding_behind() {
    let s = build_state();
    flat_register(&s, "7", "1234", None);

    assert_eq!(
        flat_claim(&s, ClaimTargetKind::Zone, "1234", "7", &[42], None, None).decision,
        1
    );
    assert_eq!(
        flat_claim(&s, ClaimTargetKind::Zone, "1234", "7", &[43], None, None).decision,
        1,
        "claiming the next slice must not be refused by the previous one"
    );
}

// ---------------------------------------------------------------------------
// Atomic route claims (PLAN Milestone 1.4).
// ---------------------------------------------------------------------------

/// A route spans nodes and edges. Claiming them through the single-kind
/// endpoints is two independent requests; this is one.
#[test]
fn a_route_claim_is_all_or_nothing() {
    let s = build_state();
    flat_register(&s, "7", "1234", None);
    flat_register(&s, "8", "5678", None);

    // Robot 8 takes node 139 first.
    assert_eq!(
        flat_claim(&s, ClaimTargetKind::Node, "5678", "8", &[139], None, None).decision,
        1
    );

    // Robot 7 asks for a route that includes node 139. It must be refused
    // whole — no part of it may be left held.
    let denied = syncbot::wire::flat_claim_route(&s, "1234", "7", &[139], &[], None, None);
    assert_eq!((denied.decision, denied.reason), (0, 2));

    let held_by_7 = s
        .coordinator()
        .read()
        .unwrap()
        .claim_manager()
        .requests()
        .iter()
        .filter(|r| r.robot_id == RobotId::new(7))
        .count();
    assert_eq!(held_by_7, 0, "a denied route must leave nothing behind");
}

/// An unknown id in either section refuses the whole route and names the id.
#[test]
fn a_route_claim_with_an_unknown_id_is_refused_whole() {
    let s = build_state();
    flat_register(&s, "7", "1234", None);

    let reply = syncbot::wire::flat_claim_route(&s, "1234", "7", &[139], &[9999], None, None);
    assert_eq!(
        (reply.decision, reply.reason, reply.blocked),
        (0, 4, Some(9999)),
        "unknown edge 9999 refuses the route and is named"
    );
    assert_eq!(
        s.coordinator()
            .read()
            .unwrap()
            .claim_manager()
            .request_count(),
        0
    );
}

/// An empty route is a bad request, not an empty grant.
#[test]
fn an_empty_route_claim_is_refused() {
    let s = build_state();
    flat_register(&s, "7", "1234", None);

    let reply = syncbot::wire::flat_claim_route(&s, "1234", "7", &[], &[], None, None);
    assert_eq!((reply.decision, reply.reason), (0, 5));
}
