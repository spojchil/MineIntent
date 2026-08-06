//! 进程内 Agent 模型—工具循环。
//!
//! 初始模型消息由上层 context composer 提供；本模块只负责可步进的循环状态、
//! 调用上限、tool-call 一次性 claim 与严格的结果配对。

mod context;
mod driver;
mod prompt;
mod runner;
mod viewport;
mod viewport_mirror;
mod viewport_reducer;

// 循环本体与转录已迁往 `toolloop`（领域无关）。这里按原名转出，调用方不受影响；
// 待 driver/runner 也脱离领域之后，本模块只剩领域侧的 context/prompt/viewport。
pub use toolloop::{
    summarize_error, transcript_path, transcript_path_from_env, AgentRun, AgentRunStep,
    AgentToolResult, AgentTranscriptRecord, FileTranscriptStore, ModelCompletion, PlannedToolCall,
    TranscriptSink, TranscriptUsage, MAX_MODEL_REQUESTS_PER_RUN, MAX_TOOL_CALLS_PER_RESPONSE,
    MAX_TOOL_CALLS_PER_RUN, MAX_TRANSCRIPT_BYTES, MAX_TRANSCRIPT_CHARS, TRANSCRIPT_FILE_NAME,
    TRANSCRIPT_PROTOCOL,
};

pub use context::{
    AgentChatInputV5, AgentChatTriggerV5, AgentContextV5Assembler, AgentContextV5AssemblyError,
    AgentContextV5EventInput, AgentContextV5Input,
};
pub use driver::{AgentLoopDriver, AgentLoopOutcome, AgentModelRequest};
pub use prompt::{
    initial_messages, initial_messages_v4_legacy, system_prompt, template_text, PromptError,
};
pub use runner::{AgentRunnerImpl, ConcreteAgentRunner};
pub use viewport::{
    BackendRoundViewportSampler, FixedUtcTimestampSource, NoRoundViewportSampler,
    RoundViewportSampler, SystemUtcTimestampSource, UtcTimestampSource,
};
pub use viewport_mirror::{
    block_fact_key, KeyframeReason, MirrorLimits, PendingViewportFrame, ViewportCommitError,
    ViewportFrame, ViewportMirror, ViewportMirrorError, ViewportObservation, ViewportProposal,
};
pub use viewport_reducer::{ViewportIncrementalReducer, ViewportReducedState, ViewportReplayError};
