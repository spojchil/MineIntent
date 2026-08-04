//! Runtime-owned mutable state and the narrow primitives shared by its reducers.

use super::*;

pub(super) struct EventWriter {
    pub(super) next_id: u64,
    pub(super) process_session_id: String,
    pub(super) connection_epoch: u64,
    pub(super) connection_attempt_id: String,
    pub(super) world_id: String,
    pub(super) dimension: Option<String>,
}

impl EventWriter {
    pub(super) fn new(world_id: &str) -> Self {
        Self {
            next_id: 0,
            process_session_id: format!(
                "pid-{}-{}",
                std::process::id(),
                now_utc().timestamp_millis()
            ),
            connection_epoch: 0,
            connection_attempt_id: "attempt-0".to_owned(),
            world_id: world_id.to_owned(),
            dimension: None,
        }
    }

    pub(super) fn new_attempt(&mut self, epoch: u64) {
        self.connection_epoch = epoch;
        self.connection_attempt_id = format!("attempt-{}", epoch);
        self.dimension = None;
    }

    pub(super) fn set_dimension(&mut self, dimension: impl Into<String>) {
        self.dimension = Some(dimension.into());
    }

    pub(super) fn context(&self) -> (String, u64, String) {
        (
            self.process_session_id.clone(),
            self.connection_epoch,
            self.connection_attempt_id.clone(),
        )
    }

    pub(super) fn emit(
        &mut self,
        source: FactSource,
        payload: BackendEventPayload,
    ) -> BackendEventEnvelope {
        self.emit_at(source, payload, now_utc().to_rfc3339())
    }

    pub(super) fn emit_at(
        &mut self,
        source: FactSource,
        payload: BackendEventPayload,
        occurred_at: String,
    ) -> BackendEventEnvelope {
        self.next_id += 1;
        BackendEventEnvelope::from_payload(
            mineintent_contracts::minecraft::BackendEventMetadata {
                id: format!("event-{}", self.next_id),
                occurred_at,
                process_session_id: self.process_session_id.clone(),
                connection_epoch: self.connection_epoch,
                connection_attempt_id: self.connection_attempt_id.clone(),
                world_id: self.world_id.clone(),
                dimension: self.dimension.clone(),
            },
            source,
            payload,
        )
    }
}

pub(super) type SharedWorld = Arc<parking_lot::RwLock<azalea::world::World>>;

pub(super) struct ObservationState {
    pub(super) world: Option<SharedWorld>,
    pub(super) snapshot: Option<MinecraftSnapshotV1>,
    /// The producer scope that authored `snapshot`.  The public snapshot wire
    /// intentionally has no scope field; this private stamp prevents frame
    /// facts from being combined with a snapshot captured before Respawn.
    pub(super) snapshot_scope_generation: u64,
    pub(super) source: Option<FactSource>,
    pub(super) tracked_entities: Vec<ProtocolEntitySnapshot>,
    /// Packet fields Azalea does not expose in the ECS capture, or a packet
    /// velocity which its handler intentionally leaves untouched.  This is a
    /// live-entity residual, not an event queue: it is cleared on scope/world
    /// reset and removed with the tracked entity.
    pub(super) entity_residuals: Vec<EntityObservationResidual>,
    /// Armor is a connection fact.  It deliberately survives same-epoch
    /// Login/Respawn scope resets, but the epoch stamp makes a new connection
    /// automatically unavailable.
    pub(super) armor: Option<u8>,
    pub(super) armor_epoch: Option<u64>,
    pub(super) light_cache: LightCache,
    pub(super) generation: u64,
}

/// The packet fields that are not necessarily represented by the ECS capture
/// have an explicit authority transition.  In particular, a new Spawn or a
/// Teleport starts a new velocity authority; it must not inherit a residual
/// from the previous incarnation of the same protocol id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EntityResidualAction {
    Retain,
    Update,
    Clear,
}

pub(super) const ENTITY_OBSERVATION_RESIDUAL_CAPACITY: usize = 1024;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct EntityObservationResidual {
    pub(super) entity_key: String,
    pub(super) head_yaw: Option<f32>,
    pub(super) velocity: Option<[f64; 3]>,
}

impl Default for ObservationState {
    fn default() -> Self {
        Self {
            world: None,
            snapshot: None,
            snapshot_scope_generation: 0,
            source: None,
            tracked_entities: Vec::new(),
            entity_residuals: Vec::new(),
            armor: None,
            armor_epoch: None,
            light_cache: LightCache::default(),
            generation: 0,
        }
    }
}

impl ObservationState {
    pub(super) fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    pub(super) fn clear_all_frame_facts(&mut self) {
        self.armor = None;
        self.armor_epoch = None;
        self.light_cache.clear();
    }

    pub(super) fn clear_light_for_scope(
        &mut self,
        epoch: u64,
        scope_generation: u64,
        dimension: Option<String>,
        has_skylight: Option<bool>,
    ) {
        self.light_cache
            .reset_scope(epoch, scope_generation, dimension, has_skylight);
    }
}

pub(super) enum ActiveMovementRegistration {
    Started { cancel_signal: Option<Arc<Notify>> },
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AttemptAdmissionState {
    NotStarted,
    Reserved {
        epoch: u64,
        reconnect_token: Option<u64>,
        join_started_epoch: Option<u64>,
        /// The vendor `AttemptToken` admitted by the canonical
        /// `StartJoinServerEvent`, if the attempt was stamped. `None` is the
        /// legacy/unstamped path.
        attempt_token: Option<azalea::join::AttemptToken>,
    },
    Bound {
        epoch: u64,
        entity: bevy_ecs::entity::Entity,
        reconnect_token: Option<u64>,
        attempt_token: Option<azalea::join::AttemptToken>,
    },
    Closed {
        epoch: u64,
    },
}

impl Default for AttemptAdmissionState {
    fn default() -> Self {
        Self::NotStarted
    }
}

/// A backend-only fence for sources that Azalea does not stamp with a
/// connection identity.  `Entity` is reusable across reconnects, so after a
/// same-entity handoff there is no sound predicate that can distinguish a
/// delayed A message from a new B message.  The conservative state is to
/// reject all unstamped source messages after that point.  A stamped
/// reconnect-return token may still install the owner, but it does not clear
/// this fence; clearing it would falsely claim provenance that the vendor
/// event does not carry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct EntitySourceFence {
    pub(super) last_bound_entity: Option<bevy_ecs::entity::Entity>,
    pub(super) pending_rebind_entity: Option<bevy_ecs::entity::Entity>,
    pub(super) ambiguous: bool,
}

impl EntitySourceFence {
    pub(super) fn begin_attempt(&mut self) {
        self.pending_rebind_entity = self.last_bound_entity;
    }

    pub(super) fn allows_unstamped(&self, entity: bevy_ecs::entity::Entity) -> bool {
        !self.ambiguous && self.pending_rebind_entity != Some(entity)
    }

    pub(super) fn allows_unstamped_global(&self) -> bool {
        !self.ambiguous && self.pending_rebind_entity.is_none()
    }

    pub(super) fn bind(&mut self, entity: bevy_ecs::entity::Entity) {
        if self.last_bound_entity == Some(entity) {
            self.ambiguous = true;
        }
        self.last_bound_entity = Some(entity);
        self.pending_rebind_entity = None;
    }
}

/// The short-lived identity captured when a canonical Azalea source is read.
/// Publication rechecks all three pieces under `command_admission`; an event
/// cannot be stamped with a later owner, epoch, or scope after a handoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CanonicalSourceAdmission {
    pub(super) entity: bevy_ecs::entity::Entity,
    pub(super) epoch: u64,
    pub(super) scope_generation: u64,
    /// The vendor attempt token captured at the canonical source, if the
    /// source was stamped. `None` only for the legacy/unstamped fallback.
    pub(super) attempt_token: Option<azalea::join::AttemptToken>,
}

/// One-to-one vendor `AttemptToken` ↔ backend epoch bindings for the whole
/// `RuntimeSession`.
///
/// Growth boundary: every successfully stamped join attempt adds at most one
/// pair of entries, and entries are intentionally never removed so that a
/// historical token can never be re-registered on a later epoch. The size is
/// therefore bounded by the number of stamped join attempts in the session.
#[derive(Default)]
pub(super) struct SourceTokenBindings {
    pub(super) token_to_epoch: std::collections::HashMap<azalea::join::AttemptToken, u64>,
    pub(super) epoch_to_token: std::collections::HashMap<u64, azalea::join::AttemptToken>,
}

impl SourceTokenBindings {
    /// Register `token` for `epoch` exactly once. Returns `false` (without
    /// mutating anything) when the token is already bound to a different
    /// epoch or the epoch is already bound to a different token; the same
    /// pair is idempotent.
    pub(super) fn bind(&mut self, token: azalea::join::AttemptToken, epoch: u64) -> bool {
        if let Some(bound_epoch) = self.token_to_epoch.get(&token) {
            return *bound_epoch == epoch;
        }
        if let Some(bound_token) = self.epoch_to_token.get(&epoch) {
            return *bound_token == token;
        }
        self.token_to_epoch.insert(token, epoch);
        self.epoch_to_token.insert(epoch, token);
        true
    }

    pub(super) fn matches(&self, token: azalea::join::AttemptToken, epoch: u64) -> bool {
        self.token_to_epoch.get(&token) == Some(&epoch)
    }
}

#[derive(Default)]
pub(super) struct EntityProducerRuntimeState {
    pub(super) owner: Option<(bevy_ecs::entity::Entity, u64)>,
    pub(super) scope_generation: u64,
    pub(super) attempt: AttemptAdmissionState,
    /// A bounded hand-off from the canonical ECS ConnectionFailed source to
    /// Azalea's high-level event handler.  It only exists for the current
    /// pre-Init attempt and is cleared on every attempt transition.
    pub(super) pending_connection_failure: Option<(bevy_ecs::entity::Entity, u64)>,
    pub(super) source_fence: EntitySourceFence,
    pub(super) source_token_bindings: SourceTokenBindings,
    pub(super) cache: EntityProducerCache,
}

impl EntityProducerRuntimeState {
    pub(super) fn reset_scope(&mut self, epoch: u64) {
        self.scope_generation = self.scope_generation.wrapping_add(1);
        self.cache.reset_scope(epoch);
    }

    pub(super) fn deactivate_scope(&mut self) {
        self.scope_generation = self.scope_generation.wrapping_add(1);
        self.cache.deactivate_scope();
    }
}

/// NEW-15：一次 pre-Init 连接尝试的完整身份。epoch/ordinal 把 (entity, token)
/// 绑到登记时刻的世代，Connecting 超时只取走仍匹配的身份，陈旧登记不伤新尝试。
#[derive(Clone, Copy, Debug)]
pub(super) struct PendingConnectionAttempt {
    pub(super) entity: azalea::ecs::entity::Entity,
    pub(super) attempt_token: azalea::join::AttemptToken,
    pub(super) epoch: u64,
    pub(super) ordinal: u64,
}

pub(super) struct SharedRuntime {
    pub(super) writer: parking_lot::Mutex<EventWriter>,
    pub(super) event_dispatch: parking_lot::Mutex<EventDispatchState>,
    pub(super) event_dispatch_wake: parking_lot::Condvar,
    pub(super) swarm: parking_lot::Mutex<Option<Swarm>>,
    /// NEW-15：当前 pre-Init 连接尝试身份；ECS 捕获系统在任务出现时登记。
    pub(super) pending_connection: parking_lot::Mutex<Option<PendingConnectionAttempt>>,
    /// NEW-15：待投递的 CreateConnectionTask 取消请求；由泵系统在 Azalea
    /// schedule 内转成 CancelConnectionTaskEvent，避免跨任务直写消息的竞争。
    pub(super) connection_cancels:
        parking_lot::Mutex<Vec<(azalea::ecs::entity::Entity, azalea::join::AttemptToken)>>,
    /// NEW-15：pre-Init 超时自主重连所需账号，run 启动时登记一次。
    pub(super) rejoin_account: parking_lot::Mutex<Option<azalea::account::Account>>,
    pub(super) shutdown: Arc<Notify>,
    pub(super) reconnect_cancel: Arc<Notify>,
    pub(super) shutdown_requested: AtomicBool,
    pub(super) stop_requested: AtomicBool,
    pub(super) dispatch_cancelled: AtomicBool,
    pub(super) config: RunConfig,
    pub(super) commands: parking_lot::Mutex<VecDeque<QueuedCommand>>,
    pub(super) subscribers: parking_lot::Mutex<Vec<Arc<RuntimeEventQueue>>>,
    pub(super) observation_subscribers: parking_lot::Mutex<Vec<ObservationSubscriber>>,
    pub(super) entity_producer: parking_lot::Mutex<EntityProducerRuntimeState>,
    pub(super) entity_packet_admission: AtomicU64,
    pub(super) sound_packet_sequence: AtomicU64,
    pub(super) next_observation_subscription_id: AtomicU64,
    pub(super) observation: parking_lot::RwLock<ObservationState>,
    /// Authoritative runtime lifecycle state.  The facade reads this value;
    /// it does not reconstruct a second lifecycle machine from callbacks.
    pub(super) backend_state: parking_lot::RwLock<BackendState>,
    pub(super) reported_dimension: parking_lot::Mutex<Option<String>>,
    pub(super) snapshot_revision: AtomicU64,
    pub(super) viewport_revision: AtomicU64,
    pub(super) lifecycle_revision: AtomicU64,
    pub(super) command_revision: AtomicU64,
    pub(super) tick_revision: AtomicU64,
    pub(super) movement_generation: AtomicU64,
    /// Serializes command admission with stop/disconnect marking.  The lock
    /// is deliberately held only while changing admission state; actuator
    /// calls and completion callbacks never run under it.
    pub(super) command_admission: parking_lot::Mutex<()>,
    pub(super) active_movement: AtomicBool,
    pub(super) active_movement_id: parking_lot::Mutex<Option<String>>,
    pub(super) active_movement_cancel_signal: parking_lot::Mutex<Option<Arc<Notify>>>,
    pub(super) active_movement_completion: parking_lot::Mutex<Option<Arc<CommandCompletionState>>>,
    /// A Move can be between its active declaration and its first actuator
    /// call.  Stop must wait for that registration window to close before it
    /// emits stopped/shuts down.
    pub(super) active_movement_registration: AtomicBool,
    pub(super) timer_started: AtomicBool,
    pub(super) initial_chat_sent: AtomicBool,
    pub(super) death_reported: AtomicBool,
    pub(super) disconnect_reported: AtomicBool,
    pub(super) stopped_reported: AtomicBool,
    pub(super) faulted_reported: AtomicBool,
    pub(super) last_close: parking_lot::Mutex<Option<BackendClose>>,
    pub(super) last_failure: parking_lot::Mutex<Option<BackendFailure>>,
    pub(super) stop_reason: parking_lot::Mutex<Option<String>>,
    pub(super) reconnect_pending: AtomicBool,
    pub(super) reconnect_add_pending: AtomicBool,
    pub(super) reconnect_attempt_token: AtomicU64,
    /// Retry ordinal is independent from the never-reset connection epoch.
    pub(super) retry_ordinal: AtomicU64,
    pub(super) reconnect_rng: AtomicU64,
    pub(super) phase_generation: AtomicU64,
    pub(super) phase_cancel: Arc<Notify>,
    pub(super) stable_generation: AtomicU64,
    pub(super) stable_cancel: Arc<Notify>,
    pub(super) stop_watchdog_generation: AtomicU64,
    pub(super) stop_watchdog_cancel: Arc<Notify>,
    pub(super) active_client: parking_lot::Mutex<Option<Client>>,
    pub(super) timers_enabled: AtomicBool,
    pub(super) ready: AtomicBool,
    pub(super) stopping: AtomicBool,
    #[cfg(test)]
    pub(super) active_movement_registration_hook:
        parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(super) event_admission_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(super) finalize_stop_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(super) event_broadcast_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(super) event_dispatch_backpressure_hook:
        parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(super) stop_signal_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(super) stop_watchdog_completion_probe: parking_lot::Mutex<Option<oneshot::Sender<()>>>,
    #[cfg(test)]
    pub(super) runtime_broker_backpressure_hook:
        parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(super) disconnect_cleanup_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(super) entity_publish_after_apply_hook:
        parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(super) entity_owner_bind_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(super) observation_write_boundary_hook:
        parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub(super) canonical_publication_probe: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl SharedRuntime {
    pub(super) fn new(config: RunConfig) -> Self {
        Self {
            writer: parking_lot::Mutex::new(EventWriter::new(&config.world_id)),
            event_dispatch: parking_lot::Mutex::new(EventDispatchState::default()),
            event_dispatch_wake: parking_lot::Condvar::new(),
            swarm: parking_lot::Mutex::new(None),
            pending_connection: parking_lot::Mutex::new(None),
            connection_cancels: parking_lot::Mutex::new(Vec::new()),
            rejoin_account: parking_lot::Mutex::new(None),
            shutdown: Arc::new(Notify::new()),
            reconnect_cancel: Arc::new(Notify::new()),
            shutdown_requested: AtomicBool::new(false),
            stop_requested: AtomicBool::new(false),
            dispatch_cancelled: AtomicBool::new(false),
            config,
            commands: parking_lot::Mutex::new(VecDeque::new()),
            subscribers: parking_lot::Mutex::new(Vec::new()),
            observation_subscribers: parking_lot::Mutex::new(Vec::new()),
            entity_producer: parking_lot::Mutex::new(EntityProducerRuntimeState::default()),
            entity_packet_admission: AtomicU64::new(0),
            sound_packet_sequence: AtomicU64::new(0),
            next_observation_subscription_id: AtomicU64::new(0),
            observation: parking_lot::RwLock::new(ObservationState::default()),
            backend_state: parking_lot::RwLock::new(BackendState::Idle),
            reported_dimension: parking_lot::Mutex::new(None),
            snapshot_revision: AtomicU64::new(0),
            viewport_revision: AtomicU64::new(0),
            lifecycle_revision: AtomicU64::new(0),
            command_revision: AtomicU64::new(0),
            tick_revision: AtomicU64::new(0),
            movement_generation: AtomicU64::new(0),
            command_admission: parking_lot::Mutex::new(()),
            active_movement: AtomicBool::new(false),
            active_movement_id: parking_lot::Mutex::new(None),
            active_movement_cancel_signal: parking_lot::Mutex::new(None),
            active_movement_completion: parking_lot::Mutex::new(None),
            active_movement_registration: AtomicBool::new(false),
            timer_started: AtomicBool::new(false),
            initial_chat_sent: AtomicBool::new(false),
            death_reported: AtomicBool::new(false),
            disconnect_reported: AtomicBool::new(false),
            stopped_reported: AtomicBool::new(false),
            faulted_reported: AtomicBool::new(false),
            last_close: parking_lot::Mutex::new(None),
            last_failure: parking_lot::Mutex::new(None),
            stop_reason: parking_lot::Mutex::new(None),
            reconnect_pending: AtomicBool::new(false),
            reconnect_add_pending: AtomicBool::new(false),
            reconnect_attempt_token: AtomicU64::new(0),
            retry_ordinal: AtomicU64::new(0),
            reconnect_rng: AtomicU64::new(0x4d494e45494e5441),
            phase_generation: AtomicU64::new(0),
            phase_cancel: Arc::new(Notify::new()),
            stable_generation: AtomicU64::new(0),
            stable_cancel: Arc::new(Notify::new()),
            stop_watchdog_generation: AtomicU64::new(0),
            stop_watchdog_cancel: Arc::new(Notify::new()),
            active_client: parking_lot::Mutex::new(None),
            timers_enabled: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            #[cfg(test)]
            active_movement_registration_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            event_admission_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            finalize_stop_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            event_broadcast_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            event_dispatch_backpressure_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            stop_signal_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            stop_watchdog_completion_probe: parking_lot::Mutex::new(None),
            #[cfg(test)]
            runtime_broker_backpressure_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            disconnect_cleanup_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            entity_publish_after_apply_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            entity_owner_bind_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            observation_write_boundary_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            canonical_publication_probe: parking_lot::Mutex::new(None),
        }
    }

    pub(super) fn set_backend_state(&self, state: BackendState) {
        *self.backend_state.write() = state;
    }

    pub(super) fn backend_state(&self) -> BackendState {
        self.backend_state.read().clone()
    }

    /// Publish one canonical block/sound observation after applying the
    /// source fact.  Admission and writer envelope construction share the
    /// command lock, while draining and callbacks happen only after all
    /// world/producer locks have been released by the caller.
    pub(super) fn emit_canonical_observation_event(
        &self,
        source: CanonicalSourceAdmission,
        payload: BackendEventPayload,
    ) -> bool {
        // Test probe runs *outside* the command admission lock, so a
        // deterministic test can rebind the owner between source admission
        // and publication without deadlocking.
        #[cfg(test)]
        self.invoke_canonical_publication_probe();
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.canonical_source_still_valid_locked(source) {
                return false;
            }
            let Some(should_drain) = self.enqueue_entity_event_if_running_locked(
                source.epoch,
                FactSource::ServerObserved,
                payload,
            ) else {
                return false;
            };
            should_drain
        };
        if should_drain {
            self.drain_events();
        }
        true
    }

    pub(super) fn emit_canonical_sound(
        &self,
        source: CanonicalSourceAdmission,
        sound_name: String,
        source_position: [f64; 3],
        volume: f64,
        pitch: f64,
    ) -> bool {
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.canonical_source_still_valid_locked(source)
                || !self.command_execution_allowed_without_lock()
            {
                return false;
            }
            let sound_sequence = self.sound_packet_sequence.fetch_add(1, Ordering::AcqRel) + 1;
            let payload = BackendEventPayload::Sound(ContractProtocolSoundPayload {
                event_type: ContractHeardSoundType::Heard,
                sound_key: format!("sound-{}-{sound_sequence}", source.epoch),
                sound_name: Some(sound_name),
                sound_id: None,
                category: None,
                source_position: ContractVec3Value {
                    x: source_position[0],
                    y: source_position[1],
                    z: source_position[2],
                },
                volume,
                pitch,
                protocol_source: ContractProtocolSoundSource::NamedSoundEffect,
            });
            let Some(should_drain) = self.enqueue_entity_event_if_running_locked(
                source.epoch,
                FactSource::ServerObserved,
                payload,
            ) else {
                return false;
            };
            should_drain
        };
        if should_drain {
            self.drain_events();
        }
        true
    }

    pub(super) fn cancel_event_admission(&self) {
        self.dispatch_cancelled.store(true, Ordering::Release);
        self.event_dispatch_wake.notify_all();
        self.wake_runtime_subscribers();
    }

    pub(super) fn wake_runtime_subscribers(&self) {
        let subscribers = self.subscribers.lock().clone();
        for subscriber in subscribers {
            subscriber.wake_all();
        }
    }

    #[cfg(test)]
    pub(super) fn set_active_movement_registration_hook(
        &self,
        hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        *self.active_movement_registration_hook.lock() = hook;
    }

    #[cfg(test)]
    pub(super) fn invoke_active_movement_registration_hook(&self) {
        let hook = self.active_movement_registration_hook.lock().take();
        if let Some(hook) = hook {
            // Never invoke a test seam while holding its registry lock.  The
            // hook intentionally may call stop() re-entrantly.
            hook();
        }
    }

    #[cfg(test)]
    pub(super) fn set_event_admission_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.event_admission_hook.lock() = hook;
    }

    #[cfg(test)]
    pub(super) fn invoke_event_admission_hook(&self) {
        let hook = self.event_admission_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    pub(super) fn set_finalize_stop_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.finalize_stop_hook.lock() = hook;
    }

    #[cfg(test)]
    pub(super) fn invoke_finalize_stop_hook(&self) {
        let hook = self.finalize_stop_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    pub(super) fn set_event_broadcast_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.event_broadcast_hook.lock() = hook;
    }

    #[cfg(test)]
    pub(super) fn invoke_event_broadcast_hook(&self) {
        let hook = self.event_broadcast_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    pub(super) fn set_event_dispatch_backpressure_hook(
        &self,
        hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        *self.event_dispatch_backpressure_hook.lock() = hook;
    }

    #[cfg(test)]
    pub(super) fn set_stop_signal_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.stop_signal_hook.lock() = hook;
    }

    #[cfg(test)]
    pub(super) fn invoke_stop_signal_hook(&self) {
        let hook = self.stop_signal_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    pub(super) fn set_runtime_broker_backpressure_hook(
        &self,
        hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        *self.runtime_broker_backpressure_hook.lock() = hook;
    }

    #[cfg(test)]
    pub(super) fn event_dispatch_counts(&self) -> (usize, usize, usize, usize) {
        self.event_dispatch.lock().queued_counts()
    }

    #[cfg(test)]
    pub(super) fn set_disconnect_cleanup_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.disconnect_cleanup_hook.lock() = hook;
    }

    #[cfg(test)]
    pub(super) fn invoke_disconnect_cleanup_hook(&self) {
        let hook = self.disconnect_cleanup_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    pub(super) fn set_entity_publish_after_apply_hook(
        &self,
        hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        *self.entity_publish_after_apply_hook.lock() = hook;
    }

    #[cfg(test)]
    pub(super) fn invoke_entity_publish_after_apply_hook(&self) {
        let hook = self.entity_publish_after_apply_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    pub(super) fn set_entity_owner_bind_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.entity_owner_bind_hook.lock() = hook;
    }

    #[cfg(test)]
    pub(super) fn invoke_entity_owner_bind_hook(&self) {
        let hook = self.entity_owner_bind_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    pub(super) fn set_canonical_publication_probe(
        &self,
        probe: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        *self.canonical_publication_probe.lock() = probe;
    }

    #[cfg(test)]
    pub(super) fn invoke_canonical_publication_probe(&self) {
        let probe = self.canonical_publication_probe.lock().take();
        if let Some(probe) = probe {
            probe();
        }
    }

    #[cfg(test)]
    pub(super) fn set_observation_write_boundary_hook(
        &self,
        hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        *self.observation_write_boundary_hook.lock() = hook;
    }

    #[cfg(test)]
    pub(super) fn invoke_observation_write_boundary_hook(&self) {
        let hook = self.observation_write_boundary_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }
}
