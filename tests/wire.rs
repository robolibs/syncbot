//! Serve-layer wire-type tests — UUID-or-INT resolution at the adapter
//! boundary (`ClaimRequestWire`, `PlanRouteRequest`, `HeartbeatRequest`).

#![cfg(any(feature = "rest", feature = "robo"))]

use std::collections::BTreeMap as OMap;
use std::sync::Arc;

use datapod::{Geo, Point, Polygon};
use graphix::vertex::EdgeType;
use timenav::wire::{
    ClaimRequestWire, ClaimTargetWire, HeartbeatRequest, PlanRouteRequest, ServeState,
    evaluate_claim, heartbeat, plan_route_request, register_robot,
};
use timenav::{
    ClaimAccessMode, ClaimDecision, ClaimId, ClaimTargetKind, ClaimWindow, Coordinator, MissionId,
    NUMERIC_ID_PROPERTY, ResourceRef, RobotId, RobotState, WorkspaceIndex,
};
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

fn build_state() -> (ServeState, uuid::Uuid, uuid::Uuid, uuid::Uuid) {
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
    let dock_uuid = dock.id();
    root.add_child(dock).expect("add dock");

    let mut ws = Workspace::new(root);
    let mut node_props = OMap::new();
    node_props.insert(NUMERIC_ID_PROPERTY.into(), "139".into());
    let a = ws.add_node(Point::new(15.0, 15.0, 0.0), node_props);
    let b = ws.add_node(Point::new(40.0, 40.0, 0.0), OMap::new());
    let node_a_uuid = ws.graph().get_vertex(a).unwrap().id;
    let node_b_uuid = ws.graph().get_vertex(b).unwrap().id;
    let _ = ws.add_edge(a, b, 1.0, EdgeType::Undirected, OMap::new());

    let idx = Arc::new(WorkspaceIndex::new(Arc::new(ws)));
    let coord = Coordinator::with_index(idx);
    (ServeState::new(coord), dock_uuid, node_a_uuid, node_b_uuid)
}

#[test]
fn evaluate_claim_resolves_numeric_resource_id() {
    let (state, dock_uuid, _, _) = build_state();

    let req = ClaimRequestWire {
        id: ClaimId::new(1),
        robot_id: RobotId::new(1),
        mission_id: MissionId::new(0),
        access_mode: ClaimAccessMode::Exclusive,
        priority: 0,
        requested_at_tick: None,
        window: ClaimWindow::default(),
        targets: vec![ClaimTargetWire {
            kind: ClaimTargetKind::Zone,
            resource_id: ResourceRef::Numeric(205),
        }],
    };

    let eval = evaluate_claim(&state, req).expect("evaluate");
    assert_eq!(eval.decision, ClaimDecision::Grant);
    // The evaluation echoes the resolved UUID, not the integer.
    // (No blocking target on a grant, but a deny would carry the UUID.)
    let _ = dock_uuid; // resolved id used internally
}

#[test]
fn evaluate_claim_rejects_unknown_numeric_id() {
    let (state, _, _, _) = build_state();
    let req = ClaimRequestWire {
        id: ClaimId::new(2),
        robot_id: RobotId::new(1),
        mission_id: MissionId::new(0),
        access_mode: ClaimAccessMode::Exclusive,
        priority: 0,
        requested_at_tick: None,
        window: ClaimWindow::default(),
        targets: vec![ClaimTargetWire {
            kind: ClaimTargetKind::Zone,
            resource_id: ResourceRef::Numeric(9999),
        }],
    };
    let err = evaluate_claim(&state, req).expect_err("should fail");
    assert!(err.message.contains("9999"), "got: {}", err.message);
}

#[test]
fn plan_route_request_accepts_numeric_node_ids() {
    let (state, _, node_a_uuid, _node_b_uuid) = build_state();

    let req = PlanRouteRequest {
        start_node_id: ResourceRef::Numeric(139),
        goal_node_id: ResourceRef::Uuid(node_a_uuid),
        use_penalties: false,
    };
    let resp = plan_route_request(&state, req).expect("plan_route");
    assert!(resp.found, "expected start==goal trivial route");
}

#[test]
fn heartbeat_accepts_numeric_node_id() {
    let (state, _, node_a_uuid, _) = build_state();
    register_robot(
        &state,
        RobotState {
            robot_id: RobotId::new(1),
            ..RobotState::default()
        },
    )
    .expect("register");

    let req = HeartbeatRequest {
        current_node_id: Some(ResourceRef::Numeric(139)),
        current_edge_id: None,
        updated_at_tick: 1,
    };
    let robot = heartbeat(&state, RobotId::new(1), req).expect("heartbeat");
    assert_eq!(robot.current_node_id, Some(node_a_uuid));
}

#[test]
fn claim_request_wire_deserializes_int_or_uuid() {
    let json_int = r#"{
        "id": 1,
        "robot_id": 1,
        "mission_id": 0,
        "access_mode": "Exclusive",
        "priority": 0,
        "requested_at_tick": null,
        "window": { "start_tick": null, "end_tick": null },
        "targets": [{ "kind": "Zone", "resource_id": "205" }]
    }"#;
    let wire: ClaimRequestWire = serde_json::from_str(json_int).expect("int form");
    assert!(matches!(
        wire.targets[0].resource_id,
        ResourceRef::Numeric(205)
    ));

    let json_uuid = r#"{
        "id": 1,
        "robot_id": 1,
        "mission_id": 0,
        "access_mode": "Exclusive",
        "priority": 0,
        "requested_at_tick": null,
        "window": { "start_tick": null, "end_tick": null },
        "targets": [{
          "kind": "Zone",
          "resource_id": "00000000-0000-0000-0000-000000000001"
        }]
    }"#;
    let wire: ClaimRequestWire = serde_json::from_str(json_uuid).expect("uuid form");
    assert!(matches!(wire.targets[0].resource_id, ResourceRef::Uuid(_)));
}
