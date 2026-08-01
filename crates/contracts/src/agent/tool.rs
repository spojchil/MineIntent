use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use super::{
    context::deserialize_optional_non_null, JsonAgentFrame, ModelName, RunId, ToolCallId,
    ToolDefinitionName, ToolName,
};

pub type JsonObject = Map<String, Value>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum AgentContextProtocol {
    #[default]
    #[serde(rename = "mineintent.agent-context.v3")]
    V3,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ToolResponseProtocol {
    #[default]
    #[serde(rename = "mineintent.tool-response.v2")]
    V2,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum AgentRunProtocol {
    #[default]
    #[serde(rename = "mineintent.agent-run.v1")]
    V1,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ToolDefinitionType {
    #[default]
    #[serde(rename = "function")]
    Function,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FunctionToolDefinition {
    pub name: ToolDefinitionName,
    pub description: String,
    pub parameters: JsonObject,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WireToolDefinition {
    pub r#type: ToolDefinitionType,
    pub function: FunctionToolDefinition,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ToolInvocation {
    pub run_id: RunId,
    pub tool_call_id: ToolCallId,
    pub name: ToolName,
    pub arguments: JsonObject,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ToolCallKey {
    pub run_id: RunId,
    pub tool_call_id: ToolCallId,
}

impl From<&ToolInvocation> for ToolCallKey {
    fn from(invocation: &ToolInvocation) -> Self {
        Self {
            run_id: invocation.run_id.clone(),
            tool_call_id: invocation.tool_call_id.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(transparent)]
pub struct RequiredNullable<T>(pub Option<T>);

impl<T> RequiredNullable<T> {
    pub fn new(value: Option<T>) -> Self {
        Self(value)
    }

    pub fn as_ref(&self) -> Option<&T> {
        self.0.as_ref()
    }

    pub fn into_inner(self) -> Option<T> {
        self.0
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(
    bound(
        deserialize = "Observation: Deserialize<'de>",
        serialize = "Observation: Serialize"
    ),
    deny_unknown_fields,
    rename_all = "camelCase"
)]
pub struct ToolExecution<Observation = JsonAgentFrame> {
    pub protocol: ToolResponseProtocol,
    pub result: Value,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub observation_after: RequiredNullable<Observation>,
}

impl<Observation> ToolExecution<Observation> {
    pub fn new(result: Value, observation_after: Option<Observation>) -> Self {
        Self {
            protocol: ToolResponseProtocol::V2,
            result,
            observation_after: RequiredNullable::new(observation_after),
        }
    }
}

fn deserialize_required_nullable<'de, D, T>(
    deserializer: D,
) -> Result<RequiredNullable<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(RequiredNullable::new)
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelUsage {
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub input_tokens: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub output_tokens: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub cache_read_tokens: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub cache_write_tokens: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelRunResult {
    pub protocol: AgentRunProtocol,
    pub model: ModelName,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub usage: Option<ModelUsage>,
}
