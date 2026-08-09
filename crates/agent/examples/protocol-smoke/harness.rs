//! 三种协议共享的 agent 场景、工具运行时和观测输出。

use std::collections::BTreeMap;
use std::error::Error;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use agent::adapters::{
    anthropic::messages::RequestOptions as AnthropicRequestOptions,
    http::{HttpModel, HttpModelConfig, Protocol},
    openai::{
        chat::RequestOptions as OpenAiChatRequestOptions,
        responses::RequestOptions as OpenAiResponsesRequestOptions,
    },
};
use agent::{
    AbortedToolBatch, AbortedToolCall, AbortedToolCallOutcome, AgentError, AgentErrorKind,
    AgentEvent, AgentSession, Compaction, FilteredObserver, IncrementalToolBatch,
    IncrementalToolCall, InputMessage, MailboxInput, Observer, PortFuture, PromptSource,
    SessionConfig, ToolBatchAbortReason, ToolBatchStart, ToolCallBatch, ToolCallId, ToolCallSlot,
    ToolDefinition, ToolResult, ToolResultBatch, ToolRuntime, TranscriptItem, TurnOutcome,
};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Semaphore};
use tokio::time::timeout;

use crate::config::{SmokeConfig, SmokeProtocol};

type SmokeError = Box<dyn Error + Send + Sync>;

pub(crate) async fn run_smoke(
    protocol: SmokeProtocol,
    config: &SmokeConfig,
) -> Result<(), SmokeError> {
    println!("\n== {} ==", protocol.label());
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let gate = Arc::new(Semaphore::new(0));
    let observer = Arc::new(FilteredObserver::new(
        Arc::new(PrintingObserver { protocol }),
        config.level,
    ));
    let model = HttpModel::new(
        HttpModelConfig::new(
            config.endpoint(protocol),
            config.api_key.clone(),
            config.model.clone(),
            wire_protocol(protocol),
        )
        .with_timeout(config.timeout)
        .with_wire_log(config.wire_log),
    )?;
    let tools = Arc::new(SmokeTools {
        started: started_tx,
        gate: gate.clone(),
        incremental_begins: AtomicUsize::new(0),
        incremental_seals: AtomicUsize::new(0),
        incremental_commits: AtomicUsize::new(0),
        legacy_dispatches: AtomicUsize::new(0),
    });
    let session = Arc::new(
        AgentSession::new(
            Arc::new(SmokePrompt),
            tools.clone(),
            Arc::new(NoCompaction),
            Arc::new(model),
            SessionConfig::default(),
        )
        .with_observer(observer),
    );

    let running = session.clone();
    let mut task = tokio::spawn(async move {
        running
            .start_if_idle(vec![InputMessage::text(
                "user",
                "请在同一条回复中调用 add 两次，分别计算 2+3 与 5+7；现在不要自己计算。",
            )
            .into()])
            .await
    });

    let call_count = match timeout(config.timeout, started_rx.recv()).await {
        Ok(Some(count)) => count,
        Ok(None) => {
            task.abort();
            return Err(io::Error::other("工具 runtime 通知通道意外关闭").into());
        }
        Err(_) => {
            task.abort();
            return Err(io::Error::other(format!("等待 {} 工具调用超时", protocol.label())).into());
        }
    };
    if tools.incremental_seals.load(Ordering::SeqCst) != 1
        || tools.legacy_dispatches.load(Ordering::SeqCst) != 0
    {
        task.abort();
        return Err(io::Error::other("冒烟测试没有走增量工具 runtime 路径").into());
    }
    println!("增量 runtime 已接收并封口，调用数：{call_count}");

    session
        .enqueue_if_running(MailboxInput::next_model_request(vec![InputMessage::text(
            "user",
            "这是工具执行期间插入的消息：结果齐全后只简短报告两个和。",
        )
        .into()]))
        .await?;
    gate.add_permits(1);

    let outcome = match timeout(config.timeout, &mut task).await {
        Ok(result) => result??,
        Err(_) => {
            task.abort();
            return Err(
                io::Error::other(format!("等待 {} agent 完成超时", protocol.label())).into(),
            );
        }
    };
    if call_count != 2 {
        return Err(io::Error::other(format!(
            "期望一个含两个调用的批次，实际调用数：{call_count}"
        ))
        .into());
    }
    if tools.incremental_begins.load(Ordering::SeqCst) != 1
        || tools.incremental_commits.load(Ordering::SeqCst) != 1
        || tools.legacy_dispatches.load(Ordering::SeqCst) != 0
    {
        return Err(io::Error::other("增量工具 runtime 的 begin/commit 路径计数异常").into());
    }

    match outcome {
        TurnOutcome::Completed { output, usage } => {
            println!("最终文本：{}", output.text_content());
            println!("usage：{usage:?}");
            Ok(())
        }
        other => Err(io::Error::other(format!("冒烟测试未正常完成：{other:?}")).into()),
    }
}

/// 显式保留旧冒烟适配器的请求参数，避免公共适配器的通用默认值改变测试强度。
fn wire_protocol(protocol: SmokeProtocol) -> Protocol {
    match protocol {
        SmokeProtocol::OpenAiChat => Protocol::openai_chat_with(
            OpenAiChatRequestOptions::new()
                .with_max_tokens(256)
                .with_tool_choice("auto"),
        ),
        SmokeProtocol::OpenAiResponses => Protocol::openai_responses_with(
            OpenAiResponsesRequestOptions::new()
                .with_max_output_tokens(256)
                .with_tool_choice("auto"),
        ),
        SmokeProtocol::AnthropicMessages => Protocol::anthropic_messages_with(
            AnthropicRequestOptions::new(256).with_tool_choice(json!({"type": "auto"})),
        ),
    }
}

struct SmokePrompt;

impl PromptSource for SmokePrompt {
    fn base_context(&self) -> Vec<TranscriptItem> {
        vec![InputMessage::text(
            "system",
            "你在执行 agent 协议冒烟测试。必须严格按用户要求调用工具。",
        )
        .into()]
    }

    fn run_context(&self) -> Vec<TranscriptItem> {
        Vec::new()
    }
}

struct SmokeTools {
    started: mpsc::UnboundedSender<usize>,
    gate: Arc<Semaphore>,
    incremental_begins: AtomicUsize,
    incremental_seals: AtomicUsize,
    incremental_commits: AtomicUsize,
    legacy_dispatches: AtomicUsize,
}

impl ToolRuntime for SmokeTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definition = ToolDefinition::new(
            "add",
            json!({
                "type": "object",
                "properties": {
                    "left": {"type": "integer"},
                    "right": {"type": "integer"}
                },
                "required": ["left", "right"],
                "additionalProperties": false
            }),
        );
        definition.description = Some("返回两个整数的和。".to_owned());
        vec![definition]
    }

    fn dispatch<'a>(
        &'a self,
        batch: ToolCallBatch,
    ) -> PortFuture<'a, Result<ToolResultBatch, AgentError>> {
        Box::pin(async move {
            self.legacy_dispatches.fetch_add(1, Ordering::SeqCst);
            self.started.send(batch.calls.len()).map_err(|_| {
                AgentError::new(AgentErrorKind::ToolDispatch, "smoke_receiver_closed")
            })?;
            let _permit =
                self.gate.acquire().await.map_err(|_| {
                    AgentError::new(AgentErrorKind::ToolDispatch, "smoke_gate_closed")
                })?;

            let results = batch
                .calls
                .iter()
                .map(|call| add_result(call.id.clone(), &call.arguments))
                .collect();
            Ok(ToolResultBatch { results })
        })
    }

    fn begin_incremental<'a>(
        &'a self,
        batch: ToolBatchStart,
    ) -> PortFuture<'a, Result<Option<Box<dyn IncrementalToolBatch + 'a>>, AgentError>> {
        Box::pin(async move {
            self.incremental_begins.fetch_add(1, Ordering::SeqCst);
            Ok(Some(Box::new(SmokeIncrementalBatch {
                runtime: self,
                batch,
                calls: BTreeMap::new(),
                sealed_count: None,
            }) as Box<dyn IncrementalToolBatch + 'a>))
        })
    }
}

/// 示例 runtime 只接管并缓存规范化调用；真正计算要等模型响应成功后的 `commit`。
struct SmokeIncrementalBatch<'a> {
    runtime: &'a SmokeTools,
    batch: ToolBatchStart,
    calls: BTreeMap<ToolCallSlot, IncrementalToolCall>,
    sealed_count: Option<u32>,
}

impl IncrementalToolBatch for SmokeIncrementalBatch<'_> {
    fn submit<'a>(
        &'a mut self,
        call: IncrementalToolCall,
    ) -> PortFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            // 这些条件由核心保证；违反意味着实现接线错误，不能降级成普通工具失败。
            assert_eq!(call.run_id, self.batch.run_id);
            assert_eq!(call.batch_attempt_id, self.batch.batch_attempt_id);
            assert!(self.sealed_count.is_none(), "封口后不能再提交工具调用");
            assert!(
                !self.calls.contains_key(&call.slot),
                "同一 slot 不能重复提交"
            );
            self.calls.insert(call.slot, call);
            Ok(())
        })
    }

    fn calls_sealed<'a>(&'a mut self, call_count: u32) -> PortFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            if self.sealed_count.is_some() {
                return Err(AgentError::new(
                    AgentErrorKind::InvalidToolBatch,
                    "smoke_batch_already_sealed",
                ));
            }
            let actual_count = u32::try_from(self.calls.len()).map_err(|_| {
                AgentError::new(
                    AgentErrorKind::InvalidToolBatch,
                    "smoke_call_count_exceeds_u32",
                )
            })?;
            let contiguous =
                (0..call_count).all(|slot| self.calls.contains_key(&ToolCallSlot::new(slot)));
            if actual_count != call_count || !contiguous {
                return Err(AgentError::new(
                    AgentErrorKind::InvalidToolBatch,
                    "smoke_incremental_slots_not_contiguous",
                ));
            }

            self.sealed_count = Some(call_count);
            self.runtime
                .incremental_seals
                .fetch_add(1, Ordering::SeqCst);
            self.runtime
                .started
                .send(call_count as usize)
                .map_err(|_| {
                    AgentError::new(AgentErrorKind::ToolDispatch, "smoke_receiver_closed")
                })?;
            Ok(())
        })
    }

    fn commit<'a>(self: Box<Self>) -> PortFuture<'a, Result<ToolResultBatch, AgentError>>
    where
        Self: 'a,
    {
        Box::pin(async move {
            let expected_count = self.sealed_count.ok_or_else(|| {
                AgentError::new(
                    AgentErrorKind::InvalidToolBatch,
                    "smoke_incremental_batch_not_sealed",
                )
            })?;
            if self.calls.len() != expected_count as usize {
                return Err(AgentError::new(
                    AgentErrorKind::InvalidToolBatch,
                    "smoke_incremental_call_count_changed",
                ));
            }
            let _permit =
                self.runtime.gate.acquire().await.map_err(|_| {
                    AgentError::new(AgentErrorKind::ToolDispatch, "smoke_gate_closed")
                })?;

            let results = self
                .calls
                .into_values()
                .map(|item| add_result(item.call.id, &item.call.arguments))
                .collect();
            self.runtime
                .incremental_commits
                .fetch_add(1, Ordering::SeqCst);
            Ok(ToolResultBatch { results })
        })
    }

    fn abort<'a>(
        self: Box<Self>,
        _reason: ToolBatchAbortReason,
    ) -> PortFuture<'a, Result<AbortedToolBatch, AgentError>>
    where
        Self: 'a,
    {
        Box::pin(async move {
            // 示例在 commit 获得 gate 前完全不执行，因此每个已 submit 项都能确定为未开始。
            let calls = self
                .calls
                .into_values()
                .map(|item| AbortedToolCall {
                    slot: item.slot,
                    call_id: item.call.id,
                    outcome: AbortedToolCallOutcome::CancelledBeforeStart,
                })
                .collect();
            Ok(AbortedToolBatch {
                batch_attempt_id: self.batch.batch_attempt_id,
                calls,
            })
        })
    }
}

fn add_result(call_id: ToolCallId, arguments: &Value) -> ToolResult {
    let values = arguments
        .get("left")
        .and_then(Value::as_i64)
        .zip(arguments.get("right").and_then(Value::as_i64));
    match values {
        Some((left, right)) => ToolResult::success_json(call_id, json!({"sum": left + right})),
        None => ToolResult::failure(call_id, "left/right 必须是整数"),
    }
}

struct NoCompaction;

impl Compaction for NoCompaction {
    fn compact<'a>(
        &'a self,
        conversation: &'a [TranscriptItem],
    ) -> PortFuture<'a, Vec<TranscriptItem>> {
        Box::pin(async move { conversation.to_vec() })
    }
}

struct PrintingObserver {
    protocol: SmokeProtocol,
}

impl Observer for PrintingObserver {
    fn observe(&self, event: &AgentEvent) {
        eprintln!(
            "[{}][{:?}] {event:?}",
            self.protocol.label(),
            event.metadata().level
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent::{RunId, ToolBatchAttemptId, ToolCall};

    fn runtime() -> (SmokeTools, mpsc::UnboundedReceiver<usize>, Arc<Semaphore>) {
        let (started, receiver) = mpsc::unbounded_channel();
        let gate = Arc::new(Semaphore::new(0));
        (
            SmokeTools {
                started,
                gate: gate.clone(),
                incremental_begins: AtomicUsize::new(0),
                incremental_seals: AtomicUsize::new(0),
                incremental_commits: AtomicUsize::new(0),
                legacy_dispatches: AtomicUsize::new(0),
            },
            receiver,
            gate,
        )
    }

    fn start() -> ToolBatchStart {
        ToolBatchStart {
            run_id: RunId::new("smoke-run"),
            batch_attempt_id: ToolBatchAttemptId::new("smoke-run/model/1/tools"),
        }
    }

    fn incremental_call(slot: u32, id: &str, left: i64, right: i64) -> IncrementalToolCall {
        let start = start();
        IncrementalToolCall {
            run_id: start.run_id,
            batch_attempt_id: start.batch_attempt_id,
            slot: ToolCallSlot::new(slot),
            call: ToolCall::new(id, "add", json!({"left": left, "right": right})),
        }
    }

    #[tokio::test]
    async fn incremental_runtime_seals_and_commits_in_slot_order() {
        let (runtime, mut started, gate) = runtime();
        let mut batch = runtime.begin_incremental(start()).await.unwrap().unwrap();

        // 完成事件可以乱序到达；commit 的结果仍按规范 slot 顺序返回。
        batch
            .submit(incremental_call(1, "second", 5, 7))
            .await
            .unwrap();
        batch
            .submit(incremental_call(0, "first", 2, 3))
            .await
            .unwrap();
        batch.calls_sealed(2).await.unwrap();
        assert_eq!(started.recv().await, Some(2));
        assert_eq!(runtime.incremental_seals.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.legacy_dispatches.load(Ordering::SeqCst), 0);

        gate.add_permits(1);
        let results = batch.commit().await.unwrap();
        assert_eq!(results.results[0].call_id.as_str(), "first");
        assert_eq!(results.results[1].call_id.as_str(), "second");
        assert_eq!(runtime.incremental_begins.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.incremental_commits.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.legacy_dispatches.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn incremental_runtime_abort_reports_every_call_as_not_started() {
        let (runtime, _started, _gate) = runtime();
        let mut batch = runtime.begin_incremental(start()).await.unwrap().unwrap();
        batch
            .submit(incremental_call(0, "first", 2, 3))
            .await
            .unwrap();
        batch
            .submit(incremental_call(1, "second", 5, 7))
            .await
            .unwrap();

        let report = batch
            .abort(ToolBatchAbortReason::ModelStreamInterrupted)
            .await
            .unwrap();
        assert_eq!(report.batch_attempt_id, start().batch_attempt_id);
        assert_eq!(report.calls.len(), 2);
        assert!(report
            .calls
            .iter()
            .all(|call| matches!(call.outcome, AbortedToolCallOutcome::CancelledBeforeStart)));
        assert_eq!(runtime.incremental_commits.load(Ordering::SeqCst), 0);
        assert_eq!(runtime.legacy_dispatches.load(Ordering::SeqCst), 0);
    }
}
