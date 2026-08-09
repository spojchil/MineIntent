//! 围绕无 I/O `Turn` 的并发会话驱动器。

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{Mutex, Notify};

use crate::events::{
    AgentEvent, EventKind, EventMetadata, ModelStreamObservation, NoopObserver, NoopStreamObserver,
    ObservedModelStreamEvent, Observer, RunStage, StreamObserver, ToolCallSummary,
};
use crate::mailbox::{Mailbox, MailboxInput, MailboxRejected, MailboxRejectedReason};
use crate::ports::{
    Compaction, IncrementalToolBatch, Model, ModelRequest, ModelResponse, ModelStreamEvent,
    ModelStreamSink, PortFuture, PromptSource, ToolRuntime,
};
use crate::run::{RequestBoundaryKind, Turn, TurnStep};
use crate::types::{
    validate_closed_transcript, AbortedToolBatch, AbortedToolCallOutcome, AgentError,
    AgentErrorKind, ContentPart, IncrementalToolCall, InputMessage, InterruptedToolBatchReceipt,
    InterruptedToolCallOutcome, InterruptedToolCallReceipt, ModelOutput, ModelUsage, RunId,
    ToolBatchAbortReason, ToolBatchAttemptId, ToolBatchStart, ToolCall, ToolCallSlot,
    ToolResultStatus, TranscriptItem,
};

const RECOVERY_RECEIPT_MARKER_KEY: &str = "agent.interrupted_tool_batch_receipt";
const RECOVERY_RECEIPT_KIND: &str = "interrupted_tool_batch_receipt";

#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// 当持久化对话记录的序列化大小超过此值时，在运行结束后压缩它。
    pub compaction_trigger_bytes: usize,
    /// 中断工具批次的执行事实在下一次模型请求中使用的普通输入角色。
    ///
    /// 默认 `user` 只用于兼容现有协议；内核不赋予该字符串固定语义，应用可按适配器能力
    /// 改为 `developer`、`operator` 或自定义角色。
    pub interrupted_tool_receipt_role: String,
    /// 单次运行允许从“已有工具执行事实的模型流中断”自动继续的最大次数。
    pub max_interrupted_tool_recoveries: u32,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            compaction_trigger_bytes: 256 * 1024,
            interrupted_tool_receipt_role: "user".to_owned(),
            max_interrupted_tool_recoveries: 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartRejectedReason {
    Busy,
    Stopping,
    /// 初始记录段包含孤立或未闭合的工具调用/结果。
    InvalidInput,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StartRejected {
    pub reason: StartRejectedReason,
    pub initial_items: Vec<TranscriptItem>,
}

impl std::fmt::Display for StartRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "run start rejected: {:?}", self.reason)
    }
}

impl std::error::Error for StartRejected {}

#[derive(Clone, Debug, PartialEq)]
pub enum TurnOutcome {
    Completed {
        output: ModelOutput,
        usage: Option<ModelUsage>,
    },
    Stopped {
        undelivered: Vec<MailboxInput>,
    },
    Failed {
        error: AgentError,
        undelivered: Vec<MailboxInput>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunPhase {
    Open,
    Sealed,
    Finalizing,
}

struct ActiveRun {
    id: RunId,
    phase: RunPhase,
}

#[derive(Default)]
struct SessionState {
    conversation: Vec<TranscriptItem>,
    run_seq: u64,
    active: Option<ActiveRun>,
    stopping: bool,
    mailbox: Mailbox,
}

enum IncrementalMode<'a> {
    Uninitialized,
    Unsupported,
    Active(Box<dyn IncrementalToolBatch + 'a>),
    Closed,
}

/// 一次模型请求期间的暂存聚合状态。模型 adapter 仍负责解析 wire；这里仅校验规范事件，
/// 并把已经完整的调用转发给可选的增量工具运行时。
struct ModelAttemptSink<'a> {
    session: &'a AgentSession,
    tools: &'a dyn ToolRuntime,
    run_id: RunId,
    request_index: u64,
    start: ToolBatchStart,
    ready_calls: BTreeMap<ToolCallSlot, ToolCall>,
    offered_calls: BTreeMap<ToolCallSlot, ToolCall>,
    sealed_count: Option<u32>,
    mode: IncrementalMode<'a>,
    runtime_rejected: bool,
    first_failure: Option<AgentError>,
    closed: bool,
}

impl<'a> ModelAttemptSink<'a> {
    fn new(session: &'a AgentSession, run_id: RunId, request_index: u64) -> Self {
        let batch_attempt_id =
            ToolBatchAttemptId::new(format!("{}/model/{request_index}/tools", run_id.as_str()));
        Self {
            session,
            tools: session.tools.as_ref(),
            start: ToolBatchStart {
                run_id: run_id.clone(),
                batch_attempt_id,
            },
            run_id,
            request_index,
            ready_calls: BTreeMap::new(),
            offered_calls: BTreeMap::new(),
            sealed_count: None,
            mode: IncrementalMode::Uninitialized,
            runtime_rejected: false,
            first_failure: None,
            closed: false,
        }
    }

    fn batch_attempt_id(&self) -> &ToolBatchAttemptId {
        &self.start.batch_attempt_id
    }

    fn close(&mut self) {
        self.closed = true;
    }

    fn runtime_rejected(&self) -> bool {
        self.runtime_rejected
    }

    fn first_failure(&self) -> Option<&AgentError> {
        self.first_failure.as_ref()
    }

    async fn ensure_incremental(&mut self) -> Result<(), AgentError> {
        if !matches!(self.mode, IncrementalMode::Uninitialized) {
            return Ok(());
        }

        let receiver = match self.tools.begin_incremental(self.start.clone()).await {
            Ok(receiver) => receiver,
            Err(error) => {
                self.runtime_rejected = true;
                return Err(error);
            }
        };
        self.mode = match receiver {
            Some(receiver) => IncrementalMode::Active(receiver),
            None => IncrementalMode::Unsupported,
        };
        Ok(())
    }

    async fn accept_ready(&mut self, slot: ToolCallSlot, call: ToolCall) -> Result<(), AgentError> {
        if self.sealed_count.is_some() {
            return Err(AgentError::new(
                AgentErrorKind::Model,
                "tool_call_ready_after_calls_sealed",
            ));
        }
        if call.id.as_str().is_empty() {
            return Err(AgentError::new(
                AgentErrorKind::InvalidToolBatch,
                "empty_tool_call_id",
            ));
        }
        if let Some(previous) = self.ready_calls.get(&slot) {
            return if previous == &call {
                Ok(())
            } else {
                Err(AgentError::new(
                    AgentErrorKind::Model,
                    "conflicting_tool_call_slot",
                ))
            };
        }
        if self
            .ready_calls
            .values()
            .any(|previous| previous.id == call.id)
        {
            return Err(AgentError::new(
                AgentErrorKind::InvalidToolBatch,
                "duplicate_tool_call_id",
            ));
        }

        self.ready_calls.insert(slot, call.clone());
        self.ensure_incremental().await?;
        if let IncrementalMode::Active(receiver) = &mut self.mode {
            let item = IncrementalToolCall {
                run_id: self.run_id.clone(),
                batch_attempt_id: self.start.batch_attempt_id.clone(),
                slot,
                call: call.clone(),
            };
            // 先记录已经交给 submit 的调用。即使确认包丢失导致 submit 返回 Err，runtime
            // 仍可能已经接管或执行；随后 abort 必须把这个不确定窗口分类出来。
            self.offered_calls.insert(slot, call);
            // 此 await 只等待运行时可靠确认接管，不允许等待实际工具执行完成。
            if let Err(error) = receiver.submit(item).await {
                self.runtime_rejected = true;
                return Err(error);
            }
        }
        Ok(())
    }

    async fn accept_seal(&mut self, call_count: u32) -> Result<(), AgentError> {
        if let Some(previous) = self.sealed_count {
            return if previous == call_count {
                Ok(())
            } else {
                Err(AgentError::new(
                    AgentErrorKind::Model,
                    "conflicting_tool_call_count",
                ))
            };
        }

        let ready_count = u32::try_from(self.ready_calls.len())
            .map_err(|_| AgentError::new(AgentErrorKind::Model, "tool_call_count_exceeds_u32"))?;
        let contiguous =
            (0..call_count).all(|slot| self.ready_calls.contains_key(&ToolCallSlot::new(slot)));
        if ready_count != call_count || !contiguous {
            return Err(AgentError::new(
                AgentErrorKind::Model,
                "tool_calls_sealed_before_all_calls_ready",
            ));
        }

        if call_count > 0 {
            self.ensure_incremental().await?;
            if let IncrementalMode::Active(receiver) = &mut self.mode {
                // 与 submit 相同，此处只等待运行时接管“不会再有新调用”的事实。
                if let Err(error) = receiver.calls_sealed(call_count).await {
                    self.runtime_rejected = true;
                    return Err(error);
                }
            }
        }
        self.sealed_count = Some(call_count);
        self.session.emit_tool_calls_sealed(
            &self.run_id,
            self.start.batch_attempt_id.clone(),
            call_count,
        );
        Ok(())
    }

    async fn reconcile_success(&mut self, response: &ModelResponse) -> Result<(), AgentError> {
        let calls = &response.output.tool_calls;
        let final_count = u32::try_from(calls.len())
            .map_err(|_| AgentError::new(AgentErrorKind::Model, "tool_call_count_exceeds_u32"))?;

        // 在把终态响应中尚未流出的调用交给 runtime 之前，先验证整个数组。
        // 这样 one-shot 与 Chat 的批量终态不会因后项非法而提前执行前项。
        let mut final_call_ids = HashSet::with_capacity(calls.len());
        for call in calls {
            if call.id.as_str().is_empty() {
                return Err(AgentError::new(
                    AgentErrorKind::InvalidToolBatch,
                    "empty_tool_call_id",
                ));
            }
            if !final_call_ids.insert(&call.id) {
                return Err(AgentError::new(
                    AgentErrorKind::InvalidToolBatch,
                    "duplicate_tool_call_id",
                ));
            }
        }

        for (slot, streamed) in &self.ready_calls {
            let Some(final_call) = calls.get(slot.get() as usize) else {
                return Err(AgentError::new(
                    AgentErrorKind::Model,
                    "streamed_tool_call_missing_from_final_response",
                ));
            };
            if streamed != final_call {
                return Err(AgentError::new(
                    AgentErrorKind::Model,
                    "streamed_tool_call_differs_from_final_response",
                ));
            }
        }
        if let Some(sealed_count) = self.sealed_count {
            if sealed_count != final_count {
                return Err(AgentError::new(
                    AgentErrorKind::Model,
                    "sealed_tool_call_count_differs_from_final_response",
                ));
            }
        }

        for (index, call) in calls.iter().enumerate() {
            let slot = ToolCallSlot::new(u32::try_from(index).map_err(|_| {
                AgentError::new(AgentErrorKind::Model, "tool_call_index_exceeds_u32")
            })?);
            if !self.ready_calls.contains_key(&slot) {
                self.accept_ready(slot, call.clone()).await?;
            }
        }

        if final_count > 0 && self.sealed_count.is_none() {
            self.accept_seal(final_count).await?;
        }
        Ok(())
    }

    fn take_incremental(&mut self) -> Option<Box<dyn IncrementalToolBatch + 'a>> {
        match std::mem::replace(&mut self.mode, IncrementalMode::Closed) {
            IncrementalMode::Active(receiver) => Some(receiver),
            IncrementalMode::Uninitialized
            | IncrementalMode::Unsupported
            | IncrementalMode::Closed => None,
        }
    }

    async fn abort(
        &mut self,
        reason: ToolBatchAbortReason,
    ) -> Result<Option<InterruptedToolBatchReceipt>, AgentError> {
        self.close();
        let Some(receiver) = self.take_incremental() else {
            return Ok(None);
        };
        let report = receiver.abort(reason).await?;
        self.validate_abort_report(report)
    }

    fn validate_abort_report(
        &self,
        mut report: AbortedToolBatch,
    ) -> Result<Option<InterruptedToolBatchReceipt>, AgentError> {
        if report.batch_attempt_id != self.start.batch_attempt_id {
            return Err(AgentError::new(
                AgentErrorKind::InvalidToolBatch,
                "aborted_tool_batch_id_mismatch",
            ));
        }
        if report.calls.len() != self.offered_calls.len() {
            return Err(AgentError::new(
                AgentErrorKind::InvalidToolBatch,
                "aborted_tool_call_count_mismatch",
            ));
        }
        report.calls.sort_by_key(|call| call.slot);
        let mut seen = HashSet::with_capacity(report.calls.len());
        let mut receipts = Vec::new();
        let mut settled_count = 0;
        let mut cancelled_count = 0;
        let mut unknown_count = 0;

        for aborted in report.calls {
            if !seen.insert(aborted.slot) {
                return Err(AgentError::new(
                    AgentErrorKind::InvalidToolBatch,
                    "duplicate_aborted_tool_call_slot",
                ));
            }
            let Some(call) = self.offered_calls.get(&aborted.slot) else {
                return Err(AgentError::new(
                    AgentErrorKind::InvalidToolBatch,
                    "unknown_aborted_tool_call_slot",
                ));
            };
            if aborted.call_id != call.id {
                return Err(AgentError::new(
                    AgentErrorKind::InvalidToolBatch,
                    "aborted_tool_call_id_mismatch",
                ));
            }

            let outcome = match aborted.outcome {
                AbortedToolCallOutcome::Settled(result) => {
                    if result.call_id != call.id {
                        return Err(AgentError::new(
                            AgentErrorKind::InvalidToolBatch,
                            "aborted_tool_result_id_mismatch",
                        ));
                    }
                    settled_count += 1;
                    Some(InterruptedToolCallOutcome::Settled(result))
                }
                AbortedToolCallOutcome::CancelledBeforeStart => {
                    cancelled_count += 1;
                    None
                }
                AbortedToolCallOutcome::OutcomeUnknown { summary } => {
                    unknown_count += 1;
                    Some(InterruptedToolCallOutcome::OutcomeUnknown { summary })
                }
            };
            if let Some(outcome) = outcome {
                receipts.push(InterruptedToolCallReceipt {
                    slot: aborted.slot,
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                    outcome,
                });
            }
        }

        self.session.emit_tool_batch_aborted(
            &self.run_id,
            self.start.batch_attempt_id.clone(),
            settled_count,
            cancelled_count,
            unknown_count,
        );
        if receipts.is_empty() {
            Ok(None)
        } else {
            Ok(Some(InterruptedToolBatchReceipt {
                batch_attempt_id: self.start.batch_attempt_id.clone(),
                calls: receipts,
            }))
        }
    }
}

impl ModelStreamSink for ModelAttemptSink<'_> {
    fn emit<'b>(&'b mut self, event: ModelStreamEvent) -> PortFuture<'b, Result<(), AgentError>> {
        Box::pin(async move {
            if let Some(error) = self.first_failure.clone() {
                return Err(error);
            }
            if self.closed {
                return Err(AgentError::new(
                    AgentErrorKind::InvalidState,
                    "model_stream_event_after_attempt_closed",
                ));
            }

            self.session
                .emit_stream_delta(&self.run_id, self.request_index, &event);
            let result = match event {
                ModelStreamEvent::TextDelta { .. } => Ok(()),
                ModelStreamEvent::ToolCallReady { slot, call } => {
                    self.accept_ready(slot, call).await
                }
                ModelStreamEvent::ToolCallsSealed { call_count } => {
                    self.accept_seal(call_count).await
                }
            };
            if let Err(error) = &result {
                self.first_failure = Some(error.clone());
            }
            result
        })
    }
}

struct PendingIncrementalBatch<'a> {
    batch_attempt_id: ToolBatchAttemptId,
    receiver: Box<dyn IncrementalToolBatch + 'a>,
}

/// 持有持久化会话状态，并确保每个会话同时只有一个活跃驱动器。
pub struct AgentSession {
    prompt: Arc<dyn PromptSource>,
    tools: Arc<dyn ToolRuntime>,
    compaction: Arc<dyn Compaction>,
    model: Arc<dyn Model>,
    observer: Arc<dyn Observer>,
    stream_observer: Arc<dyn StreamObserver>,
    config: SessionConfig,
    event_sequence: AtomicU64,
    state: Mutex<SessionState>,
    run_ended: Notify,
}

impl AgentSession {
    pub fn new(
        prompt: Arc<dyn PromptSource>,
        tools: Arc<dyn ToolRuntime>,
        compaction: Arc<dyn Compaction>,
        model: Arc<dyn Model>,
        config: SessionConfig,
    ) -> Self {
        Self {
            prompt,
            tools,
            compaction,
            model,
            observer: Arc::new(NoopObserver),
            stream_observer: Arc::new(NoopStreamObserver),
            config,
            event_sequence: AtomicU64::new(0),
            state: Mutex::new(SessionState::default()),
            run_ended: Notify::new(),
        }
    }

    /// 在将会话包装进 `Arc` 前安装结构化事件接收器。
    pub fn with_observer(mut self, observer: Arc<dyn Observer>) -> Self {
        self.observer = observer;
        self
    }

    /// 安装可能接收正文和完整工具参数的高频流观察器。
    pub fn with_stream_observer(mut self, observer: Arc<dyn StreamObserver>) -> Self {
        self.stream_observer = observer;
        self
    }

    /// 将任意规范化对话记录项加入活跃运行的队列，不预设角色。
    pub async fn enqueue_if_running(&self, input: MailboxInput) -> Result<(), MailboxRejected> {
        let (sequence, run_id, delivery, item_count) = {
            let mut state = self.state.lock().await;
            if state.stopping {
                return Err(MailboxRejected {
                    reason: MailboxRejectedReason::Stopping,
                    input,
                });
            }
            let Some(active) = &state.active else {
                return Err(MailboxRejected {
                    reason: MailboxRejectedReason::Idle,
                    input,
                });
            };
            if active.phase != RunPhase::Open {
                return Err(MailboxRejected {
                    reason: MailboxRejectedReason::Closing,
                    input,
                });
            }
            if validate_closed_transcript(&input.items).is_err() {
                return Err(MailboxRejected {
                    reason: MailboxRejectedReason::InvalidInput,
                    input,
                });
            }

            let event = (
                self.next_event_sequence(),
                active.id.clone(),
                input.delivery,
                input.items.len(),
            );
            state.mailbox.push(input);
            event
        };
        self.emit_lazy(EventKind::MailboxEnqueued.metadata(), || {
            AgentEvent::MailboxEnqueued {
                sequence,
                run_id,
                delivery,
                item_count,
            }
        });
        Ok(())
    }

    /// 仅在空闲时启动运行。初始项在下次模型请求时投递。
    pub async fn start_if_idle(
        self: &Arc<Self>,
        initial_items: Vec<TranscriptItem>,
    ) -> Result<TurnOutcome, StartRejected> {
        let (sequence, run_id, prior_items) = {
            let mut state = self.state.lock().await;
            if state.stopping {
                return Err(StartRejected {
                    reason: StartRejectedReason::Stopping,
                    initial_items,
                });
            }
            if state.active.is_some() {
                return Err(StartRejected {
                    reason: StartRejectedReason::Busy,
                    initial_items,
                });
            }
            if validate_closed_transcript(&initial_items).is_err() {
                return Err(StartRejected {
                    reason: StartRejectedReason::InvalidInput,
                    initial_items,
                });
            }

            state.run_seq = state.run_seq.saturating_add(1);
            let run_id = RunId::new(format!("run-{}", state.run_seq));
            let prior_items = state.conversation.len();
            state.active = Some(ActiveRun {
                id: run_id.clone(),
                phase: RunPhase::Open,
            });
            state
                .mailbox
                .push(MailboxInput::next_model_request(initial_items));
            (self.next_event_sequence(), run_id, prior_items)
        };

        self.emit_lazy(EventKind::RunStarted.metadata(), || {
            AgentEvent::RunStarted {
                sequence,
                run_id: run_id.clone(),
                prior_transcript_items: prior_items,
            }
        });

        // 驱动任务由 session 自己持有；等待方被取消时任务仍会继续，显式 `stop` 才是停止协议。
        let driver = Arc::clone(self);
        let driver_run_id = run_id.clone();
        match tokio::spawn(async move { driver.drive_run(driver_run_id).await }).await {
            Ok(outcome) => Ok(outcome),
            Err(join_error) if join_error.is_panic() => {
                // 端口 panic 表示实现缺陷，不能降级成普通的运行失败。先释放本轮占用并归还
                // 尚未投递的信箱内容，再继续展开原始 panic，避免 session 永久停在 Busy。
                let panic = join_error.into_panic();
                let _ = self.recover_crashed_driver(&run_id).await;
                std::panic::resume_unwind(panic);
            }
            Err(join_error) => {
                let error = AgentError::new(
                    crate::types::AgentErrorKind::InvalidState,
                    format!("run_driver_task_failed: {join_error}"),
                );
                let (undelivered, sequence) = self.recover_crashed_driver(&run_id).await;
                self.emit_lazy(EventKind::RunFailed.metadata(), || AgentEvent::RunFailed {
                    sequence,
                    run_id,
                    stage: RunStage::Boundary,
                    error_kind: error.kind,
                });
                Ok(TurnOutcome::Failed { error, undelivered })
            }
        }
    }

    /// 永久停止会话。允许进行中的模型或工具工作自然结束；驱动器会在下一个请求边界退出。
    /// 此方法是幂等的。
    pub async fn stop(&self) {
        loop {
            let notified = self.run_ended.notified();
            let idle = {
                let mut state = self.state.lock().await;
                state.stopping = true;
                if let Some(active) = &mut state.active {
                    active.phase = RunPhase::Finalizing;
                    false
                } else {
                    true
                }
            };
            if idle {
                return;
            }
            notified.await;
        }
    }

    pub async fn conversation(&self) -> Vec<TranscriptItem> {
        self.state.lock().await.conversation.clone()
    }

    async fn drive_run(&self, run_id: RunId) -> TurnOutcome {
        let mut prefix = self.prompt.base_context();
        prefix.extend(self.prompt.run_context());
        let protected_prefix_len = prefix.len();
        let prior_conversation = self.state.lock().await.conversation.clone();
        prefix.extend(prior_conversation);

        let mut turn = Turn::new(run_id.clone(), prefix);
        let core_outcome = self.drive_steps(&run_id, &mut turn).await;

        // 在压缩前关闭本轮运行，使迟到的生产者收到 `Closing`，而不是成功写入已无法投递的输入。
        let undelivered = {
            let mut state = self.state.lock().await;
            if let Some(active) = &mut state.active {
                if active.id == run_id {
                    active.phase = RunPhase::Finalizing;
                }
            }
            state.mailbox.drain_all()
        };

        // 只持久化协议上已经闭合的前缀。工具批尚未完整返回时，模型产生的调用仍属于
        // 运行中的暂存状态，不能进入下一轮的普通会话历史。
        let conversation = turn.committable_transcript()[protected_prefix_len..].to_vec();
        let conversation = self.maybe_compact(&run_id, conversation).await;
        let terminal_sequence = {
            let mut state = self.state.lock().await;
            let owns_generation = state
                .active
                .as_ref()
                .is_some_and(|active| active.id == run_id);
            if owns_generation {
                state.conversation = conversation;
                state.active = None;
            }
            self.next_event_sequence()
        };
        self.run_ended.notify_waiters();

        match core_outcome {
            CoreOutcome::Completed {
                output,
                usage,
                stats,
            } => {
                let sequence = terminal_sequence;
                self.emit_lazy(EventKind::RunCompleted.metadata(), || {
                    AgentEvent::RunCompleted {
                        sequence,
                        run_id,
                        model_requests: stats.model_requests,
                        tool_batches: stats.tool_batches,
                        interrupted_tool_recoveries: stats.interrupted_tool_recoveries,
                        usage: usage.clone(),
                    }
                });
                debug_assert!(undelivered.is_empty());
                TurnOutcome::Completed { output, usage }
            }
            CoreOutcome::Stopped => {
                let sequence = terminal_sequence;
                self.emit_lazy(EventKind::RunStopped.metadata(), || {
                    AgentEvent::RunStopped {
                        sequence,
                        run_id: run_id.clone(),
                    }
                });
                TurnOutcome::Stopped { undelivered }
            }
            CoreOutcome::Failed { error, stage } => {
                let sequence = terminal_sequence;
                self.emit_lazy(EventKind::RunFailed.metadata(), || AgentEvent::RunFailed {
                    sequence,
                    run_id,
                    stage,
                    error_kind: error.kind,
                });
                TurnOutcome::Failed { error, undelivered }
            }
        }
    }

    async fn drive_steps(&self, run_id: &RunId, turn: &mut Turn) -> CoreOutcome {
        let mut stats = RunStats::default();
        let mut pending_incremental: Option<PendingIncrementalBatch<'_>> = None;
        loop {
            let step = match turn.next_step() {
                Ok(step) => step,
                Err(error) => {
                    return CoreOutcome::Failed {
                        error,
                        stage: RunStage::Boundary,
                    };
                }
            };

            match step {
                TurnStep::RequestBoundary { kind } => {
                    let (items, sealed, drained_sequence, sealed_sequence) = {
                        let mut state = self.state.lock().await;
                        if state.stopping {
                            if let Some(active) = &mut state.active {
                                if active.id == *run_id {
                                    active.phase = RunPhase::Finalizing;
                                }
                            }
                            return CoreOutcome::Stopped;
                        }

                        let items = state.mailbox.drain(kind);
                        let sealed =
                            kind == RequestBoundaryKind::BeforeCompletion && items.is_empty();
                        if sealed {
                            if let Some(active) = &mut state.active {
                                if active.id == *run_id {
                                    // 空队列检查和封闭操作与生产者共用这把锁。
                                    active.phase = RunPhase::Sealed;
                                }
                            }
                        }
                        let drained_sequence = self.next_event_sequence();
                        let sealed_sequence = sealed.then(|| self.next_event_sequence());
                        (items, sealed, drained_sequence, sealed_sequence)
                    };

                    self.emit_lazy(EventKind::BoundaryDrained.metadata(), || {
                        AgentEvent::BoundaryDrained {
                            sequence: drained_sequence,
                            run_id: run_id.clone(),
                            kind,
                            item_count: items.len(),
                        }
                    });
                    if sealed {
                        let sequence = sealed_sequence.expect("sealed event sequence");
                        self.emit_lazy(EventKind::CompletionSealed.metadata(), || {
                            AgentEvent::CompletionSealed {
                                sequence,
                                run_id: run_id.clone(),
                            }
                        });
                    }
                    if let Err(error) = turn.resume_boundary(items) {
                        return CoreOutcome::Failed {
                            error,
                            stage: RunStage::Boundary,
                        };
                    }
                }
                TurnStep::CallModel { transcript } => {
                    stats.model_requests = stats.model_requests.saturating_add(1);
                    let request_index = stats.model_requests;
                    let definitions = self.tools.definitions();
                    let sequence = self.next_event_sequence();
                    self.emit_lazy(EventKind::ModelRequestStarted.metadata(), || {
                        AgentEvent::ModelRequestStarted {
                            sequence,
                            run_id: run_id.clone(),
                            request_index,
                            transcript_items: transcript.len(),
                            function_tools: definitions.len(),
                        }
                    });
                    let started_at = Instant::now();
                    let mut sink = ModelAttemptSink::new(self, run_id.clone(), request_index);
                    let response = self
                        .model
                        .complete_stream(
                            ModelRequest {
                                transcript,
                                function_tools: definitions,
                            },
                            &mut sink,
                        )
                        .await;
                    // adapter 已经归还 complete_stream 的独占借用；从这里起明确禁止任何
                    // 新的 CallReady，再等待 runtime 给出冻结的 abort 报告。
                    sink.close();
                    // 即使第三方 adapter 错误地吞掉 sink 返回的 Err 并最终返回 Ok，首次
                    // sink 失败仍是粘滞的，本次 attempt 绝不能进入 reconcile 或 commit。
                    let response = match response {
                        Ok(response) => match sink.first_failure().cloned() {
                            Some(error) => Err(error),
                            None => Ok(response),
                        },
                        Err(error) => Err(sink.first_failure().cloned().unwrap_or(error)),
                    };
                    let response = match response {
                        Ok(response) => response,
                        Err(error) => {
                            let reason = if sink.runtime_rejected() {
                                ToolBatchAbortReason::ToolRuntimeRejected
                            } else {
                                ToolBatchAbortReason::ModelStreamInterrupted
                            };
                            match self
                                .resolve_failed_model_attempt(
                                    run_id, turn, &mut sink, error, reason, &mut stats,
                                )
                                .await
                            {
                                Ok(()) => continue,
                                Err(outcome) => return outcome,
                            }
                        }
                    };

                    if let Err(error) = sink.reconcile_success(&response).await {
                        let reason = if sink.runtime_rejected() {
                            ToolBatchAbortReason::ToolRuntimeRejected
                        } else {
                            ToolBatchAbortReason::ModelOutputMismatch
                        };
                        match self
                            .resolve_failed_model_attempt(
                                run_id, turn, &mut sink, error, reason, &mut stats,
                            )
                            .await
                        {
                            Ok(()) => continue,
                            Err(outcome) => return outcome,
                        }
                    }

                    let batch_attempt_id = sink.batch_attempt_id().clone();
                    let sequence = self.next_event_sequence();
                    self.emit_lazy(EventKind::ModelRequestFinished.metadata(), || {
                        AgentEvent::ModelRequestFinished {
                            sequence,
                            run_id: run_id.clone(),
                            request_index,
                            local_tool_calls: response.output.tool_calls.len(),
                            duration_ms: elapsed_ms(started_at),
                            usage: response.usage.clone(),
                        }
                    });

                    if let Err(error) =
                        turn.model_response_for_attempt(response, batch_attempt_id.clone())
                    {
                        // 走到这里表示无 I/O 状态机与已经校验过的规范响应不一致，是严格的
                        // 内核错误。尽力冻结 runtime，但不伪造恢复回执。
                        let _ = sink.abort(ToolBatchAbortReason::ModelOutputMismatch).await;
                        self.emit_stream_terminal(
                            run_id,
                            request_index,
                            ModelStreamObservation::AttemptAborted {
                                error_kind: error.kind,
                            },
                        );
                        return CoreOutcome::Failed {
                            error,
                            stage: RunStage::Model,
                        };
                    }

                    if let Some(receiver) = sink.take_incremental() {
                        debug_assert!(pending_incremental.is_none());
                        pending_incremental = Some(PendingIncrementalBatch {
                            batch_attempt_id,
                            receiver,
                        });
                    }
                    self.emit_stream_terminal(
                        run_id,
                        request_index,
                        ModelStreamObservation::AttemptCommitted,
                    );
                }
                TurnStep::DispatchTools { batch } => {
                    stats.tool_batches = stats.tool_batches.saturating_add(1);
                    let sequence = self.next_event_sequence();
                    self.emit_lazy(EventKind::ToolBatchStarted.metadata(), || {
                        let calls = batch
                            .calls
                            .iter()
                            .map(|call| ToolCallSummary {
                                id: call.id.clone(),
                                name: call.name.clone(),
                            })
                            .collect();
                        AgentEvent::ToolBatchStarted {
                            sequence,
                            run_id: run_id.clone(),
                            batch_id: batch.batch_id.clone(),
                            calls,
                        }
                    });
                    let batch_id = batch.batch_id.clone();
                    let started_at = Instant::now();
                    let result = if let Some(pending) = pending_incremental.take() {
                        if pending.batch_attempt_id.as_str() != batch.batch_id.as_str() {
                            Err(AgentError::new(
                                AgentErrorKind::InvalidState,
                                "incremental_tool_batch_id_mismatch",
                            ))
                        } else {
                            // commit 的顶层错误表示运行时无法生成可信的完整结果批。模型流已经
                            // 成功，不能再把它伪装成可恢复的中断回执，因此严格终止本次运行。
                            pending.receiver.commit().await
                        }
                    } else {
                        // 不支持增量接管的 runtime 仍然只收到一次完整数组。
                        self.tools.dispatch(batch).await
                    };
                    let results = match result {
                        Ok(results) => results,
                        Err(error) => {
                            return CoreOutcome::Failed {
                                error,
                                stage: RunStage::Tools,
                            };
                        }
                    };
                    let error_count = results
                        .results
                        .iter()
                        .filter(|result| result.status == ToolResultStatus::Error)
                        .count();
                    let sequence = self.next_event_sequence();
                    self.emit_lazy(EventKind::ToolBatchFinished.metadata(), || {
                        AgentEvent::ToolBatchFinished {
                            sequence,
                            run_id: run_id.clone(),
                            batch_id,
                            result_count: results.results.len(),
                            error_count,
                            duration_ms: elapsed_ms(started_at),
                        }
                    });
                    if let Err(error) = turn.tool_results(results) {
                        return CoreOutcome::Failed {
                            error,
                            stage: RunStage::Tools,
                        };
                    }
                }
                TurnStep::Done { output, usage } => {
                    debug_assert!(pending_incremental.is_none());
                    return CoreOutcome::Completed {
                        output,
                        usage,
                        stats,
                    };
                }
            }
        }
    }

    async fn resolve_failed_model_attempt(
        &self,
        run_id: &RunId,
        turn: &mut Turn,
        sink: &mut ModelAttemptSink<'_>,
        error: AgentError,
        reason: ToolBatchAbortReason,
        stats: &mut RunStats,
    ) -> Result<(), CoreOutcome> {
        sink.close();
        self.emit_stream_terminal(
            run_id,
            sink.request_index,
            ModelStreamObservation::AttemptAborted {
                error_kind: error.kind,
            },
        );

        let receipt = match sink.abort(reason).await {
            Ok(receipt) => receipt,
            Err(abort_error) => {
                return Err(CoreOutcome::Failed {
                    error: abort_error,
                    stage: RunStage::Tools,
                });
            }
        };
        let Some(receipt) = receipt else {
            let stage = if reason == ToolBatchAbortReason::ToolRuntimeRejected {
                RunStage::Tools
            } else {
                RunStage::Model
            };
            return Err(CoreOutcome::Failed { error, stage });
        };

        let message = match self.interrupted_tool_receipt_message(&receipt) {
            Ok(message) => message,
            Err(message_error) => {
                return Err(CoreOutcome::Failed {
                    error: message_error,
                    stage: RunStage::Boundary,
                });
            }
        };
        if let Err(boundary_error) = turn.recover_model_attempt(message) {
            return Err(CoreOutcome::Failed {
                error: boundary_error,
                stage: RunStage::Boundary,
            });
        }

        if reason == ToolBatchAbortReason::ToolRuntimeRejected {
            return Err(CoreOutcome::Failed {
                error,
                stage: RunStage::Tools,
            });
        }
        if stats.interrupted_tool_recoveries
            >= u64::from(self.config.max_interrupted_tool_recoveries)
        {
            return Err(CoreOutcome::Failed {
                error: AgentError::new(
                    AgentErrorKind::Model,
                    "interrupted_tool_recovery_limit_exceeded",
                ),
                stage: RunStage::Model,
            });
        }

        stats.interrupted_tool_recoveries = stats.interrupted_tool_recoveries.saturating_add(1);
        Ok(())
    }

    fn interrupted_tool_receipt_message(
        &self,
        receipt: &InterruptedToolBatchReceipt,
    ) -> Result<InputMessage, AgentError> {
        // `ToolResult::metadata` 属于应用内部关联数据，正常协议编码也不会把它交给模型。
        // 恢复回执只携带模型本来就能看到的 status/content，避免意外暴露追踪或凭据字段。
        let mut visible_calls = receipt.calls.clone();
        for call in &mut visible_calls {
            if let InterruptedToolCallOutcome::Settled(result) = &mut call.outcome {
                result.metadata.clear();
            }
        }
        let calls = serde_json::to_value(&visible_calls).map_err(|error| {
            AgentError::new(
                AgentErrorKind::InvalidState,
                format!("serialize_interrupted_tool_receipt_failed:{error}"),
            )
        })?;
        let value = serde_json::json!({
            "kind": RECOVERY_RECEIPT_KIND,
            "aborted": true,
            "batch_attempt_id": receipt.batch_attempt_id.as_str(),
            "calls": calls,
            "unconfirmed_remainder": "discarded"
        });
        let mut message = InputMessage::new(
            self.config.interrupted_tool_receipt_role.clone(),
            vec![ContentPart::json(value)],
        );
        // 该标记属于内核持久化元数据，不依赖 role，也不应由协议 adapter 改写。
        message.provider_data.insert(
            RECOVERY_RECEIPT_MARKER_KEY.to_owned(),
            serde_json::json!({
                "version": 1,
                "batch_attempt_id": receipt.batch_attempt_id.as_str()
            }),
        );
        Ok(message)
    }

    fn emit_stream_delta(&self, run_id: &RunId, request_index: u64, event: &ModelStreamEvent) {
        self.emit_stream_observation(run_id, request_index, || {
            ModelStreamObservation::Delta(event.clone())
        });
    }

    fn emit_stream_terminal(
        &self,
        run_id: &RunId,
        request_index: u64,
        payload: ModelStreamObservation,
    ) {
        debug_assert!(matches!(
            &payload,
            ModelStreamObservation::AttemptCommitted
                | ModelStreamObservation::AttemptAborted { .. }
        ));
        self.emit_stream_observation(run_id, request_index, || payload);
    }

    fn emit_stream_observation(
        &self,
        run_id: &RunId,
        request_index: u64,
        build: impl FnOnce() -> ModelStreamObservation,
    ) {
        // 高频观察端完全独立于工具控制路径；过滤或 panic 不能丢失 CallReady/Seal。
        let enabled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.stream_observer.enabled()
        }))
        .unwrap_or(false);
        if !enabled {
            return;
        }
        let event = ObservedModelStreamEvent {
            sequence: self.next_event_sequence(),
            run_id: run_id.clone(),
            request_index,
            payload: build(),
        };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.stream_observer.observe(&event);
        }));
    }

    fn emit_tool_calls_sealed(
        &self,
        run_id: &RunId,
        batch_attempt_id: ToolBatchAttemptId,
        call_count: u32,
    ) {
        let sequence = self.next_event_sequence();
        self.emit_lazy(EventKind::ToolCallsSealed.metadata(), || {
            AgentEvent::ToolCallsSealed {
                sequence,
                run_id: run_id.clone(),
                batch_attempt_id,
                call_count,
            }
        });
    }

    fn emit_tool_batch_aborted(
        &self,
        run_id: &RunId,
        batch_attempt_id: ToolBatchAttemptId,
        settled_count: usize,
        cancelled_count: usize,
        unknown_count: usize,
    ) {
        let sequence = self.next_event_sequence();
        self.emit_lazy(EventKind::ToolBatchAborted.metadata(), || {
            AgentEvent::ToolBatchAborted {
                sequence,
                run_id: run_id.clone(),
                batch_attempt_id,
                settled_count,
                cancelled_count,
                unknown_count,
            }
        });
    }

    async fn maybe_compact(
        &self,
        run_id: &RunId,
        conversation: Vec<TranscriptItem>,
    ) -> Vec<TranscriptItem> {
        let estimated_bytes = serde_json::to_vec(&conversation)
            .map(|encoded| encoded.len())
            .unwrap_or(usize::MAX);
        if estimated_bytes <= self.config.compaction_trigger_bytes {
            return conversation;
        }

        let sequence = self.next_event_sequence();
        self.emit_lazy(EventKind::CompactionStarted.metadata(), || {
            AgentEvent::CompactionStarted {
                sequence,
                run_id: run_id.clone(),
                transcript_items: conversation.len(),
                estimated_bytes,
            }
        });
        let started_at = Instant::now();
        let compacted = self.compaction.compact(&conversation).await;
        // 压缩是受信端口，但其输出仍不能破坏核心的工具轮闭合不变量。拒绝非法结果时保留
        // 原始对话，避免一次可选优化污染下一次模型请求。
        let compacted = if validate_closed_transcript(&compacted).is_ok()
            && preserves_recovery_receipts(&conversation, &compacted)
        {
            compacted
        } else {
            conversation
        };
        let sequence = self.next_event_sequence();
        self.emit_lazy(EventKind::CompactionFinished.metadata(), || {
            AgentEvent::CompactionFinished {
                sequence,
                run_id: run_id.clone(),
                transcript_items: compacted.len(),
                duration_ms: elapsed_ms(started_at),
            }
        });
        compacted
    }

    async fn recover_crashed_driver(&self, run_id: &RunId) -> (Vec<MailboxInput>, u64) {
        let (undelivered, sequence) = {
            let mut state = self.state.lock().await;
            let owns_generation = state
                .active
                .as_ref()
                .is_some_and(|active| active.id == *run_id);
            let undelivered = if owns_generation {
                state.active = None;
                state.mailbox.drain_all()
            } else {
                Vec::new()
            };
            (undelivered, self.next_event_sequence())
        };
        self.run_ended.notify_waiters();
        (undelivered, sequence)
    }

    fn emit_lazy(&self, metadata: EventMetadata, build: impl FnOnce() -> AgentEvent) {
        // 观测器的过滤和写入故障都不能破坏状态机或把 session 卡在 Busy。
        let enabled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.observer.enabled(metadata)
        }))
        .unwrap_or(false);
        if !enabled {
            return;
        }

        let event = build();
        debug_assert_eq!(event.metadata(), metadata);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.observer.observe(&event);
        }));
    }

    fn next_event_sequence(&self) -> u64 {
        self.event_sequence.fetch_add(1, Ordering::Relaxed) + 1
    }
}

fn is_recovery_receipt(item: &TranscriptItem) -> bool {
    matches!(
        item,
        TranscriptItem::Input(message)
            if message.provider_data.contains_key(RECOVERY_RECEIPT_MARKER_KEY)
    )
}

/// 压缩可以改写普通历史，但不能静默遗忘已经发生的工具副作用。所有带内核标记的恢复
/// 回执必须按原顺序、原内容出现在压缩结果中。
fn preserves_recovery_receipts(original: &[TranscriptItem], compacted: &[TranscriptItem]) -> bool {
    original
        .iter()
        .filter(|item| is_recovery_receipt(item))
        .eq(compacted.iter().filter(|item| is_recovery_receipt(item)))
}

fn elapsed_ms(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[derive(Default)]
struct RunStats {
    model_requests: u64,
    tool_batches: u64,
    interrupted_tool_recoveries: u64,
}

enum CoreOutcome {
    Completed {
        output: ModelOutput,
        usage: Option<ModelUsage>,
        stats: RunStats,
    },
    Stopped,
    Failed {
        error: AgentError,
        stage: RunStage,
    },
}

#[cfg(test)]
mod tests;
