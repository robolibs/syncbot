//! Standalone HTTP/XML adapter for the canonical ARES peerbus service.

use axum::Router;
use axum::body::Bytes;
use axum::extract::{FromRequest, Path, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::claim::ClaimTargetKind;
use crate::wire::{ApiError, ApiResult};

pub const XML_PREFIX: &str = "/ares/v1";

pub struct Xml<T>(pub T);

impl<T, S> FromRequest<S> for Xml<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = (StatusCode, String);

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(|err| (StatusCode::BAD_REQUEST, format!("read body: {err}")))?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|err| (StatusCode::BAD_REQUEST, format!("body is not UTF-8: {err}")))?;
        quick_xml::de::from_str(text)
            .map(Xml)
            .map_err(|err| (StatusCode::BAD_REQUEST, format!("invalid XML: {err}")))
    }
}

impl<T: Serialize> IntoResponse for Xml<T> {
    fn into_response(self) -> Response {
        match quick_xml::se::to_string(&self.0) {
            Ok(body) => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/xml")],
                body,
            )
                .into_response(),
            Err(err) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialize XML: {err}"),
            )
                .into_response(),
        }
    }
}

type XmlResult<T> = Result<Xml<T>, (StatusCode, Xml<ApiError>)>;

fn result<T>(value: ApiResult<T>) -> XmlResult<T> {
    value
        .map(Xml)
        .map_err(|err| (StatusCode::BAD_REQUEST, Xml(err)))
}

pub fn router(client: crate::wire::peerbus::Client) -> Router {
    Router::new()
        .route("/ares/v1/health", get(health))
        .route("/ares/v1/fleet/snapshot", get(fleet_snapshot))
        .route("/ares/v1/zones", get(list_zones))
        .route("/ares/v1/zones/{id}", get(zone))
        .route("/ares/v1/robots", post(register))
        .route("/ares/v1/robots/register", post(register))
        .route("/ares/v1/robots/{robot}/heartbeat", post(heartbeat))
        .route("/ares/v1/claims/zone", post(claim_zone))
        .route("/ares/v1/claims/node", post(claim_node))
        .route("/ares/v1/claims/edge", post(claim_edge))
        .route("/ares/v1/leases/release/zone", post(release_zone))
        .route("/ares/v1/leases/release/node", post(release_node))
        .route("/ares/v1/leases/release/edge", post(release_edge))
        .with_state(client)
}

async fn health() -> Xml<crate::wire::Health> {
    Xml(crate::wire::health())
}

async fn fleet_snapshot(
    State(client): State<crate::wire::peerbus::Client>,
) -> XmlResult<crate::wire::FleetSnapshot> {
    result(client.fleet_snapshot())
}

async fn list_zones(
    State(client): State<crate::wire::peerbus::Client>,
) -> XmlResult<Vec<crate::wire::ZoneView>> {
    result(client.list_zones())
}

async fn zone(
    State(client): State<crate::wire::peerbus::Client>,
    Path(id): Path<String>,
) -> XmlResult<crate::wire::ZoneView> {
    result(client.zone(&id))
}

async fn register(
    State(client): State<crate::wire::peerbus::Client>,
    Xml(req): Xml<crate::wire::FlatRegister>,
) -> XmlResult<crate::wire::FlatReply> {
    result(client.register(&req.robot, &req.key, req.alive))
}

async fn heartbeat(
    State(client): State<crate::wire::peerbus::Client>,
    Path(robot): Path<String>,
    Xml(req): Xml<crate::wire::FlatHeartbeat>,
) -> XmlResult<crate::wire::FlatReply> {
    result(client.heartbeat(&robot, &req.key, req.zone, req.node, req.edge))
}

macro_rules! claim_handler {
    ($name:ident, $kind:expr) => {
        async fn $name(
            State(client): State<crate::wire::peerbus::Client>,
            Xml(req): Xml<crate::wire::FlatClaim>,
        ) -> XmlResult<crate::wire::FlatReply> {
            result(client.claim(
                $kind,
                &req.key,
                &req.robot,
                &req.id,
                req.access_mode,
                req.lease_time,
            ))
        }
    };
}

claim_handler!(claim_zone, ClaimTargetKind::Zone);
claim_handler!(claim_node, ClaimTargetKind::Node);
claim_handler!(claim_edge, ClaimTargetKind::Edge);

macro_rules! release_handler {
    ($name:ident, $kind:expr) => {
        async fn $name(
            State(client): State<crate::wire::peerbus::Client>,
            Xml(req): Xml<crate::wire::FlatRelease>,
        ) -> XmlResult<crate::wire::FlatReply> {
            result(client.release($kind, &req.key, &req.robot, req.id))
        }
    };
}

release_handler!(release_zone, ClaimTargetKind::Zone);
release_handler!(release_node, ClaimTargetKind::Node);
release_handler!(release_edge, ClaimTargetKind::Edge);
