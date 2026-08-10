//! 一个小型、提供方中立的智能体内核。
//!
//! # 工具调用的两种交付模式
//!
//! 两种模式互斥，但共享同一批次的关联和结果完整性约束：
//!
//! - **默认整批模式**：模型响应成功结束后，内核才把完整、有序的调用数组通过
//!   [`ToolRuntime::dispatch`] 一次性交给运行时。
//! - **增量接管模式**：运行时通过 [`ToolRuntime::begin_incremental`] 显式选择加入。每个
//!   已经完整解析的单项调用按 `(batch_attempt_id, slot)` 交给
//!   [`IncrementalToolBatch::submit`]；这不表示整个调用数组已经完整。只有独立的
//!   [`IncrementalToolBatch::calls_sealed`] 才封闭数组，而整个模型响应仍须成功结束后才会
//!   调用 [`IncrementalToolBatch::commit`]。
//!
//! 因而“逐项接管”与“完整批次提交”不是同一个边界。增量运行时可以在 `submit` 时立即执行，
//! 也可以只缓存调用并等到 `commit` 再执行；内核不会替应用选择调度策略。最终返回给状态机的
//! 仍必须是完整结果批次，结果通过调用标识符关联，并按模型发出顺序写入对话。
//!
//! # 模型流中断与已经发生的工具事实
//!
//! 假设调用 A 已完整解析并被增量运行时接管，而调用 B 仍在传输时模型流断开：
//!
//! - 未成功结束的模型输出只是草稿，不会写入规范对话，也不能与普通 `ToolResults` 拼成一个
//!   看似完整的工具轮；
//! - session 会 `abort` 候选批次。运行时必须冻结已经传给 `submit` 的调用，并把 A 分类为
//!   已完成、确定未开始或结果未知；尚未完整解析的 B 从未提交给运行时；
//! - 已完成或结果未知的真实事实会作为一条普通恢复输入提交，而不是伪造残缺 Assistant
//!   输出或工具结果。它和其他历史一样可由 [`Compaction`] 在请求边界概括或替换；确定未
//!   开始的调用没有副作用，因此无需写入恢复回执。
//!
//! 回执中的已完成结果来自工具运行时；内核不会把未知结果改写成成功。`commit` 和 `abort`
//! 是模型尝试的协议边界，不提供数据库事务或自动回滚。提前执行不可逆操作的运行时应自行
//! 保证去重、停止未开始的工作，并在 `abort` 返回前得到可冻结的执行结论。若服务商能够以
//! 稳定序号恢复同一个 response，adapter 可以在当前模型尝试内续流；发起一个新的模型响应
//! 不等同于续流。
//!
//! 内核还保证运行时输入仅在显式的请求或完成边界排空。提供方 wire 格式位于可选的
//! `adapters` 模块，不进入核心状态机。`openai` 与 `anthropic` feature 分别启用相应协议，
//! 二者共享 `http` transport。

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
