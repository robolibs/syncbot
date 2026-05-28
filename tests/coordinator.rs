//! Coordinator integration tests.

use timenav::{
    ArbitrationContext, ArbitrationDecision, RobotProgressState, RobotState,
    arbitrate_right_of_way, robot_missed_schedule_slot,
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
