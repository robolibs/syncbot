//! Generate the fixed zoneout workspace used by `make run`.
//!
//! The output is a real `zoneout::Workspace::save(...)` directory, not a
//! mock JSON blob. By default it writes to `examples/fixed`.

use std::collections::BTreeMap as OMap;
use std::path::PathBuf;

use datapod::{Geo, Point, Polygon};
use graphix::vertex::EdgeType;
use syncbot::NUMERIC_ID_PROPERTY;
use uuid::{Uuid, uuid};
use zoneout::{CoordMode, EdgeData, NodeData, Workspace, ZoneBuilder};

const ROOT_ZONE: Uuid = uuid!("00000000-0000-0000-0000-000000000001");
const ZONE_DOCK_A: Uuid = uuid!("00000000-0000-0000-0000-000000000100");
const ZONE_DOCK_B: Uuid = uuid!("00000000-0000-0000-0000-000000000102");
const ZONE_DOCK_C: Uuid = uuid!("00000000-0000-0000-0000-000000000103");
const ZONE_4: Uuid = uuid!("00000000-0000-0000-0000-000000000104");
const ZONE_5: Uuid = uuid!("00000000-0000-0000-0000-000000000105");
const ZONE_6: Uuid = uuid!("00000000-0000-0000-0000-000000000106");
const ZONE_7: Uuid = uuid!("00000000-0000-0000-0000-000000000107");

const NODE_DOCK_A: Uuid = uuid!("00000000-0000-0000-0000-000000001001");
const NODE_JUNCTION: Uuid = uuid!("00000000-0000-0000-0000-000000001002");
const NODE_DOCK_B: Uuid = uuid!("00000000-0000-0000-0000-000000001003");

const EDGE_A_TO_JUNCTION: Uuid = uuid!("00000000-0000-0000-0000-000000002001");
const EDGE_JUNCTION_TO_B: Uuid = uuid!("00000000-0000-0000-0000-000000002002");

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

fn props(items: &[(&str, &str)]) -> OMap<String, String> {
    items
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

fn zone(
    name: &str,
    id: Uuid,
    numeric_id: u64,
    bbox: (f64, f64, f64, f64),
    policy: &str,
    extra: &[(&str, &str)],
) -> zoneout::Zone {
    let (min_x, min_y, max_x, max_y) = bbox;
    let mut builder = ZoneBuilder::new()
        .with_name(name)
        .with_kind("zone")
        .with_boundary(rectangle(min_x, min_y, max_x, max_y))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .with_resolution(0.0)
        .with_property(NUMERIC_ID_PROPERTY, numeric_id.to_string())
        .with_property("traffic.policy", policy);
    for (key, value) in extra {
        builder = builder.with_property(*key, *value);
    }
    let mut zone = builder.build().expect("zone");
    zone.set_id(id);
    zone
}

fn node(id: Uuid, name: &str, numeric_id: u64, x: f64, y: f64) -> NodeData {
    let mut data = NodeData::with_id(id, Point::new(x, y, 0.0));
    data.name = name.into();
    data.properties = props(&[
        (NUMERIC_ID_PROPERTY, &numeric_id.to_string()),
        ("label", name),
    ]);
    data
}

fn edge(id: Uuid, numeric_id: u64, name: &str, extra: &[(&str, &str)]) -> EdgeData {
    let mut properties = props(&[
        (NUMERIC_ID_PROPERTY, &numeric_id.to_string()),
        ("label", name),
    ]);
    for (key, value) in extra {
        properties.insert((*key).into(), (*value).into());
    }
    EdgeData {
        id,
        zone_ids: Vec::new(),
        properties,
    }
}

fn build_workspace() -> Workspace {
    let mut root = ZoneBuilder::new()
        .with_name("fixed_yard")
        .with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 200.0, 200.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .with_resolution(0.0)
        .with_property("traffic.policy", "shared")
        .build()
        .expect("root zone");
    root.set_id(ROOT_ZONE);

    // 3 docks (exclusive) ...
    root.add_child(zone(
        "dock_a",
        ZONE_DOCK_A,
        1,
        (10.0, 10.0, 60.0, 60.0),
        "exclusive",
        &[("traffic.claim_required", "true")],
    ))
    .expect("dock_a");
    root.add_child(zone(
        "dock_b",
        ZONE_DOCK_B,
        2,
        (125.0, 10.0, 175.0, 60.0),
        "exclusive",
        &[("traffic.claim_required", "true")],
    ))
    .expect("dock_b");
    root.add_child(zone(
        "dock_c",
        ZONE_DOCK_C,
        3,
        (65.0, 10.0, 115.0, 60.0),
        "exclusive",
        &[("traffic.claim_required", "true")],
    ))
    .expect("dock_c");
    // ... and 4 general zones.
    root.add_child(zone(
        "zone_4",
        ZONE_4,
        4,
        (10.0, 70.0, 60.0, 120.0),
        "exclusive",
        &[("traffic.claim_required", "true")],
    ))
    .expect("zone_4");
    root.add_child(zone(
        "zone_5",
        ZONE_5,
        5,
        (65.0, 70.0, 115.0, 120.0),
        "shared",
        &[("traffic.capacity", "2")],
    ))
    .expect("zone_5");
    root.add_child(zone(
        "zone_6",
        ZONE_6,
        6,
        (125.0, 70.0, 175.0, 120.0),
        "exclusive",
        &[("traffic.claim_required", "true")],
    ))
    .expect("zone_6");
    root.add_child(zone(
        "zone_7",
        ZONE_7,
        7,
        (10.0, 130.0, 60.0, 180.0),
        "shared",
        &[("traffic.capacity", "2")],
    ))
    .expect("zone_7");
    let mut ws = Workspace::new(root);
    ws.set_coord_mode(CoordMode::Local);
    ws.set_datum(Geo::new(52.0, 5.0, 0.0));

    let dock_a = ws.add_node_data(node(NODE_DOCK_A, "dock_a_entry", 1001, 30.0, 30.0));
    let junction = ws.add_node_data(node(NODE_JUNCTION, "main_junction", 1002, 90.0, 70.0));
    let dock_b = ws.add_node_data(node(NODE_DOCK_B, "dock_b_entry", 1003, 150.0, 30.0));

    ws.add_edge_data(
        dock_a,
        junction,
        7.0,
        EdgeType::Undirected,
        edge(
            EDGE_A_TO_JUNCTION,
            2001,
            "dock_a_to_junction",
            &[("traffic.lane_type", "corridor")],
        ),
    );
    ws.add_edge_data(
        junction,
        dock_b,
        7.0,
        EdgeType::Undirected,
        edge(
            EDGE_JUNCTION_TO_B,
            2002,
            "junction_to_dock_b",
            &[("traffic.lane_type", "corridor")],
        ),
    );
    ws.refresh_graph_zone_membership();
    ws
}

fn readme() -> &'static str {
    r#"# fixed syncbot zone map

Generated by:

```sh
make fixed-map
```

Loaded by default with:

```sh
make run
```

Useful numeric IDs:

- zones: fixed_yard root + 3 docks (dock_a=1, dock_b=2, dock_c=3) and
  4 zones (zone_4=4, zone_5=5, zone_6=6, zone_7=7)
- nodes: dock_a_entry=1001, main_junction=1002, dock_b_entry=1003
- edges: dock_a_to_junction=2001, junction_to_dock_b=2002

Example route request:

```sh
curl -s http://127.0.0.1:8080/ares/v1/routes/plan \
  -H 'content-type: application/json' \
  -d '{"start_node_id":"1001","goal_node_id":"1003","use_penalties":true}'
```
"#
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("examples/fixed"));

    let workspace = build_workspace();
    if output.exists() {
        std::fs::remove_dir_all(&output)?;
    }
    workspace.save(&output)?;
    std::fs::write(output.join("README.md"), readme())?;
    println!("wrote fixed zone map to {}", output.display());
    Ok(())
}
