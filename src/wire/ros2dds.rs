//! Experimental native-Zenoh endpoint for `zenoh-bridge-ros2dds`.
//!
//! This is intentionally tiny: one ROS2 service shape only:
//!
//! - ROS2 service name: `/timenav/list_zones`
//! - Zenoh key expression: `timenav/list_zones`
//! - ROS2 type: `std_srvs/srv/Trigger`
//!
//! The bridge forwards the service request payload to this queryable without
//! the 16-byte DDS request id. We reply with a CDR-encoded
//! `std_srvs::srv::Trigger_Response`: `bool success` + `string message`.
//! `message` contains the JSON zones list.

use tokio::task::JoinHandle;
use zenoh::query::Query;

use crate::wire::{ApiError, ServeState};

/// Default key used by `zenoh-bridge-ros2dds` route logs for
/// `/timenav/list_zones`.
pub const LIST_ZONES_KEY: &str = "timenav/list_zones";

/// Running queryable task. Dropping it aborts the background loop.
pub struct Ros2DdsListZonesHandle {
    task: JoinHandle<()>,
}

impl Drop for Ros2DdsListZonesHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Declare one queryable that behaves like a ROS2 `std_srvs/srv/Trigger`
/// service server when called through `zenoh-bridge-ros2dds`.
pub async fn serve_list_zones_trigger(
    session: &zenoh::Session,
    state: ServeState,
    key_expr: impl Into<String>,
) -> zenoh::Result<Ros2DdsListZonesHandle> {
    let key = key_expr.into();
    let queryable = session
        .declare_queryable(key.clone())
        .complete(true)
        .await?;
    let task = tokio::spawn(async move {
        while let Ok(query) = queryable.recv_async().await {
            let response = match zones_json(&state) {
                Ok(json) => encode_trigger_response(true, &json),
                Err(err) => encode_trigger_response(false, &err.message),
            };
            reply_cdr(&query, &key, response).await;
        }
    });
    Ok(Ros2DdsListZonesHandle { task })
}

fn zones_json(state: &ServeState) -> crate::wire::ApiResult<String> {
    let zones = crate::wire::list_zones(state)?;
    serde_json::to_string(&zones).map_err(|err| ApiError::new(format!("serialize zones: {err}")))
}

async fn reply_cdr(query: &Query, key: &str, payload: Vec<u8>) {
    if let Err(err) = query.reply(key, payload).await {
        eprintln!("failed to reply to ROS2DDS query on {key}: {err}");
    }
}

/// Encode `std_srvs/srv/Trigger_Response` as XCDR1 little-endian CDR.
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
pub fn encode_trigger_response(success: bool, message: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + message.len());
    out.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
    out.push(u8::from(success));
    pad_to_4(&mut out);

    let len_with_nul = message.as_bytes().len() + 1;
    out.extend_from_slice(&(len_with_nul as u32).to_le_bytes());
    out.extend_from_slice(message.as_bytes());
    out.push(0);
    pad_to_4(&mut out);
    out
}

fn pad_to_4(out: &mut Vec<u8>) {
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

#[cfg(test)]
mod tests {
    use super::encode_trigger_response;

    #[test]
    fn trigger_response_cdr_contains_header_bool_and_string() {
        let bytes = encode_trigger_response(true, "ok");
        assert_eq!(&bytes[0..4], &[0x00, 0x01, 0x00, 0x00]);
        assert_eq!(bytes[4], 1);
        assert_eq!(&bytes[8..12], &3u32.to_le_bytes());
        assert_eq!(&bytes[12..15], b"ok\0");
        assert_eq!(bytes.len() % 4, 0);
    }
}
