//! ClaimManager integration tests — port of `test/claim_module_test.cpp`.

use datapod::{Geo, Point, Polygon};
use timenav::{
    ClaimAccessMode, ClaimDecision, ClaimId, ClaimManager, ClaimRequest, ClaimTarget,
    ClaimTargetKind, ClaimWindow, Lease, LeaseId, MissionId, RobotId, WorkspaceIndex,
};
use uuid::Uuid;
use zoneout::{Workspace, ZoneBuilder};

fn rectangle(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Polygon {
    Polygon { vertices: vec![
        Point::new(min_x, min_y, 0.0),
        Point::new(max_x, min_y, 0.0),
        Point::new(max_x, max_y, 0.0),
        Point::new(min_x, max_y, 0.0),
    ].into() }
}

fn make_workspace_with_zones() -> (Workspace, Uuid, Uuid) {
    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 100.0, 100.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root");

    let exclusive_zone = ZoneBuilder::new()
        .with_name("exclusive")
        .with_kind("zone")
        .with_boundary(rectangle(10.0, 10.0, 50.0, 50.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .with_property("traffic.policy", "exclusive")
        .build()
        .expect("exclusive");

    let shared_zone = ZoneBuilder::new()
        .with_name("shared")
        .with_kind("zone")
        .with_boundary(rectangle(60.0, 10.0, 90.0, 50.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .with_property("traffic.policy", "shared")
        .with_property("traffic.capacity", "2")
        .build()
        .expect("shared");

    root.add_child(exclusive_zone).unwrap();
    root.add_child(shared_zone).unwrap();
    let exclusive_id = root.children()[0].id();
    let shared_id = root.children()[1].id();

    (Workspace::new(root), exclusive_id, shared_id)
}

#[test]
fn empty_request_denied() {
    let mgr = ClaimManager::new();
    let req = ClaimRequest::default();
    let eval = mgr.evaluate_request(&req);
    assert_eq!(eval.decision, ClaimDecision::Deny);
    assert!(eval.reason.contains("does not contain any targets"));
}

#[test]
fn invalid_target_denied() {
    let (ws, _ex, _sh) = make_workspace_with_zones();
    let idx = std::sync::Arc::new(WorkspaceIndex::new(std::sync::Arc::new(ws)));
    let mgr = ClaimManager::with_index(std::sync::Arc::clone(&idx));
    let req = ClaimRequest {
        id: ClaimId::new(1),
        targets: vec![ClaimTarget {
            kind: ClaimTargetKind::Zone,
            resource_id: Uuid::new_v4(),
        }],
        ..ClaimRequest::default()
    };
    let eval = mgr.evaluate_request(&req);
    assert_eq!(eval.decision, ClaimDecision::Deny);
    assert!(eval.reason.contains("missing workspace resource"));
}

#[test]
fn exclusive_request_granted_when_clear() {
    let (ws, exclusive_id, _) = make_workspace_with_zones();
    let idx = std::sync::Arc::new(WorkspaceIndex::new(std::sync::Arc::new(ws)));
    let mgr = ClaimManager::with_index(std::sync::Arc::clone(&idx));
    let req = ClaimRequest {
        id: ClaimId::new(10),
        robot_id: RobotId::new(1),
        access_mode: ClaimAccessMode::Exclusive,
        targets: vec![ClaimTarget {
            kind: ClaimTargetKind::Zone,
            resource_id: exclusive_id,
        }],
        ..ClaimRequest::default()
    };
    let eval = mgr.evaluate_request(&req);
    assert_eq!(eval.decision, ClaimDecision::Grant);
}

#[test]
fn second_exclusive_request_conflicts() {
    let (ws, exclusive_id, _) = make_workspace_with_zones();
    let idx = std::sync::Arc::new(WorkspaceIndex::new(std::sync::Arc::new(ws)));
    let mut mgr = ClaimManager::with_index(std::sync::Arc::clone(&idx));

    let first = ClaimRequest {
        id: ClaimId::new(1),
        robot_id: RobotId::new(1),
        access_mode: ClaimAccessMode::Exclusive,
        targets: vec![ClaimTarget { kind: ClaimTargetKind::Zone, resource_id: exclusive_id }],
        ..ClaimRequest::default()
    };
    mgr.add_request(first);

    let second = ClaimRequest {
        id: ClaimId::new(2),
        robot_id: RobotId::new(2),
        access_mode: ClaimAccessMode::Exclusive,
        targets: vec![ClaimTarget { kind: ClaimTargetKind::Zone, resource_id: exclusive_id }],
        ..ClaimRequest::default()
    };
    let eval = mgr.evaluate_request(&second);
    assert_eq!(eval.decision, ClaimDecision::Deny);
    assert_eq!(eval.conflicting_claim_id, Some(ClaimId::new(1)));
}

#[test]
fn shared_request_admits_until_capacity() {
    let (ws, _, shared_id) = make_workspace_with_zones();
    let idx = std::sync::Arc::new(WorkspaceIndex::new(std::sync::Arc::new(ws)));
    let mut mgr = ClaimManager::with_index(std::sync::Arc::clone(&idx));

    let make = |id: u64, robot: u64| ClaimRequest {
        id: ClaimId::new(id),
        robot_id: RobotId::new(robot),
        access_mode: ClaimAccessMode::Shared,
        targets: vec![ClaimTarget { kind: ClaimTargetKind::Zone, resource_id: shared_id }],
        ..ClaimRequest::default()
    };

    let first = make(1, 1);
    assert_eq!(mgr.evaluate_request(&first).decision, ClaimDecision::Grant);
    mgr.add_request(first);

    let second = make(2, 2);
    assert_eq!(mgr.evaluate_request(&second).decision, ClaimDecision::Grant);
    mgr.add_request(second);

    let third = make(3, 3);
    let eval = mgr.evaluate_request(&third);
    assert_eq!(eval.decision, ClaimDecision::Deny);
    assert!(eval.reason.contains("shared zone capacity exceeded"));
}

#[test]
fn lease_lifecycle_release_expire() {
    let mut mgr = ClaimManager::new();
    let lease = Lease {
        id: LeaseId::new(7),
        claim_id: ClaimId::new(1),
        robot_id: RobotId::new(1),
        access_mode: ClaimAccessMode::Exclusive,
        targets: vec![ClaimTarget {
            kind: ClaimTargetKind::Node,
            resource_id: Uuid::new_v4(),
        }],
        granted_at_tick: Some(0),
        expires_at_tick: Some(10),
        ..Lease::default()
    };
    mgr.add_lease(lease);
    assert_eq!(mgr.lease_count(), 1);

    // Refresh extends expiry; expire fires only after current_tick >= expires.
    assert!(mgr.refresh_lease(LeaseId::new(7), 5, Some(20)));
    assert_eq!(mgr.expire_leases(15), 0);
    assert_eq!(mgr.expire_leases(25), 1);
    assert_eq!(mgr.lease_count(), 0);
    assert_eq!(mgr.released_leases().len(), 1);
}

#[test]
fn release_for_robot_archives_leases() {
    let mut mgr = ClaimManager::new();
    let make = |id: u64, robot: u64| Lease {
        id: LeaseId::new(id),
        claim_id: ClaimId::new(id),
        robot_id: RobotId::new(robot),
        targets: vec![ClaimTarget {
            kind: ClaimTargetKind::Node,
            resource_id: Uuid::new_v4(),
        }],
        ..Lease::default()
    };
    mgr.add_lease(make(1, 1));
    mgr.add_lease(make(2, 1));
    mgr.add_lease(make(3, 2));

    let released = mgr.release_leases_for_robot(RobotId::new(1), Some(99));
    assert_eq!(released, 2);
    assert_eq!(mgr.lease_count(), 1);
    assert_eq!(mgr.released_leases().len(), 2);
}

#[test]
fn upsert_replaces_existing_request() {
    let mut mgr = ClaimManager::new();
    let _ = MissionId::default();
    let req1 = ClaimRequest { id: ClaimId::new(5), priority: 1, ..ClaimRequest::default() };
    let req2 = ClaimRequest { id: ClaimId::new(5), priority: 9,
        targets: vec![ClaimTarget::default()], ..ClaimRequest::default() };
    mgr.add_request(req1);
    mgr.upsert_request(req2);
    assert_eq!(mgr.request_count(), 1);
    assert_eq!(mgr.find_request(ClaimId::new(5)).unwrap().priority, 9);

    // Window check via static helper
    assert!(mgr.find_request(ClaimId::new(5)).unwrap().window == ClaimWindow::default());
}
