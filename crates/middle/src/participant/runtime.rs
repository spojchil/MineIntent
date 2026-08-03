//! Production Participant runtime.
//!
//! The runtime is intentionally a small composition layer.  Backend callbacks,
//! internal facts, and wake candidates enter one synchronous admission point;
//! the worker then journals those admitted events in order and runs only the
//! registered wake policies.  All model-visible facts come from the explicit
//! frame source rather than from a fallback snapshot assembled here.

use std::{
    collections::VecDeque,
    fmt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex, MutexGuard, Weak,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use mineintent_contracts::{
    agent::{
        AgentChatMessageV5, AgentError, AgentErrorCode, AgentHotbarV5, AgentPoseV5,
        AgentRunRequest, AgentRunner, AgentStatusV5, CancellationSignal, ContractFuture, Deadline,
        ExecutionControl, JsonAgentDecisionContextV5, JsonObject, PromptTemplateRef, RunId,
    },
    capability::ToolCapabilityRegistry,
    information::SoundValues,
    minecraft::{
        BackendEventEnvelope, BackendEventKind, BackendEventListener, BackendEventPayload,
        BackendLifecyclePayload, MinecraftBackendApi, MinecraftFrameFacts, ProtocolChatEvent,
        Subscription,
    },
};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    sync::{broadcast, watch, Mutex as AsyncMutex, Notify},
    task::{AbortHandle, JoinHandle},
};
use uuid::Uuid;

use crate::{
    agent::{
        AgentChatInputV5, AgentChatTriggerV5, AgentContextV5Assembler, AgentContextV5AssemblyError,
        AgentContextV5EventInput, AgentContextV5Input, AgentModelRequest, ConcreteAgentRunner,
        ModelCompletion, RoundViewportSampler,
    },
    capability::{CapabilityJournal, RegistryToolDispatcher, SpeechSchedulerPort},
    memory::MemoryStore,
    speech::{
        interpret_player_chat, ChatInputContext, PlayerChatMessage, SpeechScheduler,
        SpeechTransport,
    },
    telemetry::{
        DebugDecision, DebugDecisionStatus, DebugFailureSource, DebugFailureSummary,
        DebugStateStore, DebugStateUpdate,
    },
};

const MAX_PENDING_FACTS: usize = 20;
const STOP_WORKER_SETTLE: Duration = Duration::from_millis(250);
// Participant work is smaller than the backend bridge because each admitted
// control item may retain a frame source snapshot and a journal payload while
// the worker is in model/journal I/O. These are explicit, tunable limits:
// ordinary facts are reconstructable, control is lossless, and overflow
// markers have their own bounded lane.
const PARTICIPANT_ORDINARY_CAPACITY: usize = 16;
const PARTICIPANT_CONTROL_CAPACITY: usize = 8;
const PARTICIPANT_OVERFLOW_CAPACITY: usize = 4;
const PARTICIPANT_MAX_OVERFLOW_TYPES: usize = 8;
const PARTICIPANT_MAX_PENDING_OMITTED_TYPES: usize = 8;

/// Scope identity used by every admitted fact and model run.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ParticipantScope {
    pub process_session_id: String,
    pub connection_epoch: u64,
    pub world_id: String,
    pub dimension: Option<String>,
}

impl ParticipantScope {
    pub fn new(
        process_session_id: impl Into<String>,
        connection_epoch: u64,
        world_id: impl Into<String>,
        dimension: Option<String>,
    ) -> Self {
        Self {
            process_session_id: process_session_id.into(),
            connection_epoch,
            world_id: world_id.into(),
            dimension,
        }
    }

    fn from_backend(event: &BackendEventEnvelope) -> Self {
        Self::new(
            event.process_session_id.clone(),
            event.connection_epoch,
            event.world_id.clone(),
            event.dimension.clone(),
        )
    }
}

/// A truthful opening-frame capture supplied by the application/backend
/// adapter.  `light` is an option on this seam specifically so a missing A7
/// value cannot become an invented 0 or 15.
#[derive(Clone, Debug, PartialEq)]
pub struct ParticipantFrameCapture {
    pub at: String,
    pub dimension: String,
    pub pose: AgentPoseV5,
    pub status: Option<AgentStatusV5>,
    pub hotbar: AgentHotbarV5,
    pub unread_chat: Vec<AgentChatInputV5>,
    /// Number of chat records evicted before the bounded source window.
    /// The assembler adds records retained before its newest-eight view.
    pub unread_chat_omitted: u64,
    pub sound: Option<SoundValues>,
    pub light: Option<u8>,
    pub events: Vec<AgentContextV5EventInput>,
    pub omissions: Vec<mineintent_contracts::information::InformationOmission>,
}

/// Explicit source for addressing context and v5 opening-frame facts.
pub trait ParticipantFrameSource: Send + Sync {
    fn chat_context(
        &self,
        scope: &ParticipantScope,
    ) -> Result<ChatInputContext, ParticipantSourceError>;

    fn capture(
        &self,
        scope: &ParticipantScope,
    ) -> Result<ParticipantFrameCapture, ParticipantSourceError>;

    /// Retains an addressed trigger until the corresponding opening capture
    /// has copied it. Production sources use this to make the bounded chat
    /// history lossless for admitted wakes.
    fn retain_trigger(
        &self,
        _scope: &ParticipantScope,
        _trigger: &PlayerChatMessage,
    ) -> Result<(), ParticipantSourceError> {
        Ok(())
    }

    /// Releases the retention established for one admitted trigger. The
    /// default is intentionally inert for test/fallback sources.
    fn release_trigger(&self, _scope: &ParticipantScope, _trigger: &PlayerChatMessage) {}

    /// Drops any source-side retained triggers during runtime teardown. A
    /// production app may still call the concrete source's `dispose` to stop
    /// backend listeners; this hook only prevents queued wakes from keeping
    /// bounded chat records alive.
    fn release_retained_triggers(&self) {}
}

/// Optional synchronous probe used by deterministic runtime-shaped tests to
/// hold the admission point between queue publication and fact recording.
/// Production assembly leaves this unset.
pub trait ParticipantAdmissionObserver: Send + Sync {
    fn after_work_admitted_before_fact(&self, event_type: &str);
}

/// Read-only memory seam used while assembling each opening frame.
pub trait ParticipantMemorySource: Send + Sync {
    fn read_full<'a>(&'a self) -> ContractFuture<'a, Result<String, String>>;
}

impl ParticipantMemorySource for MemoryStore {
    fn read_full<'a>(&'a self) -> ContractFuture<'a, Result<String, String>> {
        Box::pin(async move {
            MemoryStore::read_full(self)
                .await
                .map_err(|error| error.to_string())
        })
    }
}

/// The runtime only asks speech to cancel work which has not yet been sent.
/// A normal later chat therefore preserves already-created speech segments.
pub trait ParticipantSpeechControl: Send + Sync {
    fn cancel_remaining(&self, reason: &str);
}

impl<T> ParticipantSpeechControl for SpeechScheduler<T>
where
    T: SpeechTransport + 'static,
{
    fn cancel_remaining(&self, reason: &str) {
        self.stop_with_reason(reason);
    }
}

/// Combined speech dependency.  Scheduling and cancellation must be supplied
/// by the same object so a stop/scope change cannot accidentally target a
/// different scheduler than the `say` capability uses.
pub trait ParticipantSpeechPort: SpeechSchedulerPort + ParticipantSpeechControl {}

impl<T> ParticipantSpeechPort for T where T: SpeechSchedulerPort + ParticipantSpeechControl + ?Sized {}

/// Injected clock for diagnostics.  Model/frame timestamps remain facts from
/// the frame source; this clock is only used for debug failure records.
pub trait ParticipantClock: Send + Sync {
    fn now(&self) -> String;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemUtcClock;

impl ParticipantClock for SystemUtcClock {
    fn now(&self) -> String {
        utc_now()
    }
}

/// Runner-side proof that the dispatcher and its advertised registry are the
/// same assembly.  The Participant wrapper never accepts a free-standing
/// runner plus an unrelated registry.
pub trait ParticipantRegistryBound: AgentRunner<Context = JsonAgentDecisionContextV5> {
    fn tool_registry(&self) -> Arc<ToolCapabilityRegistry>;
}

impl<Model, Sampler> ParticipantRegistryBound
    for ConcreteAgentRunner<Model, RegistryToolDispatcher, Sampler>
where
    Model: mineintent_contracts::agent::ModelProvider<
        Request = AgentModelRequest,
        Response = ModelCompletion,
    >,
    Sampler: RoundViewportSampler,
{
    fn tool_registry(&self) -> Arc<ToolCapabilityRegistry> {
        self.driver().tools().registry()
    }
}

/// A per-wake runner produced by a single registry-backed factory.
pub trait ParticipantScopedAgentRunner:
    AgentRunner<Context = JsonAgentDecisionContextV5> + ParticipantRegistryBound
{
}

impl<T> ParticipantScopedAgentRunner for T where
    T: AgentRunner<Context = JsonAgentDecisionContextV5> + ParticipantRegistryBound
{
}

/// App assembly seam. Implementations must construct the runner after
/// receiving the current scope and trigger event id, normally by creating a
/// fresh `CapabilityScopeAssembly` and `RegistryToolDispatcher` for that
/// wake. The returned runner is checked against this same registry by
/// [`ParticipantAgentAssembly`].
pub trait ParticipantAgentFactory: Send + Sync {
    fn registry(&self) -> Arc<ToolCapabilityRegistry>;

    fn build(
        &self,
        scope: &ParticipantScope,
        generation: u64,
        trigger_event_id: &str,
    ) -> Result<Arc<dyn ParticipantScopedAgentRunner>, AgentError>;
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum ParticipantAgentAssemblyError {
    #[error("agent factory returned a runner bound to a different registry")]
    RegistryMismatch,
}

/// Single agent assembly used by Participant. The factory is the only source
/// of both the advertised registry and the per-wake in-process runner.
pub trait ParticipantAgentPort: Send + Sync {
    fn definitions(&self) -> Vec<mineintent_contracts::agent::WireToolDefinition>;

    fn run<'a>(
        &'a self,
        scope: ParticipantScope,
        generation: u64,
        trigger_event_id: String,
        request: AgentRunRequest<JsonAgentDecisionContextV5>,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<mineintent_contracts::agent::ModelRunResult, AgentError>>;
}

pub struct ParticipantAgentAssembly {
    factory: Arc<dyn ParticipantAgentFactory>,
    registry: Arc<ToolCapabilityRegistry>,
}

impl ParticipantAgentAssembly {
    pub fn new(factory: Arc<dyn ParticipantAgentFactory>) -> Self {
        let registry = factory.registry();
        Self { factory, registry }
    }

    pub fn registry(&self) -> Arc<ToolCapabilityRegistry> {
        Arc::clone(&self.registry)
    }
}

impl ParticipantAgentPort for ParticipantAgentAssembly {
    fn definitions(&self) -> Vec<mineintent_contracts::agent::WireToolDefinition> {
        self.registry.definitions()
    }

    fn run<'a>(
        &'a self,
        scope: ParticipantScope,
        generation: u64,
        trigger_event_id: String,
        request: AgentRunRequest<JsonAgentDecisionContextV5>,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<mineintent_contracts::agent::ModelRunResult, AgentError>> {
        let runner = match self.factory.build(&scope, generation, &trigger_event_id) {
            Ok(runner) => runner,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        if !Arc::ptr_eq(&runner.tool_registry(), &self.registry) {
            return Box::pin(async {
                Err(AgentError::new(
                    AgentErrorCode::InvalidRequest,
                    ParticipantAgentAssemblyError::RegistryMismatch.to_string(),
                ))
            });
        }
        let definitions = self.registry.definitions();
        if request.tools != definitions {
            return Box::pin(async {
                Err(AgentError::new(
                    AgentErrorCode::InvalidRequest,
                    "participant_tool_definitions_do_not_match_registry",
                ))
            });
        }
        Box::pin(async move { runner.run(request, control).await })
    }
}

/// The only initial wake kind.  More kinds may be registered by a later app,
/// but a default runtime always contains exactly `player_chat`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WakeKind {
    PlayerChat,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WakeRuleCondition {
    AddressedToParticipant,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WakeRule {
    pub kind: WakeKind,
    pub condition: WakeRuleCondition,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WakeRegistryError {
    DuplicateRule,
}

impl fmt::Display for WakeRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateRule => formatter.write_str("duplicate wake rule"),
        }
    }
}

impl std::error::Error for WakeRegistryError {}

#[derive(Clone, Debug)]
pub struct WakeRegistry {
    rules: Vec<WakeRule>,
}

impl Default for WakeRegistry {
    fn default() -> Self {
        Self::initial()
    }
}

impl WakeRegistry {
    pub fn initial() -> Self {
        Self {
            rules: vec![WakeRule {
                kind: WakeKind::PlayerChat,
                condition: WakeRuleCondition::AddressedToParticipant,
            }],
        }
    }

    pub fn register(&mut self, rule: WakeRule) -> Result<(), WakeRegistryError> {
        if self.rules.contains(&rule) {
            return Err(WakeRegistryError::DuplicateRule);
        }
        self.rules.push(rule);
        Ok(())
    }

    pub fn entries(&self) -> Vec<WakeRule> {
        self.rules.clone()
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    fn addresses_player_chat(&self, message: &PlayerChatMessage) -> bool {
        self.rules.iter().any(|rule| {
            rule.kind == WakeKind::PlayerChat
                && rule.condition == WakeRuleCondition::AddressedToParticipant
                && message.addressing.addressed_to_participant
        })
    }
}

/// Backend/internal facts which are retained until the next model wake.  The
/// payload is deliberately a bounded, sanitized summary; full chat text only
/// enters the model through the addressed trigger in the v5 frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParticipantFact {
    pub id: String,
    pub occurred_at: String,
    pub scope: ParticipantScope,
    pub event_type: String,
    pub summary: String,
}

/// One atomic result of a body/opening fact drain. The owner is shared by the
/// runtime opening path and a production `observationAfter` source; it is not
/// a second event interpretation layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParticipantFactBatch {
    pub facts: Vec<ParticipantFact>,
    pub omitted: u64,
    pub omitted_types: Vec<String>,
}

struct ParticipantFactOwnerState {
    generation: u64,
    scope: Option<ParticipantScope>,
    facts: VecDeque<ParticipantFact>,
    omitted: u64,
    omitted_types: Vec<String>,
}

/// Bounded, scope-owned facts shared by opening frames and body
/// `observationAfter`. Producers hold the runtime's admission serial before
/// recording; `drain` takes that same serial before inspecting the owner, so
/// an enqueue-then-record gap cannot be observed as a false empty drain.
pub struct ParticipantFactOwner {
    admission_serial: Arc<Mutex<()>>,
    state: Mutex<ParticipantFactOwnerState>,
}

impl ParticipantFactOwner {
    fn new(admission_serial: Arc<Mutex<()>>) -> Arc<Self> {
        Arc::new(Self {
            admission_serial,
            state: Mutex::new(ParticipantFactOwnerState {
                generation: 0,
                scope: None,
                facts: VecDeque::new(),
                omitted: 0,
                omitted_types: Vec::new(),
            }),
        })
    }

    /// Returns the owner state at the same admission linearization boundary
    /// used by runtime producers. `None` means the run scope is stale.
    pub fn drain(&self, scope: &ParticipantScope, generation: u64) -> Option<ParticipantFactBatch> {
        let _serial = lock(&self.admission_serial);
        self.drain_locked(scope, generation)
    }

    fn drain_locked(
        &self,
        scope: &ParticipantScope,
        generation: u64,
    ) -> Option<ParticipantFactBatch> {
        let mut state = lock(&self.state);
        if state.generation != generation || state.scope.as_ref() != Some(scope) {
            return None;
        }
        Some(ParticipantFactBatch {
            facts: state.facts.drain(..).collect(),
            omitted: std::mem::take(&mut state.omitted),
            omitted_types: std::mem::take(&mut state.omitted_types),
        })
    }

    /// Runtime-only scope binding. The caller already owns the admission
    /// serial and therefore must not call this from an un-serialized producer.
    fn bind_scope(&self, generation: u64, scope: Option<ParticipantScope>) {
        let mut state = lock(&self.state);
        if state.generation != generation || state.scope != scope {
            state.facts.clear();
            state.omitted = 0;
            state.omitted_types.clear();
            state.generation = generation;
            state.scope = scope;
        }
    }

    /// Runtime-only fact record. The caller already owns the admission serial.
    fn record(&self, generation: u64, fact: ParticipantFact) {
        let mut state = lock(&self.state);
        if state.generation != generation || state.scope.as_ref() != Some(&fact.scope) {
            return;
        }
        if state.facts.len() == MAX_PENDING_FACTS {
            let dropped_type = state.facts.pop_front().map(|dropped| dropped.event_type);
            state.omitted = state.omitted.saturating_add(1);
            if let Some(dropped_type) = dropped_type {
                add_pending_omitted_type(&mut state.omitted_types, &dropped_type);
            }
        }
        state.facts.push_back(fact);
    }

    /// Runtime-only omission record. The caller already owns the admission
    /// serial and has already decided that the fact belongs to this scope.
    fn record_omission(&self, generation: u64, event_type: String) {
        let mut state = lock(&self.state);
        if state.generation != generation {
            return;
        }
        state.omitted = state.omitted.saturating_add(1);
        add_pending_omitted_type(&mut state.omitted_types, &event_type);
    }
}

fn startup_scope(facts: &MinecraftFrameFacts) -> Result<ParticipantScope, &'static str> {
    let snapshot = &facts.snapshot;
    if snapshot.process_session_id.is_empty()
        || snapshot.connection_attempt_id.is_empty()
        || snapshot.world.world_id.is_empty()
        || snapshot.world.dimension.is_empty()
        || snapshot.captured_at.is_empty()
    {
        return Err("backend startup snapshot is missing scope identity");
    }
    Ok(ParticipantScope::new(
        snapshot.process_session_id.clone(),
        snapshot.connection_epoch,
        snapshot.world.world_id.clone(),
        Some(snapshot.world.dimension.clone()),
    ))
}

#[derive(Clone, Debug)]
pub enum ParticipantInternalEvent {
    Fact {
        id: String,
        occurred_at: String,
        scope: ParticipantScope,
        event_type: String,
        summary: String,
    },
    ScopeChanged {
        id: String,
        occurred_at: String,
        scope: ParticipantScope,
        reason: String,
    },
    Closed {
        id: String,
        occurred_at: String,
        scope: ParticipantScope,
        reason: String,
    },
    Faulted {
        id: String,
        occurred_at: String,
        scope: ParticipantScope,
        code: String,
    },
    Stopped {
        id: String,
        occurred_at: String,
        scope: ParticipantScope,
        reason: String,
    },
}

/// Unified public event envelope. Both variants use the same runtime
/// admission/ordering path; backend listeners need not know about internal
/// fact producers.
#[derive(Clone, Debug)]
pub enum ParticipantEvent {
    Backend(BackendEventEnvelope),
    Internal(ParticipantInternalEvent),
}

impl ParticipantInternalEvent {
    fn metadata(&self) -> (&str, &str, &ParticipantScope) {
        match self {
            Self::Fact {
                id,
                occurred_at,
                scope,
                ..
            }
            | Self::ScopeChanged {
                id,
                occurred_at,
                scope,
                ..
            }
            | Self::Closed {
                id,
                occurred_at,
                scope,
                ..
            }
            | Self::Faulted {
                id,
                occurred_at,
                scope,
                ..
            }
            | Self::Stopped {
                id,
                occurred_at,
                scope,
                ..
            } => (id, occurred_at, scope),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParticipantLifecycle {
    Created,
    Running,
    Stopping,
    Stopped,
    Faulted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParticipantAdmission {
    Recorded,
    WakeQueued { ordinal: u64 },
    Ignored,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParticipantFailureSource {
    Backend,
    Source,
    Journal,
    Model,
    Runtime,
    BodyRelease,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParticipantFailure {
    pub source: ParticipantFailureSource,
    pub code: String,
    pub summary: String,
    pub scope: Option<ParticipantScope>,
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum ParticipantSourceError {
    #[error("opening frame light is unavailable")]
    MissingLight,
    #[error("participant source scope is stale: {0}")]
    StaleScope(String),
    #[error("opening frame source is invalid: {0}")]
    Invalid(String),
    #[error("opening frame source failed: {0}")]
    Failed(String),
}

#[derive(Debug, Error)]
pub enum ParticipantRuntimeError {
    #[error("participant runtime configuration is invalid: {0}")]
    InvalidConfig(String),
    #[error("participant runtime is not started")]
    NotStarted,
    #[error("participant runtime is already started")]
    AlreadyStarted,
    #[error("participant runtime is stopped")]
    Stopped,
    #[error("participant runtime is faulted")]
    Faulted,
    #[error("participant runtime event queue is closed")]
    QueueClosed,
    #[error("backend operation failed: {0}")]
    Backend(String),
    #[error("participant source failed: {0}")]
    Source(#[from] ParticipantSourceError),
    #[error("frame assembly failed: {0}")]
    Frame(#[from] AgentContextV5AssemblyError),
    #[error("memory read failed: {0}")]
    Memory(String),
    #[error("participant handler failed: {0}")]
    Handler(String),
}

pub struct ParticipantRuntimeConfig<R> {
    pub backend: Arc<dyn MinecraftBackendApi>,
    pub agent: Arc<R>,
    pub frame_source: Arc<dyn ParticipantFrameSource>,
    pub memory: Arc<dyn ParticipantMemorySource>,
    pub journal: Arc<dyn CapabilityJournal>,
    pub speech: Arc<dyn ParticipantSpeechPort>,
    pub debug: Arc<DebugStateStore>,
    pub clock: Arc<dyn ParticipantClock>,
    pub prompt_template: PromptTemplateRef,
    pub run_deadline: Duration,
    pub wake_registry: WakeRegistry,
    /// Stable process/session identity supplied by the app. It is included in
    /// every run id so reconnects and reconstructed runtimes cannot silently
    /// reuse the ordinal-only ids of an earlier instance.
    pub run_id_namespace: String,
}

struct RuntimeState {
    lifecycle: ParticipantLifecycle,
    scope: Option<ParticipantScope>,
    generation: u64,
    next_ordinal: u64,
    active: Option<ActiveRun>,
    terminal_pending: bool,
    // There is no frozen backend seam which can prove that an evicted
    // process-session identity can never return. Keep non-sensitive digests
    // for this runtime lifetime instead of arbitrarily re-admitting an old
    // session after a fixed-size queue rolls over.
    retired_process_sessions: std::collections::HashSet<String>,
    // A backend ConnectionClosed invalidates this exact scope while the
    // Participant remains Running for a possible reconnect. Keeping the
    // tombstone prevents an old same-epoch event from reactivating a scope
    // merely because `scope` is temporarily None.
    closed_scope: Option<ParticipantScope>,
    closed_connection_attempt_id: Option<String>,
    active_connection_attempt_id: Option<String>,
}

struct ActiveRun {
    cancellation: Arc<ParticipantCancellation>,
    abort: Option<AbortHandle>,
    start_gate: Arc<ParticipantStartGate>,
}

#[derive(Clone)]
struct WakeItem {
    ordinal: u64,
    scope: ParticipantScope,
    occurred_at: String,
    trigger: PlayerChatMessage,
    trigger_retained: bool,
}

struct WorkItem {
    ticket: u64,
    ordinal: u64,
    generation: u64,
    scope: ParticipantScope,
    occurred_at: String,
    event_id: String,
    event_type: String,
    wake: Option<WakeItem>,
    scope_control: bool,
    terminal: bool,
    terminal_lifecycle: Option<ParticipantLifecycle>,
    overflow: Option<OverflowInfo>,
}

#[derive(Clone)]
struct OverflowInfo {
    dropped_count: u64,
    dropped_types: Vec<String>,
}

struct OverflowEntry {
    ticket: u64,
    item: WorkItem,
    dropped_count: u64,
    dropped_types: Vec<String>,
}

enum QueueAdmission {
    Accepted,
    Ignored,
    OrdinaryDropped { event_type: String },
}

/// Admission normally holds the runtime serial for the complete synchronous
/// admission transaction.  A bounded queue wait is the one exception: the
/// producer must not pin this guard while waiting for the worker to make room,
/// because body observationAfter needs the same serial to drain facts.
struct AdmissionSerialGuard<'a> {
    serial: &'a Mutex<()>,
    guard: Option<MutexGuard<'a, ()>>,
}

impl<'a> AdmissionSerialGuard<'a> {
    fn new(serial: &'a Mutex<()>) -> Self {
        Self {
            serial,
            guard: Some(lock(serial)),
        }
    }

    fn release(&mut self) {
        drop(self.guard.take());
    }

    fn reacquire(&mut self) {
        debug_assert!(self.guard.is_none());
        self.guard = Some(lock(self.serial));
    }
}

struct ParticipantEventQueue {
    state: Mutex<ParticipantEventQueueState>,
    wake: Condvar,
    notify: Notify,
    waiter_notify: Notify,
}

struct ParticipantEventQueueState {
    ordinary: VecDeque<WorkItem>,
    control: VecDeque<WorkItem>,
    overflow: VecDeque<OverflowEntry>,
    terminal: Option<WorkItem>,
    next_ticket: u64,
    next_admission: u64,
    open_loss_segment: Option<u64>,
    waiting_producers: usize,
    closed: bool,
}

impl ParticipantEventQueue {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ParticipantEventQueueState {
                ordinary: VecDeque::new(),
                control: VecDeque::new(),
                overflow: VecDeque::new(),
                terminal: None,
                next_ticket: 1,
                next_admission: 1,
                open_loss_segment: None,
                waiting_producers: 0,
                closed: false,
            }),
            wake: Condvar::new(),
            notify: Notify::new(),
            waiter_notify: Notify::new(),
        })
    }

    fn enqueue(
        &self,
        mut item: WorkItem,
        serial: &mut AdmissionSerialGuard<'_>,
        mut is_current: impl FnMut(&WorkItem) -> bool,
    ) -> Result<QueueAdmission, ParticipantRuntimeError> {
        let mut state = lock(&self.state);
        let ticket = state.next_ticket;
        state.next_ticket = state.next_ticket.saturating_add(1);
        item.ticket = ticket;

        loop {
            while !state.closed && state.next_admission != ticket {
                state.waiting_producers = state.waiting_producers.saturating_add(1);
                self.waiter_notify.notify_waiters();
                serial.release();
                state = self
                    .wake
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.waiting_producers = state.waiting_producers.saturating_sub(1);
                self.waiter_notify.notify_waiters();
                drop(state);
                serial.reacquire();
                state = lock(&self.state);
            }
            if state.closed {
                if state.next_admission == ticket {
                    state.next_admission = state.next_admission.saturating_add(1);
                    self.wake.notify_all();
                }
                return Err(ParticipantRuntimeError::QueueClosed);
            }

            // The queue lock is intentionally not held while checking runtime
            // scope/generation.  At this point the admission serial has been
            // reacquired after any wait, so a stale producer can cancel its
            // reserved ticket before publishing an old item.
            drop(state);
            let current = is_current(&item);
            state = lock(&self.state);
            if state.closed {
                if state.next_admission == ticket {
                    state.next_admission = state.next_admission.saturating_add(1);
                    self.wake.notify_all();
                }
                return Err(ParticipantRuntimeError::QueueClosed);
            }
            if state.next_admission != ticket {
                continue;
            }
            if !current {
                state.next_admission = state.next_admission.saturating_add(1);
                self.wake.notify_all();
                return Ok(QueueAdmission::Ignored);
            }

            if item.terminal {
                if state.terminal.is_none() {
                    state.terminal = Some(item);
                }
                state.open_loss_segment = None;
                self.commit_admission(&mut state);
                return Ok(QueueAdmission::Accepted);
            }

            if item.wake.is_some() || item.scope_control {
                if state.control.len() < PARTICIPANT_CONTROL_CAPACITY {
                    state.control.push_back(item);
                    state.open_loss_segment = None;
                    self.commit_admission(&mut state);
                    return Ok(QueueAdmission::Accepted);
                }
            } else if state.ordinary.len() < PARTICIPANT_ORDINARY_CAPACITY {
                state.ordinary.push_back(item);
                state.open_loss_segment = None;
                self.commit_admission(&mut state);
                return Ok(QueueAdmission::Accepted);
            } else {
                let event_type = item.event_type.clone();
                if state.open_loss_segment.is_some_and(|segment| {
                    state
                        .overflow
                        .back()
                        .is_some_and(|overflow| overflow.ticket == segment)
                }) {
                    if let Some(overflow) = state.overflow.back_mut() {
                        overflow.dropped_count = overflow.dropped_count.saturating_add(1);
                        add_overflow_type(&mut overflow.dropped_types, &event_type);
                    }
                    self.commit_admission(&mut state);
                    return Ok(QueueAdmission::OrdinaryDropped { event_type });
                }
                if state.overflow.len() < PARTICIPANT_OVERFLOW_CAPACITY {
                    let mut marker = item;
                    marker.event_id = format!("participant-overflow-{ticket}");
                    marker.event_type = "participant_events_omitted".to_owned();
                    marker.wake = None;
                    marker.scope_control = true;
                    marker.terminal = false;
                    marker.terminal_lifecycle = None;
                    marker.overflow = Some(OverflowInfo {
                        dropped_count: 1,
                        dropped_types: vec![event_type.clone()],
                    });
                    state.overflow.push_back(OverflowEntry {
                        ticket,
                        item: marker,
                        dropped_count: 1,
                        dropped_types: vec![event_type.clone()],
                    });
                    state.open_loss_segment = Some(ticket);
                    self.commit_admission(&mut state);
                    return Ok(QueueAdmission::OrdinaryDropped { event_type });
                }
            }

            state.waiting_producers = state.waiting_producers.saturating_add(1);
            self.waiter_notify.notify_waiters();
            serial.release();
            state = self
                .wake
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.waiting_producers = state.waiting_producers.saturating_sub(1);
            self.waiter_notify.notify_waiters();
            drop(state);
            serial.reacquire();
            state = lock(&self.state);
        }
    }

    fn commit_admission(&self, state: &mut ParticipantEventQueueState) {
        state.next_admission = state.next_admission.saturating_add(1);
        self.wake.notify_all();
        self.notify.notify_one();
    }

    async fn next(&self) -> Option<WorkItem> {
        loop {
            let notified = self.notify.notified();
            let result = {
                let mut state = lock(&self.state);
                if let Some(item) = state.pop_next() {
                    self.wake.notify_all();
                    Some(Some(item))
                } else if state.closed {
                    Some(None)
                } else {
                    None
                }
            };
            if let Some(item) = result {
                return item;
            }
            notified.await;
        }
    }

    fn close_admission(&self) {
        let mut state = lock(&self.state);
        state.closed = true;
        self.wake.notify_all();
        self.notify.notify_waiters();
        self.waiter_notify.notify_waiters();
    }

    async fn wait_for_waiters(&self, expected: usize) {
        loop {
            let notified = self.waiter_notify.notified();
            if lock(&self.state).waiting_producers >= expected {
                return;
            }
            notified.await;
        }
    }

    fn counts(&self) -> (usize, usize, usize, usize, usize) {
        let state = lock(&self.state);
        (
            state.ordinary.len(),
            state.control.len(),
            state.overflow.len(),
            usize::from(state.terminal.is_some()),
            state.waiting_producers,
        )
    }
}

impl ParticipantEventQueueState {
    fn pop_next(&mut self) -> Option<WorkItem> {
        let mut candidate: Option<(u64, QueueLane)> = None;
        if let Some(item) = self.ordinary.front() {
            candidate = Some((item.ticket, QueueLane::Ordinary));
        }
        if let Some(item) = self.control.front() {
            if candidate.is_none_or(|(ticket, _)| item.ticket < ticket) {
                candidate = Some((item.ticket, QueueLane::Control));
            }
        }
        if let Some(item) = self.overflow.front() {
            if candidate.is_none_or(|(ticket, _)| item.ticket < ticket) {
                candidate = Some((item.ticket, QueueLane::Overflow));
            }
        }
        if let Some(item) = self.terminal.as_ref() {
            if candidate.is_none_or(|(ticket, _)| item.ticket < ticket) {
                candidate = Some((item.ticket, QueueLane::Terminal));
            }
        }
        let (_, lane) = candidate?;
        match lane {
            QueueLane::Ordinary => self.ordinary.pop_front(),
            QueueLane::Control => self.control.pop_front(),
            QueueLane::Overflow => self.overflow.pop_front().map(|mut entry| {
                if self.open_loss_segment == Some(entry.ticket) {
                    self.open_loss_segment = None;
                }
                entry.item.overflow = Some(OverflowInfo {
                    dropped_count: entry.dropped_count,
                    dropped_types: entry.dropped_types,
                });
                entry.item
            }),
            QueueLane::Terminal => self.terminal.take(),
        }
    }
}

#[derive(Clone, Copy)]
enum QueueLane {
    Ordinary,
    Control,
    Overflow,
    Terminal,
}

enum BackendAdmission {
    Accepted {
        generation: u64,
        ordinal: u64,
        cleanup: Cleanup,
        scope_control: bool,
        record_fact: bool,
        terminal: bool,
    },
    StaleIgnored,
}

enum ExplicitAdmission {
    Accepted {
        generation: u64,
        ordinal: u64,
        cleanup: Cleanup,
        terminal_lifecycle: Option<ParticipantLifecycle>,
    },
    StaleIgnored,
}

struct Cleanup {
    required: bool,
    cancellation: Option<Arc<ParticipantCancellation>>,
    abort: Option<AbortHandle>,
    start_gate: Option<Arc<ParticipantStartGate>>,
}

impl Cleanup {
    fn empty() -> Self {
        Self {
            required: false,
            cancellation: None,
            abort: None,
            start_gate: None,
        }
    }

    fn required() -> Self {
        Self {
            required: true,
            ..Self::empty()
        }
    }
}

/// A single ordered Participant runtime.  Construct it in an application,
/// call `start_worker` after the application-owned backend is ready.
pub struct ParticipantRuntime<R>
where
    R: ParticipantAgentPort + 'static,
{
    backend: Arc<dyn MinecraftBackendApi>,
    agent: Arc<R>,
    frame_source: Arc<dyn ParticipantFrameSource>,
    memory: Arc<dyn ParticipantMemorySource>,
    journal: Arc<dyn CapabilityJournal>,
    speech: Arc<dyn ParticipantSpeechPort>,
    debug: Arc<DebugStateStore>,
    clock: Arc<dyn ParticipantClock>,
    prompt_template: PromptTemplateRef,
    run_deadline: Duration,
    wake_registry: WakeRegistry,
    assembler: AgentContextV5Assembler,
    admission_serial: Arc<Mutex<()>>,
    fact_owner: Arc<ParticipantFactOwner>,
    admission_observer: Mutex<Option<Arc<dyn ParticipantAdmissionObserver>>>,
    event_queue: Arc<ParticipantEventQueue>,
    admission_cancelled: AtomicBool,
    state: Mutex<RuntimeState>,
    worker: Mutex<Option<JoinHandle<()>>>,
    subscription: Mutex<Option<Box<dyn Subscription>>>,
    cleanup_serial: Mutex<()>,
    stop_serial: AsyncMutex<()>,
    stop_cleanup: watch::Sender<bool>,
    lifecycle_signal: watch::Sender<ParticipantLifecycle>,
    generation: watch::Sender<u64>,
    failures: broadcast::Sender<ParticipantFailure>,
    run_id_namespace_digest: String,
    run_id_instance_id: String,
}

impl<R> ParticipantRuntime<R>
where
    R: ParticipantAgentPort + 'static,
{
    pub fn new(config: ParticipantRuntimeConfig<R>) -> Arc<Self> {
        Self::try_new(config).expect("participant runtime configuration must be valid")
    }

    pub fn try_new(
        config: ParticipantRuntimeConfig<R>,
    ) -> Result<Arc<Self>, ParticipantRuntimeError> {
        let namespace_length = config.run_id_namespace.chars().count();
        if !(1..=128).contains(&namespace_length)
            || config.run_id_namespace.chars().any(char::is_control)
        {
            return Err(ParticipantRuntimeError::InvalidConfig(
                "run_id_namespace must contain 1..=128 non-control characters".to_owned(),
            ));
        }
        let run_id_instance_id = Uuid::new_v4().simple().to_string();
        let run_id_namespace_digest = namespace_digest(&config.run_id_namespace);
        let max_run_id = format!(
            "p-{run_id_namespace_digest}-{run_id_instance_id}-{}-{}-{}",
            base36_u64(u64::MAX),
            base36_u64(u64::MAX),
            base36_u64(u64::MAX),
        );
        if max_run_id.chars().count() > 128 {
            return Err(ParticipantRuntimeError::InvalidConfig(
                "run id assembly exceeds the contract length limit".to_owned(),
            ));
        }
        let (stop_cleanup, _) = watch::channel(true);
        let (lifecycle_signal, _) = watch::channel(ParticipantLifecycle::Created);
        let (generation, _) = watch::channel(0_u64);
        let (failures, _) = broadcast::channel(32);
        let admission_serial = Arc::new(Mutex::new(()));
        let fact_owner = ParticipantFactOwner::new(Arc::clone(&admission_serial));
        Ok(Arc::new(Self {
            backend: config.backend,
            agent: config.agent,
            frame_source: config.frame_source,
            memory: config.memory,
            journal: config.journal,
            speech: config.speech,
            debug: config.debug,
            clock: config.clock,
            prompt_template: config.prompt_template,
            run_deadline: config.run_deadline,
            wake_registry: config.wake_registry,
            assembler: AgentContextV5Assembler,
            admission_serial,
            fact_owner,
            admission_observer: Mutex::new(None),
            event_queue: ParticipantEventQueue::new(),
            admission_cancelled: AtomicBool::new(false),
            state: Mutex::new(RuntimeState {
                lifecycle: ParticipantLifecycle::Created,
                scope: None,
                generation: 0,
                next_ordinal: 0,
                active: None,
                terminal_pending: false,
                retired_process_sessions: std::collections::HashSet::new(),
                closed_scope: None,
                closed_connection_attempt_id: None,
                active_connection_attempt_id: None,
            }),
            worker: Mutex::new(None),
            subscription: Mutex::new(None),
            cleanup_serial: Mutex::new(()),
            stop_serial: AsyncMutex::new(()),
            stop_cleanup,
            lifecycle_signal,
            generation,
            failures,
            run_id_namespace_digest,
            run_id_instance_id,
        }))
    }

    pub fn lifecycle(&self) -> ParticipantLifecycle {
        lock(&self.state).lifecycle
    }

    pub fn current_scope(&self) -> Option<ParticipantScope> {
        lock(&self.state).scope.clone()
    }

    /// Current runtime generation for per-wake production source assembly.
    pub fn current_generation(&self) -> u64 {
        lock(&self.state).generation
    }

    /// A weak fact-owner port for a per-wake body observation source. The
    /// weak edge keeps an observation dispatcher from retaining a stopped
    /// runtime.
    pub fn fact_owner(&self) -> Weak<ParticipantFactOwner> {
        Arc::downgrade(&self.fact_owner)
    }

    pub fn wake_registry(&self) -> &WakeRegistry {
        &self.wake_registry
    }

    pub fn tool_definitions(&self) -> Vec<mineintent_contracts::agent::WireToolDefinition> {
        self.agent.definitions()
    }

    pub fn subscribe_failures(&self) -> broadcast::Receiver<ParticipantFailure> {
        self.failures.subscribe()
    }

    pub fn debug_snapshot(&self) -> crate::telemetry::DebugSnapshot {
        self.debug.snapshot()
    }

    /// Deterministic saturation probe for the Participant integration tests.
    /// It intentionally exposes only bounded queue counts, not queue storage
    /// or admission mutation, and is not an application composition seam.
    #[doc(hidden)]
    pub fn queue_counts_for_test(&self) -> (usize, usize, usize, usize, usize) {
        self.event_queue.counts()
    }

    /// Installs the optional admission probe before a deterministic test
    /// drives a producer. It does not participate in the production wiring or
    /// the model-visible contract.
    #[doc(hidden)]
    pub fn install_admission_observer_for_test(
        &self,
        observer: Arc<dyn ParticipantAdmissionObserver>,
    ) {
        *lock(&self.admission_observer) = Some(observer);
    }

    /// Deterministic saturation probe for tests that need to establish that a
    /// producer is blocked on ticket/capacity before exercising cancellation.
    #[doc(hidden)]
    pub async fn wait_for_queue_waiters_for_test(&self, expected: usize) {
        self.event_queue.wait_for_waiters(expected).await;
    }

    /// Deterministic test probe that waits until the published generation
    /// reaches at least `expected`. Scope/generation invalidation is always
    /// published while the admission serial is still held, so observing the
    /// new generation deterministically proves that a pending older admission
    /// cannot have resolved yet (it still needs the serial to re-check).
    #[doc(hidden)]
    pub async fn wait_for_generation_for_test(&self, expected: u64) {
        let mut receiver = self.generation.subscribe();
        loop {
            if *receiver.borrow() >= expected {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }

    /// Starts the ordered worker and attaches the runtime to an already-owned
    /// in-process backend event stream. The application must drive the frozen
    /// backend `start(OperationControl)` before this call and
    /// `stop(reason, OperationControl)` after [`Self::stop`] returns. No model
    /// call is made here.
    pub fn start_worker(self: &Arc<Self>) -> Result<(), ParticipantRuntimeError> {
        let startup_result: Result<(), ParticipantRuntimeError> = {
            let _serial = lock(&self.admission_serial);
            let mut state = lock(&self.state);
            match state.lifecycle {
                ParticipantLifecycle::Created => {
                    state.lifecycle = ParticipantLifecycle::Running;
                    self.publish_lifecycle(ParticipantLifecycle::Running);
                }
                ParticipantLifecycle::Stopped => return Err(ParticipantRuntimeError::Stopped),
                ParticipantLifecycle::Faulted => return Err(ParticipantRuntimeError::Faulted),
                ParticipantLifecycle::Running | ParticipantLifecycle::Stopping => {
                    return Err(ParticipantRuntimeError::AlreadyStarted)
                }
            }
            match self.backend.capture_frame_facts() {
                Err(error) => Err(ParticipantRuntimeError::Backend(error.to_string())),
                Ok(facts) => match startup_scope(&facts) {
                    Err(message) => Err(ParticipantRuntimeError::Backend(message.to_owned())),
                    Ok(scope) => {
                        state.scope = Some(scope.clone());
                        state.active_connection_attempt_id =
                            Some(facts.snapshot.connection_attempt_id.clone());
                        state.closed_scope = None;
                        state.closed_connection_attempt_id = None;
                        self.fact_owner
                            .bind_scope(state.generation, state.scope.clone());
                        self.record_fact(
                            state.generation,
                            ParticipantFact {
                                id: format!("participant-started-{}", self.run_id_instance_id),
                                occurred_at: facts.snapshot.captured_at,
                                scope,
                                event_type: "participant.started".to_owned(),
                                summary: "AI 参与者已进入世界".to_owned(),
                            },
                        );
                        let runtime = Arc::clone(self);
                        let worker = tokio::spawn(async move { runtime.worker_loop().await });
                        *lock(&self.worker) = Some(worker);
                        Ok(())
                    }
                },
            }
        };
        if let Err(error) = startup_result {
            self.fail_runtime_sync(
                ParticipantFailureSource::Backend,
                "backend_startup_snapshot_failed",
                "backend startup snapshot failed",
                None,
            );
            self.rollback_startup_failure(&error);
            return Err(error);
        }
        self.debug.update(DebugStateUpdate {
            connection: Some(self.backend.state()),
            ..DebugStateUpdate::default()
        });

        let listener: Arc<dyn BackendEventListener> = self.clone();
        match self.backend.subscribe(listener) {
            Ok(subscription) => {
                let mut subscription = Some(subscription);
                let lifecycle = {
                    let _serial = lock(&self.admission_serial);
                    let lifecycle = lock(&self.state).lifecycle;
                    if lifecycle == ParticipantLifecycle::Running {
                        *lock(&self.subscription) = subscription.take();
                    }
                    lifecycle
                };
                if lifecycle == ParticipantLifecycle::Running {
                    Ok(())
                } else {
                    if let Some(mut subscription) = subscription {
                        subscription.unsubscribe();
                    }
                    Err(startup_lifecycle_error(lifecycle))
                }
            }
            Err(error) => {
                self.fail_runtime_sync(
                    ParticipantFailureSource::Backend,
                    "backend_subscribe_failed",
                    "backend subscription failed",
                    None,
                );
                self.rollback_startup_failure(&ParticipantRuntimeError::Backend(error.to_string()));
                Err(ParticipantRuntimeError::Backend(error.to_string()))
            }
        }
    }

    /// Synchronously admits a backend event and returns after any required
    /// scope invalidation, cancellation, body release, and speech cancellation
    /// have completed.  The async worker is only reached after this point.
    pub fn ingest_backend_event(
        &self,
        event: BackendEventEnvelope,
    ) -> Result<ParticipantAdmission, ParticipantRuntimeError> {
        let mut serial = AdmissionSerialGuard::new(&self.admission_serial);
        self.ensure_running()?;
        let scope = ParticipantScope::from_backend(&event);
        self.debug.update(DebugStateUpdate {
            connection: Some(self.backend.state()),
            ..DebugStateUpdate::default()
        });

        let BackendAdmission::Accepted {
            generation,
            ordinal,
            cleanup,
            scope_control,
            record_fact,
            terminal,
        } = self.admit_scope_for_backend(&event, &scope)
        else {
            return Ok(ParticipantAdmission::Ignored);
        };
        self.perform_cleanup(
            cleanup,
            "participant_scope_changed",
            AgentError::new(AgentErrorCode::ScopeInvalid, "participant_scope_changed"),
        );
        if terminal {
            self.stop_cleanup.send_replace(true);
        }

        let (event_type, wake_candidate) = self.evaluate_backend_wake(&event, &scope)?;
        let terminal_lifecycle = terminal.then(|| backend_terminal_lifecycle(&event));

        let pending_fact =
            (!terminal && record_fact && wake_candidate.is_none()).then(|| ParticipantFact {
                id: event.id.clone(),
                occurred_at: event.occurred_at.clone(),
                scope: scope.clone(),
                event_type: backend_fact_type(&event).to_owned(),
                summary: backend_event_summary(&event),
            });

        let retained_trigger = wake_candidate.clone();
        let mut trigger_retained = false;
        if let Some(trigger) = retained_trigger.as_ref() {
            self.frame_source.retain_trigger(&scope, trigger)?;
            trigger_retained = true;
        }
        let wake = wake_candidate.map(|trigger| WakeItem {
            ordinal,
            scope: scope.clone(),
            occurred_at: event.occurred_at.clone(),
            trigger,
            trigger_retained,
        });
        let has_wake = wake.is_some();
        let backend_control = backend_event_is_control(&event);
        let queue_admission = match self.enqueue_work(
            WorkItem {
                ticket: 0,
                ordinal,
                generation,
                scope: scope.clone(),
                occurred_at: event.occurred_at.clone(),
                event_id: event.id,
                event_type,
                wake,
                scope_control: scope_control || backend_control,
                terminal,
                terminal_lifecycle,
                overflow: None,
            },
            &mut serial,
        ) {
            Ok(admission) => admission,
            Err(error) => {
                if trigger_retained {
                    if let Some(trigger) = retained_trigger.as_ref() {
                        self.frame_source.release_trigger(&scope, trigger);
                    }
                }
                return Err(error);
            }
        };
        if matches!(&queue_admission, QueueAdmission::Ignored) {
            if trigger_retained {
                if let Some(trigger) = retained_trigger.as_ref() {
                    self.frame_source.release_trigger(&scope, trigger);
                }
            }
            return Ok(ParticipantAdmission::Ignored);
        }
        if let Some(fact) = pending_fact {
            if matches!(&queue_admission, QueueAdmission::Accepted) {
                self.notify_admission_observer(&fact.event_type);
            }
            match queue_admission {
                QueueAdmission::Accepted => self.record_fact(generation, fact),
                QueueAdmission::OrdinaryDropped { event_type } => {
                    self.record_pending_omission(generation, event_type)
                }
                QueueAdmission::Ignored => unreachable!("ignored admission returned above"),
            }
        }
        Ok(match has_wake {
            true => ParticipantAdmission::WakeQueued { ordinal },
            false => ParticipantAdmission::Recorded,
        })
    }

    pub fn emit_internal(
        &self,
        event: ParticipantInternalEvent,
    ) -> Result<ParticipantAdmission, ParticipantRuntimeError> {
        let mut serial = AdmissionSerialGuard::new(&self.admission_serial);
        self.ensure_running()?;
        let (id, occurred_at, scope) = {
            let (id, occurred_at, scope) = event.metadata();
            (id.to_owned(), occurred_at.to_owned(), scope.clone())
        };
        let admission = match &event {
            ParticipantInternalEvent::ScopeChanged { .. } => {
                self.admit_explicit_scope(&scope, true, None)
            }
            ParticipantInternalEvent::Closed { .. }
            | ParticipantInternalEvent::Faulted { .. }
            | ParticipantInternalEvent::Stopped { .. } => {
                let terminal_lifecycle = match &event {
                    ParticipantInternalEvent::Faulted { .. } => ParticipantLifecycle::Faulted,
                    ParticipantInternalEvent::Closed { .. }
                    | ParticipantInternalEvent::Stopped { .. } => ParticipantLifecycle::Stopped,
                    _ => unreachable!("terminal arm only contains terminal events"),
                };
                self.admit_explicit_scope(&scope, false, Some(terminal_lifecycle))
            }
            ParticipantInternalEvent::Fact { .. } => self.admit_explicit_scope(&scope, false, None),
        };
        let ExplicitAdmission::Accepted {
            generation,
            ordinal,
            cleanup,
            terminal_lifecycle,
        } = admission
        else {
            return Ok(ParticipantAdmission::Ignored);
        };
        let terminal = terminal_lifecycle.is_some();
        self.perform_cleanup(
            cleanup,
            "participant_internal_scope_changed",
            AgentError::new(AgentErrorCode::ScopeInvalid, "participant_scope_changed"),
        );
        if terminal {
            self.stop_cleanup.send_replace(true);
        }

        let internal_scope_control =
            matches!(&event, ParticipantInternalEvent::ScopeChanged { .. });
        let (event_type, summary) = match event {
            ParticipantInternalEvent::Fact {
                event_type,
                summary,
                ..
            } => (event_type, summary),
            ParticipantInternalEvent::ScopeChanged { reason, .. } => {
                ("scope_changed".to_owned(), bounded_summary(reason))
            }
            ParticipantInternalEvent::Closed { reason, .. } => {
                ("connection_closed".to_owned(), bounded_summary(reason))
            }
            ParticipantInternalEvent::Faulted { code, .. } => {
                ("backend_faulted".to_owned(), bounded_summary(code))
            }
            ParticipantInternalEvent::Stopped { reason, .. } => {
                ("backend_stopped".to_owned(), bounded_summary(reason))
            }
        };
        let pending_fact = (!terminal).then(|| ParticipantFact {
            id: id.clone(),
            occurred_at: occurred_at.clone(),
            scope: scope.clone(),
            event_type: event_type.clone(),
            summary: bounded_summary(summary),
        });
        let queue_admission = self.enqueue_work(
            WorkItem {
                ticket: 0,
                ordinal,
                generation,
                scope,
                occurred_at,
                event_id: id,
                event_type,
                wake: None,
                scope_control: internal_scope_control,
                terminal,
                terminal_lifecycle,
                overflow: None,
            },
            &mut serial,
        )?;
        if matches!(&queue_admission, QueueAdmission::Ignored) {
            return Ok(ParticipantAdmission::Ignored);
        }
        if let Some(fact) = pending_fact {
            if matches!(&queue_admission, QueueAdmission::Accepted) {
                self.notify_admission_observer(&fact.event_type);
            }
            match queue_admission {
                QueueAdmission::Accepted => self.record_fact(generation, fact),
                QueueAdmission::OrdinaryDropped { event_type } => {
                    self.record_pending_omission(generation, event_type)
                }
                QueueAdmission::Ignored => unreachable!("ignored admission returned above"),
            }
        }
        Ok(ParticipantAdmission::Recorded)
    }

    pub fn ingest_event(
        &self,
        event: ParticipantEvent,
    ) -> Result<ParticipantAdmission, ParticipantRuntimeError> {
        match event {
            ParticipantEvent::Backend(event) => self.ingest_backend_event(event),
            ParticipantEvent::Internal(event) => self.emit_internal(event),
        }
    }

    /// Performs the synchronous half of shutdown. It cancels the active run,
    /// invalidates queued work, cancels remaining speech, releases the body,
    /// and unsubscribes before any worker await is attempted.
    pub fn request_stop(&self) -> Result<bool, ParticipantRuntimeError> {
        self.admission_cancelled.store(true, Ordering::Release);
        self.event_queue.close_admission();
        let (should_wait, cleanup) = {
            let _serial = lock(&self.admission_serial);
            let should_wait = {
                let mut state = lock(&self.state);
                match state.lifecycle {
                    ParticipantLifecycle::Created => {
                        state.lifecycle = ParticipantLifecycle::Stopped;
                        self.publish_lifecycle(ParticipantLifecycle::Stopped);
                        self.stop_cleanup.send_replace(true);
                        false
                    }
                    ParticipantLifecycle::Stopped => false,
                    ParticipantLifecycle::Stopping => false,
                    ParticipantLifecycle::Running | ParticipantLifecycle::Faulted => {
                        state.lifecycle = ParticipantLifecycle::Stopping;
                        self.publish_lifecycle(ParticipantLifecycle::Stopping);
                        self.stop_cleanup.send_replace(false);
                        true
                    }
                }
            };
            if should_wait {
                (true, self.invalidate_generation())
            } else {
                (false, Cleanup::empty())
            }
        };
        if !should_wait {
            self.frame_source.release_retained_triggers();
            return Ok(false);
        }

        self.perform_cleanup(cleanup, "participant_stopped", AgentError::run_cancelled());
        self.frame_source.release_retained_triggers();
        if let Some(mut subscription) = lock(&self.subscription).take() {
            subscription.unsubscribe();
        }
        self.stop_cleanup.send_replace(true);
        Ok(true)
    }

    /// Completes shutdown after [`Self::request_stop`] has performed its
    /// synchronous invalidation. A worker receives a bounded chance to finish
    /// journal/queue cancellation; only then is the abort fallback used.
    pub async fn stop(&self) -> Result<(), ParticipantRuntimeError> {
        let _stop_owner = self.stop_serial.lock().await;
        let _ = self.request_stop()?;
        self.wait_for_stop_cleanup().await;

        let worker_handle = { lock(&self.worker).take() };
        if let Some(worker) = worker_handle {
            let abort = worker.abort_handle();
            let mut worker = worker;
            if tokio::time::timeout(STOP_WORKER_SETTLE, &mut worker)
                .await
                .is_err()
            {
                abort.abort();
                let _ = worker.await;
            }
        }
        self.teardown_subscription();
        let mut state = lock(&self.state);
        state.lifecycle = ParticipantLifecycle::Stopped;
        state.terminal_pending = false;
        self.publish_lifecycle(ParticipantLifecycle::Stopped);
        Ok(())
    }

    async fn wait_for_stop_cleanup(&self) {
        let mut receiver = self.stop_cleanup.subscribe();
        loop {
            if *receiver.borrow() {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }

    fn rollback_startup_failure(&self, error: &ParticipantRuntimeError) {
        self.admission_cancelled.store(true, Ordering::Release);
        self.event_queue.close_admission();
        let cleanup = {
            let _serial = lock(&self.admission_serial);
            let mut state = lock(&self.state);
            if state.lifecycle != ParticipantLifecycle::Running {
                return;
            }
            state.lifecycle = ParticipantLifecycle::Faulted;
            self.publish_lifecycle(ParticipantLifecycle::Faulted);
            let mut cleanup = Cleanup::required();
            merge_active_cleanup(&mut cleanup, take_cleanup(&mut state));
            state.generation = state.generation.saturating_add(1);
            state.scope = None;
            state.terminal_pending = false;
            self.publish_generation(state.generation);
            self.fact_owner
                .bind_scope(state.generation, state.scope.clone());
            cleanup
        };
        self.perform_cleanup(
            cleanup,
            "participant_startup_failed",
            AgentError::new(AgentErrorCode::ScopeInvalid, handler_code(error)),
        );
        self.frame_source.release_retained_triggers();
    }

    fn ensure_running(&self) -> Result<(), ParticipantRuntimeError> {
        if self.admission_cancelled.load(Ordering::Acquire) {
            return Err(ParticipantRuntimeError::Stopped);
        }
        match lock(&self.state).lifecycle {
            ParticipantLifecycle::Created => Err(ParticipantRuntimeError::NotStarted),
            ParticipantLifecycle::Stopped | ParticipantLifecycle::Stopping => {
                Err(ParticipantRuntimeError::Stopped)
            }
            ParticipantLifecycle::Faulted => Err(ParticipantRuntimeError::Faulted),
            ParticipantLifecycle::Running => Ok(()),
        }
    }

    fn admit_scope_for_backend(
        &self,
        event: &BackendEventEnvelope,
        scope: &ParticipantScope,
    ) -> BackendAdmission {
        let terminal = backend_event_is_terminal(event);
        let scope_invalidation = backend_event_is_scope_invalidation(event);
        let reconnect_control = backend_event_is_reconnect_control(event);
        let connection_request = backend_event_is_connection_request(event);
        let transition = backend_event_is_scope_transition(event);
        let mut cleanup = Cleanup::empty();

        let mut state = lock(&self.state);

        // A close must belong to the currently active backend attempt. A
        // duplicate or late close cannot invalidate a later reconnect.
        if scope_invalidation {
            if state.scope.as_ref() != Some(scope)
                || state.active_connection_attempt_id.as_deref()
                    != Some(event.connection_attempt_id.as_str())
            {
                return BackendAdmission::StaleIgnored;
            }
            cleanup = Cleanup::required();
            merge_active_cleanup(&mut cleanup, take_cleanup(&mut state));
            state.generation = state.generation.saturating_add(1);
            state.scope = None;
            state.closed_scope = Some(scope.clone());
            state.closed_connection_attempt_id = Some(event.connection_attempt_id.clone());
            state.active_connection_attempt_id = None;
            self.publish_generation(state.generation);
            self.fact_owner
                .bind_scope(state.generation, state.scope.clone());
            state.next_ordinal = state.next_ordinal.saturating_add(1);
            return BackendAdmission::Accepted {
                generation: state.generation,
                ordinal: state.next_ordinal,
                cleanup,
                scope_control: true,
                record_fact: false,
                terminal: false,
            };
        }

        // ReconnectScheduled is journaled through the same FIFO, but it must
        // not reopen the closed scope or become a pending fact for the next
        // epoch.
        let reconnect_after_close = reconnect_control
            && state.scope.is_none()
            && state.closed_scope.as_ref() == Some(scope)
            && state.closed_connection_attempt_id.as_deref()
                == Some(event.connection_attempt_id.as_str());
        if reconnect_after_close {
            state.next_ordinal = state.next_ordinal.saturating_add(1);
            return BackendAdmission::Accepted {
                generation: state.generation,
                ordinal: state.next_ordinal,
                cleanup,
                scope_control: true,
                record_fact: false,
                terminal: false,
            };
        }

        let reopening_after_close = connection_request
            && state.scope.is_none()
            && state.closed_scope.as_ref().is_some_and(|closed| {
                if closed.process_session_id != scope.process_session_id {
                    return true;
                }
                let epoch_advanced = scope.connection_epoch > closed.connection_epoch;
                let attempt_changed = state
                    .closed_connection_attempt_id
                    .as_deref()
                    .is_some_and(|attempt| attempt != event.connection_attempt_id);
                epoch_advanced && attempt_changed
            });
        let terminal_after_close = terminal
            && state.scope.is_none()
            && state.closed_scope.as_ref() == Some(scope)
            && state.closed_connection_attempt_id.as_deref()
                == Some(event.connection_attempt_id.as_str());

        // Until a new ConnectionRequested opens a scope, no ordinary event
        // can create one after a close. A terminal envelope is the only other
        // accepted control path.
        if state.scope.is_none()
            && state.closed_scope.is_some()
            && !reopening_after_close
            && !terminal_after_close
        {
            return BackendAdmission::StaleIgnored;
        }
        if !reopening_after_close
            && !terminal_after_close
            && scope_is_stale(&state, scope, transition)
        {
            return BackendAdmission::StaleIgnored;
        }
        if !connection_request
            && state
                .active_connection_attempt_id
                .as_deref()
                .is_some_and(|attempt| attempt != event.connection_attempt_id)
        {
            return BackendAdmission::StaleIgnored;
        }

        let changed = state.scope.as_ref() != Some(scope)
            || state
                .active_connection_attempt_id
                .as_deref()
                .is_some_and(|attempt| attempt != event.connection_attempt_id);
        if state.scope.is_some() && changed {
            cleanup = Cleanup::required();
            merge_active_cleanup(&mut cleanup, take_cleanup(&mut state));
            state.generation = state.generation.saturating_add(1);
            self.publish_generation(state.generation);
        }
        if changed {
            let previous_process = state.scope.as_ref().and_then(|current| {
                (current.process_session_id != scope.process_session_id)
                    .then(|| current.process_session_id.clone())
            });
            if let Some(previous_process) = previous_process {
                retire_process_session(&mut state, &previous_process);
            }
            if let Some(closed) = state.closed_scope.take() {
                if closed.process_session_id != scope.process_session_id {
                    retire_process_session(&mut state, &closed.process_session_id);
                }
            }
            state.closed_connection_attempt_id = None;
            state.scope = Some(scope.clone());
            state.active_connection_attempt_id = Some(event.connection_attempt_id.clone());
        } else if state.active_connection_attempt_id.is_none() {
            // Internal scope facts may establish the structural scope before
            // the first backend envelope supplies its attempt identity. Bind
            // that identity without invalidating the already admitted facts.
            state.active_connection_attempt_id = Some(event.connection_attempt_id.clone());
        }
        if terminal {
            cleanup.required = true;
            merge_active_cleanup(&mut cleanup, take_cleanup(&mut state));
            state.generation = state.generation.saturating_add(1);
            state.scope = None;
            state.active_connection_attempt_id = None;
            self.publish_generation(state.generation);
            state.lifecycle = ParticipantLifecycle::Stopping;
            state.terminal_pending = true;
            self.publish_lifecycle(ParticipantLifecycle::Stopping);
        }
        self.fact_owner
            .bind_scope(state.generation, state.scope.clone());
        state.next_ordinal = state.next_ordinal.saturating_add(1);
        let ordinal = state.next_ordinal;
        BackendAdmission::Accepted {
            generation: state.generation,
            ordinal,
            cleanup,
            scope_control: false,
            record_fact: !terminal,
            terminal,
        }
    }

    fn admit_explicit_scope(
        &self,
        scope: &ParticipantScope,
        allow_same_epoch_transition: bool,
        terminal_lifecycle: Option<ParticipantLifecycle>,
    ) -> ExplicitAdmission {
        let mut cleanup = Cleanup::empty();
        let mut state = lock(&self.state);
        if scope_is_stale(&state, scope, allow_same_epoch_transition) {
            return ExplicitAdmission::StaleIgnored;
        }
        if state.scope.as_ref() != Some(scope) {
            let previous_process = state.scope.as_ref().and_then(|current| {
                (current.process_session_id != scope.process_session_id)
                    .then(|| current.process_session_id.clone())
            });
            if let Some(previous_process) = previous_process {
                retire_process_session(&mut state, &previous_process);
            }
            if let Some(closed) = state.closed_scope.take() {
                if closed.process_session_id != scope.process_session_id {
                    retire_process_session(&mut state, &closed.process_session_id);
                }
            }
            state.closed_connection_attempt_id = None;
            cleanup = Cleanup::required();
            merge_active_cleanup(&mut cleanup, take_cleanup(&mut state));
            state.generation = state.generation.saturating_add(1);
            self.publish_generation(state.generation);
        }
        state.scope = Some(scope.clone());
        if let Some(terminal_lifecycle) = terminal_lifecycle {
            cleanup.required = true;
            merge_active_cleanup(&mut cleanup, take_cleanup(&mut state));
            state.generation = state.generation.saturating_add(1);
            state.scope = None;
            self.publish_generation(state.generation);
            state.lifecycle = ParticipantLifecycle::Stopping;
            state.terminal_pending = true;
            self.publish_lifecycle(ParticipantLifecycle::Stopping);
            self.fact_owner
                .bind_scope(state.generation, state.scope.clone());
            state.next_ordinal = state.next_ordinal.saturating_add(1);
            return ExplicitAdmission::Accepted {
                generation: state.generation,
                ordinal: state.next_ordinal,
                cleanup,
                terminal_lifecycle: Some(terminal_lifecycle),
            };
        }
        self.fact_owner
            .bind_scope(state.generation, state.scope.clone());
        state.next_ordinal = state.next_ordinal.saturating_add(1);
        ExplicitAdmission::Accepted {
            generation: state.generation,
            ordinal: state.next_ordinal,
            cleanup,
            terminal_lifecycle: None,
        }
    }

    fn evaluate_backend_wake(
        &self,
        event: &BackendEventEnvelope,
        scope: &ParticipantScope,
    ) -> Result<(String, Option<PlayerChatMessage>), ParticipantRuntimeError> {
        let event_type = backend_event_type(event).to_owned();
        if backend_event_is_terminal(event) {
            return Ok((event_type, None));
        }
        let Some(chat) = as_chat_event(event) else {
            return Ok((event_type, None));
        };
        let context = self.frame_source.chat_context(scope)?;
        let Some(message) = interpret_player_chat(&chat, &context) else {
            return Ok((event_type, None));
        };
        if self.wake_registry.addresses_player_chat(&message) {
            Ok((event_type, Some(message)))
        } else {
            Ok((event_type, None))
        }
    }

    fn record_fact(&self, generation: u64, fact: ParticipantFact) {
        self.fact_owner.record(generation, fact);
    }

    fn record_pending_omission(&self, generation: u64, event_type: String) {
        self.fact_owner.record_omission(generation, event_type);
    }

    fn notify_admission_observer(&self, event_type: &str) {
        let observer = lock(&self.admission_observer).clone();
        if let Some(observer) = observer {
            observer.after_work_admitted_before_fact(event_type);
        }
    }

    /// Drains facts at the opening-frame processing boundary, rather than at
    /// wake admission.  A queued wake therefore cannot claim facts which are
    /// still observable by an active run's body observationAfter.
    fn drain_pending_facts(
        &self,
        scope: &ParticipantScope,
        generation: u64,
    ) -> Option<(Vec<ParticipantFact>, u64, Vec<String>)> {
        let _serial = lock(&self.admission_serial);
        let state = lock(&self.state);
        if state.generation != generation
            || state.lifecycle != ParticipantLifecycle::Running
            || state.scope.as_ref() != Some(scope)
        {
            return None;
        }
        drop(state);
        let batch = self.fact_owner.drain_locked(scope, generation)?;
        Some((batch.facts, batch.omitted, batch.omitted_types))
    }

    fn enqueue_work(
        &self,
        item: WorkItem,
        serial: &mut AdmissionSerialGuard<'_>,
    ) -> Result<QueueAdmission, ParticipantRuntimeError> {
        self.event_queue
            .enqueue(item, serial, |item| self.admission_item_is_current(item))
    }

    fn invalidate_generation(&self) -> Cleanup {
        let mut state = lock(&self.state);
        let mut cleanup = Cleanup::required();
        merge_active_cleanup(&mut cleanup, take_cleanup(&mut state));
        state.generation = state.generation.saturating_add(1);
        state.scope = None;
        state.terminal_pending = false;
        self.publish_generation(state.generation);
        self.fact_owner
            .bind_scope(state.generation, state.scope.clone());
        cleanup
    }

    fn perform_cleanup(&self, cleanup: Cleanup, reason: &str, cancellation_error: AgentError) {
        if !cleanup.required {
            return;
        }
        let _cleanup_serial = lock(&self.cleanup_serial);
        if let Some(cancellation) = cleanup.cancellation {
            cancellation.cancel(cancellation_error);
        }
        if let Some(start_gate) = cleanup.start_gate {
            start_gate.open();
        }
        if let Some(abort) = cleanup.abort {
            abort.abort();
        }
        self.speech.cancel_remaining(reason);
        match self.backend.motor() {
            Ok(motor) => {
                if let Err(error) = motor.release_all() {
                    self.fail_runtime_sync(
                        ParticipantFailureSource::BodyRelease,
                        "body_release_failed",
                        "body release failed",
                        None,
                    );
                    let _ = error;
                }
            }
            Err(_error) => self.fail_runtime_sync(
                ParticipantFailureSource::BodyRelease,
                "body_motor_unavailable",
                "body motor unavailable during release",
                None,
            ),
        }
        self.debug.update(DebugStateUpdate {
            body: Some(None),
            current_body_tool: Some(None),
            ..DebugStateUpdate::default()
        });
    }

    fn fail_runtime_sync(
        &self,
        source: ParticipantFailureSource,
        code: &str,
        summary: &str,
        scope: Option<ParticipantScope>,
    ) {
        let failure = ParticipantFailure {
            source,
            code: code.to_owned(),
            summary: bounded_summary(summary),
            scope: scope.clone(),
        };
        let _ = self.failures.send(failure.clone());
        self.debug.failure(DebugFailureSummary {
            at: self.clock.now(),
            source: match &failure.source {
                ParticipantFailureSource::Backend => DebugFailureSource::Backend,
                ParticipantFailureSource::Source => DebugFailureSource::Runtime,
                ParticipantFailureSource::Journal => DebugFailureSource::Runtime,
                ParticipantFailureSource::Model => DebugFailureSource::Model,
                ParticipantFailureSource::Runtime => DebugFailureSource::Runtime,
                ParticipantFailureSource::BodyRelease => DebugFailureSource::BodyTool,
            },
            code: failure.code,
            summary: failure.summary,
        });
    }

    fn report_admission_error(&self, error: ParticipantRuntimeError) {
        self.fail_runtime_sync(
            failure_source(&error),
            handler_code(&error),
            &handler_summary(&error),
            self.current_scope(),
        );
        self.journal_failure_detached(&error);
        self.mark_faulted_after_handler();
    }

    fn journal_failure_detached(&self, error: &ParticipantRuntimeError) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let journal = Arc::clone(&self.journal);
        let code = handler_code(error).to_owned();
        handle.spawn(async move {
            let payload = json!({
                "code": code,
                "summary": "participant handler failure",
            });
            if let Some(payload) = payload.as_object().cloned() {
                let _ = journal
                    .append("participant.failure".to_owned(), payload)
                    .await;
            }
        });
    }

    fn is_normal_admission_race(&self, error: &ParticipantRuntimeError) -> bool {
        matches!(
            error,
            ParticipantRuntimeError::NotStarted
                | ParticipantRuntimeError::Stopped
                | ParticipantRuntimeError::Faulted
        ) || (matches!(error, ParticipantRuntimeError::QueueClosed)
            && (self.admission_cancelled.load(Ordering::Acquire)
                || self.lifecycle() != ParticipantLifecycle::Running))
    }

    async fn worker_loop(self: Arc<Self>) {
        loop {
            if self.worker_should_exit() {
                self.teardown_subscription();
                return;
            }
            let Some(item) = self.event_queue.next().await else {
                self.teardown_subscription();
                return;
            };
            if let Err(error) = self.process_item(item).await {
                self.fail_runtime_sync(
                    failure_source(&error),
                    handler_code(&error),
                    &handler_summary(&error),
                    self.current_scope(),
                );
                let _ = self.journal_failure(&error).await;
                if !is_recoverable_wake_error(&error) {
                    self.mark_faulted_after_handler();
                    self.teardown_subscription();
                    return;
                }
            }
        }
    }

    async fn process_item(&self, item: WorkItem) -> Result<(), ParticipantRuntimeError> {
        if !self.item_is_current(&item) {
            self.release_item_trigger(&item);
            return Ok(());
        }
        if let Err(error) = self.append_event_journal(&item).await {
            self.release_item_trigger(&item);
            return Err(error);
        }
        if !self.item_is_current(&item) {
            self.release_item_trigger(&item);
            return Ok(());
        }
        if item.terminal {
            let terminal_lifecycle = item.terminal_lifecycle.ok_or_else(|| {
                ParticipantRuntimeError::Handler("terminal item missing lifecycle".to_owned())
            })?;
            self.finish_terminal(terminal_lifecycle);
            return Ok(());
        }
        if item.wake.is_none() {
            return Ok(());
        }
        self.process_wake(item).await
    }

    fn release_item_trigger(&self, item: &WorkItem) {
        if let Some(wake) = item.wake.as_ref().filter(|wake| wake.trigger_retained) {
            self.frame_source
                .release_trigger(&wake.scope, &wake.trigger);
        }
    }

    async fn process_wake(&self, item: WorkItem) -> Result<(), ParticipantRuntimeError> {
        let wake = item
            .wake
            .as_ref()
            .ok_or_else(|| ParticipantRuntimeError::Handler("wake item missing".to_owned()))?;
        let capture_result = self.frame_source.capture(&wake.scope);
        if wake.trigger_retained {
            self.frame_source
                .release_trigger(&wake.scope, &wake.trigger);
        }
        let capture = capture_result?;
        let memory = self.read_memory(&item.generation).await?;
        if !self.generation_is_current(item.generation) {
            return Ok(());
        }
        let Some((facts, omitted, omitted_types)) =
            self.drain_pending_facts(&wake.scope, item.generation)
        else {
            return Ok(());
        };
        let context = self.assemble_frame(wake, capture, memory, facts, omitted, omitted_types)?;
        let run_id = RunId::new(format!(
            "p-{}-{}-{}-{}-{}",
            self.run_id_namespace_digest,
            self.run_id_instance_id,
            base36_u64(wake.scope.connection_epoch),
            base36_u64(item.generation),
            base36_u64(wake.ordinal),
        ))
        .map_err(|error| ParticipantRuntimeError::Handler(error.to_owned()))?;
        let request = AgentRunRequest {
            run_id: run_id.clone(),
            context,
            tools: self.agent.definitions(),
            prompt_template: self.prompt_template.clone(),
        };
        request
            .validate()
            .map_err(|error| ParticipantRuntimeError::Handler(error.to_string()))?;

        let deadline = Deadline::after(Instant::now(), self.run_deadline)
            .map_err(|error| ParticipantRuntimeError::Handler(error.to_string()))?;
        let cancellation = ParticipantCancellation::new();
        let start_gate = ParticipantStartGate::new();
        if !self.publish_active(
            item.generation,
            Arc::clone(&cancellation),
            Arc::clone(&start_gate),
        ) {
            return Ok(());
        }
        let agent = Arc::clone(&self.agent);
        let task_cancellation = Arc::clone(&cancellation);
        let task_start_gate = Arc::clone(&start_gate);
        let run_scope = wake.scope.clone();
        let run_generation = item.generation;
        let trigger_event_id = item.event_id.clone();
        let task: JoinHandle<Result<mineintent_contracts::agent::ModelRunResult, AgentError>> =
            tokio::spawn(async move {
                let start = tokio::select! {
                    biased;
                    error = task_cancellation.cancelled() => Err(error),
                    _ = task_start_gate.wait() => Ok(()),
                };
                if let Err(error) = start {
                    return Err(error);
                }
                if let Some(error) = task_cancellation.cancellation_error() {
                    return Err(error);
                }
                let control = ExecutionControl::new(task_cancellation.as_ref(), deadline);
                agent
                    .run(
                        run_scope,
                        run_generation,
                        trigger_event_id,
                        request,
                        control,
                    )
                    .await
            });
        let abort = task.abort_handle();
        if !self.attach_active_abort(item.generation, &cancellation, abort.clone()) {
            cancellation.cancel(AgentError::new(
                AgentErrorCode::ScopeInvalid,
                "participant_scope_changed",
            ));
            start_gate.open();
            abort.abort();
            return Ok(());
        }
        start_gate.open();
        self.debug.update(DebugStateUpdate {
            decision: Some(Some(DebugDecision {
                status: DebugDecisionStatus::Running,
                run_id: Some(run_id.to_string()),
                model: None,
                started_at: Some(wake.occurred_at.clone()),
                context_sources: Vec::new(),
                retrieved_memory_ids: Vec::new(),
            })),
            ..DebugStateUpdate::default()
        });

        let result = tokio::select! {
            biased;
            _ = self.stopping_changed() => {
                cancellation.cancel(AgentError::run_cancelled());
                abort.abort();
                return Ok(());
            }
            _ = self.generation_changed(item.generation) => {
                cancellation.cancel(AgentError::new(AgentErrorCode::ScopeInvalid, "participant_scope_changed"));
                abort.abort();
                return Ok(());
            }
            _ = cancellation.cancelled() => {
                abort.abort();
                return Ok(());
            }
            joined = task => joined.map_err(|error| ParticipantRuntimeError::Handler(error.to_string()))?,
        };
        self.clear_active(item.generation, &cancellation);
        match result {
            Ok(_) => {
                self.debug.update(DebugStateUpdate {
                    decision: Some(Some(DebugDecision::idle())),
                    ..DebugStateUpdate::default()
                });
                Ok(())
            }
            Err(error) if is_normal_agent_error(&error) => {
                self.debug.update(DebugStateUpdate {
                    decision: Some(Some(DebugDecision::idle())),
                    ..DebugStateUpdate::default()
                });
                Ok(())
            }
            Err(error) => {
                self.debug.update(DebugStateUpdate {
                    decision: Some(Some(DebugDecision {
                        status: DebugDecisionStatus::Failed,
                        run_id: Some(run_id.to_string()),
                        model: None,
                        started_at: Some(wake.occurred_at.clone()),
                        context_sources: Vec::new(),
                        retrieved_memory_ids: Vec::new(),
                    })),
                    ..DebugStateUpdate::default()
                });
                Err(ParticipantRuntimeError::Handler(format!(
                    "agent runner returned {}",
                    error.code
                )))
            }
        }
    }

    async fn append_event_journal(&self, item: &WorkItem) -> Result<(), ParticipantRuntimeError> {
        let payload = event_payload(item);
        let future = self.journal.append("participant.event".to_owned(), payload);
        tokio::pin!(future);
        if item.wake.is_none() && item.scope_control {
            return future
                .await
                .map_err(|error| ParticipantRuntimeError::Handler(error.to_string()));
        }
        tokio::select! {
            biased;
            _ = self.generation_changed(item.generation) => Ok(()),
            result = &mut future => result.map_err(|error| ParticipantRuntimeError::Handler(error.to_string())),
        }
    }

    async fn journal_failure(&self, error: &ParticipantRuntimeError) -> Result<(), ()> {
        let payload =
            json!({"code": handler_code(error), "summary": "participant handler failure"});
        let Some(payload) = payload.as_object().cloned() else {
            return Err(());
        };
        self.journal
            .append("participant.failure".to_owned(), payload)
            .await
            .map_err(|_| ())
    }

    async fn read_memory(&self, generation: &u64) -> Result<String, ParticipantRuntimeError> {
        let future = self.memory.read_full();
        tokio::pin!(future);
        tokio::select! {
            biased;
            _ = self.stopping_changed() => Ok(String::new()),
            _ = self.generation_changed(*generation) => Ok(String::new()),
            result = &mut future => result.map_err(ParticipantRuntimeError::Memory),
        }
    }

    fn assemble_frame(
        &self,
        wake: &WakeItem,
        mut capture: ParticipantFrameCapture,
        memory: String,
        facts: Vec<ParticipantFact>,
        omitted: u64,
        omitted_types: Vec<String>,
    ) -> Result<JsonAgentDecisionContextV5, ParticipantRuntimeError> {
        if capture.light.is_none() {
            return Err(ParticipantSourceError::MissingLight.into());
        }
        if capture.dimension.is_empty() {
            return Err(ParticipantSourceError::Invalid(
                "frame dimension must not be empty".to_owned(),
            )
            .into());
        }
        if wake.scope.dimension.as_deref() != Some(capture.dimension.as_str()) {
            return Err(ParticipantSourceError::Invalid(
                "frame dimension does not match event scope".to_owned(),
            )
            .into());
        }
        if let Some(status) = capture.status.as_mut() {
            if status.armor == Some(0) {
                status.armor = None;
            }
        }
        let Some(trigger_chat) = capture
            .unread_chat
            .iter()
            .find(|chat| {
                chat.message.username == wake.trigger.sender.username
                    && chat.message.text == wake.trigger.text
                    && chat.message.at == wake.trigger.occurred_at
            })
            .cloned()
        else {
            return Err(AgentContextV5AssemblyError::TriggerChatNotInUnreadWindow.into());
        };
        let trigger_sequence = trigger_chat.sequence;
        let trigger_message = trigger_chat.message;
        let duplicate_count = capture
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    AgentContextV5EventInput::PlayerChat {
                        sequence,
                        message,
                    } if *sequence == trigger_sequence
                        && same_chat_identity(message, &trigger_message)
                )
            })
            .count();
        if duplicate_count > 1 {
            return Err(AgentContextV5AssemblyError::DuplicatePlayerChatEvent.into());
        }
        if duplicate_count == 1 {
            capture.events.retain(|event| {
                !matches!(
                    event,
                    AgentContextV5EventInput::PlayerChat {
                        sequence,
                        message,
                    } if *sequence == trigger_sequence
                        && same_chat_identity(message, &trigger_message)
                )
            });
        }
        let mut events = capture.events;
        for fact in &facts {
            if fact.scope != wake.scope {
                continue;
            }
            events.push(AgentContextV5EventInput::Summary {
                event_type: safe_fact_event_type(&fact.event_type),
                summary: fact.summary.clone(),
            });
        }
        if omitted > 0 {
            let omitted_types = if omitted_types.is_empty() {
                String::new()
            } else {
                format!("; types={}", omitted_types.join(","))
            };
            events.push(AgentContextV5EventInput::Summary {
                event_type: "participant_events_omitted".to_owned(),
                summary: format!("{} pending events omitted{}", omitted, omitted_types),
            });
        }
        self.assembler
            .assemble(AgentContextV5Input {
                memory,
                at: capture.at,
                dimension: capture.dimension,
                pose: capture.pose,
                status: capture.status,
                hotbar: capture.hotbar,
                unread_chat: capture.unread_chat,
                unread_chat_omitted: capture.unread_chat_omitted,
                sound: capture.sound,
                light: capture.light.expect("light checked above"),
                events,
                omissions: capture.omissions,
                trigger_chat: Some(AgentChatTriggerV5 {
                    sequence: trigger_sequence,
                    message: trigger_message,
                }),
            })
            .map_err(ParticipantRuntimeError::Frame)
    }

    fn item_is_current(&self, item: &WorkItem) -> bool {
        let state = lock(&self.state);
        item.terminal
            || (state.lifecycle == ParticipantLifecycle::Running
                && item.wake.is_none()
                && item.scope_control)
            || (state.generation == item.generation
                && state.lifecycle == ParticipantLifecycle::Running
                && state.scope.as_ref() == Some(&item.scope))
    }

    fn admission_item_is_current(&self, item: &WorkItem) -> bool {
        let state = lock(&self.state);
        item.terminal
            || (state.lifecycle == ParticipantLifecycle::Running
                && item.wake.is_none()
                && item.scope_control)
            || (state.lifecycle == ParticipantLifecycle::Running
                && state.generation == item.generation
                && state.scope.as_ref() == Some(&item.scope))
    }

    fn generation_is_current(&self, generation: u64) -> bool {
        lock(&self.state).generation == generation
    }

    async fn generation_changed(&self, generation: u64) {
        let mut receiver = self.generation.subscribe();
        loop {
            if *receiver.borrow() != generation {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }

    fn publish_generation(&self, generation: u64) {
        self.generation.send_replace(generation);
    }

    fn publish_lifecycle(&self, lifecycle: ParticipantLifecycle) {
        self.lifecycle_signal.send_replace(lifecycle);
    }

    async fn stopping_changed(&self) {
        let mut receiver = self.lifecycle_signal.subscribe();
        if self.is_stopping() {
            return;
        }
        let _ = receiver.changed().await;
    }

    fn worker_should_exit(&self) -> bool {
        let state = lock(&self.state);
        matches!(
            state.lifecycle,
            ParticipantLifecycle::Stopped | ParticipantLifecycle::Faulted
        ) || (state.lifecycle == ParticipantLifecycle::Stopping && !state.terminal_pending)
    }

    fn finish_terminal(&self, terminal_lifecycle: ParticipantLifecycle) {
        self.teardown_subscription();
        self.admission_cancelled.store(true, Ordering::Release);
        self.event_queue.close_admission();
        self.frame_source.release_retained_triggers();
        let mut state = lock(&self.state);
        if state.lifecycle == ParticipantLifecycle::Stopping && state.terminal_pending {
            state.lifecycle = terminal_lifecycle;
            state.terminal_pending = false;
            self.publish_lifecycle(terminal_lifecycle);
        }
    }

    fn teardown_subscription(&self) {
        if let Some(mut subscription) = lock(&self.subscription).take() {
            subscription.unsubscribe();
        }
    }

    fn publish_active(
        &self,
        generation: u64,
        cancellation: Arc<ParticipantCancellation>,
        start_gate: Arc<ParticipantStartGate>,
    ) -> bool {
        let mut state = lock(&self.state);
        if state.generation != generation || state.lifecycle != ParticipantLifecycle::Running {
            return false;
        }
        state.active = Some(ActiveRun {
            cancellation,
            abort: None,
            start_gate,
        });
        true
    }

    fn attach_active_abort(
        &self,
        generation: u64,
        cancellation: &Arc<ParticipantCancellation>,
        abort: AbortHandle,
    ) -> bool {
        let mut state = lock(&self.state);
        if state.generation != generation || state.lifecycle != ParticipantLifecycle::Running {
            return false;
        }
        let Some(active) = state.active.as_mut() else {
            return false;
        };
        if !Arc::ptr_eq(&active.cancellation, cancellation) {
            return false;
        }
        active.abort = Some(abort);
        true
    }

    fn clear_active(&self, generation: u64, cancellation: &Arc<ParticipantCancellation>) {
        let mut state = lock(&self.state);
        if state.generation == generation
            && state
                .active
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(&active.cancellation, cancellation))
        {
            state.active = None;
        }
    }

    fn is_stopping(&self) -> bool {
        matches!(
            lock(&self.state).lifecycle,
            ParticipantLifecycle::Stopping
                | ParticipantLifecycle::Stopped
                | ParticipantLifecycle::Faulted
        )
    }

    fn mark_faulted_after_handler(&self) {
        self.admission_cancelled.store(true, Ordering::Release);
        self.event_queue.close_admission();
        let cleanup = {
            let _serial = lock(&self.admission_serial);
            let mut state = lock(&self.state);
            if state.lifecycle == ParticipantLifecycle::Stopping
                || state.lifecycle == ParticipantLifecycle::Stopped
            {
                return;
            }
            state.lifecycle = ParticipantLifecycle::Faulted;
            self.publish_lifecycle(ParticipantLifecycle::Faulted);
            let mut cleanup = Cleanup::required();
            merge_active_cleanup(&mut cleanup, take_cleanup(&mut state));
            state.generation = state.generation.saturating_add(1);
            state.scope = None;
            state.terminal_pending = false;
            self.publish_generation(state.generation);
            self.fact_owner
                .bind_scope(state.generation, state.scope.clone());
            cleanup
        };
        self.perform_cleanup(
            cleanup,
            "participant_handler_failed",
            AgentError::new(AgentErrorCode::ScopeInvalid, "participant_handler_failed"),
        );
        self.frame_source.release_retained_triggers();
    }
}

impl<R> BackendEventListener for ParticipantRuntime<R>
where
    R: ParticipantAgentPort + 'static,
{
    fn on_event(&self, event: BackendEventEnvelope) {
        if let Err(error) = self.ingest_backend_event(event) {
            if !self.is_normal_admission_race(&error) {
                self.report_admission_error(error);
            }
        }
    }
}

#[derive(Clone)]
struct ParticipantStartGate {
    opened: watch::Sender<bool>,
}

impl ParticipantStartGate {
    fn new() -> Arc<Self> {
        let (opened, _) = watch::channel(false);
        Arc::new(Self { opened })
    }

    fn open(&self) {
        self.opened.send_replace(true);
    }

    async fn wait(&self) {
        let mut receiver = self.opened.subscribe();
        loop {
            if *receiver.borrow() {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

struct ParticipantCancellation {
    error: watch::Sender<Option<AgentError>>,
}

impl ParticipantCancellation {
    fn new() -> Arc<Self> {
        let (error, _) = watch::channel(None);
        Arc::new(Self { error })
    }

    fn cancel(&self, error: AgentError) {
        let _ = self.error.send_if_modified(|current| {
            if current.is_none() {
                *current = Some(error.clone());
                true
            } else {
                false
            }
        });
    }
}

impl CancellationSignal for ParticipantCancellation {
    fn cancellation_error(&self) -> Option<AgentError> {
        self.error.borrow().clone()
    }

    fn cancelled(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = AgentError> + Send + '_>> {
        Box::pin(async move {
            let mut receiver = self.error.subscribe();
            loop {
                if let Some(error) = receiver.borrow().clone() {
                    return error;
                }
                if receiver.changed().await.is_err() {
                    return AgentError::run_cancelled();
                }
            }
        })
    }
}

fn take_cleanup(state: &mut RuntimeState) -> Cleanup {
    state
        .active
        .take()
        .map_or_else(Cleanup::empty, |active| Cleanup {
            required: true,
            cancellation: Some(active.cancellation),
            abort: active.abort,
            start_gate: Some(active.start_gate),
        })
}

fn merge_active_cleanup(left: &mut Cleanup, right: Cleanup) {
    left.required |= right.required;
    if left.cancellation.is_none() {
        left.cancellation = right.cancellation;
    }
    if left.abort.is_none() {
        left.abort = right.abort;
    }
    if left.start_gate.is_none() {
        left.start_gate = right.start_gate;
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn scope_is_stale(
    state: &RuntimeState,
    scope: &ParticipantScope,
    allow_same_epoch_transition: bool,
) -> bool {
    if state
        .retired_process_sessions
        .contains(&namespace_digest(&scope.process_session_id))
    {
        return true;
    }
    if let Some(closed) = state.closed_scope.as_ref() {
        if scope.process_session_id == closed.process_session_id
            && scope.connection_epoch <= closed.connection_epoch
        {
            return true;
        }
    }
    let Some(current) = state.scope.as_ref() else {
        return false;
    };
    scope.process_session_id == current.process_session_id
        && (scope.connection_epoch < current.connection_epoch
            || (scope.connection_epoch == current.connection_epoch
                && scope != current
                && !allow_same_epoch_transition))
}

fn retire_process_session(state: &mut RuntimeState, process_session_id: &str) {
    state
        .retired_process_sessions
        .insert(namespace_digest(process_session_id));
}

fn as_chat_event(event: &BackendEventEnvelope) -> Option<BackendEventEnvelope<ProtocolChatEvent>> {
    let BackendEventEnvelope {
        protocol,
        id,
        kind,
        occurred_at,
        process_session_id,
        connection_epoch,
        connection_attempt_id,
        world_id,
        dimension,
        source,
        payload,
    } = event.clone();
    let BackendEventPayload::Chat(payload) = payload else {
        return None;
    };
    Some(BackendEventEnvelope {
        protocol,
        id,
        kind,
        occurred_at,
        process_session_id,
        connection_epoch,
        connection_attempt_id,
        world_id,
        dimension,
        source,
        payload,
    })
}

fn backend_event_type(event: &BackendEventEnvelope) -> &'static str {
    match event.kind {
        BackendEventKind::Lifecycle => "lifecycle",
        BackendEventKind::SelfState => "self",
        BackendEventKind::World => "world",
        BackendEventKind::Entity => "entity",
        BackendEventKind::Block => "block",
        BackendEventKind::Sound => "sound",
        BackendEventKind::Chat => "player_chat",
        BackendEventKind::PlayerList => "player_list",
        BackendEventKind::SnapshotChanged => "snapshot_changed",
        BackendEventKind::Overflow => "overflow",
    }
}

fn backend_event_summary(event: &BackendEventEnvelope) -> String {
    match &event.payload {
        BackendEventPayload::Chat(_) => "player_chat_not_addressed".to_owned(),
        BackendEventPayload::Lifecycle(payload) => {
            format!("lifecycle:{}", lifecycle_name(payload))
        }
        BackendEventPayload::Sound(_) => "sound_fact".to_owned(),
        BackendEventPayload::SelfState(_) => "self_state_fact".to_owned(),
        BackendEventPayload::World(_) => "world_fact".to_owned(),
        BackendEventPayload::Entity(_) => "entity_fact".to_owned(),
        BackendEventPayload::Block(_) => "block_fact".to_owned(),
        BackendEventPayload::PlayerList(_) => "player_list_fact".to_owned(),
        BackendEventPayload::SnapshotChanged(_) => "snapshot_changed_fact".to_owned(),
        BackendEventPayload::Overflow(_) => "overflow_fact".to_owned(),
    }
}

fn lifecycle_name(payload: &BackendLifecyclePayload) -> &'static str {
    match payload {
        BackendLifecyclePayload::ConnectionRequested { .. } => "connection_requested",
        BackendLifecyclePayload::TransportConnected => "transport_connected",
        BackendLifecyclePayload::LoggedIn { .. } => "logged_in",
        BackendLifecyclePayload::Ready { .. } => "ready",
        BackendLifecyclePayload::Died => "died",
        BackendLifecyclePayload::RespawnTransitionStarted { .. } => "respawn_transition_started",
        BackendLifecyclePayload::Respawned { .. } => "respawned",
        BackendLifecyclePayload::DimensionChanged { .. } => "dimension_changed",
        BackendLifecyclePayload::ReconnectScheduled { .. } => "reconnect_scheduled",
        BackendLifecyclePayload::ConnectionClosed { .. } => "connection_closed",
        BackendLifecyclePayload::Faulted { .. } => "faulted",
        BackendLifecyclePayload::Stopped { .. } => "stopped",
    }
}

fn backend_event_is_terminal(event: &BackendEventEnvelope) -> bool {
    matches!(
        event.payload,
        BackendEventPayload::Lifecycle(
            BackendLifecyclePayload::Faulted { .. } | BackendLifecyclePayload::Stopped { .. }
        )
    )
}

fn backend_event_is_control(event: &BackendEventEnvelope) -> bool {
    matches!(
        event.payload,
        BackendEventPayload::Lifecycle(_) | BackendEventPayload::Overflow(_)
    )
}

fn backend_terminal_lifecycle(event: &BackendEventEnvelope) -> ParticipantLifecycle {
    match &event.payload {
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::Faulted { .. }) => {
            ParticipantLifecycle::Faulted
        }
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::Stopped { .. }) => {
            ParticipantLifecycle::Stopped
        }
        _ => ParticipantLifecycle::Stopped,
    }
}

fn backend_event_is_scope_invalidation(event: &BackendEventEnvelope) -> bool {
    matches!(
        event.payload,
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionClosed { .. })
    )
}

fn backend_event_is_reconnect_control(event: &BackendEventEnvelope) -> bool {
    matches!(
        event.payload,
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::ReconnectScheduled { .. })
    )
}

fn backend_event_is_connection_request(event: &BackendEventEnvelope) -> bool {
    matches!(
        event.payload,
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionRequested { .. })
    )
}

fn backend_event_is_scope_transition(event: &BackendEventEnvelope) -> bool {
    matches!(
        event.payload,
        BackendEventPayload::Lifecycle(
            BackendLifecyclePayload::ConnectionRequested { .. }
                | BackendLifecyclePayload::LoggedIn { .. }
                | BackendLifecyclePayload::Respawned { .. }
                | BackendLifecyclePayload::DimensionChanged { .. }
        )
    )
}

fn same_chat_identity(left: &AgentChatMessageV5, right: &AgentChatMessageV5) -> bool {
    left.username == right.username && left.text == right.text && left.at == right.at
}

fn backend_fact_type(event: &BackendEventEnvelope) -> &'static str {
    if matches!(event.payload, BackendEventPayload::Chat(_)) {
        "player_chat_not_addressed"
    } else {
        backend_event_type(event)
    }
}

pub(crate) fn safe_fact_event_type(event_type: &str) -> String {
    if event_type == "player_chat" {
        "player_chat_fact".to_owned()
    } else {
        event_type.to_owned()
    }
}

fn event_payload(item: &WorkItem) -> JsonObject {
    let wake = item.wake.as_ref().map(|wake| {
        json!({
            "kind": "player_chat",
            "addressed": true,
            "sender": wake.trigger.sender.username,
        })
    });
    let mut value = json!({
        "id": item.event_id,
        "admissionTicket": item.ticket,
        "ordinal": item.ordinal,
        "occurredAt": item.occurred_at,
        "eventType": item.event_type,
        "scope": {
            "processSessionId": item.scope.process_session_id,
            "connectionEpoch": item.scope.connection_epoch,
            "worldId": item.scope.world_id,
            "dimension": item.scope.dimension,
        },
        "wake": wake,
    });
    if let (Some(object), Some(overflow)) = (value.as_object_mut(), item.overflow.as_ref()) {
        object.insert(
            "overflow".to_owned(),
            json!({
                "droppedCount": overflow.dropped_count,
                "droppedTypes": overflow.dropped_types,
            }),
        );
    }
    value.as_object().cloned().unwrap_or_default()
}

fn bounded_summary(value: impl AsRef<str>) -> String {
    value.as_ref().chars().take(256).collect()
}

fn add_overflow_type(types: &mut Vec<String>, event_type: &str) {
    if types.iter().any(|known| known == event_type)
        || types.len() >= PARTICIPANT_MAX_OVERFLOW_TYPES
    {
        return;
    }
    types.push(bounded_summary(event_type));
}

fn add_pending_omitted_type(types: &mut Vec<String>, event_type: &str) {
    if types.iter().any(|known| known == event_type)
        || types.len() >= PARTICIPANT_MAX_PENDING_OMITTED_TYPES
    {
        return;
    }
    types.push(bounded_summary(event_type));
}

fn namespace_digest(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn base36_u64(mut value: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".to_owned();
    }
    let mut digits = [0_u8; 13];
    let mut index = digits.len();
    while value > 0 {
        index -= 1;
        digits[index] = DIGITS[(value % 36) as usize];
        value /= 36;
    }
    String::from_utf8(digits[index..].to_vec()).expect("base36 digits are valid UTF-8")
}

fn utc_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let hour = day_seconds / 3_600;
    let minute = (day_seconds % 3_600) / 60;
    let second = day_seconds % 60;

    // Civil-from-days conversion, using the proleptic Gregorian calendar.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096).div_euclid(365);
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let month = (5 * doy + 2).div_euclid(153);
    let day = doy - (153 * month + 2).div_euclid(5) + 1;
    let month = month + if month < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn handler_summary(error: &ParticipantRuntimeError) -> String {
    match error {
        ParticipantRuntimeError::InvalidConfig(_) => {
            "participant runtime configuration invalid".to_owned()
        }
        ParticipantRuntimeError::Frame(_) => "frame assembly failed".to_owned(),
        ParticipantRuntimeError::Source(_) => "opening frame source failed".to_owned(),
        ParticipantRuntimeError::Memory(_) => "memory read failed".to_owned(),
        ParticipantRuntimeError::Handler(_) => "participant handler failed".to_owned(),
        ParticipantRuntimeError::Backend(_) => "backend operation failed".to_owned(),
        ParticipantRuntimeError::QueueClosed => "participant event queue closed".to_owned(),
        ParticipantRuntimeError::NotStarted => "participant runtime not started".to_owned(),
        ParticipantRuntimeError::AlreadyStarted => "participant runtime already started".to_owned(),
        ParticipantRuntimeError::Stopped => "participant runtime stopped".to_owned(),
        ParticipantRuntimeError::Faulted => "participant runtime faulted".to_owned(),
    }
}

fn handler_code(error: &ParticipantRuntimeError) -> &'static str {
    match error {
        ParticipantRuntimeError::InvalidConfig(_) => "invalid_runtime_configuration",
        ParticipantRuntimeError::Frame(_) => "frame_assembly_failed",
        ParticipantRuntimeError::Source(ParticipantSourceError::MissingLight) => {
            "opening_frame_light_missing"
        }
        ParticipantRuntimeError::Source(_) => "opening_frame_source_failed",
        ParticipantRuntimeError::Memory(_) => "memory_read_failed",
        ParticipantRuntimeError::Handler(_) => "participant_handler_failed",
        ParticipantRuntimeError::Backend(_) => "backend_failed",
        ParticipantRuntimeError::QueueClosed => "queue_closed",
        ParticipantRuntimeError::NotStarted => "not_started",
        ParticipantRuntimeError::AlreadyStarted => "already_started",
        ParticipantRuntimeError::Stopped => "stopped",
        ParticipantRuntimeError::Faulted => "faulted",
    }
}

fn startup_lifecycle_error(lifecycle: ParticipantLifecycle) -> ParticipantRuntimeError {
    match lifecycle {
        ParticipantLifecycle::Stopped | ParticipantLifecycle::Stopping => {
            ParticipantRuntimeError::Stopped
        }
        ParticipantLifecycle::Faulted => ParticipantRuntimeError::Faulted,
        ParticipantLifecycle::Created | ParticipantLifecycle::Running => {
            ParticipantRuntimeError::Handler("participant startup did not complete".to_owned())
        }
    }
}

fn is_recoverable_wake_error(error: &ParticipantRuntimeError) -> bool {
    matches!(
        error,
        ParticipantRuntimeError::Source(_)
            | ParticipantRuntimeError::Frame(_)
            | ParticipantRuntimeError::Memory(_)
    )
}

fn failure_source(error: &ParticipantRuntimeError) -> ParticipantFailureSource {
    match error {
        ParticipantRuntimeError::Source(_) | ParticipantRuntimeError::Frame(_) => {
            ParticipantFailureSource::Source
        }
        ParticipantRuntimeError::Memory(_) => ParticipantFailureSource::Runtime,
        ParticipantRuntimeError::Backend(_) => ParticipantFailureSource::Backend,
        ParticipantRuntimeError::Handler(_) | ParticipantRuntimeError::QueueClosed => {
            ParticipantFailureSource::Runtime
        }
        ParticipantRuntimeError::InvalidConfig(_) => ParticipantFailureSource::Runtime,
        ParticipantRuntimeError::NotStarted
        | ParticipantRuntimeError::AlreadyStarted
        | ParticipantRuntimeError::Stopped
        | ParticipantRuntimeError::Faulted => ParticipantFailureSource::Runtime,
    }
}

fn is_normal_agent_error(error: &AgentError) -> bool {
    matches!(
        error.code,
        AgentErrorCode::RunCancelled
            | AgentErrorCode::DeadlineExceeded
            | AgentErrorCode::ScopeInvalid
    )
}
