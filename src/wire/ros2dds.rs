//! Native-Zenoh ARES services for `zenoh-bridge-ros2dds`.
//!
//! ARES stays pure Zenoh here: no ROS2 libraries are linked. The ROS side uses
//! `ares_interfaces/srv/Json`:
//!
//! ```text
//! string request
//! ---
//! bool success
//! string response
//! ```
//!
//! Service names and Zenoh keys mirror the native ARES keys, for example:
//! `/ares/v1/claims/request` <-> `ares/v1/claims/request`.

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

/// Generic ROS2 service type used for all ARES JSON services.
pub const JSON_SERVICE_TYPE: &str = "ares_interfaces/srv/Json";

/// Running ARES ROS2DDS JSON service tasks. Dropping it aborts the background
/// query loops.
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

/// Declare the complete ARES JSON ROS2 service surface.
pub async fn serve_ares_json_services(
    session: &zenoh::Session,
    state: ServeState,
) -> zenoh::Result<Ros2DdsAresJsonHandle> {
    let mut tasks = Vec::new();

    tasks.push(
        spawn_json_service(session, "ares/v1/health", state.clone(), |_state, _| {
            to_json(crate::wire::health())
        })
        .await?,
    );
    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/fleet/snapshot",
            state.clone(),
            |state, _| to_json(crate::wire::fleet_snapshot(&state)?),
        )
        .await?,
    );
    tasks.push(
        spawn_json_service(session, "ares/v1/zones/list", state.clone(), |state, _| {
            to_json(crate::wire::list_zones(&state)?)
        })
        .await?,
    );
    tasks.push(
        spawn_json_service(session, "ares/v1/zones/get", state.clone(), |state, req| {
            let request: ResourceRefEnvelope = from_json(&req)?;
            to_json(crate::wire::find_zone(&state, request.id)?)
        })
        .await?,
    );
    tasks.push(
        spawn_json_service(session, "ares/v1/nodes/list", state.clone(), |state, _| {
            to_json(crate::wire::list_nodes(&state)?)
        })
        .await?,
    );
    tasks.push(
        spawn_json_service(session, "ares/v1/nodes/get", state.clone(), |state, req| {
            let request: ResourceRefEnvelope = from_json(&req)?;
            to_json(crate::wire::find_node(&state, request.id)?)
        })
        .await?,
    );
    tasks.push(
        spawn_json_service(session, "ares/v1/edges/list", state.clone(), |state, _| {
            to_json(crate::wire::list_edges(&state)?)
        })
        .await?,
    );
    tasks.push(
        spawn_json_service(session, "ares/v1/edges/get", state.clone(), |state, req| {
            let request: ResourceRefEnvelope = from_json(&req)?;
            to_json(crate::wire::find_edge(&state, request.id)?)
        })
        .await?,
    );
    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/routes/plan",
            state.clone(),
            |state, req| {
                let request: PlanRouteRequest = from_json(&req)?;
                to_json(crate::wire::plan_route_request(&state, request)?)
            },
        )
        .await?,
    );
    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/robots/register",
            state.clone(),
            |state, req| {
                let r: crate::wire::FlatRegister = from_json(&req)?;
                to_json(crate::wire::flat_register(
                    &state, &r.robot, &r.key, r.alive,
                ))
            },
        )
        .await?,
    );
    tasks.push(
        spawn_json_service(session, "ares/v1/robots/list", state.clone(), |state, _| {
            to_json(crate::wire::list_robots(&state)?)
        })
        .await?,
    );
    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/robots/get",
            state.clone(),
            |state, req| {
                let request: RobotIdEnvelope = from_json(&req)?;
                to_json(crate::wire::robot_state(&state, request.robot_id)?)
            },
        )
        .await?,
    );
    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/robots/unregister",
            state.clone(),
            |state, req| {
                let request: RobotIdEnvelope = from_json(&req)?;
                to_json(crate::wire::unregister_robot(
                    &state,
                    request.robot_id,
                    request.key,
                )?)
            },
        )
        .await?,
    );
    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/robots/heartbeat",
            state.clone(),
            |state, req| {
                let r: FlatHeartbeatEnvelope = from_json(&req)?;
                to_json(crate::wire::flat_heartbeat(
                    &state, &r.robot, &r.hb.key, r.hb.zone, r.hb.node, r.hb.edge,
                ))
            },
        )
        .await?,
    );
    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/robots/assign_route",
            state.clone(),
            |state, req| {
                let request: RobotAssignRouteEnvelope = from_json(&req)?;
                to_json(crate::wire::assign_route(
                    &state,
                    request.robot_id,
                    request.assignment,
                )?)
            },
        )
        .await?,
    );
    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/robots/schedule",
            state.clone(),
            |state, req| {
                let request: RobotScheduleEnvelope = from_json(&req)?;
                to_json(crate::wire::schedule_robot_route(
                    &state,
                    request.robot_id,
                    request.schedule,
                )?)
            },
        )
        .await?,
    );
    tasks.push(
        spawn_json_service(session, "ares/v1/claims/list", state.clone(), |state, _| {
            to_json(crate::wire::list_claims(&state)?)
        })
        .await?,
    );
    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/claims/get",
            state.clone(),
            |state, req| {
                let request: ClaimIdEnvelope = from_json(&req)?;
                to_json(crate::wire::find_claim(&state, request.claim_id)?)
            },
        )
        .await?,
    );
    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/claims/remove",
            state.clone(),
            |state, req| {
                let request: ClaimIdEnvelope = from_json(&req)?;
                to_json(crate::wire::remove_claim(&state, request.claim_id, request.key)?)
            },
        )
        .await?,
    );
    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/claims/evaluate",
            state.clone(),
            |state, req| {
                let request: ClaimRequestWire = from_json(&req)?;
                to_json(crate::wire::evaluate_claim(&state, request)?)
            },
        )
        .await?,
    );
    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/claims/request",
            state.clone(),
            |state, req| {
                let request: ClaimRequestWire = from_json(&req)?;
                to_json(crate::wire::submit_claim(&state, request)?)
            },
        )
        .await?,
    );
    tasks.push(
        spawn_json_service(session, "ares/v1/leases/list", state.clone(), |state, _| {
            to_json(crate::wire::list_leases(&state)?)
        })
        .await?,
    );
    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/leases/add",
            state.clone(),
            |state, req| {
                let body: AddLeaseEnvelope = from_json(&req)?;
                to_json(crate::wire::add_lease(&state, body.lease, body.key)?)
            },
        )
        .await?,
    );
    tasks.push(
        spawn_json_service(
            session,
            "ares/v1/leases/release",
            state.clone(),
            |state, req| {
                let request: ReleaseLeaseRequest = from_json(&req)?;
                to_json(crate::wire::release_lease(&state, request)?)
            },
        )
        .await?,
    );

    // Flat (tier-1) services — type from the service name, flat JSON body,
    // decision/reason reply.
    for (key, kind) in [
        ("ares/v1/claims/zone", ClaimTargetKind::Zone),
        ("ares/v1/claims/node", ClaimTargetKind::Node),
        ("ares/v1/claims/edge", ClaimTargetKind::Edge),
    ] {
        tasks.push(
            spawn_json_service(session, key, state.clone(), move |state, req| {
                let r: crate::wire::FlatClaim = from_json(&req)?;
                to_json(crate::wire::flat_claim(
                    &state,
                    kind,
                    &r.key,
                    &r.robot,
                    &r.id,
                    r.access_mode,
                    r.lease_time,
                ))
            })
            .await?,
        );
    }
    for (key, kind) in [
        ("ares/v1/leases/release/zone", ClaimTargetKind::Zone),
        ("ares/v1/leases/release/node", ClaimTargetKind::Node),
        ("ares/v1/leases/release/edge", ClaimTargetKind::Edge),
    ] {
        tasks.push(
            spawn_json_service(session, key, state.clone(), move |state, req| {
                let r: crate::wire::FlatRelease = from_json(&req)?;
                to_json(crate::wire::flat_release(
                    &state, kind, &r.key, &r.robot, r.id,
                ))
            })
            .await?,
        );
    }

    Ok(Ros2DdsAresJsonHandle { tasks })
}

async fn spawn_json_service<F>(
    session: &zenoh::Session,
    key: &'static str,
    state: ServeState,
    handler: F,
) -> zenoh::Result<JoinHandle<()>>
where
    F: Fn(ServeState, String) -> crate::wire::ApiResult<String> + Send + Sync + 'static,
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
            let response = match request.and_then(|request| handler(state.clone(), request)) {
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
///
/// Layout:
///
/// ```text
/// CDR header: 00 01 00 00
/// bool success: 1 byte
/// padding to 4-byte alignment
/// uint32 string length including trailing NUL
/// UTF-8 bytes
/// trailing NUL
/// padding to 4-byte alignment
/// ```
pub fn encode_json_response(success: bool, response: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + response.len());
    out.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
    out.push(u8::from(success));
    pad_to_4(&mut out);

    let len_with_nul = response.as_bytes().len() + 1;
    out.extend_from_slice(&(len_with_nul as u32).to_le_bytes());
    out.extend_from_slice(response.as_bytes());
    out.push(0);
    pad_to_4(&mut out);
    out
}

fn pad_to_4(out: &mut Vec<u8>) {
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

/// Flat heartbeat over ROS2DDS: robot id lives in the body (no URL here).
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
    /// Optional admin key (ignored unless `ServeState::admin_auth` is on).
    #[serde(default)]
    key: Option<String>,
}

#[derive(serde::Deserialize)]
struct ClaimIdEnvelope {
    claim_id: ClaimId,
    /// Optional admin key (ignored unless `ServeState::admin_auth` is on).
    #[serde(default)]
    key: Option<String>,
}

/// `leases/add` body: the flat `Lease` plus an optional admin key.
#[derive(serde::Deserialize)]
struct AddLeaseEnvelope {
    #[serde(flatten)]
    lease: Lease,
    /// Optional admin key (ignored unless `ServeState::admin_auth` is on).
    #[serde(default)]
    key: Option<String>,
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
