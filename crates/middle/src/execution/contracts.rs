use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

pub use mineintent_contracts::capability::ExecutionResource;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettledJobState {
    Completed,
    Failed,
    Cancelled,
}

impl From<SettledJobState> for JobState {
    fn from(state: SettledJobState) -> Self {
        match state {
            SettledJobState::Completed => Self::Completed,
            SettledJobState::Failed => Self::Failed,
            SettledJobState::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecutionRequest {
    pub resource: ExecutionResource,
    pub run_id: String,
    pub tool_name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ResourceLease {
    pub resource: ExecutionResource,
    pub action_id: Uuid,
    pub run_id: String,
    pub tool_name: String,
    pub acquired_at: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionRefusalCode {
    ResourceBusy,
    UnknownTool,
    ScopeInvalid,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecutionRefusal {
    pub code: ExecutionRefusalCode,
    pub summary: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct JobOutcome {
    pub job_id: Uuid,
    pub state: JobState,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub summary: Option<String>,
}

fn deserialize_optional_non_null_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    String::deserialize(deserializer).map(Some)
}

#[derive(Clone)]
pub enum AcquireDecision {
    Granted(super::ResourceLeaseHandle),
    Refused(ExecutionRefusal),
}
