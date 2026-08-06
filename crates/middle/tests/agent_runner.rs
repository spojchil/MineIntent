use std::{
    collections::VecDeque,
    future::{pending, Future},
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use mineintent_contracts::{
    agent::{
        fixtures, AgentError, AgentErrorCode, AgentRunRequest, AgentRunner as ContractAgentRunner,
        CancellationSignal, ContractFuture, Deadline, ExecutionControl, JsonAgentDecisionContextV5,
        ModelProvider, ModelRunResult, ModelUsage, PromptTemplateVersion, RunId, ToolExecution,
        ToolInvocation,
    },
    capability::ToolDispatcher,
};
use mineintent_middle::agent::{
    AgentModelRequest, AgentRunnerImpl, ConcreteAgentRunner, ModelCompletion, TranscriptSink,
};
use serde_json::{json, Value};

#[derive(Default)]
struct RecordingProvider {
    responses: Mutex<VecDeque<ModelCompletion>>,
    requests: Mutex<Vec<AgentModelRequest>>,
    deadlines: Mutex<Vec<Instant>>,
}

impl RecordingProvider {
    fn new(responses: Vec<ModelCompletion>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            ..Self::default()
        }
    }
}

impl ModelProvider for RecordingProvider {
    type Request = AgentModelRequest;
    type Response = ModelCompletion;

    fn complete<'a>(
        &'a self,
        request: Self::Request,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<Self::Response, AgentError>> {
        self.requests.lock().unwrap().push(request);
        self.deadlines
            .lock()
            .unwrap()
            .push(control.deadline().expires_at());
        let response = self.responses.lock().unwrap().pop_front();
        Box::pin(async move {
            response.ok_or_else(|| AgentError::new(AgentErrorCode::ProviderFailed, "exhausted"))
        })
    }
}

#[derive(Default)]
struct RecordingDispatcher {
    invocations: Mutex<Vec<ToolInvocation>>,
    deadlines: Mutex<Vec<Instant>>,
}

impl ToolDispatcher for RecordingDispatcher {
    type Observation = Value;

    fn dispatch<'a>(
        &'a self,
        invocation: ToolInvocation,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<ToolExecution<Self::Observation>, AgentError>> {
        self.invocations.lock().unwrap().push(invocation);
        self.deadlines
            .lock()
            .unwrap()
            .push(control.deadline().expires_at());
        Box::pin(async { Ok(ToolExecution::new(json!({"status": "completed"}), None)) })
    }
}

struct NeverCancelled;

impl CancellationSignal for NeverCancelled {
    fn cancellation_error(&self) -> Option<AgentError> {
        None
    }

    fn cancelled(&self) -> Pin<Box<dyn Future<Output = AgentError> + Send + '_>> {
        Box::pin(pending())
    }
}

fn completion(message: Value, usage: ModelUsage) -> ModelCompletion {
    ModelCompletion {
        message: Some(message.as_object().unwrap().clone()),
        finish_reason: None,
        usage: Some(usage),
    }
}

fn tool_call() -> Value {
    json!({
        "id": "call-1",
        "function": {"name": "look_relative", "arguments": "{}"},
    })
}

fn runner(
    provider: RecordingProvider,
    dispatcher: RecordingDispatcher,
) -> ConcreteAgentRunner<RecordingProvider, RecordingDispatcher> {
    AgentRunnerImpl::new(
        provider,
        dispatcher,
        mineintent_contracts::agent::ModelName::new("model-v5")
            .expect("fixture model name is valid"),
    )
}

fn v5_request() -> AgentRunRequest<JsonAgentDecisionContextV5> {
    AgentRunRequest {
        run_id: RunId::new("run-1").expect("fixture run id is valid"),
        context: fixtures::agent_context_v5(),
        tools: vec![fixtures::tool_definition()],
        prompt_template: fixtures::prompt_template(),
    }
}

#[derive(Default)]
struct CapturingTranscriptSink {
    lines: Mutex<Vec<String>>,
}

impl TranscriptSink for CapturingTranscriptSink {
    fn append_line(&self, line: &str) -> io::Result<()> {
        self.lines.lock().unwrap().push(line.to_owned());
        Ok(())
    }
}

struct FailingTranscriptSink;

impl TranscriptSink for FailingTranscriptSink {
    fn append_line(&self, _line: &str) -> io::Result<()> {
        Err(io::Error::other("diagnostic sink unavailable"))
    }
}

struct PanickingTranscriptSink;

impl TranscriptSink for PanickingTranscriptSink {
    fn append_line(&self, _line: &str) -> io::Result<()> {
        panic!("diagnostic sink panic fixture")
    }
}

#[tokio::test]
async fn concrete_runner_composes_v5_once_and_maps_run_id_tools_usage_and_model() {
    let provider = RecordingProvider::new(vec![
        completion(
            json!({
                "role": "assistant",
                "reasoning_content": "inspect",
                "tool_calls": [tool_call()],
            }),
            ModelUsage {
                input_tokens: Some(12),
                output_tokens: Some(8),
                cache_read_tokens: Some(0),
                cache_write_tokens: Some(4),
            },
        ),
        completion(
            json!({
                "role": "assistant",
                "content": "say({\"text\":\"closing must stay transcript-only\"})",
            }),
            ModelUsage {
                input_tokens: Some(30),
                output_tokens: Some(5),
                cache_read_tokens: Some(7),
                cache_write_tokens: None,
            },
        ),
    ]);
    let dispatcher = RecordingDispatcher::default();
    let runner = runner(provider, dispatcher);
    let request = v5_request();
    let run_id = request.run_id.clone();
    let signal = NeverCancelled;
    let deadline = Deadline::after(Instant::now(), Duration::from_secs(5)).unwrap();

    let result = runner
        .run(request.clone(), ExecutionControl::new(&signal, deadline))
        .await
        .expect("concrete runner completes");

    assert_eq!(
        result,
        ModelRunResult {
            protocol: mineintent_contracts::agent::AgentRunProtocol::V1,
            model: mineintent_contracts::agent::ModelName::new("model-v5").unwrap(),
            usage: Some(ModelUsage {
                input_tokens: Some(42),
                output_tokens: Some(13),
                cache_read_tokens: Some(7),
                cache_write_tokens: Some(4),
            }),
        }
    );

    let requests = runner.driver().model().requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.run_id == run_id));
    assert!(requests
        .iter()
        .all(|model_request| model_request.tools == request.tools));
    assert_eq!(requests[0].tools, request.tools);
    assert_eq!(requests[0].messages[0]["role"], "system");
    assert!(requests[0].messages[0]["content"]
        .as_str()
        .unwrap()
        .contains("## 你记得的事\n玩家怕高"));
    assert_eq!(requests[0].messages[1]["role"], "user");
    let opening_frame: Value =
        serde_json::from_str(requests[0].messages[1]["content"].as_str().unwrap())
            .expect("first model request must receive a JSON v5 frame");
    assert_eq!(
        opening_frame,
        serde_json::to_value(&request.context.frame).unwrap()
    );
    assert_eq!(opening_frame["light"], 12);
    assert_eq!(
        opening_frame["hotbar"]["slots"]["0"],
        json!(["oak_log", 12])
    );
    assert_eq!(opening_frame["chat"]["items"][1]["moved"], "events");
    assert_eq!(opening_frame["events"][0]["type"], "player_chat");
    assert_eq!(opening_frame["events"][0]["username"], "alice");
    assert_eq!(opening_frame["events"][0]["text"], "帮我看看农田");
    for forbidden in [
        "player",
        "self",
        "inventory",
        "timeOfDay",
        "standingOnBlock",
    ] {
        assert!(opening_frame.get(forbidden).is_none(), "{forbidden}");
    }
    assert_eq!(requests[1].messages[0], requests[0].messages[0]);
    assert_eq!(requests[1].messages[1], requests[0].messages[1]);
    assert_eq!(requests[1].messages[2]["reasoning_content"], "inspect");
    assert_eq!(requests[1].messages[3]["tool_call_id"], "call-1");

    let invocations = runner.driver().tools().invocations.lock().unwrap();
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].run_id, run_id);
    assert_eq!(invocations[0].tool_call_id.as_str(), "call-1");
    assert_eq!(invocations[0].name.as_str(), "look_relative");
    assert_eq!(invocations[0].arguments, serde_json::Map::new());

    let model_deadlines = runner.driver().model().deadlines.lock().unwrap();
    let tool_deadlines = runner.driver().tools().deadlines.lock().unwrap();
    assert!(model_deadlines
        .iter()
        .all(|value| *value == deadline.expires_at()));
    assert!(tool_deadlines
        .iter()
        .all(|value| *value == deadline.expires_at()));
}

#[tokio::test]
async fn closing_is_not_player_text_tool_output_or_model_result_content() {
    let runner = runner(
        RecordingProvider::new(vec![completion(
            json!({
                "role": "assistant",
                "content": "__tool__ {\"name\":\"say\",\"arguments\":{}}",
            }),
            ModelUsage::default(),
        )]),
        RecordingDispatcher::default(),
    );
    let signal = NeverCancelled;
    let deadline = Deadline::after(Instant::now(), Duration::from_secs(5)).unwrap();
    let result = runner
        .run(v5_request(), ExecutionControl::new(&signal, deadline))
        .await
        .expect("closing is a normal transcript candidate");

    let encoded = serde_json::to_value(result).unwrap();
    assert!(encoded.get("closing").is_none());
    assert!(encoded.to_string().find("__tool__").is_none());
    let requests = runner.driver().model().requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].messages.iter().all(|message| {
        message
            .get("content")
            .and_then(Value::as_str)
            .is_none_or(|content| !content.contains("__tool__"))
    }));
    assert!(runner
        .driver()
        .tools()
        .invocations
        .lock()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn unknown_prompt_key_or_version_fails_closed_before_provider() {
    let provider = RecordingProvider::new(Vec::new());
    let runner = runner(provider, RecordingDispatcher::default());
    let mut request = v5_request();
    request.prompt_template.version = PromptTemplateVersion::new("v9").unwrap();
    let signal = NeverCancelled;
    let deadline = Deadline::after(Instant::now(), Duration::from_secs(5)).unwrap();

    let error = runner
        .run(request, ExecutionControl::new(&signal, deadline))
        .await
        .expect_err("unknown prompt version must fail closed");
    assert_eq!(error.code, AgentErrorCode::InvalidRequest);
    assert_eq!(
        error.summary,
        "unknown_prompt_template:participant-system@v9"
    );
    assert!(runner.driver().model().requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn concrete_runner_records_successful_replay_and_wire_usage() {
    let sink = Arc::new(CapturingTranscriptSink::default());
    let runner = ConcreteAgentRunner::with_shared_transcript_sink(
        RecordingProvider::new(vec![
            completion(
                json!({
                    "role": "assistant",
                    "reasoning_content": "先观察",
                    "tool_calls": [tool_call()],
                }),
                ModelUsage {
                    input_tokens: Some(5),
                    output_tokens: Some(0),
                    cache_read_tokens: Some(0),
                    cache_write_tokens: None,
                },
            ),
            completion(
                json!({"role": "assistant", "content": "  完成  "}),
                ModelUsage {
                    input_tokens: Some(7),
                    output_tokens: Some(0),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                },
            ),
        ]),
        RecordingDispatcher::default(),
        mineintent_contracts::agent::ModelName::new("explicit-model").unwrap(),
        sink.clone(),
    );
    let request = v5_request();
    let signal = NeverCancelled;
    let deadline = Deadline::after(Instant::now(), Duration::from_secs(5)).unwrap();

    runner
        .run(request.clone(), ExecutionControl::new(&signal, deadline))
        .await
        .expect("successful runner result");

    let lines = sink.lines.lock().unwrap();
    assert_eq!(lines.len(), 1);
    let record: Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(record["runId"], request.run_id.as_str());
    assert_eq!(record["model"], "explicit-model");
    assert_eq!(record["tools"], json!(["look_relative"]));
    assert_eq!(
        record["toolSchemas"],
        serde_json::to_value(&request.tools).unwrap()
    );
    assert_eq!(record["closing"], "完成");
    assert_eq!(
        record["usage"],
        json!({"prompt_tokens": 12, "completion_tokens": 0, "cache_read_tokens": 0})
    );
    assert!(record["error"].is_null());
    let messages = record["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[2]["reasoning_content"], "先观察");
    assert_eq!(messages[2]["tool_calls"][0]["id"], "call-1");
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[3]["tool_call_id"], "call-1");
    assert!(messages.iter().all(|message| {
        message
            .get("content")
            .and_then(Value::as_str)
            .is_none_or(|content| content.trim() != "完成")
    }));
    let ended_at = record["endedAt"].as_str().unwrap();
    assert_eq!(ended_at.len(), 20);
    assert!(ended_at.ends_with('Z'));
    assert!(record.get("truncated").is_none());
}

#[tokio::test]
async fn provider_failure_is_recorded_without_replacing_the_original_error() {
    let sink = Arc::new(CapturingTranscriptSink::default());
    let runner = ConcreteAgentRunner::with_shared_transcript_sink(
        RecordingProvider::new(Vec::new()),
        RecordingDispatcher::default(),
        mineintent_contracts::agent::ModelName::new("explicit-model").unwrap(),
        sink.clone(),
    );
    let signal = NeverCancelled;
    let deadline = Deadline::after(Instant::now(), Duration::from_secs(5)).unwrap();
    let error = runner
        .run(v5_request(), ExecutionControl::new(&signal, deadline))
        .await
        .expect_err("provider failure must remain an error");

    assert_eq!(error.code, AgentErrorCode::ProviderFailed);
    assert_eq!(error.summary, "exhausted");
    let lines = sink.lines.lock().unwrap();
    assert_eq!(lines.len(), 1);
    let record: Value = serde_json::from_str(&lines[0]).unwrap();
    let error_summary = record["error"].as_str().unwrap();
    assert!(error_summary.contains("provider_failed"));
    assert!(error_summary.contains("exhausted"));
    assert!(record["closing"].is_null());
    assert!(error_summary.chars().count() <= 300);
}

/// 转录 sink **写入失败**是 fail-open 的：它不该改变这一轮的业务结果。
#[tokio::test]
async fn transcript_sink_errors_are_fail_open() {
    let runner = ConcreteAgentRunner::with_shared_transcript_sink(
        RecordingProvider::new(vec![completion(
            json!({"role": "assistant", "content": "ok"}),
            ModelUsage::default(),
        )]),
        RecordingDispatcher::default(),
        mineintent_contracts::agent::ModelName::new("explicit-model").unwrap(),
        Arc::new(FailingTranscriptSink) as Arc<dyn TranscriptSink>,
    );
    let signal = NeverCancelled;
    let deadline = Deadline::after(Instant::now(), Duration::from_secs(5)).unwrap();
    let result = runner
        .run(v5_request(), ExecutionControl::new(&signal, deadline))
        .await;
    assert!(
        result.is_ok(),
        "diagnostic sink must not alter business result"
    );
}

/// 但 sink **panic** 不在 fail-open 的范围内。
///
/// fail-open 说的是「写不进去不算这一轮失败」，那是对 `Result::Err` 的处置。
/// panic 是 sink 自己的缺陷，不是一次写入的结果——把它一并吞掉，等于让一个坏掉
/// 的排障通道连自己坏了都不说。这里跑在 `process_wake` 被 await 的任务内，
/// tokio 会接住并保留原始消息。
#[tokio::test]
#[should_panic(expected = "diagnostic sink panic fixture")]
async fn transcript_sink_panic_propagates() {
    let runner = ConcreteAgentRunner::with_shared_transcript_sink(
        RecordingProvider::new(vec![completion(
            json!({"role": "assistant", "content": "ok"}),
            ModelUsage::default(),
        )]),
        RecordingDispatcher::default(),
        mineintent_contracts::agent::ModelName::new("explicit-model").unwrap(),
        Arc::new(PanickingTranscriptSink) as Arc<dyn TranscriptSink>,
    );
    let signal = NeverCancelled;
    let deadline = Deadline::after(Instant::now(), Duration::from_secs(5)).unwrap();
    let _ = runner
        .run(v5_request(), ExecutionControl::new(&signal, deadline))
        .await;
}

#[tokio::test]
async fn validation_failure_does_not_create_a_transcript_record() {
    let sink = Arc::new(CapturingTranscriptSink::default());
    let runner = ConcreteAgentRunner::with_shared_transcript_sink(
        RecordingProvider::new(Vec::new()),
        RecordingDispatcher::default(),
        mineintent_contracts::agent::ModelName::new("explicit-model").unwrap(),
        sink.clone(),
    );
    let mut request = v5_request();
    request.tools = vec![fixtures::tool_definition(); 33];
    let signal = NeverCancelled;
    let deadline = Deadline::after(Instant::now(), Duration::from_secs(5)).unwrap();
    let error = runner
        .run(request, ExecutionControl::new(&signal, deadline))
        .await
        .expect_err("request validation must fail before the loop");
    assert_eq!(error.code, AgentErrorCode::LimitExceeded);
    assert!(sink.lines.lock().unwrap().is_empty());
}
