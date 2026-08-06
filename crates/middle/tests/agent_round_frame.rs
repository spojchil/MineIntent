use std::{
    collections::{HashMap, VecDeque},
    future::{pending, Future},
    io,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use mineintent_contracts::{
    agent::{
        fixtures, AgentError, AgentErrorCode, AgentRunRequest, AgentRunner as ContractAgentRunner,
        CancellationSignal, ContractFuture, Deadline, ExecutionControl, JsonAgentDecisionContextV5,
        JsonObject, ModelProvider, ModelUsage, RunId, ToolExecution, ToolInvocation,
        ViewportFrameMessage,
    },
    capability::{ExecutionResource, ToolDispatcher},
};
use mineintent_middle::agent::{
    AgentLoopDriver, AgentModelRequest, AgentRun, ConcreteAgentRunner, ModelCompletion,
    RoundViewportSampler,
};
use serde::{ser::Error as _, Serialize, Serializer};
use serde_json::{json, Value};
use tokio::sync::Notify;

fn object(value: Value) -> JsonObject {
    value.as_object().expect("fixture object").clone()
}

fn initial_run() -> AgentRun {
    AgentRun::new(
        RunId::new("round-frame-run").expect("valid run id"),
        vec![object(json!({"role": "user", "content": "opening"}))],
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

fn tool_call(id: &str, name: &str) -> Value {
    json!({
        "id": id,
        "function": {"name": name, "arguments": "{}"},
    })
}

fn completion(message: Value) -> ModelCompletion {
    ModelCompletion {
        message: Some(object(message)),
        finish_reason: None,
        usage: Some(ModelUsage::default()),
    }
}

#[derive(Default)]
struct ScriptedProvider {
    responses: Mutex<VecDeque<ModelCompletion>>,
    requests: Mutex<Vec<AgentModelRequest>>,
    deadlines: Mutex<Vec<Instant>>,
}

impl ScriptedProvider {
    fn new(responses: Vec<ModelCompletion>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            ..Self::default()
        }
    }
}

impl ModelProvider for ScriptedProvider {
    type Request = AgentModelRequest;
    type Response = ModelCompletion;

    fn complete<'a>(
        &'a self,
        request: Self::Request,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<Self::Response, AgentError>> {
        self.requests.lock().expect("request lock").push(request);
        self.deadlines
            .lock()
            .expect("deadline lock")
            .push(control.deadline().expires_at());
        let response = self.responses.lock().expect("response lock").pop_front();
        Box::pin(async move {
            response.ok_or_else(|| AgentError::new(AgentErrorCode::ProviderFailed, "exhausted"))
        })
    }
}

#[derive(Clone, Copy)]
enum DispatchBehavior {
    Success,
    Failure,
    Refused,
    Panic,
}

struct ClassifiedDispatcher {
    resources: HashMap<String, ExecutionResource>,
    behaviors: HashMap<String, DispatchBehavior>,
    resource_calls: AtomicUsize,
    invocations: Mutex<Vec<String>>,
}

impl ClassifiedDispatcher {
    fn new(
        resources: impl IntoIterator<Item = (&'static str, ExecutionResource)>,
        behaviors: impl IntoIterator<Item = (&'static str, DispatchBehavior)>,
    ) -> Self {
        Self {
            resources: resources
                .into_iter()
                .map(|(name, resource)| (name.to_owned(), resource))
                .collect(),
            behaviors: behaviors
                .into_iter()
                .map(|(name, behavior)| (name.to_owned(), behavior))
                .collect(),
            resource_calls: AtomicUsize::new(0),
            invocations: Mutex::new(Vec::new()),
        }
    }
}

impl ToolDispatcher for ClassifiedDispatcher {
    type Observation = Value;

    fn resource(&self, invocation: &ToolInvocation) -> Option<ExecutionResource> {
        self.resource_calls.fetch_add(1, Ordering::SeqCst);
        self.resources.get(invocation.name.as_str()).copied()
    }

    fn dispatch<'a>(
        &'a self,
        invocation: ToolInvocation,
        _control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<ToolExecution<Self::Observation>, AgentError>> {
        let name = invocation.name.into_inner();
        self.invocations
            .lock()
            .expect("invocation lock")
            .push(name.clone());
        let behavior = self
            .behaviors
            .get(&name)
            .copied()
            .unwrap_or(DispatchBehavior::Success);
        Box::pin(async move {
            match behavior {
                DispatchBehavior::Success => Ok(ToolExecution::new(
                    json!({"status": "completed", "tool": name}),
                    name.starts_with("body_")
                        .then(|| json!({"damage": 2, "sound": "step"})),
                )),
                DispatchBehavior::Failure => Err(AgentError::new(
                    AgentErrorCode::ToolFailed,
                    "body_read_failed",
                )),
                DispatchBehavior::Refused => Err(AgentError::new(
                    AgentErrorCode::ToolFailed,
                    "body_refused_by_backend",
                )),
                DispatchBehavior::Panic => panic!("dispatcher panic fixture"),
            }
        })
    }
}

struct ResourcePanicDispatcher {
    dispatch_calls: AtomicUsize,
}

impl ToolDispatcher for ResourcePanicDispatcher {
    type Observation = Value;

    fn resource(&self, _invocation: &ToolInvocation) -> Option<ExecutionResource> {
        panic!("resource classifier panic fixture")
    }

    fn dispatch<'a>(
        &'a self,
        _invocation: ToolInvocation,
        _control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<ToolExecution<Self::Observation>, AgentError>> {
        self.dispatch_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(ToolExecution::new(
                json!({"status": "must-not-dispatch"}),
                None,
            ))
        })
    }
}

#[derive(Clone)]
enum SampleBehavior {
    Success(Value),
    Error(&'static str),
    Panic,
    Pending,
}

struct RecordingSampler {
    behavior: SampleBehavior,
    at: String,
    calls: AtomicUsize,
    deadlines: Mutex<Vec<Instant>>,
}

impl RecordingSampler {
    fn success(value: Value) -> Self {
        Self {
            behavior: SampleBehavior::Success(canonical_viewport(&marker(&value))),
            at: "2026-08-02T12:34:56Z".to_owned(),
            calls: AtomicUsize::new(0),
            deadlines: Mutex::new(Vec::new()),
        }
    }

    fn with_behavior(behavior: SampleBehavior) -> Self {
        Self {
            behavior,
            at: "2026-08-02T12:34:56Z".to_owned(),
            calls: AtomicUsize::new(0),
            deadlines: Mutex::new(Vec::new()),
        }
    }
}

fn marker(value: &Value) -> String {
    value
        .as_object()
        .and_then(|object| object.values().next())
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn canonical_viewport(marker: &str) -> Value {
    json!({
        "protocol": "mineintent.viewport.v2",
        "frame": {
            "coordinates": "minecraft_world_absolute",
            "self": {
                "position": [0.5, 64.0, 0.5],
                "yawDegrees": 0.0,
                "pitchDegrees": 0.0
            },
            "legend": {
                "visibleEntities": "items: {type, player?, position}; nearest first",
                "visibleBlocks": "[BlockInfo, x, y, z]; nearest first"
            }
        },
        "lookedAtBlock": null,
        "visibleEntities": {
            "items": [{"type": "marker", "player": marker, "position": [0.0, 64.0, 0.0]}],
            "truncated": false
        },
        "visibleBlocks": {"blocks": [], "truncated": false}
    })
}

impl RoundViewportSampler for RecordingSampler {
    type Viewport = Value;

    fn timestamp(&self) -> String {
        self.at.clone()
    }

    fn sample<'a>(
        &'a self,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<Self::Viewport, AgentError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.deadlines
            .lock()
            .expect("sampler deadline lock")
            .push(control.deadline().expires_at());
        match self.behavior.clone() {
            SampleBehavior::Success(value) => Box::pin(async move { Ok(value) }),
            SampleBehavior::Error(summary) => {
                Box::pin(async move { Err(AgentError::new(AgentErrorCode::ToolFailed, summary)) })
            }
            SampleBehavior::Panic => Box::pin(async move { panic!("sampler panic fixture") }),
            SampleBehavior::Pending => Box::pin(async move { pending().await }),
        }
    }
}

struct NonSerializableViewport;

impl Serialize for NonSerializableViewport {
    fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Err(S::Error::custom("viewport serializer fixture"))
    }
}

struct SerializationFailSampler {
    calls: AtomicUsize,
}

impl RoundViewportSampler for SerializationFailSampler {
    type Viewport = NonSerializableViewport;

    fn timestamp(&self) -> String {
        "2026-08-02T12:34:56Z".to_owned()
    }

    fn sample<'a>(
        &'a self,
        _control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<Self::Viewport, AgentError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(NonSerializableViewport) })
    }
}

struct TimestampPanicSampler {
    sample_calls: AtomicUsize,
}

impl RoundViewportSampler for TimestampPanicSampler {
    type Viewport = Value;

    fn timestamp(&self) -> String {
        panic!("timestamp panic fixture")
    }

    fn sample<'a>(
        &'a self,
        _control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<Self::Viewport, AgentError>> {
        self.sample_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(json!({"must-not-sample": true})) })
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

#[derive(Default)]
struct ManualCancellation {
    cancelled: AtomicBool,
    notify: Notify,
}

impl ManualCancellation {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}

impl CancellationSignal for ManualCancellation {
    fn cancellation_error(&self) -> Option<AgentError> {
        self.cancelled
            .load(Ordering::SeqCst)
            .then(AgentError::run_cancelled)
    }

    fn cancelled(&self) -> Pin<Box<dyn Future<Output = AgentError> + Send + '_>> {
        Box::pin(async move {
            loop {
                let notified = self.notify.notified();
                if let Some(error) = self.cancellation_error() {
                    return error;
                }
                notified.await;
            }
        })
    }
}

fn active_control(signal: &NeverCancelled) -> ExecutionControl<'_> {
    ExecutionControl::new(
        signal,
        Deadline::after(Instant::now(), Duration::from_secs(5)).expect("test deadline"),
    )
}

fn frame_content(message: &JsonObject) -> Value {
    serde_json::from_str(
        message["content"]
            .as_str()
            .expect("user frame content is a string"),
    )
    .expect("user frame content is strict JSON")
}

fn looks_like_utc_second_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'Z'
        && bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 4 | 7 | 10 | 13 | 16 | 19) || byte.is_ascii_digit()
        })
}

#[test]
fn viewport_frame_success_and_failure_have_exact_strict_wire() {
    let success = ViewportFrameMessage::success("2026-08-02T12:34:56Z", json!({"visible": true}))
        .expect("success frame");
    assert_eq!(
        serde_json::to_string(&success).expect("success wire"),
        r#"{"protocol":"mineintent.viewport-frame.v1","at":"2026-08-02T12:34:56Z","viewport":{"visible":true}}"#
    );

    let unavailable =
        ViewportFrameMessage::unavailable("2026-08-02T12:34:56Z", "viewport_read_failed")
            .expect("failure frame");
    assert_eq!(
        serde_json::to_string(&unavailable).expect("failure wire"),
        r#"{"protocol":"mineintent.viewport-frame.v1","at":"2026-08-02T12:34:56Z","viewport":null,"unavailable":"viewport_read_failed"}"#
    );

    assert!(serde_json::from_str::<ViewportFrameMessage>(
        r#"{"protocol":"mineintent.viewport-frame.v1","at":"2026-08-02T12:34:56Z","viewport":{"visible":true},"tool_call_id":"x"}"#
    )
    .is_err());
    assert!(serde_json::from_str::<ViewportFrameMessage>(
        r#"{"protocol":"mineintent.viewport-frame.v1","at":"2026-08-02T12:34:56Z","viewport":null}"#
    )
    .is_err());
    assert!(serde_json::from_str::<ViewportFrameMessage>(
        r#"{"protocol":"mineintent.viewport-frame.v1","at":"2026-08-02T12:34:56Z","viewport":null,"unavailable":""}"#
    )
    .is_err());

    for invalid_at in [
        "",
        "   ",
        "\t",
        "2026-08-02T12:34:56\nZ",
        "2026-08-02T12:34:56\u{0000}Z",
    ] {
        assert!(ViewportFrameMessage::success(invalid_at, json!({"visible": true})).is_err());
        assert!(ViewportFrameMessage::unavailable(invalid_at, "read_failed").is_err());
        let success_wire = json!({
            "protocol": "mineintent.viewport-frame.v1",
            "at": invalid_at,
            "viewport": {"visible": true},
        });
        let failure_wire = json!({
            "protocol": "mineintent.viewport-frame.v1",
            "at": invalid_at,
            "viewport": null,
            "unavailable": "read_failed",
        });
        assert!(serde_json::from_value::<ViewportFrameMessage>(success_wire).is_err());
        assert!(serde_json::from_value::<ViewportFrameMessage>(failure_wire).is_err());
    }
}

#[tokio::test]
async fn multiple_body_calls_sample_once_after_all_tool_messages() {
    let provider = ScriptedProvider::new(vec![
        completion(json!({
            "role": "assistant",
            "tool_calls": [tool_call("one", "body_one"), tool_call("two", "body_two")],
        })),
        completion(json!({"role": "assistant", "content": "done"})),
    ]);
    let dispatcher = ClassifiedDispatcher::new(
        [
            ("body_one", ExecutionResource::Body),
            ("body_two", ExecutionResource::Body),
        ],
        [],
    );
    let sampler = RecordingSampler::success(json!({"projection": "round"}));
    let driver = AgentLoopDriver::new_with_viewport_sampler(provider, dispatcher, sampler);
    let signal = NeverCancelled;
    let mut run = initial_run();
    driver
        .drive(&mut run, &[], active_control(&signal))
        .await
        .expect("body round completes");

    let requests = driver.model().requests.lock().expect("request lock");
    let replay = &requests[1].messages;
    assert_eq!(replay[2]["role"], "tool");
    assert_eq!(replay[2]["tool_call_id"], "one");
    let body_result: Value =
        serde_json::from_str(replay[2]["content"].as_str().unwrap()).expect("body result JSON");
    assert_eq!(body_result["observationAfter"]["damage"], 2);
    assert_eq!(replay[3]["role"], "tool");
    assert_eq!(replay[3]["tool_call_id"], "two");
    assert_eq!(replay[4]["role"], "user");
    assert_eq!(
        frame_content(&replay[4])["protocol"],
        "mineintent.viewport-frame.v2"
    );
    assert_eq!(
        frame_content(&replay[4])["viewport"]["visibleEntities"]["items"][0]["player"],
        "round"
    );
    assert!(replay[4].get("tool_call_id").is_none());
    assert_eq!(driver.viewport_sampler().calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn body_failure_and_busy_still_sample_once() {
    let provider = ScriptedProvider::new(vec![
        completion(json!({
            "role": "assistant",
            "tool_calls": [
                tool_call("failed", "body_failed"),
                tool_call("busy", "body_refused"),
            ],
        })),
        completion(json!({"role": "assistant", "content": "done"})),
    ]);
    let dispatcher = ClassifiedDispatcher::new(
        [
            ("body_failed", ExecutionResource::Body),
            ("body_refused", ExecutionResource::Body),
        ],
        [
            ("body_failed", DispatchBehavior::Failure),
            ("body_refused", DispatchBehavior::Refused),
        ],
    );
    let sampler = RecordingSampler::success(json!({"after": "failures"}));
    let driver = AgentLoopDriver::new_with_viewport_sampler(provider, dispatcher, sampler);
    let signal = NeverCancelled;
    let mut run = initial_run();
    driver
        .drive(&mut run, &[], active_control(&signal))
        .await
        .expect("ordinary body failures keep the run alive");

    let requests = driver.model().requests.lock().expect("request lock");
    let replay = &requests[1].messages;
    assert_eq!(replay[2]["role"], "tool");
    assert_eq!(replay[3]["role"], "tool");
    let first: Value = serde_json::from_str(replay[2]["content"].as_str().unwrap()).unwrap();
    let second: Value = serde_json::from_str(replay[3]["content"].as_str().unwrap()).unwrap();
    assert_eq!(first["result"]["summary"], "body_read_failed");
    assert_eq!(second["result"]["summary"], "body_refused_by_backend");
    assert_eq!(replay[4]["role"], "user");
    assert_eq!(
        frame_content(&replay[4])["viewport"]["visibleEntities"]["items"][0]["player"],
        "failures"
    );
    assert_eq!(driver.viewport_sampler().calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn mixed_view_and_body_preserves_view_result_and_adds_one_round_frame() {
    let provider = ScriptedProvider::new(vec![
        completion(json!({
            "role": "assistant",
            "tool_calls": [tool_call("view", "view_tool"), tool_call("body", "body_tool")],
        })),
        completion(json!({"role": "assistant", "content": "done"})),
    ]);
    let dispatcher = ClassifiedDispatcher::new(
        [
            ("view_tool", ExecutionResource::Viewport),
            ("body_tool", ExecutionResource::Body),
        ],
        [],
    );
    let sampler = RecordingSampler::success(json!({"frame": "after-body"}));
    let driver = AgentLoopDriver::new_with_viewport_sampler(provider, dispatcher, sampler);
    let signal = NeverCancelled;
    let mut run = initial_run();
    driver
        .drive(&mut run, &[], active_control(&signal))
        .await
        .expect("mixed round completes");

    let requests = driver.model().requests.lock().expect("request lock");
    let replay = &requests[1].messages;
    let view_result: Value = serde_json::from_str(replay[2]["content"].as_str().unwrap()).unwrap();
    assert_eq!(view_result["result"]["tool"], "view_tool");
    assert_eq!(replay[2]["tool_call_id"], "view");
    assert_eq!(replay[3]["tool_call_id"], "body");
    assert_eq!(replay[4]["role"], "user");
    assert_eq!(
        frame_content(&replay[4])["viewport"]["visibleEntities"]["items"][0]["player"],
        "after-body"
    );
    assert_eq!(driver.viewport_sampler().calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn pure_view_and_say_batches_do_not_add_a_frame() {
    let provider = ScriptedProvider::new(vec![
        completion(json!({
            "role": "assistant",
            "tool_calls": [
                tool_call("view", "view_tool"),
                tool_call("say", "say_tool"),
                tool_call("memory", "memory_tool"),
            ],
        })),
        completion(json!({"role": "assistant", "content": "done"})),
    ]);
    let dispatcher = ClassifiedDispatcher::new(
        [
            ("view_tool", ExecutionResource::Viewport),
            ("say_tool", ExecutionResource::Chat),
            ("memory_tool", ExecutionResource::Memory),
        ],
        [],
    );
    let sampler = RecordingSampler::success(json!({"should": "not-run"}));
    let driver = AgentLoopDriver::new_with_viewport_sampler(provider, dispatcher, sampler);
    let signal = NeverCancelled;
    let mut run = initial_run();
    driver
        .drive(&mut run, &[], active_control(&signal))
        .await
        .expect("pure non-body round completes");

    let requests = driver.model().requests.lock().expect("request lock");
    let replay = &requests[1].messages;
    assert_eq!(replay.len(), 5);
    assert!(replay.iter().all(|message| {
        message
            .get("content")
            .and_then(Value::as_str)
            .and_then(|content| serde_json::from_str::<Value>(content).ok())
            .is_none_or(|value| value["protocol"] != "mineintent.viewport-frame.v2")
    }));
    assert_eq!(driver.viewport_sampler().calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn local_invalid_call_does_not_classify_or_sample() {
    let invalid_name = "x".repeat(65);
    let provider = ScriptedProvider::new(vec![
        completion(json!({
            "role": "assistant",
            "tool_calls": [tool_call("invalid", &invalid_name)],
        })),
        completion(json!({"role": "assistant", "content": "done"})),
    ]);
    let dispatcher = ClassifiedDispatcher::new([], []);
    let sampler = RecordingSampler::success(json!({"should": "not-run"}));
    let driver = AgentLoopDriver::new_with_viewport_sampler(provider, dispatcher, sampler);
    let signal = NeverCancelled;
    let mut run = initial_run();
    driver
        .drive(&mut run, &[], active_control(&signal))
        .await
        .expect("local invalid call remains paired");

    let requests = driver.model().requests.lock().expect("request lock");
    let replay = &requests[1].messages;
    assert_eq!(replay.len(), 3);
    assert_eq!(replay[2]["role"], "tool");
    assert_eq!(driver.tools().resource_calls.load(Ordering::SeqCst), 0);
    assert_eq!(driver.viewport_sampler().calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn missing_sampler_uses_real_utc_for_explicit_unavailable_frame() {
    let provider = ScriptedProvider::new(vec![
        completion(json!({
            "role": "assistant",
            "tool_calls": [tool_call("body", "body_tool")],
        })),
        completion(json!({"role": "assistant", "content": "done"})),
    ]);
    let dispatcher = ClassifiedDispatcher::new([("body_tool", ExecutionResource::Body)], []);
    let driver = AgentLoopDriver::new(provider, dispatcher);
    let signal = NeverCancelled;
    let mut run = initial_run();
    driver
        .drive(&mut run, &[], active_control(&signal))
        .await
        .expect("missing sampler remains an explicit failure");

    let requests = driver.model().requests.lock().expect("request lock");
    let frame = frame_content(&requests[1].messages[3]);
    assert_eq!(frame["protocol"], "mineintent.viewport-frame.v2");
    assert_eq!(frame["viewport"], Value::Null);
    assert_eq!(frame["unavailable"], "viewport_sampler_not_configured");
    let at = frame["at"].as_str().expect("UTC at string");
    assert!(looks_like_utc_second_timestamp(at));
    assert_ne!(at, "1970-01-01T00:00:00Z");
}

/// 分类器 panic 照常传播，不再被压成一条配对的工具失败。
#[tokio::test]
#[should_panic(expected = "resource classifier panic fixture")]
async fn resource_classifier_panic_propagates() {
    let provider = ScriptedProvider::new(vec![completion(json!({
        "role": "assistant",
        "tool_calls": [tool_call("unknown", "resource_panics")],
    }))]);
    let dispatcher = ResourcePanicDispatcher {
        dispatch_calls: AtomicUsize::new(0),
    };
    let sampler = RecordingSampler::success(json!({"must-not": "sample"}));
    let driver = AgentLoopDriver::new_with_viewport_sampler(provider, dispatcher, sampler);
    let signal = NeverCancelled;
    let mut run = initial_run();
    let _ = driver.drive(&mut run, &[], active_control(&signal)).await;
}

#[tokio::test]
async fn sampler_read_failure_is_an_explicit_unavailable_frame() {
    let provider = ScriptedProvider::new(vec![
        completion(json!({
            "role": "assistant",
            "tool_calls": [tool_call("body", "body_tool")],
        })),
        completion(json!({"role": "assistant", "content": "done"})),
    ]);
    let dispatcher = ClassifiedDispatcher::new([("body_tool", ExecutionResource::Body)], []);
    let sampler = RecordingSampler::with_behavior(SampleBehavior::Error("viewport_read_failed"));
    let driver = AgentLoopDriver::new_with_viewport_sampler(provider, dispatcher, sampler);
    let signal = NeverCancelled;
    let mut run = initial_run();
    driver
        .drive(&mut run, &[], active_control(&signal))
        .await
        .expect("ordinary sampler failure is observable and recoverable");

    let requests = driver.model().requests.lock().expect("request lock");
    let frame = frame_content(&requests[1].messages[3]);
    assert_eq!(frame["viewport"], Value::Null);
    assert_eq!(frame["unavailable"], "viewport_read_failed");
}

/// 采样器 panic 照常传播。
///
/// 「读不到视口」与「采样器有缺陷」是两件事：前者是世界事实，应当变成
/// `unavailable` 帧告诉模型（见上一个测试）；后者是我们的 bug，不该被伪装成
/// 一次读不到。
#[tokio::test]
#[should_panic(expected = "sampler panic fixture")]
async fn sampler_panic_propagates() {
    let provider = ScriptedProvider::new(vec![completion(json!({
        "role": "assistant",
        "tool_calls": [tool_call("body", "body_tool")],
    }))]);
    let dispatcher = ClassifiedDispatcher::new([("body_tool", ExecutionResource::Body)], []);
    let sampler = RecordingSampler::with_behavior(SampleBehavior::Panic);
    let driver = AgentLoopDriver::new_with_viewport_sampler(provider, dispatcher, sampler);
    let signal = NeverCancelled;
    let mut run = initial_run();
    let _ = driver.drive(&mut run, &[], active_control(&signal)).await;
}

/// 时间源 panic 照常传播。
#[tokio::test]
#[should_panic(expected = "timestamp panic fixture")]
async fn sampler_timestamp_panic_propagates() {
    let provider = ScriptedProvider::new(vec![completion(json!({
        "role": "assistant",
        "tool_calls": [tool_call("body", "body_tool")],
    }))]);
    let dispatcher = ClassifiedDispatcher::new([("body_tool", ExecutionResource::Body)], []);
    let sampler = TimestampPanicSampler {
        sample_calls: AtomicUsize::new(0),
    };
    let driver = AgentLoopDriver::new_with_viewport_sampler(provider, dispatcher, sampler);
    let signal = NeverCancelled;
    let mut run = initial_run();
    let _ = driver.drive(&mut run, &[], active_control(&signal)).await;
}

#[tokio::test]
async fn sampler_serialization_failure_is_an_unavailable_frame() {
    let provider = ScriptedProvider::new(vec![
        completion(json!({
            "role": "assistant",
            "tool_calls": [tool_call("body", "body_tool")],
        })),
        completion(json!({"role": "assistant", "content": "done"})),
    ]);
    let dispatcher = ClassifiedDispatcher::new([("body_tool", ExecutionResource::Body)], []);
    let sampler = SerializationFailSampler {
        calls: AtomicUsize::new(0),
    };
    let driver = AgentLoopDriver::new_with_viewport_sampler(provider, dispatcher, sampler);
    let signal = NeverCancelled;
    let mut run = initial_run();
    driver
        .drive(&mut run, &[], active_control(&signal))
        .await
        .expect("serialization failure is observable and recoverable");

    let requests = driver.model().requests.lock().expect("request lock");
    let frame = frame_content(&requests[1].messages[3]);
    assert_eq!(frame["viewport"], Value::Null);
    assert_eq!(frame["unavailable"], "viewport_frame_serialization_failed");
    assert_eq!(driver.viewport_sampler().calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn sampler_receives_the_same_absolute_deadline_as_model_and_dispatcher() {
    let provider = ScriptedProvider::new(vec![
        completion(json!({
            "role": "assistant",
            "tool_calls": [tool_call("body", "body_tool")],
        })),
        completion(json!({"role": "assistant", "content": "done"})),
    ]);
    let dispatcher = ClassifiedDispatcher::new([("body_tool", ExecutionResource::Body)], []);
    let sampler = RecordingSampler::success(json!({"ok": true}));
    let driver = AgentLoopDriver::new_with_viewport_sampler(provider, dispatcher, sampler);
    let signal = NeverCancelled;
    let deadline = Deadline::after(Instant::now(), Duration::from_secs(5)).expect("deadline");
    let expected = deadline.expires_at();
    let mut run = initial_run();
    driver
        .drive(&mut run, &[], ExecutionControl::new(&signal, deadline))
        .await
        .expect("shared control run");

    assert_eq!(
        driver
            .model()
            .deadlines
            .lock()
            .expect("model deadlines")
            .as_slice(),
        &[expected, expected]
    );
    assert_eq!(
        driver
            .viewport_sampler()
            .deadlines
            .lock()
            .expect("sampler deadlines")
            .as_slice(),
        &[expected]
    );
}

#[tokio::test]
async fn sampler_cancellation_has_priority_and_does_not_emit_a_failure_frame() {
    let provider = ScriptedProvider::new(vec![
        completion(json!({
            "role": "assistant",
            "tool_calls": [tool_call("body", "body_tool")],
        })),
        completion(json!({"role": "assistant", "content": "never"})),
    ]);
    let dispatcher = ClassifiedDispatcher::new([("body_tool", ExecutionResource::Body)], []);
    let sampler = RecordingSampler::with_behavior(SampleBehavior::Pending);
    let driver = AgentLoopDriver::new_with_viewport_sampler(provider, dispatcher, sampler);
    let signal = ManualCancellation::default();
    let deadline = Deadline::after(Instant::now(), Duration::from_secs(5)).expect("deadline");
    let mut run = initial_run();
    let drive = driver.drive(&mut run, &[], ExecutionControl::new(&signal, deadline));
    let cancel = async {
        tokio::task::yield_now().await;
        signal.cancel();
    };
    let (result, ()) = tokio::join!(drive, cancel);
    assert_eq!(
        result.expect_err("cancelled sampler stops run").code,
        AgentErrorCode::RunCancelled
    );
    assert_eq!(driver.model().requests.lock().expect("requests").len(), 1);
    assert!(driver.model().requests.lock().expect("requests")[0]
        .messages
        .iter()
        .all(|message| {
            message
                .get("content")
                .and_then(Value::as_str)
                .is_none_or(|content| !content.contains("mineintent.viewport-frame.v2"))
        }));
}

#[tokio::test]
async fn sampler_deadline_has_priority_and_does_not_emit_a_failure_frame() {
    let provider = ScriptedProvider::new(vec![
        completion(json!({
            "role": "assistant",
            "tool_calls": [tool_call("body", "body_tool")],
        })),
        completion(json!({"role": "assistant", "content": "never"})),
    ]);
    let dispatcher = ClassifiedDispatcher::new([("body_tool", ExecutionResource::Body)], []);
    let sampler = RecordingSampler::with_behavior(SampleBehavior::Pending);
    let driver = AgentLoopDriver::new_with_viewport_sampler(provider, dispatcher, sampler);
    let signal = NeverCancelled;
    let deadline = Deadline::after(Instant::now(), Duration::from_millis(20)).expect("deadline");
    let mut run = initial_run();
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        driver.drive(&mut run, &[], ExecutionControl::new(&signal, deadline)),
    )
    .await
    .expect("deadline watchdog")
    .expect_err("deadline stops pending sampler");
    assert_eq!(result.code, AgentErrorCode::DeadlineExceeded);
    assert_eq!(driver.model().requests.lock().expect("requests").len(), 1);
}

#[derive(Default)]
struct CapturingSink {
    lines: Mutex<Vec<String>>,
}

impl mineintent_middle::agent::TranscriptSink for CapturingSink {
    fn append_line(&self, line: &str) -> io::Result<()> {
        self.lines.lock().expect("sink lock").push(line.to_owned());
        Ok(())
    }
}

#[tokio::test]
async fn frame_is_in_the_next_model_request_and_transcript() {
    let sink = Arc::new(CapturingSink::default());
    let runner = ConcreteAgentRunner::with_viewport_sampler_and_shared_transcript_sink(
        ScriptedProvider::new(vec![
            completion(json!({
                "role": "assistant",
                "tool_calls": [tool_call("body", "look_relative")],
            })),
            completion(json!({"role": "assistant", "content": "done"})),
        ]),
        ClassifiedDispatcher::new([("look_relative", ExecutionResource::Body)], []),
        mineintent_contracts::agent::ModelName::new("frame-model").expect("model name"),
        RecordingSampler::success(json!({"round": 1})),
        sink.clone(),
    );
    let request = v5_request();
    let expected_opening_frame =
        serde_json::to_value(&request.context.frame).expect("v5 opening frame serializes");
    let signal = NeverCancelled;
    let deadline = Deadline::after(Instant::now(), Duration::from_secs(5)).expect("deadline");
    runner
        .run(request.clone(), ExecutionControl::new(&signal, deadline))
        .await
        .expect("runner completes");

    let requests = runner.driver().model().requests.lock().expect("requests");
    assert_eq!(requests.len(), 2);
    let opening_frame: Value = serde_json::from_str(
        requests[0].messages[1]["content"]
            .as_str()
            .expect("first opening message is JSON"),
    )
    .expect("first model request receives a JSON v5 frame");
    assert_eq!(opening_frame, expected_opening_frame);
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
    assert_eq!(
        frame_content(requests[1].messages.last().unwrap())["viewport"]["visibleEntities"]["items"]
            [0]["player"],
        "{\"round\":1}"
    );
    let lines = sink.lines.lock().expect("sink lines");
    assert_eq!(lines.len(), 1);
    let record: Value = serde_json::from_str(&lines[0]).expect("transcript JSON");
    let messages = record["messages"].as_array().expect("transcript messages");
    assert!(messages.iter().any(|message| {
        message
            .get("content")
            .and_then(Value::as_str)
            .is_some_and(|content| content.contains("mineintent.viewport-frame.v2"))
    }));
}
