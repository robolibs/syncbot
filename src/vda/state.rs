use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperatingMode {
    Manual,
    Automatic,
    Semiautomatic,
}

impl Default for OperatingMode {
    fn default() -> Self {
        Self::Manual
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionState {
    Online,
    Offline,
    ConnectionBroken,
}

impl Default for ConnectionState {
    fn default() -> Self {
        Self::Offline
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BatteryState {
    pub battery_charge: Option<f64>,
    pub charging: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReservationState {
    pub target_id: String,
    pub target_kind: String,
    pub state: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub agv_id: String,
    pub operating_mode: OperatingMode,
    pub connection_state: ConnectionState,
    pub order_update_id: u32,
    pub last_node_id: Option<String>,
    pub last_edge_id: Option<String>,
    pub order_id: Option<String>,
    pub driving_state: Option<String>,
    pub paused: bool,
    pub battery_state: BatteryState,
    pub reservation_states: Vec<ReservationState>,
    pub action_states: Vec<String>,
    pub errors: Vec<String>,
    pub information: Vec<String>,
}
