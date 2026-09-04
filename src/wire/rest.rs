//! HTTP adapter for the canonical ARES peerbus service.
//!
//! JSON is the default wire format. With `xmlt`, the same endpoints negotiate
//! XML through `Content-Type`/`Accept`. Handlers own only a peerbus client.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tower_http::trace::TraceLayer;

use crate::claim::ClaimTargetKind;
use crate::wire::{
    ApiError, FlatChallenge, FlatClaim, FlatClaimRoute, FlatHeartbeat, FlatPlanRoute, FlatProve,
    FlatRegister, FlatRelease,
};

pub const REST_PREFIX: &str = "/ares/v1";

/// Header carrying the operator key on a workspace replacement.
pub const ADMIN_KEY_HEADER: &str = "x-ares-admin-key";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WireFormat {
    Json,
    Xml,
}

/// Body ceiling for a pushed workspace. axum defaults to 2 MB, which would
/// reject exactly the large workspaces the chunked peerbus transfer exists to
/// carry. This bounds the HTTP hop only; the core enforces its own ceiling.
const MAX_WORKSPACE_BODY_BYTES: usize = 256 * 1024 * 1024;

pub fn router(client: crate::wire::peerbus::Client) -> Router {
    Router::new()
        .route("/ares/v1/health", get(health))
        .route("/ares/v1/fleet/snapshot", get(fleet_snapshot))
        .route(
            "/ares/v1/workspace",
            post(set_workspace).layer(axum::extract::DefaultBodyLimit::max(
                MAX_WORKSPACE_BODY_BYTES,
            )),
        )
        .route("/ares/v1/auth/challenge", post(auth_challenge))
        .route("/ares/v1/auth/prove", post(auth_prove))
        .route("/ares/v1/routes/plan", post(plan_route))
        .route("/ares/v1/zones", get(list_zones))
        .route("/ares/v1/zones/{id}", get(zone))
        .route("/ares/v1/robots", post(register))
        .route("/ares/v1/robots/register", post(register))
        .route("/ares/v1/robots/{robot}/heartbeat", post(heartbeat))
        .route("/ares/v1/claims/zone", post(claim_zone))
        .route("/ares/v1/claims/node", post(claim_node))
        .route("/ares/v1/claims/edge", post(claim_edge))
        .route("/ares/v1/claims/route", post(claim_route))
        .route("/ares/v1/leases/release/zone", post(release_zone))
        .route("/ares/v1/leases/release/node", post(release_node))
        .route("/ares/v1/leases/release/edge", post(release_edge))
        .with_state(client)
        .layer(TraceLayer::new_for_http())
}

async fn health(
    headers: HeaderMap,
    State(client): State<crate::wire::peerbus::Client>,
) -> Response {
    respond(preferred_format(&headers), client.health())
}

async fn fleet_snapshot(
    headers: HeaderMap,
    State(client): State<crate::wire::peerbus::Client>,
) -> Response {
    respond(preferred_format(&headers), client.fleet_snapshot())
}

/// Replace the served workspace. The body is a flat `zoneout::WorkspaceJson`,
/// passed through verbatim — the core owns parsing and validation, so this
/// adapter stays a transport and the same bytes mean the same thing on every
/// transport.
///
/// The operator key rides in `x-ares-admin-key` rather than the body, because
/// the body is the caller's document and must not need rewriting to carry
/// ours. It is an *operator* key: a robot key authorises claiming one zone,
/// never redrawing the map every robot is claiming against.
async fn set_workspace(
    headers: HeaderMap,
    State(client): State<crate::wire::peerbus::Client>,
    body: Bytes,
) -> Response {
    let key = headers
        .get(ADMIN_KEY_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    respond(
        preferred_format(&headers),
        client.set_workspace(&body, &key),
    )
}

/// Ask for a nonce to sign. Unauthenticated on purpose — the answer is
/// useless without the private key, and a robot needs this before it can
/// authenticate at all.
async fn auth_challenge(
    headers: HeaderMap,
    State(client): State<crate::wire::peerbus::Client>,
    body: Bytes,
) -> Response {
    let format = request_format(&headers);
    let req = match parse_body::<FlatChallenge>(format, &body) {
        Ok(req) => req,
        Err(err) => return respond::<()>(format, Err(err)),
    };
    respond(preferred_format(&headers), client.challenge(&req.robot))
}

async fn auth_prove(
    headers: HeaderMap,
    State(client): State<crate::wire::peerbus::Client>,
    body: Bytes,
) -> Response {
    let format = request_format(&headers);
    let req = match parse_body::<FlatProve>(format, &body) {
        Ok(req) => req,
        Err(err) => return respond::<()>(format, Err(err)),
    };
    let Some(signature) = crate::wire::decode_hex_public(&req.signature) else {
        return respond::<()>(format, Err(ApiError::new("signature must be hex encoded")));
    };
    respond(
        preferred_format(&headers),
        client.prove(&req.robot, &req.did, &signature),
    )
}

async fn plan_route(
    headers: HeaderMap,
    State(client): State<crate::wire::peerbus::Client>,
    body: Bytes,
) -> Response {
    let format = request_format(&headers);
    let req = match parse_body::<FlatPlanRoute>(format, &body) {
        Ok(req) => req,
        Err(err) => return respond::<()>(format, Err(err)),
    };
    respond(
        preferred_format(&headers),
        client.plan_route(&req.start_node_id, &req.goal_node_id, req.use_penalties),
    )
}

async fn list_zones(
    headers: HeaderMap,
    State(client): State<crate::wire::peerbus::Client>,
) -> Response {
    respond(preferred_format(&headers), client.list_zones())
}

async fn zone(
    headers: HeaderMap,
    State(client): State<crate::wire::peerbus::Client>,
    Path(id): Path<String>,
) -> Response {
    respond(preferred_format(&headers), client.zone(&id))
}

async fn register(
    headers: HeaderMap,
    State(client): State<crate::wire::peerbus::Client>,
    body: Bytes,
) -> Response {
    let format = request_format(&headers);
    let req = match parse_body::<FlatRegister>(format, &body) {
        Ok(req) => req,
        Err(err) => return respond::<()>(format, Err(err)),
    };
    respond(format, client.register(&req.robot, &req.key, req.alive))
}

async fn heartbeat(
    headers: HeaderMap,
    State(client): State<crate::wire::peerbus::Client>,
    Path(robot): Path<String>,
    body: Bytes,
) -> Response {
    let format = request_format(&headers);
    let req = match parse_body::<FlatHeartbeat>(format, &body) {
        Ok(req) => req,
        Err(err) => return respond::<()>(format, Err(err)),
    };
    // A contradictory body (both frames, or half of one) is answered with the
    // reason rather than a bare reason code, since the fix is in the caller's
    // JSON and the code alone wouldn't say which half is wrong.
    let position = match req.position() {
        Ok(position) => position,
        Err(message) => return respond::<()>(format, Err(ApiError::new(message))),
    };
    respond(
        format,
        client.heartbeat(
            &robot, &req.key, req.zone, req.node, req.edge, position, req.yaw,
        ),
    )
}

macro_rules! claim_handler {
    ($name:ident, $kind:expr) => {
        async fn $name(
            headers: HeaderMap,
            State(client): State<crate::wire::peerbus::Client>,
            body: Bytes,
        ) -> Response {
            let format = request_format(&headers);
            let req = match parse_body::<FlatClaim>(format, &body) {
                Ok(req) => req,
                Err(err) => return respond::<()>(format, Err(err)),
            };
            respond(
                format,
                client.claim(
                    $kind,
                    &req.key,
                    &req.robot,
                    &req.id,
                    req.access_mode,
                    req.lease_time,
                ),
            )
        }
    };
}

claim_handler!(claim_zone, ClaimTargetKind::Zone);
claim_handler!(claim_node, ClaimTargetKind::Node);
claim_handler!(claim_edge, ClaimTargetKind::Edge);

async fn claim_route(
    headers: HeaderMap,
    State(client): State<crate::wire::peerbus::Client>,
    body: Bytes,
) -> Response {
    let format = request_format(&headers);
    let req = match parse_body::<FlatClaimRoute>(format, &body) {
        Ok(req) => req,
        Err(err) => return respond::<()>(format, Err(err)),
    };
    respond(
        format,
        client.claim_route(
            &req.key,
            &req.robot,
            &req.node,
            &req.edge,
            req.access_mode,
            req.lease_time,
        ),
    )
}

macro_rules! release_handler {
    ($name:ident, $kind:expr) => {
        async fn $name(
            headers: HeaderMap,
            State(client): State<crate::wire::peerbus::Client>,
            body: Bytes,
        ) -> Response {
            let format = request_format(&headers);
            let req = match parse_body::<FlatRelease>(format, &body) {
                Ok(req) => req,
                Err(err) => return respond::<()>(format, Err(err)),
            };
            respond(format, client.release($kind, &req.key, &req.robot, req.id))
        }
    };
}

release_handler!(release_zone, ClaimTargetKind::Zone);
release_handler!(release_node, ClaimTargetKind::Node);
release_handler!(release_edge, ClaimTargetKind::Edge);

fn preferred_format(headers: &HeaderMap) -> WireFormat {
    if header_contains(headers, header::ACCEPT, "application/xml")
        || header_contains(headers, header::ACCEPT, "text/xml")
    {
        WireFormat::Xml
    } else {
        WireFormat::Json
    }
}

fn request_format(headers: &HeaderMap) -> WireFormat {
    if header_contains(headers, header::CONTENT_TYPE, "application/xml")
        || header_contains(headers, header::CONTENT_TYPE, "text/xml")
    {
        WireFormat::Xml
    } else {
        WireFormat::Json
    }
}

fn header_contains(headers: &HeaderMap, name: header::HeaderName, needle: &str) -> bool {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase().contains(needle))
        .unwrap_or(false)
}

fn parse_body<T: DeserializeOwned>(format: WireFormat, body: &[u8]) -> crate::wire::ApiResult<T> {
    match format {
        WireFormat::Json => serde_json::from_slice(body)
            .map_err(|err| ApiError::new(format!("invalid JSON body: {err}"))),
        WireFormat::Xml => parse_xml_body(body),
    }
}

#[cfg(feature = "xmlt")]
fn parse_xml_body<T: DeserializeOwned>(body: &[u8]) -> crate::wire::ApiResult<T> {
    let text = std::str::from_utf8(body)
        .map_err(|err| ApiError::new(format!("XML body is not UTF-8: {err}")))?;
    quick_xml::de::from_str(text).map_err(|err| ApiError::new(format!("invalid XML body: {err}")))
}

#[cfg(not(feature = "xmlt"))]
fn parse_xml_body<T: DeserializeOwned>(_body: &[u8]) -> crate::wire::ApiResult<T> {
    Err(ApiError::new(
        "XML support is not compiled; enable feature xmlt",
    ))
}

fn respond<T: Serialize>(format: WireFormat, result: crate::wire::ApiResult<T>) -> Response {
    match format {
        WireFormat::Json => match result {
            Ok(value) => Json(value).into_response(),
            Err(err) => (StatusCode::BAD_REQUEST, Json(err)).into_response(),
        },
        WireFormat::Xml => respond_xml(result),
    }
}

#[cfg(feature = "xmlt")]
fn respond_xml<T: Serialize>(result: crate::wire::ApiResult<T>) -> Response {
    let (status, body) = match result {
        Ok(value) => (StatusCode::OK, quick_xml::se::to_string(&value)),
        Err(err) => (StatusCode::BAD_REQUEST, quick_xml::se::to_string(&err)),
    };
    match body {
        Ok(body) => (status, [(header::CONTENT_TYPE, "application/xml")], body).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("serialize XML: {err}"),
        )
            .into_response(),
    }
}

#[cfg(not(feature = "xmlt"))]
fn respond_xml<T: Serialize>(_result: crate::wire::ApiResult<T>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiError::new(
            "XML support is not compiled; enable feature xmlt",
        )),
    )
        .into_response()
}
