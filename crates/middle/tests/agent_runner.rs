use std::{
    collections::VecDeque,
    future::{pending, Future},
    pin::Pin,
    sync::Mutex,
    time::{Duration, Instant},
};

use mineintent_contracts::{
    agent::{
        fixtures, AgentError, AgentErrorCode, AgentRunner as ContractAgentRunner,
        CancellationSignal, ContractFuture, Deadline, ExecutionControl, ModelProvider,
        ModelRunResult, ModelUsage, PromptTemplateVersion, ToolExecution, ToolInvocation,
    },
    capability::ToolDispatcher,
};
use mineintent_middle::agent::{
    AgentModelRequest, AgentRunnerImpl, ConcreteAgentRunner, ModelCompletion,
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
        mineintent_contracts::agent::ModelName::new("model-v4")
            .expect("fixture model name is valid"),
    )
}

#[tokio::test]
async fn concrete_runner_composes_v4_once_and_maps_run_id_tools_usage_and_model() {
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
    let request = fixtures::agent_run_request_v4();
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
            model: mineintent_contracts::agent::ModelName::new("model-v4").unwrap(),
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
        .run(
            fixtures::agent_run_request_v4(),
            ExecutionControl::new(&signal, deadline),
        )
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
            .map_or(true, |content| !content.contains("__tool__"))
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
    let mut request = fixtures::agent_run_request_v4();
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
