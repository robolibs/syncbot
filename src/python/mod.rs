//! PyO3 bindings — full coverage of the public syncbot surface.
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

use std::collections::BTreeMap as OMap;
use std::path::Path;
use std::sync::Arc;

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::claim::{ClaimAccessMode, ClaimManager, ClaimRequest, Lease};
use crate::coordinator::{
    ArbitrationContext, ArbitrationDecision, Coordinator, arbitrate_right_of_way,
};
use crate::core::ids::{ClaimId, LeaseId, RobotId};
use crate::index::WorkspaceIndex;
use crate::policy::{
    self, ZonePolicyKind, parse_traffic_bool, parse_traffic_f64, parse_traffic_string,
    parse_traffic_u64,
};
use crate::robot::{RobotProgressState, RobotState};
use crate::route::{RoutePlan, plan_route};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn err_value(e: impl ToString) -> PyErr {
    PyValueError::new_err(e.to_string())
}
fn err_runtime(e: impl ToString) -> PyErr {
    PyRuntimeError::new_err(e.to_string())
}

/// Convert a `serde_json::Value` to a Python object.
fn json_to_py<'py>(py: Python<'py>, v: &serde_json::Value) -> PyResult<Bound<'py, PyAny>> {
    Ok(match v {
        serde_json::Value::Null => py.None().into_bound(py),
        serde_json::Value::Bool(b) => b.into_pyobject(py)?.to_owned().into_any(),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into_pyobject(py)?.into_any()
            } else if let Some(u) = n.as_u64() {
                u.into_pyobject(py)?.into_any()
            } else {
                n.as_f64().unwrap().into_pyobject(py)?.to_owned().into_any()
            }
        }
        serde_json::Value::String(s) => s.into_pyobject(py)?.to_owned().into_any(),
        serde_json::Value::Array(arr) => {
            let list = PyList::empty(py);
            for item in arr {
                list.append(json_to_py(py, item)?)?;
            }
            list.into_any()
        }
        serde_json::Value::Object(map) => {
            let dict = PyDict::new(py);
            for (k, v) in map {
                dict.set_item(k, json_to_py(py, v)?)?;
            }
            dict.into_any()
        }
    })
}

/// Convert a Python object to a `serde_json::Value`.
fn py_to_json(value: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
    if value.is_none() {
        return Ok(serde_json::Value::Null);
    }
    if let Ok(b) = value.extract::<bool>() {
        return Ok(serde_json::Value::Bool(b));
    }
    if let Ok(i) = value.extract::<i64>() {
        return Ok(serde_json::Value::Number(i.into()));
    }
    if let Ok(u) = value.extract::<u64>() {
        return Ok(serde_json::Value::Number(u.into()));
    }
    if let Ok(f) = value.extract::<f64>() {
        return Ok(serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null));
    }
    if let Ok(s) = value.extract::<String>() {
        return Ok(serde_json::Value::String(s));
    }
    if let Ok(list) = value.downcast::<PyList>() {
        let mut arr = Vec::with_capacity(list.len());
        for item in list.iter() {
            arr.push(py_to_json(&item)?);
        }
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
fn version() -> &'static str {
    crate::version()
}

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
    obj_from(
        py,
        &policy::parse_edge_traffic_semantics(&properties, directed),
    )
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
        Ok(Self {
            inner: Arc::new(ws),
        })
    }

    /// Write the workspace out as a `zoneout` directory.
    fn save(&self, path: &str) -> PyResult<()> {
        self.inner.save(Path::new(path)).map_err(err_runtime)
    }

    /// Returns the root zone UUID.
    fn root_zone_id(&self) -> String {
        self.inner.root_zone().id().to_string()
    }

    /// The workspace datum as `{"lat", "lon", "alt"}`, or `None`.
    fn datum<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match self.inner.datum() {
            Some(geo) => obj_from(py, &geo_dict(*geo)),
            None => Ok(py.None().into_bound(py)),
        }
    }

    fn node_count(&self) -> usize {
        self.inner.graph().vertices().len()
    }

    fn edge_count(&self) -> usize {
        self.inner.graph().edges().len()
    }
}

// ---------------------------------------------------------------------------
// WorkspaceBuilder
// ---------------------------------------------------------------------------

/// Build a workspace from Python without an on-disk fixture.
///
/// Zones, nodes and edges take plain dicts and tuples; `traffic.*` properties
/// mean exactly what they mean everywhere else. Nodes are named so edges can
/// refer to them without the caller tracking UUIDs.
#[pyclass(name = "WorkspaceBuilder", unsendable)]
pub struct PyWorkspaceBuilder {
    root: Option<zoneout::Zone>,
    datum: datapod::Geo,
    zones: Vec<zoneout::Zone>,
    nodes: Vec<(String, datapod::Point, OMap<String, String>)>,
    edges: Vec<(String, String, f64, OMap<String, String>)>,
}

fn polygon_from(points: Vec<(f64, f64)>) -> datapod::Polygon {
    datapod::Polygon {
        vertices: points
            .into_iter()
            .map(|(x, y)| datapod::Point::new(x, y, 0.0))
            .collect(),
    }
}

fn properties_from(properties: Option<&Bound<'_, PyAny>>) -> PyResult<OMap<String, String>> {
    let Some(dict) = properties else {
        return Ok(OMap::new());
    };
    if dict.is_none() {
        return Ok(OMap::new());
    }
    let dict = dict
        .downcast::<PyDict>()
        .map_err(|_| err_value("properties must be a dict of str -> str"))?;
    let mut out = OMap::new();
    for (key, value) in dict.iter() {
        out.insert(key.extract::<String>()?, value.extract::<String>()?);
    }
    Ok(out)
}

#[pymethods]
impl PyWorkspaceBuilder {
    #[new]
    #[pyo3(signature = (name, boundary, datum, kind="workspace", properties=None))]
    fn new(
        name: &str,
        boundary: Vec<(f64, f64)>,
        datum: (f64, f64, f64),
        kind: &str,
        properties: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        let geo = datapod::Geo::new(datum.0, datum.1, datum.2);
        let mut builder = zoneout::ZoneBuilder::new()
            .with_name(name)
            .with_kind(kind)
            .with_boundary(polygon_from(boundary))
            .with_datum(geo)
            .with_resolution(0.0);
        for (key, value) in properties_from(properties)? {
            builder = builder.with_property(key, value);
        }
        Ok(Self {
            root: Some(builder.build().map_err(err_runtime)?),
            datum: geo,
            zones: Vec::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
        })
    }

    /// Add a child zone. `numeric_id` is the short alias the flat wire uses.
    #[pyo3(signature = (name, boundary, numeric_id=None, kind="zone", properties=None))]
    fn add_zone(
        &mut self,
        name: &str,
        boundary: Vec<(f64, f64)>,
        numeric_id: Option<u64>,
        kind: &str,
        properties: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<String> {
        let mut builder = zoneout::ZoneBuilder::new()
            .with_name(name)
            .with_kind(kind)
            .with_boundary(polygon_from(boundary))
            .with_datum(self.datum)
            .with_resolution(0.0);
        if let Some(n) = numeric_id {
            builder = builder.with_property(crate::NUMERIC_ID_PROPERTY, n.to_string());
        }
        for (key, value) in properties_from(properties)? {
            builder = builder.with_property(key, value);
        }
        let zone = builder.build().map_err(err_runtime)?;
        let id = zone.id().to_string();
        self.zones.push(zone);
        Ok(id)
    }

    /// Add a graph node at `(x, y)`. The name is how edges refer to it.
    #[pyo3(signature = (name, x, y, z=0.0, numeric_id=None, properties=None))]
    fn add_node(
        &mut self,
        name: &str,
        x: f64,
        y: f64,
        z: f64,
        numeric_id: Option<u64>,
        properties: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let mut props = properties_from(properties)?;
        if let Some(n) = numeric_id {
            props.insert(crate::NUMERIC_ID_PROPERTY.into(), n.to_string());
        }
        props.entry("label".into()).or_insert_with(|| name.into());
        self.nodes
            .push((name.to_string(), datapod::Point::new(x, y, z), props));
        Ok(())
    }

    /// Connect two named nodes. Undirected unless `directed=True`.
    #[pyo3(signature = (source, target, weight=1.0, numeric_id=None, directed=false, properties=None))]
    fn add_edge(
        &mut self,
        source: &str,
        target: &str,
        weight: f64,
        numeric_id: Option<u64>,
        directed: bool,
        properties: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let mut props = properties_from(properties)?;
        if let Some(n) = numeric_id {
            props.insert(crate::NUMERIC_ID_PROPERTY.into(), n.to_string());
        }
        if directed {
            props.insert("__directed".into(), "true".into());
        }
        self.edges
            .push((source.to_string(), target.to_string(), weight, props));
        Ok(())
    }

    /// Assemble the workspace. The builder is spent afterwards.
    fn build(&mut self) -> PyResult<PyWorkspace> {
        let mut root = self
            .root
            .take()
            .ok_or_else(|| err_runtime("workspace builder has already been built"))?;
        for zone in std::mem::take(&mut self.zones) {
            root.add_child(zone).map_err(err_runtime)?;
        }

        let mut ws = zoneout::Workspace::new(root);
        ws.set_coord_mode(zoneout::CoordMode::Local);
        ws.set_datum(self.datum);

        let mut by_name = OMap::new();
        for (name, position, props) in std::mem::take(&mut self.nodes) {
            let mut data = zoneout::NodeData::new(position);
            data.name = name.clone();
            data.properties = props;
            by_name.insert(name, ws.add_node_data(data));
        }

        for (source, target, weight, mut props) in std::mem::take(&mut self.edges) {
            let directed = props.remove("__directed").is_some();
            let (Some(&from), Some(&to)) = (by_name.get(&source), by_name.get(&target)) else {
                return Err(err_value(format!(
                    "edge {source:?} -> {target:?} names a node that was never added"
                )));
            };
            let edge = zoneout::EdgeData {
                id: uuid::Uuid::new_v4(),
                zone_ids: Vec::new(),
                properties: props,
            };
            let kind = if directed {
                graphix::vertex::EdgeType::Directed
            } else {
                graphix::vertex::EdgeType::Undirected
            };
            ws.add_edge_data(from, to, weight, kind, edge);
        }

        // Adding an edge does not work out which zones it crosses. Without
        // this an edge claim implies intent on nothing, and an exclusive zone
        // whose only occupant is a corridor would never register as held.
        ws.refresh_graph_zone_membership();

        Ok(PyWorkspace {
            inner: Arc::new(ws),
        })
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

    fn is_valid(&self) -> bool {
        self.inner.is_valid()
    }

    fn refresh(&mut self) -> PyResult<()> {
        match Arc::get_mut(&mut self.inner) {
            Some(i) => {
                i.refresh();
                Ok(())
            }
            None => Err(err_runtime("workspace index has outstanding shared owners")),
        }
    }

    /// Returns the JSON-style dict of zones the given node belongs to.
    fn zones_of_node<'py>(&self, py: Python<'py>, node_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let id = uuid::Uuid::parse_str(node_id).map_err(err_value)?;
        let names: Vec<String> = self
            .inner
            .zones_of_node(id)
            .into_iter()
            .map(|z| z.id().to_string())
            .collect();
        obj_from(py, &names)
    }

    /// The zones the given edge passes through, as UUID strings.
    fn zones_of_edge<'py>(&self, py: Python<'py>, edge_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let id = uuid::Uuid::parse_str(edge_id).map_err(err_value)?;
        let ids: Vec<String> = self
            .inner
            .zones_of_edge(id)
            .into_iter()
            .map(|z| z.id().to_string())
            .collect();
        obj_from(py, &ids)
    }

    /// The workspace datum as `{"lat", "lon", "alt"}`, or `None`.
    fn datum<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match self.inner.datum() {
            Some(geo) => obj_from(py, &geo_dict(geo)),
            None => Ok(py.None().into_bound(py)),
        }
    }

    /// Every zone, root first, with boundary and parsed policy — enough to
    /// draw the workspace without reaching back into `zoneout`.
    fn zones<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let Some(root) = self.inner.root_zone_id() else {
            return obj_from(py, &Vec::<ZoneView>::new());
        };
        let mut ids = vec![root];
        ids.extend(self.inner.descendant_zones(root).iter().map(|z| z.id()));
        let views: Vec<ZoneView> = ids.iter().filter_map(|id| self.zone_view(*id)).collect();
        obj_from(py, &views)
    }

    /// One zone by UUID, in the same shape `zones()` yields.
    fn zone<'py>(&self, py: Python<'py>, zone_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let id = uuid::Uuid::parse_str(zone_id).map_err(err_value)?;
        match self.zone_view(id) {
            Some(view) => obj_from(py, &view),
            None => Ok(py.None().into_bound(py)),
        }
    }

    /// Every graph node with its position and properties.
    fn nodes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let graph = self.inner.workspace().graph();
        let views: Vec<NodeView> = graph
            .vertices()
            .into_iter()
            .filter_map(|vid| graph.get_vertex(vid))
            .map(|node| NodeView {
                id: node.id.to_string(),
                name: node.name.clone(),
                numeric_id: numeric_of(&node.properties),
                x: node.position.x,
                y: node.position.y,
                z: node.position.z,
                properties: node.properties.clone(),
            })
            .collect();
        obj_from(py, &views)
    }

    /// One node by UUID.
    fn node<'py>(&self, py: Python<'py>, node_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let id = uuid::Uuid::parse_str(node_id).map_err(err_value)?;
        let Some(node) = self.inner.node(id) else {
            return Ok(py.None().into_bound(py));
        };
        obj_from(
            py,
            &NodeView {
                id: node.id.to_string(),
                name: node.name.clone(),
                numeric_id: numeric_of(&node.properties),
                x: node.position.x,
                y: node.position.y,
                z: node.position.z,
                properties: node.properties.clone(),
            },
        )
    }

    /// Every graph edge with the nodes it joins.
    fn edges<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let graph = self.inner.workspace().graph();
        let mut views = Vec::new();
        for edge in graph.edges() {
            let Some(prop) = graph.edge_property(edge.id) else {
                continue;
            };
            let ends = |vid| graph.get_vertex(vid).map(|v| v.id.to_string());
            let (Some(source), Some(target)) = (
                graph.source(edge.id).and_then(ends),
                graph.target(edge.id).and_then(ends),
            ) else {
                continue;
            };
            views.push(EdgeView {
                id: prop.id.to_string(),
                numeric_id: numeric_of(&prop.properties),
                source,
                target,
                properties: prop.properties.clone(),
            });
        }
        obj_from(py, &views)
    }

    /// The parsed `traffic.*` policy of a zone.
    fn zone_policy<'py>(&self, py: Python<'py>, zone_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let id = uuid::Uuid::parse_str(zone_id).map_err(err_value)?;
        match self.inner.zone_policy(id) {
            Some(policy) => obj_from(py, policy),
            None => Ok(py.None().into_bound(py)),
        }
    }

    /// The parsed `traffic.*` semantics of an edge.
    fn edge_semantics<'py>(&self, py: Python<'py>, edge_id: &str) -> PyResult<Bound<'py, PyAny>> {
        let id = uuid::Uuid::parse_str(edge_id).map_err(err_value)?;
        match self.inner.edge_semantics(id) {
            Some(semantics) => obj_from(py, semantics),
            None => Ok(py.None().into_bound(py)),
        }
    }

    /// Resolve the short numeric alias the flat wire uses to a UUID.
    fn zone_by_numeric_id(&self, numeric_id: u64) -> Option<String> {
        self.inner
            .zone_uuid_by_numeric_id(numeric_id)
            .map(|u| u.to_string())
    }

    fn node_by_numeric_id(&self, numeric_id: u64) -> Option<String> {
        self.inner
            .node_uuid_by_numeric_id(numeric_id)
            .map(|u| u.to_string())
    }

    fn edge_by_numeric_id(&self, numeric_id: u64) -> Option<String> {
        self.inner
            .edge_uuid_by_numeric_id(numeric_id)
            .map(|u| u.to_string())
    }

    /// Local ENU metres to WGS84, through the workspace datum.
    fn local_to_global<'py>(
        &self,
        py: Python<'py>,
        x: f64,
        y: f64,
        z: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let geo = self
            .inner
            .local_to_global(datapod::Point::new(x, y, z))
            .map_err(err_runtime)?;
        obj_from(py, &geo_dict(geo))
    }

    /// WGS84 to local ENU metres, through the workspace datum.
    fn global_to_local<'py>(
        &self,
        py: Python<'py>,
        lat: f64,
        lon: f64,
        alt: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let point = self
            .inner
            .global_to_local(datapod::Geo::new(lat, lon, alt))
            .map_err(err_runtime)?;
        obj_from(py, &point_dict(point))
    }

    /// The edge joining two nodes, if there is one.
    fn edge_between(&self, node_a: &str, node_b: &str) -> PyResult<Option<String>> {
        let a = uuid::Uuid::parse_str(node_a).map_err(err_value)?;
        let b = uuid::Uuid::parse_str(node_b).map_err(err_value)?;
        Ok(self.inner.edge_between(a, b).map(|e| e.id.to_string()))
    }
}

impl PyWorkspaceIndex {
    fn zone_view(&self, id: uuid::Uuid) -> Option<ZoneView> {
        let zone = self.inner.zone(id)?;
        let boundary = if zone.poly().has_field_boundary() {
            zone.poly()
                .field_boundary()
                .vertices
                .iter()
                .map(|v| (v.x, v.y))
                .collect()
        } else {
            Vec::new()
        };
        Some(ZoneView {
            id: id.to_string(),
            name: zone.name().to_string(),
            kind: zone.kind().to_string(),
            numeric_id: self
                .inner
                .zone_property(id, crate::NUMERIC_ID_PROPERTY)
                .and_then(|v| v.parse().ok()),
            boundary,
            policy: self.inner.zone_policy(id).cloned(),
        })
    }
}

#[derive(serde::Serialize)]
struct ZoneView {
    id: String,
    name: String,
    kind: String,
    numeric_id: Option<u64>,
    boundary: Vec<(f64, f64)>,
    policy: Option<crate::policy::ZonePolicy>,
}

#[derive(serde::Serialize)]
struct NodeView {
    id: String,
    name: String,
    numeric_id: Option<u64>,
    x: f64,
    y: f64,
    z: f64,
    properties: OMap<String, String>,
}

#[derive(serde::Serialize)]
struct EdgeView {
    id: String,
    numeric_id: Option<u64>,
    source: String,
    target: String,
    properties: OMap<String, String>,
}

fn numeric_of(properties: &OMap<String, String>) -> Option<u64> {
    properties
        .get(crate::NUMERIC_ID_PROPERTY)
        .and_then(|v| v.trim().parse().ok())
}

fn geo_dict(geo: datapod::Geo) -> serde_json::Value {
    serde_json::json!({ "lat": geo.latitude, "lon": geo.longitude, "alt": geo.altitude })
}

fn point_dict(point: datapod::Point) -> serde_json::Value {
    serde_json::json!({ "x": point.x, "y": point.y, "z": point.z })
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
            None => Self {
                inner: ClaimManager::new(),
            },
            Some(idx) => Self {
                inner: ClaimManager::with_index(idx.inner.clone()),
            },
        }
    }

    fn request_count(&self) -> usize {
        self.inner.request_count()
    }
    fn lease_count(&self) -> usize {
        self.inner.lease_count()
    }
    fn empty(&self) -> bool {
        self.inner.empty()
    }

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

    /// Record a claim, replacing whatever this robot held before.
    fn upsert_request_for_robot(&mut self, request: &Bound<'_, PyAny>) -> PyResult<bool> {
        let req: ClaimRequest = obj_to(request)?;
        Ok(self.inner.upsert_request_for_robot(req))
    }

    fn remove_requests_for_robot(&mut self, robot_id: u64) -> u64 {
        self.inner.remove_requests_for_robot(RobotId::new(robot_id))
    }

    fn expire_requests(&mut self, current_tick: u64) -> u64 {
        self.inner.expire_requests(current_tick)
    }

    #[pyo3(signature = (robot_id, released_at_tick=None))]
    fn release_leases_for_robot(&mut self, robot_id: u64, released_at_tick: Option<u64>) -> u64 {
        self.inner
            .release_leases_for_robot(RobotId::new(robot_id), released_at_tick)
    }

    fn leases_for_robot<'py>(&self, py: Python<'py>, robot_id: u64) -> PyResult<Bound<'py, PyAny>> {
        let leases: Vec<&Lease> = self.inner.leases_for_robot(RobotId::new(robot_id));
        obj_from(py, &leases)
    }

    fn next_request_id(&self) -> u64 {
        self.inner.next_request_id().raw()
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
        self.inner
            .release_lease(LeaseId::new(lease_id), released_at_tick)
    }

    fn expire_leases(&mut self, current_tick: u64) -> u64 {
        self.inner.expire_leases(current_tick)
    }

    #[pyo3(signature = (lease_id, refreshed_at_tick, expires_at_tick=None))]
    fn refresh_lease(
        &mut self,
        lease_id: u64,
        refreshed_at_tick: u64,
        expires_at_tick: Option<u64>,
    ) -> bool {
        self.inner
            .refresh_lease(LeaseId::new(lease_id), refreshed_at_tick, expires_at_tick)
    }

    fn revoke_lease(&mut self, lease_id: u64, reason: String, revoked_at_tick: u64) -> bool {
        self.inner
            .revoke_lease(LeaseId::new(lease_id), reason, revoked_at_tick)
    }

    fn evaluate_request<'py>(
        &self,
        py: Python<'py>,
        request: &Bound<'_, PyAny>,
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

    fn clear(&mut self) {
        self.inner.clear();
    }
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
            None => Self {
                inner: Coordinator::new(),
            },
            Some(idx) => Self {
                inner: Coordinator::with_index(idx.inner.clone()),
            },
        }
    }

    fn robot_count(&self) -> usize {
        self.inner.robot_count()
    }
    fn empty(&self) -> bool {
        self.inner.empty()
    }

    fn register_robot(&mut self, state: &Bound<'_, PyAny>) -> PyResult<()> {
        let s: RobotState = obj_to(state)?;
        self.inner.register_robot(s);
        Ok(())
    }

    fn unregister_robot(&mut self, robot_id: u64) -> bool {
        self.inner.unregister_robot(RobotId::new(robot_id))
    }

    fn robot_state<'py>(
        &self,
        py: Python<'py>,
        robot_id: u64,
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
        &mut self,
        robot_id: u64,
        route_plan: &Bound<'_, PyAny>,
        horizon: u64,
        updated_at_tick: u64,
    ) -> PyResult<bool> {
        let plan: RoutePlan = obj_to(route_plan)?;
        Ok(self
            .inner
            .assign_route_plan(RobotId::new(robot_id), plan, horizon, updated_at_tick))
    }

    #[pyo3(signature = (
        robot_id, current_node_id=None, current_edge_id=None, updated_at_tick=0
    ))]
    fn update_robot_progress(
        &mut self,
        robot_id: u64,
        current_node_id: Option<&str>,
        current_edge_id: Option<&str>,
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
        Ok(self
            .inner
            .update_robot_progress(RobotId::new(robot_id), cn, ce, updated_at_tick))
    }

    #[pyo3(signature = (
        robot_id, claim_id, start_tick=0, ticks_per_cost_unit=1.0, access_mode="exclusive"
    ))]
    fn schedule_robot_route<'py>(
        &mut self,
        py: Python<'py>,
        robot_id: u64,
        claim_id: u64,
        start_tick: u64,
        ticks_per_cost_unit: f64,
        access_mode: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let mode = parse_access_mode(access_mode);
        let decision = self.inner.schedule_robot_route(
            RobotId::new(robot_id),
            ClaimId::new(claim_id),
            start_tick,
            ticks_per_cost_unit,
            mode,
        );
        obj_from(py, &decision)
    }

    fn refresh_robot_leases(
        &mut self,
        robot_id: u64,
        refreshed_at_tick: u64,
        extension_ticks: u64,
    ) -> u64 {
        self.inner
            .refresh_robot_leases(RobotId::new(robot_id), refreshed_at_tick, extension_ticks)
    }

    fn revoke_robot_leases(&mut self, robot_id: u64, reason: String, revoked_at_tick: u64) -> u64 {
        self.inner
            .revoke_robot_leases(RobotId::new(robot_id), reason, revoked_at_tick)
    }

    fn handle_missed_schedule_slot(
        &mut self,
        robot_id: u64,
        current_tick: u64,
        grace_ticks: u64,
    ) -> bool {
        self.inner
            .handle_missed_schedule_slot(RobotId::new(robot_id), current_tick, grace_ticks)
    }

    fn release_behind_progress(&mut self, robot_id: u64) -> u64 {
        self.inner.release_behind_progress(RobotId::new(robot_id))
    }

    /// The rolling-horizon claim this robot needs next: the nodes and edges
    /// of the coming `horizon` steps, and nothing further. Zones are not
    /// claimed — the manager derives intent on them from these targets.
    #[pyo3(signature = (robot_id, claim_id, access_mode="exclusive"))]
    fn claim_request_for_robot<'py>(
        &self,
        py: Python<'py>,
        robot_id: u64,
        claim_id: u64,
        access_mode: &str,
    ) -> PyResult<Bound<'py, PyAny>> {
        let request = self.inner.claim_request_for_robot(
            RobotId::new(robot_id),
            ClaimId::new(claim_id),
            parse_access_mode(access_mode),
        );
        obj_from(py, &request)
    }

    /// Ask whether a claim would be granted, without recording it.
    fn evaluate_claim<'py>(
        &self,
        py: Python<'py>,
        request: &Bound<'_, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let req: ClaimRequest = obj_to(request)?;
        obj_from(py, &self.inner.claim_manager().evaluate_request(&req))
    }

    /// Record a granted claim, replacing whatever this robot held before.
    ///
    /// Replacing rather than stacking is what makes a rolling horizon release
    /// the ground behind it: moving forward and re-claiming is the release.
    /// Returns `True` when it replaced an existing claim on the same targets,
    /// `False` when it added a new one — both are success.
    fn upsert_claim_request_for_robot(&mut self, request: &Bound<'_, PyAny>) -> PyResult<bool> {
        let req: ClaimRequest = obj_to(request)?;
        Ok(self.inner.claim_manager_mut().upsert_request_for_robot(req))
    }

    /// Drop every claim this robot holds — it has arrived, or given up.
    fn remove_claim_requests_for_robot(&mut self, robot_id: u64) -> u64 {
        self.inner
            .claim_manager_mut()
            .remove_requests_for_robot(RobotId::new(robot_id))
    }

    fn claim_request_count(&self) -> usize {
        self.inner.claim_manager().request_count()
    }

    fn claim_lease_count(&self) -> usize {
        self.inner.claim_manager().lease_count()
    }

    /// Record which claims and leases a robot is holding.
    #[pyo3(signature = (robot_id, pending_claim_ids, active_lease_ids, last_claim_tick=None))]
    fn update_robot_claim_state(
        &mut self,
        robot_id: u64,
        pending_claim_ids: Vec<u64>,
        active_lease_ids: Vec<u64>,
        last_claim_tick: Option<u64>,
    ) -> bool {
        self.inner.update_robot_claim_state(
            RobotId::new(robot_id),
            pending_claim_ids.into_iter().map(ClaimId::new).collect(),
            active_lease_ids.into_iter().map(LeaseId::new).collect(),
            last_claim_tick,
        )
    }

    /// Report where a robot is. Position and heading are independent — `None`
    /// means "not reported", never "moved to nowhere".
    #[pyo3(signature = (robot_id, x=None, y=None, z=None, yaw=None, frame="local", now_ms=0))]
    #[allow(clippy::too_many_arguments)]
    fn update_robot_pose(
        &mut self,
        robot_id: u64,
        x: Option<f64>,
        y: Option<f64>,
        z: Option<f64>,
        yaw: Option<f64>,
        frame: &str,
        now_ms: u64,
    ) -> bool {
        let index = self.inner.index();
        let position = match (x, y) {
            (Some(x), Some(y)) if frame == "global" => Some(
                crate::robot::RobotPosition::from_global(x, y, z.unwrap_or(0.0), index),
            ),
            (Some(x), Some(y)) => Some(crate::robot::RobotPosition::from_local(
                x,
                y,
                z.unwrap_or(0.0),
                index,
            )),
            _ => None,
        };
        let heading = yaw.map(crate::robot::RobotHeading::from_yaw_rad);
        self.inner
            .update_robot_pose(RobotId::new(robot_id), position, heading, now_ms)
    }

    /// Expire claims whose lease window has passed.
    fn expire_claims(&mut self, now_ms: u64) -> u64 {
        self.inner.expire_claims(now_ms)
    }

    /// Heartbeat bookkeeping: how often a robot promises to report, and when
    /// it last did.
    fn set_alive(&mut self, robot_id: u64, interval_secs: u64, now_ms: u64) {
        self.inner
            .set_alive(RobotId::new(robot_id), interval_secs, now_ms);
    }

    fn touch_robot(&mut self, robot_id: u64, now_ms: u64) {
        self.inner.touch_robot(RobotId::new(robot_id), now_ms);
    }

    fn robot_active_at(&self, robot_id: u64, now_ms: u64) -> bool {
        self.inner.robot_active_at(RobotId::new(robot_id), now_ms)
    }

    fn inactive_robots_at(&self, now_ms: u64) -> Vec<u64> {
        self.inner
            .inactive_robots_at(now_ms)
            .into_iter()
            .map(|r| r.raw())
            .collect()
    }

    /// Drop robots that have stopped heart-beating, releasing what they held.
    fn sweep_inactive(&mut self, now_ms: u64) -> Vec<u64> {
        self.inner
            .sweep_inactive(now_ms)
            .into_iter()
            .map(|r| r.raw())
            .collect()
    }

    /// Full coordinator state, for persisting across a restart.
    fn snapshot<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        obj_from(py, &self.inner.snapshot())
    }

    fn claim_manager_requests<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        obj_from(py, &self.inner.claim_manager().requests())
    }
    fn claim_manager_leases<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        obj_from(py, &self.inner.claim_manager().leases())
    }

    fn clear(&mut self) {
        self.inner.clear();
    }
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
    obj_from(
        py,
        &Out {
            found: result.search.found,
            plan: &result.plan,
            failure: &result.failure,
            distance: result.search.distance,
        },
    )
}

// ---------------------------------------------------------------------------
// VDA mapping
// ---------------------------------------------------------------------------

#[pyfunction(name = "vda_order_from_route")]
fn py_vda_order_from_route<'py>(
    py: Python<'py>,
    route_plan: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let plan: RoutePlan = obj_to(route_plan)?;
    let order = crate::vda::map_route_plan(&plan);
    obj_from(py, &order)
}

#[pyfunction(name = "vda_state_from_robot")]
fn py_vda_state_from_robot<'py>(
    py: Python<'py>,
    robot_state: &Bound<'_, PyAny>,
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
    self_priority: f64,
    other_priority: f64,
    self_holds_lease: bool,
    other_holds_lease: bool,
    self_is_emergency: bool,
    other_is_emergency: bool,
    self_state: &str,
    other_state: &str,
    self_wait_ticks: u64,
    other_wait_ticks: u64,
    self_remaining_steps: u64,
    other_remaining_steps: u64,
) -> &'static str {
    let ctx = ArbitrationContext {
        self_priority,
        other_priority,
        self_holds_lease,
        other_holds_lease,
        self_is_emergency,
        other_is_emergency,
        self_state: parse_progress_state(self_state),
        other_state: parse_progress_state(other_state),
        self_wait_ticks,
        other_wait_ticks,
        self_remaining_steps,
        other_remaining_steps,
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
    m.add_class::<PyWorkspaceBuilder>()?;
    m.add_class::<PyWorkspaceIndex>()?;
    m.add_class::<PyClaimManager>()?;
    m.add_class::<PyCoordinator>()?;

    // Enum string values for discoverability
    m.add(
        "ZONE_POLICY_KINDS",
        vec![
            zone_policy_kind_str(ZonePolicyKind::Informational),
            zone_policy_kind_str(ZonePolicyKind::ExclusiveAccess),
            zone_policy_kind_str(ZonePolicyKind::SharedAccess),
            zone_policy_kind_str(ZonePolicyKind::CapacityLimited),
            zone_policy_kind_str(ZonePolicyKind::Corridor),
            zone_policy_kind_str(ZonePolicyKind::Replanning),
            zone_policy_kind_str(ZonePolicyKind::Restricted),
            zone_policy_kind_str(ZonePolicyKind::NoStop),
            zone_policy_kind_str(ZonePolicyKind::Slowdown),
        ],
    )?;
    m.add(
        "ROBOT_PROGRESS_STATES",
        vec![
            "idle",
            "following_route",
            "waiting",
            "queued",
            "blocked",
            "replanning",
        ],
    )?;
    m.add("CLAIM_ACCESS_MODES", vec!["shared", "exclusive"])?;
    m.add("CLAIM_DECISIONS", vec!["grant", "deny"])?;
    m.add(
        "SCHEDULE_DECISION_KINDS",
        vec!["proceed", "queue", "replan"],
    )?;
    Ok(())
}

#[pymodule]
fn syncbot(m: &Bound<'_, PyModule>) -> PyResult<()> {
    register_python_module(m)
}
