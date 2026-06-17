//! Zenoh robotics adapter for the syncbot core.
//!
//! Enabled with `--features robo`.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::task::JoinHandle;
use zenoh::query::Query;

use crate::claim::ClaimTargetKind;
use crate::core::ids::{ClaimId, RobotId};
use crate::index::ResourceRef;
use crate::wire::{
    ApiError, AssignRouteRequest, ClaimRequestWire, Lease, PlanRouteRequest, ReleaseLeaseRequest,
    ScheduleRobotRouteRequest, ServeState,
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

/// Install syncbot queryables on an existing Zenoh session.
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
        spawn_queryable(session, "zones/list", state.clone(), |state, _| {
            crate::wire::list_zones(&state)
        })
        .await?,
    );

    tasks.push(
        spawn_queryable(session, "zones/get", state.clone(), |state, payload| {
            let request: ResourceRefEnvelope = decode_required(payload)?;
            crate::wire::find_zone(&state, request.id)
        })
        .await?,
    );

    tasks.push(
        spawn_queryable(session, "nodes/list", state.clone(), |state, _| {
            crate::wire::list_nodes(&state)
        })
        .await?,
    );

    tasks.push(
        spawn_queryable(session, "nodes/get", state.clone(), |state, payload| {
            let request: ResourceRefEnvelope = decode_required(payload)?;
            crate::wire::find_node(&state, request.id)
        })
        .await?,
    );

    tasks.push(
        spawn_queryable(session, "edges/list", state.clone(), |state, _| {
            crate::wire::list_edges(&state)
        })
        .await?,
    );

    tasks.push(
        spawn_queryable(session, "edges/get", state.clone(), |state, payload| {
            let request: ResourceRefEnvelope = decode_required(payload)?;
            crate::wire::find_edge(&state, request.id)
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
                let req: crate::wire::FlatRegister = decode_required(payload)?;
                Ok(crate::wire::flat_register(
                    &state, &req.robot, &req.key, req.alive,
                ))
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
        spawn_queryable(session, "robots/get", state.clone(), |state, payload| {
            let request: RobotIdEnvelope = decode_required(payload)?;
            crate::wire::robot_state(&state, request.robot_id)
        })
        .await?,
    );

    tasks.push(
        spawn_queryable(
            session,
            "robots/unregister",
            state.clone(),
            |state, payload| {
                let request: RobotIdEnvelope = decode_required(payload)?;
                crate::wire::unregister_robot(&state, request.robot_id)
            },
        )
        .await?,
    );

    tasks.push(
        spawn_queryable(
            session,
            "robots/heartbeat",
            state.clone(),
            |state, payload| {
                let req: FlatHeartbeatEnvelope = decode_required(payload)?;
                Ok(crate::wire::flat_heartbeat(
                    &state,
                    &req.robot,
                    &req.hb.key,
                    req.hb.zone,
                    req.hb.node,
                    req.hb.edge,
                ))
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
        spawn_queryable(session, "claims/get", state.clone(), |state, payload| {
            let request: ClaimIdEnvelope = decode_required(payload)?;
            crate::wire::find_claim(&state, request.claim_id)
        })
        .await?,
    );

    tasks.push(
        spawn_queryable(session, "claims/remove", state.clone(), |state, payload| {
            let request: ClaimIdEnvelope = decode_required(payload)?;
            crate::wire::remove_claim(&state, request.claim_id)
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
        spawn_queryable(
            session,
            "leases/release",
            state.clone(),
            |state, payload| {
                let request: ReleaseLeaseRequest = decode_required(payload)?;
                crate::wire::release_lease(&state, request)
            },
        )
        .await?,
    );

    // Flat (tier-1) services — type from the key-expression, flat JSON body,
    // decision/reason reply. Mirror the REST `/claims/{kind}` etc.
    for (path, kind) in [
        ("claims/zone", ClaimTargetKind::Zone),
        ("claims/node", ClaimTargetKind::Node),
        ("claims/edge", ClaimTargetKind::Edge),
    ] {
        tasks.push(
            spawn_queryable(session, path, state.clone(), move |state, payload| {
                let req: crate::wire::FlatClaim = decode_required(payload)?;
                Ok(crate::wire::flat_claim(
                    &state,
                    kind,
                    &req.key,
                    &req.robot,
                    &req.id,
                    req.access_mode,
                    req.lease_time,
                ))
            })
            .await?,
        );
    }
    for (path, kind) in [
        ("leases/release/zone", ClaimTargetKind::Zone),
        ("leases/release/node", ClaimTargetKind::Node),
        ("leases/release/edge", ClaimTargetKind::Edge),
    ] {
        tasks.push(
            spawn_queryable(session, path, state.clone(), move |state, payload| {
                let req: crate::wire::FlatRelease = decode_required(payload)?;
                Ok(crate::wire::flat_release(
                    &state, kind, &req.key, &req.robot, req.id,
                ))
            })
            .await?,
        );
    }

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

/// Flat heartbeat over Zenoh: robot id lives in the body (no URL here).
#[derive(serde::Deserialize)]
struct FlatHeartbeatEnvelope {
    #[serde(deserialize_with = "crate::wire::de_scalar_string")]
    robot: String,
    #[serde(flatten)]
    hb: crate::wire::FlatHeartbeat,
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

#[derive(serde::Deserialize)]
struct ResourceRefEnvelope {
    id: ResourceRef,
}

#[derive(serde::Deserialize)]
struct RobotIdEnvelope {
    robot_id: RobotId,
}

#[derive(serde::Deserialize)]
struct ClaimIdEnvelope {
    claim_id: ClaimId,
}
