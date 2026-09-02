//! Serve-layer wire tests — route planning and the UUID-or-INT resolution
//! that happens at the adapter boundary (`ResourceRef`, `PlanRouteRequest`).

#![cfg(any(feature = "rest", feature = "robo"))]

use std::collections::BTreeMap as OMap;
use std::sync::Arc;

use datapod::{Geo, Point, Polygon};
use graphix::vertex::EdgeType;
use syncbot::wire::{PlanRouteRequest, ServeState, plan_route_request};
use syncbot::{Coordinator, NUMERIC_ID_PROPERTY, ResourceRef, WorkspaceIndex};
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

/// Root + a dock zone (numeric 205) holding two connected nodes; node `a`
/// carries numeric alias 139 and node `b` carries 140.
fn build_state() -> (ServeState, uuid::Uuid, uuid::Uuid) {
    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 100.0, 100.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root zone");
    let dock = ZoneBuilder::new()
        .with_name("dock")
        .with_kind("zone")
        .with_boundary(rectangle(10.0, 10.0, 50.0, 50.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .with_property(NUMERIC_ID_PROPERTY, "205")
        .build()
        .expect("dock zone");
    root.add_child(dock).expect("add dock");

    let mut ws = Workspace::new(root);
    let mut a_props = OMap::new();
    a_props.insert(NUMERIC_ID_PROPERTY.into(), "139".into());
    let mut b_props = OMap::new();
    b_props.insert(NUMERIC_ID_PROPERTY.into(), "140".into());
    let a = ws.add_node(Point::new(15.0, 15.0, 0.0), a_props);
    let b = ws.add_node(Point::new(40.0, 40.0, 0.0), b_props);
    let node_a_uuid = ws.graph().get_vertex(a).unwrap().id;
    let node_b_uuid = ws.graph().get_vertex(b).unwrap().id;
    let _ = ws.add_edge(a, b, 1.0, EdgeType::Undirected, OMap::new());

    let idx = Arc::new(WorkspaceIndex::new(Arc::new(ws)));
    (
        ServeState::new(Coordinator::with_index(idx)),
        node_a_uuid,
        node_b_uuid,
    )
}

#[test]
fn plan_route_accepts_numeric_node_ids() {
    let (state, node_a_uuid, node_b_uuid) = build_state();

    let response = plan_route_request(
        &state,
        PlanRouteRequest {
            start_node_id: ResourceRef::Numeric(139),
            goal_node_id: ResourceRef::Numeric(140),
            use_penalties: false,
        },
    )
    .expect("plan");

    assert!(response.found, "a → b is one edge");
    let plan = response.plan.expect("a found route carries a plan");
    assert_eq!(plan.start_node_id, node_a_uuid);
    assert_eq!(plan.goal_node_id, node_b_uuid);
    assert_eq!(plan.traversed_node_ids, vec![node_a_uuid, node_b_uuid]);
}

/// The two id forms are interchangeable per endpoint, so a client holding a
/// UUID for one node and an alias for the other still plans.
#[test]
fn plan_route_mixes_uuid_and_numeric_ids() {
    let (state, node_a_uuid, node_b_uuid) = build_state();

    let response = plan_route_request(
        &state,
        PlanRouteRequest {
            start_node_id: ResourceRef::Uuid(node_a_uuid),
            goal_node_id: ResourceRef::Numeric(140),
            use_penalties: true,
        },
    )
    .expect("plan");

    assert!(response.found);
    assert_eq!(
        response.plan.expect("plan").goal_node_id,
        node_b_uuid,
        "the numeric alias resolved to the same node the UUID names"
    );
}

#[test]
fn plan_route_rejects_an_unknown_node_id() {
    let (state, node_a_uuid, _) = build_state();

    let err = plan_route_request(
        &state,
        PlanRouteRequest {
            start_node_id: ResourceRef::Uuid(node_a_uuid),
            goal_node_id: ResourceRef::Numeric(9999),
            use_penalties: false,
        },
    )
    .expect_err("9999 is not a node");

    assert!(err.message.contains("9999"), "got: {}", err.message);
}

/// A workspace has to be bound before anything can be planned against it; the
/// error says so rather than reporting an empty graph.
#[test]
fn plan_route_without_a_workspace_reports_it() {
    let state = ServeState::new(Coordinator::new());

    let err = plan_route_request(
        &state,
        PlanRouteRequest {
            start_node_id: ResourceRef::Numeric(1),
            goal_node_id: ResourceRef::Numeric(2),
            use_penalties: false,
        },
    )
    .expect_err("no workspace bound");

    assert!(
        err.message.contains("WorkspaceIndex"),
        "got: {}",
        err.message
    );
}
