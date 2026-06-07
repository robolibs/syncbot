//! Axum REST adapter for the timenav core.
//!
//! Enabled with `--features rest`.
//! With `--features xmlt`, the same routes also accept/return XML when the
//! request uses `Content-Type: application/xml` or `Accept: application/xml`.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tower_http::trace::TraceLayer;

use crate::core::ids::{ClaimId, RobotId};
use crate::index::ResourceRef;
use crate::wire::{
    ApiError, AssignRouteRequest, ClaimRequest, ClaimRequestWire, HeartbeatRequest, Lease,
    PlanRouteRequest, ReleaseLeaseRequest, ScheduleRobotRouteRequest, ServeState,
};

/// REST API prefix used by `PRESENTATION.md`.
pub const REST_PREFIX: &str = "/ares/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WireFormat {
    Json,
    Xml,
}

/// Build a real router wired to a shared `Coordinator`.
pub fn router(state: ServeState) -> Router {
    Router::new()
        .route("/ares/v1/health", get(health))
        .route("/ares/v1/fleet/snapshot", get(fleet_snapshot))
        .route("/ares/v1/routes/plan", post(plan_route))
        .route("/ares/v1/zones", get(list_zones))
        .route("/ares/v1/zones/{id}", get(zone))
        .route("/ares/v1/nodes", get(list_nodes))
        .route("/ares/v1/nodes/{id}", get(node))
        .route("/ares/v1/edges", get(list_edges))
        .route("/ares/v1/edges/{id}", get(edge))
        .route("/ares/v1/robots", get(list_robots).post(register_robot))
        .route(
            "/ares/v1/robots/{id}",
            get(robot_state).delete(unregister_robot),
        )
        .route("/ares/v1/robots/{id}/heartbeat", post(heartbeat))
        .route("/ares/v1/robots/{id}/route", post(assign_route))
        .route("/ares/v1/robots/{id}/schedule", post(schedule_robot_route))
        .route("/ares/v1/claims", get(list_claims).post(submit_claim))
        .route("/ares/v1/claims/{id}", get(claim).delete(remove_claim))
        .route("/ares/v1/claims/evaluate", post(evaluate_claim))
        .route("/ares/v1/leases", get(list_leases).post(add_lease))
        .route("/ares/v1/leases/release", post(release_lease))
        .route("/ares/v1/leases/{id}", delete(release_lease_by_path))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

async fn health(headers: HeaderMap) -> Response {
    respond(preferred_format(&headers), Ok(crate::wire::health()))
}

async fn fleet_snapshot(headers: HeaderMap, State(state): State<ServeState>) -> Response {
    respond(
        preferred_format(&headers),
        crate::wire::fleet_snapshot(&state),
    )
}

async fn plan_route(headers: HeaderMap, State(state): State<ServeState>, body: Bytes) -> Response {
    let format = request_format(&headers);
    let request = match parse_body::<PlanRouteRequest>(format, &body) {
        Ok(request) => request,
        Err(err) => return respond::<()>(format, Err(err)),
    };
    respond(format, crate::wire::plan_route_request(&state, request))
}

async fn list_zones(headers: HeaderMap, State(state): State<ServeState>) -> Response {
    respond(preferred_format(&headers), crate::wire::list_zones(&state))
}

async fn zone(
    headers: HeaderMap,
    State(state): State<ServeState>,
    Path(id): Path<String>,
) -> Response {
    respond(
        preferred_format(&headers),
        parse_resource_ref(&id).and_then(|id| crate::wire::find_zone(&state, id)),
    )
}

async fn list_nodes(headers: HeaderMap, State(state): State<ServeState>) -> Response {
    respond(preferred_format(&headers), crate::wire::list_nodes(&state))
}

async fn node(
    headers: HeaderMap,
    State(state): State<ServeState>,
    Path(id): Path<String>,
) -> Response {
    respond(
        preferred_format(&headers),
        parse_resource_ref(&id).and_then(|id| crate::wire::find_node(&state, id)),
    )
}

async fn list_edges(headers: HeaderMap, State(state): State<ServeState>) -> Response {
    respond(preferred_format(&headers), crate::wire::list_edges(&state))
}

async fn edge(
    headers: HeaderMap,
    State(state): State<ServeState>,
    Path(id): Path<String>,
) -> Response {
    respond(
        preferred_format(&headers),
        parse_resource_ref(&id).and_then(|id| crate::wire::find_edge(&state, id)),
    )
}

async fn list_robots(headers: HeaderMap, State(state): State<ServeState>) -> Response {
    respond(preferred_format(&headers), crate::wire::list_robots(&state))
}

async fn register_robot(
    headers: HeaderMap,
    State(state): State<ServeState>,
    body: Bytes,
) -> Response {
    let format = request_format(&headers);
    let robot = match parse_body::<crate::robot::RobotState>(format, &body) {
        Ok(robot) => robot,
        Err(err) => return respond::<()>(format, Err(err)),
    };
    respond(format, crate::wire::register_robot(&state, robot))
}

async fn unregister_robot(
    headers: HeaderMap,
    State(state): State<ServeState>,
    Path(id): Path<u64>,
) -> Response {
    respond(
        preferred_format(&headers),
        crate::wire::unregister_robot(&state, RobotId::new(id)),
    )
}

async fn robot_state(
    headers: HeaderMap,
    State(state): State<ServeState>,
    Path(id): Path<u64>,
) -> Response {
    respond(
        preferred_format(&headers),
        crate::wire::robot_state(&state, RobotId::new(id)),
    )
}

async fn heartbeat(
    headers: HeaderMap,
    State(state): State<ServeState>,
    Path(id): Path<u64>,
    body: Bytes,
) -> Response {
    let format = request_format(&headers);
    let request = match parse_body::<HeartbeatRequest>(format, &body) {
        Ok(request) => request,
        Err(err) => return respond::<()>(format, Err(err)),
    };
    respond(
        format,
        crate::wire::heartbeat(&state, RobotId::new(id), request),
    )
}

async fn assign_route(
    headers: HeaderMap,
    State(state): State<ServeState>,
    Path(id): Path<u64>,
    body: Bytes,
) -> Response {
    let format = request_format(&headers);
    let request = match parse_body::<AssignRouteRequest>(format, &body) {
        Ok(request) => request,
        Err(err) => return respond::<()>(format, Err(err)),
    };
    respond(
        format,
        crate::wire::assign_route(&state, RobotId::new(id), request),
    )
}

async fn schedule_robot_route(
    headers: HeaderMap,
    State(state): State<ServeState>,
    Path(id): Path<u64>,
    body: Bytes,
) -> Response {
    let format = request_format(&headers);
    let request = match parse_body::<ScheduleRobotRouteRequest>(format, &body) {
        Ok(request) => request,
        Err(err) => return respond::<()>(format, Err(err)),
    };
    respond(
        format,
        crate::wire::schedule_robot_route(&state, RobotId::new(id), request),
    )
}

async fn list_claims(headers: HeaderMap, State(state): State<ServeState>) -> Response {
    respond(
        preferred_format(&headers),
        crate::wire::list_claims(&state) as crate::wire::ApiResult<Vec<ClaimRequest>>,
    )
}

async fn claim(
    headers: HeaderMap,
    State(state): State<ServeState>,
    Path(id): Path<u64>,
) -> Response {
    respond(
        preferred_format(&headers),
        crate::wire::find_claim(&state, ClaimId::new(id)),
    )
}

async fn remove_claim(
    headers: HeaderMap,
    State(state): State<ServeState>,
    Path(id): Path<u64>,
) -> Response {
    respond(
        preferred_format(&headers),
        crate::wire::remove_claim(&state, ClaimId::new(id)),
    )
}

async fn evaluate_claim(
    headers: HeaderMap,
    State(state): State<ServeState>,
    body: Bytes,
) -> Response {
    let format = request_format(&headers);
    let request = match parse_body::<ClaimRequestWire>(format, &body) {
        Ok(request) => request,
        Err(err) => return respond::<()>(format, Err(err)),
    };
    respond(format, crate::wire::evaluate_claim(&state, request))
}

async fn submit_claim(
    headers: HeaderMap,
    State(state): State<ServeState>,
    body: Bytes,
) -> Response {
    let format = request_format(&headers);
    let request = match parse_body::<ClaimRequestWire>(format, &body) {
        Ok(request) => request,
        Err(err) => return respond::<()>(format, Err(err)),
    };
    respond(format, crate::wire::submit_claim(&state, request))
}

async fn list_leases(headers: HeaderMap, State(state): State<ServeState>) -> Response {
    respond(
        preferred_format(&headers),
        crate::wire::list_leases(&state) as crate::wire::ApiResult<Vec<Lease>>,
    )
}

async fn add_lease(headers: HeaderMap, State(state): State<ServeState>, body: Bytes) -> Response {
    let format = request_format(&headers);
    let lease = match parse_body::<Lease>(format, &body) {
        Ok(lease) => lease,
        Err(err) => return respond::<()>(format, Err(err)),
    };
    respond(format, crate::wire::add_lease(&state, lease))
}

async fn release_lease(
    headers: HeaderMap,
    State(state): State<ServeState>,
    body: Bytes,
) -> Response {
    let format = request_format(&headers);
    let request = match parse_body::<ReleaseLeaseRequest>(format, &body) {
        Ok(request) => request,
        Err(err) => return respond::<()>(format, Err(err)),
    };
    respond(format, crate::wire::release_lease(&state, request))
}

async fn release_lease_by_path(
    headers: HeaderMap,
    State(state): State<ServeState>,
    Path(id): Path<u64>,
) -> Response {
    respond(
        preferred_format(&headers),
        crate::wire::release_lease(
            &state,
            ReleaseLeaseRequest {
                lease_id: crate::core::ids::LeaseId::new(id),
                released_at_tick: None,
            },
        ),
    )
}

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
    match result {
        Ok(value) => match serialize_xml("Response", &value) {
            Ok(body) => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/xml")],
                body,
            )
                .into_response(),
            Err(err) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain")],
                format!("serialize XML: {err}"),
            )
                .into_response(),
        },
        Err(err) => match serialize_xml("ApiError", &err) {
            Ok(body) => (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "application/xml")],
                body,
            )
                .into_response(),
            Err(xml_err) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain")],
                format!("serialize XML error response: {xml_err}"),
            )
                .into_response(),
        },
    }
}

#[cfg(feature = "xmlt")]
fn serialize_xml<T: Serialize>(
    fallback_root: &str,
    value: &T,
) -> Result<String, quick_xml::DeError> {
    quick_xml::se::to_string(value)
        .or_else(|_| quick_xml::se::to_string_with_root(fallback_root, &XmlItems { item: value }))
}

#[cfg(feature = "xmlt")]
#[derive(Serialize)]
struct XmlItems<'a, T: Serialize + ?Sized> {
    #[serde(rename = "item")]
    item: &'a T,
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

fn parse_resource_ref(raw: &str) -> crate::wire::ApiResult<ResourceRef> {
    if let Ok(uuid) = uuid::Uuid::parse_str(raw.trim()) {
        return Ok(ResourceRef::Uuid(uuid));
    }
    if let Ok(numeric_id) = raw.trim().parse::<u64>() {
        return Ok(ResourceRef::Numeric(numeric_id));
    }
    Err(ApiError::new(format!(
        "resource id {raw:?} is neither a UUID nor an unsigned integer"
    )))
}
