use std::{
    collections::VecDeque,
    future::{pending, Future},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use mineintent_contracts::{
    agent::{
        fixtures, AgentError, AgentErrorCode, CancellationSignal, ContractFuture, Deadline,
        ExecutionControl, JsonObject, ModelProvider, RunId, ToolExecution, ToolInvocation,
    },
    capability::ToolDispatcher,
};
use mineintent_middle::agent::{AgentLoopDriver, AgentModelRequest, AgentRun, ModelCompletion};
use serde_json::{json, Value};
use tokio::sync::Notify;

fn object(value: Value) -> JsonObject {
    value.as_object().cloned().expect("fixture object")
}

fn initial_run() -> AgentRun {
    AgentRun::new(
        RunId::new("driver-run").expect("valid run id"),
        vec![object(json!({"role": "user", "content": "frame"}))],
    )
}

fn tool_call(id: &str, name: &str) -> Value {
    json!({
        "id": id,
        "function": {"name": name, "arguments": "{}"},
    })
}

fn model_completion(message: Value) -> ModelCompletion {
    ModelCompletion {
        message: Some(object(message)),
        finish_reason: None,
        usage: None,
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
        self.requests.lock().expect("requests lock").push(request);
        self.deadlines
            .lock()
            .expect("deadlines lock")
            .push(control.deadline().expires_at());
        let response = self.responses.lock().expect("responses lock").pop_front();
        Box::pin(async move {
            response
                .ok_or_else(|| AgentError::new(AgentErrorCode::ProviderFailed, "script_exhausted"))
        })
    }
}

#[derive(Default)]
struct RecordingDispatcher {
    names: Mutex<Vec<String>>,
    deadlines: Mutex<Vec<Instant>>,
    active: AtomicBool,
    overlapped: AtomicBool,
}

impl ToolDispatcher for RecordingDispatcher {
    type Observation = Value;

    fn dispatch<'a>(
        &'a self,
        invocation: ToolInvocation,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<ToolExecution<Self::Observation>, AgentError>> {
        Box::pin(async move {
            if self.active.swap(true, Ordering::SeqCst) {
                self.overlapped.store(true, Ordering::SeqCst);
            }
            tokio::task::yield_now().await;
            self.names
                .lock()
                .expect("names lock")
                .push(invocation.name.into_inner());
            self.deadlines
                .lock()
                .expect("deadlines lock")
                .push(control.deadline().expires_at());
            self.active.store(false, Ordering::SeqCst);
            Ok(ToolExecution::new(json!({"status": "completed"}), None))
        })
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

fn active_control(signal: &NeverCancelled) -> ExecutionControl<'_> {
    ExecutionControl::new(
        signal,
        Deadline::after(Instant::now(), Duration::from_secs(5)).expect("short deadline"),
    )
}

#[tokio::test]
async fn driver_reuses_one_deadline_and_dispatches_every_call_sequentially() {
    let provider = ScriptedProvider::new(vec![
        model_completion(json!({
            "role": "assistant",
            "content": "",
            "reasoning_content": "act",
            "tool_calls": [tool_call("one", "move_input"), tool_call("two", "look_relative")],
        })),
        model_completion(json!({"role": "assistant", "content": "done"})),
    ]);
    let dispatcher = RecordingDispatcher::default();
    let driver = AgentLoopDriver::new(provider, dispatcher);
    let signal = NeverCancelled;
    let control = active_control(&signal);
    let expected_deadline = control.deadline().expires_at();
    let mut run = initial_run();

    let outcome = driver
        .drive(&mut run, &[fixtures::tool_definition()], control)
        .await
        .expect("loop completes");
    assert_eq!(outcome.closing, "done");
    assert_eq!(
        *driver.tools().names.lock().expect("names lock"),
        ["move_input", "look_relative"]
    );
    assert!(!driver.tools().overlapped.load(Ordering::SeqCst));
    assert!(driver
        .model()
        .deadlines
        .lock()
        .expect("model deadlines")
        .iter()
        .all(|deadline| *deadline == expected_deadline));
    assert!(driver
        .tools()
        .deadlines
        .lock()
        .expect("tool deadlines")
        .iter()
        .all(|deadline| *deadline == expected_deadline));

    let requests = driver.model().requests.lock().expect("requests lock");
    assert_eq!(requests.len(), 2);
    assert!(requests
        .iter()
        .all(|request| request.run_id.as_str() == "driver-run"));
    assert_eq!(requests[0].tools, [fixtures::tool_definition()]);
    let replay = &requests[1].messages;
    assert_eq!(replay[replay.len() - 2]["tool_call_id"], "one");
    assert_eq!(replay[replay.len() - 1]["tool_call_id"], "two");
}

#[derive(Default)]
struct FailingDispatcher;

impl ToolDispatcher for FailingDispatcher {
    type Observation = Value;

    fn dispatch<'a>(
        &'a self,
        invocation: ToolInvocation,
        _control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<ToolExecution<Self::Observation>, AgentError>> {
        match invocation.name.as_str() {
            "panic_tool" => Box::pin(async { panic!("tool panic fixture") }),
            _ => Box::pin(async {
                Err(AgentError::new(
                    AgentErrorCode::ToolFailed,
                    "body_refused_by_backend",
                ))
            }),
        }
    }
}

#[tokio::test]
async fn dispatcher_errors_stay_paired_and_the_loop_continues() {
    let provider = ScriptedProvider::new(vec![
        model_completion(json!({
            "role": "assistant",
            "tool_calls": [tool_call("error-id", "busy_tool"), tool_call("other-id", "busy_tool")],
        })),
        model_completion(json!({"role": "assistant", "content": "done"})),
    ]);
    let driver = AgentLoopDriver::new(provider, FailingDispatcher);
    let signal = NeverCancelled;
    let mut run = initial_run();
    driver
        .drive(&mut run, &[], active_control(&signal))
        .await
        .expect("per-call failures do not abort the run");

    let requests = driver.model().requests.lock().expect("requests lock");
    let replay = &requests[1].messages;
    let first: Value = serde_json::from_str(
        replay[replay.len() - 2]["content"]
            .as_str()
            .expect("first tool content"),
    )
    .expect("first tool JSON");
    let second: Value = serde_json::from_str(
        replay[replay.len() - 1]["content"]
            .as_str()
            .expect("second tool content"),
    )
    .expect("second tool JSON");
    assert_eq!(first["result"]["summary"], "body_refused_by_backend");
    assert_eq!(second["result"]["summary"], "body_refused_by_backend");
}

/// 工具 panic 不再被压成一条普通的工具失败。
///
/// 原先它会变成 `tool_dispatch_panicked` 这个模型可见的失败摘要，与「工具正常
/// 失败」不可区分，于是模型会重试——而 panic 必然可重现，同样的输入再 panic
/// 一次。日志里只有那一句摘要，没有位置也没有栈。
///
/// 现在它照常传播。生产路径上 `process_wake` 的那次 `await` 会把它接成
/// `JoinError`，走失败流 + journal，必要时把 participant 标成 Faulted——比
/// 一条被模型重试的失败结果可查得多。
#[tokio::test]
#[should_panic(expected = "tool panic fixture")]
async fn dispatcher_panic_propagates_instead_of_being_flattened() {
    let provider = ScriptedProvider::new(vec![model_completion(json!({
        "role": "assistant",
        "tool_calls": [tool_call("panic-id", "panic_tool")],
    }))]);
    let driver = AgentLoopDriver::new(provider, FailingDispatcher);
    let signal = NeverCancelled;
    let mut run = initial_run();
    let _ = driver.drive(&mut run, &[], active_control(&signal)).await;
}

struct PendingProvider {
    dropped: Arc<AtomicBool>,
}

impl ModelProvider for PendingProvider {
    type Request = AgentModelRequest;
    type Response = ModelCompletion;

    fn complete<'a>(
        &'a self,
        _request: Self::Request,
        _control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<Self::Response, AgentError>> {
        let guard = DropFlag(Arc::clone(&self.dropped));
        Box::pin(async move {
            let _guard = guard;
            pending().await
        })
    }
}

struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn driver_deadline_drops_a_blocked_provider_future() {
    let dropped = Arc::new(AtomicBool::new(false));
    let driver = AgentLoopDriver::new(
        PendingProvider {
            dropped: Arc::clone(&dropped),
        },
        RecordingDispatcher::default(),
    );
    let signal = NeverCancelled;
    let control = ExecutionControl::new(
        &signal,
        Deadline::after(Instant::now(), Duration::from_millis(20)).expect("short deadline"),
    );
    let mut run = initial_run();
    let result = tokio::time::timeout(Duration::from_secs(1), driver.drive(&mut run, &[], control))
        .await
        .expect("driver enforces its own timer")
        .expect_err("deadline stops the run");
    assert_eq!(result.code, AgentErrorCode::DeadlineExceeded);
    assert!(dropped.load(Ordering::SeqCst));
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

#[tokio::test]
async fn cancellation_wakes_and_drops_a_blocked_provider_future() {
    let dropped = Arc::new(AtomicBool::new(false));
    let driver = AgentLoopDriver::new(
        PendingProvider {
            dropped: Arc::clone(&dropped),
        },
        RecordingDispatcher::default(),
    );
    let signal = ManualCancellation::default();
    let control = ExecutionControl::new(
        &signal,
        Deadline::after(Instant::now(), Duration::from_secs(5)).expect("short deadline"),
    );
    let mut run = initial_run();
    let drive = driver.drive(&mut run, &[], control);
    let cancel = async {
        tokio::task::yield_now().await;
        signal.cancel();
    };
    let (result, ()) = tokio::join!(drive, cancel);
    assert_eq!(
        result.expect_err("cancellation stops the run").code,
        AgentErrorCode::RunCancelled
    );
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn cancellation_precedes_an_already_expired_deadline() {
    let signal = ManualCancellation::default();
    signal.cancel();
    let driver = AgentLoopDriver::new(
        ScriptedProvider::new(Vec::new()),
        RecordingDispatcher::default(),
    );
    let mut run = initial_run();
    let result = driver
        .drive(
            &mut run,
            &[],
            ExecutionControl::new(&signal, Deadline::at(Instant::now())),
        )
        .await
        .expect_err("cancelled and expired control stops immediately");
    assert_eq!(result.code, AgentErrorCode::RunCancelled);
}
