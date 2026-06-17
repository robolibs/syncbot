//! WorkspaceIndex integration tests — mirrors the C++
//! `test/workspace_index_module_test.cpp` fixtures.

use datapod::{Geo, Point, Polygon};
use graphix::vertex::EdgeType;
use std::collections::BTreeMap as OMap;
use syncbot::WorkspaceIndex;
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

fn make_workspace() -> Workspace {
    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 100.0, 100.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root zone");

    let child = ZoneBuilder::new()
        .with_name("inner")
        .with_kind("zone")
        .with_boundary(rectangle(10.0, 10.0, 50.0, 50.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .with_property("traffic.policy", "exclusive")
        .build()
        .expect("inner zone");
    root.add_child(child).expect("inner add");

    let mut ws = Workspace::new(root);
    ws.set_coord_mode(zoneout::CoordMode::Local);
    ws.set_datum(Geo::new(52.0, 5.0, 0.0));
    ws
}

#[test]
fn lookup_root_zone_and_validate() {
    let ws = make_workspace();
    let idx = WorkspaceIndex::new(std::sync::Arc::new(ws));
    let root_id = idx.root_zone_id().unwrap();
    assert!(idx.zone(root_id).is_some());
    assert_eq!(idx.descendant_zones(root_id).len(), 1);

    // No data, but coord_mode is Local and datum is set — no warning expected
    // about missing reference, but the workspace tree itself is well-formed.
    let issues = idx.validation_issues();
    let errors = issues
        .iter()
        .filter(|i| i.severity == syncbot::ValidationSeverity::Error)
        .count();
    assert_eq!(errors, 0);
}

#[test]
fn node_zone_membership_lookup() {
    let mut ws = make_workspace();
    // Add a node inside the inner zone so zone membership computes.
    let _vid = ws.add_node(Point::new(20.0, 20.0, 0.0), OMap::new());
    let inner_id = ws.root_zone().children()[0].id();
    let idx = WorkspaceIndex::new(std::sync::Arc::new(ws));

    let nodes_in_inner = idx.nodes_in_zone(inner_id);
    assert_eq!(nodes_in_inner.len(), 1);

    let node_id = nodes_in_inner[0].id;
    let zones = idx.zones_of_node(node_id);
    // Node sits in both root and inner zone.
    assert!(zones.iter().any(|z| z.id() == inner_id));
}

#[test]
fn edge_between_finds_undirected_edge() {
    let mut ws = make_workspace();
    let a = ws.add_node(Point::new(15.0, 15.0, 0.0), OMap::new());
    let b = ws.add_node(Point::new(25.0, 25.0, 0.0), OMap::new());
    let _eid = ws.add_edge(a, b, 1.0, EdgeType::Undirected, OMap::new());
    let a_id = ws.graph().get_vertex(a).unwrap().id;
    let b_id = ws.graph().get_vertex(b).unwrap().id;
    let idx = WorkspaceIndex::new(std::sync::Arc::new(ws));

    assert!(idx.edge_between(a_id, b_id).is_some());
    assert!(idx.edge_between(b_id, a_id).is_some());
}

#[test]
fn numeric_id_property_resolves_to_uuid() {
    use syncbot::{NUMERIC_ID_PROPERTY, ResourceRef};

    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 100.0, 100.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root zone");
    let zone = ZoneBuilder::new()
        .with_name("dock")
        .with_kind("zone")
        .with_boundary(rectangle(10.0, 10.0, 50.0, 50.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .with_property(NUMERIC_ID_PROPERTY, "205")
        .build()
        .expect("dock zone");
    let zone_uuid = zone.id();
    root.add_child(zone).expect("add dock");

    let mut ws = Workspace::new(root);
    let mut node_props = OMap::new();
    node_props.insert(NUMERIC_ID_PROPERTY.into(), "139".into());
    let a = ws.add_node(Point::new(15.0, 15.0, 0.0), node_props);
    let b = ws.add_node(Point::new(25.0, 25.0, 0.0), OMap::new());
    let node_a_uuid = ws.graph().get_vertex(a).unwrap().id;

    let mut edge_props = OMap::new();
    edge_props.insert(NUMERIC_ID_PROPERTY.into(), "203".into());
    let edge_vid = ws.add_edge(a, b, 1.0, EdgeType::Undirected, edge_props);
    let edge_uuid = ws.graph().edge_property(edge_vid).unwrap().id;

    let idx = WorkspaceIndex::new(std::sync::Arc::new(ws));

    assert_eq!(idx.zone_uuid_by_numeric_id(205), Some(zone_uuid));
    assert_eq!(idx.node_uuid_by_numeric_id(139), Some(node_a_uuid));
    assert_eq!(idx.edge_uuid_by_numeric_id(203), Some(edge_uuid));

    assert_eq!(
        ResourceRef::Numeric(205).resolve_zone(&idx),
        Some(zone_uuid)
    );
    assert_eq!(
        ResourceRef::Uuid(zone_uuid).resolve_zone(&idx),
        Some(zone_uuid)
    );
    assert_eq!(ResourceRef::Numeric(9999).resolve_zone(&idx), None);

    // Wire form is text in both JSON and XML: quoted UUID or quoted integer.
    let from_uuid: ResourceRef =
        serde_json::from_str(&format!("\"{zone_uuid}\"")).expect("uuid form");
    let from_num: ResourceRef = serde_json::from_str("\"205\"").expect("num form");
    assert_eq!(from_uuid, ResourceRef::Uuid(zone_uuid));
    assert_eq!(from_num, ResourceRef::Numeric(205));
}

#[test]
fn coord_conversion_local_local_roundtrip() {
    let ws = make_workspace();
    let idx = WorkspaceIndex::new(std::sync::Arc::new(ws));
    let p = Point::new(10.0, 20.0, 1.0);
    let g = idx.local_to_global(p).expect("local_to_global");
    let p2 = idx.global_to_local(g).expect("global_to_local");
    let dx = (p2.x - p.x).abs();
    let dy = (p2.y - p.y).abs();
    let dz = (p2.z - p.z).abs();
    assert!(
        dx < 1e-3 && dy < 1e-3 && dz < 1e-3,
        "round-trip drift {dx}, {dy}, {dz}"
    );
}
