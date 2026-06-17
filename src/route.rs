//! Route planning over a `WorkspaceIndex`.
//!
//! Port of `include/syncbot/route.hpp`. Three Dijkstra variants share a
//! single inner engine — neighbour iteration is wrapped in
//! `GraphTraversalAdapter`, edge filtering and per-edge extra cost are
//! supplied as closures.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zoneout::EdgeData;

use crate::core::error::{Error, Result};
use crate::index::WorkspaceIndex;
use crate::policy::{
    ZonePolicy, ZonePolicyKind, derive_effective_edge_semantics, parse_edge_traffic_semantics,
    parse_zone_policy,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RouteStep {
    pub node_id: Uuid,
    pub incoming_edge_id: Option<Uuid>,
    pub step_cost: f64,
    pub cumulative_cost: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RoutePlan {
    pub start_node_id: Uuid,
    pub goal_node_id: Uuid,
    pub steps: Vec<RouteStep>,
    pub traversed_node_ids: Vec<Uuid>,
    pub traversed_edge_ids: Vec<Uuid>,
    pub traversed_zone_ids: Vec<Uuid>,
    pub traversed_node_zone_ids: Vec<Vec<Uuid>>,
    pub traversed_edge_zone_ids: Vec<Vec<Uuid>>,
    pub total_cost: f64,
}

#[derive(Debug, Clone)]
pub struct RouteSearchState {
    pub found: bool,
    pub distance: f64,
    pub distances: HashMap<Uuid, f64>,
    pub predecessors: HashMap<Uuid, Uuid>,
}

impl Default for RouteSearchState {
    fn default() -> Self {
        Self {
            found: false,
            distance: f64::INFINITY,
            distances: HashMap::new(),
            predecessors: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteFailureKind {
    MissingStartNode,
    MissingGoalNode,
    PolicyBlocked,
    Unreachable,
}

impl Default for RouteFailureKind {
    fn default() -> Self {
        Self::Unreachable
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RouteFailure {
    pub kind: RouteFailureKind,
    pub message: String,
    pub blocked_edge_ids: Vec<Uuid>,
    pub blocked_zone_ids: Vec<Uuid>,
    pub directionally_blocked_edge_ids: Vec<Uuid>,
    pub reachable_node_ids: Vec<Uuid>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RoutePlanningResult {
    pub search: RouteSearchState,
    pub plan: Option<RoutePlan>,
    pub failure: Option<RouteFailure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteCostModel {
    GraphWeight,
    Penalized,
}

#[derive(Debug, Clone)]
struct TraversalNeighbor {
    node_id: Uuid,
    edge_id: Uuid,
    weight: f64,
}

// ---------------------------------------------------------------------------
// edge / traversal predicates (free functions, mirror C++)
// ---------------------------------------------------------------------------

pub fn allows_traversal_from_node(edge: &EdgeData, from_source: bool) -> bool {
    let semantics = parse_edge_traffic_semantics(&edge.properties, false);
    let Some(direction_raw) = semantics.preferred_direction.as_ref() else {
        return true;
    };
    let direction = direction_raw.trim().to_ascii_lowercase();
    match direction.as_str() {
        "forward" | "source_to_target" => from_source || semantics.reversible.unwrap_or(false),
        "reverse" | "target_to_source" => !from_source || semantics.reversible.unwrap_or(false),
        "bidirectional" => true,
        _ => true,
    }
}

pub fn blocked_zones_for_edge(index: &WorkspaceIndex, edge_id: Uuid) -> Vec<Uuid> {
    let mut out = Vec::new();
    for zone in index.zones_of_edge(edge_id) {
        let policy = parse_zone_policy(zone.properties());
        if policy.blocked.unwrap_or(false)
            || policy.kind == ZonePolicyKind::Restricted
            || policy.blocks_entry_without_grant
            || policy.blocks_traversal_without_grant
        {
            out.push(zone.id());
        }
    }
    out
}

pub fn is_edge_hard_blocked(index: &WorkspaceIndex, edge_id: Uuid) -> bool {
    let Some(edge) = index.edge(edge_id) else {
        return true;
    };
    let semantics = parse_edge_traffic_semantics(&edge.properties, false);
    if semantics.blocked.unwrap_or(false) || semantics.no_stop.unwrap_or(false) {
        return true;
    }
    !blocked_zones_for_edge(index, edge_id).is_empty()
}

pub fn edge_traversal_penalty(index: &WorkspaceIndex, edge_id: Uuid) -> f64 {
    let Some(edge) = index.edge(edge_id) else {
        return f64::INFINITY;
    };
    let zone_policies: Vec<ZonePolicy> = index
        .zones_of_edge(edge_id)
        .into_iter()
        .map(|z| parse_zone_policy(z.properties()))
        .collect();

    let semantics = derive_effective_edge_semantics(&edge.properties, false, &zone_policies);

    let mut penalty = 0.0;
    if let Some(b) = semantics.cost_bias {
        penalty += b.max(0.0);
    }
    if let Some(s) = semantics.speed_limit {
        if s > 0.0 {
            penalty += 1.0 / s;
        }
    }
    if let Some(p) = semantics.priority {
        penalty += (10.0 - p).max(0.0) * 0.1;
    }
    if let Some(c) = semantics.capacity {
        if c > 0 {
            penalty += 1.0 / c as f64;
        }
    }
    if semantics.lane_type.as_deref() == Some("corridor") {
        penalty += 0.75;
    }
    if semantics.passing_allowed == Some(false) {
        penalty += 0.5;
    }
    if semantics.directed && !semantics.reversible.unwrap_or(false) {
        penalty += 0.25;
    }
    if semantics.no_stop.unwrap_or(false) {
        penalty += 2.0;
    }
    if let Some(w) = semantics.clearance_width {
        if w > 0.0 {
            penalty += 1.0 / w;
        }
    }
    if let Some(h) = semantics.clearance_height {
        if h > 0.0 {
            penalty += 1.0 / h;
        }
    }

    for zp in &zone_policies {
        if zp.requires_claim || zp.blocks_entry_without_grant || zp.blocks_traversal_without_grant {
            penalty += 100.0;
        }
        if let Some(p) = zp.priority {
            penalty += (10.0 - p).max(0.0) * 0.05;
        }
        if zp.waiting_allowed == Some(false) {
            penalty += 1.5;
        }
        if zp.stop_allowed == Some(false) {
            penalty += 2.0;
        }
        if zp.kind == ZonePolicyKind::Corridor {
            penalty += 0.75;
        }
    }

    penalty
}

// ---------------------------------------------------------------------------
// graph traversal (Dijkstra inner engine)
// ---------------------------------------------------------------------------

fn neighbors_of(index: &WorkspaceIndex, node_id: Uuid) -> Vec<TraversalNeighbor> {
    let workspace = index.workspace();
    let g = workspace.graph();
    let Some(vid) = workspace.find_node(node_id) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for edge in g.edges() {
        let (Some(src), Some(tgt)) = (g.source(edge.id), g.target(edge.id)) else {
            continue;
        };
        if src != vid && tgt != vid {
            continue;
        }
        let from_source = src == vid;

        let Some(prop) = g.edge_property(edge.id) else {
            continue;
        };
        if !allows_traversal_from_node(prop, from_source) {
            continue;
        }

        let other = if from_source { tgt } else { src };
        let Some(other_node) = g.get_vertex(other) else {
            continue;
        };
        let weight = g.get_weight(edge.id).unwrap_or(0.0);
        out.push(TraversalNeighbor {
            node_id: other_node.id,
            edge_id: prop.id,
            weight,
        });
    }
    out
}

#[derive(Clone, Copy)]
struct QueueEntry {
    node_id: Uuid,
    distance: f64,
}

impl PartialEq for QueueEntry {
    fn eq(&self, other: &Self) -> bool {
        self.distance.eq(&other.distance)
    }
}
impl Eq for QueueEntry {}
impl PartialOrd for QueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for QueueEntry {
    // Reverse: smaller distance is "greater" so BinaryHeap (max-heap) yields
    // smallest first.
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .distance
            .partial_cmp(&self.distance)
            .unwrap_or(Ordering::Equal)
    }
}

fn dijkstra_inner(
    index: &WorkspaceIndex,
    start: Uuid,
    goal: Uuid,
    skip_edge: impl Fn(Uuid) -> bool,
    extra_cost: impl Fn(Uuid) -> f64,
) -> RouteSearchState {
    let mut state = RouteSearchState::default();
    if index.node(start).is_none() || index.node(goal).is_none() {
        return state;
    }
    if start == goal {
        state.found = true;
        state.distance = 0.0;
        state.distances.insert(start, 0.0);
        return state;
    }

    let mut frontier: BinaryHeap<QueueEntry> = BinaryHeap::new();
    let mut visited: HashSet<Uuid> = HashSet::new();
    state.distances.insert(start, 0.0);
    frontier.push(QueueEntry {
        node_id: start,
        distance: 0.0,
    });

    while let Some(current) = frontier.pop() {
        if !visited.insert(current.node_id) {
            continue;
        }
        if current.node_id == goal {
            state.found = true;
            state.distance = current.distance;
            return state;
        }
        for nb in neighbors_of(index, current.node_id) {
            if visited.contains(&nb.node_id) {
                continue;
            }
            if skip_edge(nb.edge_id) {
                continue;
            }
            let new_distance = current.distance + nb.weight + extra_cost(nb.edge_id);
            let better = match state.distances.get(&nb.node_id) {
                None => true,
                Some(prev) => new_distance < *prev,
            };
            if better {
                state.distances.insert(nb.node_id, new_distance);
                state.predecessors.insert(nb.node_id, current.node_id);
                frontier.push(QueueEntry {
                    node_id: nb.node_id,
                    distance: new_distance,
                });
            }
        }
    }
    state
}

pub fn shortest_path_search(index: &WorkspaceIndex, start: Uuid, goal: Uuid) -> RouteSearchState {
    dijkstra_inner(index, start, goal, |_| false, |_| 0.0)
}

pub fn shortest_path_search_with_blocking(
    index: &WorkspaceIndex,
    start: Uuid,
    goal: Uuid,
) -> RouteSearchState {
    dijkstra_inner(
        index,
        start,
        goal,
        |edge_id| is_edge_hard_blocked(index, edge_id),
        |_| 0.0,
    )
}

pub fn shortest_path_search_with_penalties(
    index: &WorkspaceIndex,
    start: Uuid,
    goal: Uuid,
) -> RouteSearchState {
    dijkstra_inner(
        index,
        start,
        goal,
        |edge_id| is_edge_hard_blocked(index, edge_id),
        |edge_id| edge_traversal_penalty(index, edge_id),
    )
}

// ---------------------------------------------------------------------------
// reconstruction & extraction
// ---------------------------------------------------------------------------

fn reconstruct_route_nodes(search: &RouteSearchState, start: Uuid, goal: Uuid) -> Vec<Uuid> {
    let mut nodes = Vec::new();
    if !search.found {
        return nodes;
    }

    let mut current = goal;
    nodes.push(current);
    while current != start {
        let Some(&pred) = search.predecessors.get(&current) else {
            return Vec::new();
        };
        current = pred;
        nodes.push(current);
    }
    nodes.reverse();
    nodes
}

pub fn extract_traversed_node_ids(
    search: &RouteSearchState,
    start: Uuid,
    goal: Uuid,
) -> Result<Vec<Uuid>> {
    if !search.found {
        return Ok(Vec::new());
    }
    let nodes = reconstruct_route_nodes(search, start, goal);
    if nodes.is_empty() && start != goal {
        return Err(Error::not_found(
            "route reconstruction could not recover a predecessor chain",
        ));
    }
    Ok(nodes)
}

pub fn extract_traversed_edge_ids_from_nodes(
    index: &WorkspaceIndex,
    route_nodes: &[Uuid],
) -> Result<Vec<Uuid>> {
    let mut out = Vec::new();
    if route_nodes.len() < 2 {
        return Ok(out);
    }
    for i in 1..route_nodes.len() {
        let Some(edge) = index.edge_between(route_nodes[i - 1], route_nodes[i]) else {
            return Err(Error::not_found(
                "route references adjacent nodes without a graph edge",
            ));
        };
        out.push(edge.id);
    }
    Ok(out)
}

pub fn extract_traversed_edge_ids(
    index: &WorkspaceIndex,
    search: &RouteSearchState,
    start: Uuid,
    goal: Uuid,
) -> Result<Vec<Uuid>> {
    let nodes = extract_traversed_node_ids(search, start, goal)?;
    extract_traversed_edge_ids_from_nodes(index, &nodes)
}

pub fn extract_traversed_zone_ids_from_nodes(
    index: &WorkspaceIndex,
    route_nodes: &[Uuid],
) -> Result<Vec<Uuid>> {
    let mut traversed = Vec::new();
    let mut seen: HashSet<Uuid> = HashSet::new();
    for node_id in route_nodes {
        for zone in index.zones_of_node(*node_id) {
            if seen.insert(zone.id()) {
                traversed.push(zone.id());
            }
        }
    }
    let edges = extract_traversed_edge_ids_from_nodes(index, route_nodes)?;
    for edge_id in edges {
        for zone in index.zones_of_edge(edge_id) {
            if seen.insert(zone.id()) {
                traversed.push(zone.id());
            }
        }
    }
    Ok(traversed)
}

pub fn extract_traversed_zone_ids(
    index: &WorkspaceIndex,
    search: &RouteSearchState,
    start: Uuid,
    goal: Uuid,
) -> Result<Vec<Uuid>> {
    let nodes = extract_traversed_node_ids(search, start, goal)?;
    extract_traversed_zone_ids_from_nodes(index, &nodes)
}

// ---------------------------------------------------------------------------
// cost accumulation
// ---------------------------------------------------------------------------

pub fn accumulate_route_cost(
    index: &WorkspaceIndex,
    route_nodes: &[Uuid],
    cost_model: RouteCostModel,
) -> Result<f64> {
    if route_nodes.is_empty() {
        return Ok(0.0);
    }
    let workspace = index.workspace();

    let mut total = 0.0;
    for i in 1..route_nodes.len() {
        let from = workspace.find_node(route_nodes[i - 1]);
        let to = workspace.find_node(route_nodes[i]);
        let (Some(from), Some(to)) = (from, to) else {
            return Err(Error::not_found(
                "route references node that is not present in the workspace",
            ));
        };
        let g = workspace.graph();
        let edge_id = g.get_edge(from, to).or_else(|| g.get_edge(to, from));
        let Some(edge_id) = edge_id else {
            return Err(Error::not_found(
                "route references adjacent nodes without a graph edge",
            ));
        };
        total += g.get_weight(edge_id).unwrap_or(0.0);
        if cost_model == RouteCostModel::Penalized {
            if let Some(prop) = g.edge_property(edge_id) {
                total += edge_traversal_penalty(index, prop.id);
            }
        }
    }
    Ok(total)
}

// ---------------------------------------------------------------------------
// step / plan reconstruction
// ---------------------------------------------------------------------------

pub fn reconstruct_route_steps(
    index: &WorkspaceIndex,
    search: &RouteSearchState,
    start: Uuid,
    goal: Uuid,
    cost_model: RouteCostModel,
) -> Result<Vec<RouteStep>> {
    let nodes = reconstruct_route_nodes(search, start, goal);
    let mut steps = Vec::new();
    if nodes.is_empty() && !(search.found && start == goal) {
        return Ok(steps);
    }

    let mut cumulative = 0.0;
    for (i, node_id) in nodes.iter().enumerate() {
        let mut step = RouteStep {
            node_id: *node_id,
            ..RouteStep::default()
        };
        if i > 0 {
            let edge = index.edge_between(nodes[i - 1], nodes[i]).ok_or_else(|| {
                Error::not_found(
                    "route reconstruction references adjacent nodes without a graph edge",
                )
            })?;
            step.incoming_edge_id = Some(edge.id);
            let partial = vec![nodes[i - 1], nodes[i]];
            step.step_cost = accumulate_route_cost(index, &partial, cost_model)?;
            cumulative += step.step_cost;
            step.cumulative_cost = cumulative;
        }
        steps.push(step);
    }
    Ok(steps)
}

// ---------------------------------------------------------------------------
// validate / build
// ---------------------------------------------------------------------------

pub fn validate_route_plan_shape(plan: &RoutePlan) -> Result<u64> {
    if plan.traversed_node_ids.is_empty() {
        if !plan.traversed_edge_ids.is_empty() || !plan.steps.is_empty() {
            return Err(Error::invalid_argument(
                "route plan contains edges or steps without any traversed nodes",
            ));
        }
        return Ok(0);
    }

    if plan.start_node_id != *plan.traversed_node_ids.first().unwrap() {
        return Err(Error::invalid_argument(
            "route plan start node does not match the first traversed node",
        ));
    }
    if plan.goal_node_id != *plan.traversed_node_ids.last().unwrap() {
        return Err(Error::invalid_argument(
            "route plan goal node does not match the last traversed node",
        ));
    }
    if plan.traversed_edge_ids.len() + 1 != plan.traversed_node_ids.len() {
        return Err(Error::invalid_argument(
            "route plan edge count must equal node count minus one",
        ));
    }
    if !plan.steps.is_empty() && plan.steps.len() != plan.traversed_node_ids.len() {
        return Err(Error::invalid_argument(
            "route plan step count must equal traversed node count",
        ));
    }
    if !plan.traversed_node_zone_ids.is_empty()
        && plan.traversed_node_zone_ids.len() != plan.traversed_node_ids.len()
    {
        return Err(Error::invalid_argument(
            "route plan node-zone coverage must align with traversed nodes",
        ));
    }
    if !plan.traversed_edge_zone_ids.is_empty()
        && plan.traversed_edge_zone_ids.len() != plan.traversed_edge_ids.len()
    {
        return Err(Error::invalid_argument(
            "route plan edge-zone coverage must align with traversed edges",
        ));
    }

    for (i, step) in plan.steps.iter().enumerate() {
        if step.node_id != plan.traversed_node_ids[i] {
            return Err(Error::invalid_argument(
                "route plan step nodes must match traversed node sequence",
            ));
        }
        if i == 0 {
            if step.incoming_edge_id.is_some() {
                return Err(Error::invalid_argument(
                    "route plan first step cannot have an incoming edge",
                ));
            }
            continue;
        }
        match step.incoming_edge_id {
            Some(eid) if eid == plan.traversed_edge_ids[i - 1] => {}
            _ => {
                return Err(Error::invalid_argument(
                    "route plan step incoming edges must match traversed edges",
                ));
            }
        }
    }

    Ok(plan.traversed_edge_ids.len() as u64)
}

pub fn build_route_plan(
    index: &WorkspaceIndex,
    start: Uuid,
    goal: Uuid,
    route_nodes: &[Uuid],
    cost_model: RouteCostModel,
) -> Result<RoutePlan> {
    let mut plan = RoutePlan {
        start_node_id: start,
        goal_node_id: goal,
        traversed_node_ids: route_nodes.to_vec(),
        ..RoutePlan::default()
    };

    if !route_nodes.is_empty() {
        if route_nodes.first() != Some(&start) {
            return Err(Error::invalid_argument(
                "route node sequence does not begin at the requested start node",
            ));
        }
        if route_nodes.last() != Some(&goal) {
            return Err(Error::invalid_argument(
                "route node sequence does not end at the requested goal node",
            ));
        }
    }

    plan.traversed_edge_ids = extract_traversed_edge_ids_from_nodes(index, route_nodes)?;
    plan.traversed_zone_ids = extract_traversed_zone_ids_from_nodes(index, route_nodes)?;

    for &node_id in route_nodes {
        let zones: Vec<Uuid> = index
            .zones_of_node(node_id)
            .into_iter()
            .map(|z| z.id())
            .collect();
        plan.traversed_node_zone_ids.push(zones);
    }
    for &edge_id in &plan.traversed_edge_ids {
        let zones: Vec<Uuid> = index
            .zones_of_edge(edge_id)
            .into_iter()
            .map(|z| z.id())
            .collect();
        plan.traversed_edge_zone_ids.push(zones);
    }

    plan.total_cost = accumulate_route_cost(index, route_nodes, cost_model)?;

    let mut cumulative = 0.0;
    for (i, &node_id) in route_nodes.iter().enumerate() {
        let mut step = RouteStep {
            node_id,
            ..RouteStep::default()
        };
        if i > 0 {
            step.incoming_edge_id = Some(plan.traversed_edge_ids[i - 1]);
            let partial = vec![route_nodes[i - 1], route_nodes[i]];
            step.step_cost = accumulate_route_cost(index, &partial, cost_model)?;
            cumulative += step.step_cost;
            step.cumulative_cost = cumulative;
        }
        plan.steps.push(step);
    }

    validate_route_plan_shape(&plan)?;
    Ok(plan)
}

pub fn build_route_plan_from_search(
    index: &WorkspaceIndex,
    search: &RouteSearchState,
    start: Uuid,
    goal: Uuid,
    cost_model: RouteCostModel,
) -> Result<RoutePlan> {
    let route_nodes = extract_traversed_node_ids(search, start, goal)?;
    build_route_plan(index, start, goal, &route_nodes, cost_model)
}

// ---------------------------------------------------------------------------
// failure diagnosis
// ---------------------------------------------------------------------------

pub fn diagnose_route_failure(index: &WorkspaceIndex, start: Uuid, goal: Uuid) -> RouteFailure {
    if index.node(start).is_none() {
        return RouteFailure {
            kind: RouteFailureKind::MissingStartNode,
            message: "start node is not present in workspace graph".into(),
            diagnostics: vec!["planner cannot start without a valid start node".into()],
            ..RouteFailure::default()
        };
    }
    if index.node(goal).is_none() {
        return RouteFailure {
            kind: RouteFailureKind::MissingGoalNode,
            message: "goal node is not present in workspace graph".into(),
            diagnostics: vec!["planner cannot finish without a valid goal node".into()],
            ..RouteFailure::default()
        };
    }

    let unconstrained = shortest_path_search(index, start, goal);
    if unconstrained.found {
        let blocked_search = shortest_path_search_with_blocking(index, start, goal);
        let mut blocked_edge_ids: Vec<Uuid> = Vec::new();
        let mut blocked_zone_ids: Vec<Uuid> = Vec::new();
        let mut seen_blocked_edges: HashSet<Uuid> = HashSet::new();
        let mut seen_blocked_zones: HashSet<Uuid> = HashSet::new();

        let mut reachable_node_ids: Vec<Uuid> = blocked_search.distances.keys().copied().collect();
        if reachable_node_ids.is_empty() {
            reachable_node_ids.push(start);
        }

        let mut diagnostics = Vec::new();
        let mut saw_restricted_resource = false;
        let mut saw_slow_resource = false;
        for &node_id in &reachable_node_ids {
            for nb in neighbors_of(index, node_id) {
                if !is_edge_hard_blocked(index, nb.edge_id) {
                    if let Some(edge) = index.edge(nb.edge_id) {
                        let zone_policies: Vec<ZonePolicy> = index
                            .zones_of_edge(nb.edge_id)
                            .into_iter()
                            .map(|z| parse_zone_policy(z.properties()))
                            .collect();
                        let semantics = derive_effective_edge_semantics(
                            &edge.properties,
                            false,
                            &zone_policies,
                        );
                        if semantics.requires_claim.unwrap_or(false)
                            || semantics.access_group.is_some()
                        {
                            saw_restricted_resource = true;
                        }
                        if let Some(s) = semantics.speed_limit {
                            if s < 1.0 {
                                saw_slow_resource = true;
                            }
                        }
                    }
                    continue;
                }

                if seen_blocked_edges.insert(nb.edge_id) {
                    blocked_edge_ids.push(nb.edge_id);
                }
                for zone_id in blocked_zones_for_edge(index, nb.edge_id) {
                    if seen_blocked_zones.insert(zone_id) {
                        blocked_zone_ids.push(zone_id);
                    }
                }
            }
        }

        if blocked_edge_ids.is_empty() {
            let unconstrained_nodes = reconstruct_route_nodes(&unconstrained, start, goal);
            if let Ok(unconstrained_edges) =
                extract_traversed_edge_ids_from_nodes(index, &unconstrained_nodes)
            {
                for edge_id in unconstrained_edges {
                    if !is_edge_hard_blocked(index, edge_id) {
                        continue;
                    }
                    if seen_blocked_edges.insert(edge_id) {
                        blocked_edge_ids.push(edge_id);
                    }
                    for zone_id in blocked_zones_for_edge(index, edge_id) {
                        if seen_blocked_zones.insert(zone_id) {
                            blocked_zone_ids.push(zone_id);
                        }
                    }
                }
            }
        }

        if saw_restricted_resource {
            diagnostics.push(
                "restricted resources were present on reachable alternatives but require claims"
                    .into(),
            );
        }
        if saw_slow_resource {
            diagnostics
                .push("slowdown policies affect costs but do not hard-block planning".into());
        }
        diagnostics.push(
            "blocked or restricted resources must be claimed; slowdown only increases cost".into(),
        );

        let mut message = String::from("route is blocked by traffic policy");
        if !blocked_edge_ids.is_empty() || !blocked_zone_ids.is_empty() {
            message.push_str(&format!(
                " ({} blocked edge(s), {} blocked zone(s))",
                blocked_edge_ids.len(),
                blocked_zone_ids.len(),
            ));
        }

        return RouteFailure {
            kind: RouteFailureKind::PolicyBlocked,
            message,
            blocked_edge_ids,
            blocked_zone_ids,
            directionally_blocked_edge_ids: Vec::new(),
            reachable_node_ids,
            diagnostics,
        };
    }

    // Unreachable
    let mut reachable_node_ids: Vec<Uuid> = unconstrained.distances.keys().copied().collect();
    if reachable_node_ids.is_empty() && index.node(start).is_some() {
        reachable_node_ids.push(start);
    }

    let mut directionally_blocked_edge_ids: Vec<Uuid> = Vec::new();
    let mut seen_directional: HashSet<Uuid> = HashSet::new();
    let workspace = index.workspace();
    let g = workspace.graph();
    for &node_id in &reachable_node_ids {
        let Some(vid) = workspace.find_node(node_id) else {
            continue;
        };
        for edge in g.edges() {
            let (Some(src), Some(tgt)) = (g.source(edge.id), g.target(edge.id)) else {
                continue;
            };
            if src != vid && tgt != vid {
                continue;
            }
            let from_source = src == vid;
            let Some(prop) = g.edge_property(edge.id) else {
                continue;
            };
            if allows_traversal_from_node(prop, from_source) {
                continue;
            }
            if seen_directional.insert(prop.id) {
                directionally_blocked_edge_ids.push(prop.id);
            }
        }
    }

    let mut message = format!(
        "goal is unreachable from start after reaching {} node(s)",
        reachable_node_ids.len(),
    );
    if !directionally_blocked_edge_ids.is_empty() {
        message.push_str(&format!(
            " with {} direction-locked edge(s)",
            directionally_blocked_edge_ids.len(),
        ));
    }

    let mut diagnostics = Vec::new();
    if !directionally_blocked_edge_ids.is_empty() {
        diagnostics.push("reachable frontier is constrained by direction-locked edges".into());
    } else {
        diagnostics.push(
            "no blocked edge was found; the graph is disconnected or lacks a legal path".into(),
        );
    }
    diagnostics.push(
        "unreachable means no legal path exists even without applying hard traffic blocking".into(),
    );

    RouteFailure {
        kind: RouteFailureKind::Unreachable,
        message,
        directionally_blocked_edge_ids,
        reachable_node_ids,
        diagnostics,
        ..RouteFailure::default()
    }
}

// ---------------------------------------------------------------------------
// top-level entry
// ---------------------------------------------------------------------------

pub fn plan_route(
    index: &WorkspaceIndex,
    start: Uuid,
    goal: Uuid,
    use_penalties: bool,
) -> RoutePlanningResult {
    let mut result = RoutePlanningResult::default();
    result.search = if use_penalties {
        shortest_path_search_with_penalties(index, start, goal)
    } else {
        shortest_path_search_with_blocking(index, start, goal)
    };

    if !result.search.found {
        result.failure = Some(diagnose_route_failure(index, start, goal));
        return result;
    }

    let cost_model = if use_penalties {
        RouteCostModel::Penalized
    } else {
        RouteCostModel::GraphWeight
    };
    match build_route_plan_from_search(index, &result.search, start, goal, cost_model) {
        Ok(plan) => result.plan = Some(plan),
        Err(err) => {
            result.failure = Some(RouteFailure {
                kind: RouteFailureKind::Unreachable,
                message: err.to_string(),
                diagnostics: vec![
                    "planner found a search path but route-plan reconstruction failed".into(),
                ],
                ..RouteFailure::default()
            });
        }
    }

    result
}
