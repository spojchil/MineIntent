//! 接入模块的连接机器：azalea 客户端的生命周期、tick 快照循环与聊天面。
//!
//! v1 范围：重连政策固定 `Never`（断线=Disconnected 相，不换纪元）；
//! 声音窗暂空（生产者随后落位）。伤害窗每 tick 由生命对比生产；
//! 移动 job 追踪产出 jobs 窗（到达/顶替/停止/走完未达/卡住）。
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
use azalea::physics::collision::BlockWithShape;
use azalea::pathfinder::goals::BlockPosGoal;
use azalea::pathfinder::{ExecutingPath, Pathfinder, PathfinderClientExt};
use azalea::protocol::packets::game::s_player_action;
use azalea::{BlockPos, SprintDirection, WalkDirection};
use parking_lot::{Mutex, RwLock};
use tokio::sync::{oneshot, watch, Notify};

use crate::{
    ChatContent, ChatEntry, ChatPosition, ConnectionPhase, DamageEntry, EntitySnapshot, Epoch,
    ExperienceState, FactSource, Inventory, InventorySlot, JobEntry, JobKind, JobOutcome,
    PlayerListEntry, PlayerRef, SelfState, SnapshotSource, TickSnapshot, Vec3Value, Window,
    WorldMeta, CHAT_WINDOW_LINES,
};

/// 伤害窗条目上限。关注类窗口 ≥ 最长一轮时长；伤害事件稀疏，按条数封顶即可。
const DAMAGE_WINDOW_ENTRIES: usize = 100;
/// 任务窗条目上限。
const JOBS_WINDOW_ENTRIES: usize = 32;
/// 移动 job 起步宽限：下令后寻路器要过几个调度周期才可见（GotoEvent 是
/// Bevy 消息，跨 schedule 投递）；宽限内不判终局。
const MOVEMENT_ARM_GRACE_TICKS: u64 = 100;
/// 卡住通知阈值：与 azalea 自己的补路超时同量级（它 3–7 秒就会自救，
/// 超过 10 秒还没推进说明自救也没起色，值得让模型知道）。
const MOVEMENT_STALL_TICKS: usize = 200;

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

/// 写口动词：中间层各门的意图汇成一条队列，tick 回调内执行
/// （ECS 只在客户端事件回调里触碰，不跨线程直写）。
#[derive(Clone, Debug)]
pub enum DoorCommand {
    /// 一行聊天；`/` 开头由 azalea 按原版语义路由为命令。
    Chat(String),
    GoTo([f64; 3]),
    /// 朝当前面向直走 N 格（化归为寻路目标，机械终止交给寻路器）。
    Forward(f64),
    StopMoving,
    Jump,
    Sneak(bool),
    Sprint(bool),
    LookAt([f64; 3]),
    Face { yaw: f64, pitch: f64 },
    Attack { entity_key: String },
    Mine([i32; 3]),
    UseOnBlock([i32; 3]),
    UseOnEntity { entity_key: String },
    UseItem,
    /// 松手：停止挖掘并松开使用中的物品。
    ReleaseHand,
    DropItem { whole_stack: bool },
    SwapOffhand,
    SelectSlot(u8),
}

struct PendingCommand {
    command: DoorCommand,
    ack: oneshot::Sender<Result<(), String>>,
}

/// 在途的移动任务（单意图槽）。终局判定按 azalea 寻路器的可观察状态：
/// 成功到达时它把 goal 置 None（execute/mod.rs 目标达成分支）；走完未达时
/// ExecutingPath 移除但 goal 留 Some。我们自己下的停止/顶替在命令处就地标注。
struct MovementJob {
    destination: [i32; 3],
    started_tick: u64,
    /// 见过寻路器活动（计算中/执行中/goal 已挂）后才允许判终局，
    /// 避开 GotoEvent 尚未被调度的起步窗口。
    armed: bool,
    /// 卡住通知只发一次。
    stall_notified: bool,
}

/// 移动 job 轮询的行动结论（纯函数，可单测）。
#[derive(Debug, PartialEq, Eq)]
enum MovementPollStep {
    /// 保持现状（含宽限期内等待）。
    Keep,
    /// 寻路器已可见，进入武装状态。
    Arm,
    /// 任务终局：出窗并清槽。
    End(JobOutcome),
    /// 卡住通知（任务继续）。
    Stall,
}

/// 判定表输入：本 tick 的全部可观察事实。
/// `goal_some`/`calculating`/`executing` 是寻路器三个状态位；
/// `at_destination` = 自身所在方块 == 目的地（站在目的地时空路径也算到达）；
/// `grace_exceeded` = 起步宽限已过；`stalled_long` = 无推进 tick 数超阈值。
#[derive(Clone, Copy, Debug, Default)]
struct MovementPoll {
    armed: bool,
    goal_some: bool,
    calculating: bool,
    executing: bool,
    at_destination: bool,
    grace_exceeded: bool,
    stalled_long: bool,
    stall_notified: bool,
}

fn movement_poll_step(poll: MovementPoll) -> MovementPollStep {
    if !poll.armed {
        if poll.goal_some || poll.calculating || poll.executing {
            return MovementPollStep::Arm;
        }
        if poll.grace_exceeded {
            // 出发都没出发（消息丢失等罕见情形）——按走完未达收束，不装作还在走。
            return MovementPollStep::End(JobOutcome::PathEnded);
        }
        return MovementPollStep::Keep;
    }
    if !poll.calculating && !poll.executing {
        if !poll.goal_some {
            // 只有寻路器的目标达成分支会在无外停时清 goal。
            return MovementPollStep::End(JobOutcome::Arrived);
        }
        // goal 还挂着但执行已停：走完未达（不可达/局部路径尽头）。
        // 站在目的地上的空路径情形按到达算——goal.success 的判据就是方块相等。
        return MovementPollStep::End(if poll.at_destination {
            JobOutcome::Arrived
        } else {
            JobOutcome::PathEnded
        });
    }
    if poll.executing && poll.stalled_long && !poll.stall_notified {
        return MovementPollStep::Stall;
    }
    MovementPollStep::Keep
}

/// 机器与外界的共享面。纯状态转换都在这里，可脱离 azalea 单测。
pub(crate) struct Inner {
    latest: RwLock<Arc<TickSnapshot>>,
    ticked_tx: watch::Sender<u64>,
    chat_window: Mutex<VecDeque<ChatEntry>>,
    damage_window: Mutex<VecDeque<DamageEntry>>,
    jobs_window: Mutex<VecDeque<JobEntry>>,
    /// 上一 tick 的生命值；下降即产伤害条目。None = 尚无基线（首帧不产）。
    last_health: Mutex<Option<f64>>,
    /// 在途移动任务（单意图槽）。
    movement_job: Mutex<Option<MovementJob>>,
    pending: Mutex<Vec<PendingCommand>>,
    /// 一次性跳跃的复位标记：跳跃布尔保持一整 tick 后放开。
    jump_reset: AtomicBool,
    /// azalea 世界模型句柄（Spawn 登记）。方块读取走它的读锁，
    /// 可在任意线程进行——世界模型不是 ECS。
    world_handle: Mutex<Option<Arc<RwLock<azalea::world::World>>>>,
    stopping: AtomicBool,
    shutdown: Notify,
    tick: AtomicU64,
    /// 全部事实窗共用的单调到达序号（聊天/伤害/任务）：
    /// 跨窗可比先后，各窗游标互不干扰。
    fact_seq: AtomicU64,
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
            damage_window: Mutex::new(VecDeque::new()),
            jobs_window: Mutex::new(VecDeque::new()),
            last_health: Mutex::new(None),
            movement_job: Mutex::new(None),
            pending: Mutex::new(Vec::new()),
            jump_reset: AtomicBool::new(false),
            world_handle: Mutex::new(None),
            stopping: AtomicBool::new(false),
            shutdown: Notify::new(),
            tick: AtomicU64::new(0),
            fact_seq: AtomicU64::new(0),
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

    /// 非就绪相的快照：连接事实 + 各事实窗（窗是机器的记忆，不随相清空）。
    fn publish_phase(&self, phase: ConnectionPhase) {
        let mut snapshot =
            TickSnapshot::empty(EPOCH, self.tick.load(Ordering::Acquire), phase);
        snapshot.chat = self.chat_window_now();
        snapshot.damage = self.damage_window_now();
        snapshot.jobs = self.jobs_window_now();
        self.publish(snapshot);
    }

    fn chat_window_now(&self) -> Window<ChatEntry> {
        Window {
            entries: self.chat_window.lock().iter().cloned().collect(),
        }
    }

    fn damage_window_now(&self) -> Window<DamageEntry> {
        Window {
            entries: self.damage_window.lock().iter().cloned().collect(),
        }
    }

    fn jobs_window_now(&self) -> Window<JobEntry> {
        Window {
            entries: self.jobs_window.lock().iter().cloned().collect(),
        }
    }

    /// 生命对比产伤害条目。回升（治疗/重生）只更新基线，不产条目。
    ///
    /// 由 `ClientboundSetHealth` 包驱动，不做每 tick 采样：自动重生把
    /// 死亡瞬间的 14→0→20 压进一个 tick 里，采样会整个错过 0（实测发生），
    /// 而每次 SetHealth 包都是一次权威变化，原版客户端也以它为准。
    fn track_health(&self, health: f64) {
        let mut last = self.last_health.lock();
        let previous = last.replace(health);
        let Some(previous) = previous else { return };
        // 阈值挡住浮点噪声；真实伤害最小半颗心（1.0）。
        if health < previous - 0.01 {
            let entry = DamageEntry {
                seq: self.fact_seq.fetch_add(1, Ordering::AcqRel),
                tick: self.tick.load(Ordering::Acquire),
                occurred_at: SystemTime::now(),
                health_before: previous as f32,
                health_after: health as f32,
                // SetHealth 不携带伤因；有伤因的事件源落位后再填。
                cause: None,
            };
            let mut window = self.damage_window.lock();
            window.push_back(entry);
            while window.len() > DAMAGE_WINDOW_ENTRIES {
                window.pop_front();
            }
        }
    }

    fn push_job(&self, destination: [i32; 3], outcome: JobOutcome) {
        let entry = JobEntry {
            seq: self.fact_seq.fetch_add(1, Ordering::AcqRel),
            tick: self.tick.load(Ordering::Acquire),
            occurred_at: SystemTime::now(),
            job: JobKind::MoveTo { destination },
            outcome,
        };
        let mut window = self.jobs_window.lock();
        window.push_back(entry);
        while window.len() > JOBS_WINDOW_ENTRIES {
            window.pop_front();
        }
    }

    /// 移动任务开槽：旧任务被顶替即出窗。
    fn begin_movement_job(&self, destination: [i32; 3]) {
        let mut slot = self.movement_job.lock();
        if let Some(job) = slot.take() {
            self.push_job(job.destination, JobOutcome::Replaced);
        }
        *slot = Some(MovementJob {
            destination,
            started_tick: self.tick.load(Ordering::Acquire),
            armed: false,
            stall_notified: false,
        });
    }

    /// 停止动词：在途任务如实出窗。没任务时不是错误。
    fn end_movement_job_stopped(&self) {
        if let Some(job) = self.movement_job.lock().take() {
            self.push_job(job.destination, JobOutcome::Stopped);
        }
    }

    fn push_chat(&self, sender: Option<(String, Option<String>)>, plain_text: String) {
        let entry = ChatEntry {
            seq: self.fact_seq.fetch_add(1, Ordering::AcqRel),
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

    /// 写口动词入队；tick 回调里执行。机器已停/断线时立即拒绝。
    ///
    /// 检查与插入在同一把 pending 锁内：排空方（断线/停机）同样持这把锁，
    /// 所以任何入队要么先于排空（被排空如实拒绝），要么后于排空（此时
    /// 相/停机标志已可见，进不了队列）——没有错过排空的第三种命运。
    fn enqueue_command(&self, command: DoorCommand) -> oneshot::Receiver<Result<(), String>> {
        let (ack, receiver) = oneshot::channel();
        let mut pending = self.pending.lock();
        let phase_ready = matches!(self.latest.read().phase, ConnectionPhase::Ready);
        if self.stopping.load(Ordering::Acquire) || !phase_ready {
            drop(pending);
            let _ = ack.send(Err("尚未连接到世界，无法行动".to_owned()));
            return receiver;
        }
        pending.push(PendingCommand { command, ack });
        receiver
    }

    fn fail_all_pending_chat(&self, reason: &str) {
        for pending in self.pending.lock().drain(..) {
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

    /// 执行一个写口动词：入队，tick 内执行，回执执行结论。
    pub async fn execute(&self, command: DoorCommand) -> Result<(), String> {
        let receiver = self.inner.enqueue_command(command);
        receiver
            .await
            .unwrap_or_else(|_| Err("连接已结束".to_owned()))
    }

    /// 聊天出站：一行 = 一次原版输入循环，`/` 开头由 azalea 按原版语义路由为命令。
    pub async fn send_chat_line(&self, line: &str) -> Result<(), String> {
        self.execute(DoorCommand::Chat(line.to_owned())).await
    }

    /// 全景视口投影：用最新快照的姿态与实体，方块走世界模型读锁
    /// （非 ECS，可在任意线程调用；计算量大，调用方自行放阻塞池）。
    pub fn scan(&self, options: &crate::ViewportOptions) -> Result<crate::ViewportProjection, String> {
        let snapshot = self.latest();
        if !matches!(snapshot.phase, ConnectionPhase::Ready) {
            return Err("尚未连接到世界，无法观察".to_owned());
        }
        let world = self
            .inner
            .world_handle
            .lock()
            .clone()
            .ok_or_else(|| "世界模型尚未就绪".to_owned())?;
        let world = world.read();
        let pose = crate::viewport::Pose {
            position: snapshot.self_state.position,
            yaw: snapshot.self_state.yaw,
            pitch: snapshot.self_state.pitch,
        };
        crate::viewport::project_with_reader(
            &pose,
            &snapshot.entities,
            crate::viewport::WorldReader::new(
                |position| probe_block_from_world(&world, position),
                |position| read_block_from_world(&world, position),
            ),
            options,
            || Ok(()),
        )
        .map_err(|error| error.to_string())
    }

    /// 定向视口投影：约束同 [`Module::scan`]。
    pub fn scan_directed(
        &self,
        positions: &[[i32; 3]],
        options: &crate::ViewportOptions,
    ) -> Result<crate::DirectedProjection, String> {
        let snapshot = self.latest();
        if !matches!(snapshot.phase, ConnectionPhase::Ready) {
            return Err("尚未连接到世界，无法观察".to_owned());
        }
        let world = self
            .inner
            .world_handle
            .lock()
            .clone()
            .ok_or_else(|| "世界模型尚未就绪".to_owned())?;
        let world = world.read();
        let bounds = crate::WorldHeightBounds::new(world.chunks.min_y(), world.chunks.height());
        let pose = crate::viewport::Pose {
            position: snapshot.self_state.position,
            yaw: snapshot.self_state.yaw,
            pitch: snapshot.self_state.pitch,
        };
        crate::viewport::project_directed_with_reader(
            &pose,
            positions,
            crate::viewport::WorldReader::new(
                |position| probe_block_from_world(&world, position),
                |position| read_block_from_world(&world, position),
            ),
            options,
            bounds,
            || Ok(()),
        )
        .map_err(|error| error.to_string())
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
            *inner.world_handle.lock() = Some(bot.world());
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
            ClientboundGamePacket::SetHealth(set_health) => {
                inner.track_health(f64::from(set_health.health));
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
            // 一次性跳跃：跳跃布尔保持了一整 tick，现在放开。
            if inner.jump_reset.swap(false, Ordering::AcqRel) {
                bot.set_jumping(false);
            }
            // 写口动词在 ECS 回调里执行：不跨线程触碰客户端。
            let pending: Vec<PendingCommand> = inner.pending.lock().drain(..).collect();
            for pending_command in pending {
                let outcome = run_command(inner, &bot, pending_command.command);
                let _ = pending_command.ack.send(outcome);
            }
            poll_movement_job(inner, &bot);
            if let Some(snapshot) = assemble_snapshot(inner, &bot) {
                inner.publish(snapshot);
            }
        }
        _ => {}
    }
}

/// 移动 job 每 tick 轮询：读寻路器三个状态位与自身方块位，
/// 交给纯判定表（`movement_poll_step`），只在这里落副作用。
fn poll_movement_job(inner: &Inner, bot: &Client) {
    let mut slot = inner.movement_job.lock();
    let Some(job) = slot.as_mut() else { return };

    let Ok((pathfinder, stall_ticks, block_pos)) = bot
        .try_query_self::<(Option<&Pathfinder>, Option<&ExecutingPath>, &Position), _>(
            |(pathfinder, executing, position)| {
                (
                    pathfinder.map(|p| (p.goal.is_some(), p.is_calculating)),
                    executing.map(|e| e.ticks_since_last_node_reached),
                    [
                        position.x.floor() as i32,
                        position.y.floor() as i32,
                        position.z.floor() as i32,
                    ],
                )
            },
        )
    else {
        return;
    };
    let (goal_some, calculating) = pathfinder.unwrap_or((false, false));
    let step = movement_poll_step(MovementPoll {
        armed: job.armed,
        goal_some,
        calculating,
        executing: stall_ticks.is_some(),
        at_destination: block_pos == job.destination,
        grace_exceeded: inner
            .tick
            .load(Ordering::Acquire)
            .saturating_sub(job.started_tick)
            > MOVEMENT_ARM_GRACE_TICKS,
        stalled_long: stall_ticks.is_some_and(|ticks| ticks > MOVEMENT_STALL_TICKS),
        stall_notified: job.stall_notified,
    });
    match step {
        MovementPollStep::Keep => {}
        MovementPollStep::Arm => job.armed = true,
        MovementPollStep::Stall => {
            job.stall_notified = true;
            let destination = job.destination;
            drop(slot);
            inner.push_job(destination, JobOutcome::Stalled);
        }
        MovementPollStep::End(outcome) => {
            let job = slot.take().expect("上面刚借到 Some");
            drop(slot);
            if outcome == JobOutcome::PathEnded {
                // goal 还挂在寻路器上（走完未达不清 goal）；清掉僵尸目标，
                // 避免下次判定被旧 goal 干扰。
                bot.stop_pathfinding();
            }
            inner.push_job(job.destination, outcome);
        }
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
        // 声音窗：生产者未落位，先空。
        sounds: Window::default(),
        damage: inner.damage_window_now(),
        jobs: inner.jobs_window_now(),
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

/// 在 tick 回调内执行一个写口动词。Err 是机器的如实拒绝，原文回到工具面。
fn run_command(inner: &Inner, bot: &Client, command: DoorCommand) -> Result<(), String> {
    match command {
        DoorCommand::Chat(line) => {
            bot.chat(&line);
            Ok(())
        }
        DoorCommand::GoTo([x, y, z]) => {
            let destination = [x.floor() as i32, y.floor() as i32, z.floor() as i32];
            inner.begin_movement_job(destination);
            bot.start_goto(BlockPosGoal(BlockPos::new(
                destination[0],
                destination[1],
                destination[2],
            )));
            Ok(())
        }
        DoorCommand::Forward(blocks) => {
            // 直走化归为寻路目标：终止条件（到达/受阻）交给寻路器。
            let (position, yaw) = bot
                .try_query_self::<(&Position, &LookDirection), _>(|(position, look)| {
                    ((position.x, position.y, position.z), f64::from(look.y_rot()))
                })
                .map_err(|_| "读不到自身位置".to_owned())?;
            let yaw = yaw.to_radians();
            let target = BlockPos::new(
                (position.0 + (-yaw.sin()) * blocks).floor() as i32,
                position.1.floor() as i32,
                (position.2 + (-yaw.cos()) * blocks).floor() as i32,
            );
            inner.begin_movement_job([target.x, target.y, target.z]);
            bot.start_goto(BlockPosGoal(target));
            Ok(())
        }
        DoorCommand::StopMoving => {
            inner.end_movement_job_stopped();
            bot.stop_pathfinding();
            bot.walk(WalkDirection::None);
            Ok(())
        }
        DoorCommand::Jump => {
            bot.set_jumping(true);
            inner.jump_reset.store(true, Ordering::Release);
            Ok(())
        }
        DoorCommand::Sneak(on) => {
            bot.set_crouching(on);
            Ok(())
        }
        DoorCommand::Sprint(on) => {
            if on {
                bot.sprint(SprintDirection::Forward);
            } else {
                // v1 简化：停疾跑=停下。原版疾跑是移动修饰符，细化随运动打磨。
                bot.walk(WalkDirection::None);
            }
            Ok(())
        }
        DoorCommand::LookAt([x, y, z]) => {
            bot.look_at(azalea::Vec3 { x, y, z });
            Ok(())
        }
        DoorCommand::Face { yaw, pitch } => {
            bot.set_direction(yaw as f32, pitch as f32);
            Ok(())
        }
        DoorCommand::Attack { entity_key } => {
            let entity = find_entity_by_key(bot, &entity_key)
                .ok_or_else(|| format!("附近没有 {entity_key} 这个实体"))?;
            bot.attack(entity);
            Ok(())
        }
        DoorCommand::Mine([x, y, z]) => {
            bot.start_mining(BlockPos::new(x, y, z));
            Ok(())
        }
        DoorCommand::UseOnBlock([x, y, z]) => {
            bot.block_interact(BlockPos::new(x, y, z));
            Ok(())
        }
        DoorCommand::UseOnEntity { entity_key } => {
            let entity = find_entity_by_key(bot, &entity_key)
                .ok_or_else(|| format!("附近没有 {entity_key} 这个实体"))?;
            bot.entity_interact(entity);
            Ok(())
        }
        DoorCommand::UseItem => {
            bot.start_use_item();
            Ok(())
        }
        DoorCommand::ReleaseHand => {
            bot.left_click_mine(false);
            bot.write_packet(s_player_action::ServerboundPlayerAction {
                action: s_player_action::Action::ReleaseUseItem,
                pos: BlockPos::new(0, 0, 0),
                direction: Default::default(),
                seq: 0,
            });
            Ok(())
        }
        DoorCommand::DropItem { whole_stack } => {
            bot.write_packet(s_player_action::ServerboundPlayerAction {
                action: if whole_stack {
                    s_player_action::Action::DropAllItems
                } else {
                    s_player_action::Action::DropItem
                },
                pos: BlockPos::new(0, 0, 0),
                direction: Default::default(),
                seq: 0,
            });
            Ok(())
        }
        DoorCommand::SwapOffhand => {
            bot.write_packet(s_player_action::ServerboundPlayerAction {
                action: s_player_action::Action::SwapItemWithOffhand,
                pos: BlockPos::new(0, 0, 0),
                direction: Default::default(),
                seq: 0,
            });
            Ok(())
        }
        DoorCommand::SelectSlot(slot) => {
            bot.set_selected_hotbar_slot(slot);
            Ok(())
        }
    }
}

/// 按快照里的实体键（`{epoch}:{协议id}`）找回 ECS 实体。
fn find_entity_by_key(bot: &Client, entity_key: &str) -> Option<azalea::ecs::entity::Entity> {
    let protocol_id: i32 = entity_key.strip_prefix("1:")?.parse().ok()?;
    let mut ecs = bot.ecs.write();
    let mut query = ecs.query::<(
        azalea::ecs::entity::Entity,
        &azalea::core::entity_id::MinecraftEntityId,
        &LoadedBy,
    )>();
    query
        .iter(&ecs)
        .find(|(_, id, loaded_by)| ***id == protocol_id && loaded_by.contains(&bot.entity))
        .map(|(entity, _, _)| entity)
}

// ---- 方块读取面（迁自旧 backend）：视口双通道读取器的世界侧实现 ----

/// `BlockProbe::Loaded` 的两位，按 `state_id` 预先算好。整张表首次使用时
/// 算一遍（`BlockStateIntegerRepr` 是 u16，表最大 64 KiB），之后每次探测
/// 是一次数组下标，零分配。
fn probe_table() -> &'static [(bool, bool)] {
    static TABLE: std::sync::OnceLock<Box<[(bool, bool)]>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        (0..=azalea::block::BlockState::MAX_STATE)
            .map(|state_id| {
                let Ok(state) = azalea::block::BlockState::try_from(state_id) else {
                    // 不该发生：迭代范围就是合法区间。按最保守的"有东西且
                    // 不透光"处理，宁可少报可见块也不误报。
                    return (true, false);
                };
                let block: Box<dyn azalea::block::BlockTrait> = Box::from(state);
                let name = block.id();
                (
                    !crate::is_air_name(name),
                    transparent_hint(name, state.outline_shape()),
                )
            })
            .collect()
    })
}

fn transparent_hint(name: &str, outline_shape: &azalea::physics::collision::VoxelShape) -> bool {
    // 26.1 的方块注册表没有暴露 transparent 布尔；对常见全体积透明块按名
    // 提示，其余非完整轮廓按保守的"可能透光"处理。
    let named_transparent = crate::is_air_name(name)
        || name.contains("glass")
        || name.ends_with("leaves")
        || name == "water"
        || name == "lava"
        || name == "powder_snow";
    named_transparent || !is_full_cube(outline_shape)
}

fn is_full_cube(shape: &azalea::physics::collision::VoxelShape) -> bool {
    let boxes = shape.to_aabbs();
    boxes.len() == 1
        && boxes[0].min.x == 0.0
        && boxes[0].min.y == 0.0
        && boxes[0].min.z == 0.0
        && boxes[0].max.x == 1.0
        && boxes[0].max.y == 1.0
        && boxes[0].max.z == 1.0
}

/// 热路径探针：不建 DTO，一次表下标。
fn probe_block_from_world(
    world: &azalea::world::World,
    position: crate::BlockPosition,
) -> crate::BlockProbe {
    let block_position = BlockPos::new(position.x, position.y, position.z);
    let y = i64::from(position.y);
    let min_y = i64::from(world.chunks.min_y());
    let max_y_exclusive = min_y + i64::from(world.chunks.height());
    if y < min_y || y >= max_y_exclusive {
        return crate::BlockProbe::OutOfWorld;
    }
    let Some(state) = world.get_block_state(block_position) else {
        return crate::BlockProbe::Unloaded;
    };
    let (visible, transparent_hint) = probe_table()[usize::from(state.id())];
    crate::BlockProbe::Loaded {
        visible,
        transparent_hint,
    }
}

/// 完整 DTO 读取：只在要把方块交给读方时调用。
fn read_block_from_world(
    world: &azalea::world::World,
    position: crate::BlockPosition,
) -> crate::BlockReadResult {
    let block_position = BlockPos::new(position.x, position.y, position.z);
    let y = i64::from(position.y);
    let min_y = i64::from(world.chunks.min_y());
    let max_y_exclusive = min_y + i64::from(world.chunks.height());
    if y < min_y || y >= max_y_exclusive {
        return crate::BlockReadResult::OutOfWorld;
    }
    let Some(state) = world.get_block_state(block_position) else {
        return crate::BlockReadResult::Unloaded;
    };
    let block: Box<dyn azalea::block::BlockTrait> = Box::from(state);
    let collision_shape = state.collision_shape();
    let collision_shapes: Vec<[f64; 6]> = collision_shape
        .to_aabbs()
        .into_iter()
        .map(|aabb| {
            [
                aabb.min.x, aabb.min.y, aabb.min.z, aabb.max.x, aabb.max.y, aabb.max.z,
            ]
        })
        .collect();
    let properties = block
        .property_map()
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
    let bounding_box = if collision_shapes.is_empty() {
        crate::BlockBoundingBox::Empty
    } else {
        crate::BlockBoundingBox::Block
    };
    crate::BlockReadResult::Loaded {
        block: crate::BlockSnapshot {
            position,
            name: block.id().to_owned(),
            state_id: u32::from(state.id()),
            properties,
            collision_shapes,
            transparent_hint: transparent_hint(block.id(), state.outline_shape()),
            bounding_box,
        },
    }
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
    fn health_drop_produces_damage_entry_and_recovery_does_not() {
        let inner = Inner::new();
        inner.track_health(20.0); // 基线，不产条目
        inner.track_health(20.0); // 不变
        inner.track_health(13.5); // 下降
        inner.track_health(17.0); // 治疗回升
        inner.track_health(17.0 - 0.001); // 浮点噪声，不产
        inner.track_health(0.0); // 致死一击

        let window = inner.damage_window_now();
        assert_eq!(window.entries.len(), 2);
        assert_eq!(window.entries[0].health_before, 20.0);
        assert_eq!(window.entries[0].health_after, 13.5);
        assert_eq!(window.entries[1].health_after, 0.0);
        // seq 与聊天同源单调。
        assert!(window.entries[0].seq < window.entries[1].seq);
    }

    #[test]
    fn movement_job_replacement_and_stop_are_recorded() {
        let inner = Inner::new();
        inner.begin_movement_job([10, 64, -3]);
        inner.begin_movement_job([20, 64, 5]); // 顶替
        inner.end_movement_job_stopped(); // 停止
        inner.end_movement_job_stopped(); // 没任务时不是事件

        let window = inner.jobs_window_now();
        let outcomes: Vec<JobOutcome> =
            window.entries.iter().map(|entry| entry.outcome).collect();
        assert_eq!(outcomes, vec![JobOutcome::Replaced, JobOutcome::Stopped]);
        assert_eq!(
            window.entries[0].job,
            JobKind::MoveTo {
                destination: [10, 64, -3]
            }
        );
    }

    /// 判定表全景：armed 前后各态的行动结论。
    #[test]
    fn movement_poll_step_covers_the_observable_states() {
        use MovementPollStep as Step;
        let poll = MovementPoll::default;
        // 起步宽限内：寻路器不可见→等待；可见→武装。
        assert_eq!(movement_poll_step(poll()), Step::Keep);
        assert_eq!(
            movement_poll_step(MovementPoll {
                calculating: true,
                ..poll()
            }),
            Step::Arm
        );
        assert_eq!(
            movement_poll_step(MovementPoll {
                goal_some: true,
                ..poll()
            }),
            Step::Arm
        );
        // 宽限耗尽还没起步：按走完未达收束。
        assert_eq!(
            movement_poll_step(MovementPoll {
                grace_exceeded: true,
                ..poll()
            }),
            Step::End(JobOutcome::PathEnded)
        );
        // 武装后：goal 清空且不在算不在走 = 寻路器宣告到达。
        assert_eq!(
            movement_poll_step(MovementPoll {
                armed: true,
                ..poll()
            }),
            Step::End(JobOutcome::Arrived)
        );
        // goal 还挂着但停了：不在目的地=走完未达；在目的地=空路径到达。
        assert_eq!(
            movement_poll_step(MovementPoll {
                armed: true,
                goal_some: true,
                ..poll()
            }),
            Step::End(JobOutcome::PathEnded)
        );
        assert_eq!(
            movement_poll_step(MovementPoll {
                armed: true,
                goal_some: true,
                at_destination: true,
                ..poll()
            }),
            Step::End(JobOutcome::Arrived)
        );
        // 执行中：正常走→保持；久无推进→通知一次，此后沉默。
        let walking = MovementPoll {
            armed: true,
            goal_some: true,
            executing: true,
            ..poll()
        };
        assert_eq!(movement_poll_step(walking), Step::Keep);
        assert_eq!(
            movement_poll_step(MovementPoll {
                stalled_long: true,
                ..walking
            }),
            Step::Stall
        );
        assert_eq!(
            movement_poll_step(MovementPoll {
                stalled_long: true,
                stall_notified: true,
                ..walking
            }),
            Step::Keep
        );
        // 重算中（部分路径续算）不是终局。
        assert_eq!(
            movement_poll_step(MovementPoll {
                armed: true,
                goal_some: true,
                calculating: true,
                ..poll()
            }),
            Step::Keep
        );
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
        let receiver = inner.enqueue_command(DoorCommand::Chat("你好".to_owned()));
        let outcome = receiver.await.expect("ack 应送达");
        assert!(outcome.is_err());
        assert!(inner.pending.lock().is_empty());
    }
}
