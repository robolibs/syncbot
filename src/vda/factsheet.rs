use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TypeSpecification {
    pub max_speed: Option<f64>,
    pub max_payload: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Factsheet {
    pub manufacturer: String,
    pub serial_number: String,
    pub protocol_version: String,
    pub agv_class: Option<String>,
    pub software_version: Option<String>,
    pub type_specification: TypeSpecification,
    pub supported_actions: Vec<String>,
}

impl Factsheet {
    pub fn new() -> Self {
        Self { protocol_version: "3.0.0".into(), ..Self::default() }
    }
}
