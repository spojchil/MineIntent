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
mod viewport;
mod viewport_incremental;

pub use context::{
    AgentChatItemV5, AgentChatMessageV5, AgentChatMovedMarkerV5, AgentChatMovedV5, AgentChatV5,
    AgentContextProtocolV3, AgentContextProtocolV4, AgentContextProtocolV5, AgentDecisionContext,
    AgentDecisionContextV3, AgentDecisionContextV4, AgentDecisionContextV5, AgentEvent,
    AgentEventV5, AgentFrame, AgentFrameV5, AgentHotbarV5, AgentItemStackV5, AgentPoseV5,
    AgentSelf, AgentStatusV5, AgentWorld, AgentWorldV5, JsonAgentDecisionContext,
    JsonAgentDecisionContextV4, JsonAgentDecisionContextV5, JsonAgentFrame, JsonAgentFrameV5,
    PlayerMessage, StableContext, StableContextV3, StableContextV4, StableContextV5,
    MAX_CHAT_ITEMS_V5, MAX_HOTBAR_SLOT,
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
pub use viewport::{
    ViewportFrameMessage, ViewportFrameMessageV2, ViewportFrameProtocol, ViewportFrameProtocolV2,
    ViewportFrameV2WireError, ViewportFrameWireError,
};
pub use viewport_incremental::{
    ViewportBaselineId, ViewportDeltaError, ViewportDeltaV1, ViewportIncrementalFrameError,
    ViewportIncrementalFrameMessageV1, ViewportIncrementalFrameProtocol,
    ViewportIncrementalPayloadV1, ViewportKeyframeError, ViewportKeyframeV1, ViewportScope,
    ViewportScopeError, ViewportUnverifiedReason,
};
