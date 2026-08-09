use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;

use serde_json::{json, Value};
use tokio::sync::{mpsc, Semaphore};

use super::*;
use crate::mailbox::{Delivery, MailboxRejectedReason};
use crate::ports::{ModelResponse, PortFuture};
use crate::types::{
    AgentErrorKind, ContentPart, InputMessage, ToolCall, ToolCallBatch, ToolCallId, ToolDefinition,
    ToolResult, ToolResultBatch,
};

fn input(role: &str, text: &str) -> TranscriptItem {
    InputMessage::text(role, text).into()
}

fn dangling_tool_call(id: &str) -> TranscriptItem {
    TranscriptItem::ModelOutput(ModelOutput::calls(vec![ToolCall::new(
        id,
        "read",
        json!({"path": "injected"}),
    )]))
}

fn complete_tool_round(id: &str) -> Vec<TranscriptItem> {
    vec![
        dangling_tool_call(id),
        TranscriptItem::ToolResults(ToolResultBatch {
            results: vec![ToolResult::success_json(
                ToolCallId::new(id),
                json!({"ok": true}),
            )],
        }),
    ]
}

fn visible_texts(transcript: &[TranscriptItem]) -> Vec<String> {
    transcript
        .iter()
        .flat_map(|item| match item {
            TranscriptItem::Input(message) => message.content.iter(),
            TranscriptItem::ModelOutput(output) => output.content.iter(),
            TranscriptItem::ToolResults(_) => [].iter(),
        })
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.clone()),
            ContentPart::Json { .. } | ContentPart::Opaque { .. } => None,
        })
        .collect()
}

fn tool_call_ids(transcript: &[TranscriptItem]) -> Vec<String> {
    transcript
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::ModelOutput(output) => Some(&output.tool_calls),
            TranscriptItem::Input(_) | TranscriptItem::ToolResults(_) => None,
        })
        .flatten()
        .map(|call| call.id.as_str().to_owned())
        .collect()
}

struct StaticPrompt;

impl PromptSource for StaticPrompt {
    fn base_context(&self) -> Vec<TranscriptItem> {
        vec![input("system", "base")]
    }

    fn run_context(&self) -> Vec<TranscriptItem> {
        vec![input("developer", "run-context")]
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

struct GatedCompaction {
    started: mpsc::UnboundedSender<()>,
    gate: Arc<Semaphore>,
}

impl Compaction for GatedCompaction {
    fn compact<'a>(
        &'a self,
        conversation: &'a [TranscriptItem],
    ) -> PortFuture<'a, Vec<TranscriptItem>> {
        Box::pin(async move {
            self.started.send(()).unwrap();
            let _permit = self.gate.acquire().await.unwrap();
            conversation.to_vec()
        })
    }
}

#[derive(Default)]
struct DanglingCompaction {
    compactions: AtomicUsize,
}

impl Compaction for DanglingCompaction {
    fn compact<'a>(
        &'a self,
        conversation: &'a [TranscriptItem],
    ) -> PortFuture<'a, Vec<TranscriptItem>> {
        Box::pin(async move {
            self.compactions.fetch_add(1, Ordering::SeqCst);
            let mut corrupted = conversation.to_vec();
            corrupted.push(dangling_tool_call("compaction-orphan"));
            corrupted
        })
    }
}

struct ChannelModel {
    requests: mpsc::UnboundedSender<ModelRequest>,
    responses: Mutex<mpsc::UnboundedReceiver<Result<ModelResponse, AgentError>>>,
}

impl Model for ChannelModel {
    fn complete<'a>(
        &'a self,
        request: ModelRequest,
    ) -> PortFuture<'a, Result<ModelResponse, AgentError>> {
        Box::pin(async move {
            self.requests.send(request).unwrap();
            self.responses.lock().await.recv().await.unwrap()
        })
    }
}

struct GatedBatchRuntime {
    batches: mpsc::UnboundedSender<ToolCallBatch>,
    gate: Arc<Semaphore>,
    dispatches: AtomicUsize,
}

impl ToolRuntime for GatedBatchRuntime {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition::new(
            "read",
            json!({"type": "object", "additionalProperties": true}),
        )]
    }

    fn dispatch<'a>(
        &'a self,
        batch: ToolCallBatch,
    ) -> PortFuture<'a, Result<ToolResultBatch, AgentError>> {
        Box::pin(async move {
            self.dispatches.fetch_add(1, Ordering::SeqCst);
            self.batches.send(batch.clone()).unwrap();
            let _permit = self.gate.acquire().await.unwrap();
            Ok(ToolResultBatch {
                results: batch
                    .calls
                    .iter()
                    .rev()
                    .map(|call| {
                        ToolResult::success_json(
                            call.id.clone(),
                            json!({"name": call.name.as_str()}),
                        )
                    })
                    .collect(),
            })
        })
    }
}

#[derive(Default)]
struct FailingBatchRuntime {
    dispatches: AtomicUsize,
}

impl ToolRuntime for FailingBatchRuntime {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition::new(
            "read",
            json!({"type": "object", "additionalProperties": true}),
        )]
    }

    fn dispatch<'a>(
        &'a self,
        _batch: ToolCallBatch,
    ) -> PortFuture<'a, Result<ToolResultBatch, AgentError>> {
        Box::pin(async move {
            self.dispatches.fetch_add(1, Ordering::SeqCst);
            Err(AgentError::new(
                AgentErrorKind::ToolDispatch,
                "dispatch infrastructure failed",
            ))
        })
    }
}

struct PanickingBatchRuntime;

impl ToolRuntime for PanickingBatchRuntime {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition::new(
            "read",
            json!({"type": "object", "additionalProperties": true}),
        )]
    }

    fn dispatch<'a>(
        &'a self,
        _batch: ToolCallBatch,
    ) -> PortFuture<'a, Result<ToolResultBatch, AgentError>> {
        Box::pin(async move { panic!("tool runtime bug") })
    }
}

#[derive(Default)]
struct FailingSecondBatchRuntime {
    dispatches: AtomicUsize,
}

impl ToolRuntime for FailingSecondBatchRuntime {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition::new(
            "read",
            json!({"type": "object", "additionalProperties": true}),
        )]
    }

    fn dispatch<'a>(
        &'a self,
        batch: ToolCallBatch,
    ) -> PortFuture<'a, Result<ToolResultBatch, AgentError>> {
        Box::pin(async move {
            let index = self.dispatches.fetch_add(1, Ordering::SeqCst);
            if index == 1 {
                return Err(AgentError::new(
                    AgentErrorKind::ToolDispatch,
                    "second dispatch failed",
                ));
            }
            Ok(ToolResultBatch {
                results: batch
                    .calls
                    .into_iter()
                    .map(|call| ToolResult::success_json(call.id, json!({"ok": true})))
                    .collect(),
            })
        })
    }
}

struct Fixture {
    session: Arc<AgentSession>,
    requests: mpsc::UnboundedReceiver<ModelRequest>,
    responses: mpsc::UnboundedSender<Result<ModelResponse, AgentError>>,
    batches: mpsc::UnboundedReceiver<ToolCallBatch>,
    gate: Arc<Semaphore>,
    runtime: Arc<GatedBatchRuntime>,
}

fn fixture() -> Fixture {
    let (request_tx, request_rx) = mpsc::unbounded_channel();
    let (response_tx, response_rx) = mpsc::unbounded_channel();
    let (batch_tx, batch_rx) = mpsc::unbounded_channel();
    let gate = Arc::new(Semaphore::new(0));
    let runtime = Arc::new(GatedBatchRuntime {
        batches: batch_tx,
        gate: gate.clone(),
        dispatches: AtomicUsize::new(0),
    });
    let session = Arc::new(AgentSession::new(
        Arc::new(StaticPrompt),
        runtime.clone(),
        Arc::new(NoCompaction),
        Arc::new(ChannelModel {
            requests: request_tx,
            responses: Mutex::new(response_rx),
        }),
        SessionConfig::default(),
    ));
    Fixture {
        session,
        requests: request_rx,
        responses: response_tx,
        batches: batch_rx,
        gate,
        runtime,
    }
}

fn model_response(output: ModelOutput) -> ModelResponse {
    ModelResponse {
        output,
        ..ModelResponse::default()
    }
}

#[tokio::test]
async fn complete_tool_array_is_forwarded_once_and_steering_waits_for_it() {
    let mut fixture = fixture();
    let session = fixture.session.clone();
    let task =
        tokio::spawn(async move { session.start_if_idle(vec![input("operator", "go")]).await });

    let first = fixture.requests.recv().await.unwrap();
    assert_eq!(
        visible_texts(&first.transcript),
        vec!["base", "run-context", "go"]
    );
    fixture
        .responses
        .send(Ok(model_response(ModelOutput::calls(vec![
            ToolCall::new("call-a", "read", json!({"path": "a"})),
            ToolCall::new("call-b", "read", json!({"path": "b"})),
        ]))))
        .unwrap();

    let batch = fixture.batches.recv().await.unwrap();
    assert_eq!(batch.calls.len(), 2);
    fixture
        .session
        .enqueue_if_running(MailboxInput::next_model_request(vec![input(
            "developer",
            "steer",
        )]))
        .await
        .unwrap();
    fixture.gate.add_permits(1);

    let second = fixture.requests.recv().await.unwrap();
    assert_eq!(visible_texts(&second.transcript).last().unwrap(), "steer");
    let TranscriptItem::ToolResults(results) = &second.transcript[4] else {
        panic!("tool batch should precede mailbox input");
    };
    assert_eq!(results.results[0].call_id.as_str(), "call-a");
    assert_eq!(results.results[1].call_id.as_str(), "call-b");
    assert_eq!(fixture.runtime.dispatches.load(Ordering::SeqCst), 1);

    fixture
        .responses
        .send(Ok(model_response(ModelOutput::text("done"))))
        .unwrap();
    let outcome = task.await.unwrap().unwrap();
    assert!(
        matches!(outcome, TurnOutcome::Completed { output, .. } if output.text_content() == "done")
    );
}

#[tokio::test]
async fn dispatch_failure_does_not_leak_an_orphaned_tool_call_into_the_next_run() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let (response_tx, response_rx) = mpsc::unbounded_channel();
    let runtime = Arc::new(FailingBatchRuntime::default());
    let session = Arc::new(AgentSession::new(
        Arc::new(StaticPrompt),
        runtime.clone(),
        Arc::new(NoCompaction),
        Arc::new(ChannelModel {
            requests: request_tx,
            responses: Mutex::new(response_rx),
        }),
        SessionConfig::default(),
    ));

    let running = session.clone();
    let first_run = tokio::spawn(async move {
        running
            .start_if_idle(vec![input("operator", "first run")])
            .await
    });
    request_rx.recv().await.unwrap();
    response_tx
        .send(Ok(model_response(ModelOutput::calls(vec![ToolCall::new(
            "orphan-call",
            "read",
            json!({"path": "missing"}),
        )]))))
        .unwrap();

    let outcome = first_run.await.unwrap().unwrap();
    let TurnOutcome::Failed { error, undelivered } = outcome else {
        panic!("dispatch 顶层错误应终止当前运行");
    };
    assert_eq!(error.kind, AgentErrorKind::ToolDispatch);
    assert_eq!(error.summary, "dispatch infrastructure failed");
    assert!(undelivered.is_empty());
    assert_eq!(runtime.dispatches.load(Ordering::SeqCst), 1);

    let conversation = session.conversation().await;
    assert!(
        tool_call_ids(&conversation).is_empty(),
        "失败运行不应持久化尚未配对的模型工具调用"
    );

    let running = session.clone();
    let second_run = tokio::spawn(async move {
        running
            .start_if_idle(vec![input("operator", "second run")])
            .await
    });
    let second_request = request_rx.recv().await.unwrap();
    assert!(
        tool_call_ids(&second_request.transcript).is_empty(),
        "新运行发送给模型的 transcript 不应包含上轮孤儿调用"
    );
    response_tx
        .send(Ok(model_response(ModelOutput::text("recovered"))))
        .unwrap();

    assert!(matches!(
        second_run.await.unwrap().unwrap(),
        TurnOutcome::Completed { output, .. } if output.text_content() == "recovered"
    ));
}

#[tokio::test]
async fn later_dispatch_failure_preserves_only_the_preceding_complete_tool_round() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let (response_tx, response_rx) = mpsc::unbounded_channel();
    let runtime = Arc::new(FailingSecondBatchRuntime::default());
    let session = Arc::new(AgentSession::new(
        Arc::new(StaticPrompt),
        runtime.clone(),
        Arc::new(NoCompaction),
        Arc::new(ChannelModel {
            requests: request_tx,
            responses: Mutex::new(response_rx),
        }),
        SessionConfig::default(),
    ));

    let running = session.clone();
    let run = tokio::spawn(async move {
        running
            .start_if_idle(vec![input("operator", "two batches")])
            .await
    });
    request_rx.recv().await.unwrap();
    response_tx
        .send(Ok(model_response(ModelOutput::calls(vec![ToolCall::new(
            "complete-call",
            "read",
            json!({}),
        )]))))
        .unwrap();

    let continuation = request_rx.recv().await.unwrap();
    assert_eq!(tool_call_ids(&continuation.transcript), ["complete-call"]);
    response_tx
        .send(Ok(model_response(ModelOutput::calls(vec![ToolCall::new(
            "failed-call",
            "read",
            json!({}),
        )]))))
        .unwrap();

    let outcome = run.await.unwrap().unwrap();
    assert!(matches!(
        outcome,
        TurnOutcome::Failed { error, .. }
            if error.kind == AgentErrorKind::ToolDispatch
                && error.summary == "second dispatch failed"
    ));
    assert_eq!(runtime.dispatches.load(Ordering::SeqCst), 2);

    let conversation = session.conversation().await;
    assert_eq!(tool_call_ids(&conversation), ["complete-call"]);
    assert!(matches!(
        conversation.get(2),
        Some(TranscriptItem::ToolResults(_))
    ));
}

#[tokio::test]
async fn tool_runtime_panic_is_rethrown_after_session_cleanup() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let (response_tx, response_rx) = mpsc::unbounded_channel();
    let session = Arc::new(AgentSession::new(
        Arc::new(StaticPrompt),
        Arc::new(PanickingBatchRuntime),
        Arc::new(NoCompaction),
        Arc::new(ChannelModel {
            requests: request_tx,
            responses: Mutex::new(response_rx),
        }),
        SessionConfig::default(),
    ));

    let running = session.clone();
    let panicking_run = tokio::spawn(async move {
        running
            .start_if_idle(vec![input("operator", "trigger panic")])
            .await
    });
    request_rx.recv().await.unwrap();
    response_tx
        .send(Ok(model_response(ModelOutput::calls(vec![ToolCall::new(
            "panic-call",
            "read",
            json!({}),
        )]))))
        .unwrap();

    let join_error = panicking_run.await.unwrap_err();
    assert!(join_error.is_panic(), "工具实现的 panic 不应降级成普通失败");
    assert!(session.conversation().await.is_empty());

    // 清理只恢复 session 的可用性，不吞掉上一次 panic，也不保存未闭合记录。
    let running = session.clone();
    let next_run = tokio::spawn(async move {
        running
            .start_if_idle(vec![input("operator", "after panic")])
            .await
    });
    let next_request = request_rx.recv().await.unwrap();
    assert!(tool_call_ids(&next_request.transcript).is_empty());
    response_tx
        .send(Ok(model_response(ModelOutput::text("recovered"))))
        .unwrap();
    assert!(matches!(
        next_run.await.unwrap().unwrap(),
        TurnOutcome::Completed { output, .. } if output.text_content() == "recovered"
    ));
}

#[tokio::test]
async fn steering_arriving_during_final_model_call_reopens_the_run() {
    let mut fixture = fixture();
    let session = fixture.session.clone();
    let task = tokio::spawn(async move {
        session
            .start_if_idle(vec![input("operator", "draft")])
            .await
    });
    fixture.requests.recv().await.unwrap();

    fixture
        .session
        .enqueue_if_running(MailboxInput::next_model_request(vec![input(
            "reviewer", "revise",
        )]))
        .await
        .unwrap();
    fixture
        .responses
        .send(Ok(model_response(ModelOutput::text("candidate"))))
        .unwrap();

    let second = fixture.requests.recv().await.unwrap();
    assert_eq!(
        visible_texts(&second.transcript),
        vec!["base", "run-context", "draft", "candidate", "revise"]
    );
    fixture
        .responses
        .send(Ok(model_response(ModelOutput::text("revised"))))
        .unwrap();
    let outcome = task.await.unwrap().unwrap();
    assert!(
        matches!(outcome, TurnOutcome::Completed { output, .. } if output.text_content() == "revised")
    );
}

#[tokio::test]
async fn when_idle_waits_through_tool_continuation() {
    let mut fixture = fixture();
    let session = fixture.session.clone();
    let task =
        tokio::spawn(async move { session.start_if_idle(vec![input("operator", "go")]).await });
    fixture.requests.recv().await.unwrap();
    fixture
        .responses
        .send(Ok(model_response(ModelOutput::calls(vec![ToolCall::new(
            "call-a",
            "read",
            json!({}),
        )]))))
        .unwrap();
    fixture.batches.recv().await.unwrap();
    fixture
        .session
        .enqueue_if_running(MailboxInput::when_idle(vec![input(
            "operator",
            "follow up",
        )]))
        .await
        .unwrap();
    fixture.gate.add_permits(1);

    let after_tools = fixture.requests.recv().await.unwrap();
    assert!(!visible_texts(&after_tools.transcript).contains(&"follow up".to_owned()));
    fixture
        .responses
        .send(Ok(model_response(ModelOutput::text("first answer"))))
        .unwrap();

    let follow_up = fixture.requests.recv().await.unwrap();
    assert_eq!(
        visible_texts(&follow_up.transcript).last().unwrap(),
        "follow up"
    );
    fixture
        .responses
        .send(Ok(model_response(ModelOutput::text("second answer"))))
        .unwrap();
    let outcome = task.await.unwrap().unwrap();
    assert!(
        matches!(outcome, TurnOutcome::Completed { output, .. } if output.text_content() == "second answer")
    );
}

#[tokio::test]
async fn idle_and_busy_rejections_return_original_content() {
    let mut fixture = fixture();
    let rejected = fixture
        .session
        .enqueue_if_running(MailboxInput::next_model_request(vec![input(
            "custom", "idle",
        )]))
        .await
        .unwrap_err();
    assert_eq!(rejected.reason, MailboxRejectedReason::Idle);
    assert_eq!(rejected.input.items, vec![input("custom", "idle")]);

    let session = fixture.session.clone();
    let task =
        tokio::spawn(async move { session.start_if_idle(vec![input("operator", "go")]).await });
    fixture.requests.recv().await.unwrap();
    let rejected = fixture
        .session
        .start_if_idle(vec![input("operator", "other")])
        .await
        .unwrap_err();
    assert_eq!(rejected.reason, StartRejectedReason::Busy);
    fixture
        .responses
        .send(Ok(model_response(ModelOutput::text("done"))))
        .unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn external_inputs_reject_dangling_calls_but_keep_input_roles_open() {
    let mut fixture = fixture();
    let invalid_items = vec![dangling_tool_call("invalid-start-call")];

    let rejected = fixture
        .session
        .start_if_idle(invalid_items.clone())
        .await
        .unwrap_err();
    assert_eq!(rejected.reason, StartRejectedReason::InvalidInput);
    assert_eq!(rejected.initial_items, invalid_items);
    assert!(fixture.requests.try_recv().is_err());

    let running = fixture.session.clone();
    let task = tokio::spawn(async move {
        running
            .start_if_idle(vec![input("custom-start-role", "valid start")])
            .await
    });
    let first_request = fixture.requests.recv().await.unwrap();
    assert_eq!(
        visible_texts(&first_request.transcript).last().unwrap(),
        "valid start"
    );

    let invalid_input =
        MailboxInput::next_model_request(vec![dangling_tool_call("invalid-mailbox-call")]);
    let rejected = fixture
        .session
        .enqueue_if_running(invalid_input.clone())
        .await
        .unwrap_err();
    assert_eq!(rejected.reason, MailboxRejectedReason::InvalidInput);
    assert_eq!(rejected.input, invalid_input);

    fixture
        .session
        .enqueue_if_running(MailboxInput::next_model_request(vec![input(
            "custom-mailbox-role",
            "valid mailbox",
        )]))
        .await
        .unwrap();
    fixture
        .responses
        .send(Ok(model_response(ModelOutput::text("candidate"))))
        .unwrap();

    let second_request = fixture.requests.recv().await.unwrap();
    assert_eq!(
        visible_texts(&second_request.transcript).last().unwrap(),
        "valid mailbox"
    );
    assert!(tool_call_ids(&second_request.transcript).is_empty());
    fixture
        .responses
        .send(Ok(model_response(ModelOutput::text("done"))))
        .unwrap();
    assert!(matches!(
        task.await.unwrap().unwrap(),
        TurnOutcome::Completed { output, .. } if output.text_content() == "done"
    ));
}

#[tokio::test]
async fn independently_closed_mailbox_segments_compose_even_when_ids_repeat() {
    let mut fixture = fixture();
    let running = fixture.session.clone();
    let task = tokio::spawn(async move {
        running
            .start_if_idle(vec![input("operator", "compose segments")])
            .await
    });
    fixture.requests.recv().await.unwrap();

    fixture
        .session
        .enqueue_if_running(MailboxInput::next_model_request(complete_tool_round(
            "reused-history-id",
        )))
        .await
        .unwrap();
    fixture
        .session
        .enqueue_if_running(MailboxInput::next_model_request(complete_tool_round(
            "reused-history-id",
        )))
        .await
        .unwrap();
    fixture
        .responses
        .send(Ok(model_response(ModelOutput::text("candidate"))))
        .unwrap();

    let continuation = fixture.requests.recv().await.unwrap();
    assert_eq!(
        tool_call_ids(&continuation.transcript)
            .iter()
            .filter(|id| id.as_str() == "reused-history-id")
            .count(),
        2
    );
    fixture
        .responses
        .send(Ok(model_response(ModelOutput::text("done"))))
        .unwrap();
    assert!(matches!(
        task.await.unwrap().unwrap(),
        TurnOutcome::Completed { output, .. } if output.text_content() == "done"
    ));
}

#[tokio::test]
async fn invalid_compaction_output_falls_back_to_the_closed_conversation() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let (response_tx, response_rx) = mpsc::unbounded_channel();
    let (batch_tx, _batch_rx) = mpsc::unbounded_channel();
    let compaction = Arc::new(DanglingCompaction::default());
    let session = Arc::new(AgentSession::new(
        Arc::new(StaticPrompt),
        Arc::new(GatedBatchRuntime {
            batches: batch_tx,
            gate: Arc::new(Semaphore::new(0)),
            dispatches: AtomicUsize::new(0),
        }),
        compaction.clone(),
        Arc::new(ChannelModel {
            requests: request_tx,
            responses: Mutex::new(response_rx),
        }),
        SessionConfig {
            compaction_trigger_bytes: 0,
        },
    ));

    let running = session.clone();
    let task = tokio::spawn(async move {
        running
            .start_if_idle(vec![input("custom-role", "keep original")])
            .await
    });
    request_rx.recv().await.unwrap();
    response_tx
        .send(Ok(model_response(ModelOutput::text("closed answer"))))
        .unwrap();
    assert!(matches!(
        task.await.unwrap().unwrap(),
        TurnOutcome::Completed { .. }
    ));
    assert_eq!(compaction.compactions.load(Ordering::SeqCst), 1);

    let conversation = session.conversation().await;
    assert_eq!(
        conversation,
        vec![
            input("custom-role", "keep original"),
            TranscriptItem::ModelOutput(ModelOutput::text("closed answer")),
        ]
    );
    assert!(tool_call_ids(&conversation).is_empty());
}

#[tokio::test]
async fn completion_seal_rejects_late_input_while_history_is_finalizing() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let (response_tx, response_rx) = mpsc::unbounded_channel();
    let (batch_tx, _batch_rx) = mpsc::unbounded_channel();
    let tool_gate = Arc::new(Semaphore::new(0));
    let (compaction_tx, mut compaction_rx) = mpsc::unbounded_channel();
    let compaction_gate = Arc::new(Semaphore::new(0));
    let session = Arc::new(AgentSession::new(
        Arc::new(StaticPrompt),
        Arc::new(GatedBatchRuntime {
            batches: batch_tx,
            gate: tool_gate,
            dispatches: AtomicUsize::new(0),
        }),
        Arc::new(GatedCompaction {
            started: compaction_tx,
            gate: compaction_gate.clone(),
        }),
        Arc::new(ChannelModel {
            requests: request_tx,
            responses: Mutex::new(response_rx),
        }),
        SessionConfig {
            compaction_trigger_bytes: 0,
        },
    ));

    let running = session.clone();
    let task = tokio::spawn(async move {
        running
            .start_if_idle(vec![input("operator", "finish")])
            .await
    });
    request_rx.recv().await.unwrap();
    response_tx
        .send(Ok(model_response(ModelOutput::text("done"))))
        .unwrap();
    compaction_rx.recv().await.unwrap();

    let rejected = session
        .enqueue_if_running(MailboxInput::next_model_request(vec![input(
            "operator", "too late",
        )]))
        .await
        .unwrap_err();
    assert_eq!(rejected.reason, MailboxRejectedReason::Closing);

    compaction_gate.add_permits(1);
    assert!(matches!(
        task.await.unwrap().unwrap(),
        TurnOutcome::Completed { .. }
    ));
}

#[tokio::test]
async fn cancelling_the_waiter_does_not_cancel_the_owned_driver() {
    let mut fixture = fixture();
    let running = fixture.session.clone();
    let waiter = tokio::spawn(async move {
        running
            .start_if_idle(vec![input("operator", "first")])
            .await
    });
    fixture.requests.recv().await.unwrap();

    // 丢弃调用方等待任务；session 内部的 driver 仍应完成并解除 Busy。
    waiter.abort();
    let _ = waiter.await;
    fixture
        .responses
        .send(Ok(model_response(ModelOutput::text("first done"))))
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let completed = fixture.session.conversation().await.iter().any(|item| {
                matches!(item, TranscriptItem::ModelOutput(output) if output.text_content() == "first done")
            });
            if completed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let running = fixture.session.clone();
    let second = tokio::spawn(async move {
        running
            .start_if_idle(vec![input("operator", "second")])
            .await
    });
    fixture.requests.recv().await.unwrap();
    fixture
        .responses
        .send(Ok(model_response(ModelOutput::text("second done"))))
        .unwrap();
    assert!(matches!(
        second.await.unwrap().unwrap(),
        TurnOutcome::Completed { .. }
    ));
}

#[test]
fn content_ir_keeps_non_text_values_available_to_adapters() {
    let value = Value::String("raw".to_owned());
    assert_eq!(
        ContentPart::json(value.clone()),
        ContentPart::Json { value }
    );
    assert_eq!(Delivery::NextModelRequest, Delivery::NextModelRequest);
    assert_eq!(ToolCallId::new("x").as_str(), "x");
}

struct PanicEnabledObserver;

impl Observer for PanicEnabledObserver {
    fn enabled(&self, _metadata: EventMetadata) -> bool {
        panic!("enabled panic")
    }

    fn observe(&self, _event: &AgentEvent) {
        unreachable!()
    }
}

struct PanicObserveObserver;

impl Observer for PanicObserveObserver {
    fn observe(&self, _event: &AgentEvent) {
        panic!("observe panic")
    }
}

#[test]
fn observer_filter_and_writer_panics_are_isolated_and_build_is_lazy() {
    let mut fixture = fixture();
    let session = Arc::get_mut(&mut fixture.session).unwrap();
    session.observer = Arc::new(PanicEnabledObserver);
    let built = AtomicBool::new(false);
    session.emit_lazy(EventKind::RunStopped.metadata(), || {
        built.store(true, Ordering::SeqCst);
        AgentEvent::RunStopped {
            sequence: 1,
            run_id: RunId::new("run-test"),
        }
    });
    assert!(!built.load(Ordering::SeqCst));

    session.observer = Arc::new(PanicObserveObserver);
    session.emit_lazy(EventKind::RunStopped.metadata(), || {
        built.store(true, Ordering::SeqCst);
        AgentEvent::RunStopped {
            sequence: 2,
            run_id: RunId::new("run-test"),
        }
    });
    assert!(built.load(Ordering::SeqCst));
}

#[derive(Default)]
struct DebugRecordingObserver {
    lines: StdMutex<Vec<String>>,
}

impl Observer for DebugRecordingObserver {
    fn observe(&self, event: &AgentEvent) {
        self.lines.lock().unwrap().push(format!("{event:?}"));
    }
}

#[tokio::test]
async fn core_failure_event_does_not_copy_provider_error_body() {
    let sensitive = "SENSITIVE_PROVIDER_BODY_SENTINEL";
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let (response_tx, response_rx) = mpsc::unbounded_channel();
    let (batch_tx, _batch_rx) = mpsc::unbounded_channel();
    let observer = Arc::new(DebugRecordingObserver::default());
    let session = Arc::new(
        AgentSession::new(
            Arc::new(StaticPrompt),
            Arc::new(GatedBatchRuntime {
                batches: batch_tx,
                gate: Arc::new(Semaphore::new(0)),
                dispatches: AtomicUsize::new(0),
            }),
            Arc::new(NoCompaction),
            Arc::new(ChannelModel {
                requests: request_tx,
                responses: Mutex::new(response_rx),
            }),
            SessionConfig::default(),
        )
        .with_observer(observer.clone()),
    );

    let running = session.clone();
    let task =
        tokio::spawn(async move { running.start_if_idle(vec![input("user", "request")]).await });
    request_rx.recv().await.unwrap();
    response_tx
        .send(Err(AgentError::new(AgentErrorKind::Model, sensitive)))
        .unwrap();
    let outcome = task.await.unwrap().unwrap();
    assert!(matches!(outcome, TurnOutcome::Failed { error, .. } if error.summary == sensitive));
    assert!(observer
        .lines
        .lock()
        .unwrap()
        .iter()
        .all(|line| !line.contains(sensitive)));
}
