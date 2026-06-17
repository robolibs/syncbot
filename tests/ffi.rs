//! End-to-end FFI test exercising opaque handles + JSON round-trips.
//!
//! Drives the C ABI from Rust by calling `extern "C"` symbols directly. This
//! covers the JSON marshaling and the handle lifecycle that a C consumer
//! would use.

use std::ffi::{CStr, CString};

use serde_json::json;
use syncbot::ffi::*;

unsafe fn cstr_into_owned(p: *mut std::os::raw::c_char) -> String {
    assert!(!p.is_null(), "expected non-null C string");
    let s = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_owned();
    unsafe {
        sb_string_free(p);
    }
    s
}

#[test]
fn version_and_traffic_helpers_round_trip() {
    let p = sb_version();
    assert_eq!(unsafe { CStr::from_ptr(p) }.to_str().unwrap(), env!("CARGO_PKG_VERSION"));

    let yes = CString::new("yes").unwrap();
    assert_eq!(unsafe { sb_parse_traffic_bool(yes.as_ptr()) }, 1);

    let mut out: u64 = 0;
    let cap = CString::new("3").unwrap();
    assert_eq!(unsafe { sb_parse_traffic_u64(cap.as_ptr(), &mut out) }, 0);
    assert_eq!(out, 3);
}

#[test]
fn parse_zone_policy_via_ffi() {
    let props = json!({"traffic.policy": "exclusive"}).to_string();
    let cs = CString::new(props).unwrap();
    let json_str = unsafe { cstr_into_owned(sb_parse_zone_policy(cs.as_ptr())) };
    let v: serde_json::Value = serde_json::from_str(&json_str).unwrap();
    assert_eq!(v["kind"], "ExclusiveAccess");
    assert_eq!(v["capacity"], 1);
}

#[test]
fn validate_zone_traffic_returns_issues() {
    let props = json!({"traffic.bogus": "x"}).to_string();
    let cs = CString::new(props).unwrap();
    let json_str = unsafe { cstr_into_owned(sb_validate_zone_traffic(cs.as_ptr())) };
    let issues: serde_json::Value = serde_json::from_str(&json_str).unwrap();
    let arr = issues.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["severity"], "Warning");
}

#[test]
fn claim_manager_full_lifecycle_via_ffi() {
    let mgr = sb_claim_manager_new();
    assert!(!mgr.is_null());
    assert_eq!(unsafe { sb_claim_manager_request_count(mgr) }, 0);

    let req = json!({
        "id": 1, "robot_id": 1, "mission_id": 0,
        "access_mode": "Exclusive", "priority": 0,
        "requested_at_tick": null,
        "window": { "start_tick": null, "end_tick": null },
        "targets": [{"kind": "Zone", "resource_id": "00000000-0000-0000-0000-000000000001"}],
    })
    .to_string();
    let cs = CString::new(req).unwrap();
    assert_eq!(unsafe { sb_claim_manager_add_request(mgr, cs.as_ptr()) }, 0);
    assert_eq!(unsafe { sb_claim_manager_request_count(mgr) }, 1);

    let eval_json = unsafe { cstr_into_owned(sb_claim_manager_evaluate(mgr, cs.as_ptr())) };
    let v: serde_json::Value = serde_json::from_str(&eval_json).unwrap();
    // Without an index bound, the eval skips index-based checks; same-id
    // request shouldn't conflict with itself, so it grants.
    assert_eq!(v["decision"], "Grant");

    assert!(unsafe { sb_claim_manager_remove_request(mgr, 1) } == 0);
    assert_eq!(unsafe { sb_claim_manager_request_count(mgr) }, 0);

    unsafe {
        sb_claim_manager_free(mgr);
    }
}

#[test]
fn coordinator_handle_lifecycle_via_ffi() {
    let c = sb_coordinator_new();
    assert!(!c.is_null());
    assert_eq!(unsafe { sb_coordinator_robot_count(c) }, 0);

    let state = json!({
        "robot_id": 7, "mission_id": 0,
        "current_node_id": null, "current_edge_id": null,
        "route_plan": null,
        "pending_claim_ids": [], "active_lease_ids": [],
        "progress_state": "Idle",
        "next_route_step_index": 0,
        "hold_reason": null, "last_claim_tick": null,
        "scheduled_start_tick": null, "reserved_until_tick": null,
        "wait_ticks": 0, "needs_replan": false, "horizon": 0,
        "updated_at_tick": 0,
    })
    .to_string();
    let cs = CString::new(state).unwrap();
    assert_eq!(unsafe { sb_coordinator_register_robot(c, cs.as_ptr()) }, 0);
    assert_eq!(unsafe { sb_coordinator_robot_count(c) }, 1);

    let s_json = unsafe { cstr_into_owned(sb_coordinator_robot_state(c, 7)) };
    let v: serde_json::Value = serde_json::from_str(&s_json).unwrap();
    assert_eq!(v["robot_id"], 7);

    assert_eq!(unsafe { sb_coordinator_unregister_robot(c, 7) }, 0);
    assert_eq!(unsafe { sb_coordinator_robot_count(c) }, 0);

    unsafe {
        sb_coordinator_free(c);
    }
}

#[test]
fn arbitration_emergency_proceeds_via_ffi() {
    let ctx = SbArbitrationContext {
        self_priority: 0.0,
        other_priority: 0.0,
        self_holds_lease: 0,
        other_holds_lease: 0,
        self_is_emergency: 1,
        other_is_emergency: 0,
        self_state: 0,
        other_state: 0,
        self_wait_ticks: 0,
        other_wait_ticks: 0,
        self_remaining_steps: 0,
        other_remaining_steps: 0,
    };
    assert_eq!(unsafe { sb_arbitrate_right_of_way(&ctx as *const _) }, 0);
}

#[test]
fn vda_order_from_route_via_ffi() {
    // Minimal RoutePlan with a 2-node route.
    let plan = json!({
        "start_node_id": "00000000-0000-0000-0000-000000000001",
        "goal_node_id": "00000000-0000-0000-0000-000000000002",
        "steps": [
            {"node_id": "00000000-0000-0000-0000-000000000001",
             "incoming_edge_id": null, "step_cost": 0.0, "cumulative_cost": 0.0},
            {"node_id": "00000000-0000-0000-0000-000000000002",
             "incoming_edge_id": "00000000-0000-0000-0000-0000000000aa",
             "step_cost": 1.0, "cumulative_cost": 1.0},
        ],
        "traversed_node_ids": [
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000002",
        ],
        "traversed_edge_ids": ["00000000-0000-0000-0000-0000000000aa"],
        "traversed_zone_ids": [],
        "traversed_node_zone_ids": [[], []],
        "traversed_edge_zone_ids": [[]],
        "total_cost": 1.0,
    })
    .to_string();
    let cs = CString::new(plan).unwrap();
    let order_json = unsafe { cstr_into_owned(sb_vda_order_from_route(cs.as_ptr())) };
    let v: serde_json::Value = serde_json::from_str(&order_json).unwrap();
    assert_eq!(v["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(v["edges"].as_array().unwrap().len(), 1);
    assert_eq!(v["version"], "3.0.0");
}
