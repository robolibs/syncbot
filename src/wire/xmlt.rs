//! XML transport adapter for the syncbot core.
//!
//! Enabled with `--features xmlt`. Identical surface and semantics to
//! [`crate::wire::rest`], but the request body is parsed as XML and the
//! response body is serialised as XML. The wire shape is whatever
//! `quick-xml` produces from the existing `serde` derives — same field
//! names, same nesting as the JSON form, just XML-encoded.

use axum::Router;
use axum::body::Bytes;
use axum::extract::{FromRequest, Path, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::core::ids::RobotId;
use crate::wire::{
    ApiError, ApiResult, AssignRouteRequest, ClaimRequestWire, Lease, PlanRouteRequest,
    ReleaseLeaseRequest, ScheduleRobotRouteRequest, ServeState,
};

/// URL prefix mirroring [`crate::wire::rest::REST_PREFIX`].
pub const XML_PREFIX: &str = "/ares/v1";

/// Axum extractor / responder for XML payloads.
///
/// Request side: reads the raw body as UTF-8 and runs `quick_xml::de`
/// against it. Response side: serialises with `quick_xml::se` and sets
/// `Content-Type: application/xml`.
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
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("read body: {e}")))?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("body is not UTF-8: {e}")))?;
        let value: T = quick_xml::de::from_str(text)
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid XML: {e}")))?;
        Ok(Xml(value))
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
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialise XML: {e}"),
            )
                .into_response(),
        }
    }
}

type XmlResult<T> = Result<Xml<T>, (StatusCode, Xml<ApiError>)>;

fn ok<T>(result: ApiResult<T>) -> XmlResult<T> {
    result
        .map(Xml)
        .map_err(|err| (StatusCode::BAD_REQUEST, Xml(err)))
}

/// Build the XML router. Same paths and verbs as [`crate::wire::rest::router`].
pub fn router(state: ServeState) -> Router {
    Router::new()
        .route("/ares/v1/health", get(health))
        .route("/ares/v1/fleet/snapshot", get(fleet_snapshot))
        .route("/ares/v1/routes/plan", post(plan_route))
        .route("/ares/v1/robots", get(list_robots).post(register_robot))
        .route(
            "/ares/v1/robots/{id}",
            get(robot_state).delete(unregister_robot),
        )
        .route("/ares/v1/robots/{id}/heartbeat", post(heartbeat))
        .route("/ares/v1/robots/{id}/route", post(assign_route))
        .route("/ares/v1/robots/{id}/schedule", post(schedule_robot_route))
        .route("/ares/v1/claims", get(list_claims).post(submit_claim))
        .route("/ares/v1/claims/evaluate", post(evaluate_claim))
        .route("/ares/v1/claims/zone", post(flat_claim_zone))
        .route("/ares/v1/claims/node", post(flat_claim_node))
        .route("/ares/v1/claims/edge", post(flat_claim_edge))
        .route("/ares/v1/leases", get(list_leases).post(add_lease))
        .route("/ares/v1/leases/release", post(release_lease))
        .route("/ares/v1/leases/release/zone", post(flat_release_zone))
        .route("/ares/v1/leases/release/node", post(flat_release_node))
        .route("/ares/v1/leases/release/edge", post(flat_release_edge))
        .route("/ares/v1/leases/{id}", delete(release_lease_by_path))
        .with_state(state)
}

async fn health() -> Xml<crate::wire::Health> {
    Xml(crate::wire::health())
}

async fn fleet_snapshot(State(state): State<ServeState>) -> XmlResult<crate::wire::FleetSnapshot> {
    ok(crate::wire::fleet_snapshot(&state))
}

async fn plan_route(
    State(state): State<ServeState>,
    Xml(request): Xml<PlanRouteRequest>,
) -> XmlResult<crate::wire::PlanRouteResponse> {
    ok(crate::wire::plan_route_request(&state, request))
}

async fn list_robots(State(state): State<ServeState>) -> XmlResult<Vec<crate::robot::RobotState>> {
    ok(crate::wire::list_robots(&state))
}

async fn register_robot(
    State(state): State<ServeState>,
    Xml(req): Xml<crate::wire::FlatRegister>,
) -> Xml<crate::wire::FlatReply> {
    Xml(crate::wire::flat_register(&state, &req.robot, &req.key))
}

fn resolve(state: &ServeState, raw: &str) -> Result<RobotId, (StatusCode, Xml<ApiError>)> {
    crate::wire::resolve_robot(state, raw).map_err(|e| (StatusCode::BAD_REQUEST, Xml(e)))
}

async fn unregister_robot(
    State(state): State<ServeState>,
    Path(id): Path<String>,
) -> XmlResult<bool> {
    let robot_id = resolve(&state, &id)?;
    ok(crate::wire::unregister_robot(&state, robot_id))
}

async fn robot_state(
    State(state): State<ServeState>,
    Path(id): Path<String>,
) -> XmlResult<crate::robot::RobotState> {
    let robot_id = resolve(&state, &id)?;
    ok(crate::wire::robot_state(&state, robot_id))
}

async fn heartbeat(
    State(state): State<ServeState>,
    Path(id): Path<String>,
    Xml(req): Xml<crate::wire::FlatHeartbeat>,
) -> Xml<crate::wire::FlatReply> {
    Xml(crate::wire::flat_heartbeat(
        &state, &id, &req.key, req.zone, req.node, req.edge,
    ))
}

async fn assign_route(
    State(state): State<ServeState>,
    Path(id): Path<String>,
    Xml(request): Xml<AssignRouteRequest>,
) -> XmlResult<crate::robot::RobotState> {
    let robot_id = resolve(&state, &id)?;
    ok(crate::wire::assign_route(&state, robot_id, request))
}

async fn schedule_robot_route(
    State(state): State<ServeState>,
    Path(id): Path<String>,
    Xml(request): Xml<ScheduleRobotRouteRequest>,
) -> XmlResult<crate::coordinator::ScheduleDecision> {
    let robot_id = resolve(&state, &id)?;
    ok(crate::wire::schedule_robot_route(&state, robot_id, request))
}

async fn list_claims(
    State(state): State<ServeState>,
) -> XmlResult<Vec<crate::claim::ClaimRequest>> {
    ok(crate::wire::list_claims(&state))
}

async fn evaluate_claim(
    State(state): State<ServeState>,
    Xml(request): Xml<ClaimRequestWire>,
) -> XmlResult<crate::claim::ClaimEvaluation> {
    ok(crate::wire::evaluate_claim(&state, request))
}

async fn submit_claim(
    State(state): State<ServeState>,
    Xml(request): Xml<ClaimRequestWire>,
) -> XmlResult<crate::claim::ClaimEvaluation> {
    ok(crate::wire::submit_claim(&state, request))
}

async fn list_leases(State(state): State<ServeState>) -> XmlResult<Vec<Lease>> {
    ok(crate::wire::list_leases(&state))
}

async fn add_lease(State(state): State<ServeState>, Xml(lease): Xml<Lease>) -> XmlResult<Lease> {
    ok(crate::wire::add_lease(&state, lease))
}

async fn release_lease(
    State(state): State<ServeState>,
    Xml(request): Xml<ReleaseLeaseRequest>,
) -> XmlResult<bool> {
    ok(crate::wire::release_lease(&state, request))
}

async fn release_lease_by_path(
    State(state): State<ServeState>,
    Path(id): Path<u64>,
) -> XmlResult<bool> {
    ok(crate::wire::release_lease(
        &state,
        ReleaseLeaseRequest {
            lease_id: crate::core::ids::LeaseId::new(id),
            released_at_tick: None,
        },
    ))
}

// --- Flat (tier-1) XML handlers --------------------------------------------

async fn flat_claim_zone(
    State(state): State<ServeState>,
    Xml(req): Xml<crate::wire::FlatClaim>,
) -> Xml<crate::wire::FlatReply> {
    Xml(crate::wire::flat_claim(
        &state,
        crate::claim::ClaimTargetKind::Zone,
        &req.key,
        &req.robot,
        &req.id,
        req.access_mode,
        req.lease_time,
    ))
}
async fn flat_claim_node(
    State(state): State<ServeState>,
    Xml(req): Xml<crate::wire::FlatClaim>,
) -> Xml<crate::wire::FlatReply> {
    Xml(crate::wire::flat_claim(
        &state,
        crate::claim::ClaimTargetKind::Node,
        &req.key,
        &req.robot,
        &req.id,
        req.access_mode,
        req.lease_time,
    ))
}
async fn flat_claim_edge(
    State(state): State<ServeState>,
    Xml(req): Xml<crate::wire::FlatClaim>,
) -> Xml<crate::wire::FlatReply> {
    Xml(crate::wire::flat_claim(
        &state,
        crate::claim::ClaimTargetKind::Edge,
        &req.key,
        &req.robot,
        &req.id,
        req.access_mode,
        req.lease_time,
    ))
}
async fn flat_release_zone(
    State(state): State<ServeState>,
    Xml(req): Xml<crate::wire::FlatRelease>,
) -> Xml<crate::wire::FlatReply> {
    Xml(crate::wire::flat_release(
        &state,
        crate::claim::ClaimTargetKind::Zone,
        &req.key,
        &req.robot,
        req.id,
    ))
}
async fn flat_release_node(
    State(state): State<ServeState>,
    Xml(req): Xml<crate::wire::FlatRelease>,
) -> Xml<crate::wire::FlatReply> {
    Xml(crate::wire::flat_release(
        &state,
        crate::claim::ClaimTargetKind::Node,
        &req.key,
        &req.robot,
        req.id,
    ))
}
async fn flat_release_edge(
    State(state): State<ServeState>,
    Xml(req): Xml<crate::wire::FlatRelease>,
) -> Xml<crate::wire::FlatReply> {
    Xml(crate::wire::flat_release(
        &state,
        crate::claim::ClaimTargetKind::Edge,
        &req.key,
        &req.robot,
        req.id,
    ))
}
