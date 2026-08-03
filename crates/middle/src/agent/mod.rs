//! 进程内 Agent 模型—工具循环。
//!
//! 初始模型消息由上层 context composer 提供；本模块只负责可步进的循环状态、
//! 调用上限、tool-call 一次性 claim 与严格的结果配对。

mod context;
mod driver;
mod prompt;
mod run;
mod runner;
mod transcript;
mod viewport;
mod viewport_mirror;
mod viewport_reducer;

pub use context::{
    AgentChatInputV5, AgentChatTriggerV5, AgentContextV5Assembler, AgentContextV5AssemblyError,
    AgentContextV5EventInput, AgentContextV5Input,
};
pub use driver::{AgentLoopDriver, AgentLoopOutcome, AgentModelRequest};
pub use prompt::{
    initial_messages, initial_messages_v4_legacy, system_prompt, template_text, PromptError,
};
pub use run::{
    AgentRun, AgentRunStep, AgentToolResult, ModelCompletion, PlannedToolCall,
    MAX_MODEL_REQUESTS_PER_RUN, MAX_TOOL_CALLS_PER_RESPONSE, MAX_TOOL_CALLS_PER_RUN,
};
pub use runner::{AgentRunnerImpl, ConcreteAgentRunner};
pub use transcript::{
    summarize_error, transcript_path, transcript_path_from_env, AgentTranscriptRecord,
    FileTranscriptStore, TranscriptSink, TranscriptUsage, MAX_TRANSCRIPT_BYTES,
    MAX_TRANSCRIPT_CHARS, TRANSCRIPT_FILE_NAME, TRANSCRIPT_PROTOCOL,
};
pub use viewport::{
    BackendRoundViewportSampler, FixedUtcTimestampSource, NoRoundViewportSampler,
    RoundViewportSampler, SystemUtcTimestampSource, UtcTimestampSource,
};
pub use viewport_mirror::{
    block_fact_key, KeyframeReason, MirrorLimits, PendingViewportFrame, ViewportCommitError,
    ViewportFrame, ViewportMirror, ViewportMirrorError, ViewportObservation, ViewportProposal,
};
pub use viewport_reducer::{ViewportIncrementalReducer, ViewportReducedState, ViewportReplayError};
