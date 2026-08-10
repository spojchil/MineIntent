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

/// 可压缩对话之外的受保护上下文。
///
/// [`PromptSource::base_context`] 在每次新运行开始时重新读取并置于模型请求最前面；它不会
/// 写入持久对话，也不会交给 [`Compaction`]。实现可以按运行动态生成全部受保护内容；返回
/// 的记录本身必须满足工具轮闭合约束。
pub trait PromptSource: Send + Sync {
    fn base_context(&self) -> Vec<TranscriptItem>;
}

/// 一个由增量工具运行时接管的候选批次。
///
/// “增量”描述调用信息的交付时机，不表示部分结果可以作为正常工具轮提交。一个候选批次有
/// 三个彼此独立的边界：
///
/// 1. [`Self::submit`] 逐项交付已经完整解析的单个调用；
/// 2. [`Self::calls_sealed`] 声明完整调用数组到此结束；
/// 3. [`Self::commit`] 仅在整个模型响应成功结束后发生，并返回完整结果批次。
///
/// `submit` 和 `calls_sealed` 的 future 成功只表示运行时已经可靠接管相应信息，不表示工具
/// 已经执行完成。立即执行、只缓存、并发、依赖排序和限流都由实现决定。`commit` 也只是模型
/// 尝试的协议提交点，不承诺 ACID 事务或自动回滚。
///
/// 如果模型响应没有成功结束，[`Self::abort`] 会取代 `commit`。即使 `submit` 返回 `Err`，
/// `abort` 的报告仍必须覆盖本次传入的调用，因为远程确认失败不能证明请求从未到达。实现
/// 必须停止确定尚未开始的工作、冻结已经接管的调用，并在 `abort` 返回后保证不再产生迟到
/// 执行或结果。只有提前执行远程或不可逆副作用的实现才通常需要持久幂等键、执行账本或状态
/// 查询；只在 `commit` 执行的本地实现可以简单缓存候选调用。
pub trait IncrementalToolBatch: Send {
    /// 接管一个已经完整解析的单项调用。
    ///
    /// 该调用之后是否还有其他调用尚未知；数组完整性只能由 [`Self::calls_sealed`] 声明。
    /// 返回 `Ok` 仅确认可靠接管，是否立刻开始执行由实现决定。
    fn submit<'a>(
        &'a mut self,
        call: IncrementalToolCall,
    ) -> PortFuture<'a, Result<(), AgentError>>;

    /// 声明工具调用数组已经传输完整。此操作可以早于整个模型响应完成，因此封口后仍不能
    /// 单凭此信号把候选批次当作一次成功的模型输出。
    fn calls_sealed<'a>(&'a mut self, call_count: u32) -> PortFuture<'a, Result<(), AgentError>>;

    /// 整个模型响应已经成功结束；等待并返回完整工具结果批次。
    ///
    /// 实现可以到此时才开始执行缓存的调用，也可以等待此前已经开始的调用。这里的
    /// “提交”是 agent 协议语义，不是数据库事务提交。
    ///
    /// 顶层 `Err` 的严格语义是无法生成可信的完整批次。驱动器会终止运行，不会把部分结果
    /// 伪造成一次可恢复的模型流中断。
    fn commit<'a>(self: Box<Self>) -> PortFuture<'a, Result<ToolResultBatch, AgentError>>
    where
        Self: 'a;

    /// 整个模型响应未能成功结束；停止未开始的工作并返回冻结报告。
    ///
    /// 报告只覆盖已经传给 [`Self::submit`] 的完整单项调用。仍在模型流中传输、尚未产生
    /// `ToolCallReady` 的残缺调用从未交给运行时，也不应出现在报告中。返回后不得再执行该
    /// 候选批次中的工作。
    fn abort<'a>(
        self: Box<Self>,
        reason: ToolBatchAbortReason,
    ) -> PortFuture<'a, Result<AbortedToolBatch, AgentError>>
    where
        Self: 'a;
}

/// 可移植的工具目录，以及两种互斥的工具交付模式。
///
/// 默认模式在模型响应成功后把完整数组一次性交给 [`Self::dispatch`]。增量模式由
/// [`Self::begin_incremental`] 显式选择，随后把已经完整解析的单项调用及独立封口信号交给
/// [`IncrementalToolBatch`]；同一候选批次不会再通过 `dispatch` 重复下发。
///
/// 内核不执行调用，也不公开并行标志。工具查找、参数校验、审批、依赖排序、加锁、并发、
/// 限流、重试和远程转发均由实现负责。
pub trait ToolRuntime: Send + Sync {
    fn definitions(&self) -> Vec<ToolDefinition>;

    fn dispatch<'a>(
        &'a self,
        batch: ToolCallBatch,
    ) -> PortFuture<'a, Result<ToolResultBatch, AgentError>>;

    /// 可选地接管一个增量候选批次。
    ///
    /// 默认返回 `None`，驱动器会缓存调用，并在模型响应成功后沿用
    /// [`ToolRuntime::dispatch`] 一次转发完整数组。返回 `Some` 后，该接收器负责本次候选
    /// 批次的逐项接管、封口以及最终 `commit` 或 `abort`。
    fn begin_incremental<'a>(
        &'a self,
        _batch: ToolBatchStart,
    ) -> PortFuture<'a, Result<Option<Box<dyn IncrementalToolBatch + 'a>>, AgentError>> {
        Box::pin(async { Ok(None) })
    }
}

/// 对话压缩策略。
///
/// 驱动器会在下一次模型请求前的安全边界以及运行收尾时按阈值调用它，同一运行中可能调用
/// 多次。此端口只接收可持久化对话，不接收 [`PromptSource::base_context`]；闭合的返回值会
/// 整体替换传入的对话，非法返回值则被丢弃并沿用原记录。驱动器不保留任何特定普通消息，
/// 因此摘要器可以把恢复回执等输入概括进新的普通消息。需要调用模型的实现自行持有该依赖。
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
/// 工具流同样区分三个边界：
///
/// - `ToolCallReady`：一个单项调用已经完整解析，可以按 slot 接管；它不表示这是末项。
/// - `ToolCallsSealed`：工具数组已经完整；它不表示正文、推理块或整个模型响应已经成功。
/// - [`Model::complete_stream`] 返回 `Ok`：最终规范响应已经成功聚合，此时增量批次才可
///   `commit`。
///
/// 例如 A 已发出 `ToolCallReady`、B 尚在传输时断流，运行时只会冻结 A；残缺的 B 不会被
/// 当作调用，未完成的模型草稿也不会成为正常对话轮次。
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

    /// 流式生成并最终返回同一份完整响应。默认实现直接委托 [`Self::complete`] 且不产生
    /// 增量事件；支持流式协议的适配器覆盖此方法，在内部完成传输解析和最终聚合。
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
