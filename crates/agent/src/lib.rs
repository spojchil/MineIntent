//! 一个小型、提供方中立的智能体内核。
//!
//! 内核只负责维护协议不变量：
//! - 模型适配器返回类型化的本地工具调用，而不是提供方的传输格式 JSON；
//! - 默认把完整调用数组只向 `ToolRuntime::dispatch` 转发一次；选择增量端口的实现则按
//!   `(batch_attempt_id, slot)` 接管完整调用，并通过独立封口信号结束传输；
//! - 完整结果通过标识符关联，并按模型的发出顺序提交；
//! - 运行时输入仅在显式的请求或完成边界排空。
//!
//! 工具调度策略仍由应用实现；提供方 wire 格式位于可选的 `adapters` 模块，不进入核心
//! 状态机。`openai` 与 `anthropic` feature 分别启用相应协议，二者共享 `http` transport。

#[cfg(any(feature = "openai", feature = "anthropic"))]
pub mod adapters;
mod events;
mod mailbox;
mod ports;
mod run;
mod session;
mod types;

pub use events::{
    AgentEvent, EventCategory, EventKind, EventLevel, EventMetadata, FilteredObserver, LevelFilter,
    ModelStreamObservation, NoopObserver, NoopStreamObserver, ObservedModelStreamEvent, Observer,
    RunStage, StreamObserver, ToolCallSummary,
};
pub use mailbox::{Delivery, MailboxInput, MailboxRejected, MailboxRejectedReason};
pub use ports::{
    Compaction, IncrementalToolBatch, Model, ModelRequest, ModelResponse, ModelStreamEvent,
    ModelStreamSink, PortFuture, PromptSource, ToolRuntime,
};
pub use run::{RequestBoundaryKind, Turn, TurnStep};
pub use session::{AgentSession, SessionConfig, StartRejected, StartRejectedReason, TurnOutcome};
pub use types::{
    AbortedToolBatch, AbortedToolCall, AbortedToolCallOutcome, AgentError, AgentErrorKind,
    ContentPart, IncrementalToolCall, InputMessage, InterruptedToolBatchReceipt,
    InterruptedToolCallOutcome, InterruptedToolCallReceipt, JsonObject, ModelOutput, ModelUsage,
    RunId, ToolBatchAbortReason, ToolBatchAttemptId, ToolBatchId, ToolBatchStart, ToolCall,
    ToolCallBatch, ToolCallId, ToolCallSlot, ToolDefinition, ToolName, ToolResult, ToolResultBatch,
    ToolResultStatus, TranscriptItem,
};
