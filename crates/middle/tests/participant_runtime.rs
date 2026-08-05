use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    time::Duration,
};

use mineintent_contracts::{
    agent::{
        fixtures, AgentError, AgentErrorCode, AgentHotbarV5, AgentItemStackV5, AgentRunRequest,
        AgentRunner, AgentStatusV5, ContractFuture, Deadline, ExecutionControl,
        JsonAgentDecisionContextV5, ModelProvider, ModelRunResult, ModelUsage, RunId, ToolCallId,
        ToolInvocation, WireToolDefinition,
    },
    capability::ToolCapabilityRegistry,
    capability::{CapabilityExecutionContext, CapabilityInvocation, ExecutionResource, ScopeGuard},
    information::InformationOmission,
    minecraft::{
        BackendClose, BackendError, BackendEventEnvelope, BackendEventKind, BackendEventListener,
        BackendEventMetadata, BackendFailure, BackendFailureCode, BackendLifecyclePayload,
        BackendReady, BackendState, FactSource, LookRelativeRequest, MinecraftBackendApi,
        MinecraftMotorDriverApi, MinecraftSnapshotV1, MoveInputRequest, OperationControl,
        ProtocolChatEvent, ProtocolObservationSource, Subscription,
    },
};
use tokio::sync::watch;

use mineintent_middle::{
    agent::{
        AgentChatInputV5, AgentContextV5EventInput, AgentModelRequest, ConcreteAgentRunner,
        ModelCompletion,
    },
    capability::{
        build_production_capability_registry, CapabilityActionIdSource, CapabilityJournal,
        CapabilityScopeAssembly, CapabilityUtcTimestampSource,
        ExplicitCapabilityInvocationAssembler, MemoryStorePort, ObservationAfterSource,
        ProductionCapabilityServices, RegistryToolDispatcher, SpeechSchedulerPort, ViewportReader,
    },
    memory::MemoryError,
    participant::{
        ParticipantAdmission, ParticipantAdmissionObserver, ParticipantAgentAssembly,
        ParticipantAgentFactory, ParticipantAgentPort, ParticipantClock, ParticipantEvent,
        ParticipantFrameCapture, ParticipantFrameSource, ParticipantInternalEvent,
        ParticipantMemorySource, ParticipantObservationAfterSource, ParticipantRegistryBound,
        ParticipantRuntime, ParticipantRuntimeConfig, ParticipantRuntimeError, ParticipantScope,
        ParticipantScopedAgentRunner, ParticipantSourceError, ParticipantSpeechControl, WakeKind,
        WakeRegistry, WakeRule, WakeRuleCondition,
    },
    speech::{ChatInputContext, PlayerChatMessage, SpeechRequest, SpeechScheduleError},
    telemetry::DebugStateStore,
};

struct TestAgent {
    requests: Arc<Mutex<Vec<AgentRunRequest<JsonAgentDecisionContextV5>>>>,
    run_count: watch::Sender<usize>,
    hold_runs: AtomicUsize,
    run_index: AtomicUsize,
    release: watch::Sender<bool>,
    fail: AtomicBool,
}

impl TestAgent {
    fn new(hold_runs: usize) -> Arc<Self> {
        let (run_count, _) = watch::channel(0_usize);
        let (release, _) = watch::channel(false);
        Arc::new(Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            run_count,
            hold_runs: AtomicUsize::new(hold_runs),
            run_index: AtomicUsize::new(0),
            release,
            fail: AtomicBool::new(false),
        })
    }

    async fn wait_for_runs(&self, expected: usize) {
        let mut receiver = self.run_count.subscribe();
        loop {
            if *receiver.borrow() >= expected {
                return;
            }
            receiver.changed().await.expect("test agent remains alive");
        }
    }

    fn release(&self) {
        self.release.send_replace(true);
    }

    fn texts(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| {
                request
                    .context
                    .frame
                    .events
                    .as_ref()
                    .and_then(|events| {
                        events.iter().find_map(|event| match event {
                            mineintent_contracts::agent::AgentEventV5::PlayerChat(message) => {
                                Some(message.text.clone())
                            }
                            mineintent_contracts::agent::AgentEventV5::Summary { .. } => None,
                        })
                    })
                    .expect("each test run is triggered by chat")
            })
            .collect()
    }
}

impl ParticipantAgentPort for TestAgent {
    fn definitions(&self) -> Vec<WireToolDefinition> {
        Vec::new()
    }

    fn run<'a>(
        &'a self,
        _scope: ParticipantScope,
        _generation: u64,
        _trigger_event_id: String,
        request: AgentRunRequest<JsonAgentDecisionContextV5>,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<ModelRunResult, AgentError>> {
        let requests = Arc::clone(&self.requests);
        let run_count = self.run_count.clone();
        let release = self.release.clone();
        let hold_runs = self.hold_runs.load(Ordering::SeqCst);
        let index = self.run_index.fetch_add(1, Ordering::SeqCst) + 1;
        let fail = self.fail.load(Ordering::SeqCst);
        Box::pin(async move {
            let request_count = {
                let mut requests = requests.lock().unwrap();
                requests.push(request);
                index
            };
            run_count.send_replace(request_count);
            if fail {
                return Err(AgentError::new(
                    AgentErrorCode::ProviderFailed,
                    "provider failure",
                ));
            }
            if index <= hold_runs {
                let mut receiver = release.subscribe();
                loop {
                    if *receiver.borrow() {
                        break;
                    }
                    tokio::select! {
                        error = control.cancelled() => return Err(error),
                        changed = receiver.changed() => {
                            changed.expect("test release remains alive");
                        }
                    }
                }
            }
            Ok(fixtures::model_run_result())
        })
    }
}

struct TestMemory;

impl ParticipantMemorySource for TestMemory {
    fn read_full<'a>(&'a self) -> ContractFuture<'a, Result<String, String>> {
        Box::pin(async { Ok("memory fact".to_owned()) })
    }
}

struct TestClock;

impl ParticipantClock for TestClock {
    fn now(&self) -> String {
        "2026-08-03T00:00:00Z".to_owned()
    }
}

struct TestJournal {
    entries: Arc<Mutex<Vec<String>>>,
    payloads: Arc<Mutex<Vec<mineintent_contracts::agent::JsonObject>>>,
    count: watch::Sender<usize>,
    gate: watch::Sender<bool>,
    step_after: AtomicUsize,
    step_release: watch::Sender<usize>,
}

impl TestJournal {
    fn new() -> Arc<Self> {
        let (count, _) = watch::channel(0_usize);
        let (gate, _) = watch::channel(false);
        let (step_release, _) = watch::channel(0_usize);
        Arc::new(Self {
            entries: Arc::new(Mutex::new(Vec::new())),
            payloads: Arc::new(Mutex::new(Vec::new())),
            count,
            gate,
            step_after: AtomicUsize::new(0),
            step_release,
        })
    }

    fn set_gate(&self, gated: bool) {
        self.gate.send_replace(gated);
    }

    fn payloads(&self) -> Vec<mineintent_contracts::agent::JsonObject> {
        self.payloads.lock().unwrap().clone()
    }

    async fn wait_for_entries(&self, expected: usize) {
        let mut receiver = self.count.subscribe();
        loop {
            if *receiver.borrow() >= expected {
                return;
            }
            receiver
                .changed()
                .await
                .expect("test journal remains alive");
        }
    }

    async fn wait_for_payload_ids(&self, expected: &[&str]) {
        let mut receiver = self.count.subscribe();
        loop {
            let present = {
                let payloads = self.payloads.lock().unwrap();
                expected.iter().all(|expected_id| {
                    payloads.iter().any(|payload| {
                        payload.get("id").and_then(serde_json::Value::as_str) == Some(*expected_id)
                    })
                })
            };
            if present {
                return;
            }
            receiver
                .changed()
                .await
                .expect("test journal remains alive");
        }
    }
}

impl CapabilityJournal for TestJournal {
    fn append<'a>(
        &'a self,
        event_type: String,
        payload: mineintent_contracts::agent::JsonObject,
    ) -> ContractFuture<'a, Result<(), AgentError>> {
        let entries = Arc::clone(&self.entries);
        let payloads = Arc::clone(&self.payloads);
        let count = self.count.clone();
        let gate = self.gate.clone();
        let step_after = &self.step_after;
        let step_release = self.step_release.clone();
        Box::pin(async move {
            let length = {
                let mut entries = entries.lock().unwrap();
                entries.push(event_type);
                entries.len()
            };
            payloads.lock().unwrap().push(payload);
            count.send_replace(length);
            let mut receiver = gate.subscribe();
            loop {
                if !*receiver.borrow() {
                    break;
                }
                receiver
                    .changed()
                    .await
                    .expect("test journal gate remains alive");
            }
            let step_after = step_after.load(Ordering::SeqCst);
            if step_after != 0 && length > step_after {
                let mut receiver = step_release.subscribe();
                loop {
                    if *receiver.borrow() >= length {
                        break;
                    }
                    receiver
                        .changed()
                        .await
                        .expect("test journal step gate remains alive");
                }
            }
            Ok(())
        })
    }
}

struct TestSpeech {
    cancelled: AtomicUsize,
    cleanup_gate: Mutex<Option<Arc<CleanupGate>>>,
}

impl TestSpeech {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            cancelled: AtomicUsize::new(0),
            cleanup_gate: Mutex::new(None),
        })
    }

    fn gate_cleanup(&self) -> Arc<CleanupGate> {
        let gate = CleanupGate::new();
        *self.cleanup_gate.lock().unwrap() = Some(Arc::clone(&gate));
        gate
    }
}

struct CleanupGate {
    started: watch::Sender<bool>,
    released: Mutex<bool>,
    condition: Condvar,
}

impl CleanupGate {
    fn new() -> Arc<Self> {
        let (started, _) = watch::channel(false);
        Arc::new(Self {
            started,
            released: Mutex::new(false),
            condition: Condvar::new(),
        })
    }

    async fn wait_started(&self) {
        let mut receiver = self.started.subscribe();
        loop {
            if *receiver.borrow() {
                return;
            }
            receiver
                .changed()
                .await
                .expect("cleanup gate remains alive");
        }
    }

    fn enter(&self) {
        self.started.send_replace(true);
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.condition.wait(released).unwrap();
        }
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.condition.notify_all();
    }
}

impl SpeechSchedulerPort for TestSpeech {
    fn schedule(&self, _request: SpeechRequest) -> Result<usize, SpeechScheduleError> {
        panic!("speech is only observed for cancellation in this runtime test")
    }
}

impl ParticipantSpeechControl for TestSpeech {
    fn cancel_remaining(&self, _reason: &str) {
        self.cancelled.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = self.cleanup_gate.lock().unwrap().clone() {
            gate.enter();
        }
    }
}

struct TestFrameSource {
    context: ChatInputContext,
    capture: Mutex<ParticipantFrameCapture>,
    chats: Mutex<Vec<AgentChatInputV5>>,
    capture_calls: AtomicUsize,
    capture_gate: Mutex<Option<Arc<CleanupGate>>>,
    missing_light: AtomicBool,
    fail_context: AtomicBool,
    retain_calls: AtomicUsize,
    release_calls: AtomicUsize,
    release_all_calls: AtomicUsize,
    retained_count: AtomicUsize,
}

struct TestAdmissionObserver {
    gate: Arc<CleanupGate>,
}

impl ParticipantAdmissionObserver for TestAdmissionObserver {
    fn after_work_admitted_before_fact(&self, _event_type: &str) {
        self.gate.enter();
    }
}

impl TestFrameSource {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            context: ChatInputContext {
                participant_username: "Bot".to_owned(),
                online_player_usernames: vec![
                    "Bot".to_owned(),
                    "Alice".to_owned(),
                    "Bob".to_owned(),
                ],
                conversation_active_with: None,
            },
            capture: Mutex::new(ParticipantFrameCapture {
                at: "2026-08-03T00:00:00Z".to_owned(),
                dimension: "minecraft:overworld".to_owned(),
                pose: mineintent_contracts::agent::AgentPoseV5 {
                    position: [1.0, 64.0, 2.0],
                    yaw_degrees: 10.0,
                    pitch_degrees: 2.0,
                },
                status: Some(AgentStatusV5 {
                    health: 20.0,
                    food: 20.0,
                    armor: Some(0),
                }),
                hotbar: AgentHotbarV5 {
                    selected: 0,
                    slots: BTreeMap::from([(0, AgentItemStackV5::new("stone", 1).unwrap())]),
                    off_hand: None,
                },
                unread_chat: Vec::new(),
                unread_chat_omitted: 0,
                sound: None,
                light: Some(7),
                events: Vec::new(),
                omissions: Vec::<InformationOmission>::new(),
            }),
            chats: Mutex::new(Vec::new()),
            capture_calls: AtomicUsize::new(0),
            capture_gate: Mutex::new(None),
            missing_light: AtomicBool::new(false),
            fail_context: AtomicBool::new(false),
            retain_calls: AtomicUsize::new(0),
            release_calls: AtomicUsize::new(0),
            release_all_calls: AtomicUsize::new(0),
            retained_count: AtomicUsize::new(0),
        })
    }

    fn set_chats(&self, chats: Vec<AgentChatInputV5>) {
        *self.chats.lock().unwrap() = chats;
    }

    fn set_armor(&self, armor: Option<u8>) {
        self.capture.lock().unwrap().status.as_mut().unwrap().armor = armor;
    }

    fn gate_capture(&self) -> Arc<CleanupGate> {
        let gate = CleanupGate::new();
        *self.capture_gate.lock().unwrap() = Some(Arc::clone(&gate));
        gate
    }

    fn capture_calls(&self) -> usize {
        self.capture_calls.load(Ordering::SeqCst)
    }

    fn retain_calls(&self) -> usize {
        self.retain_calls.load(Ordering::SeqCst)
    }

    fn release_calls(&self) -> usize {
        self.release_calls.load(Ordering::SeqCst)
    }

    fn release_all_calls(&self) -> usize {
        self.release_all_calls.load(Ordering::SeqCst)
    }

    fn retained_count(&self) -> usize {
        self.retained_count.load(Ordering::SeqCst)
    }
}

impl ParticipantFrameSource for TestFrameSource {
    fn chat_context(
        &self,
        _scope: &ParticipantScope,
    ) -> Result<ChatInputContext, ParticipantSourceError> {
        if self.fail_context.load(Ordering::SeqCst) {
            return Err(ParticipantSourceError::Failed(
                "chat secret must not leak".to_owned(),
            ));
        }
        Ok(self.context.clone())
    }

    fn capture(
        &self,
        scope: &ParticipantScope,
    ) -> Result<ParticipantFrameCapture, ParticipantSourceError> {
        self.capture_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = self.capture_gate.lock().unwrap().clone() {
            gate.enter();
        }
        let mut capture = self.capture.lock().unwrap().clone();
        capture.dimension = scope
            .dimension
            .clone()
            .ok_or_else(|| ParticipantSourceError::Invalid("missing dimension".to_owned()))?;
        capture.unread_chat = self.chats.lock().unwrap().clone();
        if self.missing_light.load(Ordering::SeqCst) {
            capture.light = None;
        }
        Ok(capture)
    }

    fn retain_trigger(
        &self,
        _scope: &ParticipantScope,
        _trigger: &PlayerChatMessage,
    ) -> Result<(), ParticipantSourceError> {
        self.retain_calls.fetch_add(1, Ordering::SeqCst);
        self.retained_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn release_trigger(&self, _scope: &ParticipantScope, _trigger: &PlayerChatMessage) {
        let mut current = self.retained_count.load(Ordering::SeqCst);
        while current > 0 {
            match self.retained_count.compare_exchange(
                current,
                current - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    self.release_calls.fetch_add(1, Ordering::SeqCst);
                    break;
                }
                Err(actual) => current = actual,
            }
        }
    }

    fn release_retained_triggers(&self) {
        self.release_all_calls.fetch_add(1, Ordering::SeqCst);
        let retained = self.retained_count.swap(0, Ordering::SeqCst);
        self.release_calls.fetch_add(retained, Ordering::SeqCst);
    }
}

struct TestMotor {
    releases: AtomicUsize,
    respawn_calls: AtomicUsize,
}

impl TestMotor {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            releases: AtomicUsize::new(0),
            respawn_calls: AtomicUsize::new(0),
        })
    }
}

impl MinecraftMotorDriverApi for TestMotor {
    fn look_relative(
        &self,
        _request: LookRelativeRequest,
        _control: OperationControl,
    ) -> mineintent_contracts::minecraft::BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async { Ok(()) })
    }

    fn move_input(
        &self,
        _request: MoveInputRequest,
        _control: OperationControl,
    ) -> mineintent_contracts::minecraft::BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async { Ok(()) })
    }

    fn respawn(
        &self,
        _control: mineintent_contracts::minecraft::OperationControl,
    ) -> mineintent_contracts::minecraft::BoxFuture<'_, Result<(), BackendError>> {
        self.respawn_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }

    fn release_all(&self) -> Result<(), BackendError> {
        self.releases.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct TestSubscription {
    closed: Arc<AtomicBool>,
    unsubscribes: Arc<AtomicUsize>,
}

impl Subscription for TestSubscription {
    fn unsubscribe(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        self.unsubscribes.fetch_add(1, Ordering::SeqCst);
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

struct TestBackend {
    motor: Arc<TestMotor>,
    snapshot: Mutex<MinecraftSnapshotV1>,
    snapshot_fail: AtomicBool,
    listener: Mutex<Option<Arc<dyn BackendEventListener>>>,
    subscription_closed: Arc<AtomicBool>,
    subscription_unsubscribes: Arc<AtomicUsize>,
    subscribe_gate: Mutex<Option<Arc<CleanupGate>>>,
    subscribe_fail: AtomicBool,
}

impl TestBackend {
    fn new(motor: Arc<TestMotor>) -> Arc<Self> {
        Arc::new(Self {
            motor,
            snapshot: Mutex::new({
                let mut snapshot = mineintent_contracts::minecraft::fixture_snapshot();
                snapshot.process_session_id = "process-test".to_owned();
                snapshot.connection_epoch = 1;
                snapshot.connection_attempt_id = "attempt-test".to_owned();
                snapshot.world.world_id = "world-test".to_owned();
                snapshot.world.dimension = "minecraft:overworld".to_owned();
                snapshot
            }),
            snapshot_fail: AtomicBool::new(false),
            listener: Mutex::new(None),
            subscription_closed: Arc::new(AtomicBool::new(false)),
            subscription_unsubscribes: Arc::new(AtomicUsize::new(0)),
            subscribe_gate: Mutex::new(None),
            subscribe_fail: AtomicBool::new(false),
        })
    }

    fn subscription_closed(&self) -> bool {
        self.subscription_closed.load(Ordering::SeqCst)
    }

    fn subscription_unsubscribes(&self) -> usize {
        self.subscription_unsubscribes.load(Ordering::SeqCst)
    }

    fn gate_subscribe(&self) -> Arc<CleanupGate> {
        let gate = CleanupGate::new();
        *self.subscribe_gate.lock().unwrap() = Some(Arc::clone(&gate));
        gate
    }

    fn fail_subscribe(&self) {
        self.subscribe_fail.store(true, Ordering::SeqCst);
    }

    fn fail_snapshot(&self) {
        self.snapshot_fail.store(true, Ordering::SeqCst);
    }

    fn emit(&self, event: BackendEventEnvelope) {
        if let Some(listener) = self.listener.lock().unwrap().as_ref() {
            listener.on_event(event);
        }
    }
}

impl MinecraftBackendApi for TestBackend {
    fn start(
        &self,
        _control: OperationControl,
    ) -> mineintent_contracts::minecraft::BoxFuture<'_, Result<BackendReady, BackendError>> {
        Box::pin(async {
            Err(BackendError::NotReady {
                state: "test backend ownership is external".to_owned(),
            })
        })
    }

    fn stop(
        &self,
        _reason: String,
        _control: OperationControl,
    ) -> mineintent_contracts::minecraft::BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async { Ok(()) })
    }

    fn state(&self) -> BackendState {
        BackendState::Idle
    }

    fn snapshot(&self) -> Result<MinecraftSnapshotV1, BackendError> {
        if self.snapshot_fail.load(Ordering::SeqCst) {
            return Err(BackendError::NotReady {
                state: "test snapshot failure".to_owned(),
            });
        }
        Ok(self.snapshot.lock().unwrap().clone())
    }

    fn subscribe(
        &self,
        listener: Arc<dyn BackendEventListener>,
    ) -> Result<Box<dyn Subscription>, BackendError> {
        if let Some(gate) = self.subscribe_gate.lock().unwrap().clone() {
            gate.enter();
        }
        if self.subscribe_fail.load(Ordering::SeqCst) {
            return Err(BackendError::NotReady {
                state: "test subscribe failure".to_owned(),
            });
        }
        *self.listener.lock().unwrap() = Some(listener);
        self.subscription_closed.store(false, Ordering::SeqCst);
        Ok(Box::new(TestSubscription {
            closed: Arc::clone(&self.subscription_closed),
            unsubscribes: Arc::clone(&self.subscription_unsubscribes),
        }))
    }

    fn observation_source(&self) -> Result<Arc<dyn ProtocolObservationSource>, BackendError> {
        Err(BackendError::NotReady {
            state: "test observation source unused".to_owned(),
        })
    }

    fn motor(&self) -> Result<Arc<dyn MinecraftMotorDriverApi>, BackendError> {
        Ok(self.motor.clone())
    }

    fn send_chat(&self, _message: String) -> Result<(), BackendError> {
        Ok(())
    }
}

fn runtime_parts(
    agent: Arc<TestAgent>,
) -> (
    Arc<ParticipantRuntime<TestAgent>>,
    Arc<TestFrameSource>,
    Arc<TestJournal>,
    Arc<TestSpeech>,
    Arc<TestMotor>,
    Arc<TestBackend>,
) {
    runtime_parts_with_namespace(agent, "participant-test")
}

fn runtime_parts_with_namespace(
    agent: Arc<TestAgent>,
    run_id_namespace: &str,
) -> (
    Arc<ParticipantRuntime<TestAgent>>,
    Arc<TestFrameSource>,
    Arc<TestJournal>,
    Arc<TestSpeech>,
    Arc<TestMotor>,
    Arc<TestBackend>,
) {
    let source = TestFrameSource::new();
    let journal = TestJournal::new();
    let speech = TestSpeech::new();
    let motor = TestMotor::new();
    let backend = TestBackend::new(Arc::clone(&motor));
    let runtime = ParticipantRuntime::new(ParticipantRuntimeConfig {
        backend: backend.clone(),
        agent,
        frame_source: source.clone(),
        memory: Arc::new(TestMemory),
        journal: journal.clone(),
        speech: speech.clone(),
        debug: Arc::new(DebugStateStore::new()),
        clock: Arc::new(TestClock),
        prompt_template: fixtures::prompt_template(),
        run_deadline: Duration::from_secs(30),
        wake_registry: WakeRegistry::default(),
        run_id_namespace: run_id_namespace.to_owned(),
    });
    (runtime, source, journal, speech, motor, backend)
}

fn scope(epoch: u64, dimension: &str) -> ParticipantScope {
    ParticipantScope::new(
        "process-test",
        epoch,
        "world-test",
        Some(dimension.to_owned()),
    )
}

fn chat_input(sequence: u64, username: &str, text: &str) -> AgentChatInputV5 {
    chat_input_at(sequence, username, text, &chat_timestamp(text))
}

fn chat_input_at(sequence: u64, username: &str, text: &str, at: &str) -> AgentChatInputV5 {
    AgentChatInputV5 {
        sequence,
        message: mineintent_contracts::agent::AgentChatMessageV5 {
            username: username.to_owned(),
            text: text.to_owned(),
            at: at.to_owned(),
        },
    }
}

fn chat_event(id: &str, epoch: u64, sender: &str, text: &str) -> BackendEventEnvelope {
    scoped_chat_event(
        id,
        "process-test",
        epoch,
        "world-test",
        "minecraft:overworld",
        sender,
        text,
    )
}

fn scoped_chat_event(
    id: &str,
    process_session_id: &str,
    epoch: u64,
    world_id: &str,
    dimension: &str,
    sender: &str,
    text: &str,
) -> BackendEventEnvelope {
    scoped_chat_event_at(
        id,
        process_session_id,
        epoch,
        world_id,
        dimension,
        sender,
        text,
        &chat_timestamp(text),
    )
}

fn scoped_chat_event_at(
    id: &str,
    process_session_id: &str,
    epoch: u64,
    world_id: &str,
    dimension: &str,
    sender: &str,
    text: &str,
    occurred_at: &str,
) -> BackendEventEnvelope {
    scoped_chat_event_at_attempt(
        id,
        process_session_id,
        epoch,
        "attempt-test",
        world_id,
        dimension,
        sender,
        text,
        occurred_at,
    )
}

fn scoped_chat_event_at_attempt(
    id: &str,
    process_session_id: &str,
    epoch: u64,
    connection_attempt_id: &str,
    world_id: &str,
    dimension: &str,
    sender: &str,
    text: &str,
    occurred_at: &str,
) -> BackendEventEnvelope {
    BackendEventEnvelope::new(
        BackendEventMetadata {
            id: id.to_owned(),
            occurred_at: occurred_at.to_owned(),
            process_session_id: process_session_id.to_owned(),
            connection_epoch: epoch,
            connection_attempt_id: connection_attempt_id.to_owned(),
            world_id: world_id.to_owned(),
            dimension: Some(dimension.to_owned()),
        },
        BackendEventKind::Chat,
        FactSource::ServerObserved,
        mineintent_contracts::minecraft::BackendEventPayload::Chat(ProtocolChatEvent {
            sender_username: Some(sender.to_owned()),
            plain_text: text.to_owned(),
            position: Some(mineintent_contracts::minecraft::ChatPosition::Chat),
            verified: Some(true),
        }),
    )
}

fn chat_timestamp(text: &str) -> String {
    let seconds = text
        .bytes()
        .fold(0_u8, |total, byte| total.wrapping_add(byte))
        % 60;
    format!("2026-08-03T00:01:{seconds:02}Z")
}

fn observation_invocation(id: &str) -> CapabilityInvocation {
    CapabilityInvocation {
        run_id: RunId::new(format!("observation-run-{id}")).unwrap(),
        tool_call_id: ToolCallId::new(format!("observation-call-{id}")).unwrap(),
        arguments: serde_json::Map::new(),
        action_id: format!("observation-action-{id}"),
        started_at: "2026-08-03T00:00:00Z".to_owned(),
    }
}

fn internal_fact(
    id: &str,
    event_scope: &ParticipantScope,
    event_type: &str,
) -> ParticipantInternalEvent {
    ParticipantInternalEvent::Fact {
        id: id.to_owned(),
        occurred_at: "2026-08-03T00:03:00Z".to_owned(),
        scope: event_scope.clone(),
        event_type: event_type.to_owned(),
        summary: format!("summary for {event_type}"),
    }
}

fn admission_ticket(payload: &mineintent_contracts::agent::JsonObject) -> u64 {
    payload
        .get("admissionTicket")
        .and_then(serde_json::Value::as_u64)
        .expect("participant journal payload has an admission ticket")
}

const TEST_ORDINARY_CAPACITY: usize = 16;
const TEST_CONTROL_CAPACITY: usize = 8;
const TEST_OVERFLOW_CAPACITY: usize = 4;

/// 把 worker 停在第二条 item 上。
///
/// 原实现靠 journal 写入阻塞间接达成；journal 收窄后普通事实不再落盘，
/// 改用 runtime 的单步闸门直说同一件事。`journal` 参数保留是为了让调用方
/// 的签名不变，同时明确它已不再是 worker 的刹车。
async fn hold_worker_on_second_journal(
    runtime: &ParticipantRuntime<TestAgent>,
    _journal: &TestJournal,
    event_scope: &ParticipantScope,
) {
    runtime.worker_gate().limit();
    runtime.worker_gate().allow(1);
    runtime
        .emit_internal(internal_fact("seed-fact", event_scope, "seed_fact"))
        .unwrap();
    runtime.worker_gate().wait_entered(1).await;
    runtime
        .emit_internal(internal_fact("held-fact", event_scope, "held_fact"))
        .unwrap();
    // 第二条到达闸门但没有许可，worker 就停在这里。
    runtime.worker_gate().wait_entered(2).await;
}

fn fill_ordinary_lane(
    runtime: &ParticipantRuntime<TestAgent>,
    event_scope: &ParticipantScope,
    prefix: &str,
) {
    let mut index = 0_usize;
    while runtime.queue_counts_for_test().0 < TEST_ORDINARY_CAPACITY {
        runtime
            .emit_internal(internal_fact(
                &format!("{prefix}-ordinary-{index}"),
                event_scope,
                "ordinary_fact",
            ))
            .unwrap();
        index += 1;
        assert!(index <= TEST_ORDINARY_CAPACITY + 2);
    }
}

fn dimension_changed_event(
    id: &str,
    process_session_id: &str,
    epoch: u64,
    world_id: &str,
    dimension: &str,
    from: &str,
    to: &str,
) -> BackendEventEnvelope {
    BackendEventEnvelope::new(
        BackendEventMetadata {
            id: id.to_owned(),
            occurred_at: format!("2026-08-03T00:02:{id}Z"),
            process_session_id: process_session_id.to_owned(),
            connection_epoch: epoch,
            connection_attempt_id: "attempt-test".to_owned(),
            world_id: world_id.to_owned(),
            dimension: Some(dimension.to_owned()),
        },
        BackendEventKind::Lifecycle,
        FactSource::ServerObserved,
        mineintent_contracts::minecraft::BackendEventPayload::Lifecycle(
            BackendLifecyclePayload::DimensionChanged {
                from: from.to_owned(),
                to: to.to_owned(),
            },
        ),
    )
}

fn lifecycle_event(id: &str, payload: BackendLifecyclePayload) -> BackendEventEnvelope {
    scoped_lifecycle_event(
        id,
        "process-test",
        1,
        "attempt-test",
        "world-test",
        Some("minecraft:overworld"),
        payload,
    )
}

fn scoped_lifecycle_event(
    id: &str,
    process_session_id: &str,
    epoch: u64,
    connection_attempt_id: &str,
    world_id: &str,
    dimension: Option<&str>,
    payload: BackendLifecyclePayload,
) -> BackendEventEnvelope {
    BackendEventEnvelope::new(
        BackendEventMetadata {
            id: id.to_owned(),
            occurred_at: "2026-08-03T00:06:00Z".to_owned(),
            process_session_id: process_session_id.to_owned(),
            connection_epoch: epoch,
            connection_attempt_id: connection_attempt_id.to_owned(),
            world_id: world_id.to_owned(),
            dimension: dimension.map(str::to_owned),
        },
        BackendEventKind::Lifecycle,
        FactSource::ServerObserved,
        mineintent_contracts::minecraft::BackendEventPayload::Lifecycle(payload),
    )
}

async fn wait_for_request(agent: &TestAgent, count: usize) {
    tokio::time::timeout(Duration::from_secs(2), agent.wait_for_runs(count))
        .await
        .expect("agent request should be deterministic");
}

async fn wait_for_lifecycle(
    runtime: &ParticipantRuntime<TestAgent>,
    lifecycle: mineintent_middle::participant::ParticipantLifecycle,
) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if runtime.lifecycle() == lifecycle {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("participant lifecycle should settle");
}

#[path = "participant_runtime/agent_binding.rs"]
mod agent_binding;
#[path = "participant_runtime/frame_and_chat.rs"]
mod frame_and_chat;
#[path = "participant_runtime/lifecycle_teardown.rs"]
mod lifecycle_teardown;
#[path = "participant_runtime/queue_admission.rs"]
mod queue_admission;
#[path = "participant_runtime/scope_identity.rs"]
mod scope_identity;
#[path = "participant_runtime/startup_stop.rs"]
mod startup_stop;

// 跨测试组共享的 fixture：原先散在用例之间，拆分后上提到父文件。
struct RealScopeGuard {
    checks: Arc<AtomicUsize>,
}

impl ScopeGuard for RealScopeGuard {
    fn check_current(&self) -> Result<(), AgentError> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn is_current(&self) -> bool {
        true
    }
}

struct NeverCancelled;

impl mineintent_contracts::agent::CancellationSignal for NeverCancelled {
    fn cancellation_error(&self) -> Option<AgentError> {
        None
    }

    fn cancelled(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = AgentError> + Send + '_>> {
        Box::pin(async { std::future::pending::<AgentError>().await })
    }
}
