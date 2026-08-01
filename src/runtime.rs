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
use serde_json::json;
use tokio::sync::{mpsc, Notify};

use crate::{
    protocol::{
        now_utc, BackendCommand, BackendCommandEnvelope, BackendEventEnvelope, BackendEventKind,
        FactSource, MotorDirection, BACKEND_COMMAND_PROTOCOL,
    },
    snapshot::{
        block_snapshot, capture, capture_pose, capture_tracked_entities, BlockPosition,
        BlockReadResult, MinecraftSnapshotV1, PoseSnapshot, ProtocolEntitySnapshot,
        TrackedPlayerSnapshot, Vec3Value,
    },
    viewport::{project as project_viewport, ViewportOptions, ViewportProjection},
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
        }
    }

    fn new_attempt(&mut self) {
        self.connection_epoch += 1;
        self.connection_attempt_id = format!("attempt-{}", self.connection_epoch);
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
        let event = BackendEventEnvelope::new(
            format!("event-{}", self.next_id),
            kind,
            self.process_session_id.clone(),
            self.connection_epoch,
            self.connection_attempt_id.clone(),
            self.world_id.clone(),
            source,
            payload,
        );
        // stdout 是跨进程事件流边界；每行一个完整 JSON 信封。
        match serde_json::to_string(&event) {
            Ok(line) => println!("{line}"),
            Err(error) => eprintln!("事件编码失败：{error}"),
        }
        event
    }
}

struct SharedRuntime {
    writer: parking_lot::Mutex<EventWriter>,
    swarm: parking_lot::Mutex<Option<Swarm>>,
    shutdown: Arc<Notify>,
    config: RunConfig,
    commands: parking_lot::Mutex<VecDeque<BackendCommandEnvelope>>,
    subscribers: parking_lot::Mutex<Vec<mpsc::UnboundedSender<BackendEventEnvelope>>>,
    world: parking_lot::Mutex<Option<Arc<parking_lot::RwLock<azalea::world::World>>>>,
    reported_dimension: parking_lot::Mutex<Option<String>>,
    snapshot: parking_lot::Mutex<Option<MinecraftSnapshotV1>>,
    snapshot_source: parking_lot::Mutex<Option<FactSource>>,
    tracked_entities: parking_lot::Mutex<Vec<ProtocolEntitySnapshot>>,
    snapshot_revision: AtomicU64,
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
            swarm: parking_lot::Mutex::new(None),
            shutdown: Arc::new(Notify::new()),
            config,
            commands: parking_lot::Mutex::new(VecDeque::new()),
            subscribers: parking_lot::Mutex::new(Vec::new()),
            world: parking_lot::Mutex::new(None),
            reported_dimension: parking_lot::Mutex::new(None),
            snapshot: parking_lot::Mutex::new(None),
            snapshot_source: parking_lot::Mutex::new(None),
            tracked_entities: parking_lot::Mutex::new(Vec::new()),
            snapshot_revision: AtomicU64::new(0),
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
        let event = self.writer.lock().emit(kind, source, payload);
        let mut subscribers = self.subscribers.lock();
        subscribers.retain(|subscriber| subscriber.send(event.clone()).is_ok());
    }

    fn new_attempt(&self) {
        self.writer.lock().new_attempt();
    }

    fn context(&self) -> (String, u64, String) {
        self.writer.lock().context()
    }

    fn connection_epoch(&self) -> u64 {
        self.writer.lock().connection_epoch
    }

    fn set_swarm(&self, swarm: Swarm) {
        *self.swarm.lock() = Some(swarm);
    }

    fn set_world(&self, world: Arc<parking_lot::RwLock<azalea::world::World>>) {
        *self.world.lock() = Some(world);
    }

    fn clear_observations(&self) {
        *self.world.lock() = None;
        *self.reported_dimension.lock() = None;
        *self.snapshot.lock() = None;
        *self.snapshot_source.lock() = None;
        self.tracked_entities.lock().clear();
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
        *self.tracked_entities.lock() = capture_tracked_entities(bot);
        let mut current = self.snapshot.lock();
        let changed = current
            .as_ref()
            .is_none_or(|previous| !previous.same_state_as(&candidate));
        if force || changed {
            self.snapshot_revision
                .store(next_revision, Ordering::Release);
            *current = Some(candidate.clone());
            *self.snapshot_source.lock() = Some(source);
            Some(candidate)
        } else {
            None
        }
    }

    fn stored_snapshot(&self) -> Option<MinecraftSnapshotV1> {
        self.snapshot.lock().clone()
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
        *self.shared.snapshot_source.lock()
    }

    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<BackendEventEnvelope> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.shared.subscribers.lock().push(sender);
        receiver
    }

    pub fn observation_source(&self) -> RuntimeObservationSource {
        RuntimeObservationSource {
            shared: self.shared.clone(),
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

/// 对齐 MineIntent `ProtocolObservationSource` 的只读最小观察面。
#[derive(Clone)]
pub struct RuntimeObservationSource {
    shared: Arc<SharedRuntime>,
}

impl RuntimeObservationSource {
    pub fn epoch(&self) -> u64 {
        self.shared.context().1
    }

    pub fn self_pose(&self) -> Option<PoseSnapshot> {
        self.shared
            .snapshot
            .lock()
            .as_ref()
            .map(|snapshot| PoseSnapshot {
                position: snapshot.self_snapshot.position.clone(),
                velocity: snapshot.self_snapshot.velocity.clone(),
                yaw: snapshot.self_snapshot.yaw,
                pitch: snapshot.self_snapshot.pitch,
                on_ground: snapshot.self_snapshot.on_ground,
            })
    }

    pub fn snapshot_source(&self) -> Option<FactSource> {
        *self.shared.snapshot_source.lock()
    }

    pub fn list_tracked_players(&self) -> Vec<TrackedPlayerSnapshot> {
        self.shared
            .snapshot
            .lock()
            .as_ref()
            .map(|snapshot| snapshot.tracked_players.clone())
            .unwrap_or_default()
    }

    pub fn list_tracked_entities(&self) -> Vec<ProtocolEntitySnapshot> {
        self.shared.tracked_entities.lock().clone()
    }

    /// 对齐 MineIntent viewport 的只读投影；所有坐标仍是 Minecraft 世界绝对坐标。
    ///
    /// 投影不会把本地缓存的方块直接宣称为可见：它会对视锥内候选执行暴露面和
    /// 遮挡射线判断。调用方如需携带事实来源，应同时读取 `snapshot_source()`。
    pub fn viewport(
        &self,
        options: &ViewportOptions,
    ) -> Result<Option<ViewportProjection>, String> {
        let Some(pose) = self.self_pose() else {
            return Ok(None);
        };
        let entities = self.list_tracked_entities();
        let Some(world) = self.shared.world.lock().clone() else {
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
    }

    /// 读取已加载世界中的绝对方块状态；结果不等于视线可见性。
    ///
    /// 上层 viewport 应基于 `transparentHint`、碰撞/轮廓几何和观察者姿态
    /// 做射线或暴露面判断，避免把“客户端缓存里有数据”误报成“玩家看到了”。
    pub fn read_block(&self, position: BlockPosition) -> BlockReadResult {
        let Some(world) = self.shared.world.lock().clone() else {
            return BlockReadResult::Unloaded;
        };
        let world = world.read();
        read_block_from_world(&world, position)
    }

    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<BackendEventEnvelope> {
        let (sender, receiver) = mpsc::unbounded_channel();
        self.shared.subscribers.lock().push(sender);
        receiver
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
        let dimension = event.name.to_string();
        let previous = state
            .shared
            .reported_dimension
            .lock()
            .replace(dimension.clone());
        if let Some(previous) = previous {
            if previous != dimension {
                state.shared.emit(
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
            if !shared.attempt_epoch_reserved.swap(false, Ordering::AcqRel) {
                shared.new_attempt();
            }
            shared.disconnect_reported.store(false, Ordering::Release);
            // 新 epoch 的服务端事实尚未到达前，不暴露上一条连接的世界缓存。
            shared.clear_observations();
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
            shared.emit(
                BackendEventKind::Lifecycle,
                FactSource::ServerObserved,
                json!({
                    "type":"ready",
                    "snapshotRevision":snapshot.as_ref().map_or(0, |value| value.snapshot_revision)
                }),
            );
            if let Some(snapshot) = snapshot {
                shared
                    .reported_dimension
                    .lock()
                    .replace(snapshot.world.dimension.clone());
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
            shared.new_attempt();
            shared.attempt_epoch_reserved.store(true, Ordering::Release);
            shared.disconnect_reported.store(false, Ordering::Release);
            shared.emit(
                BackendEventKind::Lifecycle,
                FactSource::Commanded,
                json!({"type":"reconnect_requested"}),
            );
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
    shared.emit(
        BackendEventKind::Lifecycle,
        FactSource::Commanded,
        json!({"type":"connection_requested", "attempt":1, "username":config.username, "host":config.host, "port":config.port}),
    );
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
    use super::*;

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
}
