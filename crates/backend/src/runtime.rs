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
    parse_block_property_value, BackendError, BackendEventEnvelope as ContractBackendEventEnvelope,
    BackendEventKind as ContractBackendEventKind,
    BackendEventMetadata as ContractBackendEventMetadata, BackendFailure, BackendFailureCode,
    BlockBoundingBox as ContractBlockBoundingBox, BlockPosition as ContractBlockPosition,
    BlockReadResult as ContractBlockReadResult, BoxFuture, DirectedViewportError,
    DirectedViewportProjection, EntityEquipmentSnapshot as ContractEntityEquipmentSnapshot,
    FactSource as ContractFactSource, ObservationEvent, ObservationEventListener, OperationControl,
    ProtocolBlockEvent as ContractProtocolBlockEvent,
    ProtocolBlockSnapshot as ContractProtocolBlockSnapshot,
    ProtocolEntityEvent as ContractProtocolEntityEvent,
    ProtocolEntitySnapshot as ContractProtocolEntitySnapshot, ProtocolObservationSource,
    ProtocolSoundPayload as ContractProtocolSoundPayload, SelfPose as ContractSelfPose,
    Subscription, Vec3Value as ContractVec3Value, ViewportBlock as ContractViewportBlock,
    ViewportFrame as ContractViewportFrame, ViewportLegend as ContractViewportLegend,
    ViewportProjection as ContractViewportProjection, ViewportRead as ContractViewportRead,
    ViewportSelfPose as ContractViewportSelfPose, VisibleBlocksView as ContractVisibleBlocksView,
    VisibleEntitiesView as ContractVisibleEntitiesView,
    VisibleEntityView as ContractVisibleEntityView,
};
use serde_json::json;
use tokio::sync::{mpsc, Notify};

use crate::{
    protocol::{
        now_utc, BackendCommand, BackendCommandEnvelope, BackendEventEnvelope, BackendEventKind,
        FactSource, MotorDirection, BACKEND_COMMAND_PROTOCOL,
    },
    snapshot::{
        block_snapshot, capture, capture_pose, capture_tracked_entities, BlockBoundingBox,
        BlockPosition, BlockReadResult, MinecraftSnapshotV1, PoseSnapshot, ProtocolBlockSnapshot,
        ProtocolEntitySnapshot, TrackedPlayerSnapshot, Vec3Value,
    },
    viewport::{
        project as project_viewport, project_directed as project_directed_viewport,
        project_with_checkpoint as project_viewport_with_checkpoint, ViewportBlock,
        ViewportOptions, ViewportProjection,
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

    fn emit(
        &mut self,
        kind: BackendEventKind,
        source: FactSource,
        payload: serde_json::Value,
    ) -> BackendEventEnvelope {
        self.next_id += 1;
        let event = BackendEventEnvelope::new_with_dimension(
            format!("event-{}", self.next_id),
            kind,
            self.process_session_id.clone(),
            self.connection_epoch,
            self.connection_attempt_id.clone(),
            self.world_id.clone(),
            self.dimension.clone(),
            source,
            payload,
        );
        event
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

struct SharedRuntime {
    writer: parking_lot::Mutex<EventWriter>,
    event_dispatch: parking_lot::Mutex<EventDispatchState>,
    swarm: parking_lot::Mutex<Option<Swarm>>,
    shutdown: Arc<Notify>,
    config: RunConfig,
    commands: parking_lot::Mutex<VecDeque<BackendCommandEnvelope>>,
    subscribers: parking_lot::Mutex<Vec<mpsc::UnboundedSender<BackendEventEnvelope>>>,
    observation_subscribers: parking_lot::Mutex<Vec<ObservationSubscriber>>,
    next_observation_subscription_id: AtomicU64,
    observation: parking_lot::RwLock<ObservationState>,
    reported_dimension: parking_lot::Mutex<Option<String>>,
    snapshot_revision: AtomicU64,
    viewport_revision: AtomicU64,
    lifecycle_revision: AtomicU64,
    command_revision: AtomicU64,
    tick_revision: AtomicU64,
    movement_generation: AtomicU64,
    active_movement: AtomicBool,
    active_movement_id: parking_lot::Mutex<Option<String>>,
    timer_started: AtomicBool,
    initial_chat_sent: AtomicBool,
    death_reported: AtomicBool,
    disconnect_reported: AtomicBool,
    reconnect_pending: AtomicBool,
    attempt_epoch_reserved: AtomicBool,
    ready: AtomicBool,
    stopping: AtomicBool,
}

impl SharedRuntime {
    fn new(config: RunConfig) -> Self {
        Self {
            writer: parking_lot::Mutex::new(EventWriter::new(&config.world_id)),
            event_dispatch: parking_lot::Mutex::new(EventDispatchState::default()),
            swarm: parking_lot::Mutex::new(None),
            shutdown: Arc::new(Notify::new()),
            config,
            commands: parking_lot::Mutex::new(VecDeque::new()),
            subscribers: parking_lot::Mutex::new(Vec::new()),
            observation_subscribers: parking_lot::Mutex::new(Vec::new()),
            next_observation_subscription_id: AtomicU64::new(0),
            observation: parking_lot::RwLock::new(ObservationState::default()),
            reported_dimension: parking_lot::Mutex::new(None),
            snapshot_revision: AtomicU64::new(0),
            viewport_revision: AtomicU64::new(0),
            lifecycle_revision: AtomicU64::new(0),
            command_revision: AtomicU64::new(0),
            tick_revision: AtomicU64::new(0),
            movement_generation: AtomicU64::new(0),
            active_movement: AtomicBool::new(false),
            active_movement_id: parking_lot::Mutex::new(None),
            timer_started: AtomicBool::new(false),
            initial_chat_sent: AtomicBool::new(false),
            death_reported: AtomicBool::new(false),
            disconnect_reported: AtomicBool::new(false),
            reconnect_pending: AtomicBool::new(false),
            attempt_epoch_reserved: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
        }
    }

    fn emit(&self, kind: BackendEventKind, source: FactSource, payload: serde_json::Value) {
        if matches!(kind, BackendEventKind::Lifecycle) {
            self.lifecycle_revision.fetch_add(1, Ordering::AcqRel);
        }
        let should_drain = {
            let mut dispatch = self.event_dispatch.lock();
            let event = {
                let mut writer = self.writer.lock();
                writer.emit(kind, source, payload)
            };
            dispatch.enqueue(event)
        };
        if should_drain {
            self.drain_events();
        }
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
        // stdout 是跨进程事件流边界；唯一 drainer 保证打印顺序与入队顺序一致。
        match serde_json::to_string(&event) {
            Ok(line) => println!("{line}"),
            Err(error) => eprintln!("事件编码失败：{error}"),
        }
        {
            let mut subscribers = self.subscribers.lock();
            subscribers.retain(|subscriber| subscriber.send(event.clone()).is_ok());
        }

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
    fn begin_connection_attempt(&self) {
        self.attempt_epoch_reserved.store(true, Ordering::Release);
        self.disconnect_reported.store(false, Ordering::Release);
        self.clear_observations();
        self.lifecycle_revision.fetch_add(1, Ordering::AcqRel);

        let should_drain = {
            let mut dispatch = self.event_dispatch.lock();
            let event = {
                let mut writer = self.writer.lock();
                writer.new_attempt();
                let attempt = writer.connection_epoch;
                writer.emit(
                    BackendEventKind::Lifecycle,
                    FactSource::Commanded,
                    json!({
                        "type":"connection_requested",
                        "attempt":attempt,
                        "username":self.config.username,
                        "host":self.config.host,
                        "port":self.config.port
                    }),
                )
            };
            dispatch.enqueue(event)
        };
        if should_drain {
            self.drain_events();
        }
    }

    /// `Event::Init` 消费连接发起前预留的身份，而不是再创建一个 epoch。
    /// 防御性 fallback 仍走同一入口，确保即使 Azalea 新增调用路径，也先有
    /// `connection_requested`，随后才发 transport 生命周期事件。
    fn consume_attempt_for_transport_init(&self) {
        if !self.attempt_epoch_reserved.swap(false, Ordering::AcqRel) {
            self.begin_connection_attempt();
            self.attempt_epoch_reserved.store(false, Ordering::Release);
        }
        self.disconnect_reported.store(false, Ordering::Release);
        self.clear_observations();
    }

    fn context(&self) -> (String, u64, String) {
        self.writer.lock().context()
    }

    fn set_dimension(&self, dimension: impl Into<String>) -> Option<String> {
        let dimension = dimension.into();
        self.writer.lock().set_dimension(dimension.clone());
        self.reported_dimension.lock().replace(dimension)
    }

    fn observe_dimension(&self, dimension: impl Into<String>) {
        let dimension = dimension.into();
        if let Some(previous) = self.set_dimension(dimension.clone()) {
            if previous != dimension {
                self.emit(
                    BackendEventKind::Lifecycle,
                    FactSource::ServerObserved,
                    json!({
                        "type":"dimension_changed",
                        "from":previous,
                        "to":dimension
                    }),
                );
            }
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

    fn set_swarm(&self, swarm: Swarm) {
        *self.swarm.lock() = Some(swarm);
    }

    fn set_world(&self, world: SharedWorld) {
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

    fn mark_disconnected(&self, reason: Option<String>) {
        self.ready.store(false, Ordering::Release);
        self.active_movement.store(false, Ordering::Release);
        *self.active_movement_id.lock() = None;
        self.movement_generation.fetch_add(1, Ordering::AcqRel);
        self.clear_observations();
        if !self.disconnect_reported.swap(true, Ordering::AcqRel) {
            self.emit(
                BackendEventKind::Lifecycle,
                FactSource::ServerObserved,
                json!({"type":"connection_closed", "reason":reason}),
            );
        }
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
        self.shutdown.notify_one();
    }

    fn enqueue_command(&self, command: BackendCommandEnvelope) {
        self.commands.lock().push_back(command);
    }

    fn take_commands(&self) -> Vec<BackendCommandEnvelope> {
        self.commands.lock().drain(..).collect()
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
        self.emit(
            BackendEventKind::SnapshotChanged,
            source,
            json!({"type":"snapshot", "snapshot":snapshot}),
        );
    }

    fn emit_predicted_pose(&self, bot: &Client, command_id: &str) {
        let Some(pose): Option<PoseSnapshot> = capture_pose(bot) else {
            return;
        };
        self.emit(
            BackendEventKind::Motor,
            FactSource::ClientPredicted,
            json!({"type":"predicted_pose", "commandId":command_id, "pose":pose}),
        );
    }

    fn initiate_stop(&self, reason: &str) {
        if self.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        self.emit(
            BackendEventKind::Lifecycle,
            FactSource::Commanded,
            json!({"type":"stopping", "reason":reason}),
        );
        let signal_sent = self.exit_swarm();
        self.emit(
            BackendEventKind::Lifecycle,
            FactSource::Commanded,
            json!({"type":"shutdown_requested", "swarmAvailable":signal_sent}),
        );
        self.request_shutdown();
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
        self.shared.enqueue_command(command);
        Ok(())
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

    /// 发送与主仓库 motor `lookRelative` 同语义的相对视角输入。
    pub fn look_relative(&self, yaw_degrees: f32, pitch_degrees: f32) -> Result<(), String> {
        self.send_command(self.next_command(BackendCommand::LookRelative {
            yaw_degrees,
            pitch_degrees,
        }))
    }

    /// 发送按键式移动输入；校验范围与主仓库 motor 的 50–1500ms 边界一致。
    pub fn move_input(
        &self,
        directions: Vec<MotorDirection>,
        duration_ms: u64,
        sprint: Option<bool>,
        jump: Option<bool>,
        crouch: Option<bool>,
    ) -> Result<(), String> {
        self.send_command(self.next_command(BackendCommand::Move {
            directions,
            duration_ms,
            sprint,
            jump,
            crouch,
        }))
    }

    /// 释放全部移动/跳跃/潜行输入。
    pub fn release_all(&self) -> Result<(), String> {
        self.send_command(self.next_command(BackendCommand::ReleaseAll))
    }

    /// 显式请求服务端执行重生；死亡后不会由运行时自动触发。
    pub fn respawn(&self) -> Result<(), String> {
        self.send_command(self.next_command(BackendCommand::Respawn))
    }

    /// 主动结束运行时；停止动作本身会写入 `commanded` 事件。
    pub fn stop(&self, reason: &str) {
        self.shared.initiate_stop(reason);
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
    /// NEW-08 `OutOfWorld` remains a separate error variant and is never converted to a reason.
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
        occurred_at: event.occurred_at.to_rfc3339(),
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
    let parse_error = |error: serde_json::Error| {
        eprintln!(
            "typed observation payload deferred: producer remains deferred; event_id={}; kind={:?}; error={}",
            event.id, event.kind, error
        );
    };

    match event.kind {
        BackendEventKind::Entity => {
            match serde_json::from_value::<ContractProtocolEntityEvent>(event.payload.clone()) {
                Ok(payload) => Some(ObservationEvent::Entity(ContractBackendEventEnvelope::new(
                    metadata,
                    contract_event_kind(event.kind),
                    source,
                    payload,
                ))),
                Err(error) => {
                    parse_error(error);
                    None
                }
            }
        }
        BackendEventKind::Block => {
            match serde_json::from_value::<ContractProtocolBlockEvent>(event.payload.clone()) {
                Ok(payload) => Some(ObservationEvent::Block(ContractBackendEventEnvelope::new(
                    metadata,
                    contract_event_kind(event.kind),
                    source,
                    payload,
                ))),
                Err(error) => {
                    parse_error(error);
                    None
                }
            }
        }
        BackendEventKind::Sound => {
            match serde_json::from_value::<ContractProtocolSoundPayload>(event.payload.clone()) {
                Ok(payload) => Some(ObservationEvent::Sound(ContractBackendEventEnvelope::new(
                    metadata,
                    contract_event_kind(event.kind),
                    source,
                    payload,
                ))),
                Err(error) => {
                    parse_error(error);
                    None
                }
            }
        }
        _ => None,
    }
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
    if block_position.y < world.chunks.min_y()
        || block_position.y >= world.chunks.min_y() + world.chunks.height() as i32
    {
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
    query: Query<Option<&azalea::InGameState>>,
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
    // 这是本地明确请求实际发出的协议包；只有后续 Spawn 才算服务端确认。
    let in_game = query
        .get(trigger.event().sent_by)
        .is_ok_and(|value| value.is_some());
    state.shared.emit(
        BackendEventKind::Lifecycle,
        FactSource::Commanded,
        json!({"type":"respawn_packet_dispatched", "inGameState":in_game}),
    );
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
        state.shared.emit(
            BackendEventKind::SelfState,
            FactSource::ServerObserved,
            json!({
                "type":"server_position_correction",
                "teleportId":packet.id,
                "position":Vec3Value {
                    x: packet.change.pos.x,
                    y: packet.change.pos.y,
                    z: packet.change.pos.z,
                },
                "velocity":Vec3Value {
                    x: packet.change.delta.x,
                    y: packet.change.delta.y,
                    z: packet.change.delta.z,
                },
                "yaw":packet.change.look_direction.y_rot(),
                "pitch":packet.change.look_direction.x_rot(),
                "relative":{
                    "x":packet.relative.x,
                    "y":packet.relative.y,
                    "z":packet.relative.z,
                    "yaw":packet.relative.y_rot,
                    "pitch":packet.relative.x_rot,
                    "deltaX":packet.relative.delta_x,
                    "deltaY":packet.relative.delta_y,
                    "deltaZ":packet.relative.delta_z,
                    "rotateDelta":packet.relative.rotate_delta
                }
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

fn handle_command(bot: &Client, shared: &Arc<SharedRuntime>, envelope: BackendCommandEnvelope) {
    let command_id = envelope.id;
    match envelope.command {
        BackendCommand::SendChat { message } => {
            bot.chat(message.clone());
            shared.emit(
                BackendEventKind::Chat,
                FactSource::Commanded,
                json!({"type":"sent", "commandId":command_id, "plainText":message}),
            );
        }
        BackendCommand::LookRelative {
            yaw_degrees,
            pitch_degrees,
        } => {
            let direction = bot.direction();
            bot.set_direction(
                direction.y_rot() - yaw_degrees,
                (direction.x_rot() - pitch_degrees).clamp(-90.0, 90.0),
            );
            shared.emit(
                BackendEventKind::Motor,
                FactSource::Commanded,
                json!({
                    "type":"look_relative",
                    "commandId":command_id,
                    "yawDegrees":yaw_degrees,
                    "pitchDegrees":pitch_degrees
                }),
            );
            shared.emit_predicted_pose(bot, &command_id);
        }
        BackendCommand::Move {
            directions,
            duration_ms,
            sprint,
            jump,
            crouch,
        } => {
            let direction = direction_for(&directions);
            let generation = shared.movement_generation.fetch_add(1, Ordering::AcqRel) + 1;
            shared.active_movement.store(true, Ordering::Release);
            *shared.active_movement_id.lock() = Some(command_id.clone());
            if sprint.unwrap_or(false) {
                if let Some(sprint_direction) = sprint_direction(direction) {
                    bot.sprint(sprint_direction);
                } else {
                    bot.walk(direction);
                }
            } else {
                bot.walk(direction);
            }
            if !try_set_movement_flags(bot, jump.unwrap_or(false), crouch.unwrap_or(false)) {
                return;
            }
            shared.emit(
                BackendEventKind::Motor,
                FactSource::Commanded,
                json!({
                    "type":"move_started",
                    "commandId":command_id,
                    "directions":directions,
                    "durationMs":duration_ms,
                    "sprint":sprint,
                    "jump":jump,
                    "crouch":crouch
                }),
            );
            shared.emit_predicted_pose(bot, &command_id);

            if duration_ms == 0 {
                bot.walk(WalkDirection::None);
                shared.active_movement.store(false, Ordering::Release);
                *shared.active_movement_id.lock() = None;
            } else {
                let bot_to_stop = bot.clone();
                let shared = shared.clone();
                let command_id = command_id.clone();
                tokio::task::spawn_local(async move {
                    tokio::time::sleep(Duration::from_millis(duration_ms)).await;
                    if shared.movement_generation.load(Ordering::Acquire) == generation
                        && !shared.stopping.load(Ordering::Acquire)
                    {
                        if try_set_movement_flags(&bot_to_stop, false, false) {
                            bot_to_stop.walk(WalkDirection::None);
                            shared.active_movement.store(false, Ordering::Release);
                            *shared.active_movement_id.lock() = None;
                            shared.emit(
                                BackendEventKind::Motor,
                                FactSource::Commanded,
                                json!({"type":"move_released", "commandId":command_id}),
                            );
                            shared.emit_predicted_pose(&bot_to_stop, &command_id);
                        }
                    }
                });
            }
        }
        BackendCommand::ReleaseAll => {
            shared.movement_generation.fetch_add(1, Ordering::AcqRel);
            shared.active_movement.store(false, Ordering::Release);
            *shared.active_movement_id.lock() = None;
            if !try_set_movement_flags(bot, false, false) {
                return;
            }
            bot.walk(WalkDirection::None);
            shared.emit(
                BackendEventKind::Motor,
                FactSource::Commanded,
                json!({"type":"released_all", "commandId":command_id}),
            );
            shared.emit_predicted_pose(bot, &command_id);
        }
        BackendCommand::Respawn => {
            // 服务端的死亡包与 waitingForRespawn 状态可能跨一个网络 tick；
            // 只延迟这一条已经明确请求的动作，避免请求在服务端状态切换前到达。
            // 仍走 Azalea 自带 RespawnPlugin 的消息链，保持实体绑定和 ECS 时序。
            let delayed_bot = bot.clone();
            let delayed_shared = shared.clone();
            tokio::task::spawn_local(async move {
                tokio::time::sleep(RESPAWN_SETTLE_DELAY).await;
                if delayed_shared.stopping.load(Ordering::Acquire)
                    || delayed_bot
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
            shared.emit(
                BackendEventKind::Lifecycle,
                FactSource::Commanded,
                json!({"type":"respawn_requested", "commandId":command_id}),
            );
        }
    }
}

fn process_pending_commands(bot: &Client, shared: &Arc<SharedRuntime>) {
    // 连接建立前的命令保留在队列中，避免把 chat/motor 静默丢在握手阶段。
    if !bot.logged_in() {
        return;
    }
    for command in shared.take_commands() {
        if !shared.ready.load(Ordering::Acquire)
            && !matches!(&command.command, BackendCommand::Respawn)
        {
            shared.enqueue_command(command);
            continue;
        }
        handle_command(bot, shared, command);
    }
}

async fn handle_client(bot: Client, event: Event, state: BotState) {
    let shared = &state.shared;
    if matches!(event, Event::Spawn | Event::Tick) {
        process_pending_commands(&bot, &state.shared);
    }
    match event {
        Event::Init => {
            // Swarm 重连在某些路径复用已有本地玩家事件发送器，不一定再次发出
            // Event::Init；重连调度器会预留 epoch，若 Init 到达则消费该预留，避免
            // 同一次握手被错误地记成两个 epoch。
            shared.consume_attempt_for_transport_init();
            shared.emit(
                BackendEventKind::Lifecycle,
                FactSource::ServerObserved,
                json!({"type":"transport_initialized"}),
            );
        }
        Event::Login => shared.emit(
            BackendEventKind::Lifecycle,
            FactSource::ServerObserved,
            json!({"type":"logged_in", "version":"26.1.2", "protocol":775}),
        ),
        Event::Spawn => {
            let was_dead = shared.death_reported.load(Ordering::Acquire);
            shared.ready.store(true, Ordering::Release);
            shared.death_reported.store(false, Ordering::Release);
            shared.set_world(bot.world());
            let snapshot = shared.refresh_snapshot(&bot, true, FactSource::ServerObserved);
            if let Some(snapshot) = snapshot.as_ref() {
                shared.set_dimension(snapshot.world.dimension.clone());
            }
            shared.emit(
                BackendEventKind::Lifecycle,
                FactSource::ServerObserved,
                json!({
                    "type":"ready",
                    "snapshotRevision":snapshot.as_ref().map_or(0, |value| value.snapshot_revision)
                }),
            );
            if let Some(snapshot) = snapshot {
                if was_dead {
                    shared.emit(
                        BackendEventKind::Lifecycle,
                        FactSource::ServerObserved,
                        json!({
                            "type":"respawned",
                            "dimension":snapshot.world.dimension
                        }),
                    );
                }
                shared.emit_snapshot(snapshot, FactSource::ServerObserved);
            }

            if !shared.initial_chat_sent.swap(true, Ordering::AcqRel) {
                if let Some(message) = shared.config.initial_chat.clone() {
                    bot.chat(message.clone());
                    shared.emit(
                        BackendEventKind::Chat,
                        FactSource::Commanded,
                        json!({"type":"sent", "plainText":message, "origin":"cli"}),
                    );
                }
            }

            if !shared.timer_started.swap(true, Ordering::AcqRel) {
                let duration = shared.config.duration;
                let shared = state.shared.clone();
                tokio::task::spawn_local(async move {
                    tokio::time::sleep(duration).await;
                    shared.initiate_stop("duration_elapsed");
                });
            }
        }
        Event::KeepAlive(id) => {
            shared.emit(
                BackendEventKind::KeepAlive,
                FactSource::ServerObserved,
                json!({"type":"received", "id":id}),
            );
            // azalea 在收到同一包的处理器中立即发送 ServerboundKeepAlive；这里把
            // 该主动协议动作单独记为 commanded，不把它混进服务端事实。
            shared.emit(
                BackendEventKind::KeepAlive,
                FactSource::Commanded,
                json!({"type":"acknowledged_by_azalea", "id":id}),
            );
        }
        Event::Chat(packet) => shared.emit(
            BackendEventKind::Chat,
            FactSource::ServerObserved,
            json!({
                "senderUsername": packet.sender(),
                "plainText": packet.content(),
                "rawText": packet.message().to_string(),
                "receivedAt": now_utc(),
            }),
        ),
        Event::Death(_) => {
            if !shared.death_reported.swap(true, Ordering::AcqRel) {
                shared.ready.store(false, Ordering::Release);
                shared.active_movement.store(false, Ordering::Release);
                *shared.active_movement_id.lock() = None;
                shared.movement_generation.fetch_add(1, Ordering::AcqRel);
                if try_set_movement_flags(&bot, false, false) {
                    bot.walk(WalkDirection::None);
                }
                shared.emit(
                    BackendEventKind::Lifecycle,
                    FactSource::ServerObserved,
                    json!({"type":"died"}),
                );
                if let Some(snapshot) =
                    shared.refresh_snapshot(&bot, true, FactSource::ServerObserved)
                {
                    shared.emit_snapshot(snapshot, FactSource::ServerObserved);
                }
            }
        }
        Event::Disconnect(reason) => {
            let reason = reason.map(|value| value.to_string());
            shared.mark_disconnected(reason);
            // Disconnect 会由 Azalea 同步移除本地玩家的运动组件；此处只
            // 更新运行时状态，不再向已失效的实体投递 walk/jump/crouch 消息。
        }
        Event::ConnectionFailed(error) => {
            shared.ready.store(false, Ordering::Release);
            shared.emit(
                BackendEventKind::Error,
                FactSource::ServerObserved,
                json!({"type":"connection_failed", "error":format!("{error:?}")}),
            );
            if shared.connection_epoch() > 1 {
                // 重连尝试可能早于 Paper 完成保存/重新监听；ConnectionFailed
                // 本身不触发 Azalea 的 SwarmEvent::Disconnect，因此显式断开这个
                // 空连接，让统一重连状态机继续按 delay 重试，而不是把一次拒绝
                // 误当成整个后端失败。
                bot.disconnect();
            } else {
                // 初次连接失败时没有可安全复用的已登录 client；让上层得到明确错误并结束。
                bot.exit();
            }
        }
        Event::AddPlayer(info) => shared.emit(
            BackendEventKind::PlayerList,
            FactSource::ServerObserved,
            json!({"type":"player_list_add", "uuid":info.uuid, "username":info.profile.name}),
        ),
        Event::RemovePlayer(info) => shared.emit(
            BackendEventKind::PlayerList,
            FactSource::ServerObserved,
            json!({"type":"player_list_remove", "uuid":info.uuid, "username":info.profile.name}),
        ),
        Event::UpdatePlayer(info) => shared.emit(
            BackendEventKind::PlayerList,
            FactSource::ServerObserved,
            json!({"type":"player_list_update", "uuid":info.uuid, "username":info.profile.name}),
        ),
        Event::ReceiveChunk(position) => shared.emit(
            BackendEventKind::Block,
            FactSource::ServerObserved,
            json!({"type":"chunk_loaded", "chunkX":position.x, "chunkZ":position.z}),
        ),
        Event::Tick => {
            if shared.ready.load(Ordering::Acquire) {
                let tick = shared.tick_revision.fetch_add(1, Ordering::AcqRel);
                if tick % 5 != 0 {
                    return;
                }
                if shared.active_movement.load(Ordering::Acquire) {
                    let command_id = shared
                        .active_movement_id
                        .lock()
                        .clone()
                        .unwrap_or_else(|| "movement-tick".to_owned());
                    shared.emit_predicted_pose(&bot, &command_id);
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
        shared.set_swarm(swarm.clone());
    }
    if let SwarmEvent::Disconnect(account, join_opts) = event {
        if shared.stopping.load(Ordering::Acquire)
            || shared.reconnect_pending.swap(true, Ordering::AcqRel)
        {
            return;
        }

        // SwarmEvent::Disconnect 是重连状态机的兜底边界：azalea 在复用
        // LocalPlayerEvents 时可能没有再发出 Event::Disconnect。
        shared.mark_disconnected(None);
        let delay = shared.config.reconnect_delay;
        shared.emit(
            BackendEventKind::Lifecycle,
            FactSource::Commanded,
            json!({"type":"reconnect_scheduled", "delayMs":delay.as_millis()}),
        );
        tokio::task::spawn_local(async move {
            tokio::time::sleep(delay).await;
            if shared.stopping.load(Ordering::Acquire) {
                shared.reconnect_pending.store(false, Ordering::Release);
                return;
            }
            shared.begin_connection_attempt();
            let state = BotState {
                shared: shared.clone(),
            };
            let _ = swarm.add_with_opts(&account, state, &join_opts).await;
            shared.reconnect_pending.store(false, Ordering::Release);
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
    shared.begin_connection_attempt();
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
        ProtocolSoundSource as ContractProtocolSoundSource,
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

    #[test]
    fn connection_request_preallocates_and_init_reuses_each_attempt_identity() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        assert!(events.try_recv().is_err());

        handle.shared.begin_connection_attempt();
        let first_request = events.try_recv().expect("首次连接请求事件");
        assert_eq!(first_request.connection_epoch, 1);
        assert_eq!(first_request.connection_attempt_id, "attempt-1");
        assert_eq!(first_request.payload["type"], "connection_requested");
        assert_eq!(first_request.payload["attempt"], 1);
        assert!(first_request.dimension.is_none());

        handle.shared.consume_attempt_for_transport_init();
        handle.shared.emit(
            BackendEventKind::Lifecycle,
            FactSource::ServerObserved,
            json!({"type":"transport_initialized"}),
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
        assert_eq!(second_request.payload["type"], "connection_requested");
        assert_eq!(second_request.payload["attempt"], 2);
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
            BackendEventKind::World,
            FactSource::ServerObserved,
            json!({"type":"world_observed"}),
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
        assert_eq!(changed.payload["type"], "dimension_changed");
        assert_eq!(changed.payload["from"], "minecraft:overworld");
        assert_eq!(changed.payload["to"], "minecraft:the_nether");
        assert_eq!(changed.dimension.as_deref(), Some("minecraft:the_nether"));
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
        assert_eq!(delivered[0].payload["type"], "animation");
        assert_eq!(delivered[1].payload["type"], "stopping");
        assert_eq!(delivered[1].payload["reason"], "callback-stop");
        assert_eq!(delivered[2].payload["type"], "shutdown_requested");
        assert_eq!(delivered[2].payload["swarmAvailable"], false);
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
                        BackendEventKind::World,
                        FactSource::ServerObserved,
                        json!({"producer":producer,"sequence":sequence}),
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
            assert_eq!(event.payload["type"], "connection_requested");
            assert_eq!(event.payload["attempt"], epoch);
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
    async fn read_directed_viewport_rejects_duplicates_and_out_of_world_without_reason_mapping() {
        let (_handle, source, _world) = ready_viewport_source();
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

        let handle = _handle;
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
        assert!(matches!(
            out_of_world,
            Err(DirectedViewportError::OutOfWorld { position })
                if position.y == 10_000
        ));
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
                BackendEventKind::Entity,
                FactSource::ServerObserved,
                valid_observation_payload(BackendEventKind::Entity),
            );
            self.handle.shared.emit(
                BackendEventKind::Block,
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

    fn valid_observation_payload(kind: BackendEventKind) -> serde_json::Value {
        match kind {
            BackendEventKind::Entity => {
                json!({"type":"animation", "entityKey":"entity-7", "animation":"swing"})
            }
            BackendEventKind::Block => {
                json!({"type":"chunk_loaded", "chunkX":3, "chunkZ":-4})
            }
            BackendEventKind::Sound => json!({
                "type":"heard",
                "soundKey":"minecraft:block.note_block.harp",
                "soundName":"note_block.harp",
                "soundId":12,
                "category":"blocks",
                "sourcePosition":{"x":1.5,"y":64.25,"z":-2.0},
                "volume":0.75,
                "pitch":1.25,
                "protocolSource":"named_sound_effect"
            }),
            _ => json!({"type":"ignored"}),
        }
    }

    fn emit_test_fact(handle: &RuntimeHandle, kind: BackendEventKind) {
        handle.shared.emit(
            kind,
            FactSource::ServerObserved,
            valid_observation_payload(kind),
        );
    }

    fn emit_and_capture(
        handle: &RuntimeHandle,
        kind: BackendEventKind,
        payload: serde_json::Value,
    ) -> BackendEventEnvelope {
        let mut events = handle.subscribe();
        handle
            .shared
            .emit(kind, FactSource::ServerObserved, payload);
        events
            .try_recv()
            .expect("global v1 event should be emitted for adapter fixture")
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
        assert_eq!(typed.occurred_at, raw.occurred_at.to_rfc3339());
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

        let result = source
            .read_block_with_post_read_hook(BlockPosition { x: 0, y: 64, z: 0 }, || {
                handle.shared.begin_connection_attempt()
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
            BackendEventKind::KeepAlive,
            BackendEventKind::Chat,
            BackendEventKind::SelfState,
            BackendEventKind::World,
            BackendEventKind::PlayerList,
            BackendEventKind::SnapshotChanged,
            BackendEventKind::Motor,
            BackendEventKind::Error,
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
            handle.shared.begin_connection_attempt()
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
            BackendEventKind::Entity,
            serde_json::to_value(&expected_entity).expect("entity payload should encode"),
        );
        let raw_block = emit_and_capture(
            &handle,
            BackendEventKind::Block,
            serde_json::to_value(&expected_block).expect("block payload should encode"),
        );
        let raw_sound = emit_and_capture(
            &handle,
            BackendEventKind::Sound,
            serde_json::to_value(&expected_sound).expect("sound payload should encode"),
        );

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
    fn malformed_observation_payload_is_fail_contained_and_does_not_fake_a_fact() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();
        let (listener, events) = recording_listener();
        let _subscription =
            ProtocolObservationSource::subscribe(&source, listener).expect("subscribe");

        emit_and_capture(
            &handle,
            BackendEventKind::Entity,
            json!({"type":"not_a_protocol_entity_event"}),
        );
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
