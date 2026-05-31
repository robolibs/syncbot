//! Demonstrate request → grant → release on an exclusive zone.

use datapod::{Geo, Point, Polygon};
use timenav::{
    ClaimAccessMode, ClaimDecision, ClaimId, ClaimManager, ClaimRequest, ClaimTarget,
    ClaimTargetKind, Lease, LeaseId, RobotId, WorkspaceIndex,
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

fn main() {
    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 100.0, 100.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .unwrap();
    let zone = ZoneBuilder::new()
        .with_name("dock")
        .with_kind("zone")
        .with_boundary(rectangle(10.0, 10.0, 30.0, 30.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .with_property("traffic.policy", "exclusive")
        .build()
        .unwrap();
    root.add_child(zone).unwrap();
    let zone_id = root.children()[0].id();

    let ws = Workspace::new(root);
    let idx = std::sync::Arc::new(WorkspaceIndex::new(std::sync::Arc::new(ws)));
    let mut mgr = ClaimManager::with_index(std::sync::Arc::clone(&idx));

    let r1 = ClaimRequest {
        id: ClaimId::new(1),
        robot_id: RobotId::new(1),
        access_mode: ClaimAccessMode::Exclusive,
        targets: vec![ClaimTarget {
            kind: ClaimTargetKind::Zone,
            resource_id: zone_id,
        }],
        ..ClaimRequest::default()
    };
    println!("first request — {:?}", mgr.evaluate_request(&r1).decision);
    mgr.add_request(r1.clone());
    mgr.add_lease(Lease {
        id: LeaseId::new(11),
        claim_id: ClaimId::new(1),
        robot_id: RobotId::new(1),
        access_mode: ClaimAccessMode::Exclusive,
        targets: r1.targets.clone(),
        granted_at_tick: Some(0),
        expires_at_tick: Some(100),
        ..Lease::default()
    });

    let r2 = ClaimRequest {
        id: ClaimId::new(2),
        robot_id: RobotId::new(2),
        access_mode: ClaimAccessMode::Exclusive,
        targets: vec![ClaimTarget {
            kind: ClaimTargetKind::Zone,
            resource_id: zone_id,
        }],
        ..ClaimRequest::default()
    };
    let eval = mgr.evaluate_request(&r2);
    println!("second request — {:?} ({})", eval.decision, eval.reason);
    assert_eq!(eval.decision, ClaimDecision::Deny);

    mgr.release_lease(LeaseId::new(11), Some(50));
    let eval = mgr.evaluate_request(&r2);
    println!("after release — {:?}", eval.decision);
}
