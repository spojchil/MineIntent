use std::{future::Future, pin::Pin};

use serde::{Deserialize, Deserializer, Serialize};

use super::{
    AgentError, ExecutionControl, ModelRunResult, PromptTemplateKey, PromptTemplateVersion, RunId,
    WireToolDefinition,
};

pub type ContractFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PromptTemplateRef {
    pub key: PromptTemplateKey,
    pub version: PromptTemplateVersion,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunRequest<Context> {
    pub run_id: RunId,
    pub context: Context,
    pub tools: Vec<WireToolDefinition>,
    pub prompt_template: PromptTemplateRef,
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
        if raw.tools.len() > 32 {
            return Err(serde::de::Error::custom(
                "agent run request may advertise at most 32 tools",
            ));
        }
        Ok(Self {
            run_id: raw.run_id,
            context: raw.context,
            tools: raw.tools,
            prompt_template: raw.prompt_template,
        })
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
