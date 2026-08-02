use std::{
    collections::VecDeque,
    error::Error,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use azalea::{
    accept_resource_packs::AcceptResourcePacksPlugin,
    app::{App, AppExit, Plugin, PluginGroup, PostUpdate, Update},
    auto_reconnect::AutoReconnectPlugin,
    auto_respawn::AutoRespawnPlugin,
    bot::DefaultBotPlugins,
    ecs::{
        message::{MessageReader, MessageWriter},
        prelude::{Commands, On, Query, With},
        system::Res,
    },
    entity::{Dead, LocalEntity, Physics, Position},
    prelude::{bevy_ecs, Account, Component, Resource},
    protocol::address::{ResolvedAddr, ServerAddr},
    swarm::{DefaultSwarmPlugins, Swarm, SwarmBuilder, SwarmEvent},
    Client, DefaultPlugins, Event, SprintDirection, WalkDirection,
};
use mineintent_contracts::capability::validate_directed_positions;
use mineintent_contracts::minecraft::{
    parse_block_property_value, BackendClose, BackendCloseError, BackendError,
    BackendEventEnvelope as ContractBackendEventEnvelope,
    BackendEventKind as ContractBackendEventKind,
    BackendEventMetadata as ContractBackendEventMetadata, BackendEventPayload, BackendFailure,
    BackendFailureCode, BackendKick, BackendLifecyclePayload, BackendState,
    BlockBoundingBox as ContractBlockBoundingBox, BlockPosition as ContractBlockPosition,
    BlockReadResult as ContractBlockReadResult, BoxFuture, ChatPosition, DirectedViewportError,
    DirectedViewportProjection, EntityEquipmentSnapshot as ContractEntityEquipmentSnapshot,
    FactSource as ContractFactSource, ObservationEvent, ObservationEventListener, OperationControl,
    ProtocolBlockEvent as ContractProtocolBlockEvent,
    ProtocolBlockSnapshot as ContractProtocolBlockSnapshot,
    ProtocolChatEvent as ContractProtocolChatEvent,
    ProtocolEntitySnapshot as ContractProtocolEntitySnapshot, ProtocolObservationSource,
    ProtocolPlayerListEvent as ContractProtocolPlayerListEvent,
    ProtocolSelfEvent as ContractProtocolSelfEvent,
    ProtocolSnapshotChangedEvent as ContractProtocolSnapshotChangedEvent, RelativeMovementFlags,
    SelfPose as ContractSelfPose, Subscription, Vec3Value as ContractVec3Value,
    ViewportBlock as ContractViewportBlock, ViewportFrame as ContractViewportFrame,
    ViewportLegend as ContractViewportLegend, ViewportProjection as ContractViewportProjection,
    ViewportRead as ContractViewportRead, ViewportSelfPose as ContractViewportSelfPose,
    VisibleBlocksView as ContractVisibleBlocksView,
    VisibleEntitiesView as ContractVisibleEntitiesView,
    VisibleEntityView as ContractVisibleEntityView,
};
use tokio::sync::{mpsc, oneshot, Notify};

use crate::{
    protocol::{
        now_utc, BackendCommand, BackendCommandEnvelope, BackendEventEnvelope, BackendEventKind,
        FactSource, MotorDirection, BACKEND_COMMAND_PROTOCOL,
    },
    snapshot::{
        block_snapshot, capture, capture_tracked_entities, BlockBoundingBox, BlockPosition,
        BlockReadResult, MinecraftSnapshotV1, PoseSnapshot, ProtocolBlockSnapshot,
        ProtocolEntitySnapshot, TrackedPlayerSnapshot, Vec3Value,
    },
    viewport::{
        project as project_viewport, project_directed as project_directed_viewport,
        project_with_checkpoint as project_viewport_with_checkpoint, ViewportBlock,
        ViewportOptions, ViewportProjection, WorldHeightBounds,
    },
};

#[derive(Clone, Debug)]
pub struct RunConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub world_id: String,
    pub duration: Duration,
    pub reconnect_delay: Duration,
    pub reconnect_enabled: bool,
    /// The diagnostic CLI uses a finite duration; a composition root owns the
    /// backend lifecycle and disables this timer for a production facade.
    pub auto_stop: bool,
    /// The diagnostic CLI exposes the event stream on stdout.  In-process
    /// composition roots consume the same events through `subscribe` and keep
    /// this boundary silent.
    pub emit_stdout: bool,
    /// 仅用于本地验收 M2；正式集成通过 `RuntimeHandle::send_chat`。
    pub initial_chat: Option<String>,
}

/// 服务端先发送死亡/生命值更新、再在同一 tick 设置 waitingForRespawn；显式
/// respawn 若紧贴 DeathEvent 发出会落在这段窗口里。这个延迟只作用于上层已经
/// 明确请求的重生，不会在死亡事件上自动创建请求。
const RESPAWN_SETTLE_DELAY: Duration = Duration::from_millis(100);

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: 25565,
            username: "MineIntentBot".to_owned(),
            world_id: "paper-local-world".to_owned(),
            duration: Duration::from_secs(30),
            reconnect_delay: Duration::from_secs(5),
            reconnect_enabled: true,
            auto_stop: true,
            emit_stdout: true,
            initial_chat: None,
        }
    }
}

struct EventWriter {
    next_id: u64,
    process_session_id: String,
    connection_epoch: u64,
    connection_attempt_id: String,
    world_id: String,
    dimension: Option<String>,
}

impl EventWriter {
    fn new(world_id: &str) -> Self {
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

    fn new_attempt(&mut self) {
        self.connection_epoch += 1;
        self.connection_attempt_id = format!("attempt-{}", self.connection_epoch);
        self.dimension = None;
    }

    fn set_dimension(&mut self, dimension: impl Into<String>) {
        self.dimension = Some(dimension.into());
    }

    fn context(&self) -> (String, u64, String) {
        (
            self.process_session_id.clone(),
            self.connection_epoch,
            self.connection_attempt_id.clone(),
        )
    }

    fn emit(&mut self, source: FactSource, payload: BackendEventPayload) -> BackendEventEnvelope {
        self.emit_at(source, payload, now_utc().to_rfc3339())
    }

    fn emit_at(
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

#[derive(Default)]
struct EventDispatchState {
    queue: VecDeque<BackendEventEnvelope>,
    drainer_active: bool,
}

impl EventDispatchState {
    /// Enqueue while the dispatch mutex is held and elect exactly one drainer.
    fn enqueue(&mut self, event: BackendEventEnvelope) -> bool {
        self.queue.push_back(event);
        if self.drainer_active {
            false
        } else {
            self.drainer_active = true;
            true
        }
    }
}

struct ObservationSubscriber {
    id: u64,
    epoch: u64,
    listener: Arc<dyn ObservationEventListener>,
    state: Arc<ObservationSubscriptionState>,
}

struct ObservationSubscriptionState {
    status: parking_lot::Mutex<ObservationSubscriptionStatus>,
    quiescent: parking_lot::Condvar,
}

#[derive(Default)]
struct ObservationSubscriptionStatus {
    closed: bool,
    pending_callbacks: usize,
    active_callbacks: usize,
}

impl ObservationSubscriptionState {
    fn new() -> Self {
        Self {
            status: parking_lot::Mutex::new(ObservationSubscriptionStatus::default()),
            quiescent: parking_lot::Condvar::new(),
        }
    }

    /// Reserve a callback while the registry lock is held. The reservation is
    /// later turned into an active callback outside that lock.
    fn reserve_callback(&self) -> bool {
        let mut status = self.status.lock();
        if status.closed {
            return false;
        }
        status.pending_callbacks += 1;
        true
    }

    fn start_callback(&self) -> bool {
        let mut status = self.status.lock();
        debug_assert!(status.pending_callbacks > 0);
        status.pending_callbacks = status.pending_callbacks.saturating_sub(1);
        if status.closed {
            self.quiescent.notify_all();
            return false;
        }
        status.active_callbacks += 1;
        true
    }

    fn finish_callback(&self) {
        let mut status = self.status.lock();
        debug_assert!(status.active_callbacks > 0);
        status.active_callbacks = status.active_callbacks.saturating_sub(1);
        if status.active_callbacks == 0 && status.pending_callbacks == 0 {
            self.quiescent.notify_all();
        }
    }

    fn close(&self) {
        self.status.lock().closed = true;
    }

    fn is_closed(&self) -> bool {
        self.status.lock().closed
    }

    fn wait_for_quiescence(&self) {
        let own_active_callbacks = current_observation_callback_count(self);
        let mut status = self.status.lock();
        // A pending reservation is deliberately not waited on: after `closed`
        // is set, `start_callback` consumes it and skips the listener. Waiting
        // for that reservation here would deadlock when listener A unsubscribes
        // listener B from the same dispatch pass before B starts.
        while status.active_callbacks > own_active_callbacks {
            self.quiescent.wait(&mut status);
        }
    }
}

struct ObservationDelivery {
    listener: Arc<dyn ObservationEventListener>,
    state: Arc<ObservationSubscriptionState>,
    id: u64,
}

thread_local! {
    static OBSERVATION_CALLBACK_STACK: std::cell::RefCell<Vec<usize>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn observation_state_key(state: &ObservationSubscriptionState) -> usize {
    state as *const ObservationSubscriptionState as usize
}

fn current_observation_callback_count(state: &ObservationSubscriptionState) -> usize {
    let key = observation_state_key(state);
    OBSERVATION_CALLBACK_STACK.with(|stack| {
        stack
            .borrow()
            .iter()
            .filter(|current| **current == key)
            .count()
    })
}

struct ObservationCallbackGuard {
    key: usize,
}

impl ObservationCallbackGuard {
    fn enter(state: &ObservationSubscriptionState) -> Self {
        let key = observation_state_key(state);
        OBSERVATION_CALLBACK_STACK.with(|stack| stack.borrow_mut().push(key));
        Self { key }
    }
}

impl Drop for ObservationCallbackGuard {
    fn drop(&mut self) {
        OBSERVATION_CALLBACK_STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            debug_assert_eq!(stack.pop(), Some(self.key));
        });
    }
}

type SharedWorld = Arc<parking_lot::RwLock<azalea::world::World>>;

/// The observation values used by one viewport capture share one short-lived
/// generation lock. The world itself remains behind its own read/write lock;
/// this lock only binds the world handle, snapshot, source and entities to one
/// published capture.
struct ObservationState {
    world: Option<SharedWorld>,
    snapshot: Option<MinecraftSnapshotV1>,
    source: Option<FactSource>,
    tracked_entities: Vec<ProtocolEntitySnapshot>,
    generation: u64,
}

impl Default for ObservationState {
    fn default() -> Self {
        Self {
            world: None,
            snapshot: None,
            source: None,
            tracked_entities: Vec::new(),
            generation: 0,
        }
    }
}

impl ObservationState {
    fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }
}

enum ActiveMovementRegistration {
    Started { cancel_signal: Option<Arc<Notify>> },
    Cancelled,
}

struct SharedRuntime {
    writer: parking_lot::Mutex<EventWriter>,
    event_dispatch: parking_lot::Mutex<EventDispatchState>,
    swarm: parking_lot::Mutex<Option<Swarm>>,
    shutdown: Arc<Notify>,
    reconnect_cancel: Arc<Notify>,
    shutdown_requested: AtomicBool,
    config: RunConfig,
    commands: parking_lot::Mutex<VecDeque<QueuedCommand>>,
    subscribers: parking_lot::Mutex<Vec<mpsc::UnboundedSender<BackendEventEnvelope>>>,
    observation_subscribers: parking_lot::Mutex<Vec<ObservationSubscriber>>,
    next_observation_subscription_id: AtomicU64,
    observation: parking_lot::RwLock<ObservationState>,
    /// Authoritative runtime lifecycle state.  The facade reads this value;
    /// it does not reconstruct a second lifecycle machine from callbacks.
    backend_state: parking_lot::RwLock<BackendState>,
    reported_dimension: parking_lot::Mutex<Option<String>>,
    snapshot_revision: AtomicU64,
    viewport_revision: AtomicU64,
    lifecycle_revision: AtomicU64,
    command_revision: AtomicU64,
    tick_revision: AtomicU64,
    movement_generation: AtomicU64,
    /// Serializes command admission with stop/disconnect marking.  The lock
    /// is deliberately held only while changing admission state; actuator
    /// calls and completion callbacks never run under it.
    command_admission: parking_lot::Mutex<()>,
    active_movement: AtomicBool,
    active_movement_id: parking_lot::Mutex<Option<String>>,
    active_movement_cancel_signal: parking_lot::Mutex<Option<Arc<Notify>>>,
    active_movement_completion: parking_lot::Mutex<Option<Arc<CommandCompletionState>>>,
    /// A Move can be between its active declaration and its first actuator
    /// call.  Stop must wait for that registration window to close before it
    /// emits stopped/shuts down.
    active_movement_registration: AtomicBool,
    timer_started: AtomicBool,
    initial_chat_sent: AtomicBool,
    death_reported: AtomicBool,
    disconnect_reported: AtomicBool,
    stopped_reported: AtomicBool,
    faulted_reported: AtomicBool,
    last_close: parking_lot::Mutex<Option<BackendClose>>,
    last_failure: parking_lot::Mutex<Option<BackendFailure>>,
    stop_reason: parking_lot::Mutex<Option<String>>,
    reconnect_pending: AtomicBool,
    reconnect_add_pending: AtomicBool,
    reconnect_attempt_token: AtomicU64,
    attempt_epoch_reserved: AtomicBool,
    ready: AtomicBool,
    stopping: AtomicBool,
    #[cfg(test)]
    active_movement_registration_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    event_admission_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    finalize_stop_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    event_broadcast_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    disconnect_cleanup_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl SharedRuntime {
    fn new(config: RunConfig) -> Self {
        Self {
            writer: parking_lot::Mutex::new(EventWriter::new(&config.world_id)),
            event_dispatch: parking_lot::Mutex::new(EventDispatchState::default()),
            swarm: parking_lot::Mutex::new(None),
            shutdown: Arc::new(Notify::new()),
            reconnect_cancel: Arc::new(Notify::new()),
            shutdown_requested: AtomicBool::new(false),
            config,
            commands: parking_lot::Mutex::new(VecDeque::new()),
            subscribers: parking_lot::Mutex::new(Vec::new()),
            observation_subscribers: parking_lot::Mutex::new(Vec::new()),
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
            attempt_epoch_reserved: AtomicBool::new(false),
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
            disconnect_cleanup_hook: parking_lot::Mutex::new(None),
        }
    }

    fn set_backend_state(&self, state: BackendState) {
        *self.backend_state.write() = state;
    }

    fn backend_state(&self) -> BackendState {
        self.backend_state.read().clone()
    }

    fn connection_identity(&self) -> (u64, String, u32) {
        let writer = self.writer.lock();
        (
            writer.connection_epoch,
            writer.connection_attempt_id.clone(),
            u32::try_from(writer.connection_epoch).unwrap_or(u32::MAX),
        )
    }

    /// Construct and enqueue one event. The caller may hold command admission,
    /// but this function never drains the queue or invokes a subscriber.
    fn enqueue_event(&self, source: FactSource, payload: BackendEventPayload) -> bool {
        self.enqueue_event_at(source, payload, now_utc().to_rfc3339())
    }

    fn enqueue_event_at(
        &self,
        source: FactSource,
        payload: BackendEventPayload,
        occurred_at: String,
    ) -> bool {
        let kind = payload.kind();
        if matches!(kind, BackendEventKind::Lifecycle) {
            self.lifecycle_revision.fetch_add(1, Ordering::AcqRel);
        }
        let mut dispatch = self.event_dispatch.lock();
        let event = {
            let mut writer = self.writer.lock();
            writer.emit_at(source, payload, occurred_at)
        };
        dispatch.enqueue(event)
    }

    #[cfg(test)]
    fn emit(&self, source: FactSource, payload: BackendEventPayload) {
        let should_drain = self.enqueue_event(source, payload);
        if should_drain {
            self.drain_events();
        }
    }

    /// Normal product/protocol events must linearize their admission check and
    /// queue insertion. Stop takes the same lock, so a losing late event is
    /// discarded before it can appear after `stopped`.
    fn emit_if_running(&self, source: FactSource, payload: BackendEventPayload) -> bool {
        let should_drain = {
            let _admission = self.command_admission.lock();
            let Some(should_drain) = self.enqueue_event_if_running_locked(source, payload) else {
                return false;
            };
            should_drain
        };
        if should_drain {
            self.drain_events();
        }
        true
    }

    fn enqueue_event_if_running_locked(
        &self,
        source: FactSource,
        payload: BackendEventPayload,
    ) -> Option<bool> {
        if !self.command_execution_allowed_without_lock() {
            return None;
        }
        #[cfg(test)]
        self.invoke_event_admission_hook();
        Some(self.enqueue_event(source, payload))
    }

    fn lifecycle_event_allowed_without_lock(&self) -> bool {
        !self.stopping.load(Ordering::Acquire) && !self.stopped_reported.load(Ordering::Acquire)
    }

    /// 排水期间不持有 dispatch 或 observation registry 锁。callback 内重新
    /// emit 只会把事件追加到队尾，由当前 drainer 在本事件后继续处理。
    fn drain_events(&self) {
        loop {
            let event = {
                let mut dispatch = self.event_dispatch.lock();
                let Some(event) = dispatch.queue.pop_front() else {
                    dispatch.drainer_active = false;
                    return;
                };
                event
            };
            self.broadcast_event(event);
        }
    }

    fn broadcast_event(&self, event: BackendEventEnvelope) {
        // stdout is only the diagnostic process boundary.  The production
        // facade consumes the same FIFO through `subscribe` and explicitly
        // disables this side effect.
        if self.config.emit_stdout {
            match serde_json::to_string(&event) {
                Ok(line) => println!("{line}"),
                Err(error) => eprintln!("事件编码失败：{error}"),
            }
        }
        {
            let mut subscribers = self.subscribers.lock();
            subscribers.retain(|subscriber| subscriber.send(event.clone()).is_ok());
        }
        #[cfg(test)]
        self.invoke_event_broadcast_hook();

        let observation_kind = matches!(
            event.kind,
            BackendEventKind::Entity | BackendEventKind::Block | BackendEventKind::Sound
        );
        if !observation_kind {
            return;
        }

        let Some(observation_event) = observation_event_from_backend(&event) else {
            return;
        };
        let deliveries = {
            let subscribers = self.observation_subscribers.lock();
            subscribers
                .iter()
                .filter(|subscriber| subscriber.epoch == event_epoch(&observation_event))
                .filter_map(|subscriber| {
                    subscriber
                        .state
                        .reserve_callback()
                        .then(|| ObservationDelivery {
                            listener: subscriber.listener.clone(),
                            state: subscriber.state.clone(),
                            id: subscriber.id,
                        })
                })
                .collect::<Vec<_>>()
        };

        for delivery in deliveries {
            if !delivery.state.start_callback() {
                continue;
            }
            let callback_guard = ObservationCallbackGuard::enter(&delivery.state);
            let callback_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                delivery.listener.on_event(observation_event.clone());
            }));
            drop(callback_guard);
            delivery.state.finish_callback();
            if callback_result.is_err() {
                eprintln!(
                    "observation listener panic isolated: subscription_id={}; other listeners continue",
                    delivery.id
                );
            }
        }
    }

    /// 在发起网络连接前分配身份，并保证该身份下的第一条生命周期事件就是
    /// `connection_requested`。dispatch 锁覆盖身份分配、事件编号和广播，避免
    /// 其他生产者把事件插入新 attempt 与请求事件之间。
    fn begin_connection_attempt(&self) -> bool {
        let should_drain = {
            let _admission = self.command_admission.lock();
            let Some(should_drain) = self.begin_connection_attempt_locked(None) else {
                return false;
            };
            should_drain
        };
        if should_drain {
            self.drain_events();
        }
        true
    }

    fn begin_connection_attempt_locked(&self, reconnect_token: Option<u64>) -> Option<bool> {
        if self.stopping.load(Ordering::Acquire) {
            return None;
        }
        if let Some(token) = reconnect_token {
            if self.reconnect_attempt_token.load(Ordering::Acquire) != token {
                return None;
            }
            self.reconnect_add_pending.store(true, Ordering::Release);
        }
        self.attempt_epoch_reserved.store(true, Ordering::Release);
        self.disconnect_reported.store(false, Ordering::Release);
        self.stopped_reported.store(false, Ordering::Release);
        self.faulted_reported.store(false, Ordering::Release);
        self.shutdown_requested.store(false, Ordering::Release);
        *self.stop_reason.lock() = None;
        *self.last_close.lock() = None;
        *self.last_failure.lock() = None;
        self.clear_observations();
        self.lifecycle_revision.fetch_add(1, Ordering::AcqRel);

        let mut dispatch = self.event_dispatch.lock();
        let (event, epoch, attempt_id, attempt) = {
            let mut writer = self.writer.lock();
            writer.new_attempt();
            let epoch = writer.connection_epoch;
            let attempt_id = writer.connection_attempt_id.clone();
            let attempt = u32::try_from(epoch).unwrap_or(u32::MAX);
            (
                writer.emit(
                    FactSource::Commanded,
                    BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionRequested {
                        attempt,
                    }),
                ),
                epoch,
                attempt_id,
                attempt,
            )
        };
        self.set_backend_state(BackendState::Connecting {
            epoch,
            attempt_id,
            attempt,
        });
        Some(dispatch.enqueue(event))
    }

    /// `Event::Init` 消费连接发起前预留的身份，而不是再创建一个 epoch。
    /// 防御性 fallback 仍走同一入口，确保即使 Azalea 新增调用路径，也先有
    /// `connection_requested`，随后才发 transport 生命周期事件。
    fn consume_attempt_for_transport_init(&self) -> bool {
        if !self.attempt_epoch_reserved.swap(false, Ordering::AcqRel) {
            if !self.begin_connection_attempt() {
                return false;
            }
            self.attempt_epoch_reserved.store(false, Ordering::Release);
        }
        let _admission = self.command_admission.lock();
        if self.stopping.load(Ordering::Acquire) {
            return false;
        }
        self.disconnect_reported.store(false, Ordering::Release);
        self.clear_observations();
        true
    }

    fn claim_reconnect(&self) -> bool {
        let _admission = self.command_admission.lock();
        if self.stopping.load(Ordering::Acquire)
            || self.reconnect_pending.swap(true, Ordering::AcqRel)
        {
            return false;
        }
        true
    }

    fn admit_reconnect_attempt(&self) -> Option<u64> {
        let (token, should_drain) = {
            let _admission = self.command_admission.lock();
            if self.stopping.load(Ordering::Acquire)
                || !self.reconnect_pending.load(Ordering::Acquire)
            {
                return None;
            }
            let token = self.reconnect_attempt_token.fetch_add(1, Ordering::AcqRel) + 1;
            let Some(should_drain) = self.begin_connection_attempt_locked(Some(token)) else {
                return None;
            };
            (token, should_drain)
        };
        if should_drain {
            self.drain_events();
        }
        Some(token)
    }

    fn reconnect_add_is_allowed(&self, token: u64) -> bool {
        let _admission = self.command_admission.lock();
        !self.stopping.load(Ordering::Acquire)
            && self.reconnect_add_pending.load(Ordering::Acquire)
            && self.reconnect_attempt_token.load(Ordering::Acquire) == token
    }

    fn finish_reconnect_attempt(&self, token: u64) {
        let _admission = self.command_admission.lock();
        if self.reconnect_attempt_token.load(Ordering::Acquire) == token {
            self.reconnect_add_pending.store(false, Ordering::Release);
        }
        self.reconnect_pending.store(false, Ordering::Release);
    }

    fn context(&self) -> (String, u64, String) {
        self.writer.lock().context()
    }

    fn set_dimension(&self, dimension: impl Into<String>) -> Option<String> {
        let dimension = dimension.into();
        self.writer.lock().set_dimension(dimension.clone());
        self.reported_dimension.lock().replace(dimension)
    }

    fn set_dimension_if_running(&self, dimension: impl Into<String>) -> bool {
        let _admission = self.command_admission.lock();
        if !self.command_execution_allowed_without_lock() {
            return false;
        }
        self.set_dimension(dimension);
        true
    }

    fn observe_dimension(&self, dimension: impl Into<String>) {
        let dimension = dimension.into();
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock() {
                return;
            }
            let Some(previous) = self.set_dimension(dimension.clone()) else {
                return;
            };
            if previous == dimension {
                return;
            }
            self.enqueue_event(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::DimensionChanged {
                    from: previous,
                    to: dimension,
                }),
            )
        };
        if should_drain {
            self.drain_events();
        }
    }

    fn connection_epoch(&self) -> u64 {
        self.writer.lock().connection_epoch
    }

    fn add_observation_subscription(
        &self,
        epoch: u64,
        listener: Arc<dyn ObservationEventListener>,
    ) -> (u64, Arc<ObservationSubscriptionState>) {
        let id = self
            .next_observation_subscription_id
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        let state = Arc::new(ObservationSubscriptionState::new());
        self.observation_subscribers
            .lock()
            .push(ObservationSubscriber {
                id,
                epoch,
                listener,
                state: state.clone(),
            });
        (id, state)
    }

    fn remove_observation_subscription(&self, id: u64, state: &ObservationSubscriptionState) {
        {
            let mut subscribers = self.observation_subscribers.lock();
            subscribers.retain(|subscriber| subscriber.id != id);
            state.close();
        }
        state.wait_for_quiescence();
    }

    fn set_swarm(&self, swarm: Swarm) -> bool {
        let _admission = self.command_admission.lock();
        if self.stopping.load(Ordering::Acquire) || self.stopped_reported.load(Ordering::Acquire) {
            return false;
        }
        *self.swarm.lock() = Some(swarm);
        true
    }

    fn set_world_if_running(&self, world: SharedWorld) -> bool {
        let _admission = self.command_admission.lock();
        if !self.command_execution_allowed_without_lock() {
            return false;
        }
        let mut observation = self.observation.write();
        let replaced = observation
            .world
            .as_ref()
            .is_none_or(|current| !Arc::ptr_eq(current, &world));
        observation.world = Some(world);
        if replaced {
            observation.snapshot = None;
            observation.source = None;
            observation.tracked_entities.clear();
        }
        observation.bump_generation();
        true
    }

    fn clear_observations(&self) {
        *self.reported_dimension.lock() = None;
        let mut observation = self.observation.write();
        observation.world = None;
        observation.snapshot = None;
        observation.source = None;
        observation.tracked_entities.clear();
        observation.bump_generation();
    }

    fn close_evidence(&self, reason: Option<String>) -> CloseEvidence {
        let text = reason.clone().unwrap_or_default();
        let lower = text.to_ascii_lowercase();
        if text == "deliberate_stop" {
            return CloseEvidence {
                code: "deliberate_stop".to_owned(),
                retryable: false,
                deliberate: true,
                kick: None,
                error: None,
                end_reason: Some(text),
                failure: None,
            };
        }

        // A component attached to Event::Disconnect is already kick evidence;
        // its wording must not downgrade an unclassified kick to a retryable
        // ordinary connection end.
        let during_login = !self.ready.load(Ordering::Acquire);
        let server_shutdown = lower.contains("server_shutdown")
            || lower.contains("server shutdown")
            || lower.contains("server closed")
            || lower.contains("server restarting");
        if server_shutdown {
            return CloseEvidence {
                code: "server_shutdown".to_owned(),
                retryable: true,
                deliberate: false,
                kick: reason.map(|text| BackendKick { text, during_login }),
                error: None,
                end_reason: Some(text),
                failure: None,
            };
        }
        if lower.contains("banned")
            || lower.contains("whitelist")
            || lower.contains("invalid session")
            || lower.contains("authentication")
            || lower.contains("not authenticated")
        {
            let failure_code = if lower.contains("auth") || lower.contains("session") {
                BackendFailureCode::AuthenticationFailed
            } else {
                BackendFailureCode::PermissionDenied
            };
            return CloseEvidence {
                code: "permission_denied".to_owned(),
                retryable: false,
                deliberate: false,
                kick: Some(BackendKick {
                    text: text.clone(),
                    during_login,
                }),
                error: None,
                end_reason: Some(text.clone()),
                failure: Some(BackendFailure {
                    code: failure_code,
                    message: text,
                    retryable: false,
                }),
            };
        }
        if reason.is_some() {
            return CloseEvidence {
                code: "unclassified_kick".to_owned(),
                retryable: false,
                deliberate: false,
                kick: reason.map(|text| BackendKick { text, during_login }),
                error: None,
                end_reason: Some(text.clone()),
                failure: Some(BackendFailure {
                    code: BackendFailureCode::PermissionDenied,
                    message: text,
                    retryable: false,
                }),
            };
        }
        CloseEvidence {
            code: "connection_ended".to_owned(),
            retryable: true,
            deliberate: false,
            kick: None,
            error: None,
            end_reason: None,
            failure: None,
        }
    }

    fn emit_transport_connected(&self) {
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock() {
                return;
            }
            let (epoch, attempt_id, attempt) = self.connection_identity();
            self.set_backend_state(BackendState::LoggingIn {
                epoch,
                attempt_id,
                attempt,
            });
            self.enqueue_event(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected),
            )
        };
        if should_drain {
            self.drain_events();
        }
    }

    fn emit_logged_in(&self, version: impl Into<String>, dimension: String) {
        let version = version.into();
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock() {
                return;
            }
            self.set_dimension(dimension.clone());
            let (epoch, attempt_id, attempt) = self.connection_identity();
            self.set_backend_state(BackendState::Spawning {
                epoch,
                attempt_id,
                attempt,
            });
            self.enqueue_event(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::LoggedIn {
                    version,
                    dimension,
                }),
            )
        };
        if should_drain {
            self.drain_events();
        }
    }

    fn emit_ready(&self, snapshot_revision: u64) {
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock() {
                return;
            }
            self.ready.store(true, Ordering::Release);
            let (epoch, attempt_id, _) = self.connection_identity();
            let ready_at = now_utc().to_rfc3339();
            self.set_backend_state(BackendState::Ready {
                epoch,
                attempt_id,
                ready_at: ready_at.clone(),
            });
            self.enqueue_event_at(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::Ready {
                    snapshot_revision,
                }),
                ready_at,
            )
        };
        if should_drain {
            self.drain_events();
        }
    }

    #[cfg(test)]
    fn admit_death(&self) -> bool {
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock()
                || self.death_reported.swap(true, Ordering::AcqRel)
            {
                return false;
            }
            self.ready.store(false, Ordering::Release);
            let (epoch, attempt_id, _) = self.connection_identity();
            let died_at = now_utc().to_rfc3339();
            self.set_backend_state(BackendState::Dead {
                epoch,
                attempt_id,
                died_at: died_at.clone(),
            });
            self.enqueue_event_at(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::Died),
                died_at,
            )
        };
        if should_drain {
            self.drain_events();
        }
        true
    }

    /// Claim Death and finish all synchronous local movement cleanup before
    /// making `died` visible to subscribers. The event queue may already have
    /// another drainer, so enqueueing first and draining later would still let
    /// a re-entrant stop callback run before the physical release.
    fn admit_death_and_release(&self, release_inputs: impl FnOnce() -> bool) -> Option<bool> {
        let (released, should_drain) = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock()
                || self.death_reported.swap(true, Ordering::AcqRel)
            {
                return None;
            }
            self.ready.store(false, Ordering::Release);
            let (epoch, attempt_id, _) = self.connection_identity();
            let died_at = now_utc().to_rfc3339();
            self.set_backend_state(BackendState::Dead {
                epoch,
                attempt_id,
                died_at: died_at.clone(),
            });

            let movement_id = self.active_movement_id.lock().clone();
            let had_movement = movement_id.is_some()
                || self.active_movement_completion.lock().is_some()
                || self.active_movement_registration.load(Ordering::Acquire);
            let completion = self.active_movement_completion.lock().clone();
            let cancel_signal = self.active_movement_cancel_signal.lock().clone();
            if had_movement {
                self.movement_generation.fetch_add(1, Ordering::AcqRel);
                if let Some(completion) = completion.as_ref() {
                    completion.cancel("movement stopped by death".to_owned(), true);
                }
                if let Some(signal) = cancel_signal.as_ref() {
                    signal.notify_one();
                }
            }

            // This closure is synchronous and runs before `died` is enqueued;
            // no subscriber/callback can run while command admission is held.
            let released = release_inputs();
            self.active_movement.store(false, Ordering::Release);
            *self.active_movement_id.lock() = None;
            self.active_movement_cancel_signal.lock().take();
            self.active_movement_completion.lock().take();
            self.active_movement_registration
                .store(false, Ordering::Release);
            if let Some(completion) = completion {
                finish_command(
                    &Some(completion),
                    if released {
                        Err(BackendError::Cancelled {
                            operation: "movement stopped by death".to_owned(),
                        })
                    } else {
                        Err(command_component_failure("death move"))
                    },
                );
            }
            let should_drain = self.enqueue_event_at(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::Died),
                died_at,
            );
            (released, should_drain)
        };
        if should_drain {
            self.drain_events();
        }
        Some(released)
    }

    #[cfg(test)]
    fn emit_died(&self) {
        let _ = self.admit_death();
    }

    fn emit_respawn_transition_started(&self, from_dimension: String) {
        self.emit_if_running(
            FactSource::Commanded,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::RespawnTransitionStarted {
                from_dimension,
            }),
        );
    }

    fn emit_respawned(&self, dimension: String) {
        self.emit_if_running(
            FactSource::ServerObserved,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Respawned { dimension }),
        );
    }

    fn mark_disconnected(&self, reason: Option<String>) -> BackendClose {
        self.mark_disconnected_evidence(self.close_evidence(reason))
    }

    fn mark_connection_failed(&self, error: String) -> BackendClose {
        self.mark_disconnected_evidence(CloseEvidence {
            code: "connection_failed".to_owned(),
            retryable: true,
            deliberate: false,
            kick: None,
            error: Some(BackendCloseError {
                name: "connection_failed".to_owned(),
                message: error.clone(),
                code: None,
            }),
            end_reason: Some(error.clone()),
            failure: Some(BackendFailure {
                code: BackendFailureCode::ProtocolError,
                message: error,
                retryable: true,
            }),
        })
    }

    fn mark_disconnected_evidence(&self, evidence: CloseEvidence) -> BackendClose {
        let (close, should_drain, duplicate_cleanup) = {
            let _admission = self.command_admission.lock();
            if self.stopped_reported.load(Ordering::Acquire) {
                return self
                    .last_close
                    .lock()
                    .clone()
                    .unwrap_or_else(|| BackendClose {
                        epoch: self.connection_epoch(),
                        at: now_utc().to_rfc3339(),
                        code: "connection_ended".to_owned(),
                        retryable: true,
                        deliberate: false,
                        kick: None,
                        error: None,
                        end_reason: None,
                    });
            }

            // Once stop has won admission, a late Azalea disconnect cannot
            // replace the caller's deliberate close evidence.
            let evidence = if self.stopping.load(Ordering::Acquire) && !evidence.deliberate {
                CloseEvidence {
                    code: "deliberate_stop".to_owned(),
                    retryable: false,
                    deliberate: true,
                    kick: None,
                    error: None,
                    end_reason: Some("deliberate_stop".to_owned()),
                    failure: None,
                }
            } else {
                evidence
            };

            // Publish the disconnect bit and enqueue close under one admission
            // point. The queue is drained only after this lock is released.
            if self.disconnect_reported.swap(true, Ordering::AcqRel) {
                let close = self
                    .last_close
                    .lock()
                    .clone()
                    .unwrap_or_else(|| BackendClose {
                        epoch: self.connection_epoch(),
                        at: now_utc().to_rfc3339(),
                        code: "connection_ended".to_owned(),
                        retryable: true,
                        deliberate: false,
                        kick: None,
                        error: None,
                        end_reason: None,
                    });
                (close, false, true)
            } else {
                self.ready.store(false, Ordering::Release);
                let close = BackendClose {
                    epoch: self.connection_epoch(),
                    at: now_utc().to_rfc3339(),
                    code: evidence.code,
                    retryable: evidence.retryable,
                    deliberate: evidence.deliberate,
                    kick: evidence.kick,
                    error: evidence.error,
                    end_reason: evidence.end_reason,
                };
                *self.last_close.lock() = Some(close.clone());
                *self.last_failure.lock() = evidence.failure;

                // Seal and clean the attempt before making its close visible.
                // Stop takes the same admission lock, so it cannot enqueue or
                // drain `stopped` between close admission and local cleanup.
                #[cfg(test)]
                self.invoke_disconnect_cleanup_hook();
                self.cancel_active_movement(true);
                self.cancel_pending_commands();
                self.clear_observations();

                let should_drain = self.enqueue_event(
                    FactSource::ServerObserved,
                    BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionClosed {
                        close: close.clone(),
                    }),
                );
                (close, should_drain, false)
            }
        };

        // A duplicate can race a registration that is finishing after the
        // first disconnect. Repeating cleanup is harmless and helps that
        // registration converge, while the first close already completed its
        // mandatory cleanup under admission above.
        if duplicate_cleanup {
            self.cancel_active_movement(true);
            self.cancel_pending_commands();
            self.clear_observations();
        }
        if should_drain {
            self.drain_events();
        }
        close
    }

    fn failure_for_close(&self, close: &BackendClose) -> BackendFailure {
        let recorded = self.last_failure.lock().clone();
        // A fatal classification is stronger than the reconnect policy.  In
        // particular, permission/auth/version failures must remain visible
        // instead of being rewritten as `reconnect_disabled`.
        if let Some(failure) = recorded.as_ref().filter(|failure| !failure.retryable) {
            return failure.clone();
        }
        if close.retryable && !self.config.reconnect_enabled {
            return BackendFailure {
                code: BackendFailureCode::ReconnectDisabled,
                message: format!("reconnect disabled after close {}", close.code),
                retryable: false,
            };
        }
        recorded.unwrap_or_else(|| BackendFailure {
            code: BackendFailureCode::ProtocolError,
            message: format!("backend closed with non-retryable code {}", close.code),
            retryable: false,
        })
    }

    fn emit_faulted(&self, failure: BackendFailure) -> bool {
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.lifecycle_event_allowed_without_lock()
                || self.faulted_reported.swap(true, Ordering::AcqRel)
            {
                return false;
            }
            self.set_backend_state(BackendState::Faulted {
                failure: failure.clone(),
            });
            self.enqueue_event(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::Faulted { failure }),
            )
        };
        if should_drain {
            self.drain_events();
        }
        true
    }

    fn emit_reconnect_scheduled(&self, close: &BackendClose) -> Option<Duration> {
        let delay = self.config.reconnect_delay;
        let retry_at = (now_utc()
            + chrono::Duration::from_std(delay).unwrap_or_else(|_| chrono::Duration::zero()))
        .to_rfc3339();
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.lifecycle_event_allowed_without_lock() {
                return None;
            }
            let attempt =
                u32::try_from(self.connection_epoch().saturating_add(1)).unwrap_or(u32::MAX);
            self.set_backend_state(BackendState::Reconnecting {
                attempt,
                retry_at: retry_at.clone(),
                last_close: close.clone(),
            });
            self.enqueue_event(
                FactSource::ClientPredicted,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::ReconnectScheduled {
                    attempt,
                    retry_at,
                    close_code: close.code.clone(),
                }),
            )
        };
        if should_drain {
            self.drain_events();
        }
        Some(delay)
    }

    fn exit_swarm(&self) -> bool {
        if let Some(swarm) = self.swarm.lock().clone() {
            swarm.exit();
            true
        } else {
            false
        }
    }

    fn request_shutdown(&self) {
        // `notify_one` 会保留一个 permit，即使 stop() 发生在 run() 开始
        // select 之前，也不会因为时序而永久等待。
        self.shutdown_requested.store(true, Ordering::Release);
        self.shutdown.notify_one();
    }

    #[cfg(test)]
    fn set_active_movement_registration_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.active_movement_registration_hook.lock() = hook;
    }

    #[cfg(test)]
    fn invoke_active_movement_registration_hook(&self) {
        let hook = self.active_movement_registration_hook.lock().take();
        if let Some(hook) = hook {
            // Never invoke a test seam while holding its registry lock.  The
            // hook intentionally may call stop() re-entrantly.
            hook();
        }
    }

    #[cfg(test)]
    fn set_event_admission_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.event_admission_hook.lock() = hook;
    }

    #[cfg(test)]
    fn invoke_event_admission_hook(&self) {
        let hook = self.event_admission_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    fn set_finalize_stop_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.finalize_stop_hook.lock() = hook;
    }

    #[cfg(test)]
    fn invoke_finalize_stop_hook(&self) {
        let hook = self.finalize_stop_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    fn set_event_broadcast_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.event_broadcast_hook.lock() = hook;
    }

    #[cfg(test)]
    fn invoke_event_broadcast_hook(&self) {
        let hook = self.event_broadcast_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    fn set_disconnect_cleanup_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.disconnect_cleanup_hook.lock() = hook;
    }

    #[cfg(test)]
    fn invoke_disconnect_cleanup_hook(&self) {
        let hook = self.disconnect_cleanup_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    fn command_execution_allowed(&self) -> bool {
        let _admission = self.command_admission.lock();
        self.command_execution_allowed_without_lock()
    }

    fn with_command_admission<T>(&self, actuator: impl FnOnce() -> T) -> Result<T, ()> {
        let _admission = self.command_admission.lock();
        if !self.command_execution_allowed_without_lock() {
            return Err(());
        }
        // The closure contains only synchronous actuator operations.  It does
        // not await, emit events, or invoke completion callbacks while the
        // admission lock is held.
        Ok(actuator())
    }

    /// ConnectionFailed has already sealed the current attempt, so the normal
    /// command predicate (which rejects a disconnected attempt) is too
    /// narrow. This admits only the local disconnect actuator while keeping
    /// stop/stopped linearization on the same lock.
    fn with_disconnect_admission<T>(&self, actuator: impl FnOnce() -> T) -> Result<T, ()> {
        let _admission = self.command_admission.lock();
        if self.stopping.load(Ordering::Acquire) || self.stopped_reported.load(Ordering::Acquire) {
            return Err(());
        }
        Ok(actuator())
    }

    fn with_active_movement_admission<T>(
        &self,
        command_id: &str,
        generation: u64,
        completion: &Option<Arc<CommandCompletionState>>,
        actuator: impl FnOnce() -> T,
    ) -> Result<T, ()> {
        let _admission = self.command_admission.lock();
        if !self.command_execution_allowed_without_lock()
            || self.movement_generation.load(Ordering::Acquire) != generation
            || self.active_movement_id.lock().as_deref() != Some(command_id)
            || completion
                .as_ref()
                .is_some_and(|completion| completion.cancelled.load(Ordering::Acquire))
        {
            return Err(());
        }
        Ok(actuator())
    }

    fn finalize_stop_if_ready(&self) {
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.stopping.load(Ordering::Acquire)
                || self.active_movement_registration.load(Ordering::Acquire)
                || self.active_movement_completion.lock().is_some()
                || self.active_movement_id.lock().is_some()
            {
                return;
            }

            // The readiness check precedes taking the reason and both are
            // serialized with every cleanup caller. A second finalizer can no
            // longer observe an empty reason while the first one is about to
            // put it back.
            #[cfg(test)]
            self.invoke_finalize_stop_hook();

            let Some(reason) = self.stop_reason.lock().take() else {
                return;
            };
            if self.stopped_reported.swap(true, Ordering::AcqRel) {
                return;
            }
            self.set_backend_state(BackendState::Stopped {
                reason: Some(reason.clone()),
            });
            let should_drain = self.enqueue_event(
                FactSource::Commanded,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::Stopped {
                    reason: reason.clone(),
                }),
            );
            should_drain
        };
        if should_drain {
            self.drain_events();
        }
        self.request_shutdown();
    }

    fn enqueue_command_if_running(&self, command: BackendCommandEnvelope) -> Result<(), String> {
        let _admission = self.command_admission.lock();
        if self.stopping.load(Ordering::Acquire) {
            return Err("runtime is stopping".to_owned());
        }
        self.commands.lock().push_back(QueuedCommand {
            envelope: command,
            completion: None,
        });
        Ok(())
    }

    fn enqueue_command_with_completion_if_running(
        &self,
        command: BackendCommandEnvelope,
        completion: Arc<CommandCompletionState>,
    ) -> Result<(), String> {
        let _admission = self.command_admission.lock();
        if self.stopping.load(Ordering::Acquire) {
            return Err("runtime is stopping".to_owned());
        }
        self.commands.lock().push_back(QueuedCommand {
            envelope: command,
            completion: Some(completion),
        });
        Ok(())
    }

    #[cfg(test)]
    fn pop_command(&self) -> Option<QueuedCommand> {
        self.commands.lock().pop_front()
    }

    #[cfg(test)]
    fn requeue_front(&self, command: QueuedCommand) {
        self.commands.lock().push_front(command);
    }

    fn next_command_for_processing(&self) -> Option<QueuedCommand> {
        self.next_command_for_processing_with_hook(|| {})
    }

    fn next_command_for_processing_with_hook(
        &self,
        before_decision: impl FnOnce(),
    ) -> Option<QueuedCommand> {
        let mut commands = self.commands.lock();
        before_decision();
        let should_defer = commands.front().is_some_and(|command| {
            !self.ready.load(Ordering::Acquire)
                && !matches!(&command.envelope.command, BackendCommand::Respawn)
        });
        if should_defer {
            return None;
        }
        let command = commands.pop_front()?;
        Some(command)
    }

    fn cancel_pending_commands(&self) {
        for command in self.commands.lock().drain(..) {
            if let Some(completion) = command.completion {
                completion.cancel(format!("command:{}", command.envelope.id), false);
            }
        }
    }

    /// Declare a Move before touching the Azalea actuator.  The admission
    /// lock covers the only state transition that must be atomic with stop or
    /// disconnect: checking that the command may start and marking the
    /// registration window as live.  The rest of the registration is allowed
    /// to run without that lock so cancellation never waits on a callback or
    /// a bot operation.
    fn register_active_movement(
        &self,
        command_id: &str,
        generation: u64,
        duration_ms: u64,
        completion: &Option<Arc<CommandCompletionState>>,
    ) -> ActiveMovementRegistration {
        let admitted = {
            let _admission = self.command_admission.lock();
            if self.command_execution_allowed_without_lock() {
                self.active_movement_registration
                    .store(true, Ordering::Release);
                self.active_movement.store(true, Ordering::Release);
                *self.active_movement_id.lock() = Some(command_id.to_owned());
                true
            } else {
                false
            }
        };
        if !admitted {
            if let Some(completion) = completion {
                completion.cancel(format!("command:{command_id}"), false);
            }
            return ActiveMovementRegistration::Cancelled;
        }

        #[cfg(test)]
        self.invoke_active_movement_registration_hook();

        let cancel_signal = (duration_ms > 0).then(|| Arc::new(Notify::new()));
        *self.active_movement_cancel_signal.lock() = cancel_signal.clone();
        if let (Some(completion), Some(signal)) = (completion.as_ref(), cancel_signal.as_ref()) {
            completion.begin_active_release(signal.clone());
            *self.active_movement_completion.lock() = Some(completion.clone());
        }

        let cancelled = !self.command_execution_allowed()
            || completion
                .as_ref()
                .is_some_and(|completion| completion.cancelled.load(Ordering::Acquire));
        if cancelled {
            if let Some(completion) = completion {
                completion.cancel(format!("command:{command_id}"), true);
            }
            self.clear_registered_active_movement(
                command_id,
                generation,
                &cancel_signal,
                completion,
            );
            finish_command(
                completion,
                Err(BackendError::Cancelled {
                    operation: format!("command:{command_id}"),
                }),
            );
            self.finish_active_movement_registration();
            return ActiveMovementRegistration::Cancelled;
        }

        ActiveMovementRegistration::Started { cancel_signal }
    }

    fn command_execution_allowed_without_lock(&self) -> bool {
        !self.stopping.load(Ordering::Acquire) && !self.disconnect_reported.load(Ordering::Acquire)
    }

    fn clear_registered_active_movement(
        &self,
        command_id: &str,
        generation: u64,
        cancel_signal: &Option<Arc<Notify>>,
        completion: &Option<Arc<CommandCompletionState>>,
    ) -> bool {
        let owns_active_id = self.movement_generation.load(Ordering::Acquire) == generation
            && self.active_movement_id.lock().as_deref() == Some(command_id);
        if owns_active_id {
            self.active_movement.store(false, Ordering::Release);
            *self.active_movement_id.lock() = None;
        }

        if let Some(expected_signal) = cancel_signal {
            let mut current_signal = self.active_movement_cancel_signal.lock();
            if current_signal
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, expected_signal))
            {
                current_signal.take();
            }
        }
        if let Some(expected_completion) = completion {
            let mut current_completion = self.active_movement_completion.lock();
            if current_completion
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, expected_completion))
            {
                current_completion.take();
            }
        }
        owns_active_id
    }

    fn clear_idle_movement_state(&self, generation: u64) {
        let _admission = self.command_admission.lock();
        if self.movement_generation.load(Ordering::Acquire) != generation
            || self.active_movement_registration.load(Ordering::Acquire)
            || self.active_movement.load(Ordering::Acquire)
            || self.active_movement_id.lock().is_some()
        {
            return;
        }
        self.active_movement_cancel_signal.lock().take();
        self.active_movement_completion.lock().take();
    }

    fn finish_active_movement_registration(&self) {
        self.active_movement_registration
            .store(false, Ordering::Release);
        self.finalize_stop_if_ready();
    }

    fn cancel_registered_active_movement(
        &self,
        command_id: &str,
        generation: u64,
        cancel_signal: &Option<Arc<Notify>>,
        completion: &Option<Arc<CommandCompletionState>>,
    ) {
        if let Some(completion) = completion {
            completion.cancel(format!("command:{command_id}"), true);
        }
        self.clear_registered_active_movement(command_id, generation, cancel_signal, completion);
        finish_command(
            completion,
            Err(BackendError::Cancelled {
                operation: format!("command:{command_id}"),
            }),
        );
        self.active_movement_registration
            .store(false, Ordering::Release);
        self.finalize_stop_if_ready();
    }

    fn cancel_active_movement(
        &self,
        release_on_cancel: bool,
    ) -> Option<Arc<CommandCompletionState>> {
        if !release_on_cancel {
            self.movement_generation.fetch_add(1, Ordering::AcqRel);
        }
        let completion = self.active_movement_completion.lock().clone();
        let cancel_signal = self.active_movement_cancel_signal.lock().clone();
        if let Some(completion) = completion.as_ref() {
            completion.cancel(
                "movement superseded or stopped".to_owned(),
                release_on_cancel,
            );
        }
        if let Some(signal) = cancel_signal.as_ref() {
            signal.notify_one();
        }
        let deferred_release = release_on_cancel
            && (cancel_signal.is_some()
                || completion
                    .as_ref()
                    .is_some_and(|completion| completion.active_release.load(Ordering::Acquire)));
        if !deferred_release {
            self.active_movement.store(false, Ordering::Release);
            if let Some(completion) = completion.as_ref() {
                let mut active = self.active_movement_completion.lock();
                if active
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, completion))
                {
                    active.take();
                }
            }
            self.active_movement_cancel_signal.lock().take();
            *self.active_movement_id.lock() = None;
        } else {
            self.active_movement_cancel_signal.lock().take();
        }
        completion
    }

    fn refresh_snapshot(
        &self,
        bot: &Client,
        force: bool,
        source: FactSource,
    ) -> Option<MinecraftSnapshotV1> {
        let capture_generation = self.observation.read().generation;
        let (process_session_id, connection_epoch, connection_attempt_id) = self.context();
        let next_revision = self.snapshot_revision.load(Ordering::Acquire) + 1;
        let Some(candidate) = capture(
            bot,
            &self.config.world_id,
            &process_session_id,
            connection_epoch,
            &connection_attempt_id,
            next_revision,
            self.lifecycle_revision.load(Ordering::Acquire),
            now_utc(),
        ) else {
            // 断线/重连时 Azalea 会先移除本地玩家实体；此刻不能把“读不到”
            // 伪造成坐标，也不能调用 query_self 触发 panic。
            return None;
        };
        let entities = capture_tracked_entities(bot);
        if self.connection_epoch() != connection_epoch {
            return None;
        }
        let _admission = self.command_admission.lock();
        if !self.command_execution_allowed_without_lock() {
            return None;
        }
        let mut observation = self.observation.write();
        if observation.generation != capture_generation
            || self.connection_epoch() != connection_epoch
        {
            return None;
        }
        let changed = observation
            .snapshot
            .as_ref()
            .is_none_or(|previous| !previous.same_state_as(&candidate));
        observation.tracked_entities = entities;
        if force || changed {
            self.snapshot_revision
                .store(next_revision, Ordering::Release);
            observation.snapshot = Some(candidate.clone());
            observation.source = Some(source);
            observation.bump_generation();
            Some(candidate)
        } else {
            observation.bump_generation();
            None
        }
    }

    fn stored_snapshot(&self) -> Option<MinecraftSnapshotV1> {
        self.observation.read().snapshot.clone()
    }

    fn emit_snapshot(&self, snapshot: MinecraftSnapshotV1, source: FactSource) {
        self.emit_if_running(
            source,
            BackendEventPayload::SnapshotChanged(ContractProtocolSnapshotChangedEvent {
                group: "world".to_owned(),
                snapshot_revision: snapshot.snapshot_revision,
            }),
        );
    }

    fn initiate_stop(&self, reason: &str) {
        {
            // Make stop admission atomic with the start of a Move
            // registration.  Once either side owns this short lock, the
            // other side has a clear linearization point and stopped cannot
            // be finalized in the registration gap.
            let _admission = self.command_admission.lock();
            if self.stopping.swap(true, Ordering::AcqRel) {
                return;
            }
            let reason = reason.to_owned();
            *self.stop_reason.lock() = Some(reason.clone());
            let epoch = self.connection_epoch();
            self.set_backend_state(BackendState::Stopping {
                epoch: (epoch != 0).then_some(epoch),
                reason,
            });
            self.reconnect_add_pending.store(false, Ordering::Release);
            self.reconnect_attempt_token.fetch_add(1, Ordering::AcqRel);
            self.reconnect_pending.store(false, Ordering::Release);
            self.ready.store(false, Ordering::Release);
        }
        self.reconnect_cancel.notify_one();
        self.cancel_pending_commands();
        if self.connection_epoch() > 0 && !self.disconnect_reported.load(Ordering::Acquire) {
            self.mark_disconnected(Some("deliberate_stop".to_owned()));
        } else {
            self.cancel_active_movement(true);
        }
        self.exit_swarm();
        self.finalize_stop_if_ready();
    }
}

/// 对齐 MineIntent `snapshot/subscribe/motor/sendChat` 边界的本地运行时句柄。
#[derive(Clone)]
pub struct RuntimeHandle {
    shared: Arc<SharedRuntime>,
}

impl RuntimeHandle {
    pub fn new(config: RunConfig) -> Self {
        Self {
            shared: Arc::new(SharedRuntime::new(config)),
        }
    }

    /// Read the lifecycle state owned by the real runtime admission paths.
    /// Facades should delegate to this value instead of maintaining a second
    /// state machine from a best-effort event subscription.
    pub fn state(&self) -> BackendState {
        self.shared.backend_state()
    }

    /// Return the epoch owned by the runtime admission state.  Facade-owned
    /// observation and motor handles use this value to reject a handle from a
    /// previous connection attempt before delegating to the concrete runtime
    /// seam.
    pub fn connection_epoch(&self) -> u64 {
        self.shared.connection_epoch()
    }

    pub fn snapshot(&self) -> Option<MinecraftSnapshotV1> {
        self.shared.stored_snapshot()
    }

    /// 返回当前 `snapshot()` 的事实来源；调用方不得把 `client_predicted`
    /// 快照当作服务端确认状态。
    pub fn snapshot_source(&self) -> Option<FactSource> {
        self.shared.observation.read().source
    }

    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<BackendEventEnvelope> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.shared.subscribers.lock().push(sender);
        receiver
    }

    pub fn observation_source(&self) -> RuntimeObservationSource {
        RuntimeObservationSource {
            shared: self.shared.clone(),
            bound_epoch: self.shared.connection_epoch(),
        }
    }

    pub fn send_command(&self, command: BackendCommandEnvelope) -> Result<(), String> {
        if command.protocol != BACKEND_COMMAND_PROTOCOL {
            return Err(format!(
                "不支持的命令协议：{}，期望 {}",
                command.protocol, BACKEND_COMMAND_PROTOCOL
            ));
        }
        validate_command(&command.command)?;
        self.shared.enqueue_command_if_running(command)
    }

    fn send_command_with_completion(
        &self,
        command: BackendCommand,
    ) -> Result<CommandCompletion, String> {
        validate_command(&command)?;
        let envelope = self.next_command(command);
        let (completion, state) = CommandCompletion::channel(envelope.id.clone());
        match self
            .shared
            .enqueue_command_with_completion_if_running(envelope, state.clone())
        {
            Ok(()) => Ok(completion),
            Err(error) => {
                state.cancel(format!("command:{}", completion.command_id), false);
                Err(error)
            }
        }
    }

    fn next_command(&self, command: BackendCommand) -> BackendCommandEnvelope {
        let id = self.shared.command_revision.fetch_add(1, Ordering::AcqRel) + 1;
        BackendCommandEnvelope {
            protocol: BACKEND_COMMAND_PROTOCOL.to_owned(),
            id: format!("command-{id}"),
            issued_at: now_utc(),
            command,
        }
    }

    pub fn send_chat(&self, message: impl Into<String>) -> Result<(), String> {
        self.send_command(self.next_command(BackendCommand::SendChat {
            message: message.into(),
        }))
    }

    /// 发送与主仓库 motor `lookRelative` 同语义的相对视角输入，并返回一次性完成 future。
    pub fn look_relative(
        &self,
        yaw_degrees: f32,
        pitch_degrees: f32,
    ) -> Result<CommandCompletion, String> {
        self.send_command_with_completion(BackendCommand::LookRelative {
            yaw_degrees,
            pitch_degrees,
        })
    }

    /// 发送按键式移动输入；校验范围与主仓库 motor 的 50–1500ms 边界一致，
    /// 并返回在释放动作完成时 resolve 的 future。
    pub fn move_input(
        &self,
        directions: Vec<MotorDirection>,
        duration_ms: u64,
        sprint: Option<bool>,
        jump: Option<bool>,
        crouch: Option<bool>,
    ) -> Result<CommandCompletion, String> {
        self.send_command_with_completion(BackendCommand::Move {
            directions,
            duration_ms,
            sprint,
            jump,
            crouch,
        })
    }

    /// 释放全部移动/跳跃/潜行输入。
    pub fn release_all(&self) -> Result<CommandCompletion, String> {
        self.send_command_with_completion(BackendCommand::ReleaseAll)
    }

    /// 显式请求服务端执行重生；死亡后不会由运行时自动触发。
    pub fn respawn(&self) -> Result<(), String> {
        self.send_command(self.next_command(BackendCommand::Respawn))
    }

    /// 主动结束运行时；停止动作本身会写入 `commanded` 事件。
    pub fn stop(&self, reason: &str) {
        self.shared.initiate_stop(reason);
    }

    #[cfg(test)]
    pub(crate) fn test_drive_event(&self, source: FactSource, payload: BackendEventPayload) {
        match payload.clone() {
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected) => {
                self.shared.emit_transport_connected();
            }
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::LoggedIn {
                version,
                dimension,
            }) => {
                self.shared.emit_logged_in(version, dimension);
            }
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Ready {
                snapshot_revision,
            }) => {
                self.shared.emit_ready(snapshot_revision);
            }
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Faulted { failure }) => {
                self.shared.emit_faulted(failure);
            }
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Stopped { ref reason }) => {
                self.shared.initiate_stop(reason);
            }
            _ => self.shared.emit(source, payload),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_settle_next_command(&self, result: Result<(), BackendError>) -> bool {
        while let Some(command) = self.shared.pop_command() {
            if command
                .completion
                .as_ref()
                .is_some_and(|completion| completion.cancelled.load(Ordering::Acquire))
            {
                continue;
            }
            finish_command(&command.completion, result);
            return true;
        }
        false
    }

    #[cfg(test)]
    pub(crate) fn test_has_pending_command(&self) -> bool {
        !self.shared.commands.lock().is_empty()
    }
}

fn validate_command(command: &BackendCommand) -> Result<(), String> {
    match command {
        BackendCommand::SendChat { message } => {
            if message.is_empty() || message.contains(['\r', '\n', '\0']) {
                return Err("聊天消息必须是非空的单行文本".to_owned());
            }
        }
        BackendCommand::LookRelative {
            yaw_degrees,
            pitch_degrees,
        } => {
            if !yaw_degrees.is_finite() || yaw_degrees.abs() > 90.0 {
                return Err("相对 yaw 必须是 ±90 度以内的有限数".to_owned());
            }
            if !pitch_degrees.is_finite() || pitch_degrees.abs() > 90.0 {
                return Err("相对 pitch 必须是 ±90 度以内的有限数".to_owned());
            }
        }
        BackendCommand::Move {
            directions,
            duration_ms,
            ..
        } => {
            if directions.is_empty() || directions.len() > 4 {
                return Err("移动方向必须包含 1 到 4 个按键".to_owned());
            }
            if directions
                .iter()
                .enumerate()
                .any(|(index, direction)| directions[index + 1..].contains(direction))
            {
                return Err("移动方向不能重复".to_owned());
            }
            if !(50..=1_500).contains(duration_ms) {
                return Err("移动时长必须是 50 到 1500 毫秒".to_owned());
            }
        }
        BackendCommand::ReleaseAll | BackendCommand::Respawn => {}
    }
    Ok(())
}

/// 一个 observation source 的 owned typed subscription。
///
/// 回调由共享的 FIFO drainer 同步调用；每个订阅本身没有独立队列、后台转发
/// 任务或每订阅线程。关闭会先从 registry 线性化移除，再等待在途回调结束。
pub struct RuntimeObservationSubscription {
    shared: Arc<SharedRuntime>,
    id: u64,
    state: Arc<ObservationSubscriptionState>,
    closed: bool,
}

impl RuntimeObservationSubscription {
    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.shared
            .remove_observation_subscription(self.id, &self.state);
    }

    fn closed(&self) -> bool {
        self.closed || self.state.is_closed()
    }
}

impl Subscription for RuntimeObservationSubscription {
    fn unsubscribe(&mut self) {
        self.close();
    }

    fn is_closed(&self) -> bool {
        self.closed()
    }
}

impl Drop for RuntimeObservationSubscription {
    fn drop(&mut self) {
        self.close();
    }
}

const MAX_VIEWPORT_CAPTURE_ATTEMPTS: usize = 3;

#[derive(Clone)]
struct ViewportCapture {
    generation: u64,
    world: SharedWorld,
    world_bounds: WorldHeightBounds,
    pose: PoseSnapshot,
    entities: Vec<ProtocolEntitySnapshot>,
    source: FactSource,
}

enum ViewportReadAttempt {
    Complete(ViewportReadComplete),
    Retry,
}

enum ViewportReadComplete {
    Full(ContractViewportRead),
    Directed(DirectedViewportProjection),
}

#[derive(Clone)]
enum ViewportProjectionRequest {
    Full,
    Directed(Vec<ContractBlockPosition>),
}

enum ViewportProjectionWorkerResult {
    Complete {
        capture: ViewportCapture,
        projection: ViewportKernelProjection,
    },
    Retry,
}

enum ViewportKernelProjection {
    Full(ViewportProjection),
    Directed(DirectedViewportProjection),
}

/// 对齐 MineIntent `ProtocolObservationSource` 的只读 concrete observation seam。
///
/// `bound_epoch` 是创建 source 时捕获的值；所有 observation 方法都在读前后检查它。
#[derive(Clone)]
pub struct RuntimeObservationSource {
    shared: Arc<SharedRuntime>,
    bound_epoch: u64,
}

impl RuntimeObservationSource {
    pub fn epoch(&self) -> u64 {
        self.bound_epoch
    }

    fn ensure_current_epoch(&self) -> Result<(), BackendError> {
        let current_epoch = self.shared.connection_epoch();
        if current_epoch != self.bound_epoch {
            return Err(BackendError::StaleEpoch {
                bound_epoch: self.bound_epoch,
                current_epoch,
            });
        }
        Ok(())
    }

    fn self_pose_snapshot(&self) -> Result<Option<PoseSnapshot>, BackendError> {
        self.ensure_current_epoch()?;
        let pose = self
            .shared
            .observation
            .read()
            .snapshot
            .as_ref()
            .map(|snapshot| PoseSnapshot {
                position: snapshot.self_snapshot.position.clone(),
                velocity: snapshot.self_snapshot.velocity.clone(),
                yaw: snapshot.self_snapshot.yaw,
                pitch: snapshot.self_snapshot.pitch,
                on_ground: snapshot.self_snapshot.on_ground,
            });
        self.ensure_current_epoch()?;
        Ok(pose)
    }

    pub fn self_pose(&self) -> Result<ContractSelfPose, BackendError> {
        let pose = self.self_pose_snapshot()?;
        self.ensure_current_epoch()?;
        let pose = pose.ok_or_else(|| BackendError::NotReady {
            state: "self_pose_unavailable".to_owned(),
        })?;
        Ok(contract_self_pose(pose))
    }

    pub fn snapshot_source(&self) -> Result<Option<FactSource>, BackendError> {
        self.ensure_current_epoch()?;
        let source = self.shared.observation.read().source;
        self.ensure_current_epoch()?;
        Ok(source)
    }

    pub fn list_tracked_players(&self) -> Result<Vec<TrackedPlayerSnapshot>, BackendError> {
        self.ensure_current_epoch()?;
        let observation = self.shared.observation.read();
        let players = observation
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.tracked_players.clone())
            .unwrap_or_default();
        self.ensure_current_epoch()?;
        Ok(players)
    }

    fn list_tracked_entities_snapshot(&self) -> Result<Vec<ProtocolEntitySnapshot>, BackendError> {
        self.ensure_current_epoch()?;
        let entities = self.shared.observation.read().tracked_entities.clone();
        self.ensure_current_epoch()?;
        Ok(entities)
    }

    pub fn list_tracked_entities(
        &self,
    ) -> Result<Vec<ContractProtocolEntitySnapshot>, BackendError> {
        let entities = self.list_tracked_entities_snapshot()?;
        let converted = entities
            .into_iter()
            .map(contract_entity_snapshot)
            .collect::<Result<Vec<_>, _>>()?;
        self.ensure_current_epoch()?;
        Ok(converted)
    }

    /// 对齐 MineIntent viewport 的只读投影；所有坐标仍是 Minecraft 世界绝对坐标。
    ///
    /// 投影不会把本地缓存的方块直接宣称为可见：它会对视锥内候选执行暴露面和
    /// 遮挡射线判断。这个旧方法保留可配置 kernel 的 backend seam，但不是 atomic
    /// VIEW-02 seam；它不会携带与 projection 同次 capture 的 source/revision。
    /// 需要三项一致结果时必须使用 `read_viewport(OperationControl)`。
    pub fn viewport(
        &self,
        options: &ViewportOptions,
    ) -> Result<Option<ViewportProjection>, BackendError> {
        self.ensure_current_epoch()?;
        let Some(pose) = self.self_pose_snapshot()? else {
            return Ok(None);
        };
        let entities = self.list_tracked_entities_snapshot()?;
        let Some(world) = self.shared.observation.read().world.clone() else {
            self.ensure_current_epoch()?;
            return Ok(None);
        };
        // 一次投影只读一个世界视图，避免候选扫描的每次体素访问都重新获取
        // RwLock；独立的 read_block() 仍保持短锁，供增量读取使用。
        let world = world.read();
        project_viewport(
            &pose,
            &entities,
            |position| read_block_from_world(&world, position),
            options,
        )
        .map(Some)
        .map_err(|message| BackendError::InvalidCommand {
            field: "viewport".to_owned(),
            message,
        })
        .and_then(|projection| {
            self.ensure_current_epoch()?;
            Ok(projection)
        })
    }

    /// 读取已加载世界中的绝对方块状态；结果不等于视线可见性。
    ///
    /// 上层 viewport 应基于 `transparentHint`、碰撞/轮廓几何和观察者姿态
    /// 做射线或暴露面判断，避免把“客户端缓存里有数据”误报成“玩家看到了”。
    pub fn read_block(
        &self,
        position: ContractBlockPosition,
    ) -> Result<ContractBlockReadResult, BackendError> {
        let result = self.read_block_with_post_read_hook(
            BlockPosition {
                x: position.x,
                y: position.y,
                z: position.z,
            },
            || {},
        )?;
        self.ensure_current_epoch()?;
        Ok(contract_block_read_result(result))
    }

    fn read_block_with_post_read_hook(
        &self,
        position: BlockPosition,
        after_read: impl FnOnce(),
    ) -> Result<BlockReadResult, BackendError> {
        self.ensure_current_epoch()?;
        let Some(world) = self.shared.observation.read().world.clone() else {
            after_read();
            self.ensure_current_epoch()?;
            return Ok(BlockReadResult::Unloaded);
        };
        let world = world.read();
        let result = read_block_from_world(&world, position);
        after_read();
        self.ensure_current_epoch()?;
        Ok(result)
    }

    /// Read one coherent viewport capture and attach its provenance and read
    /// revision. The default options deliberately stay in the backend kernel;
    /// callers that need custom options may use the legacy non-atomic `viewport`
    /// method, but cannot combine it with `snapshot_source()` to form this seam.
    pub fn read_viewport(
        &self,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<ContractViewportRead, BackendError>> {
        Box::pin(async move {
            control.preflight("read_viewport")?;
            let request = ViewportProjectionRequest::Full;
            for attempt in 0..MAX_VIEWPORT_CAPTURE_ATTEMPTS {
                match self
                    .read_viewport_attempt(&control, request.clone())
                    .await
                    .map_err(backend_error_from_directed)?
                {
                    ViewportReadAttempt::Complete(ViewportReadComplete::Full(read)) => {
                        return Ok(read)
                    }
                    ViewportReadAttempt::Complete(ViewportReadComplete::Directed(_)) => {
                        unreachable!("full request cannot produce directed projection")
                    }
                    ViewportReadAttempt::Retry if attempt + 1 < MAX_VIEWPORT_CAPTURE_ATTEMPTS => {
                        control.preflight("read_viewport")?;
                        tokio::task::yield_now().await;
                    }
                    ViewportReadAttempt::Retry => {}
                }
            }
            control.preflight("read_viewport")?;
            self.ensure_current_epoch()?;
            Err(BackendError::NotReady {
                state: "viewport_capture_changed".to_owned(),
            })
        })
    }

    /// Read directed coordinates against the same atomic capture and viewport kernel as full.
    /// The captured world height is the only metadata used for zero-read out-of-world geometry
    /// classification; a target read that independently returns `OutOfWorld` becomes a row.
    pub fn read_directed_viewport(
        &self,
        positions: Vec<ContractBlockPosition>,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<DirectedViewportProjection, DirectedViewportError>> {
        Box::pin(async move {
            control.preflight("read_directed_viewport")?;
            let tuples = positions
                .iter()
                .map(|position| (position.x, position.y, position.z))
                .collect::<Vec<_>>();
            validate_directed_positions(&tuples).map_err(|message| {
                DirectedViewportError::Backend(BackendError::InvalidCommand {
                    field: "positions".to_owned(),
                    message,
                })
            })?;
            let request = ViewportProjectionRequest::Directed(positions);
            for attempt in 0..MAX_VIEWPORT_CAPTURE_ATTEMPTS {
                match self
                    .read_viewport_attempt(&control, request.clone())
                    .await?
                {
                    ViewportReadAttempt::Complete(ViewportReadComplete::Directed(projection)) => {
                        return Ok(projection)
                    }
                    ViewportReadAttempt::Complete(ViewportReadComplete::Full(_)) => {
                        unreachable!("directed request cannot produce full projection")
                    }
                    ViewportReadAttempt::Retry if attempt + 1 < MAX_VIEWPORT_CAPTURE_ATTEMPTS => {
                        control.preflight("read_directed_viewport")?;
                        tokio::task::yield_now().await;
                    }
                    ViewportReadAttempt::Retry => {}
                }
            }
            control.preflight("read_directed_viewport")?;
            self.ensure_current_epoch()?;
            Err(DirectedViewportError::Backend(BackendError::NotReady {
                state: "viewport_capture_changed".to_owned(),
            }))
        })
    }

    async fn read_viewport_attempt(
        &self,
        control: &OperationControl,
        request: ViewportProjectionRequest,
    ) -> Result<ViewportReadAttempt, DirectedViewportError> {
        self.ensure_current_epoch()?;
        let operation = match &request {
            ViewportProjectionRequest::Full => "read_viewport",
            ViewportProjectionRequest::Directed(_) => "read_directed_viewport",
        };
        control.preflight(operation)?;

        let (world, initial_generation) = {
            let observation = self.shared.observation.read();
            let writer = self.shared.writer.lock();
            if writer.connection_epoch != self.bound_epoch {
                return Err(DirectedViewportError::Backend(BackendError::StaleEpoch {
                    bound_epoch: self.bound_epoch,
                    current_epoch: writer.connection_epoch,
                }));
            }
            if !self.shared.ready.load(Ordering::Acquire) {
                return Err(DirectedViewportError::Backend(BackendError::NotReady {
                    state: "not_ready".to_owned(),
                }));
            }
            if observation.snapshot.is_none() {
                return Err(DirectedViewportError::Backend(BackendError::NotReady {
                    state: "viewport_snapshot_unavailable".to_owned(),
                }));
            }
            if observation.source.is_none() {
                return Err(DirectedViewportError::Backend(BackendError::NotReady {
                    state: "viewport_source_unavailable".to_owned(),
                }));
            }
            let Some(world) = observation.world.clone() else {
                return Err(DirectedViewportError::Backend(BackendError::NotReady {
                    state: "viewport_world_unavailable".to_owned(),
                }));
            };
            (world, observation.generation)
        };

        control.preflight(operation)?;
        let projection_shared = self.shared.clone();
        let projection_world = world.clone();
        let projection_initial_generation = initial_generation;
        let projection_bound_epoch = self.bound_epoch;
        let projection_control = control.clone();
        let projection_request = request;
        let mut projection_task = tokio::task::spawn_blocking(move || {
            // Acquire the world-owned read guard before cloning the state
            // values. This makes the world view and the published metadata one
            // capture while keeping the shared observation lock short-lived.
            let world_read = projection_world.read();
            let capture = {
                let observation = projection_shared.observation.read();
                let writer = projection_shared.writer.lock();
                if writer.connection_epoch != projection_bound_epoch {
                    return Err(DirectedViewportError::Backend(BackendError::StaleEpoch {
                        bound_epoch: projection_bound_epoch,
                        current_epoch: writer.connection_epoch,
                    }));
                }
                if !projection_shared.ready.load(Ordering::Acquire) {
                    return Err(DirectedViewportError::Backend(BackendError::NotReady {
                        state: "not_ready".to_owned(),
                    }));
                }
                if observation.generation != projection_initial_generation
                    || !observation
                        .world
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &projection_world))
                {
                    return Ok(ViewportProjectionWorkerResult::Retry);
                }
                let Some(snapshot) = observation.snapshot.as_ref() else {
                    return Err(DirectedViewportError::Backend(BackendError::NotReady {
                        state: "viewport_snapshot_unavailable".to_owned(),
                    }));
                };
                if snapshot.connection_epoch != writer.connection_epoch {
                    return Err(DirectedViewportError::Backend(BackendError::NotReady {
                        state: "viewport_snapshot_epoch_mismatch".to_owned(),
                    }));
                }
                let Some(source) = observation.source else {
                    return Err(DirectedViewportError::Backend(BackendError::NotReady {
                        state: "viewport_source_unavailable".to_owned(),
                    }));
                };
                ViewportCapture {
                    generation: observation.generation,
                    world: projection_world.clone(),
                    world_bounds: WorldHeightBounds::new(
                        world_read.chunks.min_y(),
                        world_read.chunks.height(),
                    ),
                    pose: PoseSnapshot {
                        position: snapshot.self_snapshot.position.clone(),
                        velocity: snapshot.self_snapshot.velocity.clone(),
                        yaw: snapshot.self_snapshot.yaw,
                        pitch: snapshot.self_snapshot.pitch,
                        on_ground: snapshot.self_snapshot.on_ground,
                    },
                    entities: observation.tracked_entities.clone(),
                    source,
                }
            };
            projection_control.preflight(operation)?;
            let projection = match projection_request {
                ViewportProjectionRequest::Full => ViewportKernelProjection::Full(
                    project_viewport_with_checkpoint(
                        &capture.pose,
                        &capture.entities,
                        |position| read_block_from_world(&world_read, position),
                        &ViewportOptions::default(),
                        || projection_control.preflight(operation),
                    )
                    .map_err(DirectedViewportError::Backend)?,
                ),
                ViewportProjectionRequest::Directed(positions) => {
                    let positions = positions
                        .into_iter()
                        .map(|position| [position.x, position.y, position.z])
                        .collect::<Vec<_>>();
                    ViewportKernelProjection::Directed(project_directed_viewport(
                        &capture.pose,
                        &positions,
                        |position| read_block_from_world(&world_read, position),
                        &ViewportOptions::default(),
                        capture.world_bounds,
                        || projection_control.preflight(operation),
                    )?)
                }
            };
            Ok(ViewportProjectionWorkerResult::Complete {
                capture,
                projection,
            })
        });
        let cancellation = control.cancelled();
        let deadline = async {
            if let Some(deadline) = control.deadline_elapsed() {
                deadline.await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::pin!(cancellation);
        tokio::pin!(deadline);
        let worker_result = tokio::select! {
            result = &mut projection_task => result
                .map_err(|error| DirectedViewportError::Backend(BackendError::BackendFailure {
                    failure: BackendFailure {
                        code: BackendFailureCode::ProtocolError,
                        message: format!("viewport projection task failed: {error}"),
                        retryable: true,
                    },
                }))??,
            _ = &mut cancellation => {
                projection_task.abort();
                return Err(DirectedViewportError::Backend(control_wakeup_error(
                    control, operation,
                )));
            }
            _ = &mut deadline => {
                projection_task.abort();
                return Err(DirectedViewportError::Backend(control_wakeup_error(
                    control, operation,
                )));
            }
        };
        let (capture, projection) = match worker_result {
            ViewportProjectionWorkerResult::Complete {
                capture,
                projection,
            } => (capture, projection),
            ViewportProjectionWorkerResult::Retry => return Ok(ViewportReadAttempt::Retry),
        };
        control.preflight(operation)?;

        let observation = self.shared.observation.read();
        let writer = self.shared.writer.lock();
        if writer.connection_epoch != self.bound_epoch {
            return Err(DirectedViewportError::Backend(BackendError::StaleEpoch {
                bound_epoch: self.bound_epoch,
                current_epoch: writer.connection_epoch,
            }));
        }
        if !self.shared.ready.load(Ordering::Acquire) {
            return Err(DirectedViewportError::Backend(BackendError::NotReady {
                state: "not_ready".to_owned(),
            }));
        }
        if observation.generation != capture.generation
            || !observation
                .world
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &capture.world))
        {
            return Ok(ViewportReadAttempt::Retry);
        }
        if observation.source != Some(capture.source) {
            return Ok(ViewportReadAttempt::Retry);
        }
        let revision = self.shared.viewport_revision.fetch_add(1, Ordering::AcqRel) + 1;
        let complete = match projection {
            ViewportKernelProjection::Full(projection) => {
                ViewportReadComplete::Full(ContractViewportRead {
                    projection: contract_viewport_projection(projection),
                    source: contract_fact_source(capture.source),
                    revision,
                })
            }
            ViewportKernelProjection::Directed(projection) => {
                ViewportReadComplete::Directed(projection)
            }
        };
        Ok(ViewportReadAttempt::Complete(complete))
    }

    fn subscribe_listener(
        &self,
        listener: Arc<dyn ObservationEventListener>,
        post_register_hook: Option<&dyn Fn()>,
    ) -> Result<RuntimeObservationSubscription, BackendError> {
        self.ensure_current_epoch()?;
        let (id, state) = self
            .shared
            .add_observation_subscription(self.bound_epoch, listener);
        if let Some(hook) = post_register_hook {
            hook();
        }
        if let Err(error) = self.ensure_current_epoch() {
            self.shared.remove_observation_subscription(id, &state);
            return Err(error);
        }
        Ok(RuntimeObservationSubscription {
            shared: self.shared.clone(),
            id,
            state,
            closed: false,
        })
    }

    #[cfg(test)]
    fn subscribe_with_post_register_hook(
        &self,
        listener: Arc<dyn ObservationEventListener>,
        hook: impl Fn(),
    ) -> Result<RuntimeObservationSubscription, BackendError> {
        self.subscribe_listener(listener, Some(&hook))
    }
}

impl ProtocolObservationSource for RuntimeObservationSource {
    fn epoch(&self) -> u64 {
        RuntimeObservationSource::epoch(self)
    }

    fn self_pose(&self) -> Result<ContractSelfPose, BackendError> {
        RuntimeObservationSource::self_pose(self)
    }

    fn list_tracked_entities(&self) -> Result<Vec<ContractProtocolEntitySnapshot>, BackendError> {
        RuntimeObservationSource::list_tracked_entities(self)
    }

    fn read_block(
        &self,
        position: ContractBlockPosition,
    ) -> Result<ContractBlockReadResult, BackendError> {
        RuntimeObservationSource::read_block(self, position)
    }

    fn subscribe(
        &self,
        listener: Arc<dyn ObservationEventListener>,
    ) -> Result<Box<dyn Subscription>, BackendError> {
        self.subscribe_listener(listener, None)
            .map(|subscription| Box::new(subscription) as Box<dyn Subscription>)
    }

    fn read_viewport(
        &self,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<ContractViewportRead, BackendError>> {
        // Fully qualify the inherent method so the trait adapter cannot recurse.
        RuntimeObservationSource::read_viewport(self, control)
    }

    fn read_directed_viewport(
        &self,
        positions: Vec<ContractBlockPosition>,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<DirectedViewportProjection, DirectedViewportError>> {
        RuntimeObservationSource::read_directed_viewport(self, positions, control)
    }
}

fn contract_vec3(value: Vec3Value) -> ContractVec3Value {
    ContractVec3Value {
        x: value.x,
        y: value.y,
        z: value.z,
    }
}

fn contract_self_pose(pose: PoseSnapshot) -> ContractSelfPose {
    ContractSelfPose {
        position: contract_vec3(pose.position),
        velocity: contract_vec3(pose.velocity),
        yaw: f64::from(pose.yaw),
        pitch: f64::from(pose.pitch),
    }
}

fn dto_conversion_error(field: &str, message: impl Into<String>) -> BackendError {
    BackendError::BackendFailure {
        failure: BackendFailure {
            code: BackendFailureCode::ProtocolError,
            message: format!(
                "cannot convert backend observation DTO {field}: {}",
                message.into()
            ),
            retryable: false,
        },
    }
}

fn contract_entity_snapshot(
    entity: ProtocolEntitySnapshot,
) -> Result<ContractProtocolEntitySnapshot, BackendError> {
    let equipment = entity
        .equipment
        .into_iter()
        .map(|item| {
            let count = u32::try_from(item.count).map_err(|_| {
                dto_conversion_error(
                    "entity.equipment.count",
                    format!("negative item count {}", item.count),
                )
            })?;
            Ok(ContractEntityEquipmentSnapshot {
                slot: u32::from(item.slot),
                item_name: item.item_name,
                count,
            })
        })
        .collect::<Result<Vec<_>, BackendError>>()?;

    Ok(ContractProtocolEntitySnapshot {
        entity_key: entity.entity_key,
        protocol_entity_id: entity.protocol_entity_id,
        entity_type: entity.entity_type,
        name: entity.name,
        username: entity.username,
        uuid: entity.uuid,
        position: contract_vec3(entity.position),
        velocity: contract_vec3(entity.velocity),
        yaw: f64::from(entity.yaw),
        pitch: f64::from(entity.pitch),
        head_yaw: entity.head_yaw.map(f64::from),
        width: f64::from(entity.width),
        height: f64::from(entity.height),
        on_ground: entity.on_ground,
        pose: entity.pose,
        held_item_name: entity.held_item_name,
        equipment,
        valid: entity.valid,
    })
}

fn contract_block_snapshot(block: ProtocolBlockSnapshot) -> ContractProtocolBlockSnapshot {
    ContractProtocolBlockSnapshot {
        position: ContractBlockPosition {
            x: block.position.x,
            y: block.position.y,
            z: block.position.z,
        },
        name: block.name,
        state_id: block.state_id,
        properties: block
            .properties
            .into_iter()
            .map(|(key, value)| (key, parse_block_property_value(&value)))
            .collect(),
        collision_shapes: block.collision_shapes,
        transparent_hint: block.transparent_hint,
        bounding_box: match block.bounding_box {
            BlockBoundingBox::Block => ContractBlockBoundingBox::Block,
            BlockBoundingBox::Empty => ContractBlockBoundingBox::Empty,
        },
    }
}

fn contract_block_read_result(result: BlockReadResult) -> ContractBlockReadResult {
    match result {
        BlockReadResult::Loaded { block } => ContractBlockReadResult::Loaded {
            block: contract_block_snapshot(block),
        },
        BlockReadResult::Unloaded => ContractBlockReadResult::Unloaded,
        BlockReadResult::OutOfWorld => ContractBlockReadResult::OutOfWorld,
    }
}

fn contract_event_metadata(event: &BackendEventEnvelope) -> ContractBackendEventMetadata {
    ContractBackendEventMetadata {
        id: event.id.clone(),
        occurred_at: event.occurred_at.clone(),
        process_session_id: event.process_session_id.clone(),
        connection_epoch: event.connection_epoch,
        connection_attempt_id: event.connection_attempt_id.clone(),
        world_id: event.world_id.clone(),
        dimension: event.dimension.clone(),
    }
}

fn contract_event_kind(kind: BackendEventKind) -> ContractBackendEventKind {
    match kind {
        BackendEventKind::Entity => ContractBackendEventKind::Entity,
        BackendEventKind::Block => ContractBackendEventKind::Block,
        BackendEventKind::Sound => ContractBackendEventKind::Sound,
        _ => unreachable!("non-observation event cannot enter typed observation adapter"),
    }
}

fn observation_event_from_backend(event: &BackendEventEnvelope) -> Option<ObservationEvent> {
    let metadata = contract_event_metadata(event);
    let source = contract_fact_source(event.source);
    match (&event.kind, &event.payload) {
        (BackendEventKind::Entity, BackendEventPayload::Entity(payload)) => {
            Some(ObservationEvent::Entity(ContractBackendEventEnvelope::new(
                metadata,
                contract_event_kind(event.kind),
                source,
                payload.clone(),
            )))
        }
        (BackendEventKind::Block, BackendEventPayload::Block(payload)) => {
            Some(ObservationEvent::Block(ContractBackendEventEnvelope::new(
                metadata,
                contract_event_kind(event.kind),
                source,
                payload.clone(),
            )))
        }
        (BackendEventKind::Sound, BackendEventPayload::Sound(payload)) => {
            Some(ObservationEvent::Sound(ContractBackendEventEnvelope::new(
                metadata,
                contract_event_kind(event.kind),
                source,
                payload.clone(),
            )))
        }
        _ => None,
    }
}

struct CommandCompletionState {
    sender: parking_lot::Mutex<Option<oneshot::Sender<Result<(), BackendError>>>>,
    settled_result: parking_lot::Mutex<Option<Result<(), BackendError>>>,
    settled_cv: parking_lot::Condvar,
    /// Owns the single finishing transition. `settled` is published only
    /// after result, physical-release bookkeeping, and the oneshot have all
    /// been published under this ownership.
    finish_lock: parking_lot::Mutex<()>,
    cancelled: AtomicBool,
    active_release: AtomicBool,
    release_on_cancel: AtomicBool,
    cancel_signal: parking_lot::Mutex<Option<Arc<Notify>>>,
    settled: AtomicBool,
    settled_signal: Notify,
}

impl CommandCompletionState {
    fn finish(&self, result: Result<(), BackendError>) {
        let _finish = self.finish_lock.lock();
        if self.settled.load(Ordering::Acquire) {
            return;
        }
        *self.settled_result.lock() = Some(result.clone());
        self.active_release.store(false, Ordering::Release);
        if let Some(sender) = self.sender.lock().take() {
            let _ = sender.send(result);
        }
        // This is deliberately the last publication in the finishing
        // transition. Waiters that observe `settled` therefore also observe
        // the result and the completed physical-release bookkeeping.
        self.settled.store(true, Ordering::Release);
        self.settled_cv.notify_all();
        self.settled_signal.notify_one();
    }

    fn set_cancel_signal(&self, signal: Arc<Notify>) {
        let already_cancelled = self.cancelled.load(Ordering::Acquire);
        *self.cancel_signal.lock() = Some(signal.clone());
        if already_cancelled {
            signal.notify_one();
        }
    }

    fn begin_active_release(&self, signal: Arc<Notify>) {
        self.active_release.store(true, Ordering::Release);
        self.set_cancel_signal(signal);
    }

    #[cfg(test)]
    async fn wait_settled(&self) {
        while !self.settled.load(Ordering::Acquire) {
            self.settled_signal.notified().await;
        }
    }

    fn cancel(&self, operation: String, release_on_cancel: bool) {
        self.cancelled.store(true, Ordering::Release);
        self.release_on_cancel
            .fetch_or(release_on_cancel, Ordering::AcqRel);
        if let Some(signal) = self.cancel_signal.lock().as_ref() {
            signal.notify_one();
        }
        // An active Move owns the physical release.  Its task finishes the
        // oneshot only after inputs and active state have been cleared.  A
        // queued/superseded command has no physical work left and settles now.
        if !self.active_release.load(Ordering::Acquire)
            || !self.release_on_cancel.load(Ordering::Acquire)
        {
            self.finish(Err(BackendError::Cancelled { operation }));
        }
    }
}

/// Minimal command completion seam used by the runtime motor queue.
///
/// It is intentionally not a backend facade: callers only get the command id,
/// cancellation, and one ordered result for the queued motor action.
pub struct CommandCompletion {
    command_id: String,
    receiver: oneshot::Receiver<Result<(), BackendError>>,
    state: Arc<CommandCompletionState>,
}

impl CommandCompletion {
    fn channel(command_id: String) -> (Self, Arc<CommandCompletionState>) {
        let (sender, receiver) = oneshot::channel();
        let state = Arc::new(CommandCompletionState {
            sender: parking_lot::Mutex::new(Some(sender)),
            settled_result: parking_lot::Mutex::new(None),
            settled_cv: parking_lot::Condvar::new(),
            finish_lock: parking_lot::Mutex::new(()),
            cancelled: AtomicBool::new(false),
            active_release: AtomicBool::new(false),
            release_on_cancel: AtomicBool::new(false),
            cancel_signal: parking_lot::Mutex::new(None),
            settled: AtomicBool::new(false),
            settled_signal: Notify::new(),
        });
        (
            Self {
                command_id,
                receiver,
                state: state.clone(),
            },
            state,
        )
    }

    pub fn command_id(&self) -> &str {
        &self.command_id
    }

    pub fn cancel(&self) {
        self.state
            .cancel(format!("command:{}", self.command_id), true);
    }

    pub async fn wait(self) -> Result<(), BackendError> {
        match self.receiver.await {
            Ok(result) => result,
            Err(_) => Err(BackendError::Cancelled {
                operation: format!("command:{}", self.command_id),
            }),
        }
    }

    /// Synchronous companion used by the frozen `release_all` facade method.
    /// The condition variable is independent of Tokio, so this remains safe
    /// to call from a caller that happens to be inside another async runtime.
    pub(crate) fn wait_blocking(self) -> Result<(), BackendError> {
        let mut result = self.state.settled_result.lock();
        while result.is_none() {
            self.state.settled_cv.wait(&mut result);
        }
        result
            .take()
            .expect("settled result exists after completion wait")
    }

    pub(crate) fn cancellation_handle(&self) -> CommandCompletionCancellation {
        CommandCompletionCancellation(self.state.clone())
    }
}

#[derive(Clone)]
pub(crate) struct CommandCompletionCancellation(Arc<CommandCompletionState>);

impl CommandCompletionCancellation {
    pub(crate) fn cancel(&self) {
        self.0
            .cancel("command completion cancelled by caller".to_owned(), true);
    }

    pub(crate) async fn wait_settled(&self) {
        while !self.0.settled.load(Ordering::Acquire) {
            self.0.settled_signal.notified().await;
        }
    }
}

struct QueuedCommand {
    envelope: BackendCommandEnvelope,
    completion: Option<Arc<CommandCompletionState>>,
}

#[derive(Clone, Debug)]
struct CloseEvidence {
    code: String,
    retryable: bool,
    deliberate: bool,
    kick: Option<BackendKick>,
    error: Option<BackendCloseError>,
    end_reason: Option<String>,
    failure: Option<BackendFailure>,
}

fn event_epoch(event: &ObservationEvent) -> u64 {
    match event {
        ObservationEvent::Entity(event) => event.connection_epoch,
        ObservationEvent::Block(event) => event.connection_epoch,
        ObservationEvent::Sound(event) => event.connection_epoch,
    }
}

fn contract_fact_source(source: FactSource) -> ContractFactSource {
    match source {
        FactSource::Commanded => ContractFactSource::Commanded,
        FactSource::ClientPredicted => ContractFactSource::ClientPredicted,
        FactSource::ServerObserved => ContractFactSource::ServerObserved,
    }
}

fn control_wakeup_error(control: &OperationControl, operation: &str) -> BackendError {
    match control.preflight(operation) {
        Err(error) => error,
        Ok(()) => BackendError::BackendFailure {
            failure: BackendFailure {
                code: BackendFailureCode::ProtocolError,
                message: format!("{operation} control woke without cancellation or deadline"),
                retryable: true,
            },
        },
    }
}

fn backend_error_from_directed(error: DirectedViewportError) -> BackendError {
    match error {
        DirectedViewportError::Backend(error) => error,
        DirectedViewportError::OutOfWorld { .. } => BackendError::BackendFailure {
            failure: BackendFailure {
                code: BackendFailureCode::ProtocolError,
                message: "full viewport encountered an out-of-world ray coordinate".to_owned(),
                retryable: false,
            },
        },
    }
}

fn contract_viewport_projection(projection: ViewportProjection) -> ContractViewportProjection {
    ContractViewportProjection {
        frame: ContractViewportFrame {
            coordinates:
                mineintent_contracts::minecraft::ViewportCoordinateSystem::MinecraftWorldAbsolute,
            self_pose: ContractViewportSelfPose {
                position: projection.frame.self_pose.position,
                yaw_degrees: projection.frame.self_pose.yaw_degrees,
                pitch_degrees: projection.frame.self_pose.pitch_degrees,
            },
            legend: ContractViewportLegend {
                visible_entities: projection.frame.legend.visible_entities,
                visible_blocks: projection.frame.legend.visible_blocks,
            },
        },
        standing_on_block: projection.standing_on_block.map(contract_viewport_block),
        looked_at_block: projection.looked_at_block.map(contract_viewport_block),
        visible_entities: ContractVisibleEntitiesView {
            items: projection
                .visible_entities
                .items
                .into_iter()
                .map(|entity| ContractVisibleEntityView {
                    entity_type: entity.entity_type,
                    player: entity.player,
                    position: entity.position,
                })
                .collect(),
            truncated: projection.visible_entities.truncated,
        },
        visible_blocks: ContractVisibleBlocksView {
            blocks: projection.visible_blocks.blocks,
            truncated: projection.visible_blocks.truncated,
        },
    }
}

fn contract_viewport_block(block: ViewportBlock) -> ContractViewportBlock {
    ContractViewportBlock {
        block: block.block,
        position: block.position.map(f64::from),
    }
}

fn read_block_from_world(world: &azalea::world::World, position: BlockPosition) -> BlockReadResult {
    let block_position = azalea::BlockPos {
        x: position.x,
        y: position.y,
        z: position.z,
    };
    let y = i64::from(block_position.y);
    let min_y = i64::from(world.chunks.min_y());
    let max_y_exclusive = min_y + i64::from(world.chunks.height());
    if y < min_y || y >= max_y_exclusive {
        return BlockReadResult::OutOfWorld;
    }
    let Some(state) = world.get_block_state(block_position) else {
        return BlockReadResult::Unloaded;
    };
    BlockReadResult::Loaded {
        block: block_snapshot(position, state),
    }
}

#[derive(Clone, Component)]
struct BotState {
    shared: Arc<SharedRuntime>,
}

impl Default for BotState {
    fn default() -> Self {
        Self {
            shared: Arc::new(SharedRuntime::new(RunConfig::default())),
        }
    }
}

#[derive(Clone, Resource)]
struct SwarmState {
    shared: Arc<SharedRuntime>,
}

impl Default for SwarmState {
    fn default() -> Self {
        Self {
            shared: Arc::new(SharedRuntime::new(RunConfig::default())),
        }
    }
}

/// 在 Azalea 自己的 ECS schedule 内发送退出消息，避免跨任务直接写消息时
/// 与 Bevy 的双缓冲消息更新时序竞争。
struct RuntimeShutdownPlugin;

/// 只从 Azalea 的底层接收包消息中筛选服务端位置校正。
///
/// Azalea 的 `packet-event` feature 会把每一个游戏包再转发到高层
/// `LocalPlayerEvents` unbounded channel；对带区块流量的 26.1 服务器而言，
/// 这会制造无意义的积压。自有插件直接读取同一条 ECS message，只保留
/// `ClientboundPlayerPosition` 这一条 M4 需要的服务端事实。
struct ServerPositionCorrectionPlugin;

impl Plugin for ServerPositionCorrectionPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                record_server_position_corrections,
                reset_spawn_marker_on_world_loaded,
            ),
        );
        app.add_observer(record_respawn_packet);
    }
}

/// Azalea 的 `Spawn` 去重标记只在 `Login` 时清除；26.1 的跨维度/重生包走
/// `WorldLoadedEvent`，如果保留旧标记，新维度的区块加载不会再产生 Spawn。
/// 重置这两个加载边界后，下一批区块会重新进入标准 Spawn 处理，避免在这里
/// 复制一套快照或生命周期逻辑。
fn reset_spawn_marker_on_world_loaded(
    mut world_loaded: MessageReader<azalea::packet::game::WorldLoadedEvent>,
    mut commands: Commands,
    state: Res<SwarmState>,
) {
    for event in world_loaded.read() {
        state.shared.observe_dimension(event.name.to_string());
        commands.entity(event.entity).remove::<(
            azalea::events::SentSpawnEvent,
            azalea::entity::InLoadedChunk,
        )>();
    }
}

fn record_respawn_packet(
    trigger: On<azalea::packet::game::SendGamePacketEvent>,
    state: Res<SwarmState>,
) {
    let azalea::protocol::packets::game::ServerboundGamePacket::ClientCommand(packet) =
        &trigger.event().packet
    else {
        return;
    };
    if !matches!(
        packet.action,
        azalea::protocol::packets::game::s_client_command::Action::PerformRespawn
    ) {
        return;
    }
    // 这是本地明确请求的重生过渡；只有后续 Spawn 才算服务端确认。
    let from_dimension = state
        .shared
        .writer
        .lock()
        .dimension
        .clone()
        .unwrap_or_else(|| "unknown".to_owned());
    state.shared.emit_respawn_transition_started(from_dimension);
}

fn record_server_position_corrections(
    mut packets: MessageReader<azalea::packet::game::ReceiveGamePacketEvent>,
    state: Res<SwarmState>,
) {
    for event in packets.read() {
        let azalea::protocol::packets::game::ClientboundGamePacket::PlayerPosition(packet) =
            event.packet.as_ref()
        else {
            continue;
        };
        // 这是服务端主动校正玩家位置的协议事实；它不代表每个 tick
        // 都有一个服务端坐标包，因此客户端预测轨迹仍单独记录。
        state.shared.emit_if_running(
            FactSource::ServerObserved,
            BackendEventPayload::SelfState(ContractProtocolSelfEvent::ServerPositionCorrection {
                teleport_id: packet.id,
                position: ContractVec3Value {
                    x: packet.change.pos.x,
                    y: packet.change.pos.y,
                    z: packet.change.pos.z,
                },
                velocity: ContractVec3Value {
                    x: packet.change.delta.x,
                    y: packet.change.delta.y,
                    z: packet.change.delta.z,
                },
                yaw: packet.change.look_direction.y_rot(),
                pitch: packet.change.look_direction.x_rot(),
                relative: RelativeMovementFlags {
                    x: packet.relative.x,
                    y: packet.relative.y,
                    z: packet.relative.z,
                    yaw: packet.relative.y_rot,
                    pitch: packet.relative.x_rot,
                    delta_x: packet.relative.delta_x,
                    delta_y: packet.relative.delta_y,
                    delta_z: packet.relative.delta_z,
                    rotate_delta: packet.relative.rotate_delta,
                },
            }),
        );
    }
}

impl Plugin for RuntimeShutdownPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, emit_app_exit_when_stopping);
        // 死亡后冻结本地物理状态必须在 Azalea 的常规 Update 查询完成后执行，
        // 否则会与更新碰撞盒、准星命中结果的系统产生无序写入警告。
        app.add_systems(PostUpdate, freeze_dead_local_player);
    }
}

/// 禁用自动重生时，死亡是一个需要保持的事实；冻结本地物理，避免死后
/// 因客户端重力继续把观察位置推进到世界边界之外。
fn freeze_dead_local_player(
    mut query: Query<(&mut Physics, &Position), (With<LocalEntity>, With<Dead>)>,
) {
    for (mut physics, position) in &mut query {
        physics.velocity = azalea::Vec3::ZERO;
        physics.set_on_ground(true);
        physics.set_old_pos(*position);
    }
}

fn emit_app_exit_when_stopping(mut app_exit: MessageWriter<AppExit>, state: Res<SwarmState>) {
    if state.shared.stopping.load(Ordering::Acquire) {
        app_exit.write(AppExit::Success);
    }
}

fn direction_for(directions: &[MotorDirection]) -> WalkDirection {
    let forward = directions.contains(&MotorDirection::Forward);
    let back = directions.contains(&MotorDirection::Back);
    let left = directions.contains(&MotorDirection::Left);
    let right = directions.contains(&MotorDirection::Right);
    match (forward, back, left, right) {
        (true, false, true, false) => WalkDirection::ForwardLeft,
        (true, false, false, true) => WalkDirection::ForwardRight,
        (false, true, true, false) => WalkDirection::BackwardLeft,
        (false, true, false, true) => WalkDirection::BackwardRight,
        (true, false, false, false) => WalkDirection::Forward,
        (false, true, false, false) => WalkDirection::Backward,
        (false, false, true, false) => WalkDirection::Left,
        (false, false, false, true) => WalkDirection::Right,
        _ => WalkDirection::None,
    }
}

fn sprint_direction(direction: WalkDirection) -> Option<SprintDirection> {
    match direction {
        WalkDirection::Forward => Some(SprintDirection::Forward),
        WalkDirection::ForwardLeft => Some(SprintDirection::ForwardLeft),
        WalkDirection::ForwardRight => Some(SprintDirection::ForwardRight),
        _ => None,
    }
}

/// 断线时本地玩家实体可能已经被 Azalea 移除；运动清理必须使用可失败查询。
fn try_set_movement_flags(bot: &Client, jumping: bool, crouching: bool) -> bool {
    bot.try_query_self::<(&mut azalea::entity::Jumping, &mut azalea::PhysicsState), _>(
        |(mut jumping_component, mut physics)| {
            **jumping_component = jumping;
            physics.trying_to_crouch = crouching;
        },
    )
    .is_ok()
}

fn finish_command(
    completion: &Option<Arc<CommandCompletionState>>,
    result: Result<(), BackendError>,
) {
    if let Some(completion) = completion {
        completion.finish(result);
    }
}

fn reject_command_after_stop(
    shared: &Arc<SharedRuntime>,
    command_id: &str,
    completion: &Option<Arc<CommandCompletionState>>,
) -> bool {
    if shared.command_execution_allowed() {
        return false;
    }
    finish_command(
        completion,
        Err(BackendError::Cancelled {
            operation: format!("command:{command_id}"),
        }),
    );
    true
}

fn command_component_failure(operation: &str) -> BackendError {
    BackendError::BackendFailure {
        failure: BackendFailure {
            code: BackendFailureCode::ProtocolError,
            message: format!("{operation} requires an active local player"),
            retryable: true,
        },
    }
}

/// Release one active Move and settle its completion only after the physical
/// release attempt and the shared active-state cleanup have both completed.
/// The actuator is injected so the ordering seam is testable without creating
/// an Azalea client; production supplies the real walk/flag release closure.
fn release_active_movement_and_finish(
    shared: &Arc<SharedRuntime>,
    command_id: &str,
    generation: u64,
    completion: &Option<Arc<CommandCompletionState>>,
    release_inputs: impl FnOnce() -> bool,
    failure_operation: &str,
    result_if_released: Result<(), BackendError>,
) {
    let released = {
        let _admission = shared.command_admission.lock();
        let owns_movement = shared.movement_generation.load(Ordering::Acquire) == generation
            && shared.active_movement_id.lock().as_deref() == Some(command_id);
        if !owns_movement {
            return;
        }
        // Serialize the physical release with command admission.  This is a
        // short synchronous actuator section; completion settlement and stop
        // finalization remain outside the lock.
        release_inputs()
    };
    let result = if released {
        result_if_released
    } else {
        Err(command_component_failure(failure_operation))
    };
    // Keep the active completion/id visible to stop until the physical release
    // result has been settled. A stop racing this section must defer stopped;
    // the generation/id checks below prevent an old task from clearing a new
    // movement that was admitted after its release.
    finish_command(completion, result);
    {
        let _admission = shared.command_admission.lock();
        if shared.clear_registered_active_movement(command_id, generation, &None, completion) {
            shared.active_movement_cancel_signal.lock().take();
        }
    }
    shared.finalize_stop_if_ready();
}

fn handle_command(bot: &Client, shared: &Arc<SharedRuntime>, queued: QueuedCommand) {
    let QueuedCommand {
        envelope,
        completion,
    } = queued;
    let command_id = envelope.id;
    if completion
        .as_ref()
        .is_some_and(|completion| completion.cancelled.load(Ordering::Acquire))
    {
        return;
    }
    if reject_command_after_stop(shared, &command_id, &completion) {
        return;
    }
    match envelope.command {
        BackendCommand::SendChat { message } => {
            match shared.with_command_admission(|| bot.chat(message)) {
                Ok(()) => finish_command(&completion, Ok(())),
                Err(()) => {
                    finish_command(
                        &completion,
                        Err(BackendError::Cancelled {
                            operation: format!("command:{command_id}"),
                        }),
                    );
                }
            }
        }
        BackendCommand::LookRelative {
            yaw_degrees,
            pitch_degrees,
        } => {
            let result = shared.with_command_admission(|| {
                let direction = bot.direction();
                bot.set_direction(
                    direction.y_rot() - yaw_degrees,
                    (direction.x_rot() - pitch_degrees).clamp(-90.0, 90.0),
                );
            });
            match result {
                Ok(()) => finish_command(&completion, Ok(())),
                Err(()) => finish_command(
                    &completion,
                    Err(BackendError::Cancelled {
                        operation: format!("command:{command_id}"),
                    }),
                ),
            }
        }
        BackendCommand::Move {
            directions,
            duration_ms,
            sprint,
            jump,
            crouch,
        } => {
            shared.cancel_active_movement(false);
            let direction = direction_for(&directions);
            let generation = shared.movement_generation.fetch_add(1, Ordering::AcqRel) + 1;
            let registration =
                shared.register_active_movement(&command_id, generation, duration_ms, &completion);
            let ActiveMovementRegistration::Started { cancel_signal } = registration else {
                return;
            };

            // The cancellation/generation check and the first actuator call
            // share one admission point. A cancellation that wins cannot
            // touch the bot; an actuator that wins leaves the same generation
            // for the release task to clean up.
            let actuator_result =
                shared.with_active_movement_admission(&command_id, generation, &completion, || {
                    if sprint.unwrap_or(false) {
                        if let Some(sprint_direction) = sprint_direction(direction) {
                            bot.sprint(sprint_direction);
                        } else {
                            bot.walk(direction);
                        }
                    } else {
                        bot.walk(direction);
                    }
                    if !try_set_movement_flags(bot, jump.unwrap_or(false), crouch.unwrap_or(false))
                    {
                        bot.walk(WalkDirection::None);
                        return false;
                    }
                    if duration_ms == 0 {
                        bot.walk(WalkDirection::None);
                    }
                    true
                });
            let started = match actuator_result {
                Ok(started) => started,
                Err(()) => {
                    shared.cancel_registered_active_movement(
                        &command_id,
                        generation,
                        &cancel_signal,
                        &completion,
                    );
                    return;
                }
            };

            if !started {
                shared.clear_registered_active_movement(
                    &command_id,
                    generation,
                    &cancel_signal,
                    &completion,
                );
                finish_command(&completion, Err(command_component_failure("move")));
                shared.finish_active_movement_registration();
                return;
            }

            if duration_ms == 0 {
                shared.clear_registered_active_movement(
                    &command_id,
                    generation,
                    &cancel_signal,
                    &completion,
                );
                finish_command(&completion, Ok(()));
                shared.finish_active_movement_registration();
            } else {
                let cancel_signal = cancel_signal.expect("duration-positive move signal");
                let bot_to_stop = bot.clone();
                let shared = shared.clone();
                let task_shared = shared.clone();
                let completion_for_task = completion.clone();
                tokio::task::spawn_local(async move {
                    let duration = tokio::time::sleep(Duration::from_millis(duration_ms));
                    tokio::pin!(duration);
                    tokio::select! {
                        _ = &mut duration => {
                            let cancelled = completion_for_task
                                .as_ref()
                                .is_some_and(|completion| completion.cancelled.load(Ordering::Acquire))
                                || task_shared.stopping.load(Ordering::Acquire);
                            release_active_movement_and_finish(
                                &task_shared,
                                &command_id,
                                generation,
                                &completion_for_task,
                                || {
                                    let released = try_set_movement_flags(&bot_to_stop, false, false);
                                    bot_to_stop.walk(WalkDirection::None);
                                    released
                                },
                                "move release",
                                if cancelled {
                                    Err(BackendError::Cancelled {
                                        operation: format!("command:{command_id}"),
                                    })
                                } else {
                                    Ok(())
                                },
                            );
                        }
                        _ = cancel_signal.notified() => {
                            release_active_movement_and_finish(
                                &task_shared,
                                &command_id,
                                generation,
                                &completion_for_task,
                                || {
                                    let released = try_set_movement_flags(&bot_to_stop, false, false);
                                    bot_to_stop.walk(WalkDirection::None);
                                    released
                                },
                                "cancel move",
                                Err(BackendError::Cancelled {
                                    operation: format!("command:{command_id}"),
                                }),
                            );
                        }
                    }
                });
                shared.finish_active_movement_registration();
            }
        }
        BackendCommand::ReleaseAll => {
            let previous_id = shared.active_movement_id.lock().clone();
            let previous_generation = shared.movement_generation.load(Ordering::Acquire);
            let previous_completion = shared
                .cancel_active_movement(true)
                .map(Some)
                .unwrap_or(None);
            if let Some(previous_id) = previous_id {
                release_active_movement_and_finish(
                    shared,
                    &previous_id,
                    previous_generation,
                    &previous_completion,
                    || {
                        let released = try_set_movement_flags(bot, false, false);
                        bot.walk(WalkDirection::None);
                        released
                    },
                    "release_all move",
                    Err(BackendError::Cancelled {
                        operation: format!("command:{previous_id}"),
                    }),
                );
            } else {
                let released = match shared.with_command_admission(|| {
                    let released = try_set_movement_flags(bot, false, false);
                    bot.walk(WalkDirection::None);
                    released
                }) {
                    Ok(released) => released,
                    Err(()) => {
                        finish_command(
                            &completion,
                            Err(BackendError::Cancelled {
                                operation: format!("command:{command_id}"),
                            }),
                        );
                        return;
                    }
                };
                shared.clear_idle_movement_state(previous_generation);
                finish_command(
                    &previous_completion,
                    if released {
                        Err(BackendError::Cancelled {
                            operation: "movement released by release_all".to_owned(),
                        })
                    } else {
                        Err(command_component_failure("release_all move"))
                    },
                );
            }
            match shared.with_command_admission(|| {
                let released = try_set_movement_flags(bot, false, false);
                bot.walk(WalkDirection::None);
                released
            }) {
                Ok(true) => finish_command(&completion, Ok(())),
                Ok(false) => {
                    finish_command(&completion, Err(command_component_failure("release_all")))
                }
                Err(()) => finish_command(
                    &completion,
                    Err(BackendError::Cancelled {
                        operation: format!("command:{command_id}"),
                    }),
                ),
            }
        }
        BackendCommand::Respawn => {
            // 服务端的死亡包与 waitingForRespawn 状态可能跨一个网络 tick；
            // 只延迟这一条已经明确请求的动作，避免请求在服务端状态切换前到达。
            // 仍走 Azalea 自带 RespawnPlugin 的消息链，保持实体绑定和 ECS 时序。
            let delayed_bot = bot.clone();
            let delayed_shared = shared.clone();
            tokio::task::spawn_local(async move {
                tokio::time::sleep(RESPAWN_SETTLE_DELAY).await;
                let _ = delayed_shared.with_command_admission(|| {
                    if delayed_bot
                        .try_query_self::<&LocalEntity, _>(|_| ())
                        .is_err()
                    {
                        return;
                    }
                    delayed_bot
                        .ecs
                        .write()
                        .write_message(azalea::respawn::PerformRespawnEvent {
                            entity: delayed_bot.entity,
                        });
                });
            });
            finish_command(&completion, Ok(()));
        }
    }
}

fn process_pending_commands(bot: &Client, shared: &Arc<SharedRuntime>) {
    // 连接建立前的命令保留在队列中，避免把 chat/motor 静默丢在握手阶段。
    if !bot.logged_in() {
        return;
    }
    while let Some(command) = shared.next_command_for_processing() {
        handle_command(bot, shared, command);
    }
}

async fn handle_client(bot: Client, event: Event, state: BotState) {
    let shared = &state.shared;
    if matches!(event, Event::Spawn | Event::Tick) {
        if !shared.command_execution_allowed() {
            return;
        }
        process_pending_commands(&bot, &state.shared);
    }
    match event {
        Event::Init => {
            // Swarm 重连在某些路径复用已有本地玩家事件发送器，不一定再次发出
            // Event::Init；重连调度器会预留 epoch，若 Init 到达则消费该预留，避免
            // 同一次握手被错误地记成两个 epoch。
            if !shared.consume_attempt_for_transport_init() || !shared.command_execution_allowed() {
                return;
            }
            shared.emit_transport_connected();
        }
        Event::Login => {
            if !shared.command_execution_allowed() {
                return;
            }
            let dimension = bot
                .try_query_self::<Option<&azalea::world::WorldName>, _>(|world_name| {
                    world_name
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "minecraft:overworld".to_owned())
                })
                .unwrap_or_else(|_| "minecraft:overworld".to_owned());
            if !shared.command_execution_allowed() {
                return;
            }
            shared.emit_logged_in("26.1.2", dimension);
        }
        Event::Spawn => {
            if !shared.command_execution_allowed() {
                return;
            }
            let (spawn_allowed, was_dead) = {
                let _admission = shared.command_admission.lock();
                let was_dead = shared.death_reported.load(Ordering::Acquire);
                if !shared.command_execution_allowed_without_lock() {
                    (false, was_dead)
                } else {
                    shared.ready.store(true, Ordering::Release);
                    shared.death_reported.store(false, Ordering::Release);
                    (true, was_dead)
                }
            };
            if !spawn_allowed {
                return;
            }
            if !shared.set_world_if_running(bot.world()) {
                return;
            }
            let snapshot = shared.refresh_snapshot(&bot, true, FactSource::ServerObserved);
            if let Some(snapshot) = snapshot.as_ref() {
                if !shared.set_dimension_if_running(snapshot.world.dimension.clone()) {
                    return;
                }
            }
            if let Some(snapshot) = snapshot {
                if was_dead {
                    shared.emit_respawned(snapshot.world.dimension.clone());
                }
                shared.emit_ready(snapshot.snapshot_revision);
                shared.emit_snapshot(snapshot, FactSource::ServerObserved);
            } else {
                shared.emit_ready(shared.snapshot_revision.load(Ordering::Acquire));
            }

            let _ = shared.with_command_admission(|| {
                if !shared.initial_chat_sent.swap(true, Ordering::AcqRel) {
                    if let Some(message) = shared.config.initial_chat.clone() {
                        bot.chat(message);
                    }
                }
            });

            let start_timer = {
                let _admission = shared.command_admission.lock();
                shared.command_execution_allowed_without_lock()
                    && !shared.timer_started.swap(true, Ordering::AcqRel)
            };
            if start_timer && shared.config.auto_stop {
                let duration = shared.config.duration;
                let shared = state.shared.clone();
                tokio::task::spawn_local(async move {
                    tokio::time::sleep(duration).await;
                    shared.initiate_stop("duration_elapsed");
                });
            }
        }
        Event::KeepAlive(_id) => {}
        Event::Chat(packet) => {
            shared.emit_if_running(
                FactSource::ServerObserved,
                BackendEventPayload::Chat(ContractProtocolChatEvent {
                    sender_username: packet.sender(),
                    plain_text: packet.content(),
                    position: Some(ChatPosition::Chat),
                    verified: None,
                }),
            );
        }
        Event::Death(_) => {
            if shared
                .admit_death_and_release(|| {
                    let released = try_set_movement_flags(&bot, false, false);
                    bot.walk(WalkDirection::None);
                    released
                })
                .is_none()
            {
                return;
            }
            if let Some(snapshot) = shared.refresh_snapshot(&bot, true, FactSource::ServerObserved)
            {
                shared.emit_snapshot(snapshot, FactSource::ServerObserved);
            }
        }
        Event::Disconnect(reason) => {
            let reason = reason.map(|value| value.to_string());
            shared.mark_disconnected(reason);
            // Disconnect 会由 Azalea 同步移除本地玩家的运动组件；此处只
            // 更新运行时状态，不再向已失效的实体投递 walk/jump/crouch 消息。
        }
        Event::ConnectionFailed(error) => {
            shared.mark_connection_failed(format!("{error:?}"));
            // ConnectionFailed 不一定伴随单独的 swarm disconnect；显式断开让
            // 统一的 close/reconnect 分支接管，不把内部错误泄漏成旧 error kind。
            let _ = shared.with_disconnect_admission(|| bot.disconnect());
        }
        Event::AddPlayer(info) => {
            shared.emit_if_running(
                FactSource::ServerObserved,
                BackendEventPayload::PlayerList(ContractProtocolPlayerListEvent::Add {
                    uuid: info.uuid.to_string(),
                    username: info.profile.name,
                }),
            );
        }
        Event::RemovePlayer(info) => {
            shared.emit_if_running(
                FactSource::ServerObserved,
                BackendEventPayload::PlayerList(ContractProtocolPlayerListEvent::Remove {
                    uuid: info.uuid.to_string(),
                    username: info.profile.name,
                }),
            );
        }
        Event::UpdatePlayer(info) => {
            shared.emit_if_running(
                FactSource::ServerObserved,
                BackendEventPayload::PlayerList(ContractProtocolPlayerListEvent::Update {
                    uuid: info.uuid.to_string(),
                    username: info.profile.name,
                }),
            );
        }
        Event::ReceiveChunk(position) => {
            shared.emit_if_running(
                FactSource::ServerObserved,
                BackendEventPayload::Block(ContractProtocolBlockEvent::ChunkLoaded {
                    chunk_x: position.x,
                    chunk_z: position.z,
                }),
            );
        }
        Event::Tick => {
            if shared.command_execution_allowed() && shared.ready.load(Ordering::Acquire) {
                let tick = shared.tick_revision.fetch_add(1, Ordering::AcqRel);
                if tick % 5 != 0 {
                    return;
                }
                if let Some(snapshot) =
                    shared.refresh_snapshot(&bot, false, FactSource::ClientPredicted)
                {
                    // Tick 中的 Position/Physics 可能是 Azalea 本地物理预测；
                    // 不把它作为服务端事实发出，服务端事件仍单独保留为 observed。
                    shared.emit_snapshot(snapshot, FactSource::ClientPredicted);
                }
            }
        }
        _ => {}
    }
}

async fn handle_swarm(swarm: Swarm, event: SwarmEvent, state: SwarmState) {
    let shared = state.shared;
    if matches!(event, SwarmEvent::Init) {
        if !shared.set_swarm(swarm.clone()) {
            return;
        }
    }
    if let SwarmEvent::Disconnect(account, join_opts) = event {
        if !shared.claim_reconnect() {
            return;
        }
        if shared.stopping.load(Ordering::Acquire) {
            shared.reconnect_pending.store(false, Ordering::Release);
            return;
        }

        // SwarmEvent::Disconnect 是重连状态机的兜底边界：azalea 在复用
        // LocalPlayerEvents 时可能没有再发出 Event::Disconnect。
        let close = shared.mark_disconnected(None);
        if shared.stopping.load(Ordering::Acquire) || close.deliberate {
            shared.reconnect_pending.store(false, Ordering::Release);
            return;
        }
        if !close.retryable || !shared.config.reconnect_enabled {
            shared.emit_faulted(shared.failure_for_close(&close));
            shared.request_shutdown();
            shared.reconnect_pending.store(false, Ordering::Release);
            return;
        }
        let Some(delay) = shared.emit_reconnect_scheduled(&close) else {
            shared.finish_reconnect_attempt(0);
            return;
        };
        let reconnect_cancel = shared.reconnect_cancel.clone();
        tokio::task::spawn_local(async move {
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = reconnect_cancel.notified() => {
                    shared.finish_reconnect_attempt(0);
                    return;
                }
            }
            let Some(token) = shared.admit_reconnect_attempt() else {
                shared.finish_reconnect_attempt(0);
                return;
            };
            if !shared.reconnect_add_is_allowed(token) {
                shared.finish_reconnect_attempt(token);
                return;
            }
            let state = BotState {
                shared: shared.clone(),
            };
            // Do not drop add_with_opts after its first poll: it may already
            // have started Client::start_client. Stop invalidates the token
            // and exits the swarm; once this future returns, an invalid token
            // explicitly disconnects the returned client as the final guard.
            let client = swarm.add_with_opts(&account, state, &join_opts).await;
            if !shared.reconnect_add_is_allowed(token) {
                client.disconnect();
            }
            shared.finish_reconnect_attempt(token);
        });
    }
}

/// 启动 M1 连接/登录事件流，并在真实断线后按自有状态机再次加入。
pub async fn run(config: RunConfig) -> Result<(), Box<dyn Error + Send + Sync>> {
    let handle = RuntimeHandle::new(config.clone());
    run_with_handle(handle, config).await
}

/// 使用外部句柄启动运行时，供主仓库适配层调用 `snapshot/subscribe/motor/sendChat`。
pub async fn run_with_handle(
    handle: RuntimeHandle,
    config: RunConfig,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    validate_run_config(&config)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let shared = handle.shared.clone();
    if !shared.begin_connection_attempt() {
        return Ok(());
    }
    let account = Account::offline(&config.username);
    let socket: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    let address = ResolvedAddr {
        server: ServerAddr::from(socket),
        socket,
    };
    let shutdown = shared.shutdown.clone();
    let bot_state = BotState {
        shared: shared.clone(),
    };
    let swarm_state = SwarmState { shared };
    let plugins = (
        DefaultPlugins.build(),
        DefaultBotPlugins
            .build()
            .disable::<AutoRespawnPlugin>()
            .disable::<AcceptResourcePacksPlugin>()
            .disable::<AutoReconnectPlugin>(),
        ServerPositionCorrectionPlugin,
        RuntimeShutdownPlugin,
        DefaultSwarmPlugins,
    );
    let start = SwarmBuilder::new_without_plugins()
        .add_plugins(plugins)
        .set_handler(handle_client)
        .set_swarm_handler(handle_swarm)
        .set_swarm_state(swarm_state)
        .add_account_with_state(account, bot_state)
        .reconnect_after(None)
        .start(&address);
    tokio::select! {
        _ = start => {}
        _ = shutdown.notified() => {
            // 先让 SwarmBuilder 自己尝试清理；若其内部仍在等待 AppExit，
            // 丢弃 start future 后由 Tokio runtime 回收剩余任务。
        }
    }
    Ok(())
}

fn validate_run_config(config: &RunConfig) -> Result<(), String> {
    if config.host.trim().is_empty() {
        return Err("服务器 host 不能为空".to_owned());
    }
    if config.port == 0 {
        return Err("服务器 port 不能为 0".to_owned());
    }
    if config.username.is_empty()
        || config.username.len() > 16
        || !config
            .username
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err("offline 用户名必须是 1–16 个 ASCII 字母、数字或下划线".to_owned());
    }
    if config.world_id.trim().is_empty() {
        return Err("world_id 不能为空".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        future::pending,
        sync::{atomic::AtomicUsize, mpsc as std_mpsc, Barrier, Condvar, Mutex as StdMutex},
        thread,
        time::Duration as StdDuration,
    };

    use super::*;
    use crate::snapshot::{ExperienceSnapshot, InventorySnapshot, SelfSnapshot, WorldSnapshot};
    use mineintent_contracts::minecraft::{
        BackendEventProtocol as ContractBackendEventProtocol,
        BlockPropertyValue as ContractBlockPropertyValue, CancellationSignal, Deadline,
        HeardSoundType as ContractHeardSoundType,
        ProtocolEntityEvent as ContractProtocolEntityEvent,
        ProtocolSoundPayload as ContractProtocolSoundPayload,
        ProtocolSoundSource as ContractProtocolSoundSource,
        ProtocolWorldEvent as ContractProtocolWorldEvent,
    };

    struct TestCancellation {
        checks: AtomicUsize,
        trigger_at: Option<usize>,
        cancel_on_trigger: bool,
        cancelled: AtomicBool,
        triggered: AtomicBool,
        action: Option<Arc<dyn Fn() + Send + Sync>>,
    }

    impl TestCancellation {
        fn new(
            initially_cancelled: bool,
            trigger_at: Option<usize>,
            cancel_on_trigger: bool,
            action: Option<Arc<dyn Fn() + Send + Sync>>,
        ) -> Arc<Self> {
            Arc::new(Self {
                checks: AtomicUsize::new(0),
                trigger_at,
                cancel_on_trigger,
                cancelled: AtomicBool::new(initially_cancelled),
                triggered: AtomicBool::new(false),
                action,
            })
        }
    }

    impl CancellationSignal for TestCancellation {
        fn is_cancelled(&self) -> bool {
            let check = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
            if self.trigger_at == Some(check) && !self.triggered.swap(true, Ordering::SeqCst) {
                if let Some(action) = &self.action {
                    action();
                }
                if self.cancel_on_trigger {
                    self.cancelled.store(true, Ordering::SeqCst);
                }
            }
            self.cancelled.load(Ordering::SeqCst)
        }

        fn cancelled(&self) -> BoxFuture<'_, ()> {
            Box::pin(pending())
        }
    }

    struct TestDeadline {
        checks: AtomicUsize,
        trigger_at: Option<usize>,
        elapsed: AtomicBool,
        triggered: AtomicBool,
        action: Option<Arc<dyn Fn() + Send + Sync>>,
    }

    impl TestDeadline {
        fn new(
            initially_elapsed: bool,
            trigger_at: Option<usize>,
            action: Option<Arc<dyn Fn() + Send + Sync>>,
        ) -> Arc<Self> {
            Arc::new(Self {
                checks: AtomicUsize::new(0),
                trigger_at,
                elapsed: AtomicBool::new(initially_elapsed),
                triggered: AtomicBool::new(false),
                action,
            })
        }
    }

    impl Deadline for TestDeadline {
        fn has_elapsed(&self) -> bool {
            let check = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
            if self.trigger_at == Some(check) && !self.triggered.swap(true, Ordering::SeqCst) {
                if let Some(action) = &self.action {
                    action();
                }
                self.elapsed.store(true, Ordering::SeqCst);
            }
            self.elapsed.load(Ordering::SeqCst)
        }

        fn elapsed(&self) -> BoxFuture<'_, ()> {
            Box::pin(pending())
        }
    }

    struct WorkerWakeCancellation {
        checks: AtomicUsize,
        started: Notify,
        worker_started: AtomicBool,
        cancelled: AtomicBool,
        wake: Arc<(StdMutex<()>, Condvar)>,
    }

    impl WorkerWakeCancellation {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                checks: AtomicUsize::new(0),
                started: Notify::new(),
                worker_started: AtomicBool::new(false),
                cancelled: AtomicBool::new(false),
                wake: Arc::new((StdMutex::new(()), Condvar::new())),
            })
        }
    }

    impl CancellationSignal for WorkerWakeCancellation {
        fn is_cancelled(&self) -> bool {
            let check = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
            if check == 4 {
                self.worker_started.store(true, Ordering::SeqCst);
                self.started.notify_one();
                let (lock, wake) = &*self.wake;
                let guard = lock.lock().expect("worker wake lock");
                if !self.cancelled.load(Ordering::SeqCst) {
                    let _ = wake
                        .wait_timeout(guard, StdDuration::from_millis(100))
                        .expect("worker wake wait");
                }
            }
            self.cancelled.load(Ordering::SeqCst)
        }

        fn cancelled(&self) -> BoxFuture<'_, ()> {
            Box::pin(async move {
                self.started.notified().await;
                self.cancelled.store(true, Ordering::SeqCst);
                self.wake.1.notify_all();
            })
        }
    }

    struct WorkerWakeDeadline {
        checks: AtomicUsize,
        started: Notify,
        worker_started: AtomicBool,
        elapsed: AtomicBool,
        wake: Arc<(StdMutex<()>, Condvar)>,
    }

    impl WorkerWakeDeadline {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                checks: AtomicUsize::new(0),
                started: Notify::new(),
                worker_started: AtomicBool::new(false),
                elapsed: AtomicBool::new(false),
                wake: Arc::new((StdMutex::new(()), Condvar::new())),
            })
        }
    }

    impl Deadline for WorkerWakeDeadline {
        fn has_elapsed(&self) -> bool {
            let check = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
            if check == 4 {
                self.worker_started.store(true, Ordering::SeqCst);
                self.started.notify_one();
                let (lock, wake) = &*self.wake;
                let guard = lock.lock().expect("worker wake lock");
                if !self.elapsed.load(Ordering::SeqCst) {
                    let _ = wake
                        .wait_timeout(guard, StdDuration::from_millis(100))
                        .expect("worker wake wait");
                }
            }
            self.elapsed.load(Ordering::SeqCst)
        }

        fn elapsed(&self) -> BoxFuture<'_, ()> {
            Box::pin(async move {
                self.started.notified().await;
                self.elapsed.store(true, Ordering::SeqCst);
                self.wake.1.notify_all();
            })
        }
    }

    fn test_control(
        cancellation: Arc<TestCancellation>,
        deadline: Option<Arc<TestDeadline>>,
    ) -> OperationControl {
        OperationControl::new(
            cancellation,
            deadline.map(|value| value as Arc<dyn Deadline>),
        )
    }

    fn empty_world() -> SharedWorld {
        Arc::new(parking_lot::RwLock::new(azalea::world::World::default()))
    }

    fn install_viewport_observation(
        handle: &RuntimeHandle,
        snapshot: MinecraftSnapshotV1,
        source: FactSource,
        entities: Vec<ProtocolEntitySnapshot>,
        world: SharedWorld,
    ) {
        let mut observation = handle.shared.observation.write();
        observation.world = Some(world);
        observation.snapshot = Some(snapshot);
        observation.source = Some(source);
        observation.tracked_entities = entities;
        observation.bump_generation();
        handle.shared.ready.store(true, Ordering::Release);
    }

    fn ready_viewport_source() -> (RuntimeHandle, RuntimeObservationSource, SharedWorld) {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let world = empty_world();
        install_viewport_observation(
            &handle,
            observation_snapshot(1),
            FactSource::ServerObserved,
            vec![observation_entity("entity-7")],
            world.clone(),
        );
        let source = handle.observation_source();
        (handle, source, world)
    }

    #[test]
    fn command_validation_matches_motor_boundary() {
        assert!(validate_command(&BackendCommand::Move {
            directions: vec![MotorDirection::Forward, MotorDirection::Left],
            duration_ms: 1_500,
            sprint: Some(true),
            jump: Some(false),
            crouch: Some(false),
        })
        .is_ok());
        assert!(validate_command(&BackendCommand::Move {
            directions: vec![MotorDirection::Forward, MotorDirection::Forward],
            duration_ms: 100,
            sprint: None,
            jump: None,
            crouch: None,
        })
        .is_err());
        assert!(validate_command(&BackendCommand::Move {
            directions: vec![MotorDirection::Forward],
            duration_ms: 49,
            sprint: None,
            jump: None,
            crouch: None,
        })
        .is_err());
        assert!(validate_command(&BackendCommand::SendChat {
            message: "hello\nworld".to_owned(),
        })
        .is_err());
    }

    #[test]
    fn relative_look_validation_rejects_non_finite_angles() {
        assert!(validate_command(&BackendCommand::LookRelative {
            yaw_degrees: 90.0,
            pitch_degrees: -90.0,
        })
        .is_ok());
        assert!(validate_command(&BackendCommand::LookRelative {
            yaw_degrees: 90.1,
            pitch_degrees: 0.0,
        })
        .is_err());
        assert!(validate_command(&BackendCommand::LookRelative {
            yaw_degrees: f32::NAN,
            pitch_degrees: 0.0,
        })
        .is_err());
    }

    #[test]
    fn run_config_rejects_invalid_offline_username() {
        let mut config = RunConfig::default();
        config.username = "MineIntentUsernameTooLong".to_owned();
        assert!(validate_run_config(&config).is_err());
        config.username = "bad-name".to_owned();
        assert!(validate_run_config(&config).is_err());
        config.username = "MineM130Fresh".to_owned();
        assert!(validate_run_config(&config).is_ok());
    }

    #[tokio::test]
    async fn command_completion_seam_reports_fifo_success_failure_and_shutdown_cancel() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let first = handle
            .look_relative(10.0, -5.0)
            .expect("look command should enqueue");
        let second = handle
            .move_input(vec![MotorDirection::Forward], 100, None, None, None)
            .expect("move command should enqueue");
        let third = handle
            .release_all()
            .expect("release command should enqueue");
        assert_eq!(first.command_id(), "command-1");
        assert_eq!(second.command_id(), "command-2");
        assert_eq!(third.command_id(), "command-3");

        let first_queued = handle.shared.pop_command().expect("first queue item");
        let second_queued = handle.shared.pop_command().expect("second queue item");
        let third_queued = handle.shared.pop_command().expect("third queue item");
        assert_eq!(first_queued.envelope.id, "command-1");
        assert_eq!(second_queued.envelope.id, "command-2");
        assert_eq!(third_queued.envelope.id, "command-3");

        finish_command(&first_queued.completion, Ok(()));
        finish_command(
            &second_queued.completion,
            Err(BackendError::BackendFailure {
                failure: BackendFailure {
                    code: BackendFailureCode::ProtocolError,
                    message: "synthetic movement failure".to_owned(),
                    retryable: true,
                },
            }),
        );
        handle.shared.requeue_front(third_queued);
        handle.shared.cancel_pending_commands();

        assert_eq!(first.wait().await, Ok(()));
        assert!(matches!(
            second.wait().await,
            Err(BackendError::BackendFailure {
                failure: BackendFailure {
                    code: BackendFailureCode::ProtocolError,
                    ..
                }
            })
        ));
        assert!(matches!(
            third.wait().await,
            Err(BackendError::Cancelled { .. })
        ));
    }

    #[tokio::test]
    async fn runtime_stop_admission_linearizes_enqueue_and_settles_completion() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let producer_handle = handle.clone();
        let (result_tx, result_rx) = std_mpsc::channel();
        let producer = thread::spawn(move || {
            let result = producer_handle.look_relative(1.0, 2.0);
            result_tx
                .send(result)
                .expect("command producer result should be delivered");
        });

        for _ in 0..100_000 {
            if !handle.shared.commands.lock().is_empty() {
                break;
            }
            thread::yield_now();
        }
        assert!(
            !handle.shared.commands.lock().is_empty(),
            "producer must enqueue before stop wins the admission lock"
        );
        handle.stop("enqueue_race_stop");
        producer.join().expect("command producer should not panic");

        let completion = result_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("completion enqueue result");
        let completion = completion.expect("enqueue that won must return a completion");
        assert!(matches!(
            completion.wait().await,
            Err(BackendError::Cancelled { .. })
        ));
        assert!(handle.shared.commands.lock().is_empty());

        let stopped_first = RuntimeHandle::new(RunConfig::default());
        stopped_first.stop("already_stopped");
        assert!(stopped_first.send_chat("not queued").is_err());
        assert!(stopped_first.look_relative(0.0, 0.0).is_err());
        assert!(stopped_first.shared.commands.lock().is_empty());
    }

    #[test]
    fn global_stream_excludes_keep_alive_motor_error_and_commanded_chat() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();

        handle
            .send_chat("commanded chat stays local")
            .expect("commanded chat should enqueue locally");
        let _look = handle
            .look_relative(10.0, 5.0)
            .expect("look completion should enqueue locally");
        let _move = handle
            .move_input(vec![MotorDirection::Forward], 100, None, None, None)
            .expect("move completion should enqueue locally");
        let _release = handle
            .release_all()
            .expect("release completion should enqueue locally");

        assert!(events.try_recv().is_err());
        assert_eq!(handle.shared.commands.lock().len(), 4);
        assert_eq!(
            mineintent_contracts::minecraft::BackendEventKind::PRODUCT_KINDS.len(),
            9
        );
    }

    #[tokio::test]
    async fn runtime_stop_during_move_registration_cancels_before_any_actuator() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let (completion, state) = CommandCompletion::channel("command-registration".to_owned());
        let stop_handle = handle.clone();
        handle
            .shared
            .set_active_movement_registration_hook(Some(Arc::new(move || {
                stop_handle.stop("registration_stop")
            })));

        let generation = handle
            .shared
            .movement_generation
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        let registration = handle.shared.register_active_movement(
            "command-registration",
            generation,
            250,
            &Some(state.clone()),
        );

        assert!(matches!(
            registration,
            ActiveMovementRegistration::Cancelled
        ));
        assert!(!handle.shared.active_movement.load(Ordering::Acquire));
        assert!(handle.shared.active_movement_id.lock().is_none());
        assert!(handle.shared.active_movement_cancel_signal.lock().is_none());
        assert!(handle.shared.active_movement_completion.lock().is_none());
        assert!(!handle
            .shared
            .active_movement_registration
            .load(Ordering::Acquire));

        let closed = events.try_recv().expect("close precedes stopped");
        assert_eq!(payload_json(&closed)["type"], "connection_closed");
        let stopped = events
            .try_recv()
            .expect("stopped waits for registration cleanup");
        assert_eq!(payload_json(&stopped)["type"], "stopped");
        assert_eq!(payload_json(&stopped)["reason"], "registration_stop");
        assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
        assert!(matches!(
            completion.wait().await,
            Err(BackendError::Cancelled { .. })
        ));
    }

    #[test]
    fn runtime_stop_during_move_registration_without_completion_cleans_signal() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let stop_handle = handle.clone();
        handle
            .shared
            .set_active_movement_registration_hook(Some(Arc::new(move || {
                stop_handle.stop("registration_without_completion")
            })));
        let generation = handle
            .shared
            .movement_generation
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        let registration = handle.shared.register_active_movement(
            "command-registration-no-completion",
            generation,
            250,
            &None,
        );

        assert!(matches!(
            registration,
            ActiveMovementRegistration::Cancelled
        ));
        assert!(!handle.shared.active_movement.load(Ordering::Acquire));
        assert!(handle.shared.active_movement_id.lock().is_none());
        assert!(handle.shared.active_movement_cancel_signal.lock().is_none());
        assert!(handle.shared.active_movement_completion.lock().is_none());
        assert_eq!(
            payload_json(&events.try_recv().expect("close event"))["type"],
            "connection_closed"
        );
        assert_eq!(
            payload_json(&events.try_recv().expect("stopped event"))["type"],
            "stopped"
        );
        assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn active_move_completion_cancel_notifies_release_and_finishes_once() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let (completion, state) = CommandCompletion::channel("command-active".to_owned());
        let signal = Arc::new(Notify::new());
        state.begin_active_release(signal.clone());
        *handle.shared.active_movement_completion.lock() = Some(state.clone());
        handle.shared.active_movement.store(true, Ordering::Release);
        *handle.shared.active_movement_id.lock() = Some("command-active".to_owned());

        let release_attempted = Arc::new(AtomicBool::new(false));
        let release_saw_active = Arc::new(AtomicBool::new(false));
        let release_saw_pending_completion = Arc::new(AtomicBool::new(false));
        let release_attempted_by_task = release_attempted.clone();
        let release_saw_active_by_task = release_saw_active.clone();
        let release_saw_pending_by_task = release_saw_pending_completion.clone();
        let task_shared = handle.shared.clone();
        let task_state = state.clone();
        let release_waiter = tokio::spawn(async move {
            signal.notified().await;
            let completion_for_task = Some(task_state.clone());
            release_active_movement_and_finish(
                &task_shared,
                "command-active",
                0,
                &completion_for_task,
                || {
                    release_attempted_by_task.store(true, Ordering::Release);
                    release_saw_active_by_task.store(
                        task_shared.active_movement.load(Ordering::Acquire)
                            && task_shared.active_movement_id.lock().as_deref()
                                == Some("command-active"),
                        Ordering::Release,
                    );
                    release_saw_pending_by_task
                        .store(task_state.sender.lock().is_some(), Ordering::Release);
                    true
                },
                "cancel move",
                Err(BackendError::Cancelled {
                    operation: "command:command-active".to_owned(),
                }),
            );
        });

        completion.cancel();
        completion.cancel();
        assert!(
            state.sender.lock().is_some(),
            "active cancellation must defer completion until release"
        );
        tokio::time::timeout(StdDuration::from_secs(1), release_waiter)
            .await
            .expect("active movement cancellation must run the release seam")
            .expect("release waiter should not panic");

        assert!(release_attempted.load(Ordering::Acquire));
        assert!(release_saw_active.load(Ordering::Acquire));
        assert!(release_saw_pending_completion.load(Ordering::Acquire));
        assert!(!handle.shared.active_movement.load(Ordering::Acquire));
        assert!(handle.shared.active_movement_id.lock().is_none());
        assert!(state.sender.lock().is_none());
        assert!(matches!(
            completion.wait().await,
            Err(BackendError::Cancelled { .. })
        ));
    }

    #[test]
    fn active_move_cancellation_wins_first_actuator_admission() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let (completion, state) = CommandCompletion::channel("command-first-actuator".to_owned());
        let generation = handle
            .shared
            .movement_generation
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        let registration = handle.shared.register_active_movement(
            "command-first-actuator",
            generation,
            250,
            &Some(state.clone()),
        );
        assert!(matches!(
            registration,
            ActiveMovementRegistration::Started { .. }
        ));

        completion.cancel();
        let actuator_called = Arc::new(AtomicBool::new(false));
        let called = actuator_called.clone();
        let result = handle.shared.with_active_movement_admission(
            "command-first-actuator",
            generation,
            &Some(state.clone()),
            || {
                called.store(true, Ordering::Release);
            },
        );
        assert!(result.is_err());
        assert!(!actuator_called.load(Ordering::Acquire));
        let cancel_signal = handle.shared.active_movement_cancel_signal.lock().clone();
        handle.shared.cancel_registered_active_movement(
            "command-first-actuator",
            generation,
            &cancel_signal,
            &Some(state.clone()),
        );
        assert!(state.sender.lock().is_none());
    }

    #[test]
    fn not_ready_command_head_cannot_be_overtaken_by_a_later_enqueue() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let first = handle
            .look_relative(1.0, 2.0)
            .expect("first command should enqueue");
        let barrier = Arc::new(Barrier::new(2));
        let producer_started = Arc::new(AtomicBool::new(false));
        let producer_done = Arc::new(AtomicBool::new(false));
        let (producer_id_tx, producer_id_rx) = std_mpsc::channel();
        let producer_handle = handle.clone();
        let producer_barrier = barrier.clone();
        let producer_started_flag = producer_started.clone();
        let producer_done_flag = producer_done.clone();
        let producer = thread::spawn(move || {
            producer_barrier.wait();
            producer_started_flag.store(true, Ordering::Release);
            let second = producer_handle
                .release_all()
                .expect("concurrent later command should enqueue");
            producer_id_tx
                .send(second.command_id().to_owned())
                .expect("producer id should be delivered");
            producer_done_flag.store(true, Ordering::Release);
        });

        assert!(handle
            .shared
            .next_command_for_processing_with_hook(|| {
                barrier.wait();
                while !producer_started.load(Ordering::Acquire) {
                    thread::yield_now();
                }
                assert!(
                    !producer_done.load(Ordering::Acquire),
                    "producer must still be blocked by the queue lock"
                );
            })
            .is_none());
        producer
            .join()
            .expect("concurrent producer should not panic");
        let second_id = producer_id_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("later command id");

        let first_queued = handle.shared.pop_command().expect("deferred head");
        let second_queued = handle.shared.pop_command().expect("later queue item");
        assert_eq!(first_queued.envelope.id, first.command_id());
        assert_eq!(second_queued.envelope.id, second_id);
        handle.shared.requeue_front(second_queued);
        handle.shared.requeue_front(first_queued);
        handle.shared.cancel_pending_commands();
    }

    fn payload_json(event: &BackendEventEnvelope) -> serde_json::Value {
        serde_json::to_value(&event.payload).expect("strict v2 payload is serializable")
    }

    #[test]
    fn connection_request_preallocates_and_init_reuses_each_attempt_identity() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        assert!(events.try_recv().is_err());

        handle.shared.begin_connection_attempt();
        let first_request = events.try_recv().expect("首次连接请求事件");
        assert_eq!(first_request.connection_epoch, 1);
        assert_eq!(first_request.connection_attempt_id, "attempt-1");
        let first_payload = payload_json(&first_request);
        assert_eq!(first_payload["type"], "connection_requested");
        assert_eq!(first_payload["attempt"], 1);
        assert!(first_request.dimension.is_none());

        handle.shared.consume_attempt_for_transport_init();
        handle.shared.emit(
            FactSource::ServerObserved,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected),
        );
        let first_init = events.try_recv().expect("首次连接初始化事件");
        assert_eq!(
            first_init.process_session_id,
            first_request.process_session_id
        );
        assert_eq!(first_init.connection_epoch, 1);
        assert_eq!(first_init.connection_attempt_id, "attempt-1");

        handle.shared.begin_connection_attempt();
        let second_request = events.try_recv().expect("重连请求事件");
        assert_eq!(
            second_request.process_session_id,
            first_request.process_session_id
        );
        assert_eq!(second_request.connection_epoch, 2);
        assert_eq!(second_request.connection_attempt_id, "attempt-2");
        let second_payload = payload_json(&second_request);
        assert_eq!(second_payload["type"], "connection_requested");
        assert_eq!(second_payload["attempt"], 2);
        assert!(second_request.dimension.is_none());

        handle.shared.consume_attempt_for_transport_init();
        assert_eq!(handle.shared.context().1, 2);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn event_dimension_is_captured_at_emit_time_and_cleared_for_a_new_attempt() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();

        handle.shared.begin_connection_attempt();
        let first_request = events.try_recv().expect("首次连接请求事件");
        assert!(first_request.dimension.is_none());

        assert_eq!(handle.shared.set_dimension("minecraft:overworld"), None);
        handle.shared.emit(
            FactSource::ServerObserved,
            BackendEventPayload::World(ContractProtocolWorldEvent::GameChanged {
                dimension: Some("minecraft:overworld".to_owned()),
                game_mode: Some("survival".to_owned()),
            }),
        );
        let world_event = events.try_recv().expect("世界事件");
        assert_eq!(
            world_event.dimension.as_deref(),
            Some("minecraft:overworld")
        );

        handle.shared.begin_connection_attempt();
        let second_request = events.try_recv().expect("重连请求事件");
        assert_eq!(second_request.connection_epoch, 2);
        assert!(second_request.dimension.is_none());
    }

    #[test]
    fn dimension_changed_event_carries_the_new_dimension() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("连接请求事件");

        handle.shared.observe_dimension("minecraft:overworld");
        assert!(events.try_recv().is_err());
        handle.shared.observe_dimension("minecraft:the_nether");

        let changed = events.try_recv().expect("维度变化事件");
        let changed_payload = payload_json(&changed);
        assert_eq!(changed_payload["type"], "dimension_changed");
        assert_eq!(changed_payload["from"], "minecraft:overworld");
        assert_eq!(changed_payload["to"], "minecraft:the_nether");
        assert_eq!(changed.dimension.as_deref(), Some("minecraft:the_nether"));
    }

    #[test]
    fn runtime_ready_death_respawn_and_dimension_lifecycle_order_is_typed() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        handle.shared.emit_transport_connected();
        handle
            .shared
            .emit_logged_in("26.1.2", "minecraft:overworld".to_owned());
        handle.shared.emit_ready(7);
        handle.shared.emit_died();
        handle
            .shared
            .emit_respawn_transition_started("minecraft:overworld".to_owned());
        handle
            .shared
            .emit_respawned("minecraft:overworld".to_owned());
        handle.shared.observe_dimension("minecraft:the_nether");

        let emitted = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert_eq!(
            emitted
                .iter()
                .map(|event| payload_json(event)["type"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>(),
            vec![
                "transport_connected".to_owned(),
                "logged_in".to_owned(),
                "ready".to_owned(),
                "died".to_owned(),
                "respawn_transition_started".to_owned(),
                "respawned".to_owned(),
                "dimension_changed".to_owned(),
            ]
        );
        assert_eq!(payload_json(&emitted[1])["version"], "26.1.2");
        assert_eq!(
            payload_json(&emitted[1])["dimension"],
            "minecraft:overworld"
        );
        assert_eq!(payload_json(&emitted[2])["snapshotRevision"], 7);
        assert_eq!(
            payload_json(&emitted[4])["fromDimension"],
            "minecraft:overworld"
        );
        assert_eq!(
            payload_json(&emitted[5])["dimension"],
            "minecraft:overworld"
        );
        assert_eq!(payload_json(&emitted[6])["from"], "minecraft:overworld");
        assert_eq!(payload_json(&emitted[6])["to"], "minecraft:the_nether");
        assert_eq!(emitted[2].dimension.as_deref(), Some("minecraft:overworld"));
        assert_eq!(
            emitted[6].dimension.as_deref(),
            Some("minecraft:the_nether")
        );
    }

    #[test]
    fn lifecycle_state_timestamps_match_the_strict_v2_lifecycle_facts() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        handle.shared.emit_ready(7);
        let ready_event = events.try_recv().expect("ready event");
        let BackendState::Ready { ready_at, .. } = handle.state() else {
            panic!("ready state should be visible after ready admission");
        };
        assert_eq!(ready_at, ready_event.occurred_at);

        handle.shared.emit_died();
        let died_event = events.try_recv().expect("died event");
        let BackendState::Dead { died_at, .. } = handle.state() else {
            panic!("dead state should be visible after death admission");
        };
        assert_eq!(died_at, died_event.occurred_at);
    }

    #[test]
    fn stopped_runtime_rejects_late_attempt_and_ready_without_resurrection() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.stop("late_event_stop");
        let stopped = events.try_recv().expect("stopped event");
        assert_eq!(payload_json(&stopped)["type"], "stopped");
        assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));

        assert!(!handle.shared.begin_connection_attempt());
        assert!(!handle.shared.consume_attempt_for_transport_init());
        handle.shared.emit_transport_connected();
        handle
            .shared
            .emit_logged_in("26.1.2", "minecraft:overworld".to_owned());
        handle.shared.emit_ready(99);
        assert!(!handle.shared.ready.load(Ordering::Acquire));
        assert!(handle.shared.stopped_reported.load(Ordering::Acquire));
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn finalize_stop_ready_protocol_has_no_lost_wakeup_between_finalizers() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.stopping.store(true, Ordering::Release);
        *handle.shared.stop_reason.lock() = Some("lost_wakeup_regression".to_owned());

        let (hook_reached_tx, hook_reached_rx) = std_mpsc::channel();
        let (release_tx, release_rx) = std_mpsc::channel();
        let release_rx = Arc::new(StdMutex::new(Some(release_rx)));
        handle.shared.set_finalize_stop_hook(Some(Arc::new({
            let release_rx = release_rx.clone();
            move || {
                hook_reached_tx
                    .send(())
                    .expect("finalizer hook should be reached");
                release_rx
                    .lock()
                    .expect("release gate lock")
                    .take()
                    .expect("only the first finalizer owns the gate")
                    .recv()
                    .expect("release gate should open");
            }
        })));

        let first_shared = handle.shared.clone();
        let first = thread::spawn(move || first_shared.finalize_stop_if_ready());
        hook_reached_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("first finalizer must reach the readiness gate");

        let (second_attempt_tx, second_attempt_rx) = std_mpsc::channel();
        let (second_done_tx, second_done_rx) = std_mpsc::channel();
        let second_shared = handle.shared.clone();
        let second = thread::spawn(move || {
            second_attempt_tx
                .send(())
                .expect("second finalizer attempt should be observable");
            second_shared.finalize_stop_if_ready();
            second_done_tx
                .send(())
                .expect("second finalizer should finish");
        });
        second_attempt_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("second finalizer should attempt while first owns admission");
        assert!(
            second_done_rx.try_recv().is_err(),
            "second finalizer must wait instead of observing an empty reason"
        );

        release_tx.send(()).expect("release first finalizer");
        first.join().expect("first finalizer should not panic");
        second.join().expect("second finalizer should not panic");

        let stopped = events.try_recv().expect("stopped must not be lost");
        assert_eq!(payload_json(&stopped)["type"], "stopped");
        assert_eq!(payload_json(&stopped)["reason"], "lost_wakeup_regression");
        assert!(events.try_recv().is_err());
        assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
    }

    #[test]
    fn running_event_admission_enqueue_precedes_stop_terminal_event() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let (event_checked_tx, event_checked_rx) = std_mpsc::channel();
        let (release_tx, release_rx) = std_mpsc::channel();
        let release_rx = Arc::new(StdMutex::new(Some(release_rx)));
        handle.shared.set_event_admission_hook(Some(Arc::new({
            let release_rx = release_rx.clone();
            move || {
                event_checked_tx
                    .send(())
                    .expect("event admission hook should be reached");
                release_rx
                    .lock()
                    .expect("event release gate lock")
                    .take()
                    .expect("only one event admission owns the gate")
                    .recv()
                    .expect("event release gate should open");
            }
        })));

        let event_shared = handle.shared.clone();
        let emitter = thread::spawn(move || {
            event_shared.emit_if_running(
                FactSource::ServerObserved,
                BackendEventPayload::World(ContractProtocolWorldEvent::GameChanged {
                    dimension: Some("minecraft:overworld".to_owned()),
                    game_mode: Some("survival".to_owned()),
                }),
            )
        });
        event_checked_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("event must hold admission between check and enqueue");

        let (stop_attempt_tx, stop_attempt_rx) = std_mpsc::channel();
        let stop_handle = handle.clone();
        let stopper = thread::spawn(move || {
            stop_attempt_tx
                .send(())
                .expect("stop attempt should be observable");
            stop_handle.stop("event_admission_stop");
        });
        stop_attempt_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("stop must attempt while event owns admission");

        release_tx.send(()).expect("release event admission");
        assert!(emitter.join().expect("event emitter should not panic"));
        stopper.join().expect("stopper should not panic");

        let kinds = std::iter::from_fn(|| events.try_recv().ok())
            .map(|event| payload_json(&event)["type"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                "game_changed".to_owned(),
                "connection_closed".to_owned(),
                "stopped".to_owned()
            ]
        );
    }

    #[test]
    fn stop_cannot_overtake_first_disconnect_cleanup() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");
        handle.shared.observation.write().world = Some(empty_world());
        handle
            .send_chat("queued before disconnect")
            .expect("pre-disconnect command should queue");

        let (cleanup_reached_tx, cleanup_reached_rx) = std_mpsc::channel();
        let (release_cleanup_tx, release_cleanup_rx) = std_mpsc::channel();
        let release_cleanup_rx = Arc::new(StdMutex::new(Some(release_cleanup_rx)));
        handle.shared.set_disconnect_cleanup_hook(Some(Arc::new({
            let release_cleanup_rx = release_cleanup_rx.clone();
            move || {
                cleanup_reached_tx
                    .send(())
                    .expect("disconnect cleanup hook should be reached");
                release_cleanup_rx
                    .lock()
                    .expect("disconnect cleanup gate lock")
                    .take()
                    .expect("only the first disconnect owns the gate")
                    .recv()
                    .expect("disconnect cleanup gate should open");
            }
        })));

        let disconnect_shared = handle.shared.clone();
        let disconnect = thread::spawn(move || {
            disconnect_shared.mark_disconnected(Some("Server closed".to_owned()))
        });
        cleanup_reached_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("first disconnect must reach cleanup while holding admission");

        let (stop_attempt_tx, stop_attempt_rx) = std_mpsc::channel();
        let (stop_done_tx, stop_done_rx) = std_mpsc::channel();
        let stop_handle = handle.clone();
        let stopper = thread::spawn(move || {
            stop_attempt_tx
                .send(())
                .expect("stop attempt should be observable");
            stop_handle.stop("disconnect_cleanup_race");
            stop_done_tx.send(()).expect("stop completion signal");
        });
        stop_attempt_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("stop must attempt while disconnect owns admission");
        assert!(
            stop_done_rx
                .recv_timeout(StdDuration::from_millis(50))
                .is_err(),
            "stopped must not overtake first-disconnect cleanup"
        );
        assert!(
            events.try_recv().is_err(),
            "close is not visible before cleanup"
        );

        release_cleanup_tx
            .send(())
            .expect("release disconnect cleanup");
        let close = disconnect
            .join()
            .expect("disconnect thread should not panic");
        assert_eq!(close.code, "server_shutdown");
        stopper.join().expect("stop thread should not panic");
        stop_done_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("stop should finish after cleanup");

        let closed = events.try_recv().expect("close event after cleanup");
        let stopped = events.try_recv().expect("stopped event after cleanup");
        assert_eq!(payload_json(&closed)["type"], "connection_closed");
        assert_eq!(payload_json(&stopped)["type"], "stopped");
        assert!(handle.shared.observation.read().world.is_none());
        assert!(handle.shared.commands.lock().is_empty());
        assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn died_broadcast_reentrant_stop_has_no_post_stopped_actuator() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let callback_handle = handle.clone();
        handle
            .shared
            .set_event_broadcast_hook(Some(Arc::new(move || {
                callback_handle.stop("died_callback_stop");
            })));
        let actuator_called = Arc::new(AtomicBool::new(false));
        let actuator_after_stop = Arc::new(AtomicBool::new(false));
        let called = actuator_called.clone();
        let after_stop = actuator_after_stop.clone();
        let result = handle.shared.admit_death_and_release(|| {
            if handle.shared.stopping.load(Ordering::Acquire) {
                after_stop.store(true, Ordering::Release);
            }
            called.store(true, Ordering::Release);
            true
        });

        assert_eq!(result, Some(true));
        assert!(actuator_called.load(Ordering::Acquire));
        assert!(
            !actuator_after_stop.load(Ordering::Acquire),
            "Death actuator must finish before a re-entrant died callback can stop"
        );
        let kinds = std::iter::from_fn(|| events.try_recv().ok())
            .map(|event| payload_json(&event)["type"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                "died".to_owned(),
                "connection_closed".to_owned(),
                "stopped".to_owned()
            ]
        );
    }

    #[test]
    fn stop_wins_before_late_death_claim_without_actuator_or_event() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.stop("late_death_stop");
        let _stopped = events.try_recv().expect("stop event");
        let actuator_called = Arc::new(AtomicBool::new(false));
        let called = actuator_called.clone();
        assert_eq!(
            handle.shared.admit_death_and_release(|| {
                called.store(true, Ordering::Release);
                true
            }),
            None
        );
        assert!(!actuator_called.load(Ordering::Acquire));
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn stop_cancels_admitted_reconnect_add_intent_and_preserves_terminal_state() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle
            .shared
            .reconnect_pending
            .store(true, Ordering::Release);
        let token = handle
            .shared
            .admit_reconnect_attempt()
            .expect("reconnect should be admitted before stop");
        let request = events.try_recv().expect("reconnect connection request");
        assert_eq!(payload_json(&request)["type"], "connection_requested");
        assert!(handle.shared.reconnect_add_is_allowed(token));

        handle.stop("cancel_reconnect_add");
        assert!(!handle.shared.reconnect_add_is_allowed(token));
        assert!(!handle.shared.reconnect_add_pending.load(Ordering::Acquire));
        assert!(!handle.shared.reconnect_pending.load(Ordering::Acquire));
        assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
        assert!(handle.shared.stopped_reported.load(Ordering::Acquire));
        tokio::time::timeout(
            StdDuration::from_secs(1),
            handle.shared.reconnect_cancel.notified(),
        )
        .await
        .expect("stop must wake a pending reconnect add task");

        let event_count_after_stop = std::iter::from_fn(|| events.try_recv().ok()).count();
        assert_eq!(
            event_count_after_stop, 2,
            "close then stopped after admitted attempt"
        );
        assert!(!handle.shared.begin_connection_attempt());
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn stop_wins_before_reconnect_attempt_admission() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.stop("stop_before_reconnect");
        handle
            .shared
            .reconnect_pending
            .store(true, Ordering::Release);
        assert!(handle.shared.admit_reconnect_attempt().is_none());
        assert!(!handle.shared.reconnect_add_pending.load(Ordering::Acquire));
        assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
    }

    #[test]
    fn runtime_retryable_close_then_reconnect_reuses_the_sealed_close_code() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let close = handle
            .shared
            .mark_disconnected(Some("Server restarting".to_owned()));
        assert_eq!(close.code, "server_shutdown");
        assert!(close.retryable);
        assert!(!close.deliberate);
        handle.shared.emit_reconnect_scheduled(&close);

        let closed = events.try_recv().expect("connection closed event");
        let scheduled = events.try_recv().expect("reconnect scheduled event");
        assert_eq!(payload_json(&closed)["type"], "connection_closed");
        assert_eq!(payload_json(&closed)["close"]["code"], close.code);
        assert_eq!(payload_json(&scheduled)["type"], "reconnect_scheduled");
        assert_eq!(payload_json(&scheduled)["closeCode"], close.code);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn runtime_fatal_close_emits_faulted_without_reconnect_disabled_rewrite() {
        let mut config = RunConfig::default();
        config.reconnect_enabled = false;
        let handle = RuntimeHandle::new(config);
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let close = handle
            .shared
            .mark_disconnected(Some("You are banned from this server".to_owned()));
        let failure = handle.shared.failure_for_close(&close);
        assert_eq!(failure.code, BackendFailureCode::PermissionDenied);
        assert!(!failure.retryable);
        handle.shared.emit_faulted(failure);

        let closed = events.try_recv().expect("fatal close event");
        let faulted = events.try_recv().expect("faulted event");
        assert_eq!(payload_json(&closed)["type"], "connection_closed");
        assert_eq!(payload_json(&closed)["close"]["code"], "permission_denied");
        assert_eq!(payload_json(&faulted)["type"], "faulted");
        assert_eq!(
            payload_json(&faulted)["failure"]["code"],
            "permission_denied"
        );
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn runtime_invalid_session_close_uses_permission_denied_with_authentication_failure() {
        let mut config = RunConfig::default();
        config.reconnect_enabled = false;
        let handle = RuntimeHandle::new(config);
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let close = handle
            .shared
            .mark_disconnected(Some("Invalid session".to_owned()));
        assert_eq!(close.code, "permission_denied");
        assert!(!close.retryable);
        assert!(close.kick.is_some());
        let failure = handle.shared.failure_for_close(&close);
        assert_eq!(failure.code, BackendFailureCode::AuthenticationFailed);
        assert!(!failure.retryable);
        handle.shared.emit_faulted(failure);

        let closed = events.try_recv().expect("invalid-session close event");
        let faulted = events.try_recv().expect("invalid-session fault event");
        assert_eq!(payload_json(&closed)["close"]["code"], "permission_denied");
        assert_eq!(
            payload_json(&faulted)["failure"]["code"],
            "authentication_failed"
        );
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn runtime_disconnect_kick_text_does_not_infer_forced_timeout_or_version_codes() {
        let cases = [
            ("server_shutdown", "server_shutdown", true, None),
            (
                "Unsupported version",
                "unclassified_kick",
                false,
                Some(BackendFailureCode::PermissionDenied),
            ),
            (
                "Timed out",
                "unclassified_kick",
                false,
                Some(BackendFailureCode::PermissionDenied),
            ),
        ];
        for (reason, expected_code, retryable, failure_code) in cases {
            let handle = RuntimeHandle::new(RunConfig::default());
            handle.shared.begin_connection_attempt();
            let close = handle.shared.mark_disconnected(Some(reason.to_owned()));
            assert_eq!(close.code, expected_code, "reason={reason}");
            assert_eq!(close.retryable, retryable, "reason={reason}");
            assert_eq!(
                handle.shared.failure_for_close(&close).code,
                failure_code.unwrap_or(BackendFailureCode::ProtocolError),
                "reason={reason}"
            );
        }
    }

    #[test]
    fn runtime_unclassified_disconnect_component_is_fatal_not_reconnectable() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let close = handle
            .shared
            .mark_disconnected(Some("Removed by an administrator".to_owned()));
        assert_eq!(close.code, "unclassified_kick");
        assert!(!close.retryable);
        assert!(close.kick.is_some());
        let failure = handle.shared.failure_for_close(&close);
        assert_eq!(failure.code, BackendFailureCode::PermissionDenied);
        assert!(!failure.retryable);
        assert!(!handle.shared.config.reconnect_enabled || !close.retryable);
        handle.shared.emit_faulted(failure);

        let _closed = events.try_recv().expect("unclassified kick close event");
        let faulted = events.try_recv().expect("unclassified kick fault event");
        assert_eq!(payload_json(&faulted)["type"], "faulted");
        assert!(
            events.try_recv().is_err(),
            "fatal kick must not schedule reconnect"
        );
    }

    #[test]
    fn runtime_connection_failed_retains_error_and_disabled_retry_is_distinct() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");
        let close = handle
            .shared
            .mark_connection_failed("tcp reset by peer".to_owned());
        let closed = events.try_recv().expect("connection failed close event");
        assert_eq!(payload_json(&closed)["close"]["code"], "connection_failed");
        assert_eq!(
            payload_json(&closed)["close"]["error"]["message"],
            "tcp reset by peer"
        );
        assert_eq!(
            handle.shared.failure_for_close(&close).code,
            BackendFailureCode::ProtocolError
        );
    }

    #[test]
    fn runtime_retryable_close_with_disabled_reconnect_emits_reconnect_disabled() {
        let mut config = RunConfig::default();
        config.reconnect_enabled = false;
        let handle = RuntimeHandle::new(config);
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");
        let close = handle
            .shared
            .mark_disconnected(Some("Server closed".to_owned()));
        assert!(close.retryable);
        assert_eq!(
            handle.shared.failure_for_close(&close).code,
            BackendFailureCode::ReconnectDisabled
        );
    }

    #[test]
    fn runtime_expected_stop_closes_then_stops_after_local_cleanup_with_reason() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");
        handle.shared.set_dimension("minecraft:overworld");
        handle.shared.observation.write().world = Some(empty_world());

        handle.stop("operator_requested");
        let closed = events.try_recv().expect("expected close event");
        let stopped = events.try_recv().expect("stopped event");
        assert_eq!(payload_json(&closed)["type"], "connection_closed");
        assert_eq!(payload_json(&closed)["close"]["code"], "deliberate_stop");
        assert_eq!(payload_json(&closed)["close"]["deliberate"], true);
        assert_eq!(payload_json(&stopped)["type"], "stopped");
        assert_eq!(payload_json(&stopped)["reason"], "operator_requested");
        assert!(handle.shared.observation.read().world.is_none());
        assert!(handle.shared.pop_command().is_none());
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn runtime_active_stop_defers_stopped_until_release_seam_finishes() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let (completion, state) = CommandCompletion::channel("command-stop".to_owned());
        state.begin_active_release(Arc::new(Notify::new()));
        *handle.shared.active_movement_completion.lock() = Some(state.clone());
        handle.shared.active_movement.store(true, Ordering::Release);
        *handle.shared.active_movement_id.lock() = Some("command-stop".to_owned());

        handle.stop("operator_requested");
        let closed = events.try_recv().expect("close must precede deferred stop");
        assert_eq!(payload_json(&closed)["type"], "connection_closed");
        assert!(events.try_recv().is_err(), "stopped must wait for release");
        assert!(!handle.shared.shutdown_requested.load(Ordering::Acquire));
        assert!(state.sender.lock().is_some());

        let completion_for_release = Some(state.clone());
        release_active_movement_and_finish(
            &handle.shared,
            "command-stop",
            0,
            &completion_for_release,
            || {
                assert!(handle.shared.active_movement.load(Ordering::Acquire));
                assert!(state.sender.lock().is_some());
                true
            },
            "stop move",
            Err(BackendError::Cancelled {
                operation: "command:command-stop".to_owned(),
            }),
        );
        state.wait_settled().await;
        assert!(matches!(
            completion.wait().await,
            Err(BackendError::Cancelled { .. })
        ));
        assert!(!handle.shared.active_movement.load(Ordering::Acquire));
        assert!(handle.shared.active_movement_id.lock().is_none());
        assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
        let stopped = events.try_recv().expect("stopped after release");
        assert_eq!(payload_json(&stopped)["type"], "stopped");
        assert_eq!(payload_json(&stopped)["reason"], "operator_requested");

        handle.stop("duplicate_reason");
        release_active_movement_and_finish(
            &handle.shared,
            "command-stop",
            0,
            &completion_for_release,
            || panic!("a settled active move must not release twice"),
            "stop move",
            Err(BackendError::Cancelled {
                operation: "command:command-stop".to_owned(),
            }),
        );
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn command_completion_settled_signal_waits_for_release_and_result_publication() {
        for _ in 0..64 {
            let (completion, state) = CommandCompletion::channel("settlement-race".to_owned());
            state.begin_active_release(Arc::new(Notify::new()));
            let barrier = Arc::new(Barrier::new(2));
            let waiter_state = state.clone();
            let observed_state = waiter_state.clone();
            let waiter_barrier = barrier.clone();
            let waiter = thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("settlement waiter runtime");
                runtime.block_on(async move {
                    waiter_barrier.wait();
                    CommandCompletionCancellation(waiter_state.clone())
                        .wait_settled()
                        .await;
                });
                assert!(!observed_state.active_release.load(Ordering::Acquire));
                assert!(observed_state.settled_result.lock().is_some());
            });
            barrier.wait();
            state.finish(Ok(()));
            waiter
                .join()
                .expect("settlement waiter should not race early");
            assert_eq!(completion.wait_blocking(), Ok(()));
        }
    }

    #[tokio::test]
    async fn runtime_active_move_without_completion_stop_wakes_release_before_shutdown() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let signal = Arc::new(Notify::new());
        *handle.shared.active_movement_cancel_signal.lock() = Some(signal.clone());
        handle.shared.active_movement.store(true, Ordering::Release);
        *handle.shared.active_movement_id.lock() = Some("command-no-completion".to_owned());
        let task_shared = handle.shared.clone();
        let released = Arc::new(AtomicBool::new(false));
        let released_by_task = released.clone();
        let release_task = tokio::spawn(async move {
            signal.notified().await;
            let no_completion = None;
            release_active_movement_and_finish(
                &task_shared,
                "command-no-completion",
                0,
                &no_completion,
                || {
                    assert!(task_shared.active_movement.load(Ordering::Acquire));
                    released_by_task.store(true, Ordering::Release);
                    true
                },
                "stop move",
                Err(BackendError::Cancelled {
                    operation: "command:command-no-completion".to_owned(),
                }),
            );
        });

        handle.stop("operator_requested");
        let closed = events.try_recv().expect("close must be emitted");
        assert_eq!(payload_json(&closed)["type"], "connection_closed");
        assert!(
            events.try_recv().is_err(),
            "stopped waits for no-completion release"
        );
        assert!(!handle.shared.shutdown_requested.load(Ordering::Acquire));
        assert!(handle.shared.active_movement.load(Ordering::Acquire));

        tokio::time::timeout(StdDuration::from_secs(1), release_task)
            .await
            .expect("no-completion move cancellation must wake promptly")
            .expect("release task should not panic");
        assert!(released.load(Ordering::Acquire));
        assert!(!handle.shared.active_movement.load(Ordering::Acquire));
        assert!(handle.shared.active_movement_id.lock().is_none());
        assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
        let stopped = events
            .try_recv()
            .expect("stopped after no-completion release");
        assert_eq!(payload_json(&stopped)["reason"], "operator_requested");
    }

    #[tokio::test]
    async fn runtime_active_move_without_completion_disconnect_wakes_release() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");
        let signal = Arc::new(Notify::new());
        *handle.shared.active_movement_cancel_signal.lock() = Some(signal.clone());
        handle.shared.active_movement.store(true, Ordering::Release);
        *handle.shared.active_movement_id.lock() = Some("command-disconnect".to_owned());
        let task_shared = handle.shared.clone();
        let release_task = tokio::spawn(async move {
            signal.notified().await;
            let no_completion = None;
            release_active_movement_and_finish(
                &task_shared,
                "command-disconnect",
                0,
                &no_completion,
                || true,
                "disconnect move",
                Err(BackendError::Cancelled {
                    operation: "command:command-disconnect".to_owned(),
                }),
            );
        });

        let close = handle
            .shared
            .mark_disconnected(Some("Server closed".to_owned()));
        assert_eq!(close.code, "server_shutdown");
        assert_eq!(
            payload_json(&events.try_recv().expect("close event"))["type"],
            "connection_closed"
        );
        tokio::time::timeout(StdDuration::from_secs(1), release_task)
            .await
            .expect("disconnect must wake no-completion move")
            .expect("release task should not panic");
        assert!(!handle.shared.active_movement.load(Ordering::Acquire));
        assert!(handle.shared.active_movement_id.lock().is_none());
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn observation_callback_can_stop_without_deadlock_and_preserves_fifo() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let request = events.try_recv().expect("connection request event");
        assert_eq!(request.id, "event-1");

        let source = handle.observation_source();
        let stop_event_ids = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let (stop_returned_tx, stop_returned_rx) = std_mpsc::channel();
        let stop_listener = Arc::new(StopOnFirstListener {
            handle: handle.clone(),
            event_ids: stop_event_ids.clone(),
            stop_returned: stop_returned_tx,
            invoked: AtomicBool::new(false),
        });
        let (other_listener, other_events) = recording_listener();
        let _stop_subscription = ProtocolObservationSource::subscribe(&source, stop_listener)
            .expect("stop listener subscription should succeed");
        let _other_subscription = ProtocolObservationSource::subscribe(&source, other_listener)
            .expect("other listener subscription should succeed");

        let emitting_handle = handle.clone();
        let (emit_finished_tx, emit_finished_rx) = std_mpsc::channel();
        let emit_thread = thread::spawn(move || {
            emit_test_fact(&emitting_handle, BackendEventKind::Entity);
            emit_finished_tx
                .send(())
                .expect("emit completion should be observable");
        });
        emit_finished_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("callback stop and its drain must finish within the bound");
        stop_returned_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("stop must return from inside the callback");
        emit_thread.join().expect("event thread should not panic");

        let mut delivered = Vec::new();
        while let Ok(event) = events.try_recv() {
            delivered.push(event);
        }
        assert_eq!(
            delivered
                .iter()
                .map(|event| event.id.as_str())
                .collect::<Vec<_>>(),
            vec!["event-2", "event-3", "event-4"]
        );
        assert_eq!(payload_json(&delivered[0])["type"], "animation");
        assert_eq!(payload_json(&delivered[1])["type"], "connection_closed");
        assert_eq!(payload_json(&delivered[1])["close"]["deliberate"], true);
        assert_eq!(payload_json(&delivered[2])["type"], "stopped");
        assert_eq!(payload_json(&delivered[2])["reason"], "callback-stop");
        assert_eq!(&*stop_event_ids.lock(), &["event-2".to_owned()]);
        assert_eq!(
            other_events
                .lock()
                .iter()
                .map(observation_event_id)
                .collect::<Vec<_>>(),
            vec!["event-2"]
        );
    }

    #[test]
    fn nested_observation_emit_is_fifo_and_drained_before_top_level_returns() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request event");

        let source = handle.observation_source();
        let nested_event_ids = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let nested_listener = Arc::new(NestedEmitListener {
            handle: handle.clone(),
            event_ids: nested_event_ids.clone(),
            emitted: AtomicBool::new(false),
        });
        let (other_listener, other_events) = recording_listener();
        let _nested_subscription = ProtocolObservationSource::subscribe(&source, nested_listener)
            .expect("nested listener subscription should succeed");
        let _other_subscription = ProtocolObservationSource::subscribe(&source, other_listener)
            .expect("other listener subscription should succeed");

        let emitting_handle = handle.clone();
        let (emit_finished_tx, emit_finished_rx) = std_mpsc::channel();
        let emit_thread = thread::spawn(move || {
            emit_test_fact(&emitting_handle, BackendEventKind::Entity);
            emit_finished_tx
                .send(())
                .expect("nested emit completion should be observable");
        });
        emit_finished_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("nested emit and its drain must finish within the bound");
        emit_thread
            .join()
            .expect("nested emit thread should not panic");

        assert_eq!(
            &*nested_event_ids.lock(),
            &[
                "event-2".to_owned(),
                "event-3".to_owned(),
                "event-4".to_owned()
            ]
        );
        assert_eq!(
            other_events
                .lock()
                .iter()
                .map(observation_event_id)
                .collect::<Vec<_>>(),
            vec!["event-2", "event-3", "event-4"]
        );
        let mut delivered = Vec::new();
        while let Ok(event) = events.try_recv() {
            delivered.push(event);
        }
        assert_eq!(
            delivered
                .iter()
                .map(|event| event.id.as_str())
                .collect::<Vec<_>>(),
            vec!["event-2", "event-3", "event-4"]
        );
        assert_eq!(delivered[0].kind, BackendEventKind::Entity);
        assert_eq!(delivered[1].kind, BackendEventKind::Entity);
        assert_eq!(delivered[2].kind, BackendEventKind::Block);
    }

    #[test]
    fn concurrent_producers_keep_connection_request_first_for_each_epoch() {
        const PRODUCER_COUNT: usize = 8;
        const EVENTS_PER_PRODUCER: usize = 32;
        const ATTEMPT_COUNT: usize = 40;

        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let start = Arc::new(Barrier::new(PRODUCER_COUNT + 2));
        let (worker_finished_tx, worker_finished_rx) = std_mpsc::channel();
        let mut workers = Vec::new();
        for producer in 0..PRODUCER_COUNT {
            let shared = handle.shared.clone();
            let start = start.clone();
            let worker_finished = worker_finished_tx.clone();
            workers.push(thread::spawn(move || {
                start.wait();
                for sequence in 0..EVENTS_PER_PRODUCER {
                    shared.emit(
                        FactSource::ServerObserved,
                        BackendEventPayload::World(ContractProtocolWorldEvent::GameChanged {
                            dimension: Some(format!("producer:{producer}")),
                            game_mode: Some(format!("sequence:{sequence}")),
                        }),
                    );
                }
                worker_finished
                    .send(())
                    .expect("producer completion should be observable");
            }));
        }
        let attempt_shared = handle.shared.clone();
        let attempt_start = start.clone();
        let attempt_finished = worker_finished_tx.clone();
        workers.push(thread::spawn(move || {
            attempt_start.wait();
            for _ in 0..ATTEMPT_COUNT {
                attempt_shared.begin_connection_attempt();
            }
            attempt_finished
                .send(())
                .expect("attempt completion should be observable");
        }));
        let worker_count = workers.len();
        drop(worker_finished_tx);
        start.wait();
        for _ in 0..worker_count {
            worker_finished_rx
                .recv_timeout(StdDuration::from_secs(1))
                .expect("all concurrent producers must finish within the bound");
        }
        for worker in workers {
            worker.join().expect("producer should not panic");
        }

        let mut delivered = Vec::new();
        while let Ok(event) = events.try_recv() {
            delivered.push(event);
        }
        assert_eq!(
            delivered.len(),
            PRODUCER_COUNT * EVENTS_PER_PRODUCER + ATTEMPT_COUNT
        );
        for (index, event) in delivered.iter().enumerate() {
            assert_eq!(event.id, format!("event-{}", index + 1));
        }

        let mut first_event_by_epoch = std::collections::BTreeMap::new();
        for event in &delivered {
            if event.connection_epoch > 0 {
                first_event_by_epoch
                    .entry(event.connection_epoch)
                    .or_insert(event);
            }
        }
        assert_eq!(first_event_by_epoch.len(), ATTEMPT_COUNT);
        for epoch in 1..=ATTEMPT_COUNT as u64 {
            let event = first_event_by_epoch
                .get(&epoch)
                .expect("every attempt epoch must have an event");
            assert_eq!(event.kind, BackendEventKind::Lifecycle);
            assert_eq!(event.connection_attempt_id, format!("attempt-{epoch}"));
            let payload = payload_json(event);
            assert_eq!(payload["type"], "connection_requested");
            assert_eq!(payload["attempt"], epoch);
        }
    }

    fn observation_snapshot(epoch: u64) -> MinecraftSnapshotV1 {
        MinecraftSnapshotV1 {
            protocol: "mineintent.minecraft.snapshot.v1".to_owned(),
            snapshot_revision: 1,
            lifecycle_revision: 1,
            captured_at: now_utc(),
            process_session_id: "test-session".to_owned(),
            connection_epoch: epoch,
            connection_attempt_id: format!("attempt-{epoch}"),
            world: WorldSnapshot {
                world_id: "test-world".to_owned(),
                dimension: "minecraft:overworld".to_owned(),
                minecraft_version: "26.1.2".to_owned(),
                protocol_version: 775,
                game_mode: "survival".to_owned(),
                min_y: -64,
                height: 384,
            },
            self_snapshot: SelfSnapshot {
                entity_key: "self".to_owned(),
                username: "MineIntentBot".to_owned(),
                position: Vec3Value {
                    x: 1.0,
                    y: 64.0,
                    z: 2.0,
                },
                velocity: Vec3Value {
                    x: -1.5,
                    y: 0.25,
                    z: 2.75,
                },
                yaw: 0.25,
                pitch: -0.1,
                on_ground: true,
                alive: true,
                health: 20.0,
                food: 20,
                food_saturation: 5.0,
                experience: ExperienceSnapshot {
                    level: 0,
                    progress: 0.0,
                    total: 0,
                },
            },
            inventory: InventorySnapshot {
                selected_hotbar_slot: 0,
                slots: Vec::new(),
            },
            tracked_players: Vec::new(),
        }
    }

    fn observation_entity(entity_key: &str) -> ProtocolEntitySnapshot {
        ProtocolEntitySnapshot {
            entity_key: entity_key.to_owned(),
            protocol_entity_id: 7,
            entity_type: "zombie".to_owned(),
            name: Some("zombie".to_owned()),
            username: None,
            uuid: None,
            position: Vec3Value {
                x: 3.0,
                y: 64.0,
                z: 4.0,
            },
            velocity: Vec3Value {
                x: -0.25,
                y: 0.5,
                z: 0.75,
            },
            yaw: 0.125,
            pitch: -0.25,
            head_yaw: Some(0.5),
            width: 0.625,
            height: 1.875,
            on_ground: false,
            pose: Some("standing".to_owned()),
            held_item_name: Some("iron_sword".to_owned()),
            equipment: vec![crate::snapshot::EntityEquipmentSnapshot {
                slot: 2,
                item_name: "iron_sword".to_owned(),
                count: 3,
            }],
            valid: true,
        }
    }

    fn snapshot_at(epoch: u64, x: f64, y: f64, z: f64) -> MinecraftSnapshotV1 {
        let mut snapshot = observation_snapshot(epoch);
        snapshot.self_snapshot.position = Vec3Value { x, y, z };
        snapshot
    }

    fn no_deadline_control() -> OperationControl {
        test_control(TestCancellation::new(false, None, false, None), None)
    }

    #[tokio::test]
    async fn read_viewport_returns_projection_source_and_unique_revision() {
        let (_handle, source, _world) = ready_viewport_source();

        let first = source
            .read_viewport(no_deadline_control())
            .await
            .expect("ready viewport read should succeed");
        assert_eq!(first.projection.frame.self_pose.position, [1.0, 64.0, 2.0]);
        assert_eq!(
            first.source,
            mineintent_contracts::minecraft::FactSource::ServerObserved
        );
        assert!(first.revision > 0);

        let second = source
            .read_viewport(no_deadline_control())
            .await
            .expect("second ready viewport read should succeed");
        assert!(second.revision > first.revision);
        assert_eq!(second.projection, first.projection);
        assert_eq!(second.source, first.source);
    }

    #[tokio::test]
    async fn read_directed_viewport_uses_atomic_capture_and_revision_discipline() {
        let (handle, source, _world) = ready_viewport_source();
        let position = ContractBlockPosition { x: 0, y: 64, z: -1 };
        let first = source
            .read_directed_viewport(vec![position], no_deadline_control())
            .await
            .expect("directed unloaded target should still return a strict result");
        assert!(first.seen.is_empty());
        assert_eq!(first.unseen.len(), 1);
        assert_eq!(first.unseen[0].at, [0, 64, -1]);
        assert!(first.unseen[0]
            .why
            .contains(&mineintent_contracts::minecraft::DirectedWhy::ChunkNotLoaded));
        assert!(first.unseen[0].by.is_none());
        let revision = handle.shared.viewport_revision.load(Ordering::Acquire);

        let second = source
            .read_directed_viewport(vec![position], no_deadline_control())
            .await
            .expect("second directed read should succeed");
        assert_eq!(second, first);
        assert_eq!(
            handle.shared.viewport_revision.load(Ordering::Acquire),
            revision + 1
        );
    }

    #[tokio::test]
    async fn read_directed_viewport_rejects_duplicates_and_serializes_out_of_world_rows() {
        let (handle, source, _world) = ready_viewport_source();
        let duplicate = source
            .read_directed_viewport(
                vec![
                    ContractBlockPosition { x: 0, y: 64, z: -1 },
                    ContractBlockPosition { x: 0, y: 64, z: -1 },
                ],
                no_deadline_control(),
            )
            .await;
        assert!(matches!(
            duplicate,
            Err(DirectedViewportError::Backend(BackendError::InvalidCommand {
                field,
                ..
            })) if field == "positions"
        ));

        {
            let mut observation = handle.shared.observation.write();
            observation.snapshot = Some(snapshot_at(1, 0.0, 9_999.0, 0.0));
            observation.bump_generation();
        }
        let out_of_world = handle
            .observation_source()
            .read_directed_viewport(
                vec![ContractBlockPosition {
                    x: 0,
                    y: 10_000,
                    z: -1,
                }],
                no_deadline_control(),
            )
            .await;
        let out_of_world = out_of_world.expect("out-of-world coordinates are row-wise answers");
        assert!(out_of_world.seen.is_empty());
        assert_eq!(out_of_world.unseen.len(), 1);
        assert_eq!(out_of_world.unseen[0].at, [0, 10_000, -1]);
        assert_eq!(
            out_of_world.unseen[0].why,
            [mineintent_contracts::minecraft::DirectedWhy::OutOfWorld]
        );
        assert!(out_of_world.unseen[0].by.is_none());
        assert!(serde_json::to_value(&out_of_world.unseen[0])
            .unwrap()
            .get("block")
            .is_none());
    }

    #[tokio::test]
    async fn read_directed_viewport_uses_current_world_height_bounds_rowwise() {
        let (handle, source, world) = ready_viewport_source();
        let (min_y, height) = {
            let world = world.read();
            (world.chunks.min_y(), world.chunks.height())
        };
        let upper_y = i32::try_from(i64::from(min_y) + i64::from(height))
            .expect("test world height upper bound fits i32 coordinates");

        let mut lower_snapshot = snapshot_at(1, 0.5, f64::from(min_y), 0.5);
        lower_snapshot.self_snapshot.pitch = -35.0;
        {
            let mut observation = handle.shared.observation.write();
            observation.snapshot = Some(lower_snapshot);
            observation.bump_generation();
        }
        let lower = source
            .read_directed_viewport(
                vec![
                    ContractBlockPosition {
                        x: 0,
                        y: min_y - 1,
                        z: -3,
                    },
                    ContractBlockPosition {
                        x: 0,
                        y: min_y,
                        z: -3,
                    },
                ],
                no_deadline_control(),
            )
            .await
            .expect("lower boundary should be answered per coordinate");
        assert_eq!(
            lower
                .unseen
                .iter()
                .find(|item| item.at == [0, min_y - 1, -3])
                .expect("lower out-of-world row")
                .why,
            [mineintent_contracts::minecraft::DirectedWhy::OutOfWorld]
        );
        assert!(lower
            .unseen
            .iter()
            .find(|item| item.at == [0, min_y, -3])
            .is_some_and(|item| {
                !item
                    .why
                    .contains(&mineintent_contracts::minecraft::DirectedWhy::OutOfWorld)
            }));

        let mut upper_snapshot = snapshot_at(1, 0.5, f64::from(upper_y - 1), 0.5);
        upper_snapshot.self_snapshot.pitch = 0.0;
        {
            let mut observation = handle.shared.observation.write();
            observation.snapshot = Some(upper_snapshot);
            observation.bump_generation();
        }
        let upper = source
            .read_directed_viewport(
                vec![
                    ContractBlockPosition {
                        x: 0,
                        y: upper_y - 1,
                        z: -3,
                    },
                    ContractBlockPosition {
                        x: 0,
                        y: upper_y,
                        z: -3,
                    },
                ],
                no_deadline_control(),
            )
            .await
            .expect("upper boundary should be answered per coordinate");
        assert_eq!(
            upper
                .unseen
                .iter()
                .find(|item| item.at == [0, upper_y, -3])
                .expect("upper out-of-world row")
                .why,
            [mineintent_contracts::minecraft::DirectedWhy::OutOfWorld]
        );
        assert!(upper
            .unseen
            .iter()
            .find(|item| item.at == [0, upper_y - 1, -3])
            .is_some_and(|item| {
                !item
                    .why
                    .contains(&mineintent_contracts::minecraft::DirectedWhy::OutOfWorld)
            }));
    }

    #[tokio::test]
    async fn read_directed_viewport_cancel_and_deadline_keep_full_kernel_boundaries() {
        let (_handle, source, _world) = ready_viewport_source();
        let cancelled = source
            .read_directed_viewport(
                vec![ContractBlockPosition { x: 0, y: 64, z: -1 }],
                test_control(TestCancellation::new(true, None, false, None), None),
            )
            .await;
        assert_eq!(
            cancelled,
            Err(DirectedViewportError::Backend(BackendError::Cancelled {
                operation: "read_directed_viewport".to_owned()
            }))
        );

        let deadline = TestDeadline::new(true, None, None);
        let expired = source
            .read_directed_viewport(
                vec![ContractBlockPosition { x: 0, y: 64, z: -1 }],
                test_control(
                    TestCancellation::new(false, None, false, None),
                    Some(deadline),
                ),
            )
            .await;
        assert_eq!(
            expired,
            Err(DirectedViewportError::Backend(
                BackendError::DeadlineExceeded {
                    operation: "read_directed_viewport".to_owned()
                }
            ))
        );
    }

    #[tokio::test]
    async fn read_directed_viewport_worker_wakeup_preserves_operation_name() {
        let (_handle, source, _world) = ready_viewport_source();
        let cancellation = WorkerWakeCancellation::new();
        let cancelled = tokio::time::timeout(
            Duration::from_secs(1),
            source.read_directed_viewport(
                vec![ContractBlockPosition { x: 0, y: 64, z: -1 }],
                OperationControl::new(cancellation.clone(), None),
            ),
        )
        .await
        .expect("worker cancellation test must be bounded");
        assert!(cancellation.worker_started.load(Ordering::SeqCst));
        assert_eq!(
            cancelled,
            Err(DirectedViewportError::Backend(BackendError::Cancelled {
                operation: "read_directed_viewport".to_owned()
            }))
        );

        let (_handle, source, _world) = ready_viewport_source();
        let deadline = WorkerWakeDeadline::new();
        let expired = tokio::time::timeout(
            Duration::from_secs(1),
            source.read_directed_viewport(
                vec![ContractBlockPosition { x: 0, y: 64, z: -1 }],
                OperationControl::new(
                    TestCancellation::new(false, None, false, None),
                    Some(deadline.clone()),
                ),
            ),
        )
        .await
        .expect("worker deadline test must be bounded");
        assert!(deadline.worker_started.load(Ordering::SeqCst));
        assert_eq!(
            expired,
            Err(DirectedViewportError::Backend(
                BackendError::DeadlineExceeded {
                    operation: "read_directed_viewport".to_owned()
                }
            ))
        );
    }

    #[tokio::test]
    async fn read_directed_viewport_retries_generation_and_rejects_stale_epoch_atomically() {
        let (handle, source, world) = ready_viewport_source();
        let replacement = snapshot_at(1, 9.0, 64.0, 10.0);
        let shared = handle.shared.clone();
        let action: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let mut observation = shared.observation.write();
            observation.snapshot = Some(replacement.clone());
            observation.source = Some(FactSource::ClientPredicted);
            observation.tracked_entities = vec![observation_entity("replacement")];
            observation.world = Some(world.clone());
            observation.bump_generation();
        });
        let trigger = TestCancellation::new(false, Some(4), false, Some(action));
        let read = source
            .read_directed_viewport(
                vec![ContractBlockPosition { x: 0, y: 64, z: -1 }],
                test_control(trigger, None),
            )
            .await
            .expect("changed directed capture should retry");
        assert_eq!(read.unseen[0].at, [0, 64, -1]);

        let (handle, source, _world) = ready_viewport_source();
        let shared = handle.shared.clone();
        let action: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            shared.begin_connection_attempt();
        });
        let trigger = TestCancellation::new(false, Some(4), false, Some(action));
        let stale = source
            .read_directed_viewport(
                vec![ContractBlockPosition { x: 0, y: 64, z: -1 }],
                test_control(trigger, None),
            )
            .await;
        assert!(matches!(
            stale,
            Err(DirectedViewportError::Backend(BackendError::StaleEpoch {
                bound_epoch: 1,
                current_epoch: 2,
            }))
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn read_viewport_revision_is_unique_for_concurrent_successes() {
        let (_handle, source, _world) = ready_viewport_source();
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let source = source.clone();
            tasks.push(tokio::spawn(async move {
                source
                    .read_viewport(no_deadline_control())
                    .await
                    .expect("concurrent viewport read should succeed")
                    .revision
            }));
        }

        let mut revisions = Vec::new();
        for task in tasks {
            revisions.push(task.await.expect("viewport task should not panic"));
        }
        revisions.sort_unstable();
        assert!(revisions.windows(2).all(|window| window[0] < window[1]));
    }

    #[tokio::test]
    async fn read_viewport_preflight_cancel_and_deadline_do_not_scan() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();

        let cancellation = TestCancellation::new(true, None, false, None);
        let cancelled = source
            .read_viewport(test_control(cancellation.clone(), None))
            .await;
        assert_eq!(
            cancelled,
            Err(BackendError::Cancelled {
                operation: "read_viewport".to_owned()
            })
        );
        assert_eq!(cancellation.checks.load(Ordering::SeqCst), 1);

        let deadline_cancellation = TestCancellation::new(false, None, false, None);
        let deadline = TestDeadline::new(true, None, None);
        let expired = source
            .read_viewport(test_control(
                deadline_cancellation.clone(),
                Some(deadline.clone()),
            ))
            .await;
        assert_eq!(
            expired,
            Err(BackendError::DeadlineExceeded {
                operation: "read_viewport".to_owned()
            })
        );
        assert_eq!(deadline_cancellation.checks.load(Ordering::SeqCst), 1);
        assert_eq!(deadline.checks.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn read_viewport_observes_cancellation_and_deadline_during_projection() {
        let (_handle, source, _world) = ready_viewport_source();

        let cancellation = TestCancellation::new(false, Some(7), true, None);
        let cancelled = source
            .read_viewport(test_control(cancellation.clone(), None))
            .await;
        assert_eq!(
            cancelled,
            Err(BackendError::Cancelled {
                operation: "read_viewport".to_owned()
            })
        );
        assert!(cancellation.checks.load(Ordering::SeqCst) >= 7);

        let deadline_cancellation = TestCancellation::new(false, None, false, None);
        let deadline = TestDeadline::new(false, Some(7), None);
        let expired = source
            .read_viewport(test_control(deadline_cancellation, Some(deadline.clone())))
            .await;
        assert_eq!(
            expired,
            Err(BackendError::DeadlineExceeded {
                operation: "read_viewport".to_owned()
            })
        );
        assert!(deadline.checks.load(Ordering::SeqCst) >= 7);
    }

    #[tokio::test]
    async fn read_viewport_retries_when_capture_generation_changes() {
        let (handle, source, world) = ready_viewport_source();
        let replacement = snapshot_at(1, 9.0, 64.0, 10.0);
        let shared = handle.shared.clone();
        let action: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let mut observation = shared.observation.write();
            observation.snapshot = Some(replacement.clone());
            observation.source = Some(FactSource::ClientPredicted);
            observation.tracked_entities = vec![observation_entity("replacement")];
            observation.world = Some(world.clone());
            observation.bump_generation();
        });
        let trigger = TestCancellation::new(false, Some(4), false, Some(action));

        let read = source
            .read_viewport(test_control(trigger, None))
            .await
            .expect("a changed capture should be retried, not mixed");
        assert_eq!(read.projection.frame.self_pose.position, [9.0, 64.0, 10.0]);
        assert_eq!(
            read.source,
            mineintent_contracts::minecraft::FactSource::ClientPredicted
        );
    }

    #[tokio::test]
    async fn read_viewport_rejects_epoch_change_after_capture() {
        let (handle, source, _world) = ready_viewport_source();
        let shared = handle.shared.clone();
        let action: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            shared.begin_connection_attempt();
        });
        let trigger = TestCancellation::new(false, Some(4), false, Some(action));

        let read = source.read_viewport(test_control(trigger, None)).await;
        assert_eq!(
            read,
            Err(BackendError::StaleEpoch {
                bound_epoch: 1,
                current_epoch: 2,
            })
        );
    }

    #[tokio::test]
    async fn read_viewport_rejects_missing_ready_capture_parts() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();
        handle.shared.ready.store(true, Ordering::Release);

        let missing_snapshot = source.read_viewport(no_deadline_control()).await;
        assert_eq!(
            missing_snapshot,
            Err(BackendError::NotReady {
                state: "viewport_snapshot_unavailable".to_owned()
            })
        );

        let world = empty_world();
        {
            let mut observation = handle.shared.observation.write();
            observation.snapshot = Some(observation_snapshot(1));
            observation.source = None;
            observation.world = Some(world.clone());
            observation.bump_generation();
        }
        let missing_source = source.read_viewport(no_deadline_control()).await;
        assert_eq!(
            missing_source,
            Err(BackendError::NotReady {
                state: "viewport_source_unavailable".to_owned()
            })
        );

        {
            let mut observation = handle.shared.observation.write();
            observation.source = Some(FactSource::ServerObserved);
            observation.bump_generation();
        }
        let ready = source.read_viewport(no_deadline_control()).await;
        assert!(ready.is_ok());

        {
            let mut observation = handle.shared.observation.write();
            observation.world = None;
            observation.bump_generation();
        }
        let missing_world = source.read_viewport(no_deadline_control()).await;
        assert_eq!(
            missing_world,
            Err(BackendError::NotReady {
                state: "viewport_world_unavailable".to_owned()
            })
        );
    }

    struct RecordingListener {
        events: Arc<parking_lot::Mutex<Vec<ObservationEvent>>>,
    }

    impl ObservationEventListener for RecordingListener {
        fn on_event(&self, event: ObservationEvent) {
            self.events.lock().push(event);
        }
    }

    fn recording_listener() -> (
        Arc<RecordingListener>,
        Arc<parking_lot::Mutex<Vec<ObservationEvent>>>,
    ) {
        let events = Arc::new(parking_lot::Mutex::new(Vec::new()));
        (
            Arc::new(RecordingListener {
                events: events.clone(),
            }),
            events,
        )
    }

    struct NoopListener;

    impl ObservationEventListener for NoopListener {
        fn on_event(&self, _event: ObservationEvent) {}
    }

    fn observation_event_id(event: &ObservationEvent) -> &str {
        match event {
            ObservationEvent::Entity(event) => &event.id,
            ObservationEvent::Block(event) => &event.id,
            ObservationEvent::Sound(event) => &event.id,
        }
    }

    struct StopOnFirstListener {
        handle: RuntimeHandle,
        event_ids: Arc<parking_lot::Mutex<Vec<String>>>,
        stop_returned: std_mpsc::Sender<()>,
        invoked: AtomicBool,
    }

    impl ObservationEventListener for StopOnFirstListener {
        fn on_event(&self, event: ObservationEvent) {
            self.event_ids
                .lock()
                .push(observation_event_id(&event).to_owned());
            if !self.invoked.swap(true, Ordering::SeqCst) {
                self.handle.stop("callback-stop");
                self.stop_returned
                    .send(())
                    .expect("stop completion should be observable");
            }
        }
    }

    struct NestedEmitListener {
        handle: RuntimeHandle,
        event_ids: Arc<parking_lot::Mutex<Vec<String>>>,
        emitted: AtomicBool,
    }

    impl ObservationEventListener for NestedEmitListener {
        fn on_event(&self, event: ObservationEvent) {
            self.event_ids
                .lock()
                .push(observation_event_id(&event).to_owned());
            if self.emitted.swap(true, Ordering::SeqCst) {
                return;
            }
            self.handle.shared.emit(
                FactSource::ServerObserved,
                valid_observation_payload(BackendEventKind::Entity),
            );
            self.handle.shared.emit(
                FactSource::ServerObserved,
                valid_observation_payload(BackendEventKind::Block),
            );
        }
    }

    struct PanicListener;

    impl ObservationEventListener for PanicListener {
        fn on_event(&self, _event: ObservationEvent) {
            panic!("observation listener test panic");
        }
    }

    struct ReentrantListener {
        source: RuntimeObservationSource,
        invoked: AtomicBool,
        pose: Arc<parking_lot::Mutex<Option<Result<ContractSelfPose, BackendError>>>>,
        block: Arc<parking_lot::Mutex<Option<Result<ContractBlockReadResult, BackendError>>>>,
        nested_subscription_succeeded: AtomicBool,
    }

    impl ObservationEventListener for ReentrantListener {
        fn on_event(&self, _event: ObservationEvent) {
            if self.invoked.swap(true, Ordering::SeqCst) {
                return;
            }
            *self.pose.lock() = Some(self.source.self_pose());
            *self.block.lock() =
                Some(
                    self.source
                        .read_block(ContractBlockPosition { x: 0, y: 64, z: 0 }),
                );
            let nested = ProtocolObservationSource::subscribe(&self.source, Arc::new(NoopListener));
            self.nested_subscription_succeeded
                .store(nested.is_ok(), Ordering::SeqCst);
        }
    }

    struct BlockingListener {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
        calls: Arc<AtomicUsize>,
    }

    impl ObservationEventListener for BlockingListener {
        fn on_event(&self, _event: ObservationEvent) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.wait();
            self.release.wait();
        }
    }

    struct AUnsubscribesBListener {
        b_subscription: Arc<StdMutex<Option<Box<dyn Subscription>>>>,
        calls: Arc<AtomicUsize>,
    }

    impl ObservationEventListener for AUnsubscribesBListener {
        fn on_event(&self, _event: ObservationEvent) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut subscription = self
                .b_subscription
                .lock()
                .expect("B subscription mutex should not be poisoned")
                .take()
                .expect("B subscription should be present during A callback");
            subscription.unsubscribe();
        }
    }

    fn valid_observation_payload(kind: BackendEventKind) -> BackendEventPayload {
        match kind {
            BackendEventKind::Entity => {
                BackendEventPayload::Entity(ContractProtocolEntityEvent::Animation {
                    entity_key: "entity-7".to_owned(),
                    animation: "swing".to_owned(),
                })
            }
            BackendEventKind::Block => {
                BackendEventPayload::Block(ContractProtocolBlockEvent::ChunkLoaded {
                    chunk_x: 3,
                    chunk_z: -4,
                })
            }
            BackendEventKind::Sound => BackendEventPayload::Sound(ContractProtocolSoundPayload {
                event_type: mineintent_contracts::minecraft::HeardSoundType::Heard,
                sound_key: "minecraft:block.note_block.harp".to_owned(),
                sound_name: Some("note_block.harp".to_owned()),
                sound_id: Some(12),
                category: Some("blocks".to_owned()),
                source_position: ContractVec3Value {
                    x: 1.5,
                    y: 64.25,
                    z: -2.0,
                },
                volume: 0.75,
                pitch: 1.25,
                protocol_source:
                    mineintent_contracts::minecraft::ProtocolSoundSource::NamedSoundEffect,
            }),
            BackendEventKind::Lifecycle => {
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected)
            }
            BackendEventKind::Chat => BackendEventPayload::Chat(ContractProtocolChatEvent {
                sender_username: Some("Alex".to_owned()),
                plain_text: "hello".to_owned(),
                position: Some(ChatPosition::Chat),
                verified: None,
            }),
            BackendEventKind::SelfState => BackendEventPayload::SelfState(
                ContractProtocolSelfEvent::ServerPositionCorrection {
                    teleport_id: 1,
                    position: ContractVec3Value {
                        x: 0.0,
                        y: 64.0,
                        z: 0.0,
                    },
                    velocity: ContractVec3Value {
                        x: 0.0,
                        y: 0.0,
                        z: 0.0,
                    },
                    yaw: 0.0,
                    pitch: 0.0,
                    relative: RelativeMovementFlags {
                        x: false,
                        y: false,
                        z: false,
                        yaw: false,
                        pitch: false,
                        delta_x: false,
                        delta_y: false,
                        delta_z: false,
                        rotate_delta: false,
                    },
                },
            ),
            BackendEventKind::World => {
                BackendEventPayload::World(ContractProtocolWorldEvent::GameChanged {
                    dimension: Some("minecraft:overworld".to_owned()),
                    game_mode: Some("survival".to_owned()),
                })
            }
            BackendEventKind::PlayerList => {
                BackendEventPayload::PlayerList(ContractProtocolPlayerListEvent::Add {
                    uuid: "uuid-1".to_owned(),
                    username: "Alex".to_owned(),
                })
            }
            BackendEventKind::SnapshotChanged => {
                BackendEventPayload::SnapshotChanged(ContractProtocolSnapshotChangedEvent {
                    group: "self".to_owned(),
                    snapshot_revision: 1,
                })
            }
            BackendEventKind::Overflow => BackendEventPayload::Overflow(
                mineintent_contracts::minecraft::BackendOverflowPayload {
                    event_type: mineintent_contracts::minecraft::OverflowType::Overflow,
                    dropped_count: 1,
                    dropped_kinds: vec![BackendEventKind::Entity],
                },
            ),
        }
    }

    fn emit_test_fact(handle: &RuntimeHandle, kind: BackendEventKind) {
        handle
            .shared
            .emit(FactSource::ServerObserved, valid_observation_payload(kind));
    }

    fn emit_and_capture(
        handle: &RuntimeHandle,
        payload: BackendEventPayload,
    ) -> BackendEventEnvelope {
        let mut events = handle.subscribe();
        handle.shared.emit(FactSource::ServerObserved, payload);
        events
            .try_recv()
            .expect("global v2 event should be emitted for adapter fixture")
    }

    fn contract_event_kind(event: &ObservationEvent) -> ContractBackendEventKind {
        match event {
            ObservationEvent::Entity(_) => ContractBackendEventKind::Entity,
            ObservationEvent::Block(_) => ContractBackendEventKind::Block,
            ObservationEvent::Sound(_) => ContractBackendEventKind::Sound,
        }
    }

    fn assert_metadata<T>(
        raw: &BackendEventEnvelope,
        typed: &ContractBackendEventEnvelope<T>,
        kind: ContractBackendEventKind,
    ) {
        assert_eq!(typed.protocol, ContractBackendEventProtocol::V2);
        assert_eq!(typed.id, raw.id);
        assert_eq!(typed.kind, kind);
        assert_eq!(typed.occurred_at, raw.occurred_at);
        assert_eq!(typed.process_session_id, raw.process_session_id);
        assert_eq!(typed.connection_epoch, raw.connection_epoch);
        assert_eq!(typed.connection_attempt_id, raw.connection_attempt_id);
        assert_eq!(typed.world_id, raw.world_id);
        assert_eq!(typed.dimension, raw.dimension);
        assert_eq!(typed.source, ContractFactSource::ServerObserved);
    }

    fn contract_entity_event_fixture() -> ContractProtocolEntityEvent {
        ContractProtocolEntityEvent::Spawned {
            entity: ContractProtocolEntitySnapshot {
                entity_key: "entity-7".to_owned(),
                protocol_entity_id: 7,
                entity_type: "zombie".to_owned(),
                name: Some("zombie".to_owned()),
                username: None,
                uuid: Some("uuid-7".to_owned()),
                position: ContractVec3Value {
                    x: 3.0,
                    y: 64.0,
                    z: 4.0,
                },
                velocity: ContractVec3Value {
                    x: -0.25,
                    y: 0.5,
                    z: 0.75,
                },
                yaw: 0.125,
                pitch: -0.25,
                head_yaw: Some(0.5),
                width: 0.625,
                height: 1.875,
                on_ground: false,
                pose: Some("standing".to_owned()),
                held_item_name: Some("iron_sword".to_owned()),
                equipment: vec![ContractEntityEquipmentSnapshot {
                    slot: 2,
                    item_name: "iron_sword".to_owned(),
                    count: 3,
                }],
                valid: true,
            },
        }
    }

    fn contract_block_event_fixture() -> ContractProtocolBlockEvent {
        ContractProtocolBlockEvent::Updated {
            old_block: None,
            new_block: Some(ContractProtocolBlockSnapshot {
                position: ContractBlockPosition { x: 3, y: 64, z: -2 },
                name: "stone".to_owned(),
                state_id: 42,
                properties: [(
                    "axis".to_owned(),
                    ContractBlockPropertyValue::String("y".to_owned()),
                )]
                .into_iter()
                .collect(),
                collision_shapes: vec![[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]],
                transparent_hint: false,
                bounding_box: ContractBlockBoundingBox::Block,
            }),
        }
    }

    fn contract_sound_fixture() -> ContractProtocolSoundPayload {
        ContractProtocolSoundPayload {
            event_type: ContractHeardSoundType::Heard,
            sound_key: "minecraft:block.note_block.harp".to_owned(),
            sound_name: Some("note_block.harp".to_owned()),
            sound_id: Some(12),
            category: Some("blocks".to_owned()),
            source_position: ContractVec3Value {
                x: 1.5,
                y: 64.25,
                z: -2.0,
            },
            volume: 0.75,
            pitch: 1.25,
            protocol_source: ContractProtocolSoundSource::NamedSoundEffect,
        }
    }

    fn backend_block_fixture() -> ProtocolBlockSnapshot {
        ProtocolBlockSnapshot {
            position: BlockPosition { x: 3, y: 64, z: -2 },
            name: "stone".to_owned(),
            state_id: 42,
            properties: [("axis".to_owned(), "y".to_owned())].into_iter().collect(),
            collision_shapes: vec![[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]],
            transparent_hint: false,
            bounding_box: BlockBoundingBox::Block,
        }
    }

    #[test]
    fn observation_source_binds_epoch_and_returns_structured_stale_errors() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();
        assert_eq!(source.epoch(), 1);

        {
            let mut observation = handle.shared.observation.write();
            observation.snapshot = Some(observation_snapshot(1));
            observation.source = Some(FactSource::ServerObserved);
            observation
                .tracked_entities
                .push(observation_entity("entity-7"));
            observation.bump_generation();
        }

        let pose = source
            .self_pose()
            .expect("current source pose should not be stale");
        assert_eq!(pose.position.x, 1.0);
        assert_eq!(source.list_tracked_entities().unwrap().len(), 1);
        assert_eq!(
            source
                .read_block(ContractBlockPosition { x: 0, y: 64, z: 0 })
                .expect("current source block read should not be stale"),
            ContractBlockReadResult::Unloaded
        );

        handle.shared.begin_connection_attempt();
        assert_eq!(source.epoch(), 1, "old source must keep its bound epoch");
        let stale = BackendError::StaleEpoch {
            bound_epoch: 1,
            current_epoch: 2,
        };
        let stale_wire = serde_json::to_value(&stale).expect("stale error should be structured");
        assert_eq!(stale_wire["code"], "stale_epoch");
        assert_eq!(stale_wire["boundEpoch"], 1);
        assert_eq!(stale_wire["currentEpoch"], 2);
        assert_eq!(source.self_pose(), Err(stale.clone()));
        assert_eq!(source.list_tracked_players(), Err(stale.clone()));
        assert_eq!(source.list_tracked_entities(), Err(stale.clone()));
        assert_eq!(
            source.read_block(ContractBlockPosition { x: 0, y: 64, z: 0 }),
            Err(stale.clone())
        );
        assert_eq!(source.snapshot_source(), Err(stale));
        assert!(matches!(
            ProtocolObservationSource::subscribe(&source, Arc::new(NoopListener)),
            Err(BackendError::StaleEpoch {
                bound_epoch: 1,
                current_epoch: 2,
            })
        ));
    }

    #[test]
    fn self_pose_without_snapshot_is_stable_not_ready() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();
        let expected = BackendError::NotReady {
            state: "self_pose_unavailable".to_owned(),
        };
        assert_eq!(source.self_pose(), Err(expected.clone()));
        assert_eq!(source.self_pose(), Err(expected));
    }

    #[test]
    fn read_block_unloaded_early_return_rechecks_epoch() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();

        let result =
            source.read_block_with_post_read_hook(BlockPosition { x: 0, y: 64, z: 0 }, || {
                handle.shared.begin_connection_attempt();
            });
        assert!(matches!(
            result,
            Err(BackendError::StaleEpoch {
                bound_epoch: 1,
                current_epoch: 2,
            })
        ));
    }

    #[test]
    fn protocol_observation_trait_object_maps_pose_entity_block_and_viewport_dto() {
        let (_handle, concrete_source, _world) = ready_viewport_source();
        let source: Arc<dyn ProtocolObservationSource> = Arc::new(concrete_source);

        let pose = source
            .self_pose()
            .expect("trait object pose should be ready");
        assert_eq!(
            pose.position,
            ContractVec3Value {
                x: 1.0,
                y: 64.0,
                z: 2.0
            }
        );
        assert_eq!(
            pose.velocity,
            ContractVec3Value {
                x: -1.5,
                y: 0.25,
                z: 2.75
            }
        );
        assert_eq!(pose.yaw, 0.25_f32 as f64);
        assert_eq!(pose.pitch, -0.1_f32 as f64);

        let entities = source
            .list_tracked_entities()
            .expect("trait object entity list should be ready");
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].entity_key, "entity-7");
        assert_eq!(entities[0].velocity.x, -0.25_f32 as f64);
        assert_eq!(entities[0].head_yaw, Some(0.5_f32 as f64));
        assert_eq!(entities[0].equipment[0].slot, 2);
        assert_eq!(entities[0].equipment[0].count, 3);

        assert_eq!(
            source
                .read_block(ContractBlockPosition { x: 0, y: 64, z: 0 })
                .expect("trait object block read should be ready"),
            ContractBlockReadResult::Unloaded
        );
        assert_eq!(
            source
                .read_block(ContractBlockPosition {
                    x: 0,
                    y: 10_000,
                    z: 0
                })
                .expect("out-of-world block read should be explicit"),
            ContractBlockReadResult::OutOfWorld
        );

        let converted = contract_block_snapshot(backend_block_fixture());
        assert_eq!(
            converted.position,
            ContractBlockPosition { x: 3, y: 64, z: -2 }
        );
        assert_eq!(converted.name, "stone");
        assert_eq!(converted.state_id, 42);
        assert_eq!(
            converted.properties["axis"],
            ContractBlockPropertyValue::String("y".to_owned())
        );
        assert_eq!(
            converted.collision_shapes,
            vec![[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]]
        );
        assert!(!converted.transparent_hint);
        assert_eq!(converted.bounding_box, ContractBlockBoundingBox::Block);
    }

    #[tokio::test]
    async fn protocol_observation_trait_object_delegates_atomic_viewport() {
        let (_handle, concrete_source, _world) = ready_viewport_source();
        let source: Arc<dyn ProtocolObservationSource> = Arc::new(concrete_source);
        let read = source
            .read_viewport(no_deadline_control())
            .await
            .expect("trait object viewport should delegate to atomic implementation");
        assert_eq!(read.projection.frame.self_pose.position, [1.0, 64.0, 2.0]);
        assert_eq!(read.source, ContractFactSource::ServerObserved);
        assert!(read.revision > 0);
    }

    #[test]
    fn observation_subscription_filters_kind_and_epoch_without_background_tasks() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        handle.shared.set_dimension("minecraft:overworld");
        let source = handle.observation_source();
        let (old_listener, old_events) = recording_listener();
        let mut old_subscription =
            ProtocolObservationSource::subscribe(&source, old_listener).expect("subscribe");
        assert_eq!(handle.shared.observation_subscribers.lock().len(), 1);

        for kind in [
            BackendEventKind::Entity,
            BackendEventKind::Block,
            BackendEventKind::Sound,
        ] {
            emit_test_fact(&handle, kind);
        }
        for kind in [
            BackendEventKind::Lifecycle,
            BackendEventKind::Chat,
            BackendEventKind::SelfState,
            BackendEventKind::World,
            BackendEventKind::PlayerList,
            BackendEventKind::SnapshotChanged,
            BackendEventKind::Overflow,
        ] {
            emit_test_fact(&handle, kind);
        }

        let observed = old_events.lock();
        assert_eq!(observed.len(), 3);
        assert_eq!(
            observed.iter().map(contract_event_kind).collect::<Vec<_>>(),
            vec![
                ContractBackendEventKind::Entity,
                ContractBackendEventKind::Block,
                ContractBackendEventKind::Sound
            ]
        );
        drop(observed);

        handle.shared.begin_connection_attempt();
        emit_test_fact(&handle, BackendEventKind::Entity);
        assert_eq!(old_events.lock().len(), 3);

        let new_source = handle.observation_source();
        let (new_listener, new_events) = recording_listener();
        let mut new_subscription = ProtocolObservationSource::subscribe(&new_source, new_listener)
            .expect("new epoch source subscribes");
        emit_test_fact(&handle, BackendEventKind::Block);
        assert_eq!(old_events.lock().len(), 3);
        assert_eq!(new_events.lock().len(), 1);
        assert!(matches!(
            &new_events.lock()[0],
            ObservationEvent::Block(event) if event.connection_epoch == 2
        ));

        old_subscription.unsubscribe();
        new_subscription.unsubscribe();
    }

    #[test]
    fn observation_subscription_registration_rechecks_epoch_after_reconnect_hook() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();
        let result = source.subscribe_with_post_register_hook(Arc::new(NoopListener), || {
            handle.shared.begin_connection_attempt();
        });
        assert!(matches!(
            result,
            Err(BackendError::StaleEpoch {
                bound_epoch: 1,
                current_epoch: 2,
            })
        ));
        assert_eq!(handle.shared.observation_subscribers.lock().len(), 0);

        let new_source = handle.observation_source();
        let subscription =
            ProtocolObservationSource::subscribe(&new_source, Arc::new(NoopListener))
                .expect("current epoch subscription should succeed");
        assert!(!subscription.is_closed());
        drop(subscription);
        assert_eq!(handle.shared.observation_subscribers.lock().len(), 0);
    }

    #[test]
    fn observation_subscription_unsubscribe_and_drop_release_listener_registry() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();
        let (listener, events) = recording_listener();
        let mut unsubscribed =
            ProtocolObservationSource::subscribe(&source, listener).expect("subscribe");
        assert_eq!(handle.shared.observation_subscribers.lock().len(), 1);
        unsubscribed.unsubscribe();
        unsubscribed.unsubscribe();
        assert!(unsubscribed.is_closed());
        assert_eq!(handle.shared.observation_subscribers.lock().len(), 0);
        emit_test_fact(&handle, BackendEventKind::Entity);
        assert!(
            events.lock().is_empty(),
            "unsubscribe must prevent delivery"
        );

        let (dropped_listener, _dropped_events) = recording_listener();
        let dropped = ProtocolObservationSource::subscribe(&source, dropped_listener)
            .expect("second subscription");
        assert_eq!(handle.shared.observation_subscribers.lock().len(), 1);
        drop(dropped);
        assert_eq!(handle.shared.observation_subscribers.lock().len(), 0);
    }

    #[test]
    fn observation_events_convert_entity_block_sound_to_v2_typed_payloads() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        handle.shared.set_dimension("minecraft:the_nether");
        let source = handle.observation_source();
        let (listener, events) = recording_listener();
        let _subscription =
            ProtocolObservationSource::subscribe(&source, listener).expect("subscribe");

        let expected_entity = contract_entity_event_fixture();
        let expected_block = contract_block_event_fixture();
        let expected_sound = contract_sound_fixture();
        let raw_entity = emit_and_capture(
            &handle,
            BackendEventPayload::Entity(expected_entity.clone()),
        );
        let raw_block =
            emit_and_capture(&handle, BackendEventPayload::Block(expected_block.clone()));
        let raw_sound =
            emit_and_capture(&handle, BackendEventPayload::Sound(expected_sound.clone()));

        let observed = events.lock();
        assert_eq!(observed.len(), 3);
        match &observed[0] {
            ObservationEvent::Entity(event) => {
                assert_metadata(&raw_entity, event, ContractBackendEventKind::Entity);
                assert_eq!(event.payload, expected_entity);
            }
            other => panic!("expected typed entity event, got {other:?}"),
        }
        match &observed[1] {
            ObservationEvent::Block(event) => {
                assert_metadata(&raw_block, event, ContractBackendEventKind::Block);
                assert_eq!(event.payload, expected_block);
            }
            other => panic!("expected typed block event, got {other:?}"),
        }
        match &observed[2] {
            ObservationEvent::Sound(event) => {
                assert_metadata(&raw_sound, event, ContractBackendEventKind::Sound);
                assert_eq!(event.payload, expected_sound);
            }
            other => panic!("expected typed sound event, got {other:?}"),
        }
    }

    #[test]
    fn typed_observation_payload_is_direct_and_kind_bound() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();
        let (listener, events) = recording_listener();
        let _subscription =
            ProtocolObservationSource::subscribe(&source, listener).expect("subscribe");

        assert!(events.lock().is_empty());
        emit_test_fact(&handle, BackendEventKind::Entity);
        assert_eq!(events.lock().len(), 1);
    }

    #[test]
    fn callback_panic_isolated_from_later_listeners_and_events() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();
        let _panic_subscription =
            ProtocolObservationSource::subscribe(&source, Arc::new(PanicListener))
                .expect("panic listener subscription should succeed");
        let (listener, events) = recording_listener();
        let _recording_subscription =
            ProtocolObservationSource::subscribe(&source, listener).expect("subscribe");

        emit_test_fact(&handle, BackendEventKind::Entity);
        emit_test_fact(&handle, BackendEventKind::Block);
        assert_eq!(events.lock().len(), 2);
    }

    #[test]
    fn callback_runs_outside_registry_lock_and_can_read_and_resubscribe() {
        let (handle, source, _world) = ready_viewport_source();
        let pose = Arc::new(parking_lot::Mutex::new(None));
        let block = Arc::new(parking_lot::Mutex::new(None));
        let listener = Arc::new(ReentrantListener {
            source: source.clone(),
            invoked: AtomicBool::new(false),
            pose: pose.clone(),
            block: block.clone(),
            nested_subscription_succeeded: AtomicBool::new(false),
        });
        let _subscription =
            ProtocolObservationSource::subscribe(&source, listener.clone()).expect("subscribe");

        emit_test_fact(&handle, BackendEventKind::Entity);
        assert!(listener
            .nested_subscription_succeeded
            .load(Ordering::SeqCst));
        assert!(pose.lock().as_ref().is_some_and(Result::is_ok));
        assert_eq!(
            block.lock().as_ref(),
            Some(&Ok(ContractBlockReadResult::Unloaded))
        );
    }

    #[test]
    fn unsubscribe_waits_for_active_callback_and_returns_before_no_new_delivery() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let calls = Arc::new(AtomicUsize::new(0));
        let listener = Arc::new(BlockingListener {
            entered: entered.clone(),
            release: release.clone(),
            calls: calls.clone(),
        });
        let subscription = ProtocolObservationSource::subscribe(&source, listener)
            .expect("blocking subscription should succeed");
        let holder = Arc::new(StdMutex::new(Some(subscription)));

        let emitting_handle = handle.clone();
        let emit_thread = thread::spawn(move || {
            emit_test_fact(&emitting_handle, BackendEventKind::Entity);
        });
        entered.wait();

        let (unsubscribed_tx, unsubscribed_rx) = std_mpsc::channel();
        let unsubscribe_holder = holder.clone();
        let unsubscribe_thread = thread::spawn(move || {
            let mut subscription = unsubscribe_holder
                .lock()
                .expect("subscription mutex should not be poisoned")
                .take()
                .expect("subscription should be owned by unsubscribe thread");
            subscription.unsubscribe();
            unsubscribed_tx
                .send(())
                .expect("unsubscribe completion should be observable");
        });
        assert!(unsubscribed_rx
            .recv_timeout(StdDuration::from_millis(100))
            .is_err());

        release.wait();
        unsubscribed_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("unsubscribe should return after active callback finishes");
        emit_thread.join().expect("event thread should not panic");
        unsubscribe_thread
            .join()
            .expect("unsubscribe thread should not panic");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(handle.shared.observation_subscribers.lock().len(), 0);

        emit_test_fact(&handle, BackendEventKind::Block);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn listener_a_can_unsubscribe_reserved_listener_b_without_deadlock_or_delivery() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();
        let b_calls = Arc::new(AtomicUsize::new(0));
        let b_holder = Arc::new(StdMutex::new(None));
        let a_calls = Arc::new(AtomicUsize::new(0));
        let a_listener = Arc::new(AUnsubscribesBListener {
            b_subscription: b_holder.clone(),
            calls: a_calls.clone(),
        });
        let _a_subscription = ProtocolObservationSource::subscribe(&source, a_listener)
            .expect("A subscription should succeed");
        let b_listener = Arc::new(BlockingListener {
            entered: Arc::new(Barrier::new(1)),
            release: Arc::new(Barrier::new(1)),
            calls: b_calls.clone(),
        });
        let b_subscription = ProtocolObservationSource::subscribe(&source, b_listener)
            .expect("B subscription should succeed");
        *b_holder
            .lock()
            .expect("B subscription mutex should not be poisoned") = Some(b_subscription);

        let emitting_handle = handle.clone();
        let (emit_finished_tx, emit_finished_rx) = std_mpsc::channel();
        let emit_thread = thread::spawn(move || {
            emit_test_fact(&emitting_handle, BackendEventKind::Entity);
            emit_finished_tx
                .send(())
                .expect("emit completion should be observable");
        });
        emit_finished_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("A unsubscribing reserved B must not deadlock dispatch");
        emit_thread.join().expect("event thread should not panic");
        assert_eq!(a_calls.load(Ordering::SeqCst), 1);
        assert_eq!(b_calls.load(Ordering::SeqCst), 0);
        assert_eq!(handle.shared.observation_subscribers.lock().len(), 1);

        emit_test_fact(&handle, BackendEventKind::Block);
        assert_eq!(b_calls.load(Ordering::SeqCst), 0);
    }
}
