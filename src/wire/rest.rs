//! Axum REST adapter for the timenav core.
//!
//! Enabled with `--features rest`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use tower_http::trace::TraceLayer;

use crate::core::ids::{ClaimId, RobotId};
use crate::index::ResourceRef;
use crate::wire::{
    ApiError, AssignRouteRequest, ClaimRequest, ClaimRequestWire, HeartbeatRequest, Lease,
    PlanRouteRequest, ReleaseLeaseRequest, ScheduleRobotRouteRequest, ServeState,
};

/// REST API prefix used by `PRESENTATION.md`.
pub const REST_PREFIX: &str = "/ares/v1";

type RestResult<T> = std::result::Result<Json<T>, (StatusCode, Json<ApiError>)>;

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
        .route("/ares/v1/robots/{id}", get(robot_state).delete(unregister_robot))
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

async fn health() -> Json<crate::wire::Health> {
    Json(crate::wire::health())
}

async fn fleet_snapshot(State(state): State<ServeState>) -> RestResult<crate::wire::FleetSnapshot> {
    ok(crate::wire::fleet_snapshot(&state))
}

async fn plan_route(
    State(state): State<ServeState>,
    Json(request): Json<PlanRouteRequest>,
) -> RestResult<crate::wire::PlanRouteResponse> {
    ok(crate::wire::plan_route_request(&state, request))
}

async fn list_zones(State(state): State<ServeState>) -> RestResult<Vec<crate::wire::ZoneView>> {
    ok(crate::wire::list_zones(&state))
}

async fn zone(
    State(state): State<ServeState>,
    Path(id): Path<String>,
) -> RestResult<crate::wire::ZoneView> {
    ok(parse_resource_ref(&id).and_then(|id| crate::wire::find_zone(&state, id)))
}

async fn list_nodes(State(state): State<ServeState>) -> RestResult<Vec<crate::wire::NodeView>> {
    ok(crate::wire::list_nodes(&state))
}

async fn node(
    State(state): State<ServeState>,
    Path(id): Path<String>,
) -> RestResult<crate::wire::NodeView> {
    ok(parse_resource_ref(&id).and_then(|id| crate::wire::find_node(&state, id)))
}

async fn list_edges(State(state): State<ServeState>) -> RestResult<Vec<crate::wire::EdgeView>> {
    ok(crate::wire::list_edges(&state))
}

async fn edge(
    State(state): State<ServeState>,
    Path(id): Path<String>,
) -> RestResult<crate::wire::EdgeView> {
    ok(parse_resource_ref(&id).and_then(|id| crate::wire::find_edge(&state, id)))
}

async fn list_robots(State(state): State<ServeState>) -> RestResult<Vec<crate::robot::RobotState>> {
    ok(crate::wire::list_robots(&state))
}

async fn register_robot(
    State(state): State<ServeState>,
    Json(robot): Json<crate::robot::RobotState>,
) -> RestResult<crate::robot::RobotState> {
    ok(crate::wire::register_robot(&state, robot))
}

async fn unregister_robot(
    State(state): State<ServeState>,
    Path(id): Path<u64>,
) -> RestResult<bool> {
    ok(crate::wire::unregister_robot(&state, RobotId::new(id)))
}

async fn robot_state(
    State(state): State<ServeState>,
    Path(id): Path<u64>,
) -> RestResult<crate::robot::RobotState> {
    ok(crate::wire::robot_state(&state, RobotId::new(id)))
}

async fn heartbeat(
    State(state): State<ServeState>,
    Path(id): Path<u64>,
    Json(request): Json<HeartbeatRequest>,
) -> RestResult<crate::robot::RobotState> {
    ok(crate::wire::heartbeat(&state, RobotId::new(id), request))
}

async fn assign_route(
    State(state): State<ServeState>,
    Path(id): Path<u64>,
    Json(request): Json<AssignRouteRequest>,
) -> RestResult<crate::robot::RobotState> {
    ok(crate::wire::assign_route(&state, RobotId::new(id), request))
}

async fn schedule_robot_route(
    State(state): State<ServeState>,
    Path(id): Path<u64>,
    Json(request): Json<ScheduleRobotRouteRequest>,
) -> RestResult<crate::coordinator::ScheduleDecision> {
    ok(crate::wire::schedule_robot_route(&state, RobotId::new(id), request))
}

async fn list_claims(State(state): State<ServeState>) -> RestResult<Vec<ClaimRequest>> {
    ok(crate::wire::list_claims(&state))
}

async fn claim(
    State(state): State<ServeState>,
    Path(id): Path<u64>,
) -> RestResult<ClaimRequest> {
    ok(crate::wire::find_claim(&state, ClaimId::new(id)))
}

async fn remove_claim(
    State(state): State<ServeState>,
    Path(id): Path<u64>,
) -> RestResult<bool> {
    ok(crate::wire::remove_claim(&state, ClaimId::new(id)))
}

async fn evaluate_claim(
    State(state): State<ServeState>,
    Json(request): Json<ClaimRequestWire>,
) -> RestResult<crate::claim::ClaimEvaluation> {
    ok(crate::wire::evaluate_claim(&state, request))
}

async fn submit_claim(
    State(state): State<ServeState>,
    Json(request): Json<ClaimRequestWire>,
) -> RestResult<crate::claim::ClaimEvaluation> {
    ok(crate::wire::submit_claim(&state, request))
}

async fn list_leases(State(state): State<ServeState>) -> RestResult<Vec<Lease>> {
    ok(crate::wire::list_leases(&state))
}

async fn add_lease(
    State(state): State<ServeState>,
    Json(lease): Json<Lease>,
) -> RestResult<Lease> {
    ok(crate::wire::add_lease(&state, lease))
}

async fn release_lease(
    State(state): State<ServeState>,
    Json(request): Json<ReleaseLeaseRequest>,
) -> RestResult<bool> {
    ok(crate::wire::release_lease(&state, request))
}

async fn release_lease_by_path(
    State(state): State<ServeState>,
    Path(id): Path<u64>,
) -> RestResult<bool> {
    ok(crate::wire::release_lease(
        &state,
        ReleaseLeaseRequest { lease_id: crate::core::ids::LeaseId::new(id), released_at_tick: None },
    ))
}

fn ok<T>(result: crate::wire::ApiResult<T>) -> RestResult<T> {
    result.map(Json).map_err(|err| (StatusCode::BAD_REQUEST, Json(err)))
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
