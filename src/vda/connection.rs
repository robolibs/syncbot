use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionStatus { Online, Offline, ConnectionBroken }

impl Default for ConnectionStatus {
    fn default() -> Self { Self::Offline }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Connection {
    pub interface_name: String,
    pub manufacturer: String,
    pub serial_number: String,
    pub version: String,
    pub connection_id: Option<String>,
    pub status: ConnectionStatus,
    pub timestamp_ms: Option<u64>,
}
