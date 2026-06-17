//! XML-transport wire-type tests. Exercises the same `serde` derives the
//! `Xml<T>` extractor uses, so a round-trip here proves the extractor will
//! deserialise real PLC payloads.

#![cfg(feature = "xmlt")]

use syncbot::wire::{AssignRouteRequest, ClaimRequestWire, ClaimTargetWire, PlanRouteRequest};
use syncbot::{
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
        key: None,
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

#[test]
fn assign_route_request_accepts_minimal_route_plan_xml() {
    let xml = r#"<AssignRouteRequest>
        <route_plan>
            <start_node_id>00000000-0000-0000-0000-000000001001</start_node_id>
            <goal_node_id>00000000-0000-0000-0000-000000001003</goal_node_id>
            <total_cost>0</total_cost>
        </route_plan>
        <horizon>100</horizon>
        <updated_at_tick>10</updated_at_tick>
    </AssignRouteRequest>"#;
    let req: AssignRouteRequest = quick_xml::de::from_str(xml).expect("deserialise");
    assert_eq!(req.route_plan.steps.len(), 0);
    assert_eq!(req.route_plan.traversed_zone_ids.len(), 0);
    assert_eq!(req.horizon, 100);
}

// --- Flat (tier-1) envelope XML parsing ------------------------------------

use syncbot::wire::{FlatClaim, FlatHeartbeat, FlatRegister, FlatRelease, FlatReply};

#[test]
fn flat_register_parses_xml() {
    let req: FlatRegister =
        quick_xml::de::from_str("<m><robot>7</robot><key>1234</key></m>").expect("de");
    assert_eq!(req.robot, "7");
    assert_eq!(req.key, "1234");
    // did:pass key survives as a string
    let req: FlatRegister =
        quick_xml::de::from_str("<m><robot>7</robot><key>did:pass=secret</key></m>").expect("de");
    assert_eq!(req.key, "did:pass=secret");
}

#[test]
fn flat_heartbeat_parses_xml_zone_or_node() {
    let req: FlatHeartbeat =
        quick_xml::de::from_str("<m><key>1234</key><zone>42</zone></m>").expect("de");
    assert_eq!(req.key, "1234");
    assert_eq!(req.zone, Some(42));
    assert_eq!(req.node, None);

    let req: FlatHeartbeat =
        quick_xml::de::from_str("<m><key>1234</key><node>12</node></m>").expect("de");
    assert_eq!(req.node, Some(12));
    assert_eq!(req.zone, None);
}

#[test]
fn flat_claim_parses_repeated_ids_xml() {
    let req: FlatClaim = quick_xml::de::from_str(
        "<m><key>1234</key><robot>7</robot><id>42</id><id>43</id><id>44</id></m>",
    )
    .expect("de");
    assert_eq!(req.robot, "7");
    assert_eq!(req.id, vec![42, 43, 44]);

    // single id
    let req: FlatClaim =
        quick_xml::de::from_str("<m><key>1234</key><robot>7</robot><id>42</id></m>").expect("de");
    assert_eq!(req.id, vec![42]);
}

#[test]
fn flat_release_parses_xml() {
    let req: FlatRelease =
        quick_xml::de::from_str("<m><key>1234</key><robot>7</robot><id>42</id></m>").expect("de");
    assert_eq!(req.robot, "7");
    assert_eq!(req.id, 42);
}

#[test]
fn flat_reply_serialises_to_xml() {
    let xml = quick_xml::se::to_string(&FlatReply::ok()).expect("ser");
    // clean root, not the Rust type name `<FlatReply>`
    assert!(xml.starts_with("<reply>"), "got {xml}");
    assert!(!xml.contains("FlatReply"), "leaked Rust type name: {xml}");
    assert!(xml.contains("<decision>1</decision>"));
    assert!(xml.contains("<reason>0</reason>"));
}

#[test]
fn flat_register_key_optional_xml() {
    // no <key> -> default "0"
    let req: FlatRegister = quick_xml::de::from_str("<m><robot>7</robot></m>").expect("de");
    assert_eq!(req.robot, "7");
    assert_eq!(req.key, "0");
}

#[test]
fn flat_claim_access_mode_and_lease_xml() {
    let req: FlatClaim = quick_xml::de::from_str(
        "<m><robot>7</robot><id>42</id><access_mode>1</access_mode><lease_time>30</lease_time></m>",
    )
    .expect("de");
    assert_eq!(req.key, "0"); // omitted -> default
    assert_eq!(req.robot, "7");
    assert_eq!(req.id, vec![42]);
    assert_eq!(req.access_mode, Some(1));
    assert_eq!(req.lease_time, Some(30));
}

#[test]
fn flat_heartbeat_accepts_zone_minus_one_xml() {
    let req: FlatHeartbeat =
        quick_xml::de::from_str("<m><key>1234</key><zone>-1</zone></m>").expect("de");
    assert_eq!(req.zone, Some(-1));
}

#[test]
fn flat_register_alive_xml() {
    let req: FlatRegister =
        quick_xml::de::from_str("<m><robot>7</robot><key>1234</key><alive>5</alive></m>")
            .expect("de");
    assert_eq!(req.alive, Some(5));
    // absent -> None (server applies the 2s default)
    let req: FlatRegister = quick_xml::de::from_str("<m><robot>7</robot></m>").expect("de");
    assert_eq!(req.alive, None);
}
