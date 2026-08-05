//! Participant 对外端口与事实所有权：scope、帧捕获、各 trait、
//! 唤醒规则登记与 ParticipantFactOwner。

use super::*;

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

    pub(super) fn from_backend(event: &BackendEventEnvelope) -> Self {
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
    pub(super) factory: Arc<dyn ParticipantAgentFactory>,
    pub(super) registry: Arc<ToolCapabilityRegistry>,
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
    pub(super) rules: Vec<WakeRule>,
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

    pub(super) fn addresses_player_chat(&self, message: &PlayerChatMessage) -> bool {
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

pub(super) struct ParticipantFactOwnerState {
    pub(super) generation: u64,
    pub(super) scope: Option<ParticipantScope>,
    pub(super) facts: VecDeque<ParticipantFact>,
    pub(super) omitted: u64,
    pub(super) omitted_types: Vec<String>,
}

/// Bounded, scope-owned facts shared by opening frames and body
/// `observationAfter`. Producers hold the runtime's admission serial before
/// recording; `drain` takes that same serial before inspecting the owner, so
/// an enqueue-then-record gap cannot be observed as a false empty drain.
pub struct ParticipantFactOwner {
    pub(super) admission_serial: Arc<Mutex<()>>,
    pub(super) state: Mutex<ParticipantFactOwnerState>,
}

impl ParticipantFactOwner {
    pub(super) fn new(admission_serial: Arc<Mutex<()>>) -> Arc<Self> {
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

    pub(super) fn drain_locked(
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
    pub(super) fn bind_scope(&self, generation: u64, scope: Option<ParticipantScope>) {
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
    pub(super) fn record(&self, generation: u64, fact: ParticipantFact) {
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
    pub(super) fn record_omission(&self, generation: u64, event_type: String) {
        let mut state = lock(&self.state);
        if state.generation != generation {
            return;
        }
        state.omitted = state.omitted.saturating_add(1);
        // 丢弃意味着模型这一轮少看见了东西，值得留痕；但正是它高频，
        // 所以只在第一条和之后每 100 条各说一次，其余靠计数。
        if state.omitted == 1 || state.omitted % 100 == 0 {
            tracing::warn!(
                target: "mineintent_middle",
                omitted = state.omitted,
                event_type = %event_type,
                "可重建事实被丢弃（队列饱和）；模型本轮会看到 omission 标记"
            );
        }
        add_pending_omitted_type(&mut state.omitted_types, &event_type);
    }
}
