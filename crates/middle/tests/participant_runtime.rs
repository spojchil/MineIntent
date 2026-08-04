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

    fn block_after(&self, entry_count: usize) {
        self.step_after.store(entry_count, Ordering::SeqCst);
    }

    fn release_through(&self, entry_count: usize) {
        self.step_release.send_replace(entry_count);
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

async fn hold_worker_on_second_journal(
    runtime: &ParticipantRuntime<TestAgent>,
    journal: &TestJournal,
    event_scope: &ParticipantScope,
) {
    journal.block_after(1);
    runtime
        .emit_internal(internal_fact("seed-fact", event_scope, "seed_fact"))
        .unwrap();
    journal.wait_for_entries(1).await;
    runtime
        .emit_internal(internal_fact("held-fact", event_scope, "held_fact"))
        .unwrap();
    journal.wait_for_entries(2).await;
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

#[tokio::test]
async fn startup_registry_and_addressing_are_explicit_and_symmetric() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    assert_eq!(runtime.wake_registry().len(), 1);
    assert_eq!(
        runtime.wake_registry().entries(),
        vec![WakeRule {
            kind: WakeKind::PlayerChat,
            condition: WakeRuleCondition::AddressedToParticipant,
        }]
    );

    source.set_chats(Vec::new());
    assert!(matches!(
        runtime.ingest_backend_event(chat_event("01", 1, "Alice", "hello everyone")),
        Ok(ParticipantAdmission::Recorded)
    ));
    assert!(agent.requests.lock().unwrap().is_empty());

    let alice = chat_input(1, "Alice", "@Bot help");
    source.set_chats(vec![alice.clone()]);
    runtime
        .ingest_event(ParticipantEvent::Backend(chat_event(
            "02",
            1,
            "Alice",
            "@Bot help",
        )))
        .unwrap();
    wait_for_request(&agent, 1).await;

    let bob = chat_input(2, "Bob", "@Bot look");
    source.set_chats(vec![alice, bob.clone()]);
    runtime
        .ingest_backend_event(chat_event("03", 1, "Bob", "@Bot look"))
        .unwrap();
    wait_for_request(&agent, 2).await;
    assert_eq!(agent.texts(), vec!["@Bot help", "@Bot look"]);
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn startup_seeds_one_participant_started_without_calling_model() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    assert!(agent.requests.lock().unwrap().is_empty());
    assert!(matches!(
        runtime.start_worker(),
        Err(ParticipantRuntimeError::AlreadyStarted)
    ));
    assert!(agent.requests.lock().unwrap().is_empty());

    let trigger = chat_input(301, "Alice", "@Bot startup seed");
    source.set_chats(vec![trigger]);
    runtime
        .ingest_backend_event(chat_event("startup-chat", 1, "Alice", "@Bot startup seed"))
        .unwrap();
    wait_for_request(&agent, 1).await;

    let request = agent.requests.lock().unwrap()[0].clone();
    let seed_events = request
        .context
        .frame
        .events
        .as_ref()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            mineintent_contracts::agent::AgentEventV5::Summary {
                event_type,
                summary,
            } if event_type == "participant.started" => Some(summary.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(seed_events, vec!["AI 参与者已进入世界"]);
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn startup_snapshot_failure_rolls_back_without_worker_or_model_call() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, backend) = runtime_parts(Arc::clone(&agent));
    backend.fail_snapshot();

    let error = runtime.start_worker().unwrap_err();
    assert!(matches!(error, ParticipantRuntimeError::Backend(_)));
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Faulted
    );
    assert!(runtime.current_scope().is_none());
    assert!(agent.requests.lock().unwrap().is_empty());
    assert_eq!(backend.subscription_unsubscribes(), 0);
    assert_eq!(source.retained_count(), 0);
    assert!(source.release_all_calls() >= 1);

    runtime.stop().await.unwrap();
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
}

#[tokio::test]
async fn stop_finishes_while_subscribe_is_blocked_and_closes_late_handle() {
    let agent = TestAgent::new(0);
    let (runtime, _source, _journal, _speech, _motor, backend) = runtime_parts(Arc::clone(&agent));
    let subscribe_gate = backend.gate_subscribe();
    let start_runtime = Arc::clone(&runtime);
    let start = tokio::task::spawn_blocking(move || start_runtime.start_worker());
    subscribe_gate.wait_started().await;

    let stop_runtime = Arc::clone(&runtime);
    tokio::time::timeout(Duration::from_secs(1), stop_runtime.stop())
        .await
        .expect("stop must not wait for a blocked backend subscribe")
        .unwrap();
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
    assert!(!backend.subscription_closed());

    subscribe_gate.release();
    let start_result = tokio::time::timeout(Duration::from_secs(1), start)
        .await
        .expect("blocked start must return after subscribe is released")
        .unwrap();
    assert!(matches!(
        start_result,
        Err(ParticipantRuntimeError::Stopped)
    ));
    assert!(backend.subscription_closed());
    assert_eq!(backend.subscription_unsubscribes(), 1);
    runtime.stop().await.unwrap();
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
}

#[tokio::test]
async fn stop_finishes_before_blocked_subscribe_error_without_late_attach() {
    let agent = TestAgent::new(0);
    let (runtime, _source, _journal, _speech, _motor, backend) = runtime_parts(Arc::clone(&agent));
    let subscribe_gate = backend.gate_subscribe();
    backend.fail_subscribe();

    let start_runtime = Arc::clone(&runtime);
    let start = tokio::task::spawn_blocking(move || start_runtime.start_worker());
    subscribe_gate.wait_started().await;

    tokio::time::timeout(Duration::from_secs(1), runtime.stop())
        .await
        .expect("stop must not wait for a blocked subscribe that will fail")
        .unwrap();
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
    assert_eq!(backend.subscription_unsubscribes(), 0);
    assert!(!backend.subscription_closed());

    subscribe_gate.release();
    let start_result = tokio::time::timeout(Duration::from_secs(1), start)
        .await
        .expect("blocked start must return after the backend reports subscribe failure")
        .unwrap();
    assert!(matches!(
        start_result,
        Err(ParticipantRuntimeError::Backend(_))
    ));
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
    assert_eq!(backend.subscription_unsubscribes(), 0);
    assert!(!backend.subscription_closed());
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn subscribe_failure_rolls_back_worker_and_lifecycle() {
    let agent = TestAgent::new(0);
    let (runtime, _source, _journal, _speech, _motor, backend) = runtime_parts(Arc::clone(&agent));
    backend.fail_subscribe();
    let error = runtime.start_worker().unwrap_err();
    assert!(matches!(error, ParticipantRuntimeError::Backend(_)));
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Faulted
    );
    assert_eq!(backend.subscription_unsubscribes(), 0);
    runtime.stop().await.unwrap();
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
}

#[tokio::test]
async fn ordinary_events_do_not_release_body_or_cancel_speech() {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, speech, motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let first = chat_input(1, "Alice", "@Bot first");
    source.set_chats(vec![first]);
    runtime
        .ingest_backend_event(chat_event("11", 1, "Alice", "@Bot first"))
        .unwrap();
    wait_for_request(&agent, 1).await;

    let second = chat_input(2, "Bob", "@Bot second");
    source.set_chats(vec![chat_input(1, "Alice", "@Bot first"), second]);
    runtime
        .ingest_backend_event(chat_event("12", 1, "Bob", "@Bot second"))
        .unwrap();
    assert_eq!(motor.releases.load(Ordering::SeqCst), 0);
    assert_eq!(speech.cancelled.load(Ordering::SeqCst), 0);
    runtime.request_stop().unwrap();
    assert!(motor.releases.load(Ordering::SeqCst) >= 1);
    assert!(speech.cancelled.load(Ordering::SeqCst) >= 1);
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn v5_frame_requires_light_deduplicates_trigger_and_preserves_armor() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let trigger = chat_input(5, "Alice", "@Bot frame");
    source.set_chats(vec![trigger.clone()]);
    source.capture.lock().unwrap().events = vec![AgentContextV5EventInput::PlayerChat {
        sequence: 5,
        message: trigger.message.clone(),
    }];
    runtime
        .ingest_backend_event(chat_event("21", 1, "Alice", "@Bot frame"))
        .unwrap();
    wait_for_request(&agent, 1).await;
    let request = agent.requests.lock().unwrap().remove(0);
    let wire = serde_json::to_value(&request.context).unwrap();
    assert_eq!(wire["frame"]["light"], 7);
    assert!(wire["frame"].get("viewport").is_none());
    assert!(wire["frame"]["status"].get("armor").is_none());
    assert_eq!(wire.to_string().matches("@Bot frame").count(), 1);
    assert!(wire["frame"]["chat"]["items"][0].get("text").is_none());
    assert!(wire.to_string().find("sequence").is_none());

    source.set_armor(Some(6));
    source.capture.lock().unwrap().events.clear();
    let next = chat_input(6, "Bob", "@Bot armor");
    source.set_chats(vec![trigger, next]);
    runtime
        .ingest_backend_event(chat_event("22", 1, "Bob", "@Bot armor"))
        .unwrap();
    wait_for_request(&agent, 2).await;
    let request = agent.requests.lock().unwrap().remove(0);
    assert_eq!(request.context.frame.status.unwrap().armor, Some(6));

    source.missing_light.store(true, Ordering::SeqCst);
    let missing = chat_input(7, "Alice", "@Bot no light");
    source.set_chats(vec![missing]);
    let mut failures = runtime.subscribe_failures();
    runtime
        .ingest_backend_event(chat_event("23", 1, "Alice", "@Bot no light"))
        .unwrap();
    let failure = tokio::time::timeout(Duration::from_secs(2), failures.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failure.code, "opening_frame_light_missing");
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Running
    );
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn production_observation_after_is_body_only_and_drains_facts_once() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    let opening = chat_input(401, "Alice", "@Bot observation");
    source.set_chats(vec![opening]);
    runtime
        .ingest_backend_event(chat_event(
            "observation-trigger",
            1,
            "Alice",
            "@Bot observation",
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;

    let run_scope = runtime
        .current_scope()
        .expect("startup scope remains active");
    let generation = runtime.current_generation();
    runtime
        .emit_internal(internal_fact("body-fact", &run_scope, "self_hurt"))
        .unwrap();

    let frame_source: Arc<dyn ParticipantFrameSource> = source.clone();
    let observation = ParticipantObservationAfterSource::new(
        frame_source,
        runtime.fact_owner(),
        run_scope.clone(),
        generation,
        "body-trigger",
    );
    let checks = Arc::new(AtomicUsize::new(0));
    let signal = NeverCancelled;
    let guard = RealScopeGuard {
        checks: Arc::clone(&checks),
    };
    let deadline = Deadline::after(std::time::Instant::now(), Duration::from_secs(1)).unwrap();
    let first = observation
        .observe_after(
            observation_invocation("first"),
            ExecutionResource::Body,
            serde_json::json!({"status": "failed"}),
            CapabilityExecutionContext::new(
                &run_scope.world_id,
                "body-trigger",
                ExecutionControl::new(&signal, deadline),
                &guard,
            ),
        )
        .await
        .unwrap()
        .expect("body observation returns direct frame");
    assert!(first.get("world").is_some());
    assert!(first.get("viewport").is_none());
    assert!(first.get("stable").is_none());
    assert!(first["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| { event["type"] == "self_hurt" }));
    assert!(first.get("triggerPlayer").is_none());

    let signal = NeverCancelled;
    let guard = RealScopeGuard {
        checks: Arc::clone(&checks),
    };
    let deadline = Deadline::after(std::time::Instant::now(), Duration::from_secs(1)).unwrap();
    let second = observation
        .observe_after(
            observation_invocation("second"),
            ExecutionResource::Body,
            serde_json::json!({"status": "completed"}),
            CapabilityExecutionContext::new(
                &run_scope.world_id,
                "body-trigger",
                ExecutionControl::new(&signal, deadline),
                &guard,
            ),
        )
        .await
        .unwrap()
        .expect("passive body facts remain sampleable");
    assert!(second.get("events").is_none());
    assert!(second.get("status").is_some());
    assert_eq!(source.capture_calls(), 3, "opening plus two body samples");

    for (resource, id) in [
        (ExecutionResource::Viewport, "viewport"),
        (ExecutionResource::Chat, "chat"),
        (ExecutionResource::Memory, "memory"),
    ] {
        let signal = NeverCancelled;
        let guard = RealScopeGuard {
            checks: Arc::clone(&checks),
        };
        let deadline = Deadline::after(std::time::Instant::now(), Duration::from_secs(1)).unwrap();
        let result = observation
            .observe_after(
                observation_invocation(id),
                resource,
                serde_json::json!({"status": "completed"}),
                CapabilityExecutionContext::new(
                    &run_scope.world_id,
                    "body-trigger",
                    ExecutionControl::new(&signal, deadline),
                    &guard,
                ),
            )
            .await
            .unwrap();
        assert!(result.is_none(), "{resource:?} must not sample body facts");
    }
    assert_eq!(source.capture_calls(), 3);
    assert!(checks.load(Ordering::SeqCst) >= 7);
    runtime.stop().await.unwrap();
    drop(runtime);

    let signal = NeverCancelled;
    let guard = RealScopeGuard { checks };
    let deadline = Deadline::after(std::time::Instant::now(), Duration::from_secs(1)).unwrap();
    let error = observation
        .observe_after(
            observation_invocation("after-runtime-drop"),
            ExecutionResource::Body,
            serde_json::json!({"status": "completed"}),
            CapabilityExecutionContext::new(
                &run_scope.world_id,
                "body-trigger",
                ExecutionControl::new(&signal, deadline),
                &guard,
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, AgentErrorCode::ScopeInvalid);
}

#[tokio::test]
async fn repeated_chat_text_uses_timestamp_and_sequence_identity() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    let first = chat_input_at(200, "Alice", "@Bot same text", "2026-08-03T00:10:00Z");
    let second = chat_input_at(201, "Alice", "@Bot same text", "2026-08-03T00:10:01Z");
    source.set_chats(vec![first.clone(), second.clone()]);
    runtime
        .ingest_backend_event(scoped_chat_event_at(
            "repeat-1",
            "process-test",
            1,
            "world-test",
            "minecraft:overworld",
            "Alice",
            "@Bot same text",
            &first.message.at,
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;
    let first_request = agent.requests.lock().unwrap()[0].clone();
    let first_events = first_request.context.frame.events.as_ref().unwrap();
    assert_eq!(
        first_events
            .iter()
            .filter_map(|event| match event {
                mineintent_contracts::agent::AgentEventV5::PlayerChat(message) => {
                    Some(message.at.as_str())
                }
                mineintent_contracts::agent::AgentEventV5::Summary { .. } => None,
            })
            .collect::<Vec<_>>(),
        vec![first.message.at.as_str()]
    );
    assert!(first_request
        .context
        .frame
        .chat
        .as_ref()
        .unwrap()
        .items
        .iter()
        .any(|item| matches!(
            item,
            mineintent_contracts::agent::AgentChatItemV5::Message(message)
                if message.at == second.message.at
        )));

    source.set_chats(vec![first.clone(), second.clone()]);
    runtime
        .ingest_backend_event(scoped_chat_event_at(
            "repeat-2",
            "process-test",
            1,
            "world-test",
            "minecraft:overworld",
            "Alice",
            "@Bot same text",
            &second.message.at,
        ))
        .unwrap();
    wait_for_request(&agent, 2).await;
    let second_request = agent.requests.lock().unwrap()[1].clone();
    let second_events = second_request.context.frame.events.as_ref().unwrap();
    let trigger_events = second_events
        .iter()
        .filter_map(|event| match event {
            mineintent_contracts::agent::AgentEventV5::PlayerChat(message) => Some(message),
            mineintent_contracts::agent::AgentEventV5::Summary { .. } => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(trigger_events.len(), 1);
    assert_eq!(trigger_events[0].at, second.message.at);
    assert!(second_request
        .context
        .frame
        .chat
        .as_ref()
        .unwrap()
        .items
        .iter()
        .any(|item| matches!(
            item,
            mineintent_contracts::agent::AgentChatItemV5::Message(message)
                if message.at == first.message.at
        )));
    assert!(second_request
        .context
        .frame
        .chat
        .as_ref()
        .unwrap()
        .items
        .iter()
        .any(|item| matches!(
            item,
            mineintent_contracts::agent::AgentChatItemV5::Moved(moved)
                if moved.at == second.message.at
        )));
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn second_chat_queues_fifo_without_preempting_first_run() {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, speech, motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let first = chat_input(10, "Alice", "@Bot first");
    source.set_chats(vec![first.clone()]);
    runtime
        .ingest_backend_event(chat_event("31", 1, "Alice", "@Bot first"))
        .unwrap();
    wait_for_request(&agent, 1).await;
    let second = chat_input(11, "Bob", "@Bot second");
    source.set_chats(vec![first, second]);
    runtime
        .ingest_backend_event(chat_event("32", 1, "Bob", "@Bot second"))
        .unwrap();
    assert_eq!(agent.requests.lock().unwrap().len(), 1);
    assert_eq!(motor.releases.load(Ordering::SeqCst), 0);
    assert_eq!(speech.cancelled.load(Ordering::SeqCst), 0);
    agent.release();
    wait_for_request(&agent, 2).await;
    assert_eq!(agent.texts(), vec!["@Bot first", "@Bot second"]);
    assert_eq!(source.retain_calls(), 2);
    assert_eq!(source.retained_count(), 0);
    assert_eq!(source.release_calls(), 2);
    runtime.stop().await.unwrap();
    assert!(source.release_all_calls() >= 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fact_recorded_after_wake_admission_is_drained_at_opening_processing_boundary() {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(agent.clone());
    runtime.start_worker().unwrap();

    let first = chat_input(100, "Alice", "@Bot first boundary");
    source.set_chats(vec![first.clone()]);
    runtime
        .ingest_backend_event(chat_event(
            "boundary-first",
            1,
            "Alice",
            "@Bot first boundary",
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;

    let second = chat_input(101, "Bob", "@Bot second boundary");
    source.set_chats(vec![first, second]);
    let capture_gate = source.gate_capture();
    let admission = runtime
        .ingest_backend_event(chat_event(
            "boundary-second",
            1,
            "Bob",
            "@Bot second boundary",
        ))
        .unwrap();
    assert!(matches!(admission, ParticipantAdmission::WakeQueued { .. }));

    agent.release();
    tokio::time::timeout(Duration::from_secs(2), capture_gate.wait_started())
        .await
        .expect("second opening capture should reach the controlled gate");

    let admission_gate = CleanupGate::new();
    runtime.install_admission_observer_for_test(Arc::new(TestAdmissionObserver {
        gate: Arc::clone(&admission_gate),
    }));
    let fact_runtime = Arc::clone(&runtime);
    let fact_scope = scope(1, "minecraft:overworld");
    let fact_producer = tokio::task::spawn_blocking(move || {
        fact_runtime.emit_internal(internal_fact(
            "damage-after-admission",
            &fact_scope,
            "self_hurt",
        ))
    });
    tokio::time::timeout(Duration::from_secs(2), admission_gate.wait_started())
        .await
        .expect("producer should stop after queue admission before record_fact");
    capture_gate.release();
    admission_gate.release();
    fact_producer
        .await
        .expect("fact producer task should join")
        .expect("fact admission should succeed");
    wait_for_request(&agent, 2).await;

    let requests = agent.requests.lock().unwrap();
    let second_events = requests[1]
        .context
        .frame
        .events
        .as_ref()
        .expect("second opening has trigger and fact");
    assert!(second_events.iter().any(|event| {
        matches!(
            event,
            mineintent_contracts::agent::AgentEventV5::Summary { event_type, .. }
                if event_type == "self_hurt"
        )
    }));

    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn active_run_body_drain_precedes_queued_opening_without_fact_replay() {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    let first = chat_input_at(120, "Alice", "@Bot seam first", "2026-08-03T00:11:00Z");
    source.set_chats(vec![first.clone()]);
    runtime
        .ingest_backend_event(scoped_chat_event_at(
            "seam-first",
            "process-test",
            1,
            "world-test",
            "minecraft:overworld",
            "Alice",
            "@Bot seam first",
            &first.message.at,
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;

    let run_scope = runtime.current_scope().expect("first run scope is active");
    let generation = runtime.current_generation();
    runtime
        .emit_internal(internal_fact("seam-damage", &run_scope, "self_hurt"))
        .unwrap();

    let second = chat_input_at(121, "Bob", "@Bot seam second", "2026-08-03T00:11:01Z");
    source.set_chats(vec![first, second]);
    let admission = runtime
        .ingest_backend_event(scoped_chat_event_at(
            "seam-second",
            "process-test",
            1,
            "world-test",
            "minecraft:overworld",
            "Bob",
            "@Bot seam second",
            "2026-08-03T00:11:01Z",
        ))
        .unwrap();
    assert!(matches!(admission, ParticipantAdmission::WakeQueued { .. }));
    assert_eq!(agent.requests.lock().unwrap().len(), 1);

    let frame_source: Arc<dyn ParticipantFrameSource> = source.clone();
    let observation = ParticipantObservationAfterSource::new(
        frame_source,
        runtime.fact_owner(),
        run_scope.clone(),
        generation,
        "seam-first",
    );
    let checks = Arc::new(AtomicUsize::new(0));
    let signal = NeverCancelled;
    let guard = RealScopeGuard {
        checks: Arc::clone(&checks),
    };
    let deadline = Deadline::after(std::time::Instant::now(), Duration::from_secs(1)).unwrap();
    let first_observation = observation
        .observe_after(
            observation_invocation("seam-body-failure"),
            ExecutionResource::Body,
            serde_json::json!({"status": "failed"}),
            CapabilityExecutionContext::new(
                &run_scope.world_id,
                "seam-first",
                ExecutionControl::new(&signal, deadline),
                &guard,
            ),
        )
        .await
        .unwrap()
        .expect("body ordinary failure still has observation");
    assert!(first_observation["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| { event["type"] == "self_hurt" }));

    let signal = NeverCancelled;
    let guard = RealScopeGuard {
        checks: Arc::clone(&checks),
    };
    let deadline = Deadline::after(std::time::Instant::now(), Duration::from_secs(1)).unwrap();
    let second_observation = observation
        .observe_after(
            observation_invocation("seam-body-second"),
            ExecutionResource::Body,
            serde_json::json!({"status": "failed"}),
            CapabilityExecutionContext::new(
                &run_scope.world_id,
                "seam-first",
                ExecutionControl::new(&signal, deadline),
                &guard,
            ),
        )
        .await
        .unwrap()
        .expect("second body sample keeps passive frame facts");
    assert!(second_observation.get("events").is_none());

    agent.release();
    wait_for_request(&agent, 2).await;
    let requests = agent.requests.lock().unwrap();
    let second_opening = requests[1]
        .context
        .frame
        .events
        .as_ref()
        .expect("second opening keeps its trigger event");
    assert!(!second_opening.iter().any(|event| {
        matches!(
            event,
            mineintent_contracts::agent::AgentEventV5::Summary { event_type, .. }
                if event_type == "self_hurt"
        )
    }));
    drop(requests);
    assert_eq!(agent.texts(), vec!["@Bot seam first", "@Bot seam second"]);
    assert_eq!(runtime.current_scope(), Some(run_scope));
    assert_eq!(runtime.current_generation(), generation);
    assert_eq!(source.retained_count(), 0);
    assert!(checks.load(Ordering::SeqCst) >= 6);
    runtime.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn body_observation_progresses_while_full_control_admission_waits() {
    let agent = TestAgent::new(1);
    let (runtime, source, journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    let first = chat_input_at(130, "Alice", "@Bot deadlock first", "2026-08-03T00:12:00Z");
    source.set_chats(vec![first.clone()]);
    runtime
        .ingest_backend_event(scoped_chat_event_at(
            "deadlock-first",
            "process-test",
            1,
            "world-test",
            "minecraft:overworld",
            "Alice",
            "@Bot deadlock first",
            &first.message.at,
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;

    let run_scope = runtime.current_scope().expect("first run scope is active");
    let generation = runtime.current_generation();
    let queued: Vec<_> = (0..TEST_CONTROL_CAPACITY)
        .map(|index| {
            chat_input_at(
                131 + index as u64,
                "Alice",
                &format!("@Bot deadlock queued {index}"),
                &format!("2026-08-03T00:12:{:02}Z", index + 1),
            )
        })
        .collect();
    let blocked_chat = chat_input_at(140, "Bob", "@Bot deadlock blocked", "2026-08-03T00:12:11Z");
    let mut chats = vec![first.clone()];
    chats.extend(queued.iter().cloned());
    chats.push(blocked_chat.clone());
    source.set_chats(chats);

    runtime
        .emit_internal(internal_fact("deadlock-body-fact", &run_scope, "self_hurt"))
        .unwrap();
    for (index, chat) in queued.iter().enumerate() {
        runtime
            .ingest_backend_event(scoped_chat_event_at(
                &format!("deadlock-queued-{index}"),
                "process-test",
                1,
                "world-test",
                "minecraft:overworld",
                &chat.message.username,
                &chat.message.text,
                &chat.message.at,
            ))
            .unwrap();
    }
    assert_eq!(runtime.queue_counts_for_test().1, TEST_CONTROL_CAPACITY);

    let blocked_runtime = Arc::clone(&runtime);
    let blocked_event = scoped_chat_event_at(
        "deadlock-blocked",
        "process-test",
        1,
        "world-test",
        "minecraft:overworld",
        &blocked_chat.message.username,
        &blocked_chat.message.text,
        &blocked_chat.message.at,
    );
    let blocked =
        tokio::task::spawn_blocking(move || blocked_runtime.ingest_backend_event(blocked_event));
    tokio::time::timeout(
        Duration::from_secs(2),
        runtime.wait_for_queue_waiters_for_test(1),
    )
    .await
    .expect("the next addressed admission must wait for control capacity");

    let frame_source: Arc<dyn ParticipantFrameSource> = source.clone();
    let observation = Arc::new(ParticipantObservationAfterSource::new(
        frame_source,
        runtime.fact_owner(),
        run_scope.clone(),
        generation,
        "deadlock-first",
    ));
    let observation_handle = tokio::runtime::Handle::current();
    let mut observation_task = tokio::task::spawn_blocking({
        let observation = Arc::clone(&observation);
        move || {
            let signal = NeverCancelled;
            let checks = Arc::new(AtomicUsize::new(0));
            let guard = RealScopeGuard { checks };
            let deadline =
                Deadline::after(std::time::Instant::now(), Duration::from_secs(1)).unwrap();
            observation_handle.block_on(observation.observe_after(
                observation_invocation("deadlock-body"),
                ExecutionResource::Body,
                serde_json::json!({"status": "failed"}),
                CapabilityExecutionContext::new(
                    &run_scope.world_id,
                    "deadlock-first",
                    ExecutionControl::new(&signal, deadline),
                    &guard,
                ),
            ))
        }
    });
    let body_result = tokio::time::timeout(Duration::from_secs(2), &mut observation_task).await;
    assert!(
        !blocked.is_finished(),
        "the full control lane must remain blocked while the active run is held"
    );

    agent.release();
    let blocked_admission = tokio::time::timeout(Duration::from_secs(2), blocked)
        .await
        .expect("blocked admission must complete after the active run releases capacity")
        .expect("blocked admission task must not panic")
        .expect("blocked admission must retain its structured success boundary");
    assert!(matches!(
        blocked_admission,
        ParticipantAdmission::WakeQueued { .. }
    ));

    let body = body_result
        .expect("body observation must finish while the producer waits")
        .expect("body observation task must not panic")
        .expect("body observation must succeed")
        .expect("body observation must return a direct frame");
    assert!(body["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["type"] == "self_hurt"));

    wait_for_request(&agent, TEST_CONTROL_CAPACITY + 2).await;
    let expected_texts = std::iter::once(first.message.text.clone())
        .chain(queued.iter().map(|chat| chat.message.text.clone()))
        .chain(std::iter::once(blocked_chat.message.text.clone()))
        .collect::<Vec<_>>();
    assert_eq!(agent.texts(), expected_texts);

    let payloads = journal.payloads();
    let wake_ids = (0..TEST_CONTROL_CAPACITY)
        .map(|index| format!("deadlock-queued-{index}"))
        .chain(std::iter::once("deadlock-blocked".to_owned()))
        .collect::<Vec<_>>();
    let wake_tickets = wake_ids
        .iter()
        .map(|id| {
            payloads
                .iter()
                .find(|payload| payload.get("id").and_then(serde_json::Value::as_str) == Some(id))
                .map(admission_ticket)
                .expect("every queued wake must be journaled")
        })
        .collect::<Vec<_>>();
    assert!(wake_tickets
        .windows(2)
        .all(|tickets| tickets[0] < tickets[1]));

    runtime.stop().await.unwrap();
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
    assert_eq!(source.retained_count(), 0);
    assert_eq!(source.retain_calls(), TEST_CONTROL_CAPACITY + 2);
    assert_eq!(source.release_calls(), TEST_CONTROL_CAPACITY + 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn waiting_old_wake_is_ignored_after_scope_generation_changes() {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    let first = chat_input(150, "Alice", "@Bot stale wait first");
    source.set_chats(vec![first.clone()]);
    runtime
        .ingest_backend_event(scoped_chat_event_at(
            "stale-wait-first",
            "process-test",
            1,
            "world-test",
            "minecraft:overworld",
            "Alice",
            &first.message.text,
            &first.message.at,
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;

    let queued: Vec<_> = (0..TEST_CONTROL_CAPACITY)
        .map(|index| {
            chat_input(
                151 + index as u64,
                "Alice",
                &format!("@Bot stale wait {index}"),
            )
        })
        .collect();
    for (index, chat) in queued.iter().enumerate() {
        runtime
            .ingest_backend_event(scoped_chat_event_at(
                &format!("stale-wait-queued-{index}"),
                "process-test",
                1,
                "world-test",
                "minecraft:overworld",
                &chat.message.username,
                &chat.message.text,
                &chat.message.at,
            ))
            .unwrap();
    }
    assert_eq!(runtime.queue_counts_for_test().1, TEST_CONTROL_CAPACITY);

    let old_runtime = Arc::clone(&runtime);
    let old_chat = chat_input(160, "Bob", "@Bot stale wait blocked");
    let old_event = scoped_chat_event_at(
        "stale-wait-blocked",
        "process-test",
        1,
        "world-test",
        "minecraft:overworld",
        &old_chat.message.username,
        &old_chat.message.text,
        &old_chat.message.at,
    );
    let old_producer =
        tokio::task::spawn_blocking(move || old_runtime.ingest_backend_event(old_event));
    tokio::time::timeout(
        Duration::from_secs(2),
        runtime.wait_for_queue_waiters_for_test(1),
    )
    .await
    .expect("old wake producer must reach the bounded queue wait");

    let new_scope = scope(2, "minecraft:nether");
    let generation = runtime.current_generation();
    let scope_runtime = Arc::clone(&runtime);
    let scope_event = ParticipantInternalEvent::ScopeChanged {
        id: "stale-wait-scope-change".to_owned(),
        occurred_at: "2026-08-03T00:13:00Z".to_owned(),
        scope: new_scope.clone(),
        reason: "scope changes while an old wake waits for capacity".to_owned(),
    };
    let scope_producer =
        tokio::task::spawn_blocking(move || scope_runtime.emit_internal(scope_event));
    // Deterministic synchronization: wait until the scope-change producer has
    // published the invalidation (generation bump), which happens while it
    // still holds the admission serial. The old wake cannot resolve before
    // that serial is released, so this proves the scope/generation change was
    // applied while the old admission was still pending. The previous
    // `wait_for_queue_waiters_for_test(2)` assertion was a scheduling
    // assumption: the worker may pop a stale control item and wake the old
    // producer before the scope producer registers as the second queue
    // waiter, so the transient "two waiters" count is not a protocol fact.
    tokio::time::timeout(
        Duration::from_secs(2),
        runtime.wait_for_generation_for_test(generation + 1),
    )
    .await
    .expect("scope change must be published while the old wake waits for capacity");

    let old_admission = tokio::time::timeout(Duration::from_secs(2), old_producer)
        .await
        .expect("old waiting producer must complete after scope cancellation")
        .expect("old producer task must not panic")
        .expect("old producer must keep the structured admission boundary");
    assert!(matches!(old_admission, ParticipantAdmission::Ignored));

    let scope_admission = tokio::time::timeout(Duration::from_secs(2), scope_producer)
        .await
        .expect("scope change producer must complete after the stale ticket is skipped")
        .expect("scope producer task must not panic")
        .expect("scope change admission must succeed");
    assert!(matches!(scope_admission, ParticipantAdmission::Recorded));
    assert_eq!(runtime.current_scope(), Some(new_scope));

    runtime.stop().await.unwrap();
    assert_eq!(source.retained_count(), 0);
    assert_eq!(source.retain_calls(), TEST_CONTROL_CAPACITY + 2);
}

#[tokio::test]
async fn journal_gate_serializes_later_chat_before_model() {
    let agent = TestAgent::new(0);
    let (runtime, source, journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    journal.set_gate(false);
    let first = chat_input(20, "Alice", "@Bot journal one");
    source.set_chats(vec![first]);
    journal.set_gate(true);
    runtime
        .ingest_backend_event(chat_event("41", 1, "Alice", "@Bot journal one"))
        .unwrap();
    journal.wait_for_entries(1).await;
    let second = chat_input(21, "Bob", "@Bot journal two");
    source.set_chats(vec![chat_input(20, "Alice", "@Bot journal one"), second]);
    runtime
        .ingest_backend_event(chat_event("42", 1, "Bob", "@Bot journal two"))
        .unwrap();
    assert!(agent.requests.lock().unwrap().is_empty());
    journal.set_gate(false);
    wait_for_request(&agent, 2).await;
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn request_stop_wakes_full_control_and_overflow_producer() {
    let agent = TestAgent::new(0);
    let (runtime, source, journal, _speech, _motor, backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let mut failures = runtime.subscribe_failures();
    let current = scope(1, "minecraft:overworld");
    hold_worker_on_second_journal(&runtime, &journal, &current).await;

    fill_ordinary_lane(&runtime, &current, "stop-saturation");
    for marker_index in 0..TEST_OVERFLOW_CAPACITY {
        while runtime.queue_counts_for_test().0 < TEST_ORDINARY_CAPACITY {
            runtime
                .emit_internal(internal_fact(
                    &format!("stop-fill-{marker_index}"),
                    &current,
                    "ordinary_fact",
                ))
                .unwrap();
        }
        runtime
            .emit_internal(internal_fact(
                &format!("stop-loss-{marker_index}"),
                &current,
                "ordinary_loss_candidate",
            ))
            .unwrap();
        let (_, _, overflow, _, _) = runtime.queue_counts_for_test();
        assert_eq!(overflow, marker_index + 1);
        if marker_index + 1 < TEST_OVERFLOW_CAPACITY {
            let release_entry = 2 + marker_index;
            journal.release_through(release_entry);
            journal.wait_for_entries(release_entry + 1).await;
        }
    }

    for chat_index in 0..TEST_CONTROL_CAPACITY {
        let text = format!("@Bot control-{chat_index}");
        backend.emit(chat_event(
            &format!("control-{chat_index}"),
            1,
            "Alice",
            &text,
        ));
    }
    let (ordinary, control, overflow, terminal, waiting) = runtime.queue_counts_for_test();
    assert_eq!(ordinary, TEST_ORDINARY_CAPACITY);
    assert_eq!(control, TEST_CONTROL_CAPACITY);
    assert_eq!(overflow, TEST_OVERFLOW_CAPACITY);
    assert_eq!(terminal, 0);
    assert_eq!(waiting, 0);

    let producer_backend = Arc::clone(&backend);
    let producer = std::thread::spawn(move || {
        producer_backend.emit(chat_event(
            "control-blocked",
            1,
            "Bob",
            "@Bot control-blocked",
        ));
    });
    tokio::time::timeout(
        Duration::from_secs(2),
        runtime.wait_for_queue_waiters_for_test(1),
    )
    .await
    .expect("the extra control producer must reach the bounded queue wait");

    let callback = tokio::task::spawn_blocking(move || producer.join());
    let stop_runtime = Arc::clone(&runtime);
    let stop = tokio::task::spawn_blocking(move || stop_runtime.request_stop());
    let stopped = tokio::time::timeout(Duration::from_secs(2), stop)
        .await
        .expect("request_stop must not wait indefinitely for a full lane")
        .expect("request_stop task must not panic")
        .expect("request_stop must succeed");
    assert!(stopped);
    tokio::time::timeout(Duration::from_secs(2), callback)
        .await
        .expect("backend listener callback must return after queue cancellation")
        .expect("backend listener callback task must not panic")
        .expect("backend listener thread must not panic");
    assert!(matches!(
        failures.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(source.retain_calls(), TEST_CONTROL_CAPACITY + 1);
    assert_eq!(source.retained_count(), 0);
    assert_eq!(source.release_calls(), TEST_CONTROL_CAPACITY + 1);
    assert_ne!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Faulted
    );

    journal.release_through(usize::MAX);
    journal.set_gate(false);
    runtime.stop().await.unwrap();
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
}

#[tokio::test]
async fn old_scope_omission_marker_keeps_ticket_and_cannot_cross_generation() {
    let agent = TestAgent::new(0);
    let (runtime, source, journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let old_scope = scope(1, "minecraft:overworld");
    hold_worker_on_second_journal(&runtime, &journal, &old_scope).await;
    fill_ordinary_lane(&runtime, &old_scope, "marker");
    runtime
        .emit_internal(internal_fact(
            "first-loss",
            &old_scope,
            "old_scope_loss_candidate",
        ))
        .unwrap();
    assert_eq!(runtime.queue_counts_for_test().2, 1);

    let new_scope = scope(2, "minecraft:nether");
    runtime
        .emit_internal(ParticipantInternalEvent::ScopeChanged {
            id: "scope-to-nether".to_owned(),
            occurred_at: "2026-08-03T00:04:00Z".to_owned(),
            scope: new_scope.clone(),
            reason: "dimension transition after ordinary loss".to_owned(),
        })
        .unwrap();
    assert_eq!(runtime.queue_counts_for_test().2, 1);
    journal.release_through(usize::MAX);
    tokio::time::timeout(
        Duration::from_secs(2),
        journal.wait_for_payload_ids(&["participant-overflow-19", "scope-to-nether"]),
    )
    .await
    .expect("marker and transition must both reach the journal");

    let payloads = journal.payloads();
    let marker = payloads
        .iter()
        .find(|payload| {
            payload.get("eventType").and_then(serde_json::Value::as_str)
                == Some("participant_events_omitted")
        })
        .expect("the first loss marker remains journal-visible");
    let transition = payloads
        .iter()
        .find(|payload| {
            payload.get("id").and_then(serde_json::Value::as_str) == Some("scope-to-nether")
        })
        .expect("the scope transition remains journal-visible");
    assert!(admission_ticket(marker) < admission_ticket(transition));

    journal.release_through(usize::MAX);
    journal.set_gate(false);
    let current_chat = chat_input(700, "Alice", "@Bot after marker");
    source.set_chats(vec![current_chat.clone()]);
    runtime
        .ingest_backend_event(scoped_chat_event_at(
            "chat-after-marker",
            "process-test",
            2,
            "world-test",
            "minecraft:nether",
            "Alice",
            "@Bot after marker",
            &current_chat.message.at,
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;
    let request = agent.requests.lock().unwrap()[0].clone();
    assert!(
        !request.context.frame.events.as_ref().is_some_and(|events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    mineintent_contracts::agent::AgentEventV5::Summary { event_type, .. }
                        if event_type == "participant_events_omitted"
                )
            })
        })
    );
    assert_eq!(runtime.current_scope(), Some(new_scope));
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn lifecycle_controls_keep_ticket_fifo_when_ordinary_lane_is_full() {
    let agent = TestAgent::new(0);
    let (runtime, _source, journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let overworld = scope(1, "minecraft:overworld");
    hold_worker_on_second_journal(&runtime, &journal, &overworld).await;
    fill_ordinary_lane(&runtime, &overworld, "lifecycle");

    for event in [
        scoped_lifecycle_event(
            "connection-requested-lane",
            "process-test",
            1,
            "attempt-test",
            "world-test",
            Some("minecraft:overworld"),
            BackendLifecyclePayload::ConnectionRequested { attempt: 1 },
        ),
        scoped_lifecycle_event(
            "logged-in-lane",
            "process-test",
            1,
            "attempt-test",
            "world-test",
            Some("minecraft:overworld"),
            BackendLifecyclePayload::LoggedIn {
                version: "1.21".to_owned(),
                dimension: "minecraft:overworld".to_owned(),
            },
        ),
        scoped_lifecycle_event(
            "ready-lane",
            "process-test",
            1,
            "attempt-test",
            "world-test",
            Some("minecraft:overworld"),
            BackendLifecyclePayload::Ready {
                snapshot_revision: 1,
            },
        ),
        scoped_lifecycle_event(
            "dimension-changed-lane",
            "process-test",
            1,
            "attempt-test",
            "world-test",
            Some("minecraft:nether"),
            BackendLifecyclePayload::DimensionChanged {
                from: "minecraft:overworld".to_owned(),
                to: "minecraft:nether".to_owned(),
            },
        ),
    ] {
        runtime.ingest_backend_event(event).unwrap();
    }

    let (ordinary, control, overflow, terminal, waiting) = runtime.queue_counts_for_test();
    assert_eq!(ordinary, TEST_ORDINARY_CAPACITY);
    assert_eq!(control, 4);
    assert_eq!(overflow, 0);
    assert_eq!(terminal, 0);
    assert_eq!(waiting, 0);

    journal.release_through(usize::MAX);
    journal.set_gate(false);
    tokio::time::timeout(
        Duration::from_secs(2),
        journal.wait_for_payload_ids(&[
            "connection-requested-lane",
            "logged-in-lane",
            "ready-lane",
            "dimension-changed-lane",
        ]),
    )
    .await
    .expect("the complete lifecycle control batch must reach the journal");
    let lifecycle_ids: Vec<String> = journal
        .payloads()
        .iter()
        .filter_map(|payload| {
            let id = payload.get("id").and_then(serde_json::Value::as_str)?;
            id.ends_with("-lane").then(|| id.to_owned())
        })
        .collect();
    assert_eq!(
        lifecycle_ids,
        vec![
            "connection-requested-lane",
            "logged-in-lane",
            "ready-lane",
            "dimension-changed-lane",
        ]
    );
    let lifecycle_tickets: Vec<u64> = journal
        .payloads()
        .iter()
        .filter_map(|payload| {
            let id = payload.get("id").and_then(serde_json::Value::as_str)?;
            id.ends_with("-lane").then(|| admission_ticket(payload))
        })
        .collect();
    assert!(lifecycle_tickets
        .windows(2)
        .all(|tickets| tickets[0] < tickets[1]));
    let all_tickets: Vec<u64> = journal.payloads().iter().map(admission_ticket).collect();
    assert!(all_tickets
        .windows(2)
        .all(|tickets| tickets[0] < tickets[1]));
    assert!(!journal.payloads().iter().any(|payload| {
        payload.get("eventType").and_then(serde_json::Value::as_str)
            == Some("participant_events_omitted")
    }));
    assert_eq!(runtime.current_scope(), Some(scope(1, "minecraft:nether")));
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn stale_scope_chat_cannot_drain_current_pending_fact() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let current = scope(2, "minecraft:overworld");
    runtime
        .emit_internal(ParticipantInternalEvent::Fact {
            id: "health-2".to_owned(),
            occurred_at: "2026-08-03T00:02:00Z".to_owned(),
            scope: current,
            event_type: "health_baseline".to_owned(),
            summary: "health baseline scope two".to_owned(),
        })
        .unwrap();
    source.set_chats(vec![chat_input(30, "Alice", "@Bot stale")]);
    assert!(matches!(
        runtime.ingest_backend_event(chat_event("51", 1, "Alice", "@Bot stale")),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert!(agent.requests.lock().unwrap().is_empty());
    let current_chat = chat_input(31, "Alice", "@Bot current");
    source.set_chats(vec![current_chat]);
    runtime
        .ingest_backend_event(chat_event("52", 2, "Alice", "@Bot current"))
        .unwrap();
    wait_for_request(&agent, 1).await;
    let request = agent.requests.lock().unwrap().remove(0);
    assert!(request.context.frame.events.as_ref().unwrap().iter().any(|event| {
        matches!(event, mineintent_contracts::agent::AgentEventV5::Summary { event_type, .. } if event_type == "health_baseline")
    }));
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn retired_process_sessions_cannot_reactivate_old_scope() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    let process_a = ParticipantScope::new(
        "process-A",
        5,
        "world-A",
        Some("minecraft:overworld".to_owned()),
    );
    source.set_chats(Vec::new());
    runtime
        .ingest_backend_event(scoped_chat_event(
            "a-fact",
            &process_a.process_session_id,
            process_a.connection_epoch,
            &process_a.world_id,
            process_a.dimension.as_deref().unwrap(),
            "Alice",
            "A-only fact",
        ))
        .unwrap();

    let process_b = ParticipantScope::new(
        "process-B",
        1,
        "world-B",
        Some("minecraft:overworld".to_owned()),
    );
    let first_b = chat_input(101, "Alice", "@Bot B first");
    source.set_chats(vec![first_b]);
    runtime
        .ingest_backend_event(scoped_chat_event(
            "b-first",
            &process_b.process_session_id,
            process_b.connection_epoch,
            &process_b.world_id,
            process_b.dimension.as_deref().unwrap(),
            "Alice",
            "@Bot B first",
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;
    let first_request = agent.requests.lock().unwrap()[0].clone();
    assert!(!first_request
        .context
        .frame
        .events
        .as_ref()
        .is_some_and(|events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    mineintent_contracts::agent::AgentEventV5::Summary { event_type, .. }
                        if event_type == "player_chat_not_addressed"
                )
            })
        }));

    source.set_chats(Vec::new());
    assert!(matches!(
        runtime.ingest_backend_event(scoped_chat_event(
            "a-late",
            "process-A",
            6,
            "world-A",
            "minecraft:overworld",
            "Alice",
            "A late fact",
        )),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert!(matches!(
        runtime.ingest_backend_event(dimension_changed_event(
            "a-transition-late",
            "process-A",
            6,
            "world-A",
            "minecraft:nether",
            "minecraft:overworld",
            "minecraft:nether",
        )),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert!(matches!(
        runtime.ingest_backend_event(dimension_changed_event(
            "b-transition-low",
            "process-B",
            0,
            "world-B",
            "minecraft:nether",
            "minecraft:overworld",
            "minecraft:nether",
        )),
        Ok(ParticipantAdmission::Ignored)
    ));

    assert!(matches!(
        runtime.ingest_backend_event(dimension_changed_event(
            "b-transition-valid",
            "process-B",
            1,
            "world-B",
            "minecraft:nether",
            "minecraft:overworld",
            "minecraft:nether",
        )),
        Ok(ParticipantAdmission::Recorded)
    ));
    let b_scope = ParticipantScope::new(
        "process-B",
        1,
        "world-B",
        Some("minecraft:nether".to_owned()),
    );
    runtime
        .emit_internal(ParticipantInternalEvent::Fact {
            id: "b-health".to_owned(),
            occurred_at: "2026-08-03T00:03:00Z".to_owned(),
            scope: b_scope.clone(),
            event_type: "health_baseline".to_owned(),
            summary: "B health baseline".to_owned(),
        })
        .unwrap();
    let second_b = chat_input(102, "Alice", "@Bot B second");
    source.set_chats(vec![second_b]);
    runtime
        .ingest_backend_event(scoped_chat_event(
            "b-second",
            "process-B",
            1,
            "world-B",
            "minecraft:nether",
            "Alice",
            "@Bot B second",
        ))
        .unwrap();
    wait_for_request(&agent, 2).await;
    let second_request = agent.requests.lock().unwrap()[1].clone();
    assert!(second_request
        .context
        .frame
        .events
        .as_ref()
        .is_some_and(|events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    mineintent_contracts::agent::AgentEventV5::Summary { event_type, .. }
                        if event_type == "health_baseline"
                )
            })
        }));
    assert_eq!(runtime.current_scope(), Some(b_scope));
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn stale_internal_scope_events_cannot_pollute_current_scope() {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, speech, motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    runtime
        .emit_internal(ParticipantInternalEvent::Fact {
            id: "a-seed".to_owned(),
            occurred_at: "2026-08-03T00:04:00Z".to_owned(),
            scope: ParticipantScope::new(
                "process-A",
                5,
                "world-A",
                Some("minecraft:overworld".to_owned()),
            ),
            event_type: "old_fact".to_owned(),
            summary: "old process fact".to_owned(),
        })
        .unwrap();

    let b_scope = ParticipantScope::new(
        "process-B",
        1,
        "world-B",
        Some("minecraft:overworld".to_owned()),
    );
    source.set_chats(vec![chat_input(110, "Alice", "@Bot B active")]);
    runtime
        .ingest_backend_event(scoped_chat_event(
            "b-active",
            "process-B",
            1,
            "world-B",
            "minecraft:overworld",
            "Alice",
            "@Bot B active",
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;
    let releases_before_stale = motor.releases.load(Ordering::SeqCst);
    let speech_cancels_before_stale = speech.cancelled.load(Ordering::SeqCst);

    runtime
        .emit_internal(ParticipantInternalEvent::Fact {
            id: "b-health".to_owned(),
            occurred_at: "2026-08-03T00:04:01Z".to_owned(),
            scope: b_scope.clone(),
            event_type: "health_baseline".to_owned(),
            summary: "B health baseline".to_owned(),
        })
        .unwrap();
    let old_scope = ParticipantScope::new(
        "process-A",
        6,
        "world-A",
        Some("minecraft:nether".to_owned()),
    );
    assert!(matches!(
        runtime.emit_internal(ParticipantInternalEvent::Fact {
            id: "a-late-fact".to_owned(),
            occurred_at: "2026-08-03T00:04:02Z".to_owned(),
            scope: old_scope.clone(),
            event_type: "old_health".to_owned(),
            summary: "late A fact".to_owned(),
        }),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert!(matches!(
        runtime.emit_internal(ParticipantInternalEvent::ScopeChanged {
            id: "a-late-transition".to_owned(),
            occurred_at: "2026-08-03T00:04:03Z".to_owned(),
            scope: old_scope,
            reason: "late A transition".to_owned(),
        }),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert_eq!(runtime.current_scope(), Some(b_scope.clone()));
    assert_eq!(motor.releases.load(Ordering::SeqCst), releases_before_stale);
    assert_eq!(
        speech.cancelled.load(Ordering::SeqCst),
        speech_cancels_before_stale
    );
    assert_eq!(agent.requests.lock().unwrap().len(), 1);

    source.set_chats(vec![chat_input(111, "Alice", "@Bot B second")]);
    runtime
        .ingest_backend_event(scoped_chat_event(
            "b-second-internal-regression",
            "process-B",
            1,
            "world-B",
            "minecraft:overworld",
            "Alice",
            "@Bot B second",
        ))
        .unwrap();
    agent.release();
    wait_for_request(&agent, 2).await;
    let second_request = agent.requests.lock().unwrap()[1].clone();
    let events = second_request.context.frame.events.as_ref().unwrap();
    assert!(events.iter().any(|event| {
        matches!(
            event,
            mineintent_contracts::agent::AgentEventV5::Summary { event_type, .. }
                if event_type == "health_baseline"
        )
    }));
    assert!(!events.iter().any(|event| {
        matches!(
            event,
            mineintent_contracts::agent::AgentEventV5::Summary { event_type, .. }
                if event_type == "old_health"
        )
    }));
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn retired_process_identity_is_not_evicted_after_many_reconnects() {
    let agent = TestAgent::new(0);
    let (runtime, _source, _journal, speech, motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    for index in 0..10_u64 {
        let process = format!("process-{index}");
        let admission = runtime.ingest_backend_event(scoped_chat_event(
            &format!("ordinary-{index}"),
            &process,
            0,
            &format!("world-{index}"),
            "minecraft:overworld",
            "Alice",
            "ordinary message",
        ));
        assert!(matches!(admission, Ok(ParticipantAdmission::Recorded)));
    }
    assert_eq!(
        runtime.current_scope(),
        Some(ParticipantScope::new(
            "process-9",
            0,
            "world-9",
            Some("minecraft:overworld".to_owned()),
        ))
    );
    let releases_before_stale = motor.releases.load(Ordering::SeqCst);
    let speech_cancels_before_stale = speech.cancelled.load(Ordering::SeqCst);
    let old_scope = ParticipantScope::new(
        "process-0",
        99,
        "world-0",
        Some("minecraft:nether".to_owned()),
    );
    assert!(matches!(
        runtime.ingest_backend_event(scoped_chat_event(
            "late-ordinary-0",
            "process-0",
            99,
            "world-0",
            "minecraft:nether",
            "Alice",
            "ordinary late message",
        )),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert!(matches!(
        runtime.emit_internal(ParticipantInternalEvent::ScopeChanged {
            id: "late-transition-0".to_owned(),
            occurred_at: "2026-08-03T00:05:00Z".to_owned(),
            scope: old_scope,
            reason: "late old process transition".to_owned(),
        }),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert_eq!(
        runtime.current_scope(),
        Some(ParticipantScope::new(
            "process-9",
            0,
            "world-9",
            Some("minecraft:overworld".to_owned()),
        ))
    );
    assert_eq!(motor.releases.load(Ordering::SeqCst), releases_before_stale);
    assert_eq!(
        speech.cancelled.load(Ordering::SeqCst),
        speech_cancels_before_stale
    );
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn scope_change_cancels_active_run_and_drops_old_queue() {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, speech, motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let old = chat_input(40, "Alice", "@Bot old");
    source.set_chats(vec![old.clone()]);
    runtime
        .ingest_backend_event(chat_event("61", 1, "Alice", "@Bot old"))
        .unwrap();
    wait_for_request(&agent, 1).await;
    source.set_chats(vec![old.clone(), chat_input(41, "Bob", "@Bot queued old")]);
    runtime
        .ingest_backend_event(chat_event("62", 1, "Bob", "@Bot queued old"))
        .unwrap();

    runtime
        .emit_internal(ParticipantInternalEvent::ScopeChanged {
            id: "scope-2".to_owned(),
            occurred_at: "2026-08-03T00:03:00Z".to_owned(),
            scope: scope(2, "minecraft:nether"),
            reason: "dimension changed".to_owned(),
        })
        .unwrap();
    assert!(motor.releases.load(Ordering::SeqCst) >= 1);
    assert!(speech.cancelled.load(Ordering::SeqCst) >= 1);
    let new_chat = chat_input(42, "Alice", "@Bot new scope");
    source.set_chats(vec![new_chat]);
    runtime
        .ingest_backend_event(scoped_chat_event_at(
            "63",
            "process-test",
            2,
            "world-test",
            "minecraft:nether",
            "Alice",
            "@Bot new scope",
            &chat_input(42, "Alice", "@Bot new scope").message.at,
        ))
        .unwrap();
    wait_for_request(&agent, 2).await;
    assert_eq!(agent.texts(), vec!["@Bot old", "@Bot new scope"]);
    assert_eq!(source.retain_calls(), 3);
    assert_eq!(source.retained_count(), 0);
    assert_eq!(source.release_calls(), 3);
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn request_stop_releases_before_bounded_worker_settle_and_speech_cancel() {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, speech, motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    source.set_chats(vec![chat_input(50, "Alice", "@Bot stop")]);
    runtime
        .ingest_backend_event(chat_event("71", 1, "Alice", "@Bot stop"))
        .unwrap();
    wait_for_request(&agent, 1).await;
    assert!(runtime.request_stop().unwrap());
    assert!(motor.releases.load(Ordering::SeqCst) >= 1);
    assert!(speech.cancelled.load(Ordering::SeqCst) >= 1);
    runtime.stop().await.unwrap();
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
}

#[tokio::test]
async fn concurrent_stop_uses_one_cleanup_and_completion_owner() {
    let agent = TestAgent::new(0);
    let (runtime, _source, _journal, speech, motor, backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let gate = speech.gate_cleanup();

    let first_runtime = Arc::clone(&runtime);
    let handle = tokio::runtime::Handle::current();
    let first = tokio::task::spawn_blocking(move || handle.block_on(first_runtime.stop()));
    gate.wait_started().await;

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (done_tx, mut done_rx) = tokio::sync::oneshot::channel();
    let second_runtime = Arc::clone(&runtime);
    let second = tokio::spawn(async move {
        let _ = entered_tx.send(());
        let result = second_runtime.stop().await;
        let _ = done_tx.send(());
        result
    });
    entered_rx.await.unwrap();
    tokio::task::yield_now().await;
    assert!(done_rx.try_recv().is_err());

    gate.release();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert!(backend.subscription_closed());
    assert_eq!(speech.cancelled.load(Ordering::SeqCst), 1);
    assert_eq!(motor.releases.load(Ordering::SeqCst), 1);
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
}

#[tokio::test]
async fn backend_listener_surfaces_source_error_and_uses_injected_debug_clock() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    source.fail_context.store(true, Ordering::SeqCst);
    let mut failures = runtime.subscribe_failures();
    backend.emit(chat_event("81", 1, "Alice", "@Bot secret"));
    let failure = tokio::time::timeout(Duration::from_secs(2), failures.recv())
        .await
        .unwrap()
        .unwrap();
    // 入队路径的失败带 ingest: 前缀，与 worker 路径可辨（两条路径的致命判据
    // 相同，但排障时必须知道是谁先报的）。
    assert_eq!(failure.code, "ingest:opening_frame_source_failed");
    assert!(!failure.summary.contains("secret"));
    let debug = runtime.debug_snapshot();
    assert_eq!(debug.recent_failures[0].at, "2026-08-03T00:00:00Z");
    assert!(!serde_json::to_string(&*debug).unwrap().contains("secret"));
    // 曾断言 Faulted，实测证伪：同伴在游戏里死一次，入队侧的 source 读取即
    // 失败，按旧行为整个同伴永久失聪。oracle 的 #recordFailure 只落盘不改
    // 生命周期（runtime.ts:552-557），worker 路径也把 Source 归为可恢复；
    // 本断言随致命判据统一而改为 Running。
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Running
    );
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn terminal_path_releases_queued_trigger_retention() {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, _speech, _motor, backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    let first = chat_input(501, "Alice", "@Bot terminal first");
    source.set_chats(vec![first.clone()]);
    runtime
        .ingest_backend_event(chat_event(
            "terminal-first",
            1,
            "Alice",
            "@Bot terminal first",
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;

    let second = chat_input(502, "Bob", "@Bot terminal queued");
    source.set_chats(vec![first, second]);
    runtime
        .ingest_backend_event(chat_event(
            "terminal-queued",
            1,
            "Bob",
            "@Bot terminal queued",
        ))
        .unwrap();
    assert_eq!(source.retained_count(), 1);

    backend.emit(lifecycle_event(
        "terminal-release",
        BackendLifecyclePayload::Stopped {
            reason: "release queued trigger".to_owned(),
        },
    ));
    wait_for_lifecycle(
        &runtime,
        mineintent_middle::participant::ParticipantLifecycle::Stopped,
    )
    .await;
    assert_eq!(source.retained_count(), 0);
    assert_eq!(source.retain_calls(), 2);
    assert_eq!(source.release_calls(), 2);
    assert!(source.release_all_calls() >= 1);
    runtime.stop().await.unwrap();
}

async fn assert_backend_terminal_event(
    payload: BackendLifecyclePayload,
    expected: mineintent_middle::participant::ParticipantLifecycle,
) {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, speech, motor, backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    source.set_chats(vec![chat_input(300, "Alice", "@Bot terminal")]);
    runtime
        .ingest_backend_event(chat_event("terminal-trigger", 1, "Alice", "@Bot terminal"))
        .unwrap();
    wait_for_request(&agent, 1).await;
    let releases_before = motor.releases.load(Ordering::SeqCst);
    let speech_cancels_before = speech.cancelled.load(Ordering::SeqCst);

    backend.emit(lifecycle_event("terminal-event", payload));
    assert!(motor.releases.load(Ordering::SeqCst) > releases_before);
    assert!(speech.cancelled.load(Ordering::SeqCst) > speech_cancels_before);
    assert_eq!(backend.subscription_unsubscribes(), 0);
    wait_for_lifecycle(&runtime, expected).await;
    assert_eq!(backend.subscription_unsubscribes(), 1);
    runtime.stop().await.unwrap();
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
    assert_eq!(backend.subscription_unsubscribes(), 1);
}

#[tokio::test]
async fn backend_terminal_events_teardown_after_journal_and_stop_is_bounded() {
    assert_backend_terminal_event(
        BackendLifecyclePayload::Stopped {
            reason: "backend stopped".to_owned(),
        },
        mineintent_middle::participant::ParticipantLifecycle::Stopped,
    )
    .await;
    assert_backend_terminal_event(
        BackendLifecyclePayload::Faulted {
            failure: BackendFailure {
                code: BackendFailureCode::ProtocolError,
                message: "protocol failure".to_owned(),
                retryable: false,
            },
        },
        mineintent_middle::participant::ParticipantLifecycle::Faulted,
    )
    .await;

    let agent = TestAgent::new(1);
    let (runtime, source, journal, speech, motor, backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    source.set_chats(vec![chat_input(301, "Alice", "@Bot gated terminal")]);
    runtime
        .ingest_backend_event(chat_event(
            "terminal-gated-trigger",
            1,
            "Alice",
            "@Bot gated terminal",
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;
    journal.set_gate(true);
    backend.emit(lifecycle_event(
        "terminal-gated",
        BackendLifecyclePayload::Stopped {
            reason: "journal gate".to_owned(),
        },
    ));
    assert!(motor.releases.load(Ordering::SeqCst) >= 1);
    assert!(speech.cancelled.load(Ordering::SeqCst) >= 1);
    tokio::time::timeout(Duration::from_secs(1), runtime.stop())
        .await
        .expect("stop must abort a terminal journal within the bounded fallback")
        .unwrap();
    assert_eq!(backend.subscription_unsubscribes(), 1);
    journal.set_gate(false);
}

#[tokio::test]
async fn backend_connection_closed_keeps_running_until_reconnect_or_terminal() {
    let agent = TestAgent::new(1);
    let (runtime, source, journal, speech, motor, backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    source.set_chats(vec![chat_input(400, "Alice", "@Bot before close")]);
    runtime
        .ingest_backend_event(chat_event("close-trigger", 1, "Alice", "@Bot before close"))
        .unwrap();
    wait_for_request(&agent, 1).await;
    let initial_journal_entries = journal.entries.lock().unwrap().len();
    let releases_before = motor.releases.load(Ordering::SeqCst);
    let speech_before = speech.cancelled.load(Ordering::SeqCst);

    backend.emit(scoped_lifecycle_event(
        "retryable-close",
        "process-test",
        1,
        "attempt-test",
        "world-test",
        Some("minecraft:overworld"),
        BackendLifecyclePayload::ConnectionClosed {
            close: BackendClose {
                epoch: 1,
                at: "2026-08-03T00:06:01Z".to_owned(),
                code: "transport_reset".to_owned(),
                retryable: true,
                deliberate: false,
                kick: None,
                error: None,
                end_reason: None,
            },
        },
    ));
    assert!(motor.releases.load(Ordering::SeqCst) > releases_before);
    assert!(speech.cancelled.load(Ordering::SeqCst) > speech_before);
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Running
    );
    assert_eq!(backend.subscription_unsubscribes(), 0);
    tokio::time::timeout(
        Duration::from_secs(2),
        journal.wait_for_entries(initial_journal_entries + 1),
    )
    .await
    .unwrap();
    let entries_after_close = journal.entries.lock().unwrap().len();

    assert!(matches!(
        runtime.ingest_backend_event(scoped_lifecycle_event(
            "wrong-reconnect-attempt",
            "process-test",
            1,
            "attempt-wrong",
            "world-test",
            Some("minecraft:overworld"),
            BackendLifecyclePayload::ReconnectScheduled {
                attempt: 2,
                retry_at: "2026-08-03T00:06:02Z".to_owned(),
                close_code: "transport_reset".to_owned(),
            },
        )),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert!(matches!(
        runtime.ingest_backend_event(scoped_lifecycle_event(
            "wrong-terminal-attempt",
            "process-test",
            1,
            "attempt-wrong",
            "world-test",
            Some("minecraft:overworld"),
            BackendLifecyclePayload::Faulted {
                failure: BackendFailure {
                    code: BackendFailureCode::ProtocolError,
                    message: "wrong attempt must not fault".to_owned(),
                    retryable: false,
                },
            },
        )),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert!(matches!(
        runtime.ingest_backend_event(scoped_lifecycle_event(
            "same-epoch-new-attempt",
            "process-test",
            1,
            "attempt-two",
            "world-test",
            Some("minecraft:overworld"),
            BackendLifecyclePayload::ConnectionRequested { attempt: 2 },
        )),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert!(matches!(
        runtime.ingest_backend_event(scoped_lifecycle_event(
            "higher-epoch-reused-attempt",
            "process-test",
            2,
            "attempt-test",
            "world-test",
            Some("minecraft:overworld"),
            BackendLifecyclePayload::ConnectionRequested { attempt: 2 },
        )),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert_eq!(runtime.current_scope(), None);
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Running
    );
    assert_eq!(journal.entries.lock().unwrap().len(), entries_after_close);

    source.set_chats(vec![chat_input(401, "Alice", "@Bot stale after close")]);
    assert!(matches!(
        runtime.ingest_backend_event(scoped_chat_event(
            "stale-after-close",
            "process-test",
            1,
            "world-test",
            "minecraft:overworld",
            "Alice",
            "@Bot stale after close",
        )),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert_eq!(agent.requests.lock().unwrap().len(), 1);
    assert_eq!(
        journal.entries.lock().unwrap().len(),
        initial_journal_entries + 1
    );

    backend.emit(scoped_lifecycle_event(
        "reconnect-scheduled",
        "process-test",
        1,
        "attempt-test",
        "world-test",
        Some("minecraft:overworld"),
        BackendLifecyclePayload::ReconnectScheduled {
            attempt: 2,
            retry_at: "2026-08-03T00:06:02Z".to_owned(),
            close_code: "transport_reset".to_owned(),
        },
    ));
    tokio::time::timeout(
        Duration::from_secs(2),
        journal.wait_for_entries(initial_journal_entries + 2),
    )
    .await
    .unwrap();
    assert_eq!(agent.requests.lock().unwrap().len(), 1);
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Running
    );
    assert_eq!(backend.subscription_unsubscribes(), 0);

    backend.emit(scoped_lifecycle_event(
        "connection-requested-2",
        "process-test",
        2,
        "attempt-two",
        "world-test",
        Some("minecraft:overworld"),
        BackendLifecyclePayload::ConnectionRequested { attempt: 2 },
    ));
    backend.emit(scoped_lifecycle_event(
        "logged-in-2",
        "process-test",
        2,
        "attempt-two",
        "world-test",
        Some("minecraft:overworld"),
        BackendLifecyclePayload::LoggedIn {
            version: "1.21".to_owned(),
            dimension: "minecraft:overworld".to_owned(),
        },
    ));
    backend.emit(scoped_lifecycle_event(
        "ready-2",
        "process-test",
        2,
        "attempt-two",
        "world-test",
        Some("minecraft:overworld"),
        BackendLifecyclePayload::Ready {
            snapshot_revision: 2,
        },
    ));
    source.set_chats(vec![chat_input(402, "Alice", "@Bot after reconnect")]);
    runtime
        .ingest_backend_event(scoped_chat_event_at_attempt(
            "chat-after-reconnect",
            "process-test",
            2,
            "attempt-two",
            "world-test",
            "minecraft:overworld",
            "Alice",
            "@Bot after reconnect",
            &chat_input(402, "Alice", "@Bot after reconnect").message.at,
        ))
        .unwrap();
    wait_for_request(&agent, 2).await;
    assert_eq!(
        runtime.current_scope(),
        Some(scope(2, "minecraft:overworld"))
    );
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Running
    );
    assert_eq!(backend.subscription_unsubscribes(), 0);

    backend.emit(scoped_lifecycle_event(
        "fatal-after-reconnect",
        "process-test",
        2,
        "attempt-two",
        "world-test",
        Some("minecraft:overworld"),
        BackendLifecyclePayload::Faulted {
            failure: BackendFailure {
                code: BackendFailureCode::ProtocolError,
                message: "fatal after reconnect".to_owned(),
                retryable: false,
            },
        },
    ));
    wait_for_lifecycle(
        &runtime,
        mineintent_middle::participant::ParticipantLifecycle::Faulted,
    )
    .await;
    assert_eq!(backend.subscription_unsubscribes(), 1);
    runtime.stop().await.unwrap();

    let second_agent = TestAgent::new(0);
    let (second_runtime, _source, second_journal, second_speech, second_motor, second_backend) =
        runtime_parts(Arc::clone(&second_agent));
    second_runtime.start_worker().unwrap();
    second_backend.emit(lifecycle_event(
        "deliberate-close-requested",
        BackendLifecyclePayload::ConnectionRequested { attempt: 1 },
    ));
    second_backend.emit(lifecycle_event(
        "deliberate-close-ready",
        BackendLifecyclePayload::Ready {
            snapshot_revision: 1,
        },
    ));
    tokio::time::timeout(Duration::from_secs(2), second_journal.wait_for_entries(2))
        .await
        .unwrap();
    assert_eq!(
        second_runtime.current_scope(),
        Some(scope(1, "minecraft:overworld"))
    );
    let second_releases_before = second_motor.releases.load(Ordering::SeqCst);
    let second_speech_before = second_speech.cancelled.load(Ordering::SeqCst);
    second_backend.emit(scoped_lifecycle_event(
        "deliberate-close",
        "process-test",
        1,
        "attempt-test",
        "world-test",
        Some("minecraft:overworld"),
        BackendLifecyclePayload::ConnectionClosed {
            close: BackendClose {
                epoch: 1,
                at: "2026-08-03T00:06:03Z".to_owned(),
                code: "requested_disconnect".to_owned(),
                retryable: false,
                deliberate: true,
                kick: None,
                error: None,
                end_reason: None,
            },
        },
    ));
    assert!(second_motor.releases.load(Ordering::SeqCst) > second_releases_before);
    assert!(second_speech.cancelled.load(Ordering::SeqCst) > second_speech_before);
    assert_eq!(second_runtime.current_scope(), None);
    assert_eq!(
        second_runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Running
    );
    assert_eq!(second_backend.subscription_unsubscribes(), 0);
    tokio::time::timeout(Duration::from_secs(2), second_journal.wait_for_entries(3))
        .await
        .unwrap();
    second_backend.emit(lifecycle_event(
        "stopped-after-deliberate-close",
        BackendLifecyclePayload::Stopped {
            reason: "backend stop confirmed".to_owned(),
        },
    ));
    wait_for_lifecycle(
        &second_runtime,
        mineintent_middle::participant::ParticipantLifecycle::Stopped,
    )
    .await;
    assert_eq!(second_backend.subscription_unsubscribes(), 1);
    second_runtime.stop().await.unwrap();
}

#[tokio::test]
async fn agent_assembly_rejects_tool_definition_drift() {
    let registry = Arc::new(ToolCapabilityRegistry::new(Vec::new()).unwrap());
    let factory = Arc::new(AssemblyFactory {
        registry: Arc::clone(&registry),
        runner_registry: Arc::clone(&registry),
        bindings: Arc::new(Mutex::new(Vec::new())),
    });
    let assembly = ParticipantAgentAssembly::new(factory);
    assert!(assembly.definitions().is_empty());
    let mut request = AgentRunRequest {
        run_id: mineintent_contracts::agent::RunId::new("assembly-test").unwrap(),
        context: fixtures::agent_context_v5(),
        tools: Vec::new(),
        prompt_template: fixtures::prompt_template(),
    };
    request.tools.push(
        serde_json::from_value(serde_json::json!({
            "type": "function",
            "function": {
                "name": "drift",
                "description": "drift",
                "parameters": {"type": "object"}
            }
        }))
        .unwrap(),
    );
    let signal = NeverCancelled;
    let deadline = mineintent_contracts::agent::Deadline::after(
        std::time::Instant::now(),
        Duration::from_secs(1),
    )
    .unwrap();
    let result = assembly
        .run(
            scope(1, "minecraft:overworld"),
            0,
            "assembly-event".to_owned(),
            request,
            ExecutionControl::new(&signal, deadline),
        )
        .await;
    assert_eq!(result.unwrap_err().code, AgentErrorCode::InvalidRequest);
}

#[tokio::test]
async fn agent_factory_binds_each_wake_scope_and_trigger_identity() {
    let registry = Arc::new(ToolCapabilityRegistry::new(Vec::new()).unwrap());
    let bindings = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(AssemblyFactory {
        registry: Arc::clone(&registry),
        runner_registry: Arc::clone(&registry),
        bindings: Arc::clone(&bindings),
    });
    let assembly = ParticipantAgentAssembly::new(factory);
    let signal = NeverCancelled;
    let scope_one = ParticipantScope::new(
        "process-one",
        1,
        "world-one",
        Some("minecraft:overworld".to_owned()),
    );
    let scope_two = ParticipantScope::new(
        "process-two",
        2,
        "world-two",
        Some("minecraft:nether".to_owned()),
    );
    let deadline_one = mineintent_contracts::agent::Deadline::after(
        std::time::Instant::now(),
        Duration::from_secs(1),
    )
    .unwrap();
    assembly
        .run(
            scope_one.clone(),
            1,
            "chat-one".to_owned(),
            AgentRunRequest {
                run_id: mineintent_contracts::agent::RunId::new("factory-one").unwrap(),
                context: fixtures::agent_context_v5(),
                tools: Vec::new(),
                prompt_template: fixtures::prompt_template(),
            },
            ExecutionControl::new(&signal, deadline_one),
        )
        .await
        .unwrap();
    let deadline_two = mineintent_contracts::agent::Deadline::after(
        std::time::Instant::now(),
        Duration::from_secs(1),
    )
    .unwrap();
    assembly
        .run(
            scope_two.clone(),
            2,
            "chat-two".to_owned(),
            AgentRunRequest {
                run_id: mineintent_contracts::agent::RunId::new("factory-two").unwrap(),
                context: fixtures::agent_context_v5(),
                tools: Vec::new(),
                prompt_template: fixtures::prompt_template(),
            },
            ExecutionControl::new(&signal, deadline_two),
        )
        .await
        .unwrap();
    assert_eq!(
        *bindings.lock().unwrap(),
        vec![
            (scope_one, "chat-one".to_owned()),
            (scope_two, "chat-two".to_owned()),
        ]
    );
}

#[tokio::test]
async fn agent_assembly_rejects_runner_bound_to_foreign_registry() {
    let registry = Arc::new(ToolCapabilityRegistry::new(Vec::new()).unwrap());
    let foreign_registry = Arc::new(ToolCapabilityRegistry::new(Vec::new()).unwrap());
    let factory = Arc::new(AssemblyFactory {
        registry,
        runner_registry: foreign_registry,
        bindings: Arc::new(Mutex::new(Vec::new())),
    });
    let assembly = ParticipantAgentAssembly::new(factory);
    let signal = NeverCancelled;
    let deadline = mineintent_contracts::agent::Deadline::after(
        std::time::Instant::now(),
        Duration::from_secs(1),
    )
    .unwrap();
    let result = assembly
        .run(
            scope(1, "minecraft:overworld"),
            1,
            "foreign-registry".to_owned(),
            AgentRunRequest {
                run_id: mineintent_contracts::agent::RunId::new("foreign-registry").unwrap(),
                context: fixtures::agent_context_v5(),
                tools: Vec::new(),
                prompt_template: fixtures::prompt_template(),
            },
            ExecutionControl::new(&signal, deadline),
        )
        .await;
    assert_eq!(result.unwrap_err().code, AgentErrorCode::InvalidRequest);
}

#[tokio::test]
async fn real_concrete_runner_uses_one_registry_and_rebinds_scope_per_wake() {
    let motor = TestMotor::new();
    let backend = TestBackend::new(Arc::clone(&motor));
    let backend_api: Arc<dyn MinecraftBackendApi> = backend.clone();
    let journal = TestJournal::new();
    let speech = TestSpeech::new();
    let memory = Arc::new(RealMemoryPort::default());
    let services = ProductionCapabilityServices::new(
        Arc::clone(&backend_api),
        Arc::new(ViewportReader::new(backend_api)),
        journal.clone(),
        speech,
        memory.clone(),
    );
    let registry = build_production_capability_registry(services).unwrap();
    let definitions = registry.definitions();
    let names: Vec<_> = definitions
        .iter()
        .map(|definition| definition.function.name.as_str().to_owned())
        .collect();
    assert_eq!(
        names,
        vec![
            "look_relative",
            "move_input",
            "respawn",
            "view",
            "say",
            "remember"
        ]
    );

    let bindings = Arc::new(Mutex::new(Vec::new()));
    let model_requests = Arc::new(Mutex::new(Vec::new()));
    let scope_checks = Arc::new(AtomicUsize::new(0));
    let factory = Arc::new(ConcreteRegistryFactory {
        registry: Arc::clone(&registry),
        bindings: Arc::clone(&bindings),
        model_requests: Arc::clone(&model_requests),
        scope_checks: Arc::clone(&scope_checks),
    });
    let assembly = ParticipantAgentAssembly::new(factory.clone());
    assert!(Arc::ptr_eq(&assembly.registry(), &registry));
    assert_eq!(assembly.definitions(), definitions);

    let scope_one = ParticipantScope::new(
        "real-process",
        7,
        "world-one",
        Some("minecraft:overworld".to_owned()),
    );
    let scope_two = ParticipantScope::new(
        "real-process",
        8,
        "world-two",
        Some("minecraft:nether".to_owned()),
    );
    let signal = NeverCancelled;
    for (scope, trigger, run_id) in [
        (scope_one.clone(), "real-chat-one", "real-run-one"),
        (scope_two.clone(), "real-chat-two", "real-run-two"),
    ] {
        let deadline = mineintent_contracts::agent::Deadline::after(
            std::time::Instant::now(),
            Duration::from_secs(1),
        )
        .unwrap();
        assembly
            .run(
                scope,
                1,
                trigger.to_owned(),
                AgentRunRequest {
                    run_id: mineintent_contracts::agent::RunId::new(run_id).unwrap(),
                    context: fixtures::agent_context_v5(),
                    tools: definitions.clone(),
                    prompt_template: fixtures::prompt_template(),
                },
                ExecutionControl::new(&signal, deadline),
            )
            .await
            .unwrap();
    }

    assert_eq!(
        *bindings.lock().unwrap(),
        vec![
            (scope_one, "real-chat-one".to_owned()),
            (scope_two, "real-chat-two".to_owned()),
        ]
    );
    assert_eq!(memory.appends.load(Ordering::SeqCst), 2);
    assert!(scope_checks.load(Ordering::SeqCst) >= 2);
    assert_eq!(
        model_requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.tools == definitions)
            .count(),
        4
    );
    assert_eq!(
        journal.entries.lock().unwrap().as_slice(),
        &[
            "memory.remembered".to_owned(),
            "memory.remembered".to_owned(),
        ]
    );
}

#[tokio::test]
async fn reconstructed_runtimes_have_distinct_bounded_run_ids() {
    let first_agent = TestAgent::new(0);
    let (first_runtime, first_source, _journal, _speech, _motor, _backend) =
        runtime_parts_with_namespace(Arc::clone(&first_agent), "same-session");
    first_runtime.start_worker().unwrap();
    first_source.set_chats(vec![chat_input(90, "Alice", "@Bot first id")]);
    first_runtime
        .ingest_backend_event(chat_event("90", 1, "Alice", "@Bot first id"))
        .unwrap();
    wait_for_request(&first_agent, 1).await;
    let first_id = first_agent.requests.lock().unwrap()[0].run_id.to_string();
    first_runtime.stop().await.unwrap();

    let second_agent = TestAgent::new(0);
    let (second_runtime, second_source, _journal, _speech, _motor, _backend) =
        runtime_parts_with_namespace(Arc::clone(&second_agent), "same-session");
    second_runtime.start_worker().unwrap();
    second_source.set_chats(vec![chat_input(91, "Alice", "@Bot second id")]);
    second_runtime
        .ingest_backend_event(chat_event("91", 1, "Alice", "@Bot second id"))
        .unwrap();
    wait_for_request(&second_agent, 1).await;
    let second_id = second_agent.requests.lock().unwrap()[0].run_id.to_string();
    second_runtime.stop().await.unwrap();

    assert_ne!(first_id, second_id);
    assert!(first_id.chars().count() <= 128);
    assert!(second_id.chars().count() <= 128);
    assert!(!first_id.contains("same-session"));
    assert!(!first_id.contains("process-test"));
    assert!(first_id.starts_with("p-"));
    assert!(first_id.split('-').skip(1).all(|part| part
        .chars()
        .all(|character: char| character.is_ascii_hexdigit()
            || character.is_ascii_digit()
            || character.is_ascii_lowercase())));

    let max_namespace = "n".repeat(128);
    let max_agent = TestAgent::new(0);
    let (max_runtime, max_source, _journal, _speech, _motor, _backend) =
        runtime_parts_with_namespace(Arc::clone(&max_agent), &max_namespace);
    max_runtime.start_worker().unwrap();
    max_source.set_chats(vec![chat_input(92, "Alice", "@Bot max id")]);
    max_runtime
        .ingest_backend_event(chat_event("92", 1, "Alice", "@Bot max id"))
        .unwrap();
    wait_for_request(&max_agent, 1).await;
    assert!(
        max_agent.requests.lock().unwrap()[0]
            .run_id
            .to_string()
            .chars()
            .count()
            <= 128
    );
    max_runtime.stop().await.unwrap();

    let invalid_agent = TestAgent::new(0);
    let invalid_motor = TestMotor::new();
    let invalid_config = ParticipantRuntimeConfig {
        backend: TestBackend::new(Arc::clone(&invalid_motor)),
        agent: invalid_agent,
        frame_source: TestFrameSource::new(),
        memory: Arc::new(TestMemory),
        journal: TestJournal::new(),
        speech: TestSpeech::new(),
        debug: Arc::new(DebugStateStore::new()),
        clock: Arc::new(TestClock),
        prompt_template: fixtures::prompt_template(),
        run_deadline: Duration::from_secs(30),
        wake_registry: WakeRegistry::default(),
        run_id_namespace: "x".repeat(129),
    };
    assert!(ParticipantRuntime::try_new(invalid_config).is_err());
}

#[derive(Default)]
struct RealMemoryPort {
    appends: AtomicUsize,
}

impl MemoryStorePort for RealMemoryPort {
    fn append<'a>(&'a self, _text: String) -> ContractFuture<'a, Result<(), MemoryError>> {
        self.appends.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }

    fn replace<'a>(
        &'a self,
        _old_text: String,
        _new_text: String,
    ) -> ContractFuture<'a, Result<(), MemoryError>> {
        Box::pin(async { Ok(()) })
    }

    fn rewrite<'a>(&'a self, _text: String) -> ContractFuture<'a, Result<(), MemoryError>> {
        Box::pin(async { Ok(()) })
    }
}

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

struct RealActionIds;

impl CapabilityActionIdSource for RealActionIds {
    fn next_action_id(&self, invocation: &ToolInvocation) -> Result<String, AgentError> {
        Ok(format!("real-action-{}", invocation.tool_call_id))
    }
}

struct RealUtc;

impl CapabilityUtcTimestampSource for RealUtc {
    fn now_utc(&self) -> Result<String, AgentError> {
        Ok("2026-08-03T00:00:00Z".to_owned())
    }
}

struct RealRegistryModel {
    requests: Arc<Mutex<Vec<AgentModelRequest>>>,
    calls: AtomicUsize,
}

impl ModelProvider for RealRegistryModel {
    type Request = AgentModelRequest;
    type Response = ModelCompletion;

    fn complete<'a>(
        &'a self,
        request: Self::Request,
        _control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<Self::Response, AgentError>> {
        self.requests.lock().unwrap().push(request);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let message = if call == 0 {
            serde_json::json!({
                "role": "assistant",
                "tool_calls": [{
                    "id": "real-remember-call",
                    "function": {
                        "name": "remember",
                        "arguments": "{\"operation\":\"append\",\"text\":\"real scope fact\"}"
                    }
                }]
            })
        } else {
            serde_json::json!({"role": "assistant", "content": "done"})
        };
        Box::pin(async move {
            Ok(ModelCompletion {
                message: message.as_object().cloned(),
                finish_reason: None,
                usage: Some(ModelUsage::default()),
            })
        })
    }
}

struct ConcreteRegistryFactory {
    registry: Arc<ToolCapabilityRegistry>,
    bindings: Arc<Mutex<Vec<(ParticipantScope, String)>>>,
    model_requests: Arc<Mutex<Vec<AgentModelRequest>>>,
    scope_checks: Arc<AtomicUsize>,
}

impl ParticipantAgentFactory for ConcreteRegistryFactory {
    fn registry(&self) -> Arc<ToolCapabilityRegistry> {
        Arc::clone(&self.registry)
    }

    fn build(
        &self,
        scope: &ParticipantScope,
        _generation: u64,
        trigger_event_id: &str,
    ) -> Result<Arc<dyn ParticipantScopedAgentRunner>, AgentError> {
        self.bindings
            .lock()
            .unwrap()
            .push((scope.clone(), trigger_event_id.to_owned()));
        let scope_guard: Arc<dyn ScopeGuard> = Arc::new(RealScopeGuard {
            checks: Arc::clone(&self.scope_checks),
        });
        let dispatcher = RegistryToolDispatcher::new(
            Arc::clone(&self.registry),
            Default::default(),
            Arc::new(ExplicitCapabilityInvocationAssembler::new(
                Arc::new(RealActionIds),
                Arc::new(RealUtc),
            )),
            Arc::new(CapabilityScopeAssembly::new(
                scope.world_id.clone(),
                trigger_event_id.to_owned(),
                scope_guard,
            )),
        );
        Ok(Arc::new(ConcreteAgentRunner::new(
            RealRegistryModel {
                requests: Arc::clone(&self.model_requests),
                calls: AtomicUsize::new(0),
            },
            dispatcher,
            mineintent_contracts::agent::ModelName::new("real-participant-model").unwrap(),
        )))
    }
}

struct AssemblyFactory {
    registry: Arc<ToolCapabilityRegistry>,
    runner_registry: Arc<ToolCapabilityRegistry>,
    bindings: Arc<Mutex<Vec<(ParticipantScope, String)>>>,
}

impl ParticipantAgentFactory for AssemblyFactory {
    fn registry(&self) -> Arc<ToolCapabilityRegistry> {
        Arc::clone(&self.registry)
    }

    fn build(
        &self,
        scope: &ParticipantScope,
        _generation: u64,
        trigger_event_id: &str,
    ) -> Result<Arc<dyn ParticipantScopedAgentRunner>, AgentError> {
        self.bindings
            .lock()
            .unwrap()
            .push((scope.clone(), trigger_event_id.to_owned()));
        Ok(Arc::new(AssemblyRunner {
            registry: Arc::clone(&self.runner_registry),
        }))
    }
}

struct AssemblyRunner {
    registry: Arc<ToolCapabilityRegistry>,
}

impl ParticipantRegistryBound for AssemblyRunner {
    fn tool_registry(&self) -> Arc<ToolCapabilityRegistry> {
        Arc::clone(&self.registry)
    }
}

impl AgentRunner for AssemblyRunner {
    type Context = JsonAgentDecisionContextV5;

    fn run<'a>(
        &'a self,
        _request: AgentRunRequest<Self::Context>,
        _control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<ModelRunResult, AgentError>> {
        Box::pin(async { Ok(fixtures::model_run_result()) })
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

/// 实测抓到的移植偏差回归：模型 provider 一次失败曾把整个 runtime 打成
/// Faulted，此后任何指名聊天都不再唤醒（同伴永久失聪）。oracle
/// runtime.ts:311-314 只 catch 住记 model.decision_failed 并继续。
/// 本回归钉住：失败后 runtime 仍 Running，且下一次唤醒照常进模型。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_provider_failure_ends_the_run_not_the_participant() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    agent.fail.store(true, Ordering::SeqCst);
    let first = chat_input(10, "Alice", "@Bot first");
    source.set_chats(vec![first.clone()]);
    runtime
        .ingest_backend_event(chat_event("41", 1, "Alice", "@Bot first"))
        .unwrap();
    wait_for_request(&agent, 1).await;
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Running,
        "模型失败不得让同伴进入 Faulted"
    );

    agent.fail.store(false, Ordering::SeqCst);
    let second = chat_input(11, "Bob", "@Bot second");
    source.set_chats(vec![first, second]);
    runtime
        .ingest_backend_event(chat_event("42", 1, "Bob", "@Bot second"))
        .unwrap();
    wait_for_request(&agent, 2).await;
    assert_eq!(
        agent.texts(),
        vec!["@Bot first", "@Bot second"],
        "失败之后的唤醒必须照常进入模型"
    );
    runtime.stop().await.unwrap();
}

/// 实测抓到的第二处移植偏差回归：入队路径把瞬时 source 错误当致命，
/// 同伴在游戏里死一次就永久 Faulted。致命判据必须与 worker 路径同一条规则。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transient_source_failure_during_ingest_does_not_fault_the_participant() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    source.fail_context.store(true, Ordering::SeqCst);
    // 走监听器路径：致命判定发生在 on_event，不在公开的 ingest 返回值上。
    mineintent_contracts::minecraft::BackendEventListener::on_event(
        runtime.as_ref(),
        chat_event("51", 1, "Alice", "@Bot while dead"),
    );
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Running,
        "瞬时 source 失败不得打死同伴"
    );

    source.fail_context.store(false, Ordering::SeqCst);
    let recovered = chat_input(12, "Bob", "@Bot after recovery");
    source.set_chats(vec![recovered]);
    runtime
        .ingest_backend_event(chat_event("52", 1, "Bob", "@Bot after recovery"))
        .unwrap();
    wait_for_request(&agent, 1).await;
    assert_eq!(
        agent.texts(),
        vec!["@Bot after recovery"],
        "恢复后必须照常唤醒"
    );
    runtime.stop().await.unwrap();
}
