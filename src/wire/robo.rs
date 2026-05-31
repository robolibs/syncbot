//! Zenoh robotics adapter for the timenav core.
//!
//! Enabled with `--features robo`.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::task::JoinHandle;
use zenoh::query::Query;

use crate::core::ids::RobotId;
use crate::wire::{
    ApiError, AssignRouteRequest, ClaimRequestWire, HeartbeatRequest, Lease, PlanRouteRequest,
    ReleaseLeaseRequest, ScheduleRobotRouteRequest, ServeState,
};

/// Zenoh key-expression prefix used by `PRESENTATION.md`.
pub const ROBO_PREFIX: &str = "ares/v1";

/// Build a normalized Zenoh key under `ares/v1`.
pub fn key_expr(path: &str) -> String {
    let path = path.trim_matches('/');
    if path.is_empty() {
        ROBO_PREFIX.to_string()
    } else {
        format!("{ROBO_PREFIX}/{path}")
    }
}

/// Running Zenoh queryable tasks. Dropping this handle aborts them.
pub struct ZenohServeHandle {
    tasks: Vec<JoinHandle<()>>,
}

impl ZenohServeHandle {
    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }
}

impl Drop for ZenohServeHandle {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Install timenav queryables on an existing Zenoh session.
pub async fn serve(session: &zenoh::Session, state: ServeState) -> zenoh::Result<ZenohServeHandle> {
    let mut tasks = Vec::new();

    tasks.push(
        spawn_queryable(session, "health", state.clone(), |_state, _| {
            Ok(crate::wire::health())
        })
        .await?,
    );

    tasks.push(
        spawn_queryable(session, "fleet/snapshot", state.clone(), |state, _| {
            crate::wire::fleet_snapshot(&state)
        })
        .await?,
    );

    tasks.push(
        spawn_queryable(session, "routes/plan", state.clone(), |state, payload| {
            let request: PlanRouteRequest = decode_required(payload)?;
            crate::wire::plan_route_request(&state, request)
        })
        .await?,
    );

    tasks.push(
        spawn_queryable(
            session,
            "robots/register",
            state.clone(),
            |state, payload| {
                let robot: crate::robot::RobotState = decode_required(payload)?;
                crate::wire::register_robot(&state, robot)
            },
        )
        .await?,
    );

    tasks.push(
        spawn_queryable(session, "robots/list", state.clone(), |state, _| {
            crate::wire::list_robots(&state)
        })
        .await?,
    );

    tasks.push(
        spawn_queryable(
            session,
            "robots/heartbeat",
            state.clone(),
            |state, payload| {
                let request: RobotHeartbeatEnvelope = decode_required(payload)?;
                crate::wire::heartbeat(&state, request.robot_id, request.heartbeat)
            },
        )
        .await?,
    );

    tasks.push(
        spawn_queryable(
            session,
            "robots/assign_route",
            state.clone(),
            |state, payload| {
                let request: RobotAssignRouteEnvelope = decode_required(payload)?;
                crate::wire::assign_route(&state, request.robot_id, request.assignment)
            },
        )
        .await?,
    );

    tasks.push(
        spawn_queryable(
            session,
            "robots/schedule",
            state.clone(),
            |state, payload| {
                let request: RobotScheduleEnvelope = decode_required(payload)?;
                crate::wire::schedule_robot_route(&state, request.robot_id, request.schedule)
            },
        )
        .await?,
    );

    tasks.push(
        spawn_queryable(session, "claims/list", state.clone(), |state, _| {
            crate::wire::list_claims(&state)
        })
        .await?,
    );

    tasks.push(
        spawn_queryable(
            session,
            "claims/evaluate",
            state.clone(),
            |state, payload| {
                let request: ClaimRequestWire = decode_required(payload)?;
                crate::wire::evaluate_claim(&state, request)
            },
        )
        .await?,
    );

    tasks.push(
        spawn_queryable(
            session,
            "claims/request",
            state.clone(),
            |state, payload| {
                let request: ClaimRequestWire = decode_required(payload)?;
                crate::wire::submit_claim(&state, request)
            },
        )
        .await?,
    );

    tasks.push(
        spawn_queryable(session, "leases/list", state.clone(), |state, _| {
            crate::wire::list_leases(&state)
        })
        .await?,
    );

    tasks.push(
        spawn_queryable(session, "leases/add", state.clone(), |state, payload| {
            let lease: Lease = decode_required(payload)?;
            crate::wire::add_lease(&state, lease)
        })
        .await?,
    );

    tasks.push(
        spawn_queryable(session, "leases/release", state, |state, payload| {
            let request: ReleaseLeaseRequest = decode_required(payload)?;
            crate::wire::release_lease(&state, request)
        })
        .await?,
    );

    Ok(ZenohServeHandle { tasks })
}

/// Publish the current fleet snapshot to `ares/v1/fleet/state`.
pub async fn publish_fleet_state(
    session: &zenoh::Session,
    state: &ServeState,
) -> zenoh::Result<()> {
    let payload =
        encode(&crate::wire::fleet_snapshot(state).map_err(|e| zenoh::Error::from(e.message))?);
    session.put(key_expr("fleet/state"), payload).await?;
    Ok(())
}

async fn spawn_queryable<T, F>(
    session: &zenoh::Session,
    path: &'static str,
    state: ServeState,
    handler: F,
) -> zenoh::Result<JoinHandle<()>>
where
    T: Serialize + Send + Sync + 'static,
    F: Fn(ServeState, Option<String>) -> crate::wire::ApiResult<T> + Send + Sync + 'static,
{
    let key = key_expr(path);
    let queryable = session.declare_queryable(key.clone()).await?;
    let task = tokio::spawn(async move {
        while let Ok(query) = queryable.recv_async().await {
            let payload = query
                .payload()
                .and_then(|payload| payload.try_to_string().ok())
                .map(|payload| payload.to_string());
            match handler(state.clone(), payload) {
                Ok(value) => reply_json(&query, &key, &value).await,
                Err(err) => reply_error(&query, &err).await,
            }
        }
    });
    Ok(task)
}

async fn reply_json<T: Serialize>(query: &Query, key: &str, value: &T) {
    match serde_json::to_string(value) {
        Ok(payload) => {
            let _ = query.reply(key, payload).await;
        }
        Err(err) => {
            reply_error(query, &ApiError::new(format!("serialize response: {err}"))).await;
        }
    }
}

async fn reply_error(query: &Query, error: &ApiError) {
    let payload = serde_json::to_string(error)
        .unwrap_or_else(|_| "{\"message\":\"internal serialization error\"}".into());
    let _ = query.reply_err(payload).await;
}

fn decode_required<T: DeserializeOwned>(payload: Option<String>) -> crate::wire::ApiResult<T> {
    let payload = payload.ok_or_else(|| ApiError::new("missing JSON payload"))?;
    serde_json::from_str(&payload)
        .map_err(|err| ApiError::new(format!("invalid JSON payload: {err}")))
}

fn encode<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|_| "{\"message\":\"internal serialization error\"}".into())
}

#[derive(serde::Deserialize)]
struct RobotHeartbeatEnvelope {
    robot_id: RobotId,
    heartbeat: HeartbeatRequest,
}

#[derive(serde::Deserialize)]
struct RobotAssignRouteEnvelope {
    robot_id: RobotId,
    assignment: AssignRouteRequest,
}

#[derive(serde::Deserialize)]
struct RobotScheduleEnvelope {
    robot_id: RobotId,
    schedule: ScheduleRobotRouteRequest,
}
