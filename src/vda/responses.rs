use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionStatus { Accepted, Rejected, Running, Finished, Failed }

impl Default for ActionStatus {
    fn default() -> Self { Self::Rejected }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Response {
    pub action_id: String,
    pub status: ActionStatus,
    pub description: Option<String>,
    pub result_code: Option<String>,
}
