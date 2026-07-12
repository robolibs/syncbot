#![cfg(feature = "peerbus")]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use datapod::{Geo, Point, Polygon};
use syncbot::wire::ServeState;
use syncbot::wire::peerbus::{Client, CoreService};
use syncbot::{ClaimTargetKind, Coordinator, NUMERIC_ID_PROPERTY, WorkspaceIndex};
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

fn state() -> ServeState {
    let mut root = ZoneBuilder::new()
        .with_name("root")
        .with_kind("workspace")
        .with_boundary(rectangle(0.0, 0.0, 100.0, 100.0))
        .with_datum(Geo::new(52.0, 5.0, 0.0))
        .build()
        .expect("root");
    for (name, numeric) in [("a", "42"), ("b", "43")] {
        root.add_child(
            ZoneBuilder::new()
                .with_name(name)
                .with_kind("zone")
                .with_boundary(rectangle(10.0, 10.0, 50.0, 50.0))
                .with_datum(Geo::new(52.0, 5.0, 0.0))
                .with_property(NUMERIC_ID_PROPERTY, numeric)
                .build()
                .expect("zone"),
        )
        .expect("add zone");
    }
    let mut workspace = Workspace::new(root);
    let mut properties = BTreeMap::new();
    properties.insert(NUMERIC_ID_PROPERTY.into(), "139".into());
    workspace.add_node(Point::new(15.0, 15.0, 0.0), properties);

    let index = Arc::new(WorkspaceIndex::new(Arc::new(workspace)));
    ServeState::new(Coordinator::with_index(index))
}

fn identity() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "ares-integration-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[test]
fn canonical_peerbus_flow_preserves_flat_semantics() {
    let identity = identity();
    let _core = CoreService::with_identity(state(), &identity).expect("core");
    let client = Client::connect(identity).expect("client");

    let zones = client.list_zones().expect("list zones");
    assert!(zones.iter().any(|zone| zone.numeric_id == Some(42)));
    assert!(
        client
            .fleet_snapshot()
            .expect("fleet snapshot")
            .robots
            .is_empty()
    );

    assert_eq!(client.register("7", "1234", Some(2)).unwrap().decision, 1);
    assert_eq!(client.register("8", "5678", None).unwrap().decision, 1);
    assert_eq!(
        client
            .heartbeat("7", "1234", Some(-1), Some(139), None)
            .unwrap()
            .decision,
        1
    );

    let grant = client
        .claim(
            ClaimTargetKind::Zone,
            "1234",
            "7",
            &[42, 43],
            Some(1),
            Some(30),
        )
        .unwrap();
    assert_eq!((grant.decision, grant.reason), (1, 0));

    let conflict = client
        .claim(ClaimTargetKind::Zone, "5678", "8", &[43], None, None)
        .unwrap();
    assert_eq!(
        (conflict.decision, conflict.reason, conflict.blocked),
        (0, 2, Some(43))
    );

    assert_eq!(
        client
            .release(ClaimTargetKind::Zone, "1234", "7", 43)
            .unwrap()
            .decision,
        1
    );
    assert_eq!(
        client
            .claim(ClaimTargetKind::Zone, "5678", "8", &[43], None, None)
            .unwrap()
            .decision,
        1
    );
}

#[cfg(all(feature = "rest", feature = "xmlt"))]
#[test]
fn http_json_and_xml_adapters_call_peerbus_not_coordinator() {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    let identity = identity();
    let _core = CoreService::with_identity(state(), &identity).expect("core");
    let client = Client::connect(identity).expect("client");
    let app = syncbot::wire::rest::router(client);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        let zones = app
            .clone()
            .oneshot(
                Request::get("/ares/v1/zones/42")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("zone response");
        assert_eq!(zones.status(), StatusCode::OK);
        let body = to_bytes(zones.into_body(), usize::MAX).await.unwrap();
        let zone: syncbot::wire::ZoneView = serde_json::from_slice(&body).unwrap();
        assert_eq!(zone.numeric_id, Some(42));

        let register = app
            .clone()
            .oneshot(
                Request::post("/ares/v1/robots")
                    .header(header::CONTENT_TYPE, "application/xml")
                    .body(Body::from("<reg><robot>7</robot><key>1234</key></reg>"))
                    .unwrap(),
            )
            .await
            .expect("register response");
        assert_eq!(register.status(), StatusCode::OK);
        let body = to_bytes(register.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains("<decision>1</decision>"));
        assert!(body.contains("<reason>0</reason>"));

        let heartbeat = app
            .clone()
            .oneshot(
                Request::post("/ares/v1/robots/7/heartbeat")
                    .header(header::CONTENT_TYPE, "application/xml")
                    .header(header::ACCEPT, "application/xml")
                    .body(Body::from("<m><key>1234</key><zone>-1</zone></m>"))
                    .unwrap(),
            )
            .await
            .expect("heartbeat response");
        assert_eq!(heartbeat.status(), StatusCode::OK);
        let body = to_bytes(heartbeat.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.starts_with("<reply>"), "unexpected XML reply: {body}");
        assert!(body.contains("<decision>1</decision>"));

        let claim = app
            .clone()
            .oneshot(
                Request::post("/ares/v1/claims/zone")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"robot":"7","key":"1234","id":[42,43]}"#))
                    .unwrap(),
            )
            .await
            .expect("claim response");
        assert_eq!(claim.status(), StatusCode::OK);
        let body = to_bytes(claim.into_body(), usize::MAX).await.unwrap();
        let reply: syncbot::wire::FlatReply = serde_json::from_slice(&body).unwrap();
        assert_eq!((reply.decision, reply.reason), (1, 0));
    });
    drop(runtime);
    drop(app);
}
