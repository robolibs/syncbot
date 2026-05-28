//! XML-transport wire-type tests. Exercises the same `serde` derives the
//! `Xml<T>` extractor uses, so a round-trip here proves the extractor will
//! deserialise real PLC payloads.

#![cfg(feature = "xmlt")]

use timenav::wire::{ClaimRequestWire, ClaimTargetWire, PlanRouteRequest};
use timenav::{
    ClaimAccessMode, ClaimId, ClaimTargetKind, ClaimWindow, MissionId, ResourceRef, RobotId,
};

#[test]
fn claim_request_wire_xml_roundtrip_numeric_target() {
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

    let xml = quick_xml::se::to_string(&req).expect("serialise");
    let back: ClaimRequestWire = quick_xml::de::from_str(&xml).expect("deserialise");

    assert_eq!(back.id, ClaimId::new(1));
    assert_eq!(back.robot_id, RobotId::new(1));
    assert_eq!(back.access_mode, ClaimAccessMode::Exclusive);
    assert_eq!(back.targets.len(), 1);
    assert_eq!(back.targets[0].kind, ClaimTargetKind::Zone);
    assert!(matches!(
        back.targets[0].resource_id,
        ResourceRef::Numeric(205)
    ));
}

#[test]
fn resource_ref_accepts_int_in_xml() {
    let xml = "<ResourceRef>205</ResourceRef>";
    let v: ResourceRef = quick_xml::de::from_str(xml).expect("deserialise");
    assert!(matches!(v, ResourceRef::Numeric(205)));
}

#[test]
fn resource_ref_accepts_uuid_in_xml() {
    let xml = "<ResourceRef>00000000-0000-0000-0000-000000000001</ResourceRef>";
    let v: ResourceRef = quick_xml::de::from_str(xml).expect("deserialise");
    assert!(matches!(v, ResourceRef::Uuid(_)));
}

#[test]
fn plan_route_request_xml_accepts_int_and_uuid() {
    let xml = r#"<PlanRouteRequest>
        <start_node_id>139</start_node_id>
        <goal_node_id>00000000-0000-0000-0000-000000000042</goal_node_id>
        <use_penalties>true</use_penalties>
    </PlanRouteRequest>"#;
    let req: PlanRouteRequest = quick_xml::de::from_str(xml).expect("deserialise");
    assert!(matches!(req.start_node_id, ResourceRef::Numeric(139)));
    assert!(matches!(req.goal_node_id, ResourceRef::Uuid(_)));
    assert!(req.use_penalties);
}

#[test]
fn claim_request_wire_accepts_handwritten_xml() {
    // Shape a PLC would actually send: flat fields, numeric resource id.
    let xml = r#"<ClaimRequestWire>
        <id>1</id>
        <robot_id>1</robot_id>
        <mission_id>0</mission_id>
        <access_mode>Exclusive</access_mode>
        <priority>0</priority>
        <window></window>
        <targets>
            <kind>Zone</kind>
            <resource_id>205</resource_id>
        </targets>
    </ClaimRequestWire>"#;
    let req: ClaimRequestWire = quick_xml::de::from_str(xml).expect("deserialise");
    assert_eq!(req.id, ClaimId::new(1));
    assert_eq!(req.targets.len(), 1);
    assert!(matches!(
        req.targets[0].resource_id,
        ResourceRef::Numeric(205)
    ));
}

#[test]
fn claim_request_wire_accepts_multiple_targets() {
    let xml = r#"<ClaimRequestWire>
        <id>1</id>
        <robot_id>1</robot_id>
        <mission_id>0</mission_id>
        <access_mode>Exclusive</access_mode>
        <priority>0</priority>
        <window></window>
        <targets><kind>Zone</kind><resource_id>205</resource_id></targets>
        <targets><kind>Node</kind><resource_id>139</resource_id></targets>
    </ClaimRequestWire>"#;
    let req: ClaimRequestWire = quick_xml::de::from_str(xml).expect("deserialise");
    assert_eq!(req.targets.len(), 2);
    assert_eq!(req.targets[0].kind, ClaimTargetKind::Zone);
    assert_eq!(req.targets[1].kind, ClaimTargetKind::Node);
}
