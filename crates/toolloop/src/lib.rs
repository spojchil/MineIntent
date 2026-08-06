//! 领域无关的模型—工具循环。
//!
//! 这里只有「模型说一轮话、工具执行一批、结果原样配回去」这条线，以及守住
//! 它的六条纪律：可回放（无效调用也产生配对的失败结果）、tool-call ID 在任何
//! 副作用之前一次性 claim、结果数量/ID/顺序严格配对、工具 panic 转结构化失败、
//! 每响应与每 run 的调用上限、状态机强制调用顺序。
//!
//! 初始消息由上层组装后传进来；本 crate 不渲染提示词、不认识任何领域概念。
//! 换一套工具就是换一种 agent —— 这是它单独成 crate 的全部理由，也是它的
//! 依赖表必须保持干净的理由。

mod control;
mod run;
mod transcript;

pub use control::{await_with_control, is_run_control_error};

pub use run::{
    AgentRun, AgentRunStep, AgentToolResult, ModelCompletion, PlannedToolCall,
    MAX_MODEL_REQUESTS_PER_RUN, MAX_TOOL_CALLS_PER_RESPONSE, MAX_TOOL_CALLS_PER_RUN,
};
pub use transcript::{
    summarize_error, transcript_path, transcript_path_from_env, utc_timestamp_now,
    AgentTranscriptRecord, FileTranscriptStore, TranscriptSink, TranscriptUsage,
    MAX_TRANSCRIPT_BYTES, MAX_TRANSCRIPT_CHARS, TRANSCRIPT_FILE_NAME, TRANSCRIPT_PROTOCOL,
};
