//! Opt-in state persistence tests.
//!
//! Exercises the `snapshot()`/`restore()` API directly (no env vars, to avoid
//! cross-test races) plus the atomic-write round-trip through a temp file.

use std::sync::Arc;

use datapod::{Geo, Point, Polygon};
use syncbot::persist::{self, CoordinatorSnapshot};
use syncbot::{
    ClaimAccessMode, ClaimId, ClaimRequest, ClaimTarget, ClaimTargetKind, Coordinator, Key, Lease,
    LeaseId, RobotId, WorkspaceIndex,
};
use uuid::Uuid;
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

/// Build a fresh workspace with a single exclusive zone and return the index
/// plus that zone's UUID. Called twice on purpose (pre- and post-restore) to
/// prove the index is re-attached, not carried in the snapshot.
fn make_index() -> (Arc<WorkspaceIndex>, Uuid) {
    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 100.0, 100.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root");
    let zone = ZoneBuilder::new()
        .with_name("dock")
        .with_kind("zone")
        .with_boundary(rectangle(10.0, 10.0, 50.0, 50.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .with_property("traffic.policy", "exclusive")
        .build()
        .expect("zone");
    root.add_child(zone).unwrap();
    let zone_id = root.children()[0].id();
    let idx = Arc::new(WorkspaceIndex::new(Arc::new(Workspace::new(root))));
    (idx, zone_id)
}

const ROBOT: RobotId = RobotId::new(7);

/// Register a robot with a key, place an active claim request and a lease on
/// `zone_id`, and advance the claim-id counter. Returns the last minted id.
fn populate(coord: &mut Coordinator, zone_id: Uuid) -> ClaimId {
    assert!(coord.register_with_key(ROBOT, Key::Numeric(1234)));

    let claim_id = coord.claim_manager().next_request_id();
    let target = ClaimTarget {
        kind: ClaimTargetKind::Zone,
        resource_id: zone_id,
    };
    coord.claim_manager_mut().add_request(ClaimRequest {
        id: claim_id,
        robot_id: ROBOT,
        access_mode: ClaimAccessMode::Exclusive,
        targets: vec![target],
        ..ClaimRequest::default()
    });
    coord.claim_manager_mut().add_lease(Lease {
        id: LeaseId::new(1),
        claim_id,
        robot_id: ROBOT,
        access_mode: ClaimAccessMode::Exclusive,
        targets: vec![target],
        active: true,
        ..Lease::default()
    });

    // Advance the monotonic counter so we can prove it continues after restore.
    coord.claim_manager().next_request_id()
}

#[test]
fn snapshot_restore_round_trips_through_json() {
    let (idx, zone_id) = make_index();
    let mut coord = Coordinator::with_index(Arc::clone(&idx));
    let last_minted = populate(&mut coord, zone_id);

    // snapshot -> JSON -> back to a snapshot value.
    let snapshot = coord.snapshot();
    let json = serde_json::to_string(&snapshot).expect("serialize snapshot");
    let decoded: CoordinatorSnapshot = serde_json::from_str(&json).expect("deserialize snapshot");

    // Fresh, independently-built index (proves it is re-attached on restore).
    let (idx2, zone_id2) = make_index();
    assert_ne!(
        zone_id, zone_id2,
        "the two workspaces mint distinct zone ids"
    );
    let restored = Coordinator::restore(decoded, Some(Arc::clone(&idx2)));

    // Robot still registered.
    assert!(restored.has_robot(ROBOT));
    assert!(restored.find_robot_state(ROBOT).is_some());

    // Right key validates, wrong key does not.
    assert!(restored.validate_key(ROBOT, &Key::Numeric(1234)));
    assert!(!restored.validate_key(ROBOT, &Key::Numeric(9999)));

    // Claim request and lease survived.
    assert_eq!(restored.claim_manager().request_count(), 1);
    assert!(restored.claim_manager().has_lease(LeaseId::new(1)));

    // Index re-attached to both the coordinator and the claim manager.
    assert!(restored.has_index());
    assert!(restored.claim_manager().has_index());

    // Next minted id continues strictly above the pre-restart counter and above
    // any restored id.
    let next = restored.claim_manager().next_request_id();
    assert!(
        next.raw() > last_minted.raw(),
        "next id {} must exceed pre-restart max {}",
        next.raw(),
        last_minted.raw()
    );
}

#[test]
fn robot_alive_and_synthetic_counter_survive_restore() {
    let (idx, zone_id) = make_index();
    let mut coord = Coordinator::with_index(Arc::clone(&idx));

    // A UUID robot mints a synthetic id and records liveness.
    let uuid = "12345678-1234-1234-1234-1234567890ab";
    let synth = coord.resolve_or_mint_robot_id(uuid).expect("mint");
    assert!(coord.register_with_key(synth, Key::Pass("secret".into())));
    coord.set_alive(synth, 5, 1_000);
    let _ = zone_id;

    let snapshot = coord.snapshot();
    let json = serde_json::to_string(&snapshot).unwrap();
    let decoded: CoordinatorSnapshot = serde_json::from_str(&json).unwrap();

    let (idx2, _) = make_index();
    let restored = Coordinator::restore(decoded, Some(idx2));

    // UUID→id mapping and key survived.
    assert_eq!(restored.resolve_robot_id(uuid), Some(synth));
    assert!(restored.validate_key(synth, &Key::Pass("secret".into())));

    // Liveness survived: still active just after last-seen, and the synthetic
    // counter did not regress (a fresh UUID mints a DIFFERENT id).
    assert!(restored.robot_active_at(synth, 1_500));

    let mut restored = restored;
    let other = restored
        .resolve_or_mint_robot_id("abcdef00-0000-0000-0000-000000000000")
        .expect("mint second");
    assert_ne!(other, synth, "synthetic id counter must not regress");
}

#[test]
fn atomic_write_round_trip_and_missing_file() {
    let (idx, zone_id) = make_index();
    let mut coord = Coordinator::with_index(Arc::clone(&idx));
    populate(&mut coord, zone_id);
    let snapshot = coord.snapshot();

    // Unique temp dir for this test run.
    let dir = std::env::temp_dir().join(format!(
        "syncbot-persist-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("state.json");

    // Missing file loads as None.
    assert!(persist::load(&path).expect("load missing").is_none());

    // Atomic write, then load back.
    persist::write_atomic(&path, &snapshot).expect("write");
    assert!(path.exists(), "state file exists after write");
    let loaded = persist::load(&path).expect("load").expect("some snapshot");

    // Restore from the loaded snapshot and confirm state carried over.
    let (idx2, _) = make_index();
    let restored = Coordinator::restore(loaded, Some(idx2));
    assert!(restored.has_robot(ROBOT));
    assert!(restored.validate_key(ROBOT, &Key::Numeric(1234)));

    // No leftover temp files after a successful rename.
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .expect("read dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "unexpected temp files: {leftovers:?}");

    // Cleanup.
    let _ = std::fs::remove_dir_all(&dir);
}
