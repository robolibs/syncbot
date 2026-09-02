//! ROS2/DDS adapter exposed through `zenoh-bridge-ros2dds`.
//!
//! ARES stays independent of ROS libraries. Each JSON service decodes the
//! bridge's CDR envelope, translates the flat request to a canonical datapod
//! call over peerbus, then encodes the peerbus reply as CDR again.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::task::JoinHandle;
use zenoh::query::Query;

use crate::claim::ClaimTargetKind;
use crate::wire::ApiError;

pub const JSON_SERVICE_TYPE: &str = "ares_interfaces/srv/Json";

pub struct Ros2DdsAresJsonHandle {
    tasks: Vec<JoinHandle<()>>,
}

impl Ros2DdsAresJsonHandle {
    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }
}

impl Drop for Ros2DdsAresJsonHandle {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Declare the canonical ARES JSON ROS2 service surface. The adapter owns no
/// coordinator handle; all stateful work goes through `client`.
pub async fn serve_ares_json_services(
    session: &zenoh::Session,
    client: crate::wire::peerbus::Client,
) -> zenoh::Result<Ros2DdsAresJsonHandle> {
    let mut tasks = Vec::new();

    tasks.push(
        spawn_json_service(session, "ares/v1/health", client.clone(), |client, _| {
            to_json(client.health()?)
        })
        .await?,
    );
    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/zones/get",
            client.clone(),
            |client, raw| {
                let req: ResourceGetEnvelope = from_json(&raw)?;
                to_json(client.zone(&req.id)?)
            },
        )
        .await?,
    );
    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/fleet/snapshot",
            client.clone(),
            |client, _| to_json(client.fleet_snapshot()?),
        )
        .await?,
    );
    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/routes/plan",
            client.clone(),
            |client, raw| {
                let req: crate::wire::FlatPlanRoute = from_json(&raw)?;
                to_json(client.plan_route(
                    &req.start_node_id,
                    &req.goal_node_id,
                    req.use_penalties,
                )?)
            },
        )
        .await?,
    );
    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/robots/register",
            client.clone(),
            |client, raw| {
                let req: crate::wire::FlatRegister = from_json(&raw)?;
                to_json(client.register(&req.robot, &req.key, req.alive)?)
            },
        )
        .await?,
    );
    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/robots/heartbeat",
            client.clone(),
            |client, raw| {
                let req: FlatHeartbeatEnvelope = from_json(&raw)?;
                // The same helper every other adapter uses, so a robot reports
                // its pose identically whichever transport it speaks.
                let position = req.heartbeat.position().map_err(ApiError::new)?;
                to_json(client.heartbeat(
                    &req.robot,
                    &req.heartbeat.key,
                    req.heartbeat.zone,
                    req.heartbeat.node,
                    req.heartbeat.edge,
                    position,
                    req.heartbeat.yaw,
                )?)
            },
        )
        .await?,
    );

    for (key, kind) in [
        ("ares/v1/claims/zone", ClaimTargetKind::Zone),
        ("ares/v1/claims/node", ClaimTargetKind::Node),
        ("ares/v1/claims/edge", ClaimTargetKind::Edge),
    ] {
        tasks.push(
            spawn_json_service(session, key, client.clone(), move |client, raw| {
                let req: crate::wire::FlatClaim = from_json(&raw)?;
                to_json(client.claim(
                    kind,
                    &req.key,
                    &req.robot,
                    &req.id,
                    req.access_mode,
                    req.lease_time,
                )?)
            })
            .await?,
        );
    }

    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/claims/route",
            client.clone(),
            |client, raw| {
                let req: crate::wire::FlatClaimRoute = from_json(&raw)?;
                to_json(client.claim_route(
                    &req.key,
                    &req.robot,
                    &req.node,
                    &req.edge,
                    req.access_mode,
                    req.lease_time,
                )?)
            },
        )
        .await?,
    );

    for (key, kind) in [
        ("ares/v1/leases/release/zone", ClaimTargetKind::Zone),
        ("ares/v1/leases/release/node", ClaimTargetKind::Node),
        ("ares/v1/leases/release/edge", ClaimTargetKind::Edge),
    ] {
        tasks.push(
            spawn_json_service(session, key, client.clone(), move |client, raw| {
                let req: crate::wire::FlatRelease = from_json(&raw)?;
                to_json(client.release(kind, &req.key, &req.robot, req.id)?)
            })
            .await?,
        );
    }

    Ok(Ros2DdsAresJsonHandle { tasks })
}

async fn spawn_json_service<F>(
    session: &zenoh::Session,
    key: &'static str,
    client: crate::wire::peerbus::Client,
    handler: F,
) -> zenoh::Result<JoinHandle<()>>
where
    F: Fn(crate::wire::peerbus::Client, String) -> crate::wire::ApiResult<String>
        + Send
        + Sync
        + 'static,
{
    let queryable = session
        .declare_queryable(key.to_string())
        .complete(true)
        .await?;
    let task = tokio::spawn(async move {
        while let Ok(query) = queryable.recv_async().await {
            let request = query
                .payload()
                .map(|payload| decode_json_request(&payload.to_bytes()))
                .unwrap_or_else(|| Ok(String::new()));
            let response = match request.and_then(|raw| handler(client.clone(), raw)) {
                Ok(response_json) => encode_json_response(true, &response_json),
                Err(err) => encode_json_response(false, &err.message),
            };
            reply_cdr(&query, key, response).await;
        }
    });
    Ok(task)
}

async fn reply_cdr(query: &Query, key: &str, payload: Vec<u8>) {
    if let Err(err) = query.reply(key, payload).await {
        eprintln!("failed to reply to ROS2DDS query on {key}: {err}");
    }
}

fn to_json<T: Serialize>(value: T) -> crate::wire::ApiResult<String> {
    serde_json::to_string(&value).map_err(|err| ApiError::new(format!("serialize response: {err}")))
}

fn from_json<T: DeserializeOwned>(raw: &str) -> crate::wire::ApiResult<T> {
    serde_json::from_str(raw).map_err(|err| ApiError::new(format!("invalid JSON request: {err}")))
}

fn decode_json_request(bytes: &[u8]) -> crate::wire::ApiResult<String> {
    if bytes.is_empty() {
        return Ok(String::new());
    }
    for offset in [4usize, 8, 20, 0] {
        if let Some(text) = try_decode_cdr_string_at(bytes, offset) {
            return Ok(text);
        }
    }
    Err(ApiError::new(format!(
        "invalid {JSON_SERVICE_TYPE} request payload: could not decode CDR string"
    )))
}

fn try_decode_cdr_string_at(bytes: &[u8], offset: usize) -> Option<String> {
    let len_bytes = bytes.get(offset..offset + 4)?;
    let len_with_nul =
        u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]) as usize;
    if len_with_nul == 0 {
        return Some(String::new());
    }
    let start = offset + 4;
    let end = start.checked_add(len_with_nul)?;
    let raw = bytes.get(start..end)?;
    let without_nul = raw.strip_suffix(&[0]).unwrap_or(raw);
    String::from_utf8(without_nul.to_vec()).ok()
}

/// Encode `ares_interfaces/srv/Json_Response` as XCDR1 little-endian CDR.
pub fn encode_json_response(success: bool, response: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + response.len());
    out.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
    out.push(u8::from(success));
    pad_to_4(&mut out);
    let len_with_nul = response.len() + 1;
    out.extend_from_slice(&(len_with_nul as u32).to_le_bytes());
    out.extend_from_slice(response.as_bytes());
    out.push(0);
    pad_to_4(&mut out);
    out
}

fn pad_to_4(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
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

#[cfg(test)]
mod tests {
    use super::{decode_json_request, encode_json_response};

    #[test]
    fn json_response_cdr_contains_header_bool_and_string() {
        let bytes = encode_json_response(true, "ok");
        assert_eq!(&bytes[0..4], &[0x00, 0x01, 0x00, 0x00]);
        assert_eq!(bytes[4], 1);
        assert_eq!(&bytes[8..12], &3u32.to_le_bytes());
        assert_eq!(&bytes[12..15], b"ok\0");
        assert_eq!(bytes.len() % 4, 0);
    }

    #[test]
    fn decodes_json_service_string_request() {
        let mut bytes = vec![0x00, 0x01, 0x00, 0x00];
        bytes.extend_from_slice(&12u32.to_le_bytes());
        bytes.extend_from_slice(b"{\"ok\":true}\0");
        let request = decode_json_request(&bytes).expect("decode");
        assert_eq!(request, "{\"ok\":true}");
    }
}
