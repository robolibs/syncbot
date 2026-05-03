//! VDA adapter integration tests.

use datapod::{Geo, OMap, Point, Polygon};
use graphix::vertex::EdgeType;
use timenav::{
    RobotProgressState, RobotState, WorkspaceIndex, plan_route, vda,
};
use zoneout::{NodeData, Workspace, ZoneBuilder};

fn rectangle(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Polygon {
    Polygon { vertices: vec![
        Point::new(min_x, min_y, 0.0),
        Point::new(max_x, min_y, 0.0),
        Point::new(max_x, max_y, 0.0),
        Point::new(min_x, max_y, 0.0),
    ].into() }
}

fn three_node_workspace() -> Workspace {
    let root = ZoneBuilder::new()
        .with_name("root").with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 100.0, 100.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build().unwrap();
    let mut ws = Workspace::new(root);
    let n1 = ws.add_node_data(NodeData::new(Point::new(10.0, 10.0, 0.0)));
    let n2 = ws.add_node_data(NodeData::new(Point::new(50.0, 10.0, 0.0)));
    let n3 = ws.add_node_data(NodeData::new(Point::new(90.0, 10.0, 0.0)));
    let _ = ws.add_edge(n1, n2, 1.0, EdgeType::Undirected, OMap::new());
    let _ = ws.add_edge(n2, n3, 1.0, EdgeType::Undirected, OMap::new());
    ws
}

#[test]
fn order_from_route_plan_has_matching_node_and_edge_counts() {
    let ws = three_node_workspace();
    let n1 = ws.graph().get_vertex(ws.graph().vertices()[0]).unwrap().id;
    let n3 = ws.graph().get_vertex(ws.graph().vertices()[2]).unwrap().id;
    let idx = WorkspaceIndex::new(std::sync::Arc::new(ws));

    let result = plan_route(&idx, n1, n3, false);
    let plan = result.plan.expect("plan");

    let order = vda::map_route_plan(&plan);
    assert_eq!(order.nodes.len(), plan.traversed_node_ids.len());
    assert_eq!(order.edges.len(), plan.traversed_edge_ids.len());
    for (i, n) in order.nodes.iter().enumerate() {
        assert_eq!(n.sequence_id, i.to_string());
    }
    assert_eq!(order.version, "3.0.0");
}

#[test]
fn robot_state_to_vda_state_following_route_drives() {
    let robot = RobotState {
        progress_state: RobotProgressState::FollowingRoute,
        ..RobotState::default()
    };
    let s = vda::map_robot_state(&robot);
    assert_eq!(s.driving_state.as_deref(), Some("DRIVING"));
    assert!(!s.paused);

    let robot = RobotState {
        progress_state: RobotProgressState::Waiting,
        ..RobotState::default()
    };
    let s = vda::map_robot_state(&robot);
    assert_eq!(s.driving_state.as_deref(), Some("STOPPED"));
    assert!(s.paused);
}
