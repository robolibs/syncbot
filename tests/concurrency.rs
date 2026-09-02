//! Concurrent load against one coordinator (PLAN 2.3.4).
//!
//! `ServeState` is an `Arc<RwLock<Coordinator>>` and adapters call into it from
//! many threads. Nothing exercised that: every other test drives it from one.
//! The `next_request_id` load/store race fixed earlier would have been caught
//! by construction here rather than by reading the code.

#![cfg(feature = "rest")]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use datapod::{Geo, Point, Polygon};
use syncbot::wire::{ServeState, flat_claim, flat_register, flat_release};
use syncbot::{ClaimTargetKind, Coordinator, NUMERIC_ID_PROPERTY, RobotId, WorkspaceIndex};
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

/// `zones` exclusive zones, numbered from 0.
fn state(zones: u64) -> ServeState {
    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rect(0.0, 0.0, 10_000.0, 1_000.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root");
    for i in 0..zones {
        let x0 = 10.0 + i as f64 * 50.0;
        root.add_child(
            ZoneBuilder::new()
                .with_name(format!("z{i}"))
                .with_kind("zone")
                .with_boundary(rect(x0, 10.0, x0 + 30.0, 500.0))
                .with_datum(Geo::new(52.0, 5.0, 0.0))
                .with_property(NUMERIC_ID_PROPERTY, i.to_string())
                .with_property("traffic.policy", "exclusive")
                .build()
                .expect("zone"),
        )
        .expect("add");
    }
    let index = Arc::new(WorkspaceIndex::new(Arc::new(Workspace::new(root))));
    ServeState::new(Coordinator::with_index(index))
}

/// SAFETY under load: however the interleaving falls out, no exclusive zone
/// ends up held by two robots.
#[test]
fn concurrent_claims_never_double_book_a_zone() {
    const ROBOTS: u64 = 8;
    const ZONES: u64 = 4;
    const ROUNDS: usize = 60;

    let state = state(ZONES);
    for robot in 0..ROBOTS {
        assert_eq!(
            flat_register(&state, &robot.to_string(), "1234", Some(9999)).decision,
            1
        );
    }

    let grants = Arc::new(AtomicUsize::new(0));
    let denials = Arc::new(AtomicUsize::new(0));

    thread::scope(|scope| {
        for robot in 0..ROBOTS {
            let state = state.clone();
            let grants = Arc::clone(&grants);
            let denials = Arc::clone(&denials);
            scope.spawn(move || {
                let robot = robot.to_string();
                for round in 0..ROUNDS {
                    // Every robot fights over the same small set of zones.
                    let zone = (round as u64) % ZONES;
                    let reply = flat_claim(
                        &state,
                        ClaimTargetKind::Zone,
                        "1234",
                        &robot,
                        &[zone],
                        None,
                        None,
                    );
                    match reply.decision {
                        1 => grants.fetch_add(1, Ordering::Relaxed),
                        _ => denials.fetch_add(1, Ordering::Relaxed),
                    };
                    // Half the time, give it back again.
                    if round % 2 == 0 {
                        flat_release(&state, ClaimTargetKind::Zone, "1234", &robot, zone);
                    }
                }
            });
        }
    });

    // Nothing was lost: every request produced exactly one decision.
    let total = grants.load(Ordering::Relaxed) + denials.load(Ordering::Relaxed);
    assert_eq!(
        total,
        ROBOTS as usize * ROUNDS,
        "some requests produced no decision at all"
    );

    // And the ledger is still sound: one holder per exclusive zone.
    let coord = state.coordinator();
    let guard = coord.read().unwrap();
    let mut holder: BTreeMap<uuid::Uuid, RobotId> = BTreeMap::new();
    for request in guard.claim_manager().requests() {
        for target in &request.targets {
            if let Some(other) = holder.get(&target.resource_id) {
                assert_eq!(
                    *other, request.robot_id,
                    "zone {} is held by both {} and {}",
                    target.resource_id, other, request.robot_id
                );
            }
            holder.insert(target.resource_id, request.robot_id);
        }
    }
}

/// Claim ids are minted under concurrency and must stay unique — a collision
/// silently overwrites a ledger entry, releasing a zone nobody released.
#[test]
fn concurrently_minted_claim_ids_are_unique() {
    const THREADS: usize = 8;
    const PER_THREAD: usize = 200;

    let state = state(1);
    let coord = state.coordinator();
    let minted = Arc::new(std::sync::Mutex::new(Vec::new()));

    thread::scope(|scope| {
        for _ in 0..THREADS {
            let coord = Arc::clone(&coord);
            let minted = Arc::clone(&minted);
            scope.spawn(move || {
                let mut local = Vec::with_capacity(PER_THREAD);
                for _ in 0..PER_THREAD {
                    let guard = coord.read().unwrap();
                    local.push(guard.claim_manager().next_request_id());
                }
                minted.lock().unwrap().extend(local);
            });
        }
    });

    let all = minted.lock().unwrap();
    let unique: BTreeSet<_> = all.iter().copied().collect();
    assert_eq!(
        unique.len(),
        all.len(),
        "{} of {} minted claim ids collided",
        all.len() - unique.len(),
        all.len()
    );
}

/// Registration races must not produce two robots with the same id, nor lose
/// the winner's key.
#[test]
fn concurrent_registration_of_one_id_has_exactly_one_winner() {
    const THREADS: usize = 16;

    let state = state(1);
    let wins = Arc::new(AtomicUsize::new(0));

    thread::scope(|scope| {
        for _ in 0..THREADS {
            let state = state.clone();
            let wins = Arc::clone(&wins);
            scope.spawn(move || {
                if flat_register(&state, "7", "1234", Some(9999)).decision == 1 {
                    wins.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });

    assert_eq!(
        wins.load(Ordering::Relaxed),
        1,
        "exactly one registration of robot 7 may succeed"
    );
    let coord = state.coordinator();
    let guard = coord.read().unwrap();
    assert_eq!(guard.robot_count(), 1, "one robot, not several");
    assert!(
        guard.has_robot(RobotId::new(7)),
        "the winner must actually be registered"
    );
}
