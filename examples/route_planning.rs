//! Build a tiny workspace, plan a route, and print the resulting plan.

use std::collections::BTreeMap as OMap;

use datapod::{Geo, Point, Polygon};
use graphix::vertex::EdgeType;
use timenav::{WorkspaceIndex, plan_route};
use zoneout::{NodeData, Workspace, ZoneBuilder};

fn rectangle(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Polygon {
    Polygon {
        vertices: vec![
            Point::new(min_x, min_y, 0.0),
            Point::new(max_x, min_y, 0.0),
            Point::new(max_x, max_y, 0.0),
            Point::new(min_x, max_y, 0.0),
        ]
        .into(),
    }
}

fn main() {
    let root = ZoneBuilder::new()
        .with_name("yard")
        .with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 100.0, 100.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root zone");

    let mut ws = Workspace::new(root);
    let a = ws.add_node_data(NodeData::new(Point::new(10.0, 10.0, 0.0)));
    let b = ws.add_node_data(NodeData::new(Point::new(50.0, 10.0, 0.0)));
    let c = ws.add_node_data(NodeData::new(Point::new(90.0, 10.0, 0.0)));
    let d = ws.add_node_data(NodeData::new(Point::new(50.0, 50.0, 0.0)));
    let _ = ws.add_edge(a, b, 4.0, EdgeType::Undirected, OMap::new());
    let _ = ws.add_edge(b, c, 4.0, EdgeType::Undirected, OMap::new());
    let _ = ws.add_edge(b, d, 4.0, EdgeType::Undirected, OMap::new());

    let a_id = ws.graph().get_vertex(a).unwrap().id;
    let c_id = ws.graph().get_vertex(c).unwrap().id;

    let idx = WorkspaceIndex::new(std::sync::Arc::new(ws));
    let result = plan_route(&idx, a_id, c_id, false);

    match result.plan {
        Some(plan) => {
            println!(
                "planned route from {} to {}",
                plan.start_node_id, plan.goal_node_id
            );
            println!("  total cost: {}", plan.total_cost);
            println!("  nodes:      {}", plan.traversed_node_ids.len());
            println!("  edges:      {}", plan.traversed_edge_ids.len());
            for (i, step) in plan.steps.iter().enumerate() {
                println!(
                    "  step {i}: node={} cumulative={}",
                    step.node_id, step.cumulative_cost
                );
            }
        }
        None => {
            let failure = result.failure.expect("must have failure if no plan");
            eprintln!("planning failed ({:?}): {}", failure.kind, failure.message);
            for diag in &failure.diagnostics {
                eprintln!("  - {diag}");
            }
            std::process::exit(1);
        }
    }
}
