//! 进程内 Agent 模型—工具循环。
//!
//! 初始模型消息由上层 context composer 提供；本模块只负责可步进的循环状态、
//! 调用上限、tool-call 一次性 claim 与严格的结果配对。

mod run;

pub use run::{
    AgentRun, AgentRunStep, AgentToolResult, ModelCompletion, PlannedToolCall,
    MAX_MODEL_REQUESTS_PER_RUN, MAX_TOOL_CALLS_PER_RESPONSE, MAX_TOOL_CALLS_PER_RUN,
};
