//! Fast lookup layer over a `zoneout::Workspace` plus structural validation.
//!
//! Port of `include/syncbot/workspace_index.hpp`. Maintains by-UUID maps for
//! zones / nodes / edges, parent/child zone relationships, and a
//! `nodes_by_zone` index. Resolved zone references walk the tree via
//! `Workspace::find_zone`, which is O(tree-depth) and fine at this scale.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use concord::{Enu, Geo, to_enu, to_wgs_from_enu};
use datapod::Point;
use graphix::vertex::{EdgeId, VertexId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zoneout::{CoordMode, EdgeData, NodeData, Workspace, Zone};

use crate::core::error::{Error, Result};
use crate::policy::{
    EdgeTrafficSemantics, TrafficIssueSeverity, ZonePolicy, parse_edge_traffic_semantics,
    parse_zone_policy, validate_edge_traffic_properties, validate_zone_traffic_properties,
};

/// Property key for an optional numeric ID alias on zones / nodes / edges.
///
/// When a resource carries this property (parsed as `u64`), the index
/// builds a reverse lookup so legacy adapters can address it by integer
/// instead of UUID. The UUID remains the canonical identifier.
pub const NUMERIC_ID_PROPERTY: &str = "external.numeric_id";

/// Wire-level reference to a workspace resource — either a UUID or an
/// integer alias resolved against `WorkspaceIndex` via [`NUMERIC_ID_PROPERTY`].
///
/// On the wire, both variants are encoded as a text token to keep the form
/// consistent across JSON and XML transports. JSON output therefore quotes
/// numeric IDs (`"resource_id": "205"`). Deserialisation is tolerant: the
/// token is parsed first as a UUID, then as an unsigned integer.
///
/// `Serialize`/`Deserialize` are hand-written because `#[serde(untagged)]`
/// over a `(Uuid, u64)` does not work in XML, where every leaf is text and
/// the format cannot distinguish a string from a number at parse time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceRef {
    Uuid(Uuid),
    Numeric(u64),
}

impl Serialize for ResourceRef {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Uuid(u) => serializer.serialize_str(&u.to_string()),
            Self::Numeric(n) => serializer.collect_str(n),
        }
    }
}

impl<'de> Deserialize<'de> for ResourceRef {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = ResourceRef;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a UUID string or an unsigned integer (as text or number)")
            }

            fn visit_u64<E: serde::de::Error>(self, n: u64) -> std::result::Result<Self::Value, E> {
                Ok(ResourceRef::Numeric(n))
            }

            fn visit_i64<E: serde::de::Error>(self, n: i64) -> std::result::Result<Self::Value, E> {
                u64::try_from(n)
                    .map(ResourceRef::Numeric)
                    .map_err(|_| E::custom(format!("negative resource id {n}")))
            }

            fn visit_str<E: serde::de::Error>(
                self,
                s: &str,
            ) -> std::result::Result<Self::Value, E> {
                let s = s.trim();
                if let Ok(u) = Uuid::parse_str(s) {
                    return Ok(ResourceRef::Uuid(u));
                }
                if let Ok(n) = s.parse::<u64>() {
                    return Ok(ResourceRef::Numeric(n));
                }
                Err(E::custom(format!(
                    "resource id {s:?} is neither a UUID nor an unsigned integer"
                )))
            }

            fn visit_string<E: serde::de::Error>(
                self,
                s: String,
            ) -> std::result::Result<Self::Value, E> {
                self.visit_str(&s)
            }
        }
        // deserialize_string asks the format for a text token. quick-xml
        // returns the leaf element body; serde_json returns the JSON string.
        // JSON inputs that present a bare number would fail here; clients
        // should quote numeric resource IDs to keep cross-transport parity.
        deserializer.deserialize_string(V)
    }
}

impl ResourceRef {
    pub fn resolve_zone(self, idx: &WorkspaceIndex) -> Option<Uuid> {
        match self {
            Self::Uuid(u) => idx.zone(u).map(|_| u),
            Self::Numeric(n) => idx.zone_uuid_by_numeric_id(n),
        }
    }

    pub fn resolve_node(self, idx: &WorkspaceIndex) -> Option<Uuid> {
        match self {
            Self::Uuid(u) => idx.node(u).map(|_| u),
            Self::Numeric(n) => idx.node_uuid_by_numeric_id(n),
        }
    }

    pub fn resolve_edge(self, idx: &WorkspaceIndex) -> Option<Uuid> {
        match self {
            Self::Uuid(u) => idx.edge(u).map(|_| u),
            Self::Numeric(n) => idx.edge_uuid_by_numeric_id(n),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub severity: ValidationSeverity,
    pub category: String,
    pub resource_kind: String,
    pub resource_id: Option<Uuid>,
    pub message: String,
}

/// One traversable step out of a node, resolved once at index build time.
///
/// `from_source` records which way the edge is being walked, because a
/// direction-locked edge is legal in one direction and not the other.
#[derive(Debug, Clone, Copy)]
pub struct Adjacency {
    /// The node at the far end.
    pub node_id: Uuid,
    pub edge_id: Uuid,
    pub weight: f64,
    /// Whether walking this way leaves the edge's source endpoint.
    pub from_source: bool,
}

pub struct WorkspaceIndex {
    workspace: Arc<Workspace>,
    /// Node uuid → the edges leaving it. Built once per `rebuild`, because
    /// Dijkstra otherwise rescans every edge in the graph per node it expands,
    /// which is `O(V·E)` for what should be `O(E log V)`.
    adjacency: HashMap<Uuid, Vec<Adjacency>>,
    /// Parsed `traffic.*` policies, resolved once per `rebuild`.
    ///
    /// Parsing clones the whole property map, and claim evaluation re-parses
    /// the same few zones inside loops over requests x leases x targets. The
    /// workspace is immutable between rebinds, so caching is exact.
    zone_policies: HashMap<Uuid, ZonePolicy>,
    edge_semantics: HashMap<Uuid, EdgeTrafficSemantics>,
    /// Every zone's ancestors, so containment questions are a set lookup
    /// instead of a fresh `Vec` and a walk up the tree each time.
    zone_ancestors: HashMap<Uuid, HashSet<Uuid>>,
    zone_ids: HashSet<Uuid>,
    nodes: HashMap<Uuid, VertexId<NodeData>>,
    edges: HashMap<Uuid, EdgeId>,
    parents: HashMap<Uuid, Uuid>,
    children: HashMap<Uuid, Vec<Uuid>>,
    nodes_by_zone: HashMap<Uuid, Vec<VertexId<NodeData>>>,
    zones_by_numeric_id: HashMap<u64, Uuid>,
    nodes_by_numeric_id: HashMap<u64, Uuid>,
    edges_by_numeric_id: HashMap<u64, Uuid>,
    duplicate_zone_ids: Vec<Uuid>,
    duplicate_node_ids: Vec<Uuid>,
    duplicate_edge_ids: Vec<Uuid>,
    duplicate_zone_numeric_ids: Vec<u64>,
    duplicate_node_numeric_ids: Vec<u64>,
    duplicate_edge_numeric_ids: Vec<u64>,
}

impl WorkspaceIndex {
    /// Build an index over an `Arc<Workspace>` (shared ownership).
    pub fn new(workspace: Arc<Workspace>) -> Self {
        let mut idx = Self {
            workspace,
            adjacency: HashMap::new(),
            zone_policies: HashMap::new(),
            edge_semantics: HashMap::new(),
            zone_ancestors: HashMap::new(),
            zone_ids: HashSet::new(),
            nodes: HashMap::new(),
            edges: HashMap::new(),
            parents: HashMap::new(),
            children: HashMap::new(),
            nodes_by_zone: HashMap::new(),
            zones_by_numeric_id: HashMap::new(),
            nodes_by_numeric_id: HashMap::new(),
            edges_by_numeric_id: HashMap::new(),
            duplicate_zone_ids: Vec::new(),
            duplicate_node_ids: Vec::new(),
            duplicate_edge_ids: Vec::new(),
            duplicate_zone_numeric_ids: Vec::new(),
            duplicate_node_numeric_ids: Vec::new(),
            duplicate_edge_numeric_ids: Vec::new(),
        };
        idx.rebuild();
        idx
    }

    /// Convenience: build an index over a value-owned `Workspace` by wrapping
    /// it in an `Arc`.
    pub fn from_workspace(workspace: Workspace) -> Self {
        Self::new(Arc::new(workspace))
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }
    pub fn workspace_arc(&self) -> Arc<Workspace> {
        Arc::clone(&self.workspace)
    }

    pub fn refresh(&mut self) {
        self.rebuild();
    }

    pub fn rebind(&mut self, workspace: Arc<Workspace>) {
        self.workspace = workspace;
        self.rebuild();
    }

    pub fn root_zone(&self) -> &Zone {
        self.workspace.root_zone()
    }

    pub fn root_zone_id(&self) -> Option<Uuid> {
        Some(self.root_zone().id())
    }

    pub fn zone(&self, zone_id: Uuid) -> Option<&Zone> {
        if !self.zone_ids.contains(&zone_id) {
            return None;
        }
        self.workspace.find_zone(zone_id)
    }

    pub fn node(&self, node_id: Uuid) -> Option<&NodeData> {
        let vid = *self.nodes.get(&node_id)?;
        self.workspace.graph().get_vertex(vid)
    }

    pub fn edge(&self, edge_id: Uuid) -> Option<&EdgeData> {
        let eid = *self.edges.get(&edge_id)?;
        self.workspace.graph().edge_property(eid)
    }

    pub fn vertex_id(&self, node_id: Uuid) -> Option<VertexId<NodeData>> {
        self.nodes.get(&node_id).copied()
    }

    pub fn edge_id(&self, edge_id: Uuid) -> Option<EdgeId> {
        self.edges.get(&edge_id).copied()
    }

    pub fn parent_zone(&self, zone_id: Uuid) -> Option<&Zone> {
        let pid = *self.parents.get(&zone_id)?;
        self.workspace.find_zone(pid)
    }

    pub fn child_zones(&self, zone_id: Uuid) -> Vec<&Zone> {
        match self.children.get(&zone_id) {
            None => Vec::new(),
            Some(ids) => ids
                .iter()
                .filter_map(|id| self.workspace.find_zone(*id))
                .collect(),
        }
    }

    pub fn ancestor_zones(&self, zone_id: Uuid) -> Vec<&Zone> {
        let mut out = Vec::new();
        let mut current = self.parents.get(&zone_id).copied();
        while let Some(pid) = current {
            if let Some(z) = self.workspace.find_zone(pid) {
                out.push(z);
            }
            current = self.parents.get(&pid).copied();
        }
        out
    }

    /// Every zone below `zone_id`, breadth-first.
    ///
    /// Iterative on purpose: the zone tree can arrive from an untrusted
    /// workspace push, and recursion over attacker-controlled nesting is a
    /// stack overflow — which aborts the process rather than returning an
    /// error. Depth is bounded at the door instead (see `max_zone_depth`).
    pub fn descendant_zones(&self, zone_id: Uuid) -> Vec<&Zone> {
        let mut out = Vec::new();
        let mut pending = vec![zone_id];
        while let Some(current) = pending.pop() {
            for child in self.child_zones(current) {
                out.push(child);
                pending.push(child.id());
            }
        }
        out
    }

    /// How deeply the zone tree nests, counting the root as depth 1.
    ///
    /// Used to refuse an unreasonable pushed workspace before anything walks
    /// it. Computed iteratively for the same reason.
    pub fn max_zone_depth(&self) -> usize {
        let Some(root) = self.root_zone_id() else {
            return 0;
        };
        let mut deepest = 0usize;
        let mut pending = vec![(root, 1usize)];
        let mut visited: HashSet<Uuid> = HashSet::new();
        while let Some((zone_id, depth)) = pending.pop() {
            if !visited.insert(zone_id) {
                continue; // a malformed tree must not loop forever
            }
            deepest = deepest.max(depth);
            if let Some(children) = self.children.get(&zone_id) {
                for child in children {
                    pending.push((*child, depth + 1));
                }
            }
        }
        deepest
    }

    pub fn nodes_in_zone(&self, zone_id: Uuid) -> Vec<&NodeData> {
        let Some(vids) = self.nodes_by_zone.get(&zone_id) else {
            return Vec::new();
        };
        vids.iter()
            .filter_map(|vid| self.workspace.graph().get_vertex(*vid))
            .collect()
    }

    pub fn zones_of_node(&self, node_id: Uuid) -> Vec<&Zone> {
        match self.node(node_id) {
            None => Vec::new(),
            Some(node) => node
                .zone_ids
                .iter()
                .filter_map(|zid| self.workspace.find_zone(*zid))
                .collect(),
        }
    }

    pub fn zones_of_edge(&self, edge_id: Uuid) -> Vec<&Zone> {
        match self.edge(edge_id) {
            None => Vec::new(),
            Some(edge) => edge
                .zone_ids
                .iter()
                .filter_map(|zid| self.workspace.find_zone(*zid))
                .collect(),
        }
    }

    /// The edges leaving `node_id`, or an empty slice for an unknown node.
    pub fn neighbors(&self, node_id: Uuid) -> &[Adjacency] {
        self.adjacency
            .get(&node_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// This zone's parsed traffic policy. Prefer this to calling
    /// `parse_zone_policy` on the properties: it is the same value, resolved
    /// once at build time.
    pub fn zone_policy(&self, zone_id: Uuid) -> Option<&ZonePolicy> {
        self.zone_policies.get(&zone_id)
    }

    /// This edge's parsed traffic semantics, structural direction excluded.
    pub fn edge_semantics(&self, edge_id: Uuid) -> Option<&EdgeTrafficSemantics> {
        self.edge_semantics.get(&edge_id)
    }

    /// The parsed policies of every zone containing `node_id`.
    pub fn node_zone_policies(&self, node_id: Uuid) -> Vec<&ZonePolicy> {
        self.zones_of_node(node_id)
            .into_iter()
            .filter_map(|zone| self.zone_policy(zone.id()))
            .collect()
    }

    /// The parsed policies of every zone containing `edge_id`.
    pub fn edge_zone_policies(&self, edge_id: Uuid) -> Vec<&ZonePolicy> {
        self.zones_of_edge(edge_id)
            .into_iter()
            .filter_map(|zone| self.zone_policy(zone.id()))
            .collect()
    }

    /// Whether `ancestor` is `zone_id` itself or one of its ancestors.
    pub fn zone_contains(&self, ancestor: Uuid, zone_id: Uuid) -> bool {
        ancestor == zone_id
            || self
                .zone_ancestors
                .get(&zone_id)
                .is_some_and(|ancestors| ancestors.contains(&ancestor))
    }

    pub fn edge_between(&self, node_a_id: Uuid, node_b_id: Uuid) -> Option<&EdgeData> {
        let a = *self.nodes.get(&node_a_id)?;
        let b = *self.nodes.get(&node_b_id)?;
        let g = self.workspace.graph();
        if let Some(eid) = g.get_edge(a, b) {
            return g.edge_property(eid);
        }
        if let Some(eid) = g.get_edge(b, a) {
            return g.edge_property(eid);
        }
        None
    }

    pub fn datum(&self) -> Option<Geo> {
        self.workspace.datum().copied()
    }

    pub fn coord_mode(&self) -> CoordMode {
        self.workspace.coord_mode()
    }

    /// Local 3D point → global Geo. Requires `CoordMode::Local` and a
    /// reference origin on the workspace.
    pub fn local_to_global(&self, local_point: Point) -> Result<Geo> {
        if self.workspace.coord_mode() != CoordMode::Local {
            return Err(Error::invalid_argument(
                "local_to_global requires local coord_mode and a reference origin",
            ));
        }
        let Some(reference) = self.workspace.datum().copied() else {
            return Err(Error::invalid_argument(
                "local_to_global requires local coord_mode and a reference origin",
            ));
        };
        let enu = Enu::new(local_point.x, local_point.y, local_point.z, reference);
        Ok(to_wgs_from_enu(enu))
    }

    /// Global Geo → local 3D point. Same preconditions as `local_to_global`.
    pub fn global_to_local(&self, global_point: Geo) -> Result<Point> {
        if self.workspace.coord_mode() != CoordMode::Local {
            return Err(Error::invalid_argument(
                "global_to_local requires local coord_mode and a reference origin",
            ));
        }
        let Some(reference) = self.workspace.datum().copied() else {
            return Err(Error::invalid_argument(
                "global_to_local requires local coord_mode and a reference origin",
            ));
        };
        let enu = to_enu(reference, global_point);
        Ok(Point::new(enu.east(), enu.north(), enu.up()))
    }

    /// Local point → global, anchored on the datum alone.
    ///
    /// [`local_to_global`](Self::local_to_global) refuses outside
    /// `CoordMode::Local`, because in global mode the *workspace's* geometry is
    /// already lat/lon and a local frame for it would mean nothing. A robot's
    /// frame is a different question: it reports x/y/z against the datum
    /// whatever the workspace happens to store, so gating that on the
    /// workspace's storage mode would leave fleet positions unconvertible — and
    /// so incomparable — for no reason. Needs only a datum.
    pub fn pose_local_to_global(&self, local: Point) -> Option<Geo> {
        let reference = self.workspace.datum().copied()?;
        Some(to_wgs_from_enu(Enu::new(
            local.x, local.y, local.z, reference,
        )))
    }

    /// Global → local point, anchored on the datum alone. See
    /// [`pose_local_to_global`](Self::pose_local_to_global).
    pub fn pose_global_to_local(&self, global: Geo) -> Option<Point> {
        let reference = self.workspace.datum().copied()?;
        let enu = to_enu(reference, global);
        Some(Point::new(enu.east(), enu.north(), enu.up()))
    }

    pub fn zone_property(&self, zone_id: Uuid, key: &str) -> Option<String> {
        let zone = self.zone(zone_id)?;
        zone.property(key).cloned()
    }

    pub fn edge_property(&self, edge_id: Uuid, key: &str) -> Option<String> {
        let edge = self.edge(edge_id)?;
        edge.properties.get(key).cloned()
    }

    pub fn zone_uuid_by_numeric_id(&self, n: u64) -> Option<Uuid> {
        self.zones_by_numeric_id.get(&n).copied()
    }

    pub fn node_uuid_by_numeric_id(&self, n: u64) -> Option<Uuid> {
        self.nodes_by_numeric_id.get(&n).copied()
    }

    pub fn edge_uuid_by_numeric_id(&self, n: u64) -> Option<Uuid> {
        self.edges_by_numeric_id.get(&n).copied()
    }

    pub fn validation_issues(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();

        if self.workspace.coord_mode() == CoordMode::Local && !self.workspace.has_datum() {
            issues.push(ValidationIssue {
                severity: ValidationSeverity::Error,
                category: "invalid_reference".into(),
                resource_kind: "workspace".into(),
                resource_id: None,
                message: "workspace uses local coordinates without a reference origin".into(),
            });
        }
        if self.workspace.coord_mode() == CoordMode::Global && self.workspace.has_datum() {
            issues.push(ValidationIssue {
                severity: ValidationSeverity::Warning,
                category: "ignored_reference".into(),
                resource_kind: "workspace".into(),
                resource_id: None,
                message: "workspace reference origin is set but coord_mode is global; concord conversions are disabled".into(),
            });
        }

        for zid in &self.duplicate_zone_ids {
            issues.push(ValidationIssue {
                severity: ValidationSeverity::Error,
                category: "duplicate_id".into(),
                resource_kind: "zone".into(),
                resource_id: Some(*zid),
                message: "multiple zones share the same UUID".into(),
            });
        }
        for nid in &self.duplicate_node_ids {
            issues.push(ValidationIssue {
                severity: ValidationSeverity::Error,
                category: "duplicate_id".into(),
                resource_kind: "node".into(),
                resource_id: Some(*nid),
                message: "multiple nodes share the same UUID".into(),
            });
        }
        for eid in &self.duplicate_edge_ids {
            issues.push(ValidationIssue {
                severity: ValidationSeverity::Error,
                category: "duplicate_id".into(),
                resource_kind: "edge".into(),
                resource_id: Some(*eid),
                message: "multiple edges share the same UUID".into(),
            });
        }

        for n in &self.duplicate_zone_numeric_ids {
            issues.push(ValidationIssue {
                severity: ValidationSeverity::Error,
                category: "duplicate_numeric_id".into(),
                resource_kind: "zone".into(),
                resource_id: None,
                message: format!("multiple zones share numeric id {n}"),
            });
        }
        for n in &self.duplicate_node_numeric_ids {
            issues.push(ValidationIssue {
                severity: ValidationSeverity::Error,
                category: "duplicate_numeric_id".into(),
                resource_kind: "node".into(),
                resource_id: None,
                message: format!("multiple nodes share numeric id {n}"),
            });
        }
        for n in &self.duplicate_edge_numeric_ids {
            issues.push(ValidationIssue {
                severity: ValidationSeverity::Error,
                category: "duplicate_numeric_id".into(),
                resource_kind: "edge".into(),
                resource_id: None,
                message: format!("multiple edges share numeric id {n}"),
            });
        }

        // Zones — node membership consistency + traffic properties
        for &zid in &self.zone_ids {
            let Some(zone) = self.workspace.find_zone(zid) else {
                continue;
            };
            if zone.id() == Uuid::nil() {
                issues.push(ValidationIssue {
                    severity: ValidationSeverity::Error,
                    category: "missing_id".into(),
                    resource_kind: "zone".into(),
                    resource_id: None,
                    message: "zone is missing a non-null id".into(),
                });
            }
            for &node_id in zone.node_ids() {
                if self.node(node_id).is_none() {
                    issues.push(ValidationIssue {
                        severity: ValidationSeverity::Error,
                        category: "broken_membership".into(),
                        resource_kind: "zone".into(),
                        resource_id: Some(zid),
                        message:
                            "zone references node id that is not present in the workspace graph"
                                .into(),
                    });
                    continue;
                }
                if let Some(node) = self.node(node_id) {
                    if !node.zone_ids.contains(&zid) {
                        issues.push(ValidationIssue {
                            severity: ValidationSeverity::Error,
                            category: "inconsistent_membership".into(),
                            resource_kind: "zone".into(),
                            resource_id: Some(zid),
                            message:
                                "zone lists node membership but the node does not list the zone"
                                    .into(),
                        });
                    }
                }
            }

            for parse_issue in validate_zone_traffic_properties(zone.properties()) {
                issues.push(ValidationIssue {
                    severity: match parse_issue.severity {
                        TrafficIssueSeverity::Error => ValidationSeverity::Error,
                        TrafficIssueSeverity::Warning => ValidationSeverity::Warning,
                    },
                    category: "traffic_property".into(),
                    resource_kind: "zone".into(),
                    resource_id: Some(zid),
                    message: format!("{}: {}", parse_issue.key, parse_issue.message),
                });
            }
        }

        // Nodes — zone-id back-reference consistency
        for (&node_id, &vid) in &self.nodes {
            let Some(node) = self.workspace.graph().get_vertex(vid) else {
                continue;
            };
            if node_id == Uuid::nil() {
                issues.push(ValidationIssue {
                    severity: ValidationSeverity::Error,
                    category: "missing_id".into(),
                    resource_kind: "node".into(),
                    resource_id: None,
                    message: "node is missing a non-null id".into(),
                });
            }
            for zid in &node.zone_ids {
                if self.zone(*zid).is_none() {
                    issues.push(ValidationIssue {
                        severity: ValidationSeverity::Error,
                        category: "broken_membership".into(),
                        resource_kind: "node".into(),
                        resource_id: Some(node_id),
                        message:
                            "node references zone id that is not present in the workspace tree"
                                .into(),
                    });
                }
            }
        }

        // Edges — zone membership + traffic properties
        for (&edge_uuid, &eid) in &self.edges {
            let Some(edge) = self.workspace.graph().edge_property(eid) else {
                continue;
            };
            if edge_uuid == Uuid::nil() {
                issues.push(ValidationIssue {
                    severity: ValidationSeverity::Error,
                    category: "missing_id".into(),
                    resource_kind: "edge".into(),
                    resource_id: None,
                    message: "edge is missing a non-null id".into(),
                });
            }

            let g = self.workspace.graph();
            let source = g.source(eid);
            let target = g.target(eid);
            for zid in &edge.zone_ids {
                if self.zone(*zid).is_none() {
                    issues.push(ValidationIssue {
                        severity: ValidationSeverity::Error,
                        category: "broken_membership".into(),
                        resource_kind: "edge".into(),
                        resource_id: Some(edge_uuid),
                        message:
                            "edge references zone id that is not present in the workspace tree"
                                .into(),
                    });
                    continue;
                }
                let src_has = source
                    .and_then(|s| g.get_vertex(s))
                    .map(|n| n.zone_ids.contains(zid))
                    .unwrap_or(false);
                let tgt_has = target
                    .and_then(|t| g.get_vertex(t))
                    .map(|n| n.zone_ids.contains(zid))
                    .unwrap_or(false);
                if !src_has && !tgt_has {
                    issues.push(ValidationIssue {
                        severity: ValidationSeverity::Warning,
                        category: "inconsistent_membership".into(),
                        resource_kind: "edge".into(),
                        resource_id: Some(edge_uuid),
                        message: "edge lists a zone that is not present on either endpoint node"
                            .into(),
                    });
                }
            }

            for parse_issue in validate_edge_traffic_properties(&edge.properties) {
                issues.push(ValidationIssue {
                    severity: match parse_issue.severity {
                        TrafficIssueSeverity::Error => ValidationSeverity::Error,
                        TrafficIssueSeverity::Warning => ValidationSeverity::Warning,
                    },
                    category: "traffic_property".into(),
                    resource_kind: "edge".into(),
                    resource_id: Some(edge_uuid),
                    message: format!("{}: {}", parse_issue.key, parse_issue.message),
                });
            }
        }

        issues
    }

    pub fn is_valid(&self) -> bool {
        self.validation_issues().is_empty()
    }

    fn rebuild(&mut self) {
        self.adjacency.clear();
        self.zone_policies.clear();
        self.edge_semantics.clear();
        self.zone_ancestors.clear();
        self.zone_ids.clear();
        self.nodes.clear();
        self.edges.clear();
        self.parents.clear();
        self.children.clear();
        self.nodes_by_zone.clear();
        self.zones_by_numeric_id.clear();
        self.nodes_by_numeric_id.clear();
        self.edges_by_numeric_id.clear();
        self.duplicate_zone_ids.clear();
        self.duplicate_node_ids.clear();
        self.duplicate_edge_ids.clear();
        self.duplicate_zone_numeric_ids.clear();
        self.duplicate_node_numeric_ids.clear();
        self.duplicate_edge_numeric_ids.clear();

        Self::index_zone_tree(
            self.workspace.root_zone(),
            &mut self.zone_ids,
            &mut self.parents,
            &mut self.children,
            &mut self.duplicate_zone_ids,
        );

        let g = self.workspace.graph();
        for vid in g.vertices() {
            if let Some(node) = g.get_vertex(vid) {
                if !node.id.is_nil() && self.nodes.contains_key(&node.id) {
                    if !self.duplicate_node_ids.contains(&node.id) {
                        self.duplicate_node_ids.push(node.id);
                    }
                } else {
                    self.nodes.insert(node.id, vid);
                }
                for zid in &node.zone_ids {
                    self.nodes_by_zone.entry(*zid).or_default().push(vid);
                }
                if let Some(n) = parse_numeric_id(node.properties.get(NUMERIC_ID_PROPERTY)) {
                    insert_numeric_id(
                        n,
                        node.id,
                        &mut self.nodes_by_numeric_id,
                        &mut self.duplicate_node_numeric_ids,
                    );
                }
            }
        }

        for edge in g.edges() {
            if let Some(prop) = g.edge_property(edge.id) {
                if !prop.id.is_nil() && self.edges.contains_key(&prop.id) {
                    if !self.duplicate_edge_ids.contains(&prop.id) {
                        self.duplicate_edge_ids.push(prop.id);
                    }
                } else {
                    self.edges.insert(prop.id, edge.id);
                }
                if let Some(n) = parse_numeric_id(prop.properties.get(NUMERIC_ID_PROPERTY)) {
                    insert_numeric_id(
                        n,
                        prop.id,
                        &mut self.edges_by_numeric_id,
                        &mut self.duplicate_edge_numeric_ids,
                    );
                }
            }
        }

        for edge in g.edges() {
            let (Some(src), Some(tgt)) = (g.source(edge.id), g.target(edge.id)) else {
                continue;
            };
            let (Some(src_node), Some(tgt_node)) = (g.get_vertex(src), g.get_vertex(tgt)) else {
                continue;
            };
            let Some(prop) = g.edge_property(edge.id) else {
                continue;
            };
            let weight = g.get_weight(edge.id).unwrap_or(0.0);
            self.adjacency
                .entry(src_node.id)
                .or_default()
                .push(Adjacency {
                    node_id: tgt_node.id,
                    edge_id: prop.id,
                    weight,
                    from_source: true,
                });
            // A self-loop is one step out of the node, not two.
            if src != tgt {
                self.adjacency
                    .entry(tgt_node.id)
                    .or_default()
                    .push(Adjacency {
                        node_id: src_node.id,
                        edge_id: prop.id,
                        weight,
                        from_source: false,
                    });
            }
        }

        for &zid in &self.zone_ids {
            let Some(zone) = self.workspace.find_zone(zid) else {
                continue;
            };
            if let Some(n) = parse_numeric_id(zone.property(NUMERIC_ID_PROPERTY)) {
                insert_numeric_id(
                    n,
                    zid,
                    &mut self.zones_by_numeric_id,
                    &mut self.duplicate_zone_numeric_ids,
                );
            }
            self.zone_policies
                .insert(zid, parse_zone_policy(zone.properties()));

            let mut ancestors = HashSet::new();
            let mut current = self.parents.get(&zid).copied();
            while let Some(parent) = current {
                if !ancestors.insert(parent) {
                    break; // a cycle in a malformed tree must not hang the build
                }
                current = self.parents.get(&parent).copied();
            }
            self.zone_ancestors.insert(zid, ancestors);
        }

        for (&edge_uuid, &eid) in &self.edges {
            if let Some(prop) = self.workspace.graph().edge_property(eid) {
                self.edge_semantics.insert(
                    edge_uuid,
                    parse_edge_traffic_semantics(&prop.properties, false),
                );
            }
        }
    }

    /// Walk the zone tree, recording ids and parent/child links.
    ///
    /// An explicit worklist rather than recursion: this runs over a workspace
    /// that may have arrived from an untrusted push, where deep nesting would
    /// otherwise overflow the stack and abort the process.
    fn index_zone_tree(
        root: &Zone,
        zone_ids: &mut HashSet<Uuid>,
        parents: &mut HashMap<Uuid, Uuid>,
        children: &mut HashMap<Uuid, Vec<Uuid>>,
        duplicate_zone_ids: &mut Vec<Uuid>,
    ) {
        let mut pending: Vec<(&Zone, Option<Uuid>)> = vec![(root, None)];
        while let Some((zone, parent)) = pending.pop() {
            let zid = zone.id();
            if zone_ids.contains(&zid) {
                if !duplicate_zone_ids.contains(&zid) {
                    duplicate_zone_ids.push(zid);
                }
            } else {
                zone_ids.insert(zid);
            }
            if let Some(parent_id) = parent {
                parents.insert(zid, parent_id);
                children.entry(parent_id).or_default().push(zid);
            }
            for child in zone.children() {
                pending.push((child, Some(zid)));
            }
        }
    }
}

fn parse_numeric_id(raw: Option<&String>) -> Option<u64> {
    raw?.trim().parse::<u64>().ok()
}

fn insert_numeric_id(n: u64, uuid: Uuid, map: &mut HashMap<u64, Uuid>, duplicates: &mut Vec<u64>) {
    if let Some(existing) = map.get(&n) {
        if *existing != uuid && !duplicates.contains(&n) {
            duplicates.push(n);
        }
    } else {
        map.insert(n, uuid);
    }
}
