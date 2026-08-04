use std::{future::Future, pin::Pin};

use serde::{ser::Error as _, Deserialize, Deserializer, Serialize, Serializer};

use super::{
    AgentError, ExecutionControl, ModelRunResult, PromptTemplateKey, PromptTemplateVersion, RunId,
    WireToolDefinition,
};

pub type ContractFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub const MAX_AGENT_RUN_TOOLS: usize = 32;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PromptTemplateRef {
    pub key: PromptTemplateKey,
    pub version: PromptTemplateVersion,
}

#[derive(Clone, Debug)]
pub struct AgentRunRequest<Context> {
    pub run_id: RunId,
    pub context: Context,
    pub tools: Vec<WireToolDefinition>,
    pub prompt_template: PromptTemplateRef,
}

impl<Context> AgentRunRequest<Context> {
    pub fn validate(&self) -> Result<(), AgentError> {
        if self.tools.len() > MAX_AGENT_RUN_TOOLS {
            return Err(AgentError::new(
                super::AgentErrorCode::LimitExceeded,
                "agent_run_tool_limit_exceeded",
            ));
        }

        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SerializedAgentRunRequest<'a, Context> {
    run_id: &'a RunId,
    context: &'a Context,
    tools: &'a [WireToolDefinition],
    prompt_template: &'a PromptTemplateRef,
}

impl<Context> Serialize for AgentRunRequest<Context>
where
    Context: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(S::Error::custom)?;
        SerializedAgentRunRequest {
            run_id: &self.run_id,
            context: &self.context,
            tools: &self.tools,
            prompt_template: &self.prompt_template,
        }
        .serialize(serializer)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawAgentRunRequest<Context> {
    run_id: RunId,
    context: Context,
    tools: Vec<WireToolDefinition>,
    prompt_template: PromptTemplateRef,
}

impl<'de, Context> Deserialize<'de> for AgentRunRequest<Context>
where
    Context: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawAgentRunRequest::<Context>::deserialize(deserializer)?;
        let request = Self {
            run_id: raw.run_id,
            context: raw.context,
            tools: raw.tools,
            prompt_template: raw.prompt_template,
        };
        request.validate().map_err(serde::de::Error::custom)?;
        Ok(request)
    }
}

pub trait AgentRunner: Send + Sync {
    type Context: Send;

    fn run<'a>(
        &'a self,
        request: AgentRunRequest<Self::Context>,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<ModelRunResult, AgentError>>;
}

pub trait ModelProvider: Send + Sync {
    type Request: Send;
    type Response: Send;

    fn complete<'a>(
        &'a self,
        request: Self::Request,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<Self::Response, AgentError>>;
}
