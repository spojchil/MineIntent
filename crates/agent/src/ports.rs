//! 会话驱动器所需的输入输出端口。

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use crate::types::{
    AbortedToolBatch, AgentError, IncrementalToolCall, ModelOutput, ModelUsage,
    ToolBatchAbortReason, ToolBatchStart, ToolCall, ToolCallBatch, ToolCallSlot, ToolDefinition,
    ToolResultBatch, TranscriptItem,
};

/// 装箱后的端口异步返回值使本包无需依赖 `async-trait` 宏。
pub type PortFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 可压缩对话之外的上下文。每次新运行都会刷新这两部分。
pub trait PromptSource: Send + Sync {
    fn base_context(&self) -> Vec<TranscriptItem>;
    fn run_context(&self) -> Vec<TranscriptItem>;
}

/// 一个由增量工具运行时接管的候选批次。
///
/// `submit` 和 `calls_sealed` 的 future 成功只表示运行时已经可靠接管相应信息，不表示工具
/// 已经执行完成。立即执行、缓存、并发、依赖排序和限流都由实现决定。即使 `submit` 返回
/// `Err`，随后 `abort` 的报告仍必须覆盖本次传入的调用，因为远程确认失败不能证明请求从未
/// 到达。只有 `commit` 才等待并返回成功模型响应所对应的完整结果；`abort` 则冻结中断批次
/// 中所有已经传给 `submit` 的调用。
pub trait IncrementalToolBatch: Send {
    fn submit<'a>(
        &'a mut self,
        call: IncrementalToolCall,
    ) -> PortFuture<'a, Result<(), AgentError>>;

    /// 声明工具调用数组已经传输完整。此操作可以早于整个模型响应完成。
    fn calls_sealed<'a>(&'a mut self, call_count: u32) -> PortFuture<'a, Result<(), AgentError>>;

    /// 整个模型响应已经成功结束；等待并返回完整工具结果批次。
    ///
    /// 顶层 `Err` 的严格语义是无法生成可信的完整批次。驱动器会终止运行，不会把部分结果
    /// 伪造成一次可恢复的模型流中断。
    fn commit<'a>(self: Box<Self>) -> PortFuture<'a, Result<ToolResultBatch, AgentError>>
    where
        Self: 'a;

    /// 整个模型响应未能成功结束；停止未开始的工作并返回冻结报告。
    fn abort<'a>(
        self: Box<Self>,
        reason: ToolBatchAbortReason,
    ) -> PortFuture<'a, Result<AbortedToolBatch, AgentError>>
    where
        Self: 'a;
}

/// 可移植的工具目录，以及完整批次或增量批次的接管操作。
///
/// 内核不执行调用，也不公开并行标志。它只把完整数组一次性交给 `dispatch`，或把已经完整
/// 的单项调用及独立封口信号转给增量实现。工具查找、参数校验、审批、依赖排序、加锁、
/// 并发、限流、重试和远程转发均由实现负责。
pub trait ToolRuntime: Send + Sync {
    fn definitions(&self) -> Vec<ToolDefinition>;

    fn dispatch<'a>(
        &'a self,
        batch: ToolCallBatch,
    ) -> PortFuture<'a, Result<ToolResultBatch, AgentError>>;

    /// 可选地接管一个增量候选批次。默认返回 `None`，驱动器会缓存调用，并在模型响应成功
    /// 后沿用 [`ToolRuntime::dispatch`] 一次转发完整数组。
    fn begin_incremental<'a>(
        &'a self,
        _batch: ToolBatchStart,
    ) -> PortFuture<'a, Result<Option<Box<dyn IncrementalToolBatch + 'a>>, AgentError>> {
        Box::pin(async { Ok(None) })
    }
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

/// 模型适配器在聚合一份最终响应期间发出的增量事件。
///
/// `ToolCallReady` 只表示单个调用已完整解析；它既不表示该调用是末项，也不表示整个模型
/// 响应成功。`ToolCallsSealed` 是工具数组自身的独立封口信号，仍需等待
/// [`Model::complete_stream`] 成功返回后才能提交该批次。
#[derive(Clone, Debug, PartialEq)]
pub enum ModelStreamEvent {
    TextDelta { part_index: u32, delta: String },
    ToolCallReady { slot: ToolCallSlot, call: ToolCall },
    ToolCallsSealed { call_count: u32 },
}

/// 模型适配器向驱动器发送增量事件的异步接收端。
pub trait ModelStreamSink: Send {
    /// 返回 `Err` 表示当前模型 attempt 已经失效。adapter 应立即停止读取/发布并把该错误
    /// 从 `complete_stream` 返回；核心仍会粘滞记录首次失败，防止错误实现吞错后提交。
    fn emit<'a>(&'a mut self, event: ModelStreamEvent) -> PortFuture<'a, Result<(), AgentError>>;
}

/// 一次模型请求只对应一次语义完整的响应。重试、超时、流式聚合和服务商传输格式转换
/// 均由适配器负责。
pub trait Model: Send + Sync {
    fn complete<'a>(
        &'a self,
        request: ModelRequest,
    ) -> PortFuture<'a, Result<ModelResponse, AgentError>>;

    /// 流式生成并最终返回同一份完整响应。默认实现保持 one-shot 兼容性且不产生增量事件；
    /// 支持流式协议的适配器覆盖此方法，在内部完成传输解析和最终聚合。
    ///
    /// 一旦向 `sink` 发布任何事件，实现不得把一个新的模型响应静默当作原响应重试并拼接；
    /// 否则正文草稿会重复，而 `ToolCallReady` 还可能已经触发外部影响。只有服务商明确支持
    /// 恢复同一个 response，且适配器能按稳定序号去重时，才可在当前 attempt 内续流；其他
    /// 中断必须返回 `Err`，由 session 冻结增量工具批。
    fn complete_stream<'a>(
        &'a self,
        request: ModelRequest,
        _sink: &'a mut dyn ModelStreamSink,
    ) -> PortFuture<'a, Result<ModelResponse, AgentError>> {
        self.complete(request)
    }
}
