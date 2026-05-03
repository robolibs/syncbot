//! PyO3 bindings — full coverage of the public timenav surface.
//!
//! Pattern: every Rust struct that crosses the boundary becomes either
//! - a `#[pyclass]` for stateful types (`Workspace`, `WorkspaceIndex`,
//!   `ClaimManager`, `Coordinator`, `RoutePlan`), or
//! - a JSON dict / typed tuple for plain data types (`ClaimRequest`,
//!   `Lease`, `RobotState`, `ClaimEvaluation`, `ScheduleDecision`).
//!
//! For data types we use `serde_json::Value` round-trips, which gives us
//! ergonomic Python-side `dict` / `list` access without per-field PyO3
//! boilerplate.

use std::path::Path;
use std::sync::Arc;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::claim::{
    ClaimAccessMode, ClaimManager, ClaimRequest, Lease,
};
use crate::coordinator::{
    ArbitrationContext, ArbitrationDecision, Coordinator,
    arbitrate_right_of_way,
};
use crate::core::ids::{ClaimId, LeaseId, RobotId};
use crate::index::WorkspaceIndex;
use crate::policy::{
    self, ZonePolicyKind, parse_traffic_bool, parse_traffic_f64,
    parse_traffic_string, parse_traffic_u64,
};
use crate::robot::{RobotProgressState, RobotState};
use crate::route::{RoutePlan, plan_route};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn err_value(e: impl ToString) -> PyErr { PyValueError::new_err(e.to_string()) }
fn err_runtime(e: impl ToString) -> PyErr { PyRuntimeError::new_err(e.to_string()) }

/// Convert a `serde_json::Value` to a Python object.
fn json_to_py<'py>(py: Python<'py>, v: &serde_json::Value) -> PyResult<Bound<'py, PyAny>> {
    Ok(match v {
        serde_json::Value::Null => py.None().into_bound(py),
        serde_json::Value::Bool(b) => b.into_pyobject(py)?.to_owned().into_any(),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() { i.into_pyobject(py)?.into_any() }
            else if let Some(u) = n.as_u64() { u.into_pyobject(py)?.into_any() }
            else { n.as_f64().unwrap().into_pyobject(py)?.to_owned().into_any() }
        }
        serde_json::Value::String(s) => s.into_pyobject(py)?.to_owned().into_any(),
        serde_json::Value::Array(arr) => {
            let list = PyList::empty(py);
            for item in arr { list.append(json_to_py(py, item)?)?; }
            list.into_any()
        }
        serde_json::Value::Object(map) => {
            let dict = PyDict::new(py);
            for (k, v) in map { dict.set_item(k, json_to_py(py, v)?)?; }
            dict.into_any()
        }
    })
}

/// Convert a Python object to a `serde_json::Value`.
fn py_to_json(value: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
    if value.is_none() { return Ok(serde_json::Value::Null); }
    if let Ok(b) = value.extract::<bool>() { return Ok(serde_json::Value::Bool(b)); }
    if let Ok(i) = value.extract::<i64>() { return Ok(serde_json::Value::Number(i.into())); }
    if let Ok(u) = value.extract::<u64>() { return Ok(serde_json::Value::Number(u.into())); }
    if let Ok(f) = value.extract::<f64>() {
        return Ok(serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null));
    }
    if let Ok(s) = value.extract::<String>() { return Ok(serde_json::Value::String(s)); }
    if let Ok(list) = value.downcast::<PyList>() {
        let mut arr = Vec::with_capacity(list.len());
        for item in list.iter() { arr.push(py_to_json(&item)?); }
        return Ok(serde_json::Value::Array(arr));
    }
    if let Ok(dict) = value.downcast::<PyDict>() {
        let mut map = serde_json::Map::new();
        for (k, v) in dict.iter() {
            let key: String = k.extract()?;
            map.insert(key, py_to_json(&v)?);
        }
        return Ok(serde_json::Value::Object(map));
    }
    Err(PyValueError::new_err(format!(
        "unsupported python value for JSON conversion: {value}"
    )))
}

fn obj_to<T: for<'de> serde::Deserialize<'de>>(value: &Bound<'_, PyAny>) -> PyResult<T> {
    let json = py_to_json(value)?;
    serde_json::from_value(json).map_err(err_value)
}

fn obj_from<'py, T: serde::Serialize>(py: Python<'py>, value: &T) -> PyResult<Bound<'py, PyAny>> {
    let v = serde_json::to_value(value).map_err(err_value)?;
    json_to_py(py, &v)
}

fn parse_progress_state(s: &str) -> RobotProgressState {
    match s {
        "idle" => RobotProgressState::Idle,
        "following_route" => RobotProgressState::FollowingRoute,
        "waiting" => RobotProgressState::Waiting,
        "queued" => RobotProgressState::Queued,
        "blocked" => RobotProgressState::Blocked,
        "replanning" => RobotProgressState::Replanning,
        _ => RobotProgressState::Idle,
    }
}

fn parse_access_mode(s: &str) -> ClaimAccessMode {
    match s {
        "shared" => ClaimAccessMode::Shared,
        _ => ClaimAccessMode::Exclusive,
    }
}

// ---------------------------------------------------------------------------
// version + traffic parsers
// ---------------------------------------------------------------------------

#[pyfunction]
fn version() -> &'static str { crate::version() }

#[pyfunction(name = "parse_traffic_bool")]
fn py_parse_traffic_bool(value: &str) -> PyResult<bool> {
    parse_traffic_bool(value).map_err(err_value)
}

#[pyfunction(name = "parse_traffic_u64")]
fn py_parse_traffic_u64(value: &str) -> PyResult<u64> {
    parse_traffic_u64(value).map_err(err_value)
}

#[pyfunction(name = "parse_traffic_f64")]
fn py_parse_traffic_f64(value: &str) -> PyResult<f64> {
    parse_traffic_f64(value).map_err(err_value)
}

#[pyfunction(name = "parse_traffic_string")]
fn py_parse_traffic_string(value: &str) -> PyResult<String> {
    parse_traffic_string(value).map_err(err_value)
}

#[pyfunction(name = "parse_zone_policy")]
fn py_parse_zone_policy<'py>(
    py: Python<'py>,
    properties: std::collections::BTreeMap<String, String>,
) -> PyResult<Bound<'py, PyAny>> {
    obj_from(py, &policy::parse_zone_policy(&properties))
}

#[pyfunction(name = "parse_edge_traffic_semantics")]
#[pyo3(signature = (properties, directed=false))]
fn py_parse_edge_traffic_semantics<'py>(
    py: Python<'py>,
    properties: std::collections::BTreeMap<String, String>,
    directed: bool,
) -> PyResult<Bound<'py, PyAny>> {
    obj_from(py, &policy::parse_edge_traffic_semantics(&properties, directed))
}

#[pyfunction(name = "validate_zone_traffic_properties")]
fn py_validate_zone_traffic<'py>(
    py: Python<'py>,
    properties: std::collections::BTreeMap<String, String>,
) -> PyResult<Bound<'py, PyAny>> {
    obj_from(py, &policy::validate_zone_traffic_properties(&properties))
}

#[pyfunction(name = "validate_edge_traffic_properties")]
fn py_validate_edge_traffic<'py>(
    py: Python<'py>,
    properties: std::collections::BTreeMap<String, String>,
) -> PyResult<Bound<'py, PyAny>> {
    obj_from(py, &policy::validate_edge_traffic_properties(&properties))
}

fn zone_policy_kind_str(k: ZonePolicyKind) -> &'static str {
    match k {
        ZonePolicyKind::Informational => "informational",
        ZonePolicyKind::ExclusiveAccess => "exclusive",
        ZonePolicyKind::SharedAccess => "shared",
        ZonePolicyKind::CapacityLimited => "capacity_limited",
        ZonePolicyKind::Corridor => "corridor",
        ZonePolicyKind::Replanning => "replanning",
        ZonePolicyKind::Restricted => "restricted",
        ZonePolicyKind::NoStop => "no_stop",
        ZonePolicyKind::Slowdown => "slowdown",
    }
}

// ---------------------------------------------------------------------------
// Workspace
// ---------------------------------------------------------------------------

#[pyclass(name = "Workspace")]
pub struct PyWorkspace {
    inner: Arc<zoneout::Workspace>,
}

#[pymethods]
impl PyWorkspace {
    /// Load a workspace from a directory path.
    #[staticmethod]
    fn load(path: &str) -> PyResult<Self> {
        let ws = zoneout::Workspace::load(Path::new(path)).map_err(err_runtime)?;
        Ok(Self { inner: Arc::new(ws) })
    }

    /// Returns the root zone UUID.
    fn root_zone_id(&self) -> String {
        self.inner.root_zone().id().to_string()
    }
}

// ---------------------------------------------------------------------------
// WorkspaceIndex
// ---------------------------------------------------------------------------

#[pyclass(name = "WorkspaceIndex")]
pub struct PyWorkspaceIndex {
    inner: Arc<WorkspaceIndex>,
}

#[pymethods]
impl PyWorkspaceIndex {
    #[new]
    fn new(workspace: &PyWorkspace) -> Self {
        Self {
            inner: Arc::new(WorkspaceIndex::new(workspace.inner.clone())),
        }
    }

    fn root_zone_id(&self) -> Option<String> {
        self.inner.root_zone_id().map(|u| u.to_string())
    }

    fn validation_issues<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        obj_from(py, &self.inner.validation_issues())
    }

    fn is_valid(&self) -> bool { self.inner.is_valid() }

    fn refresh(&mut self) -> PyResult<()> {
        match Arc::get_mut(&mut self.inner) {
            Some(i) => { i.refresh(); Ok(()) }
            None => Err(err_runtime("workspace index has outstanding shared owners")),
        }
    }

    /// Returns the JSON-style dict of zones the given node belongs to.
    fn zones_of_node<'py>(&self, py: Python<'py>, node_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let id = uuid::Uuid::parse_str(node_id).map_err(err_value)?;
        let names: Vec<String> = self.inner.zones_of_node(id).into_iter()
            .map(|z| z.id().to_string()).collect();
        obj_from(py, &names)
    }
}

// ---------------------------------------------------------------------------
// ClaimManager
// ---------------------------------------------------------------------------

#[pyclass(name = "ClaimManager", unsendable)]
pub struct PyClaimManager {
    inner: ClaimManager,
}

#[pymethods]
impl PyClaimManager {
    #[new]
    #[pyo3(signature = (index=None))]
    fn new(index: Option<&PyWorkspaceIndex>) -> Self {
        match index {
            None => Self { inner: ClaimManager::new() },
            Some(idx) => Self { inner: ClaimManager::with_index(idx.inner.clone()) },
        }
    }

    fn request_count(&self) -> usize { self.inner.request_count() }
    fn lease_count(&self) -> usize { self.inner.lease_count() }
    fn empty(&self) -> bool { self.inner.empty() }

    fn add_request(&mut self, request: &Bound<'_, PyAny>) -> PyResult<()> {
        let req: ClaimRequest = obj_to(request)?;
        self.inner.add_request(req);
        Ok(())
    }

    fn upsert_request(&mut self, request: &Bound<'_, PyAny>) -> PyResult<()> {
        let req: ClaimRequest = obj_to(request)?;
        self.inner.upsert_request(req);
        Ok(())
    }

    fn remove_request(&mut self, claim_id: u64) -> bool {
        self.inner.remove_request(ClaimId::new(claim_id))
    }

    fn add_lease(&mut self, lease: &Bound<'_, PyAny>) -> PyResult<()> {
        let l: Lease = obj_to(lease)?;
        self.inner.add_lease(l);
        Ok(())
    }

    fn upsert_lease(&mut self, lease: &Bound<'_, PyAny>) -> PyResult<()> {
        let l: Lease = obj_to(lease)?;
        self.inner.upsert_lease(l);
        Ok(())
    }

    fn remove_lease(&mut self, lease_id: u64) -> bool {
        self.inner.remove_lease(LeaseId::new(lease_id))
    }

    #[pyo3(signature = (lease_id, released_at_tick=None))]
    fn release_lease(&mut self, lease_id: u64, released_at_tick: Option<u64>) -> bool {
        self.inner.release_lease(LeaseId::new(lease_id), released_at_tick)
    }

    fn expire_leases(&mut self, current_tick: u64) -> u64 {
        self.inner.expire_leases(current_tick)
    }

    #[pyo3(signature = (lease_id, refreshed_at_tick, expires_at_tick=None))]
    fn refresh_lease(
        &mut self, lease_id: u64, refreshed_at_tick: u64,
        expires_at_tick: Option<u64>,
    ) -> bool {
        self.inner.refresh_lease(LeaseId::new(lease_id), refreshed_at_tick, expires_at_tick)
    }

    fn revoke_lease(&mut self, lease_id: u64, reason: String, revoked_at_tick: u64) -> bool {
        self.inner.revoke_lease(LeaseId::new(lease_id), reason, revoked_at_tick)
    }

    fn evaluate_request<'py>(
        &self, py: Python<'py>, request: &Bound<'_, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let req: ClaimRequest = obj_to(request)?;
        obj_from(py, &self.inner.evaluate_request(&req))
    }

    fn requests<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        obj_from(py, &self.inner.requests())
    }
    fn leases<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        obj_from(py, &self.inner.leases())
    }
    fn released_leases<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        obj_from(py, &self.inner.released_leases())
    }

    fn clear(&mut self) { self.inner.clear(); }
}

// ---------------------------------------------------------------------------
// Coordinator
// ---------------------------------------------------------------------------

#[pyclass(name = "Coordinator", unsendable)]
pub struct PyCoordinator {
    inner: Coordinator,
}

#[pymethods]
impl PyCoordinator {
    #[new]
    #[pyo3(signature = (index=None))]
    fn new(index: Option<&PyWorkspaceIndex>) -> Self {
        match index {
            None => Self { inner: Coordinator::new() },
            Some(idx) => Self { inner: Coordinator::with_index(idx.inner.clone()) },
        }
    }

    fn robot_count(&self) -> usize { self.inner.robot_count() }
    fn empty(&self) -> bool { self.inner.empty() }

    fn register_robot(&mut self, state: &Bound<'_, PyAny>) -> PyResult<()> {
        let s: RobotState = obj_to(state)?;
        self.inner.register_robot(s);
        Ok(())
    }

    fn unregister_robot(&mut self, robot_id: u64) -> bool {
        self.inner.unregister_robot(RobotId::new(robot_id))
    }

    fn robot_state<'py>(
        &self, py: Python<'py>, robot_id: u64,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        match self.inner.find_robot_state(RobotId::new(robot_id)) {
            None => Ok(None),
            Some(s) => Ok(Some(obj_from(py, s)?)),
        }
    }

    fn robot_states<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        obj_from(py, &self.inner.robot_states())
    }

    fn assign_route_plan(
        &mut self, robot_id: u64,
        route_plan: &Bound<'_, PyAny>,
        horizon: u64, updated_at_tick: u64,
    ) -> PyResult<bool> {
        let plan: RoutePlan = obj_to(route_plan)?;
        Ok(self.inner.assign_route_plan(RobotId::new(robot_id), plan, horizon, updated_at_tick))
    }

    #[pyo3(signature = (
        robot_id, current_node_id=None, current_edge_id=None, updated_at_tick=0
    ))]
    fn update_robot_progress(
        &mut self, robot_id: u64,
        current_node_id: Option<&str>, current_edge_id: Option<&str>,
        updated_at_tick: u64,
    ) -> PyResult<bool> {
        let cn = match current_node_id {
            None => None,
            Some(s) => Some(uuid::Uuid::parse_str(s).map_err(err_value)?),
        };
        let ce = match current_edge_id {
            None => None,
            Some(s) => Some(uuid::Uuid::parse_str(s).map_err(err_value)?),
        };
        Ok(self.inner.update_robot_progress(RobotId::new(robot_id), cn, ce, updated_at_tick))
    }

    #[pyo3(signature = (
        robot_id, claim_id, start_tick=0, ticks_per_cost_unit=1.0, access_mode="exclusive"
    ))]
    fn schedule_robot_route<'py>(
        &mut self, py: Python<'py>,
        robot_id: u64, claim_id: u64,
        start_tick: u64, ticks_per_cost_unit: f64, access_mode: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mode = parse_access_mode(access_mode);
        let decision = self.inner.schedule_robot_route(
            RobotId::new(robot_id), ClaimId::new(claim_id),
            start_tick, ticks_per_cost_unit, mode,
        );
        obj_from(py, &decision)
    }

    fn refresh_robot_leases(
        &mut self, robot_id: u64, refreshed_at_tick: u64, extension_ticks: u64,
    ) -> u64 {
        self.inner.refresh_robot_leases(RobotId::new(robot_id), refreshed_at_tick, extension_ticks)
    }

    fn revoke_robot_leases(
        &mut self, robot_id: u64, reason: String, revoked_at_tick: u64,
    ) -> u64 {
        self.inner.revoke_robot_leases(RobotId::new(robot_id), reason, revoked_at_tick)
    }

    fn handle_missed_schedule_slot(
        &mut self, robot_id: u64, current_tick: u64, grace_ticks: u64,
    ) -> bool {
        self.inner.handle_missed_schedule_slot(RobotId::new(robot_id), current_tick, grace_ticks)
    }

    fn release_behind_progress(&mut self, robot_id: u64) -> u64 {
        self.inner.release_behind_progress(RobotId::new(robot_id))
    }

    fn claim_manager_requests<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        obj_from(py, &self.inner.claim_manager().requests())
    }
    fn claim_manager_leases<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        obj_from(py, &self.inner.claim_manager().leases())
    }

    fn clear(&mut self) { self.inner.clear(); }
}

// ---------------------------------------------------------------------------
// route planning
// ---------------------------------------------------------------------------

#[pyfunction(name = "plan_route")]
#[pyo3(signature = (index, start_node_id, goal_node_id, use_penalties=true))]
fn py_plan_route<'py>(
    py: Python<'py>,
    index: &PyWorkspaceIndex,
    start_node_id: &str,
    goal_node_id: &str,
    use_penalties: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let start = uuid::Uuid::parse_str(start_node_id).map_err(err_value)?;
    let goal = uuid::Uuid::parse_str(goal_node_id).map_err(err_value)?;
    let result = plan_route(&index.inner, start, goal, use_penalties);
    #[derive(serde::Serialize)]
    struct Out<'a> {
        found: bool,
        plan: &'a Option<RoutePlan>,
        failure: &'a Option<crate::route::RouteFailure>,
        distance: f64,
    }
    obj_from(py, &Out {
        found: result.search.found,
        plan: &result.plan,
        failure: &result.failure,
        distance: result.search.distance,
    })
}

// ---------------------------------------------------------------------------
// VDA mapping
// ---------------------------------------------------------------------------

#[pyfunction(name = "vda_order_from_route")]
fn py_vda_order_from_route<'py>(
    py: Python<'py>, route_plan: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let plan: RoutePlan = obj_to(route_plan)?;
    let order = crate::vda::map_route_plan(&plan);
    obj_from(py, &order)
}

#[pyfunction(name = "vda_state_from_robot")]
fn py_vda_state_from_robot<'py>(
    py: Python<'py>, robot_state: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let state: RobotState = obj_to(robot_state)?;
    let s = crate::vda::map_robot_state(&state);
    obj_from(py, &s)
}

// ---------------------------------------------------------------------------
// arbitration
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
#[pyfunction(name = "arbitrate_right_of_way")]
#[pyo3(signature = (
    self_priority = 0.0, other_priority = 0.0,
    self_holds_lease = false, other_holds_lease = false,
    self_is_emergency = false, other_is_emergency = false,
    self_state = "idle", other_state = "idle",
    self_wait_ticks = 0u64, other_wait_ticks = 0u64,
    self_remaining_steps = 0u64, other_remaining_steps = 0u64,
))]
fn py_arbitrate_right_of_way(
    self_priority: f64, other_priority: f64,
    self_holds_lease: bool, other_holds_lease: bool,
    self_is_emergency: bool, other_is_emergency: bool,
    self_state: &str, other_state: &str,
    self_wait_ticks: u64, other_wait_ticks: u64,
    self_remaining_steps: u64, other_remaining_steps: u64,
) -> &'static str {
    let ctx = ArbitrationContext {
        self_priority, other_priority,
        self_holds_lease, other_holds_lease,
        self_is_emergency, other_is_emergency,
        self_state: parse_progress_state(self_state),
        other_state: parse_progress_state(other_state),
        self_wait_ticks, other_wait_ticks,
        self_remaining_steps, other_remaining_steps,
    };
    match arbitrate_right_of_way(&ctx) {
        ArbitrationDecision::Proceed => "proceed",
        ArbitrationDecision::Yield => "yield",
        ArbitrationDecision::Replan => "replan",
    }
}

// ---------------------------------------------------------------------------
// module registration
// ---------------------------------------------------------------------------

pub fn register_python_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Free functions
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add_function(wrap_pyfunction!(py_parse_traffic_bool, m)?)?;
    m.add_function(wrap_pyfunction!(py_parse_traffic_u64, m)?)?;
    m.add_function(wrap_pyfunction!(py_parse_traffic_f64, m)?)?;
    m.add_function(wrap_pyfunction!(py_parse_traffic_string, m)?)?;
    m.add_function(wrap_pyfunction!(py_parse_zone_policy, m)?)?;
    m.add_function(wrap_pyfunction!(py_parse_edge_traffic_semantics, m)?)?;
    m.add_function(wrap_pyfunction!(py_validate_zone_traffic, m)?)?;
    m.add_function(wrap_pyfunction!(py_validate_edge_traffic, m)?)?;
    m.add_function(wrap_pyfunction!(py_arbitrate_right_of_way, m)?)?;
    m.add_function(wrap_pyfunction!(py_plan_route, m)?)?;
    m.add_function(wrap_pyfunction!(py_vda_order_from_route, m)?)?;
    m.add_function(wrap_pyfunction!(py_vda_state_from_robot, m)?)?;

    // Classes
    m.add_class::<PyWorkspace>()?;
    m.add_class::<PyWorkspaceIndex>()?;
    m.add_class::<PyClaimManager>()?;
    m.add_class::<PyCoordinator>()?;

    // Enum string values for discoverability
    m.add("ZONE_POLICY_KINDS", vec![
        zone_policy_kind_str(ZonePolicyKind::Informational),
        zone_policy_kind_str(ZonePolicyKind::ExclusiveAccess),
        zone_policy_kind_str(ZonePolicyKind::SharedAccess),
        zone_policy_kind_str(ZonePolicyKind::CapacityLimited),
        zone_policy_kind_str(ZonePolicyKind::Corridor),
        zone_policy_kind_str(ZonePolicyKind::Replanning),
        zone_policy_kind_str(ZonePolicyKind::Restricted),
        zone_policy_kind_str(ZonePolicyKind::NoStop),
        zone_policy_kind_str(ZonePolicyKind::Slowdown),
    ])?;
    m.add("ROBOT_PROGRESS_STATES", vec![
        "idle", "following_route", "waiting", "queued", "blocked", "replanning",
    ])?;
    m.add("CLAIM_ACCESS_MODES", vec!["shared", "exclusive"])?;
    m.add("CLAIM_DECISIONS", vec!["grant", "deny"])?;
    m.add("SCHEDULE_DECISION_KINDS", vec!["proceed", "queue", "replan"])?;
    Ok(())
}

#[pymodule]
fn timenav(m: &Bound<'_, PyModule>) -> PyResult<()> {
    register_python_module(m)
}
