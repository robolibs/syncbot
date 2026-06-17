//! C ABI for syncbot.
//!
//! Conventions: opaque Box-backed handles (free with the matching
//! *_free); fallible calls return bool/int with the reason in the
//! thread-local sb_last_error(); complex inputs/outputs use JSON C strings.
//!
//! `include/syncbot.h` is generated from this file by cbindgen.

// extern "C" fns take raw pointers from C and deref them by design.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_int};
use std::path::Path;
use std::ptr;
use std::sync::Arc;

use uuid::Uuid;

use crate::claim::{ClaimManager, ClaimRequest, Lease};
use crate::coordinator::{
    ArbitrationContext, ArbitrationDecision, Coordinator, ScheduleDecision, arbitrate_right_of_way,
};
use crate::core::ids::{ClaimId, LeaseId, RobotId};
use crate::index::WorkspaceIndex;
use crate::policy::{
    parse_zone_policy, validate_edge_traffic_properties, validate_zone_traffic_properties,
};
use crate::robot::{RobotProgressState, RobotState};
use crate::route::{RoutePlan, plan_route};

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(msg: impl Into<String>) {
    let s = msg.into();
    let cs = CString::new(s).unwrap_or_else(|_| CString::new("invalid utf8 in error").unwrap());
    LAST_ERROR.with(|e| *e.borrow_mut() = Some(cs));
}

fn clear_last_error() {
    LAST_ERROR.with(|e| *e.borrow_mut() = None);
}

/// Returns a pointer to the last error message, or NULL if there is none.
/// The pointer is valid until the next FFI call on this thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_last_error() -> *const c_char {
    LAST_ERROR.with(|e| match e.borrow().as_ref() {
        None => ptr::null(),
        Some(cs) => cs.as_ptr(),
    })
}

/// Crate version, as a static NUL-terminated string.
#[unsafe(no_mangle)]
pub extern "C" fn sb_version() -> *const c_char {
    static VERSION: &[u8] = b"0.0.2\0";
    VERSION.as_ptr() as *const c_char
}

/// Free a C string returned by any `sb_*` function that documents it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_string_free(s: *mut c_char) {
    if !s.is_null() {
        unsafe {
            let _ = CString::from_raw(s);
        }
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

unsafe fn cstr_to_str<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(p) }.to_str().ok()
}

fn to_c_string(s: String) -> *mut c_char {
    match CString::new(s) {
        Ok(cs) => cs.into_raw(),
        Err(_) => {
            set_last_error("internal: nul byte in returned string");
            ptr::null_mut()
        }
    }
}

fn json_out<T: serde::Serialize + ?Sized>(value: &T) -> *mut c_char {
    match serde_json::to_string(value) {
        Ok(s) => to_c_string(s),
        Err(e) => {
            set_last_error(format!("json serialize failed: {e}"));
            ptr::null_mut()
        }
    }
}

unsafe fn json_in<T: for<'de> serde::Deserialize<'de>>(p: *const c_char) -> Option<T> {
    let s = unsafe { cstr_to_str(p) }?;
    match serde_json::from_str(s) {
        Ok(v) => Some(v),
        Err(e) => {
            set_last_error(format!("json parse failed: {e}"));
            None
        }
    }
}

unsafe fn parse_uuid(p: *const c_char) -> Option<Uuid> {
    let s = unsafe { cstr_to_str(p) }?;
    match Uuid::parse_str(s) {
        Ok(u) => Some(u),
        Err(e) => {
            set_last_error(format!("invalid uuid: {e}"));
            None
        }
    }
}

// ---------------------------------------------------------------------------
// traffic value parsers
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_parse_traffic_bool(value: *const c_char) -> c_int {
    clear_last_error();
    let Some(s) = (unsafe { cstr_to_str(value) }) else {
        set_last_error("null or non-utf8 input");
        return -1;
    };
    match crate::policy::parse_traffic_bool(s) {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(e) => {
            set_last_error(e.to_string());
            -1
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_parse_traffic_u64(value: *const c_char, out: *mut u64) -> c_int {
    clear_last_error();
    if out.is_null() {
        set_last_error("null out pointer");
        return -1;
    }
    let Some(s) = (unsafe { cstr_to_str(value) }) else {
        set_last_error("null or non-utf8 input");
        return -1;
    };
    match crate::policy::parse_traffic_u64(s) {
        Ok(v) => {
            unsafe {
                *out = v;
            }
            0
        }
        Err(e) => {
            set_last_error(e.to_string());
            -1
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_parse_traffic_f64(value: *const c_char, out: *mut f64) -> c_int {
    clear_last_error();
    if out.is_null() {
        set_last_error("null out pointer");
        return -1;
    }
    let Some(s) = (unsafe { cstr_to_str(value) }) else {
        set_last_error("null or non-utf8 input");
        return -1;
    };
    match crate::policy::parse_traffic_f64(s) {
        Ok(v) => {
            unsafe {
                *out = v;
            }
            0
        }
        Err(e) => {
            set_last_error(e.to_string());
            -1
        }
    }
}

/// Parse a zone-policy `properties` object (JSON map of strings) and return
/// the parsed `ZonePolicy` as a JSON string. Caller frees with
/// `sb_string_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_parse_zone_policy(properties_json: *const c_char) -> *mut c_char {
    clear_last_error();
    let Some(props) =
        (unsafe { json_in::<std::collections::BTreeMap<String, String>>(properties_json) })
    else {
        return ptr::null_mut();
    };
    let policy = parse_zone_policy(&props);
    json_out(&policy)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_validate_zone_traffic(properties_json: *const c_char) -> *mut c_char {
    clear_last_error();
    let Some(props) =
        (unsafe { json_in::<std::collections::BTreeMap<String, String>>(properties_json) })
    else {
        return ptr::null_mut();
    };
    json_out(&validate_zone_traffic_properties(&props))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_validate_edge_traffic(properties_json: *const c_char) -> *mut c_char {
    clear_last_error();
    let Some(props) =
        (unsafe { json_in::<std::collections::BTreeMap<String, String>>(properties_json) })
    else {
        return ptr::null_mut();
    };
    json_out(&validate_edge_traffic_properties(&props))
}

// ---------------------------------------------------------------------------
// Workspace handle
// ---------------------------------------------------------------------------

pub struct SbWorkspace {
    inner: Arc<zoneout::Workspace>,
}

/// Load a workspace from a directory path on disk.
/// Returns NULL on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_workspace_load(path: *const c_char) -> *mut SbWorkspace {
    clear_last_error();
    let Some(s) = (unsafe { cstr_to_str(path) }) else {
        set_last_error("null or non-utf8 path");
        return ptr::null_mut();
    };
    match zoneout::Workspace::load(Path::new(s)) {
        Ok(ws) => Box::into_raw(Box::new(SbWorkspace {
            inner: Arc::new(ws),
        })),
        Err(e) => {
            set_last_error(format!("workspace load: {e}"));
            ptr::null_mut()
        }
    }
}

/// Free a workspace handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_workspace_free(ws: *mut SbWorkspace) {
    if !ws.is_null() {
        unsafe {
            drop(Box::from_raw(ws));
        }
    }
}

// ---------------------------------------------------------------------------
// WorkspaceIndex handle
// ---------------------------------------------------------------------------

pub struct SbWorkspaceIndex {
    inner: Arc<WorkspaceIndex>,
}

/// Build a `WorkspaceIndex` from a workspace handle. The index keeps a
/// shared reference to the workspace via Arc — the workspace handle remains
/// valid to free after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_workspace_index_new(ws: *const SbWorkspace) -> *mut SbWorkspaceIndex {
    clear_last_error();
    if ws.is_null() {
        set_last_error("null workspace");
        return ptr::null_mut();
    }
    let workspace = unsafe { (*ws).inner.clone() };
    let idx = WorkspaceIndex::new(workspace);
    Box::into_raw(Box::new(SbWorkspaceIndex {
        inner: Arc::new(idx),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_workspace_index_free(idx: *mut SbWorkspaceIndex) {
    if !idx.is_null() {
        unsafe {
            drop(Box::from_raw(idx));
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_workspace_index_refresh(idx: *mut SbWorkspaceIndex) -> c_int {
    clear_last_error();
    if idx.is_null() {
        set_last_error("null index");
        return -1;
    }
    // We need a mutable borrow on the inner WorkspaceIndex. Try Arc::get_mut.
    let entry = unsafe { &mut (*idx).inner };
    match Arc::get_mut(entry) {
        Some(inner) => {
            inner.refresh();
            0
        }
        None => {
            set_last_error("workspace index has outstanding shared owners");
            -1
        }
    }
}

/// Returns the JSON list of `ValidationIssue`s. Caller frees with
/// `sb_string_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_workspace_index_validation_issues(
    idx: *const SbWorkspaceIndex,
) -> *mut c_char {
    clear_last_error();
    if idx.is_null() {
        set_last_error("null index");
        return ptr::null_mut();
    }
    let inner = unsafe { &(*idx).inner };
    json_out(&inner.validation_issues())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_workspace_index_is_valid(idx: *const SbWorkspaceIndex) -> c_int {
    clear_last_error();
    if idx.is_null() {
        set_last_error("null index");
        return -1;
    }
    let inner = unsafe { &(*idx).inner };
    if inner.is_valid() { 1 } else { 0 }
}

/// Returns the root zone UUID (as a hyphenated string), or NULL on error.
/// Caller frees with `sb_string_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_workspace_index_root_zone_id(
    idx: *const SbWorkspaceIndex,
) -> *mut c_char {
    clear_last_error();
    if idx.is_null() {
        set_last_error("null index");
        return ptr::null_mut();
    }
    let inner = unsafe { &(*idx).inner };
    match inner.root_zone_id() {
        Some(u) => to_c_string(u.to_string()),
        None => {
            set_last_error("workspace has no root zone");
            ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// route planning
// ---------------------------------------------------------------------------

/// Plan a route between two node UUIDs. Returns a JSON-encoded
/// `RoutePlanningResult` (which has `search`, `plan`, `failure` fields).
/// Caller frees with `sb_string_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_plan_route(
    idx: *const SbWorkspaceIndex,
    start_node_id: *const c_char,
    goal_node_id: *const c_char,
    use_penalties: c_int,
) -> *mut c_char {
    clear_last_error();
    if idx.is_null() {
        set_last_error("null index");
        return ptr::null_mut();
    }
    let Some(start) = (unsafe { parse_uuid(start_node_id) }) else {
        return ptr::null_mut();
    };
    let Some(goal) = (unsafe { parse_uuid(goal_node_id) }) else {
        return ptr::null_mut();
    };
    let inner = unsafe { &(*idx).inner };
    let result = plan_route(inner, start, goal, use_penalties != 0);
    // RoutePlanningResult isn't Serialize (search has HashMaps); package the
    // useful fields into a JSON object directly.
    #[derive(serde::Serialize)]
    struct Out<'a> {
        found: bool,
        plan: &'a Option<RoutePlan>,
        failure: &'a Option<crate::route::RouteFailure>,
        distance: f64,
    }
    let out = Out {
        found: result.search.found,
        plan: &result.plan,
        failure: &result.failure,
        distance: result.search.distance,
    };
    json_out(&out)
}

// ---------------------------------------------------------------------------
// ClaimManager handle
// ---------------------------------------------------------------------------

pub struct SbClaimManager {
    inner: ClaimManager,
}

#[unsafe(no_mangle)]
pub extern "C" fn sb_claim_manager_new() -> *mut SbClaimManager {
    Box::into_raw(Box::new(SbClaimManager {
        inner: ClaimManager::new(),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_claim_manager_with_index(
    idx: *const SbWorkspaceIndex,
) -> *mut SbClaimManager {
    clear_last_error();
    if idx.is_null() {
        set_last_error("null index");
        return ptr::null_mut();
    }
    let arc = unsafe { (*idx).inner.clone() };
    Box::into_raw(Box::new(SbClaimManager {
        inner: ClaimManager::with_index(arc),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_claim_manager_free(mgr: *mut SbClaimManager) {
    if !mgr.is_null() {
        unsafe {
            drop(Box::from_raw(mgr));
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_claim_manager_request_count(mgr: *const SbClaimManager) -> u64 {
    if mgr.is_null() {
        return 0;
    }
    unsafe { (*mgr).inner.request_count() as u64 }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_claim_manager_lease_count(mgr: *const SbClaimManager) -> u64 {
    if mgr.is_null() {
        return 0;
    }
    unsafe { (*mgr).inner.lease_count() as u64 }
}

/// Add a claim request (JSON-encoded `ClaimRequest`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_claim_manager_add_request(
    mgr: *mut SbClaimManager,
    request_json: *const c_char,
) -> c_int {
    clear_last_error();
    if mgr.is_null() {
        set_last_error("null manager");
        return -1;
    }
    let Some(req) = (unsafe { json_in::<ClaimRequest>(request_json) }) else {
        return -1;
    };
    unsafe {
        (*mgr).inner.add_request(req);
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_claim_manager_remove_request(
    mgr: *mut SbClaimManager,
    claim_id: u64,
) -> c_int {
    clear_last_error();
    if mgr.is_null() {
        set_last_error("null manager");
        return -1;
    }
    if unsafe { (*mgr).inner.remove_request(ClaimId::new(claim_id)) } {
        0
    } else {
        -1
    }
}

/// Add a lease (JSON-encoded `Lease`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_claim_manager_add_lease(
    mgr: *mut SbClaimManager,
    lease_json: *const c_char,
) -> c_int {
    clear_last_error();
    if mgr.is_null() {
        set_last_error("null manager");
        return -1;
    }
    let Some(lease) = (unsafe { json_in::<Lease>(lease_json) }) else {
        return -1;
    };
    unsafe {
        (*mgr).inner.add_lease(lease);
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_claim_manager_release_lease(
    mgr: *mut SbClaimManager,
    lease_id: u64,
    released_at_tick_or_neg1: i64,
) -> c_int {
    clear_last_error();
    if mgr.is_null() {
        set_last_error("null manager");
        return -1;
    }
    let tick = if released_at_tick_or_neg1 < 0 {
        None
    } else {
        Some(released_at_tick_or_neg1 as u64)
    };
    if unsafe { (*mgr).inner.release_lease(LeaseId::new(lease_id), tick) } {
        0
    } else {
        -1
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_claim_manager_expire_leases(
    mgr: *mut SbClaimManager,
    current_tick: u64,
) -> u64 {
    if mgr.is_null() {
        return 0;
    }
    unsafe { (*mgr).inner.expire_leases(current_tick) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_claim_manager_refresh_lease(
    mgr: *mut SbClaimManager,
    lease_id: u64,
    refreshed_at_tick: u64,
    expires_at_tick_or_neg1: i64,
) -> c_int {
    clear_last_error();
    if mgr.is_null() {
        set_last_error("null manager");
        return -1;
    }
    let exp = if expires_at_tick_or_neg1 < 0 {
        None
    } else {
        Some(expires_at_tick_or_neg1 as u64)
    };
    if unsafe {
        (*mgr)
            .inner
            .refresh_lease(LeaseId::new(lease_id), refreshed_at_tick, exp)
    } {
        0
    } else {
        -1
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_claim_manager_revoke_lease(
    mgr: *mut SbClaimManager,
    lease_id: u64,
    reason: *const c_char,
    revoked_at_tick: u64,
) -> c_int {
    clear_last_error();
    if mgr.is_null() {
        set_last_error("null manager");
        return -1;
    }
    let reason_str = unsafe { cstr_to_str(reason) }.unwrap_or("").to_string();
    if unsafe {
        (*mgr)
            .inner
            .revoke_lease(LeaseId::new(lease_id), reason_str, revoked_at_tick)
    } {
        0
    } else {
        -1
    }
}

/// Evaluate a request. Returns JSON-encoded `ClaimEvaluation`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_claim_manager_evaluate(
    mgr: *const SbClaimManager,
    request_json: *const c_char,
) -> *mut c_char {
    clear_last_error();
    if mgr.is_null() {
        set_last_error("null manager");
        return ptr::null_mut();
    }
    let Some(req) = (unsafe { json_in::<ClaimRequest>(request_json) }) else {
        return ptr::null_mut();
    };
    let eval = unsafe { (*mgr).inner.evaluate_request(&req) };
    json_out(&eval)
}

/// Returns JSON list of all active requests.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_claim_manager_requests(mgr: *const SbClaimManager) -> *mut c_char {
    clear_last_error();
    if mgr.is_null() {
        set_last_error("null manager");
        return ptr::null_mut();
    }
    json_out(unsafe { (*mgr).inner.requests() })
}

/// Returns JSON list of all active leases.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_claim_manager_leases(mgr: *const SbClaimManager) -> *mut c_char {
    clear_last_error();
    if mgr.is_null() {
        set_last_error("null manager");
        return ptr::null_mut();
    }
    json_out(unsafe { (*mgr).inner.leases() })
}

// ---------------------------------------------------------------------------
// Coordinator handle
// ---------------------------------------------------------------------------

pub struct SbCoordinator {
    inner: Coordinator,
}

#[unsafe(no_mangle)]
pub extern "C" fn sb_coordinator_new() -> *mut SbCoordinator {
    Box::into_raw(Box::new(SbCoordinator {
        inner: Coordinator::new(),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_coordinator_with_index(
    idx: *const SbWorkspaceIndex,
) -> *mut SbCoordinator {
    clear_last_error();
    if idx.is_null() {
        set_last_error("null index");
        return ptr::null_mut();
    }
    let arc = unsafe { (*idx).inner.clone() };
    Box::into_raw(Box::new(SbCoordinator {
        inner: Coordinator::with_index(arc),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_coordinator_free(c: *mut SbCoordinator) {
    if !c.is_null() {
        unsafe {
            drop(Box::from_raw(c));
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_coordinator_robot_count(c: *const SbCoordinator) -> u64 {
    if c.is_null() {
        return 0;
    }
    unsafe { (*c).inner.robot_count() as u64 }
}

/// Register a robot from a JSON-encoded `RobotState`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_coordinator_register_robot(
    c: *mut SbCoordinator,
    state_json: *const c_char,
) -> c_int {
    clear_last_error();
    if c.is_null() {
        set_last_error("null coordinator");
        return -1;
    }
    let Some(state) = (unsafe { json_in::<RobotState>(state_json) }) else {
        return -1;
    };
    unsafe {
        (*c).inner.register_robot(state);
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_coordinator_unregister_robot(
    c: *mut SbCoordinator,
    robot_id: u64,
) -> c_int {
    clear_last_error();
    if c.is_null() {
        set_last_error("null coordinator");
        return -1;
    }
    if unsafe { (*c).inner.unregister_robot(RobotId::new(robot_id)) } {
        0
    } else {
        -1
    }
}

/// JSON-encoded `RobotState` for the given robot, or NULL if unknown.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_coordinator_robot_state(
    c: *const SbCoordinator,
    robot_id: u64,
) -> *mut c_char {
    clear_last_error();
    if c.is_null() {
        set_last_error("null coordinator");
        return ptr::null_mut();
    }
    match unsafe { (*c).inner.find_robot_state(RobotId::new(robot_id)) } {
        None => {
            set_last_error("robot not registered");
            ptr::null_mut()
        }
        Some(s) => json_out(s),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_coordinator_assign_route_plan(
    c: *mut SbCoordinator,
    robot_id: u64,
    route_plan_json: *const c_char,
    horizon: u64,
    updated_at_tick: u64,
) -> c_int {
    clear_last_error();
    if c.is_null() {
        set_last_error("null coordinator");
        return -1;
    }
    let Some(plan) = (unsafe { json_in::<RoutePlan>(route_plan_json) }) else {
        return -1;
    };
    if unsafe {
        (*c).inner
            .assign_route_plan(RobotId::new(robot_id), plan, horizon, updated_at_tick)
    } {
        0
    } else {
        -1
    }
}

/// Schedule a robot's route. Returns JSON-encoded `ScheduleDecision`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_coordinator_schedule_robot_route(
    c: *mut SbCoordinator,
    robot_id: u64,
    claim_id: u64,
    start_tick: u64,
    ticks_per_cost_unit: f64,
    access_mode_shared: c_int,
) -> *mut c_char {
    clear_last_error();
    if c.is_null() {
        set_last_error("null coordinator");
        return ptr::null_mut();
    }
    let access = if access_mode_shared != 0 {
        crate::claim::ClaimAccessMode::Shared
    } else {
        crate::claim::ClaimAccessMode::Exclusive
    };
    let decision: ScheduleDecision = unsafe {
        (*c).inner.schedule_robot_route(
            RobotId::new(robot_id),
            ClaimId::new(claim_id),
            start_tick,
            ticks_per_cost_unit,
            access,
        )
    };
    json_out(&decision)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_coordinator_handle_missed_schedule_slot(
    c: *mut SbCoordinator,
    robot_id: u64,
    current_tick: u64,
    grace_ticks: u64,
) -> c_int {
    clear_last_error();
    if c.is_null() {
        set_last_error("null coordinator");
        return -1;
    }
    let r = unsafe {
        (*c).inner
            .handle_missed_schedule_slot(RobotId::new(robot_id), current_tick, grace_ticks)
    };
    if r { 1 } else { 0 }
}

// ---------------------------------------------------------------------------
// arbitration (POD context)
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct SbArbitrationContext {
    pub self_priority: f64,
    pub other_priority: f64,
    pub self_holds_lease: u8,
    pub other_holds_lease: u8,
    pub self_is_emergency: u8,
    pub other_is_emergency: u8,
    pub self_state: u8,
    pub other_state: u8,
    pub self_wait_ticks: u64,
    pub other_wait_ticks: u64,
    pub self_remaining_steps: u64,
    pub other_remaining_steps: u64,
}

#[repr(C)]
pub enum SbArbitrationDecision {
    Proceed = 0,
    Yield = 1,
    Replan = 2,
}

fn progress_state_from_u8(value: u8) -> RobotProgressState {
    match value {
        0 => RobotProgressState::Idle,
        1 => RobotProgressState::FollowingRoute,
        2 => RobotProgressState::Waiting,
        3 => RobotProgressState::Queued,
        4 => RobotProgressState::Blocked,
        5 => RobotProgressState::Replanning,
        _ => RobotProgressState::Idle,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_arbitrate_right_of_way(ctx: *const SbArbitrationContext) -> c_int {
    if ctx.is_null() {
        return SbArbitrationDecision::Replan as c_int;
    }
    let c = unsafe { &*ctx };
    let context = ArbitrationContext {
        self_priority: c.self_priority,
        other_priority: c.other_priority,
        self_holds_lease: c.self_holds_lease != 0,
        other_holds_lease: c.other_holds_lease != 0,
        self_is_emergency: c.self_is_emergency != 0,
        other_is_emergency: c.other_is_emergency != 0,
        self_state: progress_state_from_u8(c.self_state),
        other_state: progress_state_from_u8(c.other_state),
        self_wait_ticks: c.self_wait_ticks,
        other_wait_ticks: c.other_wait_ticks,
        self_remaining_steps: c.self_remaining_steps,
        other_remaining_steps: c.other_remaining_steps,
    };
    match arbitrate_right_of_way(&context) {
        ArbitrationDecision::Proceed => SbArbitrationDecision::Proceed as c_int,
        ArbitrationDecision::Yield => SbArbitrationDecision::Yield as c_int,
        ArbitrationDecision::Replan => SbArbitrationDecision::Replan as c_int,
    }
}

// ---------------------------------------------------------------------------
// VDA mapping
// ---------------------------------------------------------------------------

/// Map a JSON-encoded `RoutePlan` to a JSON-encoded VDA `Order`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_vda_order_from_route(route_plan_json: *const c_char) -> *mut c_char {
    clear_last_error();
    let Some(plan) = (unsafe { json_in::<RoutePlan>(route_plan_json) }) else {
        return ptr::null_mut();
    };
    let order = crate::vda::map_route_plan(&plan);
    json_out(&order)
}

/// Map a JSON-encoded `RobotState` to a JSON-encoded VDA `State`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sb_vda_state_from_robot(robot_state_json: *const c_char) -> *mut c_char {
    clear_last_error();
    let Some(state) = (unsafe { json_in::<RobotState>(robot_state_json) }) else {
        return ptr::null_mut();
    };
    let s = crate::vda::map_robot_state(&state);
    json_out(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_string() {
        let p = sb_version();
        let s = unsafe { CStr::from_ptr(p) }.to_str().unwrap();
        assert_eq!(s, "0.0.2");
    }

    #[test]
    fn parse_bool_round_trip() {
        let s = CString::new("yes").unwrap();
        assert_eq!(unsafe { sb_parse_traffic_bool(s.as_ptr()) }, 1);
        let s = CString::new("nope").unwrap();
        assert_eq!(unsafe { sb_parse_traffic_bool(s.as_ptr()) }, -1);
        let err = unsafe { sb_last_error() };
        assert!(!err.is_null());
    }

    #[test]
    fn arbitration_emergency_proceeds() {
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
        let d = unsafe { sb_arbitrate_right_of_way(&ctx as *const _) };
        assert_eq!(d, SbArbitrationDecision::Proceed as c_int);
    }

    #[test]
    fn claim_manager_handle_lifecycle() {
        let mgr = sb_claim_manager_new();
        assert!(!mgr.is_null());
        assert_eq!(unsafe { sb_claim_manager_request_count(mgr) }, 0);

        let req_json =
            CString::new(serde_json::to_string(&ClaimRequest::default()).unwrap()).unwrap();
        // Empty targets — but add_request doesn't validate; evaluate does.
        assert_eq!(
            unsafe { sb_claim_manager_add_request(mgr, req_json.as_ptr()) },
            0
        );
        assert_eq!(unsafe { sb_claim_manager_request_count(mgr) }, 1);

        let eval_json = unsafe { sb_claim_manager_evaluate(mgr, req_json.as_ptr()) };
        assert!(!eval_json.is_null());
        unsafe {
            sb_string_free(eval_json);
        }
        unsafe {
            sb_claim_manager_free(mgr);
        }
    }

    #[test]
    fn parse_zone_policy_roundtrip() {
        let props = CString::new(r#"{"traffic.policy":"exclusive"}"#).unwrap();
        let out = unsafe { sb_parse_zone_policy(props.as_ptr()) };
        assert!(!out.is_null());
        let s = unsafe { CStr::from_ptr(out) }.to_str().unwrap();
        assert!(s.contains("ExclusiveAccess"));
        unsafe {
            sb_string_free(out);
        }
    }
}
