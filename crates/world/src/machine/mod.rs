//! 接入模块的连接机器：azalea 客户端的生命周期、tick 快照循环与聊天面。
//!
//! v1 范围：重连政策固定 `Never`（断线=Disconnected 相，不换纪元）；
//! 声音窗暂空（生产者随后落位）。伤害窗每 tick 由生命对比生产；
//! 移动 job 追踪产出 jobs 窗（到达/顶替/停止/走完未达/卡住）。
//!
//! 线程模型：机器独占一个线程（tokio current_thread + LocalSet，azalea 需要），
//! 对外全部经共享状态交流——快照 latest-wins、聊天出站走队列在 tick 内执行
//! （ECS 只在客户端事件回调里触碰，不跨线程直写）。

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use std::sync::atomic::Ordering;
use tokio::sync::oneshot;

use crate::{ConnectionPhase, SnapshotSource, TickSnapshot};

mod blocks;
mod capture;
mod connect;
mod door;
mod job;
mod mining;
mod movement;
pub mod observed;
mod state;

pub use door::DoorCommand;

use self::blocks::{probe_block_from_world, read_block_from_world};
use self::connect::run_swarm;
use self::state::Inner;

/// 伤害窗条目上限。关注类窗口 ≥ 最长一轮时长；伤害事件稀疏，按条数封顶即可。
const DAMAGE_WINDOW_ENTRIES: usize = 100;
/// 任务窗条目上限。
const JOBS_WINDOW_ENTRIES: usize = 32;
/// 物品栏变化窗条目上限。
const INVENTORY_WINDOW_ENTRIES: usize = 64;
/// 拾取窗条目上限。挖一片树、砸一堆矿会连着来，比格位变化更密，取同一量级。
const PICKUP_WINDOW_ENTRIES: usize = 64;
/// 声音窗条目上限。声音是所有窗里最密的（脚步、方块、环境音一刻不停），
/// 取得比别的窗大，靠游标每帧排空，不靠窗本身留存。
const SOUND_WINDOW_ENTRIES: usize = 256;
/// 屏开/关事实窗条目上限。开关稀疏，小窗足矣。
const SCREEN_WINDOW_ENTRIES: usize = 16;
/// 移动 job 起步宽限：下令后寻路器要过几个调度周期才可见（GotoEvent 是
/// Bevy 消息，跨 schedule 投递）；宽限内不判终局。
const MOVEMENT_ARM_GRACE_TICKS: u64 = 100;
/// 卡住通知阈值：与 azalea 自己的补路超时同量级（它 3–7 秒就会自救，
/// 超过 10 秒还没推进说明自救也没起色，值得让模型知道）。
const MOVEMENT_STALL_TICKS: usize = 200;

/// 一块挖不碎的时限。徒手挖石头约 15 秒（300 tick）是原版量级；取 400 tick
/// 留足余量——超过它仍不碎，多半是够不着、被挡或工具不对，不是慢。
const MINING_STALL_TICKS: u64 = 400;

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
                ConnectionPhase::Stopped { reason } => return Err(format!("连接已停止：{reason}")),
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
        self.inner.publish_phase(ConnectionPhase::Stopped {
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
    pub fn scan(
        &self,
        options: &crate::ViewportOptions,
    ) -> Result<crate::ViewportProjection, String> {
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

    /// 同 [`Module::scan`]，另把射线走过的空格记进 [`crate::ObservedSpace`]。
    ///
    /// 单独一个入口而不是给 `scan` 加参数：`scan` 服务的是**工具回执**（模型问
    /// 「我看见什么」），一次性；本入口服务的是**眼睛**，每帧都跑，是三态记忆
    /// 里「确认为空」那一位的唯一产生方。
    fn scan_observing(
        &self,
        options: &crate::ViewportOptions,
        space: &mut crate::ObservedSpace,
    ) -> Result<crate::ViewportProjection, String> {
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
        crate::viewport::project_observing(
            &pose,
            &snapshot.entities,
            crate::viewport::WorldReader::new(
                |position| probe_block_from_world(&world, position),
                |position| read_block_from_world(&world, position),
            ),
            options,
            || Ok(()),
            space,
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

    /// 增量视口投影：对比方块记忆只报变化，并当场推进记忆（回执走内核
    /// settled 通道必达模型，产出即送达）。约束同 [`Module::scan`]。
    /// 开启**合法寻路**：寻路只按 `memory` 里观察过的方块规划路线
    /// （不调用就是原样——azalea 读服务端推来的全部已加载区块，包括同伴从没看过的
    /// 地方，那会泄露未见地形；理由详见 `machine::observed` 模块文档）。
    ///
    /// 传进来的必须是**组合根那一份**记忆：轮末帧每 250ms 往里推进增量，寻路要
    /// 看到的正是同一份，两份会各说各话。
    pub fn use_observed_pathfinding(
        &self,
        memory: std::sync::Arc<std::sync::Mutex<crate::BlockMemory>>,
    ) {
        let world = self.inner.world_handle.lock().clone();
        let Some(world) = world else {
            // 世界还没就绪：装不上就如实什么都不做，调用方在 wait_ready 之后再叫一次。
            return;
        };
        *self.inner.observed.lock() = Some(std::sync::Arc::new(
            crate::machine::observed::ObservedBlocks::new(memory, world),
        ));
    }

    /// 诊断：同一目标，全量世界 vs 只按观察过的地图，各算一次路。
    ///
    /// 不产生移动，也不装组件——纯粹为了回答「合法之后还找不找得到路、路长多少、
    /// 算多久」。返回 `(全量, 合法)`。
    pub fn compare_paths(
        &self,
        memory: std::sync::Arc<std::sync::Mutex<crate::BlockMemory>>,
        goal: [i32; 3],
    ) -> Result<
        (
            crate::machine::observed::PathAttempt,
            crate::machine::observed::PathAttempt,
        ),
        String,
    > {
        let snapshot = self.latest();
        if !matches!(snapshot.phase, ConnectionPhase::Ready) {
            return Err("尚未连接到世界".to_owned());
        }
        let position = &snapshot.self_state.position;
        let start = azalea::BlockPos::new(
            position.x.floor() as i32,
            position.y.floor() as i32,
            position.z.floor() as i32,
        );
        crate::machine::observed::compare(
            &self.inner,
            memory,
            start,
            azalea::BlockPos::new(goal[0], goal[1], goal[2]),
        )
    }

    /// 睁眼一次：把合法可见的方块整份写进记忆，返回吸收了多少格。
    ///
    /// 眼睛走这条路而不是 [`Self::scan_changes`]：**差异没有消费者了**。方块不进
    /// 会话区之后，记忆只需要「把看见的收进来」，不需要知道哪些是新的。
    /// 实测差异那一层是 +3.7ms / 35%（8.8ms → 13.3ms），省下来是白赚的。
    ///
    /// 差异那套代码**保留不动**：将来做订阅（「盯着这个熔炉」）时，它就是原料；
    /// 而且 `scan` 工具的 `changes` 模式现在仍然在用它。
    pub fn absorb(
        &self,
        memory: &std::sync::Mutex<crate::BlockMemory>,
        options: &crate::ViewportOptions,
    ) -> Result<usize, String> {
        // 自由空间先落在一份本次投影专用的暂存里，**投影全程不持记忆的锁**：
        // 投影约十毫秒，而寻路器每 tick 要读这本记忆上千次。
        // 观察发生的刻取自同一份快照：投影读的就是它的姿态与实体。
        let at_tick = self.latest().tick;
        let mut free_space = crate::ObservedSpace::new();
        let projection = self.scan_observing(options, &mut free_space)?;
        let mut memory = memory.lock().map_err(|_| "方块记忆锁中毒".to_owned())?;
        // 空的先上账、方块后上账：两者由构造保证不相交（射线撞到第一个非空气
        // 就停），万一相交也让「有东西」赢，与三态的优先级一致。
        memory.absorb_empty(&free_space, at_tick);
        memory.absorb_visible(&projection.visible_blocks.blocks, at_tick);
        for block in [&projection.standing_on_block, &projection.looked_at_block]
            .into_iter()
            .flatten()
        {
            memory.absorb_visible(std::slice::from_ref(block), at_tick);
        }
        Ok(projection.visible_blocks.blocks.len())
    }

    pub fn scan_changes(
        &self,
        memory: &std::sync::Mutex<crate::BlockMemory>,
        options: &crate::ViewportOptions,
    ) -> Result<Vec<crate::BlockChange>, String> {
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
        // 对比与推进在同一次持锁内完成：与并行吸收（其他 scan 回执）互斥，
        // 不会对着推进到一半的记忆做 diff。
        let mut memory = memory.lock().expect("方块记忆锁不应中毒");
        let changes = crate::viewport::project_changes(
            &pose,
            &memory,
            |position| read_block_from_world(&world, position),
            options,
            bounds,
        )?;
        memory.apply(&changes, snapshot.tick);
        Ok(changes)
    }

    /// 全部在途任务。空 = 什么都没在跑。
    ///
    /// 槽位是唯一真相源，不另建镜像表——两份真相迟早会不一致。
    pub fn jobs_in_flight(&self) -> Vec<crate::JobStatus> {
        self.inner.jobs_in_flight()
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
}
