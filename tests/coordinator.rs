//! Coordinator integration tests.

use syncbot::{
    ArbitrationContext, ArbitrationDecision, Coordinator, Key, RobotId, RobotProgressState,
    RobotState, arbitrate_right_of_way, robot_missed_schedule_slot,
};

#[test]
fn arbitration_emergency_wins() {
    let ctx = ArbitrationContext {
        self_is_emergency: true,
        ..ArbitrationContext::default()
    };
    assert_eq!(arbitrate_right_of_way(&ctx), ArbitrationDecision::Proceed);
    let ctx = ArbitrationContext {
        other_is_emergency: true,
        ..ArbitrationContext::default()
    };
    assert_eq!(arbitrate_right_of_way(&ctx), ArbitrationDecision::Yield);
}

#[test]
fn arbitration_lease_holder_proceeds() {
    let ctx = ArbitrationContext {
        self_holds_lease: true,
        ..ArbitrationContext::default()
    };
    assert_eq!(arbitrate_right_of_way(&ctx), ArbitrationDecision::Proceed);
    let ctx = ArbitrationContext {
        other_holds_lease: true,
        ..ArbitrationContext::default()
    };
    assert_eq!(arbitrate_right_of_way(&ctx), ArbitrationDecision::Yield);
}

#[test]
fn arbitration_priority_then_state() {
    let ctx = ArbitrationContext {
        self_priority: 5.0,
        other_priority: 3.0,
        ..ArbitrationContext::default()
    };
    assert_eq!(arbitrate_right_of_way(&ctx), ArbitrationDecision::Proceed);

    let ctx = ArbitrationContext {
        self_state: RobotProgressState::FollowingRoute,
        other_state: RobotProgressState::Waiting,
        ..ArbitrationContext::default()
    };
    assert_eq!(arbitrate_right_of_way(&ctx), ArbitrationDecision::Proceed);
}

#[test]
fn arbitration_tie_replans() {
    let ctx = ArbitrationContext::default();
    assert_eq!(arbitrate_right_of_way(&ctx), ArbitrationDecision::Replan);
}

#[test]
fn missed_schedule_slot_only_when_late_and_not_idle() {
    let mut state = RobotState {
        scheduled_start_tick: Some(10),
        progress_state: RobotProgressState::Waiting,
        ..RobotState::default()
    };

    // Not late yet
    assert!(!robot_missed_schedule_slot(&state, 10, 0));
    // Late, but Idle/FollowingRoute states are excluded
    state.progress_state = RobotProgressState::Idle;
    assert!(!robot_missed_schedule_slot(&state, 20, 0));
    // Late + Waiting + grace exceeded
    state.progress_state = RobotProgressState::Waiting;
    assert!(robot_missed_schedule_slot(&state, 20, 5));
    assert!(!robot_missed_schedule_slot(&state, 20, 100));
}

// -- hardening regression tests ------------------------------------------

#[test]
fn failed_register_does_not_poison_uuid_map() {
    let mut c = Coordinator::new();
    let uuid = "12345678-1234-1234-1234-1234567890ab";

    // First registration mints a synthetic id and commits the binding.
    let id1 = c.resolve_or_mint_robot_id(uuid).expect("mint");
    assert!(c.register_with_key(id1, Key::Numeric(1)));

    // A duplicate register attempt for the same UUID fails but must NOT rebind
    // or drop the committed mapping.
    let id_again = c.resolve_or_mint_robot_id(uuid).expect("resolve existing");
    assert_eq!(id_again, id1);
    assert!(!c.register_with_key(id_again, Key::Numeric(2)));

    // The UUID still resolves to the original committed id (no poisoning).
    assert_eq!(c.resolve_robot_id(uuid), Some(id1));
    // And the original key is intact.
    assert!(c.validate_key(id1, &Key::Numeric(1)));
    assert!(!c.validate_key(id1, &Key::Numeric(2)));
}

#[test]
fn alive_interval_overflow_saturates_without_panic() {
    let mut c = Coordinator::new();
    // A huge interval is stored as-is (no clamp), but the `interval * 2000`
    // liveness math is saturating so it cannot overflow-panic. This only
    // verifies "no panic" — it does not assert any clamping of the stored value.
    c.set_alive(RobotId::new(1), u64::MAX, 0);
    // The far-future query does not panic (saturating math).
    let _ = c.robot_active_at(RobotId::new(1), u64::MAX);
    let _ = c.inactive_robots_at(u64::MAX);
    // Well within the window the robot is active.
    assert!(c.robot_active_at(RobotId::new(1), 1_000));
    assert!(c.inactive_robots_at(1_000).is_empty());
}
