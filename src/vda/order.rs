use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActionReference {
    pub action_id: String,
    pub action_type: String,
    pub blocking: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceReservation {
    pub target_id: String,
    pub target_kind: String,
    pub requires_claim: bool,
    pub access_group: Option<String>,
    pub schedule_window: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrderNode {
    pub node_id: String,
    pub sequence_id: String,
    pub released: bool,
    pub zone_id: Option<String>,
    pub node_position_hint: Option<String>,
    pub reservations: Vec<ResourceReservation>,
    pub actions: Vec<ActionReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderEdge {
    pub edge_id: String,
    pub start_node_id: String,
    pub end_node_id: String,
    pub released: bool,
    pub zone_id: Option<String>,
    pub max_speed: Option<f64>,
    pub bidirectional: bool,
    pub reservations: Vec<ResourceReservation>,
    pub actions: Vec<ActionReference>,
}

impl Default for OrderEdge {
    fn default() -> Self {
        Self {
            edge_id: String::new(),
            start_node_id: String::new(),
            end_node_id: String::new(),
            released: false,
            zone_id: None,
            max_speed: None,
            bidirectional: true,
            reservations: Vec::new(),
            actions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub header_id: String,
    pub order_id: String,
    pub order_update_id: u32,
    pub version: String,
    pub timestamp_ms: Option<u64>,
    pub nodes: Vec<OrderNode>,
    pub edges: Vec<OrderEdge>,
}

impl Default for Order {
    fn default() -> Self {
        Self {
            header_id: String::new(),
            order_id: String::new(),
            order_update_id: 0,
            version: "3.0.0".into(),
            timestamp_ms: None,
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }
}
