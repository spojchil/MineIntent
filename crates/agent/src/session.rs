//! 围绕无 I/O `Turn` 的并发会话驱动器。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{Mutex, Notify};

use crate::events::{
    AgentEvent, EventKind, EventMetadata, NoopObserver, Observer, RunStage, ToolCallSummary,
};
use crate::mailbox::{Mailbox, MailboxInput, MailboxRejected, MailboxRejectedReason};
use crate::ports::{Compaction, Model, ModelRequest, PromptSource, ToolRuntime};
use crate::run::{RequestBoundaryKind, Turn, TurnStep};
use crate::types::{
    validate_closed_transcript, AgentError, ModelOutput, ModelUsage, RunId, ToolResultStatus,
    TranscriptItem,
};

#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// 当持久化对话记录的序列化大小超过此值时，在运行结束后压缩它。
    pub compaction_trigger_bytes: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            compaction_trigger_bytes: 256 * 1024,
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

/// 持有持久化会话状态，并确保每个会话同时只有一个活跃驱动器。
pub struct AgentSession {
    prompt: Arc<dyn PromptSource>,
    tools: Arc<dyn ToolRuntime>,
    compaction: Arc<dyn Compaction>,
    model: Arc<dyn Model>,
    observer: Arc<dyn Observer>,
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
                    let definitions = self.tools.definitions();
                    let sequence = self.next_event_sequence();
                    self.emit_lazy(EventKind::ModelRequestStarted.metadata(), || {
                        AgentEvent::ModelRequestStarted {
                            sequence,
                            run_id: run_id.clone(),
                            request_index: stats.model_requests,
                            transcript_items: transcript.len(),
                            function_tools: definitions.len(),
                        }
                    });
                    let started_at = Instant::now();
                    let response = match self
                        .model
                        .complete(ModelRequest {
                            transcript,
                            function_tools: definitions,
                        })
                        .await
                    {
                        Ok(response) => response,
                        Err(error) => {
                            return CoreOutcome::Failed {
                                error,
                                stage: RunStage::Model,
                            };
                        }
                    };
                    let sequence = self.next_event_sequence();
                    self.emit_lazy(EventKind::ModelRequestFinished.metadata(), || {
                        AgentEvent::ModelRequestFinished {
                            sequence,
                            run_id: run_id.clone(),
                            request_index: stats.model_requests,
                            local_tool_calls: response.output.tool_calls.len(),
                            duration_ms: elapsed_ms(started_at),
                            usage: response.usage.clone(),
                        }
                    });
                    if let Err(error) = turn.model_response(response) {
                        return CoreOutcome::Failed {
                            error,
                            stage: RunStage::Model,
                        };
                    }
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
                    // 对模型生成的整个批次只调用一次 dispatch。
                    let started_at = Instant::now();
                    let results = match self.tools.dispatch(batch).await {
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
                    return CoreOutcome::Completed {
                        output,
                        usage,
                        stats,
                    };
                }
            }
        }
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
        let compacted = if validate_closed_transcript(&compacted).is_ok() {
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

fn elapsed_ms(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[derive(Default)]
struct RunStats {
    model_requests: u64,
    tool_batches: u64,
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
