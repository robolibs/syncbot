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
    let mut restored = Coordinator::restore(decoded, Some(Arc::clone(&idx2)));

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
    let mut restored = Coordinator::restore(decoded, Some(idx2));

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
    let mut restored = Coordinator::restore(loaded, Some(idx2));
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

/// The snapshot carries every robot's key in plaintext, so it must not be
/// readable by anyone but its owner.
#[cfg(unix)]
#[test]
fn the_state_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("syncbot-perms-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("state.json");

    syncbot::persist::write_atomic(&path, &syncbot::persist::CoordinatorSnapshot::default())
        .expect("write");

    let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "state file mode was {:o}",
        mode & 0o777
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// Behavioural fidelity across a restart (PLAN 2.3.7).
//
// The tests above round-trip *fields*. These assert the restored coordinator
// *behaves* the same — which is the thing an operator actually depends on.
// ---------------------------------------------------------------------------

#[cfg(feature = "rest")]
mod behaviour {
    use std::sync::Arc;

    use datapod::{Geo, Point, Polygon};
    use syncbot::wire::{ServeState, flat_claim, flat_register};
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

    fn index() -> Arc<WorkspaceIndex> {
        let mut root = ZoneBuilder::new()
            .with_name("root")
            .with_kind("workspace")
            .with_boundary(rect(0.0, 0.0, 1000.0, 1000.0))
            .with_datum(Geo::new(52.0, 5.0, 0.0))
            .build()
            .expect("root");
        for i in 0..3u64 {
            let x0 = 10.0 + i as f64 * 100.0;
            root.add_child(
                ZoneBuilder::new()
                    .with_name(format!("z{i}"))
                    .with_kind("zone")
                    .with_boundary(rect(x0, 10.0, x0 + 50.0, 500.0))
                    .with_datum(Geo::new(52.0, 5.0, 0.0))
                    .with_property(NUMERIC_ID_PROPERTY, i.to_string())
                    .with_property("traffic.policy", "exclusive")
                    .build()
                    .expect("zone"),
            )
            .expect("add");
        }
        Arc::new(WorkspaceIndex::new(Arc::new(Workspace::new(root))))
    }

    /// Snapshot a live session, restore it, and check the restored core makes
    /// the same decisions: the claim that was blocked is still blocked, the
    /// key that worked still works, and a wrong key is still refused.
    ///
    /// The index is shared across the restart, as it is in production — the
    /// workspace is reloaded from the same directory and keeps its uuids.
    /// Claims are keyed by resource uuid, so a workspace rebuilt with fresh
    /// ones would legitimately orphan every claim (which is what
    /// `WorkspaceAccepted::stale_claims` reports on a push).
    #[test]
    fn a_restored_core_decides_the_same_way() {
        let workspace = index();
        let before = ServeState::new(Coordinator::with_index(Arc::clone(&workspace)))
            .with_kdf_params(syncbot::core::key::insecure_test_cost());
        flat_register(&before, "7", "1234", None);
        flat_register(&before, "8", "5678", None);
        assert_eq!(
            flat_claim(
                &before,
                ClaimTargetKind::Zone,
                "1234",
                "7",
                &[0],
                None,
                None
            )
            .decision,
            1
        );
        let blocked = flat_claim(
            &before,
            ClaimTargetKind::Zone,
            "5678",
            "8",
            &[0],
            None,
            None,
        );
        assert_eq!((blocked.decision, blocked.reason), (0, 2));

        // Restart.
        let snapshot = before.coordinator().read().unwrap().snapshot();
        let after = ServeState::new(Coordinator::restore(snapshot, Some(workspace)))
            .with_kdf_params(syncbot::core::key::insecure_test_cost());

        // The holder still holds it.
        let still_blocked =
            flat_claim(&after, ClaimTargetKind::Zone, "5678", "8", &[0], None, None);
        assert_eq!(
            (still_blocked.decision, still_blocked.reason),
            (0, 2),
            "robot 7's claim must survive the restart"
        );
        // And can still re-assert it with its own key.
        let owner = flat_claim(&after, ClaimTargetKind::Zone, "1234", "7", &[0], None, None);
        assert_eq!((owner.decision, owner.reason), (1, 0));
        // A wrong key is still a wrong key.
        let wrong = flat_claim(&after, ClaimTargetKind::Zone, "9999", "7", &[1], None, None);
        assert_eq!((wrong.decision, wrong.reason), (0, 1));
        // A robot that never registered is still unknown.
        let stranger = flat_claim(
            &after,
            ClaimTargetKind::Zone,
            "0000",
            "99",
            &[1],
            None,
            None,
        );
        assert_eq!(stranger.decision, 0);
    }

    /// A claim id minted after a restart must not collide with one restored
    /// from the snapshot — the ledger would silently overwrite an entry.
    #[test]
    fn minted_claim_ids_do_not_collide_with_restored_ones() {
        let workspace = index();
        let before = ServeState::new(Coordinator::with_index(Arc::clone(&workspace)))
            .with_kdf_params(syncbot::core::key::insecure_test_cost());
        flat_register(&before, "7", "1234", None);
        for zone in 0..3u64 {
            flat_claim(
                &before,
                ClaimTargetKind::Zone,
                "1234",
                "7",
                &[zone],
                None,
                None,
            );
        }
        let existing: Vec<_> = before
            .coordinator()
            .read()
            .unwrap()
            .claim_manager()
            .requests()
            .iter()
            .map(|r| r.id)
            .collect();

        let snapshot = before.coordinator().read().unwrap().snapshot();
        let after = ServeState::new(Coordinator::restore(snapshot, Some(workspace)))
            .with_kdf_params(syncbot::core::key::insecure_test_cost());

        flat_register(&after, "8", "5678", None);
        let minted = after
            .coordinator()
            .read()
            .unwrap()
            .claim_manager()
            .next_request_id();
        assert!(
            !existing.contains(&minted),
            "minted {minted} collides with a restored id from {existing:?}"
        );
    }

    /// A lease keeps its wall-clock deadline across a restart rather than
    /// being silently renewed by it.
    #[test]
    fn a_lease_deadline_survives_a_restart() {
        let workspace = index();
        let before = ServeState::new(Coordinator::with_index(Arc::clone(&workspace)))
            .with_kdf_params(syncbot::core::key::insecure_test_cost());
        flat_register(&before, "7", "1234", None);
        assert_eq!(
            flat_claim(
                &before,
                ClaimTargetKind::Zone,
                "1234",
                "7",
                &[0],
                Some(1),
                Some(1)
            )
            .decision,
            1
        );
        let deadline = before
            .coordinator()
            .read()
            .unwrap()
            .claim_manager()
            .requests()[0]
            .window
            .end_tick
            .expect("a leased claim has a deadline");

        let snapshot = before.coordinator().read().unwrap().snapshot();
        let after = ServeState::new(Coordinator::restore(snapshot, Some(workspace)))
            .with_kdf_params(syncbot::core::key::insecure_test_cost());
        let restored = after
            .coordinator()
            .read()
            .unwrap()
            .claim_manager()
            .requests()[0]
            .window
            .end_tick
            .expect("deadline survives");
        assert_eq!(deadline, restored, "a restart must not extend a lease");

        std::thread::sleep(std::time::Duration::from_millis(1_200));
        flat_register(&after, "8", "5678", None);
        let now_free = flat_claim(&after, ClaimTargetKind::Zone, "5678", "8", &[0], None, None);
        assert_eq!(
            (now_free.decision, now_free.reason),
            (1, 0),
            "the restored lease still expires on time"
        );
    }

    /// Claims name resources by uuid. A workspace rebuilt with fresh uuids —
    /// not a reload of the same one — orphans them, which is exactly what
    /// `WorkspaceAccepted::stale_claims` exists to report.
    #[test]
    fn claims_do_not_follow_a_workspace_rebuilt_with_new_uuids() {
        let before = ServeState::new(Coordinator::with_index(index()))
            .with_kdf_params(syncbot::core::key::insecure_test_cost());
        flat_register(&before, "7", "1234", None);
        assert_eq!(
            flat_claim(
                &before,
                ClaimTargetKind::Zone,
                "1234",
                "7",
                &[0],
                None,
                None
            )
            .decision,
            1
        );

        let snapshot = before.coordinator().read().unwrap().snapshot();
        // A *different* workspace: same numeric aliases, different uuids.
        let after = ServeState::new(Coordinator::restore(snapshot, Some(index())))
            .with_kdf_params(syncbot::core::key::insecure_test_cost());

        flat_register(&after, "8", "5678", None);
        let free = flat_claim(&after, ClaimTargetKind::Zone, "5678", "8", &[0], None, None);
        assert_eq!(
            (free.decision, free.reason),
            (1, 0),
            "the old claim names a uuid this workspace does not have, so zone 0 is free"
        );
    }
}
