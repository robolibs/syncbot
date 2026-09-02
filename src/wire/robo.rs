//! Zenoh adapter for the canonical ARES peerbus service.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::task::JoinHandle;
use zenoh::query::Query;

use crate::claim::ClaimTargetKind;
use crate::wire::{ApiError, ApiResult};

pub const ROBO_PREFIX: &str = "ares/v1";

pub fn key_expr(path: &str) -> String {
    let path = path.trim_matches('/');
    if path.is_empty() {
        ROBO_PREFIX.to_string()
    } else {
        format!("{ROBO_PREFIX}/{path}")
    }
}

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

/// Install canonical ARES queryables. This adapter has no coordinator handle.
pub async fn serve(
    session: &zenoh::Session,
    client: crate::wire::peerbus::Client,
) -> zenoh::Result<ZenohServeHandle> {
    let mut tasks = Vec::new();

    tasks.push(
        spawn_queryable(session, "health", client.clone(), |client, _| {
            client.health()
        })
        .await?,
    );
    tasks.push(
        spawn_queryable(session, "zones/get", client.clone(), |client, payload| {
            let req: ResourceGetEnvelope = decode_required(payload)?;
            client.zone(&req.id)
        })
        .await?,
    );
    tasks.push(
        spawn_queryable(session, "fleet/snapshot", client.clone(), |client, _| {
            client.fleet_snapshot()
        })
        .await?,
    );
    tasks.push(
        spawn_queryable(session, "routes/plan", client.clone(), |client, payload| {
            let req: crate::wire::FlatPlanRoute = decode_required(payload)?;
            client.plan_route(&req.start_node_id, &req.goal_node_id, req.use_penalties)
        })
        .await?,
    );
    tasks.push(
        spawn_queryable(
            session,
            "robots/register",
            client.clone(),
            |client, payload| {
                let req: crate::wire::FlatRegister = decode_required(payload)?;
                client.register(&req.robot, &req.key, req.alive)
            },
        )
        .await?,
    );
    tasks.push(
        spawn_queryable(
            session,
            "robots/heartbeat",
            client.clone(),
            |client, payload| {
                let req: FlatHeartbeatEnvelope = decode_required(payload)?;
                // The same helper every other adapter uses, so a robot reports
                // its pose identically whichever transport it speaks.
                let position = req.heartbeat.position().map_err(ApiError::new)?;
                client.heartbeat(
                    &req.robot,
                    &req.heartbeat.key,
                    req.heartbeat.zone,
                    req.heartbeat.node,
                    req.heartbeat.edge,
                    position,
                    req.heartbeat.yaw,
                )
            },
        )
        .await?,
    );

    for (path, kind) in [
        ("claims/zone", ClaimTargetKind::Zone),
        ("claims/node", ClaimTargetKind::Node),
        ("claims/edge", ClaimTargetKind::Edge),
    ] {
        tasks.push(
            spawn_queryable(session, path, client.clone(), move |client, payload| {
                let req: crate::wire::FlatClaim = decode_required(payload)?;
                client.claim(
                    kind,
                    &req.key,
                    &req.robot,
                    &req.id,
                    req.access_mode,
                    req.lease_time,
                )
            })
            .await?,
        );
    }

    tasks.push(
        spawn_queryable(
            session,
            "claims/route",
            client.clone(),
            |client, payload| {
                let req: crate::wire::FlatClaimRoute = decode_required(payload)?;
                client.claim_route(
                    &req.key,
                    &req.robot,
                    &req.node,
                    &req.edge,
                    req.access_mode,
                    req.lease_time,
                )
            },
        )
        .await?,
    );

    for (path, kind) in [
        ("leases/release/zone", ClaimTargetKind::Zone),
        ("leases/release/node", ClaimTargetKind::Node),
        ("leases/release/edge", ClaimTargetKind::Edge),
    ] {
        tasks.push(
            spawn_queryable(session, path, client.clone(), move |client, payload| {
                let req: crate::wire::FlatRelease = decode_required(payload)?;
                client.release(kind, &req.key, &req.robot, req.id)
            })
            .await?,
        );
    }

    Ok(ZenohServeHandle { tasks })
}

async fn spawn_queryable<T, F>(
    session: &zenoh::Session,
    path: &'static str,
    client: crate::wire::peerbus::Client,
    handler: F,
) -> zenoh::Result<JoinHandle<()>>
where
    T: Serialize + Send + Sync + 'static,
    F: Fn(crate::wire::peerbus::Client, Option<String>) -> ApiResult<T> + Send + Sync + 'static,
{
    let key = key_expr(path);
    let queryable = session.declare_queryable(key.clone()).await?;
    let task = tokio::spawn(async move {
        while let Ok(query) = queryable.recv_async().await {
            let payload = query
                .payload()
                .and_then(|payload| payload.try_to_string().ok())
                .map(|payload| payload.to_string());
            match handler(client.clone(), payload) {
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

fn decode_required<T: DeserializeOwned>(payload: Option<String>) -> ApiResult<T> {
    let payload = payload.ok_or_else(|| ApiError::new("missing JSON payload"))?;
    serde_json::from_str(&payload)
        .map_err(|err| ApiError::new(format!("invalid JSON payload: {err}")))
}

#[derive(serde::Deserialize)]
struct FlatHeartbeatEnvelope {
    #[serde(deserialize_with = "crate::wire::de_scalar_string")]
    robot: String,
    #[serde(flatten)]
    heartbeat: crate::wire::FlatHeartbeat,
}

#[derive(serde::Deserialize)]
struct ResourceGetEnvelope {
    #[serde(deserialize_with = "crate::wire::de_scalar_string")]
    id: String,
}
