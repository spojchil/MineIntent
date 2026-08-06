//! Truthful production seams for Participant opening frames and body
//! observationAfter.  The source owns only bounded, scope-filtered chat
//! state; self facts come from the backend's one coherent frame-facts call and
//! sound comes from the existing B5 `SoundHistory` product.

use std::{
    collections::{BTreeMap, VecDeque},
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{Arc, Mutex, MutexGuard, Weak},
};

use mineintent_contracts::{
    agent::{
        AgentChatMessageV5, AgentError, AgentErrorCode, AgentFrameV5, AgentHotbarV5,
        AgentItemStackV5, AgentPoseV5, AgentStatusV5, ContractFuture, JsonObject,
    },
    capability::{CapabilityExecutionContext, CapabilityInvocation, ExecutionResource},
    information::{SoundObservation as ContractSoundObservation, SoundValues},
    minecraft::{
        BackendEventEnvelope, BackendEventKind, BackendEventListener, BackendEventPayload,
        BackendEventProtocol, ChatPosition, MinecraftBackendApi, MinecraftFrameFacts, Subscription,
    },
};

use crate::{
    agent::{AgentContextV5Assembler, AgentContextV5EventInput, AgentContextV5Input},
    capability::ObservationAfterSource,
    participant::{
        information_adapters::SoundHistory,
        runtime::{
            safe_fact_event_type, ParticipantFactOwner, ParticipantFrameCapture,
            ParticipantFrameSource, ParticipantScope, ParticipantSourceError,
        },
    },
    speech::{ChatInputContext, PlayerChatMessage},
};

const CHAT_HISTORY_CAPACITY: usize = 8;
/// At most eight addressed wakes can be resident in the Participant control
/// lane, plus the one synchronous admission currently waiting to publish its
/// queue item. This is a hard ownership bound, not an expected traffic rate.
const MAX_PINNED_CHAT_TRIGGERS: usize = 9;
const RECENT_SOUND_LIMIT: f64 = 20.0;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone)]
struct ChatRecord {
    source_event_id: String,
    sequence: u64,
    message: AgentChatMessageV5,
}

struct ChatState {
    disposed: bool,
    scope: Option<ParticipantScope>,
    next_sequence: u64,
    history: VecDeque<ChatRecord>,
    pinned: BTreeMap<String, ChatRecord>,
    omitted: u64,
}

struct ProductionFrameInner {
    backend: Arc<dyn MinecraftBackendApi>,
    sound_history: Arc<SoundHistory>,
    chat: Mutex<ChatState>,
}

struct ProductionChatListener {
    inner: Weak<ProductionFrameInner>,
}

impl BackendEventListener for ProductionChatListener {
    fn on_event(&self, event: BackendEventEnvelope) {
        // 这里不再 catch：backend 的 dispatcher 已经把整个 on_event 包住并报告
        // （facade.rs 的 "listener panic isolated"）。在里面再包一层，只会让
        // 我们自己代码里的 panic 被吞掉、那句报告永远不触发——一条聊天因为
        // 我们的缺陷消失，日志里一个字都没有。
        if let Some(inner) = self.inner.upgrade() {
            inner.record_chat_event(event);
        }
    }
}

impl ProductionFrameInner {
    fn ensure_live(&self) -> Result<(), ParticipantSourceError> {
        if lock(&self.chat).disposed {
            return Err(ParticipantSourceError::Failed(
                "participant frame source is disposed".to_owned(),
            ));
        }
        Ok(())
    }

    fn bind_scope(&self, scope: &ParticipantScope) -> Result<(), ParticipantSourceError> {
        validate_scope(scope)?;
        let mut state = lock(&self.chat);
        if state.disposed {
            return Err(ParticipantSourceError::Failed(
                "participant frame source is disposed".to_owned(),
            ));
        }
        if state.scope.as_ref() != Some(scope) {
            state.scope = Some(scope.clone());
            state.history.clear();
            state.pinned.clear();
            state.omitted = 0;
        }
        Ok(())
    }

    fn record_chat_event(&self, event: BackendEventEnvelope) {
        if event.protocol != BackendEventProtocol::V2 || event.kind != BackendEventKind::Chat {
            return;
        }
        let BackendEventPayload::Chat(payload) = event.payload else {
            return;
        };
        if payload.position != Some(ChatPosition::Chat) {
            return;
        }
        let Some(username) = payload
            .sender_username
            .filter(|username| !username.is_empty())
        else {
            return;
        };

        let event_scope = ParticipantScope::new(
            event.process_session_id,
            event.connection_epoch,
            event.world_id,
            event.dimension,
        );
        let message = AgentChatMessageV5 {
            username,
            text: payload.plain_text,
            at: event.occurred_at,
        };
        if message.validate().is_err() || event.id.is_empty() {
            return;
        }

        let needs_scope_bind = {
            let state = lock(&self.chat);
            state.scope.as_ref() != Some(&event_scope)
        };
        if needs_scope_bind {
            let Ok(snapshot) = self.backend.snapshot() else {
                return;
            };
            if validate_snapshot_scope(&snapshot, &event_scope).is_err() {
                return;
            }
            if self.bind_scope(&event_scope).is_err() {
                return;
            }
        }
        let mut state = lock(&self.chat);
        if state.disposed || state.scope.as_ref() != Some(&event_scope) {
            return;
        }
        if state
            .history
            .iter()
            .any(|record| record.source_event_id == event.id)
            || state.pinned.contains_key(&event.id)
        {
            return;
        }
        let Some(sequence) = allocate_sequence(&mut state) else {
            return;
        };
        push_history(
            &mut state,
            ChatRecord {
                source_event_id: event.id,
                sequence,
                message,
            },
        );
    }

    fn capture_chat(
        &self,
        scope: &ParticipantScope,
    ) -> Result<(Vec<crate::agent::AgentChatInputV5>, u64), ParticipantSourceError> {
        let state = lock(&self.chat);
        if state.disposed {
            return Err(ParticipantSourceError::Failed(
                "participant frame source is disposed".to_owned(),
            ));
        }
        if state.scope.as_ref() != Some(scope) {
            return Err(ParticipantSourceError::StaleScope(
                "chat history scope does not match frame scope".to_owned(),
            ));
        }
        let mut records = state.history.iter().cloned().collect::<Vec<_>>();
        for record in state.pinned.values() {
            if !records
                .iter()
                .any(|known| known.source_event_id == record.source_event_id)
            {
                records.push(record.clone());
            }
        }
        records.sort_by_key(|record| record.sequence);
        Ok((
            records
                .into_iter()
                .map(|record| crate::agent::AgentChatInputV5 {
                    sequence: record.sequence,
                    message: record.message,
                })
                .collect(),
            state.omitted,
        ))
    }

    fn retain_trigger(
        &self,
        scope: &ParticipantScope,
        trigger: &PlayerChatMessage,
    ) -> Result<(), ParticipantSourceError> {
        validate_trigger_scope(scope, trigger)?;
        self.bind_scope(scope)?;
        let mut state = lock(&self.chat);
        if state.disposed {
            return Err(ParticipantSourceError::Failed(
                "participant frame source is disposed".to_owned(),
            ));
        }
        if let Some(existing) = state.pinned.get(&trigger.source_event_id) {
            if existing.message.username == trigger.sender.username
                && existing.message.text == trigger.text
                && existing.message.at == trigger.occurred_at
            {
                return Ok(());
            }
            return Err(ParticipantSourceError::Invalid(
                "addressed trigger identity was reused with different content".to_owned(),
            ));
        }
        if state.pinned.len() >= MAX_PINNED_CHAT_TRIGGERS {
            return Err(ParticipantSourceError::Failed(
                "bounded addressed-trigger retention is full".to_owned(),
            ));
        }
        let record = if let Some(record) = state
            .history
            .iter()
            .find(|record| record.source_event_id == trigger.source_event_id)
            .cloned()
        {
            record
        } else {
            let sequence = allocate_sequence(&mut state).ok_or_else(|| {
                ParticipantSourceError::Failed("chat sequence space exhausted".to_owned())
            })?;
            ChatRecord {
                source_event_id: trigger.source_event_id.clone(),
                sequence,
                message: AgentChatMessageV5 {
                    username: trigger.sender.username.clone(),
                    text: trigger.text.clone(),
                    at: trigger.occurred_at.clone(),
                },
            }
        };
        state.pinned.insert(trigger.source_event_id.clone(), record);
        Ok(())
    }

    fn release_trigger(&self, scope: &ParticipantScope, trigger: &PlayerChatMessage) {
        let mut state = lock(&self.chat);
        if state.disposed || state.scope.as_ref() != Some(scope) {
            return;
        }
        let Some(record) = state.pinned.remove(&trigger.source_event_id) else {
            return;
        };
        if !state
            .history
            .iter()
            .any(|known| known.source_event_id == record.source_event_id)
        {
            state.omitted = state.omitted.saturating_add(1);
        }
    }

    fn release_all_triggers(&self) {
        let mut state = lock(&self.chat);
        let pinned = std::mem::take(&mut state.pinned);
        for record in pinned.values() {
            if !state
                .history
                .iter()
                .any(|known| known.source_event_id == record.source_event_id)
            {
                state.omitted = state.omitted.saturating_add(1);
            }
        }
    }
}

fn allocate_sequence(state: &mut ChatState) -> Option<u64> {
    let sequence = state.next_sequence;
    state.next_sequence = state.next_sequence.checked_add(1)?;
    Some(sequence)
}

fn push_history(state: &mut ChatState, record: ChatRecord) {
    if state.history.len() == CHAT_HISTORY_CAPACITY {
        if let Some(dropped) = state.history.pop_front() {
            if !state.pinned.contains_key(&dropped.source_event_id) {
                state.omitted = state.omitted.saturating_add(1);
            }
        }
    }
    state.history.push_back(record);
}

/// Production backend-backed frame source. The backend subscription is weakly
/// owned by the listener and can be explicitly disposed; dropping the source
/// performs the same teardown.
pub struct ProductionParticipantFrameSource {
    inner: Arc<ProductionFrameInner>,
    chat_subscription: Mutex<Option<Box<dyn Subscription>>>,
    owns_sound_history: bool,
}

impl ProductionParticipantFrameSource {
    pub fn new(
        backend: Arc<dyn MinecraftBackendApi>,
    ) -> Result<Self, mineintent_contracts::minecraft::BackendError> {
        let sound_history = Arc::new(SoundHistory::new(backend.clone())?);
        Self::from_sound_history(backend, sound_history, true)
    }

    /// Reuses the B5 sound bundle. The supplied history remains owned by the
    /// caller and is therefore not disposed when this source is disposed.
    pub fn with_sound_history(
        backend: Arc<dyn MinecraftBackendApi>,
        sound_history: Arc<SoundHistory>,
    ) -> Result<Self, mineintent_contracts::minecraft::BackendError> {
        Self::from_sound_history(backend, sound_history, false)
    }

    fn from_sound_history(
        backend: Arc<dyn MinecraftBackendApi>,
        sound_history: Arc<SoundHistory>,
        owns_sound_history: bool,
    ) -> Result<Self, mineintent_contracts::minecraft::BackendError> {
        let inner = Arc::new(ProductionFrameInner {
            backend: backend.clone(),
            sound_history,
            chat: Mutex::new(ChatState {
                disposed: false,
                scope: None,
                next_sequence: 1,
                history: VecDeque::with_capacity(CHAT_HISTORY_CAPACITY),
                pinned: BTreeMap::new(),
                omitted: 0,
            }),
        });
        let listener: Arc<dyn BackendEventListener> = Arc::new(ProductionChatListener {
            inner: Arc::downgrade(&inner),
        });
        let subscription = backend.subscribe(listener)?;
        Ok(Self {
            inner,
            chat_subscription: Mutex::new(Some(subscription)),
            owns_sound_history,
        })
    }

    pub fn dispose(&self) {
        {
            let mut state = lock(&self.inner.chat);
            if state.disposed {
                return;
            }
            state.disposed = true;
            state.scope = None;
            state.history.clear();
            state.pinned.clear();
            state.omitted = 0;
        }
        if let Some(mut subscription) = lock(&self.chat_subscription).take() {
            // 不得不 catch 的理由：`dispose` 由 `Drop for
            // ProductionParticipantFrameSource` 调用，因此 unwind 途中可达。
            // 那时 unsubscribe 再 panic 就是 double panic → `abort()`。语言约束。
            if let Err(_payload) = catch_unwind(AssertUnwindSafe(|| subscription.unsubscribe())) {
                tracing::error!(
                    target: "mineintent_middle",
                    "退订聊天事件时 panic：已隔离以避免 double panic 中止进程；这是缺陷"
                );
            }
        }
        if self.owns_sound_history {
            self.inner.sound_history.dispose();
        }
    }

    pub fn is_disposed(&self) -> bool {
        lock(&self.inner.chat).disposed
    }
}

impl Drop for ProductionParticipantFrameSource {
    fn drop(&mut self) {
        self.dispose();
    }
}

impl ParticipantFrameSource for ProductionParticipantFrameSource {
    fn chat_context(
        &self,
        scope: &ParticipantScope,
    ) -> Result<ChatInputContext, ParticipantSourceError> {
        self.inner.ensure_live()?;
        let snapshot = self
            .inner
            .backend
            .snapshot()
            .map_err(|error| ParticipantSourceError::Failed(error.to_string()))?;
        validate_snapshot_scope(&snapshot, scope)?;
        // Do not let a stale run mutate the bounded chat owner. Scope binding
        // occurs only after the backend has proved this is the current scope.
        self.inner.bind_scope(scope)?;
        if snapshot.self_snapshot.username.is_empty() {
            return Err(ParticipantSourceError::Invalid(
                "backend participant username is empty".to_owned(),
            ));
        }
        let online_player_usernames = snapshot
            .tracked_players
            .iter()
            .filter(|player| player.listed)
            .map(|player| {
                if player.username.is_empty() {
                    Err(ParticipantSourceError::Invalid(
                        "listed tracked-player username is empty".to_owned(),
                    ))
                } else {
                    Ok(player.username.clone())
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ChatInputContext {
            participant_username: snapshot.self_snapshot.username,
            online_player_usernames,
            conversation_active_with: None,
        })
    }

    fn capture(
        &self,
        scope: &ParticipantScope,
    ) -> Result<ParticipantFrameCapture, ParticipantSourceError> {
        self.inner.ensure_live()?;
        // This is the sole frame-facts read. All self pose/status/inventory,
        // timestamp, armor, and light fields below derive from this DTO.
        let facts = self
            .inner
            .backend
            .capture_frame_facts()
            .map_err(|error| ParticipantSourceError::Failed(error.to_string()))?;
        validate_snapshot_scope(&facts.snapshot, scope)?;
        // A stale observation must fail without clearing chat/pins belonging
        // to the backend's current scope.
        self.inner.bind_scope(scope)?;
        let (unread_chat, unread_chat_omitted) = self.inner.capture_chat(scope)?;
        frame_capture_from_facts(
            &facts,
            unread_chat,
            unread_chat_omitted,
            self.inner.sound_history.as_ref(),
        )
    }

    fn retain_trigger(
        &self,
        scope: &ParticipantScope,
        trigger: &PlayerChatMessage,
    ) -> Result<(), ParticipantSourceError> {
        self.inner.retain_trigger(scope, trigger)
    }

    fn release_trigger(&self, scope: &ParticipantScope, trigger: &PlayerChatMessage) {
        self.inner.release_trigger(scope, trigger);
    }

    fn release_retained_triggers(&self) {
        self.inner.release_all_triggers();
    }
}

fn validate_scope(scope: &ParticipantScope) -> Result<(), ParticipantSourceError> {
    if scope.process_session_id.is_empty()
        || scope.world_id.is_empty()
        || scope.dimension.as_deref().is_none_or(str::is_empty)
    {
        return Err(ParticipantSourceError::Invalid(
            "participant frame scope is incomplete".to_owned(),
        ));
    }
    Ok(())
}

fn validate_snapshot_scope(
    snapshot: &mineintent_contracts::minecraft::MinecraftSnapshotV1,
    scope: &ParticipantScope,
) -> Result<(), ParticipantSourceError> {
    validate_scope(scope)?;
    if snapshot.process_session_id != scope.process_session_id
        || snapshot.connection_epoch != scope.connection_epoch
        || snapshot.world.world_id != scope.world_id
        || Some(snapshot.world.dimension.as_str()) != scope.dimension.as_deref()
    {
        return Err(ParticipantSourceError::StaleScope(
            "backend frame snapshot does not match participant scope".to_owned(),
        ));
    }
    if snapshot.captured_at.is_empty() || snapshot.connection_attempt_id.is_empty() {
        return Err(ParticipantSourceError::Invalid(
            "backend frame snapshot is missing timestamp or connection identity".to_owned(),
        ));
    }
    Ok(())
}

fn validate_trigger_scope(
    scope: &ParticipantScope,
    trigger: &PlayerChatMessage,
) -> Result<(), ParticipantSourceError> {
    validate_scope(scope)?;
    if trigger.source_event_id.is_empty()
        || trigger.sender.username.is_empty()
        || trigger.world.world_id != scope.world_id
        || trigger.world.connection_epoch != scope.connection_epoch
        || trigger.world.dimension.as_deref() != scope.dimension.as_deref()
    {
        return Err(ParticipantSourceError::StaleScope(
            "addressed trigger does not match participant scope".to_owned(),
        ));
    }
    let message = AgentChatMessageV5 {
        username: trigger.sender.username.clone(),
        text: trigger.text.clone(),
        at: trigger.occurred_at.clone(),
    };
    message.validate().map_err(ParticipantSourceError::Invalid)
}

fn frame_capture_from_facts(
    facts: &MinecraftFrameFacts,
    unread_chat: Vec<crate::agent::AgentChatInputV5>,
    unread_chat_omitted: u64,
    sound_history: &SoundHistory,
) -> Result<ParticipantFrameCapture, ParticipantSourceError> {
    let snapshot = &facts.snapshot;
    let self_snapshot = &snapshot.self_snapshot;
    let yaw_degrees = self_snapshot.yaw.to_degrees();
    let pitch_degrees = self_snapshot.pitch.to_degrees();
    if !self_snapshot.position.x.is_finite()
        || !self_snapshot.position.y.is_finite()
        || !self_snapshot.position.z.is_finite()
        || !yaw_degrees.is_finite()
        || !pitch_degrees.is_finite()
        || !self_snapshot.health.is_finite()
        || !self_snapshot.food.is_finite()
    {
        return Err(ParticipantSourceError::Invalid(
            "backend frame facts contain a non-finite numeric value".to_owned(),
        ));
    }
    let armor = match facts.armor {
        None | Some(0) => None,
        Some(value @ 1..=20) => Some(value),
        Some(value) => {
            return Err(ParticipantSourceError::Invalid(format!(
                "backend armor {value} is outside 0..=20"
            )))
        }
    };
    let status = AgentStatusV5 {
        health: self_snapshot.health,
        food: self_snapshot.food,
        armor,
    };
    status.validate().map_err(ParticipantSourceError::Invalid)?;

    let hotbar = map_hotbar(&snapshot.inventory)?;
    let light = match facts.light {
        None => None,
        Some(value @ 0..=15) => Some(value),
        Some(value) => {
            return Err(ParticipantSourceError::Invalid(format!(
                "backend light {value} is outside 0..=15"
            )))
        }
    };
    let recent_sounds = sound_history
        .recent_for_scope(
            &snapshot.process_session_id,
            snapshot.connection_epoch,
            &snapshot.world.world_id,
            Some(snapshot.world.dimension.as_str()),
            RECENT_SOUND_LIMIT,
        )
        .into_iter()
        .map(|sound| ContractSoundObservation {
            sound_name: sound.sound_name,
            category: sound.category,
            distance: sound.distance,
            direction: sound.direction,
            volume: sound.volume,
            pitch: sound.pitch,
            observed_at: sound.observed_at,
        })
        .collect::<Vec<_>>();
    let sound = (!recent_sounds.is_empty()).then_some(SoundValues {
        recent_sounds: Some(recent_sounds),
    });
    Ok(ParticipantFrameCapture {
        at: snapshot.captured_at.clone(),
        dimension: snapshot.world.dimension.clone(),
        pose: AgentPoseV5 {
            position: [
                self_snapshot.position.x,
                self_snapshot.position.y,
                self_snapshot.position.z,
            ],
            yaw_degrees,
            pitch_degrees,
        },
        status: Some(status),
        hotbar,
        unread_chat,
        unread_chat_omitted,
        sound,
        light,
        events: Vec::new(),
        omissions: Vec::new(),
    })
}

fn map_hotbar(
    inventory: &mineintent_contracts::minecraft::InventorySnapshot,
) -> Result<AgentHotbarV5, ParticipantSourceError> {
    if inventory.selected_hotbar_slot > 8 {
        return Err(ParticipantSourceError::Invalid(
            "backend selected hotbar slot is outside 0..=8".to_owned(),
        ));
    }
    let mut slots = BTreeMap::new();
    let mut off_hand = None;
    for slot in &inventory.slots {
        match slot.slot {
            // The atomic snapshot is a full Player inventory. Main-inventory
            // slots are intentionally not part of the v5 hotbar projection.
            0..=35 => continue,
            36..=44 => {
                let wire_slot = (slot.slot - 36) as u8;
                if slots.contains_key(&wire_slot) {
                    return Err(ParticipantSourceError::Invalid(
                        "duplicate backend hotbar slot".to_owned(),
                    ));
                }
                let item = AgentItemStackV5::new(slot.item_name.clone(), slot.count)
                    .map_err(ParticipantSourceError::Invalid)?;
                slots.insert(wire_slot, item);
            }
            45 => {
                if off_hand.is_some() {
                    return Err(ParticipantSourceError::Invalid(
                        "duplicate backend off-hand slot".to_owned(),
                    ));
                }
                off_hand = Some(
                    AgentItemStackV5::new(slot.item_name.clone(), slot.count)
                        .map_err(ParticipantSourceError::Invalid)?,
                );
            }
            value => {
                return Err(ParticipantSourceError::Invalid(format!(
                    "backend inventory slot {value} is outside Player menu 0..=45"
                )))
            }
        }
    }
    Ok(AgentHotbarV5 {
        selected: inventory.selected_hotbar_slot,
        slots,
        off_hand,
    })
}

/// Body-only observation source for one exact model wake. It uses a weak fact
/// owner, so a dropped/stopped runtime cannot be retained by a registry or
/// dispatcher assembled for an in-flight run.
pub struct ParticipantObservationAfterSource {
    frame_source: Arc<dyn ParticipantFrameSource>,
    fact_owner: Weak<ParticipantFactOwner>,
    scope: ParticipantScope,
    generation: u64,
    trigger_event_id: String,
}

impl ParticipantObservationAfterSource {
    pub fn new(
        frame_source: Arc<dyn ParticipantFrameSource>,
        fact_owner: Weak<ParticipantFactOwner>,
        scope: ParticipantScope,
        generation: u64,
        trigger_event_id: impl Into<String>,
    ) -> Self {
        Self {
            frame_source,
            fact_owner,
            scope,
            generation,
            trigger_event_id: trigger_event_id.into(),
        }
    }
}

impl ObservationAfterSource for ParticipantObservationAfterSource {
    fn observe_after<'a>(
        &'a self,
        _invocation: CapabilityInvocation,
        resource: ExecutionResource,
        _result: serde_json::Value,
        context: CapabilityExecutionContext<'a>,
    ) -> ContractFuture<'a, Result<Option<JsonObject>, AgentError>> {
        Box::pin(async move {
            if resource != ExecutionResource::Body {
                context.check_at(std::time::Instant::now())?;
                return Ok(None);
            }
            context.check_at(std::time::Instant::now())?;
            if context.world_id() != self.scope.world_id
                || context.chat_event_id() != self.trigger_event_id
            {
                return Err(AgentError::new(
                    AgentErrorCode::ScopeInvalid,
                    "participant_observation_scope_mismatch",
                ));
            }
            let capture = self
                .frame_source
                .capture(&self.scope)
                .map_err(source_error)?;
            context.check_at(std::time::Instant::now())?;
            let owner = self.fact_owner.upgrade().ok_or_else(|| {
                AgentError::new(
                    AgentErrorCode::ScopeInvalid,
                    "participant_fact_owner_dropped",
                )
            })?;
            let batch = owner.drain(&self.scope, self.generation).ok_or_else(|| {
                AgentError::new(
                    AgentErrorCode::ScopeInvalid,
                    "participant_observation_scope_stale",
                )
            })?;
            let frame =
                assemble_direct_frame(capture, batch.facts, batch.omitted, batch.omitted_types)
                    .map_err(|error| {
                        AgentError::new(AgentErrorCode::ToolFailed, error.to_string())
                    })?;
            context.check_at(std::time::Instant::now())?;
            let value = serde_json::to_value(frame).map_err(|error| {
                AgentError::new(
                    AgentErrorCode::ToolFailed,
                    format!("participant observation serialization failed: {error}"),
                )
            })?;
            Ok(value.as_object().cloned())
        })
    }
}

fn source_error(error: ParticipantSourceError) -> AgentError {
    match error {
        ParticipantSourceError::StaleScope(summary) => {
            AgentError::new(AgentErrorCode::ScopeInvalid, summary)
        }
        other => AgentError::new(AgentErrorCode::ToolFailed, other.to_string()),
    }
}

fn assemble_direct_frame(
    mut capture: ParticipantFrameCapture,
    facts: Vec<crate::participant::runtime::ParticipantFact>,
    omitted: u64,
    omitted_types: Vec<String>,
) -> Result<AgentFrameV5, crate::agent::AgentContextV5AssemblyError> {
    // 与 runtime 的装配同一纪律：光照读不到就缺席，不让整帧失败。
    // 这条路径供轮末视野帧使用，它同样会在死亡期间被取用。
    if let Some(status) = capture.status.as_mut() {
        if status.armor == Some(0) {
            status.armor = None;
        }
    }
    let mut events = capture.events;
    for fact in facts {
        events.push(AgentContextV5EventInput::Summary {
            event_type: safe_fact_event_type(&fact.event_type),
            summary: fact.summary,
        });
    }
    if omitted > 0 {
        let types = if omitted_types.is_empty() {
            String::new()
        } else {
            format!("; types={}", omitted_types.join(","))
        };
        events.push(AgentContextV5EventInput::Summary {
            event_type: "participant_events_omitted".to_owned(),
            summary: format!("{} pending events omitted{}", omitted, types),
        });
    }
    AgentContextV5Assembler
        .assemble(AgentContextV5Input {
            memory: String::new(),
            at: capture.at,
            dimension: capture.dimension,
            pose: capture.pose,
            status: capture.status,
            hotbar: capture.hotbar,
            unread_chat: capture.unread_chat,
            unread_chat_omitted: capture.unread_chat_omitted,
            sound: capture.sound,
            light: capture.light,
            events,
            omissions: capture.omissions,
            trigger_chat: None,
        })
        .map(|context| context.frame)
}
