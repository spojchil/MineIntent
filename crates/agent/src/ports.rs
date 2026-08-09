//! 会话驱动器所需的输入输出端口。

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use crate::types::{
    AgentError, ModelOutput, ModelUsage, ToolCallBatch, ToolDefinition, ToolResultBatch,
    TranscriptItem,
};

/// 装箱后的端口异步返回值使本包无需依赖 `async-trait` 宏。
pub type PortFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 可压缩对话之外的上下文。每次新运行都会刷新这两部分。
pub trait PromptSource: Send + Sync {
    fn base_context(&self) -> Vec<TranscriptItem>;
    fn run_context(&self) -> Vec<TranscriptItem>;
}

/// 可移植的工具目录，以及一次分发整个批次的操作。
///
/// 内核不会逐个遍历调用，也不公开并行标志。工具查找、参数校验、审批、依赖排序、加锁、
/// 并发、限流、重试和远程转发均由实现负责。返回的异步结果应完成整个批次。
pub trait ToolRuntime: Send + Sync {
    fn definitions(&self) -> Vec<ToolDefinition>;

    fn dispatch<'a>(
        &'a self,
        batch: ToolCallBatch,
    ) -> PortFuture<'a, Result<ToolResultBatch, AgentError>>;
}

/// 对话压缩策略。它只接收持久对话，不接收基础上下文或单次运行上下文。需要调用模型的
/// 实现自行持有该依赖。
pub trait Compaction: Send + Sync {
    fn compact<'a>(
        &'a self,
        conversation: &'a [TranscriptItem],
    ) -> PortFuture<'a, Vec<TranscriptItem>>;
}

/// 传给服务商适配器的规范请求。
pub struct ModelRequest {
    pub transcript: Vec<TranscriptItem>,
    /// 仅包含可移植的本地分发函数。托管工具和服务商原生工具在模型适配器中配置，
    /// 不会进入 `ToolRuntime::dispatch`。
    pub function_tools: Vec<ToolDefinition>,
}

/// 经模型适配器规范化的服务商响应。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelResponse {
    pub output: ModelOutput,
    /// 仅用于诊断的服务商值；内核不会据此分支。
    pub finish_reason: Option<Value>,
    pub usage: Option<ModelUsage>,
}

/// 一次模型请求只对应一次语义完整的响应。重试、超时、流式聚合和服务商传输格式转换
/// 均由适配器负责。
pub trait Model: Send + Sync {
    fn complete<'a>(
        &'a self,
        request: ModelRequest,
    ) -> PortFuture<'a, Result<ModelResponse, AgentError>>;
}
