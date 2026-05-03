use datapod::OMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionBlockingType { None, Soft, Hard }

impl Default for ActionBlockingType {
    fn default() -> Self { Self::None }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstantAction {
    pub action_id: String,
    pub action_type: String,
    pub blocking_type: ActionBlockingType,
    pub description: Option<String>,
    pub parameters: OMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstantActions {
    pub header_id: String,
    pub header_version: u32,
    pub actions: Vec<InstantAction>,
}
