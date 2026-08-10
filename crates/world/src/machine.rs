//! 接入模块的连接机器：azalea 客户端的生命周期、tick 快照循环与聊天面。
//!
//! v1 范围：重连政策固定 `Never`（断线=Disconnected 相，不换纪元）；
//! 声音/伤害窗暂空（生产者随后落位）；写口只有聊天（运动/手随后）。
//!
//! 线程模型：机器独占一个线程（tokio current_thread + LocalSet，azalea 需要），
//! 对外全部经共享状态交流——快照 latest-wins、聊天出站走队列在 tick 内执行
//! （ECS 只在客户端事件回调里触碰，不跨线程直写）。

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use azalea::{
    accept_resource_packs::AcceptResourcePacksPlugin,
    app::{App, AppExit, Plugin, PluginGroup, Update},
    auto_reconnect::AutoReconnectPlugin,
    bot::DefaultBotPlugins,
    ecs::message::MessageWriter,
    entity::{
        dimensions::EntityDimensions, inventory::Inventory as InventoryComponent, Dead,
        EntityKindComponent, EntityUuid, LoadedBy, LocalEntity, LookDirection, Physics,
        Pose as PoseComponent, Position,
    },
    player::GameProfileComponent,
    prelude::{bevy_ecs, Account, Component, Resource},
    protocol::address::{ResolvedAddr, ServerAddr},
    protocol::packets::game::{c_game_event, ClientboundGamePacket},
    swarm::{DefaultSwarmPlugins, Swarm, SwarmBuilder, SwarmEvent},
    world::WorldName,
    Client, DefaultPlugins, Event,
};
use azalea::ecs::system::Res;
use parking_lot::{Mutex, RwLock};
use tokio::sync::{oneshot, watch, Notify};

use crate::{
    ChatContent, ChatEntry, ChatPosition, ConnectionPhase, EntitySnapshot, Epoch, ExperienceState,
    FactSource, Inventory, InventorySlot, PlayerListEntry, PlayerRef, SelfState, SnapshotSource,
    TickSnapshot, Vec3Value, Window, WorldMeta, CHAT_WINDOW_LINES,
};

/// 连接配置。v1 只有离线身份、重连固定 Never。
#[derive(Clone, Debug)]
pub struct ConnectionConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
}

impl ConnectionConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.host.trim().is_empty() {
            return Err("服务器 host 不能为空".to_owned());
        }
        if self.port == 0 {
            return Err("服务器 port 不能为 0".to_owned());
        }
        if self.username.is_empty()
            || self.username.len() > 16
            || !self
                .username
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err("offline 用户名必须是 1–16 个 ASCII 字母、数字或下划线".to_owned());
        }
        Ok(())
    }
}

struct PendingChat {
    line: String,
    ack: oneshot::Sender<Result<(), String>>,
}

/// 机器与外界的共享面。纯状态转换都在这里，可脱离 azalea 单测。
pub(crate) struct Inner {
    latest: RwLock<Arc<TickSnapshot>>,
    ticked_tx: watch::Sender<u64>,
    chat_window: Mutex<VecDeque<ChatEntry>>,
    pending_chat: Mutex<Vec<PendingChat>>,
    stopping: AtomicBool,
    shutdown: Notify,
    tick: AtomicU64,
    chat_seq: AtomicU64,
    day_time: AtomicU64,
    /// (rain_level, thunder_level)。
    weather: Mutex<(f32, f32)>,
    dimension: Mutex<String>,
}

const EPOCH: Epoch = Epoch(1);

impl Inner {
    fn new() -> Self {
        let (ticked_tx, _) = watch::channel(0);
        Self {
            latest: RwLock::new(Arc::new(TickSnapshot::empty(
                EPOCH,
                0,
                ConnectionPhase::Connecting,
            ))),
            ticked_tx,
            chat_window: Mutex::new(VecDeque::new()),
            pending_chat: Mutex::new(Vec::new()),
            stopping: AtomicBool::new(false),
            shutdown: Notify::new(),
            tick: AtomicU64::new(0),
            chat_seq: AtomicU64::new(0),
            day_time: AtomicU64::new(0),
            weather: Mutex::new((0.0, 0.0)),
            dimension: Mutex::new("minecraft:overworld".to_owned()),
        }
    }

    fn publish(&self, snapshot: TickSnapshot) {
        // 停机后在途回调不得把终态改写回 Ready/Disconnected；
        // Stopped 相自己（stop 发布的那次）放行。
        if self.stopping.load(Ordering::Acquire)
            && !matches!(snapshot.phase, ConnectionPhase::Stopped { .. })
        {
            return;
        }
        *self.latest.write() = Arc::new(snapshot);
        // 通知合并：错过十次只醒一次，醒后读方自己取 latest。
        self.ticked_tx.send_modify(|counter| *counter += 1);
    }

    /// 非就绪相的快照：连接事实 + 聊天窗（窗是机器的记忆，不随相清空）。
    fn publish_phase(&self, phase: ConnectionPhase) {
        let mut snapshot =
            TickSnapshot::empty(EPOCH, self.tick.load(Ordering::Acquire), phase);
        snapshot.chat = self.chat_window_now();
        self.publish(snapshot);
    }

    fn chat_window_now(&self) -> Window<ChatEntry> {
        Window {
            entries: self.chat_window.lock().iter().cloned().collect(),
        }
    }

    fn push_chat(&self, sender: Option<(String, Option<String>)>, plain_text: String) {
        let entry = ChatEntry {
            seq: self.chat_seq.fetch_add(1, Ordering::AcqRel),
            tick: self.tick.load(Ordering::Acquire),
            occurred_at: SystemTime::now(),
            source: FactSource::ServerObserved,
            sender: sender.map(|(username, uuid)| PlayerRef { username, uuid }),
            content: ChatContent {
                plain_text,
                position: Some(ChatPosition::Chat),
                verified: None,
            },
        };
        let mut window = self.chat_window.lock();
        window.push_back(entry);
        while window.len() > CHAT_WINDOW_LINES {
            window.pop_front();
        }
    }

    fn apply_set_time(&self, overworld_total_ticks: u64) {
        self.day_time
            .store(overworld_total_ticks, Ordering::Release);
    }

    fn apply_game_event(&self, event: c_game_event::EventType, param: f32) {
        let mut weather = self.weather.lock();
        match event {
            c_game_event::EventType::StartRaining => weather.0 = 1.0,
            c_game_event::EventType::StopRaining => weather.0 = 0.0,
            c_game_event::EventType::RainLevelChange => weather.0 = param,
            c_game_event::EventType::ThunderLevelChange => weather.1 = param,
            _ => {}
        }
    }

    fn world_meta_now(&self) -> WorldMeta {
        let (rain_level, thunder_level) = *self.weather.lock();
        WorldMeta {
            dimension: self.dimension.lock().clone(),
            day_time: self.day_time.load(Ordering::Acquire),
            rain_level,
            thunder_level,
        }
    }

    /// 出站聊天入队；tick 回调里执行。机器已停/断线时立即拒绝。
    ///
    /// 检查与插入在同一把 pending 锁内：排空方（断线/停机）同样持这把锁，
    /// 所以任何入队要么先于排空（被排空如实拒绝），要么后于排空（此时
    /// 相/停机标志已可见，进不了队列）——没有错过排空的第三种命运。
    fn enqueue_chat(&self, line: String) -> oneshot::Receiver<Result<(), String>> {
        let (ack, receiver) = oneshot::channel();
        let mut pending = self.pending_chat.lock();
        let phase_ready = matches!(self.latest.read().phase, ConnectionPhase::Ready);
        if self.stopping.load(Ordering::Acquire) || !phase_ready {
            drop(pending);
            let _ = ack.send(Err("尚未连接到世界，无法发言".to_owned()));
            return receiver;
        }
        pending.push(PendingChat { line, ack });
        receiver
    }

    fn fail_all_pending_chat(&self, reason: &str) {
        for pending in self.pending_chat.lock().drain(..) {
            let _ = pending.ack.send(Err(reason.to_owned()));
        }
    }
}

/// 接入模块：一次连接的所有权句柄。
pub struct Module {
    inner: Arc<Inner>,
    done: Mutex<Option<oneshot::Receiver<()>>>,
}

impl Module {
    /// 启动连接。立即返回；就绪与否观察快照相（或 [`Module::wait_ready`]）。
    pub fn start(config: ConnectionConfig) -> Result<Self, String> {
        config.validate()?;
        let inner = Arc::new(Inner::new());
        let thread_inner = inner.clone();
        let (done_tx, done_rx) = oneshot::channel();
        std::thread::Builder::new()
            .name("world-machine".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("接入机器线程的 tokio runtime 构建失败");
                let local = tokio::task::LocalSet::new();
                local.block_on(&runtime, run_swarm(thread_inner.clone(), config));
                thread_inner.fail_all_pending_chat("连接已结束");
                let _ = done_tx.send(());
            })
            .map_err(|error| format!("接入机器线程启动失败：{error}"))?;
        Ok(Self {
            inner,
            done: Mutex::new(Some(done_rx)),
        })
    }

    /// 等到快照相变为 Ready；超时返回 Err。
    pub async fn wait_ready(&self, timeout: Duration) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut ticked = self.inner.ticked_tx.subscribe();
        loop {
            match &self.inner.latest.read().phase {
                ConnectionPhase::Ready => return Ok(()),
                ConnectionPhase::Disconnected { reason } => {
                    return Err(format!("连接失败：{reason}"))
                }
                ConnectionPhase::Stopped { reason } => {
                    return Err(format!("连接已停止：{reason}"))
                }
                ConnectionPhase::Connecting => {}
            }
            tokio::select! {
                changed = ticked.changed() => {
                    if changed.is_err() {
                        return Err("接入机器已退出".to_owned());
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    return Err("等待进入世界超时".to_owned());
                }
            }
        }
    }

    /// 停止：断开连接、合流机器线程。完成 = 无活执行流。
    pub async fn stop(&self, reason: &str) -> Result<(), String> {
        self.inner.stopping.store(true, Ordering::Release);
        self.inner.shutdown.notify_waiters();
        self.inner
            .publish_phase(ConnectionPhase::Stopped {
                reason: reason.to_owned(),
            });
        // 停机标志先行，再排空：入队与排空同锁，晚到的入队看得见标志。
        self.inner.fail_all_pending_chat("正在停机");
        let Some(done) = self.done.lock().take() else {
            return Ok(());
        };
        match tokio::time::timeout(Duration::from_secs(10), done).await {
            Ok(_) => Ok(()),
            Err(_) => Err("接入机器线程 10 秒内未合流".to_owned()),
        }
    }

    /// 睡到快照被替换（新 tick 落地或相变化）。watch 语义，天然合并。
    pub async fn ticked(&self) {
        let mut receiver = self.inner.ticked_tx.subscribe();
        let _ = receiver.changed().await;
    }

    /// 聊天出站：一行 = 一次原版输入循环，`/` 开头由 azalea 按原版语义路由为命令。
    pub async fn send_chat_line(&self, line: &str) -> Result<(), String> {
        let receiver = self.inner.enqueue_chat(line.to_owned());
        receiver
            .await
            .unwrap_or_else(|_| Err("连接已结束".to_owned()))
    }

    /// 聊天窗读取：最近 count 条，旧在前新在后。
    pub fn recent_chat(&self, count: usize) -> Vec<String> {
        let window = self.inner.chat_window.lock();
        let skip = window.len().saturating_sub(count);
        window
            .iter()
            .skip(skip)
            .map(|entry| match &entry.sender {
                Some(sender) => format!("{}: {}", sender.username, entry.content.plain_text),
                None => entry.content.plain_text.clone(),
            })
            .collect()
    }
}

impl SnapshotSource for Module {
    fn latest(&self) -> Arc<TickSnapshot> {
        self.inner.latest.read().clone()
    }
}

#[derive(Clone, Component)]
struct BotState {
    inner: Arc<Inner>,
}

impl Default for BotState {
    fn default() -> Self {
        Self {
            inner: Arc::new(Inner::new()),
        }
    }
}

#[derive(Clone, Resource)]
struct SwarmState {
    inner: Arc<Inner>,
}

impl Default for SwarmState {
    fn default() -> Self {
        Self {
            inner: Arc::new(Inner::new()),
        }
    }
}

/// 在 azalea 自己的 schedule 内发送退出消息，避免跨任务写消息与
/// Bevy 双缓冲时序竞争。
struct MachineShutdownPlugin;

impl Plugin for MachineShutdownPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, emit_app_exit_when_stopping);
    }
}

fn emit_app_exit_when_stopping(mut app_exit: MessageWriter<AppExit>, state: Res<SwarmState>) {
    if state.inner.stopping.load(Ordering::Acquire) {
        app_exit.write(AppExit::Success);
    }
}

async fn run_swarm(inner: Arc<Inner>, config: ConnectionConfig) {
    let socket: SocketAddr = match format!("{}:{}", config.host, config.port).parse() {
        Ok(socket) => socket,
        Err(error) => {
            inner.publish_phase(ConnectionPhase::Disconnected {
                reason: format!("服务器地址无效：{error}"),
            });
            return;
        }
    };
    let address = ResolvedAddr {
        server: ServerAddr::from(socket),
        socket,
    };
    let account = Account::offline(&config.username);
    let bot_state = BotState {
        inner: inner.clone(),
    };
    let swarm_state = SwarmState {
        inner: inner.clone(),
    };
    let plugins = (
        DefaultPlugins.build(),
        // v1 保留自动重生：没有任何复活路径时死亡即永久（实盘死在虚空里
        // 只会一直坠落）。"死亡作为要保持的事实"如何进产品，随死亡处理裁定。
        DefaultBotPlugins
            .build()
            .disable::<AcceptResourcePacksPlugin>()
            .disable::<AutoReconnectPlugin>(),
        MachineShutdownPlugin,
        DefaultSwarmPlugins,
    );
    let shutdown = inner.clone();
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
        _ = shutdown.shutdown.notified() => {
            // 先让 SwarmBuilder 的 AppExit 路径清理；select 丢弃 start future 后
            // 由本线程 runtime 回收剩余任务。
            // 已知风险（上游未修）：azalea 在 AppExit 清空 ECS 后，残留事件
            // 处理可在持 ECS 写锁时重入读锁（username 路径）自死锁。此时本
            // 线程卡死，Module::stop 的 10 秒合流超时会如实报错；companion
            // 进程退出时由操作系统回收该线程。嵌入式使用者需自担此泄漏。
        }
    }
}

async fn handle_swarm(_swarm: Swarm, event: SwarmEvent, state: SwarmState) {
    if let SwarmEvent::Disconnect(_account, _join_opts, _token) = event {
        // 重连政策 Never：断线是要保持的事实，不是要修复的故障。
        if !state.inner.stopping.load(Ordering::Acquire) {
            state.inner.publish_phase(ConnectionPhase::Disconnected {
                reason: "与服务器的连接已断开".to_owned(),
            });
        }
        state.inner.fail_all_pending_chat("连接已断开");
    }
}

async fn handle_client(bot: Client, event: Event, state: BotState) {
    let inner = &state.inner;
    if inner.stopping.load(Ordering::Acquire) {
        return;
    }
    match event {
        Event::Spawn => {
            let dimension = bot
                .try_query_self::<Option<&WorldName>, _>(|world_name| {
                    world_name.map(ToString::to_string)
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| "minecraft:overworld".to_owned());
            *inner.dimension.lock() = dimension;
            if let Some(snapshot) = assemble_snapshot(inner, &bot) {
                inner.publish(snapshot);
            }
        }
        Event::Chat(packet) => {
            let sender = packet
                .sender()
                .map(|username| (username, packet.sender_uuid().map(|uuid| uuid.to_string())));
            inner.push_chat(sender, packet.content());
        }
        Event::Packet(packet) => match &*packet {
            ClientboundGamePacket::SetTime(set_time) => {
                // 时钟表的键是服务端注册表动态 id；原版主世界只有一个昼夜钟，
                // 取首个条目。多时钟服务器的按键解析随 registry 读取落位。
                if let Some(clock) = set_time.clock_updates.values().next() {
                    inner.apply_set_time(clock.total_ticks);
                }
            }
            ClientboundGamePacket::GameEvent(game_event) => {
                inner.apply_game_event(game_event.event, game_event.param);
            }
            _ => {}
        },
        Event::Disconnect(reason) => {
            inner.publish_phase(ConnectionPhase::Disconnected {
                reason: reason
                    .map(|text| text.to_string())
                    .unwrap_or_else(|| "与服务器的连接已断开".to_owned()),
            });
            inner.fail_all_pending_chat("连接已断开");
        }
        Event::Tick => {
            inner.tick.fetch_add(1, Ordering::AcqRel);
            // 出站聊天在 ECS 回调里执行：不跨线程触碰客户端。
            let pending: Vec<PendingChat> = inner.pending_chat.lock().drain(..).collect();
            for pending_chat in pending {
                bot.chat(&pending_chat.line);
                let _ = pending_chat.ack.send(Ok(()));
            }
            if let Some(snapshot) = assemble_snapshot(inner, &bot) {
                inner.publish(snapshot);
            }
        }
        _ => {}
    }
}

/// 从 ECS 组装一帧快照。断线/换维度瞬间本地玩家实体可能已被移除——
/// 读不到就返回 None，不伪造坐标。
fn assemble_snapshot(inner: &Inner, bot: &Client) -> Option<TickSnapshot> {
    let pose = bot
        .try_query_self::<(&Position, &Physics, &LookDirection), _>(|(position, physics, look)| {
            (
                Vec3Value {
                    x: position.x,
                    y: position.y,
                    z: position.z,
                },
                Vec3Value {
                    x: physics.velocity.x,
                    y: physics.velocity.y,
                    z: physics.velocity.z,
                },
                f64::from(look.y_rot()),
                f64::from(look.x_rot()),
                physics.on_ground(),
            )
        })
        .ok()?;
    let (position, velocity, yaw, pitch, on_ground) = pose;

    let health = bot
        .get_component::<azalea::entity::metadata::Health>()
        .map(|value| f64::from(value.0))
        .unwrap_or(0.0);
    let hunger = bot.hunger();
    let experience = bot.experience();
    let alive = bot.get_component::<Dead>().is_none();

    let self_state = SelfState {
        entity_key: bot.uuid().to_string(),
        username: bot.username(),
        position,
        velocity,
        yaw,
        pitch,
        on_ground,
        alive,
        health,
        food: f64::from(hunger.food),
        food_saturation: f64::from(hunger.saturation),
        oxygen: None,
        experience: Some(ExperienceState {
            level: experience.level,
            progress: f64::from(experience.progress),
            total: u64::from(experience.total),
        }),
        // 状态效果读取随后补；空集是"没读"不是"没有"，渲染层无效果时不着墨。
        effects: Vec::new(),
        inventory: capture_inventory(bot),
    };

    Some(TickSnapshot {
        epoch: EPOCH,
        tick: inner.tick.load(Ordering::Acquire),
        captured_at: SystemTime::now(),
        phase: ConnectionPhase::Ready,
        world_meta: inner.world_meta_now(),
        self_state,
        entities: capture_entities(bot),
        players: capture_players(bot),
        chat: inner.chat_window_now(),
        // 声音/伤害窗：生产者未落位，先空。
        sounds: Window::default(),
        damage: Window::default(),
    })
}

fn capture_inventory(bot: &Client) -> Inventory {
    bot.get_component::<InventoryComponent>()
        .map(|inventory| {
            let slots = inventory
                .menu()
                .slots()
                .into_iter()
                .enumerate()
                .filter_map(|(slot, item)| {
                    if item.is_empty() {
                        None
                    } else {
                        Some(InventorySlot {
                            slot: slot as u32,
                            item_name: canonical_registry_name(&item.kind().to_string()),
                            count: item.count() as u32,
                            metadata: None,
                            durability_used: None,
                        })
                    }
                })
                .collect();
            Inventory {
                selected_hotbar_slot: inventory.selected_hotbar_slot,
                slots,
            }
        })
        .unwrap_or_default()
}

fn capture_players(bot: &Client) -> Vec<PlayerListEntry> {
    let mut players: Vec<_> = bot
        .tab_list()
        .into_iter()
        .map(|(uuid, info)| {
            let entity = bot.entity_by_uuid(uuid);
            let observed = entity.as_ref().and_then(|entity| {
                entity
                    .try_query_self::<(&Position, &LookDirection), _>(|(position, look)| {
                        (
                            Vec3Value {
                                x: position.x,
                                y: position.y,
                                z: position.z,
                            },
                            f64::from(look.y_rot()),
                            f64::from(look.x_rot()),
                        )
                    })
                    .ok()
            });
            let (position, yaw, pitch) = match observed {
                Some((position, yaw, pitch)) => (Some(position), Some(yaw), Some(pitch)),
                None => (None, None, None),
            };
            PlayerListEntry {
                player_key: uuid.to_string(),
                uuid: Some(uuid.to_string()),
                username: info.profile.name,
                listed: true,
                entity_tracked: entity.is_some(),
                position,
                yaw,
                pitch,
                held_item_name: None,
            }
        })
        .collect();
    players.sort_by(|left, right| left.player_key.cmp(&right.player_key));
    players
}

/// 读取当前客户端已知、仍在 ECS 中的实体；自身已在 self_state，不重复列出。
fn capture_entities(bot: &Client) -> Vec<EntitySnapshot> {
    let Ok(owner_world) = bot.try_query_self::<&WorldName, _>(|world_name| world_name.clone())
    else {
        return Vec::new();
    };
    let mut ecs = bot.ecs.write();
    let mut query = ecs.query::<(
        azalea::ecs::entity::Entity,
        &azalea::core::entity_id::MinecraftEntityId,
        &LoadedBy,
        &WorldName,
        &Position,
        &Physics,
        &LookDirection,
        Option<&EntityUuid>,
        Option<&EntityKindComponent>,
        Option<&GameProfileComponent>,
        Option<&Dead>,
        Option<&LocalEntity>,
        Option<&EntityDimensions>,
        Option<&PoseComponent>,
    )>();
    let mut entities: Vec<_> = query
        .iter(&ecs)
        .filter_map(
            |(
                _entity,
                protocol_entity_id,
                loaded_by,
                world_name,
                position,
                physics,
                look,
                uuid,
                kind,
                profile,
                dead,
                local,
                dimensions,
                pose,
            )| {
                if local.is_some() || !loaded_by.contains(&bot.entity) || world_name != &owner_world
                {
                    return None;
                }
                let uuid = uuid.map(|value| (**value).to_string());
                Some(EntitySnapshot {
                    entity_key: format!("{}:{}", EPOCH.0, **protocol_entity_id),
                    protocol_entity_id: **protocol_entity_id,
                    entity_type: kind
                        .map(|value| canonical_registry_name(&(**value).to_string()))
                        .unwrap_or_else(|| "unknown".to_owned()),
                    name: None,
                    username: profile.map(|value| value.name.clone()),
                    uuid,
                    position: Vec3Value {
                        x: position.x,
                        y: position.y,
                        z: position.z,
                    },
                    velocity: Vec3Value {
                        x: physics.velocity.x,
                        y: physics.velocity.y,
                        z: physics.velocity.z,
                    },
                    yaw: f64::from(look.y_rot()),
                    pitch: f64::from(look.x_rot()),
                    head_yaw: None,
                    width: dimensions.map_or(0.6, |value| f64::from(value.width)),
                    height: dimensions.map_or(1.8, |value| f64::from(value.height)),
                    on_ground: physics.on_ground(),
                    pose: pose.map(|value| format!("{value:?}").to_ascii_lowercase()),
                    held_item_name: None,
                    equipment: Vec::new(),
                    valid: dead.is_none(),
                })
            },
        )
        .collect();
    entities.sort_by(|left, right| left.entity_key.cmp(&right.entity_key));
    entities
}

/// azalea 注册名规范化：剥 `minecraft:` 前缀，与旧契约同法。
fn canonical_registry_name(name: &str) -> String {
    name.strip_prefix("minecraft:").unwrap_or(name).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_validation_rejects_bad_host_port_username() {
        let good = ConnectionConfig {
            host: "127.0.0.1".to_owned(),
            port: 25565,
            username: "xiao_ming".to_owned(),
        };
        assert!(good.validate().is_ok());

        for (host, port, username) in [
            ("", 25565, "bot"),
            ("127.0.0.1", 0, "bot"),
            ("127.0.0.1", 25565, ""),
            ("127.0.0.1", 25565, "名字里有中文"),
            ("127.0.0.1", 25565, "seventeen_letters_x"),
        ] {
            let config = ConnectionConfig {
                host: host.to_owned(),
                port,
                username: username.to_owned(),
            };
            assert!(config.validate().is_err(), "{config:?} 该被拒绝");
        }
    }

    #[test]
    fn chat_window_keeps_at_most_the_vanilla_line_count() {
        let inner = Inner::new();
        for index in 0..150 {
            inner.push_chat(Some(("alice".to_owned(), None)), format!("第 {index} 句"));
        }
        let window = inner.chat_window_now();
        assert_eq!(window.entries.len(), CHAT_WINDOW_LINES);
        assert_eq!(window.entries[0].content.plain_text, "第 50 句");
        assert_eq!(window.entries[99].content.plain_text, "第 149 句");
        // seq 单调且不随窗口逐出重置：同 tick 多条消息靠它做恰好一次消费。
        assert_eq!(window.entries[0].seq, 50);
        assert_eq!(window.entries[99].seq, 149);
    }

    #[test]
    fn weather_and_time_transitions_follow_game_events() {
        let inner = Inner::new();
        inner.apply_set_time(37_500);
        inner.apply_game_event(c_game_event::EventType::StartRaining, 0.0);
        let meta = inner.world_meta_now();
        assert_eq!(meta.day_time, 37_500);
        assert_eq!(meta.rain_level, 1.0);

        inner.apply_game_event(c_game_event::EventType::RainLevelChange, 0.4);
        inner.apply_game_event(c_game_event::EventType::ThunderLevelChange, 0.7);
        let meta = inner.world_meta_now();
        assert_eq!(meta.rain_level, 0.4);
        assert_eq!(meta.thunder_level, 0.7);

        inner.apply_game_event(c_game_event::EventType::StopRaining, 0.0);
        assert_eq!(inner.world_meta_now().rain_level, 0.0);
    }

    #[test]
    fn phase_snapshots_keep_the_chat_window_and_notify() {
        let inner = Inner::new();
        let ticked = inner.ticked_tx.subscribe();
        inner.push_chat(None, "服务器广播".to_owned());
        // 停机后 Ready 快照不得改写状态。
        inner.stopping.store(true, Ordering::Release);
        inner.publish(TickSnapshot::empty(EPOCH, 9, ConnectionPhase::Ready));
        assert!(
            !matches!(inner.latest.read().phase, ConnectionPhase::Ready),
            "停机期间的在途 Ready 快照应被丢弃"
        );
        inner.stopping.store(false, Ordering::Release);
        inner.publish_phase(ConnectionPhase::Disconnected {
            reason: "网络断开".to_owned(),
        });

        let latest = inner.latest.read().clone();
        assert!(matches!(&latest.phase, ConnectionPhase::Disconnected { reason } if reason == "网络断开"));
        assert_eq!(latest.chat.entries.len(), 1);
        assert!(ticked.has_changed().unwrap());
    }

    #[tokio::test]
    async fn chat_enqueued_before_ready_is_rejected_immediately() {
        let inner = Inner::new();
        let receiver = inner.enqueue_chat("你好".to_owned());
        let outcome = receiver.await.expect("ack 应送达");
        assert!(outcome.is_err());
        assert!(inner.pending_chat.lock().is_empty());
    }
}
