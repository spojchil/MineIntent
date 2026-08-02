//! Agent 与模型 provider 的公共契约。
//!
//! 这里只描述进程内边界和模型可见数据，不包含 Agent 循环实现、模型 SDK 或传输层。

mod context;
mod control;
mod error;
pub mod fixtures;
mod ids;
mod runner;
mod tool;

pub use context::{
    AgentDecisionContext, AgentEvent, AgentFrame, AgentSelf, AgentWorld, JsonAgentDecisionContext,
    JsonAgentFrame, PlayerMessage, StableContext,
};
pub use control::{CancellationSignal, Deadline, ExecutionControl};
pub use error::{AgentError, AgentErrorCode};
pub use ids::{
    ModelName, PromptTemplateKey, PromptTemplateVersion, RunId, ToolCallId, ToolDefinitionName,
    ToolName,
};
pub use runner::{
    AgentRunRequest, AgentRunner, ContractFuture, ModelProvider, PromptTemplateRef,
    MAX_AGENT_RUN_TOOLS,
};
pub use tool::{
    AgentContextProtocol, AgentRunProtocol, FunctionToolDefinition, JsonObject, ModelRunResult,
    ModelUsage, RequiredNullable, ToolCallKey, ToolDefinitionType, ToolExecution, ToolInvocation,
    ToolResponseProtocol, WireToolDefinition,
};
