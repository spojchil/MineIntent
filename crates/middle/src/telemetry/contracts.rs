use serde::{de::Deserializer, Deserialize, Serialize};

pub use mineintent_contracts::information::PassiveObservations;
pub use mineintent_contracts::minecraft::{BackendState, Vec3Value};

/// The only debug-state protocol revision currently exposed by the middle layer.
pub const DEBUG_STATE_PROTOCOL: &str = "mineintent.debug-state.v1";

pub const MAX_RECENT_FAILURES: usize = 10;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum DebugStateProtocol {
    #[default]
    #[serde(rename = "mineintent.debug-state.v1")]
    V1,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugFailureSource {
    Backend,
    Model,
    BodyTool,
    Memory,
    Runtime,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DebugFailureSummary {
    pub at: String,
    pub source: DebugFailureSource,
    pub code: String,
    pub summary: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugContextSourceKind {
    Runtime,
    Event,
    Memory,
    Player,
    Summary,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DebugContextSource {
    pub id: String,
    pub kind: DebugContextSourceKind,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DebugInventoryItem {
    pub item_name: String,
    pub count: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DebugBodyState {
    pub position: Vec3Value,
    pub health: f64,
    pub food: f64,
    pub inventory: Vec<DebugInventoryItem>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DebugBodyTool {
    pub id: String,
    pub tool: String,
    pub purpose: String,
    pub started_at: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugDecisionStatus {
    Idle,
    Running,
    Failed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DebugDecision {
    pub status: DebugDecisionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    pub context_sources: Vec<DebugContextSource>,
    pub retrieved_memory_ids: Vec<String>,
}

impl DebugDecision {
    pub fn idle() -> Self {
        Self {
            status: DebugDecisionStatus::Idle,
            run_id: None,
            model: None,
            started_at: None,
            context_sources: Vec::new(),
            retrieved_memory_ids: Vec::new(),
        }
    }
}

/// The mutable, non-derived portion of a participant debug snapshot.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DebugStateInput {
    pub connection: BackendState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<DebugBodyState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_body_tool: Option<DebugBodyTool>,
    pub recent_failures: Vec<DebugFailureSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observations: Option<PassiveObservations>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<DebugDecision>,
}

impl Default for DebugStateInput {
    fn default() -> Self {
        Self {
            connection: BackendState::Idle,
            body: None,
            current_body_tool: None,
            recent_failures: Vec::new(),
            observations: None,
            decision: Some(DebugDecision::idle()),
        }
    }
}

/// A top-level patch. Scalar fields use `None` for an absent field. Optional
/// fields use three states: outer `None` means absent/untouched, `Some(Some)`
/// sets a value, and `Some(None)` explicitly clears the stored value. This is
/// the Rust equivalent of spreading a TypeScript `Partial<DebugStateInput>`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DebugStateUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<BackendState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, deserialize_with = "deserialize_optional_patch")]
    pub body: Option<Option<DebugBodyState>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, deserialize_with = "deserialize_optional_patch")]
    pub current_body_tool: Option<Option<DebugBodyTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent_failures: Option<Vec<DebugFailureSummary>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, deserialize_with = "deserialize_optional_patch")]
    pub observations: Option<Option<PassiveObservations>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, deserialize_with = "deserialize_optional_patch")]
    pub decision: Option<Option<DebugDecision>>,
}

fn deserialize_optional_patch<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

impl From<DebugStateInput> for DebugStateUpdate {
    fn from(input: DebugStateInput) -> Self {
        Self {
            connection: Some(input.connection),
            body: Some(input.body),
            current_body_tool: Some(input.current_body_tool),
            recent_failures: Some(input.recent_failures),
            observations: Some(input.observations),
            decision: Some(input.decision),
        }
    }
}

impl From<&DebugStateInput> for DebugStateUpdate {
    fn from(input: &DebugStateInput) -> Self {
        input.clone().into()
    }
}

impl From<&DebugStateUpdate> for DebugStateUpdate {
    fn from(update: &DebugStateUpdate) -> Self {
        update.clone()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ParticipantDebugState {
    pub protocol: DebugStateProtocol,
    pub revision: u64,
    pub captured_at: String,
    pub connection: BackendState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<DebugBodyState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_body_tool: Option<DebugBodyTool>,
    pub recent_failures: Vec<DebugFailureSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observations: Option<PassiveObservations>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<DebugDecision>,
}

impl ParticipantDebugState {
    pub(crate) fn from_input(revision: u64, captured_at: String, input: DebugStateInput) -> Self {
        Self {
            protocol: DebugStateProtocol::V1,
            revision,
            captured_at,
            connection: input.connection,
            body: input.body,
            current_body_tool: input.current_body_tool,
            recent_failures: input.recent_failures,
            observations: input.observations,
            decision: input.decision,
        }
    }
}
