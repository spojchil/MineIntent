use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use mineintent_contracts::{
    agent::{
        AgentError, AgentErrorCode, CancellationSignal, ContractFuture, Deadline, ExecutionControl,
        JsonObject, RunId, ToolCallId, ToolExecution, ToolInvocation, ToolName,
    },
    capability::{
        CapabilityExecutionContext, CapabilityInvocation, ExecutionResource, ScopeGuard,
        ToolDispatcher,
    },
    minecraft::{
        BackendError, BackendEventListener, BackendReady, BackendState, BlockPosition,
        BlockReadResult, BoxFuture, DirectedViewportError, MinecraftBackendApi,
        MinecraftMotorDriverApi, MinecraftSnapshotV1, ObservationEventListener, OperationControl,
        ProtocolEntitySnapshot, ProtocolObservationSource, SelfPose, Subscription,
    },
};
use mineintent_middle::{
    capability::{
        build_production_capability_registry, CapabilityActionIdSource, CapabilityJournal,
        CapabilityScopeAssembly, CapabilityUtcTimestampSource,
        ExplicitCapabilityInvocationAssembler, MemoryStorePort, ObservationAfterSource,
        ProductionCapabilityServices, RegistryToolDispatcher, SpeechSchedulerPort, ViewportReader,
    },
    memory::MemoryError,
    speech::{SpeechRequest, SpeechScheduleError},
};
use serde_json::{json, Value};
use tokio::sync::Notify;

const TIMEOUT: Duration = Duration::from_secs(1);

// This test module uses fakes for middle-layer contract tests only. It does not exercise a real
// Minecraft server or claim Paper-to-Agent end-to-end coverage.

#[derive(Clone)]
struct TestCancellation {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl TestCancellation {
    fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
        }
    }

    fn trigger(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}

impl CancellationSignal for TestCancellation {
    fn cancellation_error(&self) -> Option<AgentError> {
        self.cancelled
            .load(Ordering::SeqCst)
            .then(AgentError::run_cancelled)
    }

    fn cancelled(&self) -> Pin<Box<dyn Future<Output = AgentError> + Send + '_>> {
        let cancelled = Arc::clone(&self.cancelled);
        let notify = Arc::clone(&self.notify);
        Box::pin(async move {
            loop {
                if cancelled.load(Ordering::SeqCst) {
                    return AgentError::run_cancelled();
                }
                notify.notified().await;
            }
        })
    }
}

struct TestScope {
    current: AtomicBool,
}

impl TestScope {
    fn new() -> Self {
        Self {
            current: AtomicBool::new(true),
        }
    }

    fn invalidate(&self) {
        self.current.store(false, Ordering::SeqCst);
    }
}

impl ScopeGuard for TestScope {
    fn check_current(&self) -> Result<(), AgentError> {
        if self.current.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(AgentError::new(
                AgentErrorCode::ScopeInvalid,
                "scope_invalid",
            ))
        }
    }

    fn is_current(&self) -> bool {
        self.current.load(Ordering::SeqCst)
    }
}

#[derive(Default)]
struct RecordingJournal {
    events: Mutex<Vec<(String, JsonObject)>>,
    fail: AtomicBool,
}

impl RecordingJournal {
    fn set_fail(&self, fail: bool) {
        self.fail.store(fail, Ordering::SeqCst);
    }

    fn events(&self) -> Vec<(String, JsonObject)> {
        self.events.lock().expect("journal lock").clone()
    }
}

impl CapabilityJournal for RecordingJournal {
    fn append<'a>(
        &'a self,
        event_type: String,
        payload: JsonObject,
    ) -> ContractFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            if self.fail.load(Ordering::SeqCst) {
                return Err(AgentError::new(
                    AgentErrorCode::ToolFailed,
                    "journal failed\nwith control",
                ));
            }
            self.events
                .lock()
                .expect("journal lock")
                .push((event_type, payload));
            Ok(())
        })
    }
}

#[derive(Default)]
struct FakeSpeech {
    requests: Mutex<Vec<SpeechRequest>>,
    fail: AtomicBool,
}

impl FakeSpeech {
    fn set_fail(&self, fail: bool) {
        self.fail.store(fail, Ordering::SeqCst);
    }

    fn requests(&self) -> Vec<SpeechRequest> {
        self.requests.lock().expect("speech lock").clone()
    }
}

impl SpeechSchedulerPort for FakeSpeech {
    fn schedule(&self, request: SpeechRequest) -> Result<usize, SpeechScheduleError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(SpeechScheduleError::InvalidRequest);
        }
        let segments = request.text.chars().count().div_ceil(256).max(1);
        self.requests.lock().expect("speech lock").push(request);
        Ok(segments)
    }
}

#[derive(Default)]
struct FakeMemory {
    edits: Mutex<Vec<String>>,
    fail: AtomicBool,
}

impl FakeMemory {
    fn set_fail(&self, fail: bool) {
        self.fail.store(fail, Ordering::SeqCst);
    }

    fn record(&self, edit: String) -> Result<(), MemoryError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(MemoryError::AnchorNotUnique { count: 2 });
        }
        self.edits.lock().expect("memory lock").push(edit);
        Ok(())
    }

    fn edits(&self) -> Vec<String> {
        self.edits.lock().expect("memory lock").clone()
    }
}

impl MemoryStorePort for FakeMemory {
    fn append<'a>(&'a self, text: String) -> ContractFuture<'a, Result<(), MemoryError>> {
        Box::pin(async move { self.record(format!("append:{text}")) })
    }

    fn replace<'a>(
        &'a self,
        old_text: String,
        new_text: String,
    ) -> ContractFuture<'a, Result<(), MemoryError>> {
        Box::pin(async move { self.record(format!("replace:{old_text}->{new_text}")) })
    }

    fn rewrite<'a>(&'a self, text: String) -> ContractFuture<'a, Result<(), MemoryError>> {
        Box::pin(async move { self.record(format!("rewrite:{text}")) })
    }
}

struct CountingObservationAfter {
    calls: AtomicUsize,
}

impl CountingObservationAfter {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl ObservationAfterSource for CountingObservationAfter {
    fn observe_after<'a>(
        &'a self,
        _invocation: CapabilityInvocation,
        _resource: ExecutionResource,
        _result: Value,
        context: CapabilityExecutionContext<'a>,
    ) -> ContractFuture<'a, Result<Option<JsonObject>, AgentError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            context.check_at(Instant::now())?;
            Ok(Some(object(json!({"interruption": "fake"}))))
        })
    }
}

struct FixedActionIds;

impl CapabilityActionIdSource for FixedActionIds {
    fn next_action_id(&self, invocation: &ToolInvocation) -> Result<String, AgentError> {
        Ok(format!("action-{}", invocation.tool_call_id))
    }
}

struct FixedUtc;

impl CapabilityUtcTimestampSource for FixedUtc {
    fn now_utc(&self) -> Result<String, AgentError> {
        Ok("2026-08-02T00:00:00.000Z".to_owned())
    }
}

struct FakeMotor {
    pose: Arc<Mutex<SelfPose>>,
    look_fail: AtomicBool,
    move_fail: AtomicBool,
    release_fail: AtomicBool,
    release_calls: AtomicUsize,
    respawn_calls: AtomicUsize,
    calls: AtomicUsize,
    started: Arc<Notify>,
    blocked: AtomicBool,
    unblock: Arc<Notify>,
}

impl FakeMotor {
    fn new(pose: Arc<Mutex<SelfPose>>) -> Self {
        Self {
            pose,
            look_fail: AtomicBool::new(false),
            move_fail: AtomicBool::new(false),
            release_fail: AtomicBool::new(false),
            release_calls: AtomicUsize::new(0),
            respawn_calls: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
            started: Arc::new(Notify::new()),
            blocked: AtomicBool::new(false),
            unblock: Arc::new(Notify::new()),
        }
    }

    fn set_look_fail(&self, value: bool) {
        self.look_fail.store(value, Ordering::SeqCst);
    }

    fn set_move_fail(&self, value: bool) {
        self.move_fail.store(value, Ordering::SeqCst);
    }

    fn set_release_fail(&self, value: bool) {
        self.release_fail.store(value, Ordering::SeqCst);
    }

    fn set_blocked(&self, value: bool) {
        self.blocked.store(value, Ordering::SeqCst);
    }

    fn release_block(&self) {
        self.unblock.notify_waiters();
    }

    fn release_calls(&self) -> usize {
        self.release_calls.load(Ordering::SeqCst)
    }
    fn respawn_calls(&self) -> usize {
        self.respawn_calls.load(Ordering::SeqCst)
    }
}

impl MinecraftMotorDriverApi for FakeMotor {
    fn look_relative(
        &self,
        request: mineintent_contracts::minecraft::LookRelativeRequest,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        let pose = Arc::clone(&self.pose);
        let started = Arc::clone(&self.started);
        let unblock = Arc::clone(&self.unblock);
        let blocked = self.blocked.load(Ordering::SeqCst);
        let fail = self.look_fail.load(Ordering::SeqCst);
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            control.preflight("look_relative")?;
            if blocked {
                started.notify_one();
                unblock.notified().await;
            }
            if fail {
                return Err(backend_failure("look failed"));
            }
            let mut pose = pose.lock().expect("pose lock");
            pose.yaw -= request.yaw_degrees.to_radians();
            pose.pitch -= request.pitch_degrees.to_radians();
            Ok(())
        })
    }

    fn move_input(
        &self,
        request: mineintent_contracts::minecraft::MoveInputRequest,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        let pose = Arc::clone(&self.pose);
        let started = Arc::clone(&self.started);
        let unblock = Arc::clone(&self.unblock);
        let blocked = self.blocked.load(Ordering::SeqCst);
        let fail = self.move_fail.load(Ordering::SeqCst);
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            control.preflight("move_input")?;
            if blocked {
                started.notify_one();
                unblock.notified().await;
            }
            if fail {
                return Err(backend_failure("move failed"));
            }
            let mut pose = pose.lock().expect("pose lock");
            for direction in request.directions {
                match direction {
                    mineintent_contracts::minecraft::MotorMoveDirection::Forward => {
                        pose.position.z -= 0.5
                    }
                    mineintent_contracts::minecraft::MotorMoveDirection::Back => {
                        pose.position.z += 0.5
                    }
                    mineintent_contracts::minecraft::MotorMoveDirection::Left => {
                        pose.position.x -= 0.5
                    }
                    mineintent_contracts::minecraft::MotorMoveDirection::Right => {
                        pose.position.x += 0.5
                    }
                }
            }
            Ok(())
        })
    }

    fn respawn(
        &self,
        _control: mineintent_contracts::minecraft::OperationControl,
    ) -> mineintent_contracts::minecraft::BoxFuture<'_, Result<(), BackendError>> {
        self.respawn_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }

    fn release_all(&self) -> Result<(), BackendError> {
        self.release_calls.fetch_add(1, Ordering::SeqCst);
        if self.release_fail.load(Ordering::SeqCst) {
            Err(backend_failure("release failed"))
        } else {
            Ok(())
        }
    }
}

struct FakeSource {
    pose: Arc<Mutex<SelfPose>>,
}

impl ProtocolObservationSource for FakeSource {
    fn epoch(&self) -> u64 {
        1
    }

    fn self_pose(&self) -> Result<SelfPose, BackendError> {
        Ok(*self.pose.lock().expect("pose lock"))
    }

    fn list_tracked_entities(&self) -> Result<Vec<ProtocolEntitySnapshot>, BackendError> {
        Err(backend_failure("unused entity read"))
    }

    fn read_block(&self, _position: BlockPosition) -> Result<BlockReadResult, BackendError> {
        Err(backend_failure("unused block read"))
    }

    fn subscribe(
        &self,
        _listener: Arc<dyn ObservationEventListener>,
    ) -> Result<Box<dyn Subscription>, BackendError> {
        Err(backend_failure("unused subscription"))
    }

    fn read_viewport(
        &self,
        _control: OperationControl,
    ) -> BoxFuture<'_, Result<mineintent_contracts::minecraft::ViewportRead, BackendError>> {
        Box::pin(async { Err(backend_failure("view failure")) })
    }

    fn read_directed_viewport(
        &self,
        _positions: Vec<BlockPosition>,
        _control: OperationControl,
    ) -> BoxFuture<
        '_,
        Result<mineintent_contracts::minecraft::DirectedViewportProjection, DirectedViewportError>,
    > {
        Box::pin(async {
            Err(DirectedViewportError::Backend(backend_failure(
                "directed view failure",
            )))
        })
    }
}

struct FakeBackend {
    motor: Arc<FakeMotor>,
    source: Arc<FakeSource>,
    motor_calls: AtomicUsize,
    source_calls: AtomicUsize,
}

impl FakeBackend {
    fn new() -> Self {
        let pose = Arc::new(Mutex::new(SelfPose {
            position: mineintent_contracts::minecraft::Vec3Value {
                x: 0.0,
                y: 64.0,
                z: 0.0,
            },
            velocity: mineintent_contracts::minecraft::Vec3Value::default(),
            yaw: 0.0,
            pitch: 0.0,
        }));
        Self {
            motor: Arc::new(FakeMotor::new(Arc::clone(&pose))),
            source: Arc::new(FakeSource { pose }),
            motor_calls: AtomicUsize::new(0),
            source_calls: AtomicUsize::new(0),
        }
    }

    fn motor_calls(&self) -> usize {
        self.motor_calls.load(Ordering::SeqCst)
    }

    fn source_calls(&self) -> usize {
        self.source_calls.load(Ordering::SeqCst)
    }
}

impl MinecraftBackendApi for FakeBackend {
    fn start(
        &self,
        _control: OperationControl,
    ) -> BoxFuture<'_, Result<BackendReady, BackendError>> {
        Box::pin(async { Err(backend_failure("unused start")) })
    }

    fn stop(
        &self,
        _reason: String,
        _control: OperationControl,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async { Err(backend_failure("unused stop")) })
    }

    fn state(&self) -> BackendState {
        BackendState::Idle
    }

    fn snapshot(&self) -> Result<MinecraftSnapshotV1, BackendError> {
        Err(backend_failure("unused snapshot"))
    }

    fn subscribe(
        &self,
        _listener: Arc<dyn BackendEventListener>,
    ) -> Result<Box<dyn Subscription>, BackendError> {
        Err(backend_failure("unused subscription"))
    }

    fn observation_source(&self) -> Result<Arc<dyn ProtocolObservationSource>, BackendError> {
        self.source_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::clone(&self.source) as Arc<dyn ProtocolObservationSource>)
    }

    fn motor(&self) -> Result<Arc<dyn MinecraftMotorDriverApi>, BackendError> {
        self.motor_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::clone(&self.motor) as Arc<dyn MinecraftMotorDriverApi>)
    }

    fn send_chat(&self, _message: String) -> Result<(), BackendError> {
        Err(backend_failure("unused chat"))
    }
}

fn backend_failure(message: &str) -> BackendError {
    BackendError::BackendFailure {
        failure: mineintent_contracts::minecraft::BackendFailure {
            code: mineintent_contracts::minecraft::BackendFailureCode::ProtocolError,
            message: message.to_owned(),
            retryable: false,
        },
    }
}

struct Harness {
    dispatcher: RegistryToolDispatcher,
    backend: Arc<FakeBackend>,
    journal: Arc<RecordingJournal>,
    speech: Arc<FakeSpeech>,
    memory: Arc<FakeMemory>,
    scope: Arc<TestScope>,
}

fn harness() -> Harness {
    let backend = Arc::new(FakeBackend::new());
    let journal = Arc::new(RecordingJournal::default());
    let speech = Arc::new(FakeSpeech::default());
    let memory = Arc::new(FakeMemory::default());
    let backend_api: Arc<dyn MinecraftBackendApi> = backend.clone();
    let journal_api: Arc<dyn CapabilityJournal> = journal.clone();
    let speech_api: Arc<dyn SpeechSchedulerPort> = speech.clone();
    let memory_api: Arc<dyn MemoryStorePort> = memory.clone();
    let services = ProductionCapabilityServices::new(
        backend_api.clone(),
        Arc::new(ViewportReader::new(backend_api)),
        journal_api,
        speech_api,
        memory_api,
    );
    let registry = build_production_capability_registry(services).expect("production registry");
    let scope = Arc::new(TestScope::new());
    let scope_guard: Arc<dyn ScopeGuard> = scope.clone();
    let assembler = Arc::new(ExplicitCapabilityInvocationAssembler::new(
        Arc::new(FixedActionIds),
        Arc::new(FixedUtc),
    ));
    let scope_assembly = Arc::new(CapabilityScopeAssembly::new(
        "world-1",
        "chat-1",
        scope_guard,
    ));
    let dispatcher = RegistryToolDispatcher::new(registry, assembler, scope_assembly);
    Harness {
        dispatcher,
        backend,
        journal,
        speech,
        memory,
        scope,
    }
}

fn object(value: Value) -> JsonObject {
    value.as_object().cloned().expect("object fixture")
}

fn tool_invocation(name: &str, arguments: Value) -> ToolInvocation {
    ToolInvocation {
        run_id: RunId::new("run-b4").expect("run id"),
        tool_call_id: ToolCallId::new(format!("call-{name}")).expect("call id"),
        name: ToolName::new(name).expect("tool name"),
        arguments: object(arguments),
    }
}

fn active_control(cancellation: &TestCancellation) -> ExecutionControl<'_> {
    ExecutionControl::new(
        cancellation,
        Deadline::after(Instant::now(), Duration::from_secs(5)).expect("deadline"),
    )
}

async fn dispatch(
    dispatcher: &RegistryToolDispatcher,
    cancellation: &TestCancellation,
    name: &str,
    arguments: Value,
) -> Result<ToolExecution<JsonObject>, AgentError> {
    dispatcher
        .dispatch(
            tool_invocation(name, arguments),
            active_control(cancellation),
        )
        .await
}

#[tokio::test]
async fn production_registry_definitions_and_dispatch_share_ordered_registry() {
    let harness = harness();
    let names: Vec<_> = harness
        .dispatcher
        .registry()
        .definitions()
        .into_iter()
        .map(|definition| definition.function.name.into_inner())
        .collect();
    assert_eq!(
        names,
        [
            "look_relative",
            "move_input",
            "respawn",
            "view",
            "say",
            "remember"
        ]
    );
    for name in names {
        let invocation = tool_invocation(name.as_str(), json!({}));
        assert!(harness
            .dispatcher
            .registry()
            .resolve(name.as_str())
            .is_some());
        assert_eq!(
            harness.dispatcher.resource(&invocation),
            match name.as_str() {
                "look_relative" | "move_input" | "respawn" => Some(ExecutionResource::Body),
                "view" => Some(ExecutionResource::Viewport),
                "say" => Some(ExecutionResource::Chat),
                "remember" => Some(ExecutionResource::Memory),
                _ => None,
            }
        );
    }

    let cancellation = TestCancellation::new();
    let say = dispatch(
        &harness.dispatcher,
        &cancellation,
        "say",
        json!({"text": "hello"}),
    )
    .await
    .expect("registry-resolved say dispatches");
    assert_eq!(say.result["status"], "queued");
    assert_eq!(harness.speech.requests().len(), 1);
}

#[tokio::test]
async fn strict_arguments_have_zero_backend_or_port_side_effects() {
    let harness = harness();
    let cancellation = TestCancellation::new();
    for (name, arguments) in [
        (
            "look_relative",
            json!({"yaw_degrees": 91, "pitch_degrees": 0}),
        ),
        (
            "move_input",
            json!({"directions": ["forward", "forward"], "duration_ms": 50}),
        ),
        ("say", json!({"text": "   "})),
        (
            "remember",
            json!({"operation": "append", "text": "x", "newText": "mixed"}),
        ),
    ] {
        let execution = dispatch(&harness.dispatcher, &cancellation, name, arguments)
            .await
            .expect("invalid arguments remain paired ordinary failures");
        assert_eq!(execution.result["status"], "failed", "{name}");
    }
    assert_eq!(harness.backend.motor_calls(), 0);
    assert_eq!(harness.backend.source_calls(), 0);
    assert!(harness.speech.requests().is_empty());
    assert!(harness.memory.edits().is_empty());
    assert!(harness.journal.events().is_empty());
}

#[tokio::test]
async fn body_success_measures_effect_releases_lease_and_injects_one_observation() {
    let harness = harness();
    let observation = Arc::new(CountingObservationAfter::new());
    let dispatcher = harness
        .dispatcher
        .with_observation_after(Arc::clone(&observation) as Arc<dyn ObservationAfterSource>);
    let cancellation = TestCancellation::new();

    let execution = dispatch(
        &dispatcher,
        &cancellation,
        "look_relative",
        json!({"yaw_degrees": 15, "pitch_degrees": -5}),
    )
    .await
    .expect("look completes");
    assert_eq!(execution.result["protocol"], "mineintent.tool-result.v1");
    assert_eq!(execution.result["status"], "completed");
    assert!(execution.result.get("viewport").is_none());
    assert!(
        (execution.result["effect"]["relativeTurnDegrees"]["yaw"]
            .as_f64()
            .unwrap()
            - 15.0)
            .abs()
            < 0.000_001
    );
    assert!(
        (execution.result["effect"]["relativeTurnDegrees"]["pitch"]
            .as_f64()
            .unwrap()
            + 5.0)
            .abs()
            < 0.000_001
    );
    assert_eq!(observation.calls(), 1);
    assert!(execution.observation_after.as_ref().is_some());
    assert_eq!(harness.backend.motor.release_calls(), 1);
    let events = harness.journal.events();
    assert_eq!(events[0].0, "body_tool.completed");
    assert_eq!(events[0].1["actionId"], "action-call-look_relative");
    assert_eq!(events[0].1["startedAt"], "2026-08-02T00:00:00.000Z");
}

#[tokio::test]
async fn move_input_success_maps_forward_and_right_to_body_relative_effect() {
    let harness = harness();
    let observation = Arc::new(CountingObservationAfter::new());
    let dispatcher = harness
        .dispatcher
        .with_observation_after(Arc::clone(&observation) as Arc<dyn ObservationAfterSource>);
    let cancellation = TestCancellation::new();
    let execution = dispatch(
        &dispatcher,
        &cancellation,
        "move_input",
        json!({
            "directions": ["forward", "right"],
            "duration_ms": 50,
            "sprint": true
        }),
    )
    .await
    .expect("move completes");
    assert_eq!(execution.result["protocol"], "mineintent.tool-result.v1");
    assert_eq!(execution.result["status"], "completed");
    assert!(execution.result.get("viewport").is_none());
    assert_eq!(
        execution.result["effect"]["coordinates"],
        "body_relative_before_move"
    );
    let displacement = execution.result["effect"]["relativeDisplacement"]
        .as_array()
        .expect("relative displacement");
    assert!((displacement[0].as_f64().unwrap() - 0.5).abs() < 0.000_001);
    assert!(displacement[1].as_f64().unwrap().abs() < 0.000_001);
    assert!((displacement[2].as_f64().unwrap() - 0.5).abs() < 0.000_001);
    assert_eq!(execution.result["effect"]["movement"], "changed");
    assert_eq!(observation.calls(), 1);
    assert!(execution.observation_after.as_ref().is_some());
    assert_eq!(harness.backend.motor.release_calls(), 1);
}

#[tokio::test]
async fn body_ordinary_failure_still_injects_observation_and_releases_lease() {
    let harness = harness();
    harness.backend.motor.set_move_fail(true);
    let observation = Arc::new(CountingObservationAfter::new());
    let dispatcher = harness
        .dispatcher
        .with_observation_after(Arc::clone(&observation) as Arc<dyn ObservationAfterSource>);
    let cancellation = TestCancellation::new();

    let execution = dispatch(
        &dispatcher,
        &cancellation,
        "move_input",
        json!({"directions": ["forward"], "duration_ms": 50}),
    )
    .await
    .expect("ordinary body failure is paired");
    assert_eq!(execution.result["protocol"], "mineintent.tool-result.v1");
    assert_eq!(execution.result["status"], "failed");
    assert!(
        execution.result["summary"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 300
    );
    assert_eq!(observation.calls(), 1);
    assert!(execution.observation_after.as_ref().is_some());
    assert_eq!(harness.backend.motor.release_calls(), 1);
}

#[tokio::test]
async fn non_body_capabilities_never_call_observation_after_and_return_null() {
    let harness = harness();
    let observation = Arc::new(CountingObservationAfter::new());
    let dispatcher = harness
        .dispatcher
        .with_observation_after(Arc::clone(&observation) as Arc<dyn ObservationAfterSource>);
    let cancellation = TestCancellation::new();

    for (name, arguments) in [
        ("view", json!({"mode": "full"})),
        ("say", json!({"text": "hello"})),
        (
            "remember",
            json!({"operation": "append", "text": "keep this"}),
        ),
    ] {
        let execution = dispatch(&dispatcher, &cancellation, name, arguments)
            .await
            .expect("non-body capability returns paired result");
        assert!(execution.observation_after.as_ref().is_none(), "{name}");
    }
    assert_eq!(observation.calls(), 0);
}

#[tokio::test]
async fn say_stays_queued_when_normal_journal_append_fails() {
    let harness = harness();
    harness.journal.set_fail(true);
    let cancellation = TestCancellation::new();
    let execution = dispatch(
        &harness.dispatcher,
        &cancellation,
        "say",
        json!({"text": "already queued"}),
    )
    .await
    .expect("normal journal failure does not retract speech");
    assert_eq!(execution.result["protocol"], "mineintent.tool-result.v1");
    assert_eq!(execution.result["status"], "queued");
    assert_eq!(harness.speech.requests().len(), 1);
}

#[tokio::test]
async fn remember_success_is_minimal_and_journal_redacts_all_memory_text() {
    let harness = harness();
    let cancellation = TestCancellation::new();
    for arguments in [
        json!({"operation": "append", "text": "append text"}),
        json!({"operation": "replace", "oldText": "old", "newText": ""}),
        json!({"operation": "rewrite", "text": "new full text"}),
    ] {
        let execution = dispatch(&harness.dispatcher, &cancellation, "remember", arguments)
            .await
            .expect("memory edit completes");
        assert_eq!(
            execution.result,
            json!({
                "protocol": "mineintent.tool-result.v1",
                "status": "completed"
            })
        );
        assert!(execution.observation_after.as_ref().is_none());
    }
    assert_eq!(
        harness.memory.edits(),
        [
            "append:append text",
            "replace:old->",
            "rewrite:new full text"
        ]
    );
    for (_, payload) in harness.journal.events() {
        assert!(payload.get("text").is_none());
        assert!(payload.get("oldText").is_none());
        assert!(payload.get("newText").is_none());
    }
}

#[tokio::test]
async fn ordinary_failure_is_paired_for_every_production_capability() {
    let harness = harness();
    let cancellation = TestCancellation::new();

    harness.backend.motor.set_look_fail(true);
    let look = dispatch(
        &harness.dispatcher,
        &cancellation,
        "look_relative",
        json!({"yaw_degrees": 1, "pitch_degrees": 0}),
    )
    .await
    .expect("look failure paired");
    assert_eq!(look.result["status"], "failed");

    harness.backend.motor.set_move_fail(true);
    let move_result = dispatch(
        &harness.dispatcher,
        &cancellation,
        "move_input",
        json!({"directions": ["forward"], "duration_ms": 50}),
    )
    .await
    .expect("move failure paired");
    assert_eq!(move_result.result["status"], "failed");

    let view = dispatch(
        &harness.dispatcher,
        &cancellation,
        "view",
        json!({"mode": "full"}),
    )
    .await
    .expect("view failure paired");
    assert_eq!(view.result["status"], "failed");

    harness.speech.set_fail(true);
    let say = dispatch(
        &harness.dispatcher,
        &cancellation,
        "say",
        json!({"text": "speech failure"}),
    )
    .await
    .expect("say failure paired");
    assert_eq!(say.result["status"], "failed");

    harness.memory.set_fail(true);
    let remember = dispatch(
        &harness.dispatcher,
        &cancellation,
        "remember",
        json!({"operation": "append", "text": "memory failure"}),
    )
    .await
    .expect("memory failure paired");
    assert_eq!(remember.result["status"], "failed");
}

#[tokio::test]
async fn cleanup_failure_does_not_keep_body_tool_gate_occupied() {
    let harness = harness();
    harness.backend.motor.set_release_fail(true);
    let cancellation = TestCancellation::new();
    let execution = dispatch(
        &harness.dispatcher,
        &cancellation,
        "look_relative",
        json!({"yaw_degrees": 0, "pitch_degrees": 0}),
    )
    .await
    .expect("cleanup failure is best effort");
    assert_eq!(execution.result["status"], "completed");
    assert_eq!(harness.backend.motor.release_calls(), 1);
}

/// 原来这里还有半个断言：第二次派发会拿到 `resource_busy`。那半个连同仲裁器
/// 一起删了——它只有在测试自己 `tokio::spawn` 造出第二条并发派发时才成立，而
/// 生产侧的 `dispatch_in_order` 是顺序 for 循环，永远造不出来。剩下的这半个是
/// 真的：马达卡住时取消一次身体动作，取消要能穿出来。
#[tokio::test]
async fn a_blocked_body_call_still_propagates_cancellation() {
    let harness = harness();
    harness.backend.motor.set_blocked(true);
    let cancellation = TestCancellation::new();
    let dispatcher = Arc::new(harness.dispatcher);
    let signal_for_task = cancellation.clone();
    let dispatcher_for_task = Arc::clone(&dispatcher);
    let call = tokio::spawn(async move {
        dispatcher_for_task
            .dispatch(
                tool_invocation(
                    "look_relative",
                    json!({"yaw_degrees": 1, "pitch_degrees": 0}),
                ),
                active_control(&signal_for_task),
            )
            .await
    });
    tokio::time::timeout(TIMEOUT, harness.backend.motor.started.notified())
        .await
        .expect("body call starts");

    cancellation.trigger();
    let result = tokio::time::timeout(TIMEOUT, call)
        .await
        .expect("cancelled call completes")
        .expect("task joins");
    assert_eq!(
        result.expect_err("cancelled body propagates").code,
        AgentErrorCode::RunCancelled
    );
    harness.backend.motor.release_block();
}

#[tokio::test]
async fn unknown_cancelled_deadline_and_invalid_scope_remain_structured_dispatch_errors() {
    let harness = harness();
    let cancellation = TestCancellation::new();
    let unknown = harness
        .dispatcher
        .dispatch(
            tool_invocation("missing", json!({})),
            active_control(&cancellation),
        )
        .await
        .expect_err("unknown tool");
    assert_eq!(unknown.code, AgentErrorCode::UnknownTool);

    cancellation.trigger();
    let cancelled = harness
        .dispatcher
        .dispatch(
            tool_invocation(
                "look_relative",
                json!({"yaw_degrees": 0, "pitch_degrees": 0}),
            ),
            active_control(&cancellation),
        )
        .await
        .expect_err("cancelled dispatch");
    assert_eq!(cancelled.code, AgentErrorCode::RunCancelled);

    let fresh = TestCancellation::new();
    let expired_control = ExecutionControl::new(
        &fresh,
        Deadline::at(
            Instant::now()
                .checked_sub(Duration::from_millis(1))
                .unwrap(),
        ),
    );
    let deadline = harness
        .dispatcher
        .dispatch(
            tool_invocation(
                "look_relative",
                json!({"yaw_degrees": 0, "pitch_degrees": 0}),
            ),
            expired_control,
        )
        .await
        .expect_err("expired dispatch");
    assert_eq!(deadline.code, AgentErrorCode::DeadlineExceeded);

    let fresh = TestCancellation::new();
    harness.scope.invalidate();
    let scope = harness
        .dispatcher
        .dispatch(
            tool_invocation(
                "look_relative",
                json!({"yaw_degrees": 0, "pitch_degrees": 0}),
            ),
            active_control(&fresh),
        )
        .await
        .expect_err("invalid scope dispatch");
    assert_eq!(scope.code, AgentErrorCode::ScopeInvalid);
    assert_eq!(harness.backend.motor_calls(), 0);
}

/// 产品裁定（2026-08-04 维护者，方案 1）：重生是同伴对自身处境的处置权，
/// 由它自己决定何时调用。本回归钉住三件事：工具在生产 registry 里、
/// 占身体资源（与转向/移动互斥）、完成语义只是「请求已派发」。
#[tokio::test]
async fn respawn_is_a_body_tool_that_reports_request_dispatched_only() {
    let harness = harness();
    let cancellation = TestCancellation::new();
    let invocation = tool_invocation("respawn", json!({}));
    assert_eq!(
        harness.dispatcher.resource(&invocation),
        Some(ExecutionResource::Body),
        "重生与转向/移动共用身体资源，必须互斥"
    );

    let execution = dispatch(&harness.dispatcher, &cancellation, "respawn", json!({}))
        .await
        .expect("respawn dispatches");
    assert_eq!(execution.result["status"], "completed");
    assert_eq!(
        execution.result["effect"]["requested"], true,
        "完成只表示请求已派发，复活是随后的生命周期事实"
    );
    assert_eq!(harness.backend.motor.respawn_calls(), 1);
}
