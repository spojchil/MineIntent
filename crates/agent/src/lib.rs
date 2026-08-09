//! 持有一段持续对话的 agent 会话。
//!
//! 所需能力全部经四个端口注入：提示词源、工具、上下文压缩策略、模型。
//! 对外三个入口：`inject_if_running`（向活动轮的请求边界信箱投递）、
//! `start_if_idle`（空闲时起轮）、`stop`。
//! 本层不设请求次数上限；上下文体积由压缩触发约束。

mod mailbox;
mod ports;
mod run;
mod session;
mod types;

pub use ports::{Compaction, Message, Model, ModelCompletion, ModelRequest, PortFuture, PromptSource, Tools};
pub use run::{PlannedToolCall, ToolResult, Turn, TurnStep};
pub use session::{AgentSession, SessionConfig, StartRejected, StartRejectedReason, TurnOutcome};
pub use types::{
    AgentError, AgentErrorKind, JsonObject, ModelUsage, RunId, ToolCallId, ToolDefinition,
    ToolInvocation, ToolName,
};
