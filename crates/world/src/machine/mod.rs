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

use tokio::sync::watch;

use crate::{ConnectionPhase, SnapshotSource, TickSnapshot};

mod blocks;
mod capture;
mod connect;
mod door;
mod input;
mod job;
mod light;
mod mining;
mod state;

pub use door::DoorCommand;
pub use input::InputCompletion;

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
/// 同步写入的 `MiningQueued` 等待 Azalea GameTick 消费的最大 tick 数。命中说明
/// 调度链没有接单，不是方块挖不动。
const MINING_DISPATCH_TIMEOUT_TICKS: u64 = 100;
/// 本地方块预测等待服务端确认的最大 tick 数。它约束的是协议确认悬挂，不能与
/// `MineProgress` 停滞混为一谈。
const MINING_PREDICTION_SETTLE_TIMEOUT_TICKS: u64 = 400;
/// 同一目标的 `MineProgress` 连续多久没有严格增长，才判挖掘没有产生进展。
/// 这是无进展窗口，不是总工期：再慢的方块只要进度仍在增长，就不会被时间误杀。
const MINING_NO_PROGRESS_TICKS: u64 = 400;

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
    /// 可克隆的完成事实：重复或并发 stop 都必须等同一个 machine-thread 终局。
    done: watch::Receiver<bool>,
}

impl Module {
    /// 启动连接。立即返回；就绪与否观察快照相（或 [`Module::wait_ready`]）。
    pub fn start(config: ConnectionConfig) -> Result<Self, String> {
        config.validate()?;
        let inner = Arc::new(Inner::new());
        let thread_inner = inner.clone();
        let (done_tx, done_rx) = watch::channel(false);
        std::thread::Builder::new()
            .name("world-machine".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("接入机器线程的 tokio runtime 构建失败");
                let local = tokio::task::LocalSet::new();
                local.block_on(&runtime, run_swarm(thread_inner.clone(), config));
                // 完成事实必须晚于 LocalSet/runtime 析构；否则 stop 可能已经返回 Ok，
                // Azalea 的残留任务却仍在 teardown（甚至卡在已知的上游死锁）。
                drop(local);
                drop(runtime);
                thread_inner.end_running_jobs();
                if !thread_inner.is_stopping()
                    && !matches!(
                        &thread_inner.latest.read().phase,
                        ConnectionPhase::Disconnected { .. }
                    )
                {
                    thread_inner.publish_phase(ConnectionPhase::Disconnected {
                        reason: "连接执行流已经结束".to_owned(),
                    });
                }
                thread_inner.fail_all_pending_commands("连接已结束");
                done_tx.send_replace(true);
            })
            .map_err(|error| format!("接入机器线程启动失败：{error}"))?;
        Ok(Self {
            inner,
            done: done_rx,
        })
    }

    /// 等到快照相变为 Ready；超时返回 Err。
    pub async fn wait_ready(&self, timeout: Duration) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut ticked = self.inner.ticked_tx.subscribe();
        loop {
            if self.inner.is_stopping() {
                return Err("连接正在停止".to_owned());
            }
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
        // 外线程只请求，不清 JobSlot、不触 ECS。已经被 owner thread 领取的命令/tick
        // 先完成；thread teardown 才是 stop 的线性化点。
        self.inner.request_stop(reason);
        // notify_one 会保存 permit，覆盖 stop 早于 run_swarm 建立 waiter 的启动窗口。
        self.inner.shutdown.notify_one();
        self.inner.fail_all_pending_commands("正在停机");

        let mut done = self.done.clone();
        if !*done.borrow() {
            match tokio::time::timeout(Duration::from_secs(10), done.changed()).await {
                Ok(Ok(())) if *done.borrow() => {}
                Ok(Ok(())) => return Err("接入机器线程结束信号无效".to_owned()),
                Ok(Err(_)) => return Err("接入机器线程异常退出，未报告完成".to_owned()),
                Err(_) => return Err("接入机器线程 10 秒内未合流".to_owned()),
            }
        }

        // 此刻 owner thread 与 runtime 均已销毁，才可对外宣告 Stopped。
        self.inner.publish_phase(ConnectionPhase::Stopped {
            reason: self.inner.stop_reason().unwrap_or(reason).to_owned(),
        });
        Ok(())
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

    /// 按一次键鼠：入队，下一 tick 按下，按满时长（或提前结束）后松开，回执松开时的结果。
    ///
    /// 两段等待：先等机器接受（未连接、死亡等如实拒绝），再等松开。
    pub async fn input(&self, spec: crate::InputSpec) -> Result<crate::InputOutcome, String> {
        if !(1..=crate::MAX_INPUT_TICKS).contains(&spec.ticks) {
            return Err(format!(
                "按住时长要在 1..={} tick 之间",
                crate::MAX_INPUT_TICKS
            ));
        }
        if spec
            .turn
            .is_some_and(|turn| !turn.yaw.is_finite() || !turn.pitch.is_finite())
        {
            return Err("转动角度要是有限数".to_owned());
        }
        let (completion, released) = InputCompletion::new();
        self.execute(DoorCommand::Input(spec, completion)).await?;
        released.await.map_err(|_| "按键期间连接结束了".to_owned())
    }

    /// 聊天出站：一行 = 一次原版输入循环，`/` 开头由 azalea 按原版语义路由为命令。
    pub async fn send_chat_line(&self, line: &str) -> Result<(), String> {
        self.execute(DoorCommand::Chat(line.to_owned())).await
    }

    /// 拷贝视野里的表面方块供按需成像，随即放开世界读锁；解析资源与出像素都在锁外。
    /// 在阻塞线程上调用。
    ///
    /// 范围与原版一致：以所在区块为中心、视距内的区块，全世界高度。视距取客户端
    /// 默认视距与服务端视距的较小者。
    ///
    /// 和原版只建视锥里的区块段一样，只交 `visible` 认可的区块段里的表面方块：调用方
    /// 拿同一份位姿快照与视距造出区块段判据（区块段坐标 = 方块坐标 >> 4），世界层不知道
    /// 相机与视锥。判据外只再解码贴着它的一圈区块段（表面判定与平滑光照要查邻格）和
    /// 脚边一圈（未加载计数）。恒真判据即整个视距。
    pub fn capture_view<F, V>(&self, visible: F) -> Result<crate::BlockRegion, String>
    where
        F: FnOnce(&TickSnapshot, u32) -> V,
        V: Fn([i32; 3]) -> bool + Sync,
    {
        use self::blocks::{render_class, RenderClass};

        let snapshot = self.latest();
        if !matches!(snapshot.phase, ConnectionPhase::Ready) {
            return Err("尚未连接到世界，无法采集图片".to_owned());
        }
        let handle = self
            .inner
            .world_handle
            .lock()
            .clone()
            .ok_or_else(|| "世界模型尚未就绪".to_owned())?;
        let server = self
            .inner
            .server_view_distance
            .load(std::sync::atomic::Ordering::Relaxed);
        let client = u32::from(crate::CLIENT_VIEW_DISTANCE);
        let view_distance = if server == 0 {
            client
        } else {
            client.min(server)
        };

        let position = &snapshot.self_state.position;
        let feet = [position.x, position.y, position.z].map(|v| v.floor() as i32);
        let reach = (view_distance as i32 + 1) * 16;
        if feet
            .iter()
            .any(|v| v.checked_add(reach).is_none() || v.checked_sub(reach).is_none())
        {
            return Err("图片采集坐标越界".to_owned());
        }
        let visible = visible(&snapshot, view_distance);
        let origin_x = (feet[0].div_euclid(16) - view_distance as i32) * 16;
        let origin_z = (feet[2].div_euclid(16) - view_distance as i32) * 16;

        // 第一遍：冻结。世界读锁里把视距内各区块的分段原样克隆出来（只复制调色板数据），
        // 随即放锁；逐格解码、分类与成像都在锁外。方块因此是同一时刻的副本，与位姿快照
        // 相差不超过一个 tick；逐格解码若在锁内做，长时间持锁会饿住客户端循环、心跳超时被踢。
        let loaded = handle.read();
        let min_y = loaded.chunks.min_y();
        let height = loaded.chunks.height() as i32;
        let chunks_per_side = view_distance as i32 * 2 + 1;
        let columns: Vec<Option<Vec<azalea::world::Section>>> = (0..chunks_per_side
            * chunks_per_side)
            .map(|i| {
                let pos = azalea::core::position::ChunkPos::new(
                    origin_x.div_euclid(16) + i % chunks_per_side,
                    origin_z.div_euclid(16) + i / chunks_per_side,
                );
                loaded
                    .chunks
                    .0
                    .get(&pos)
                    .map(|chunk| chunk.read().sections.to_vec())
            })
            .collect();
        // 光照与方块同一时刻冻结：光照段是 Arc，克隆只加引用计数。
        let lights: Vec<Option<light::ColumnLight>> = {
            let store = self.inner.light.lock();
            (0..chunks_per_side * chunks_per_side)
                .map(|i| {
                    store.column(
                        origin_x.div_euclid(16) + i % chunks_per_side,
                        origin_z.div_euclid(16) + i / chunks_per_side,
                    )
                })
                .collect()
        };
        // 生物群系协议号 → 注册表名（服务端在配置阶段同步的顺序）。
        let biome_names: Vec<String> = {
            use azalea::registry::DataRegistry;
            let registry = azalea::Identifier::from(azalea::registry::data::Biome::NAME);
            (0u32..)
                .map_while(|id| {
                    loaded
                        .registries
                        .protocol_id_to_identifier(registry.clone(), id)
                        .map(|name| name.to_string())
                })
                .collect()
        };
        drop(loaded);
        let biome_zoom_seed = self
            .inner
            .biome_zoom_seed
            .load(std::sync::atomic::Ordering::Relaxed);
        let clock_ticks = self
            .inner
            .day_time
            .load(std::sync::atomic::Ordering::Acquire);

        // 第二遍：挑区块段。区块段网格按 (y, z, x) 编号，坐标相对视距窗口的西北角与世界底。
        let (side, layers) = (chunks_per_side, height / 16);
        let grid = |[sx, sy, sz]: [i32; 3]| ((sy * side + sz) * side + sx) as usize;
        let in_grid = |[sx, sy, sz]: [i32; 3]| {
            (0..side).contains(&sx) && (0..layers).contains(&sy) && (0..side).contains(&sz)
        };
        let present = |[sx, sy, sz]: [i32; 3]| {
            columns[(sz * side + sx) as usize]
                .as_ref()
                .is_some_and(|sections| (sy as usize) < sections.len())
        };
        let first = [origin_x >> 4, min_y >> 4, origin_z >> 4];
        let mut shown = vec![false; (side * side * layers) as usize];
        let mut wanted_sections = vec![false; shown.len()];
        for sy in 0..layers {
            for sz in 0..side {
                for sx in 0..side {
                    let local = [sx, sy, sz];
                    if !present(local) || !visible([first[0] + sx, first[1] + sy, first[2] + sz]) {
                        continue;
                    }
                    shown[grid(local)] = true;
                    for dy in -1..=1 {
                        for dz in -1..=1 {
                            for dx in -1..=1 {
                                let near = [sx + dx, sy + dy, sz + dz];
                                if in_grid(near) {
                                    wanted_sections[grid(near)] = true;
                                }
                            }
                        }
                    }
                }
            }
        }
        // 未加载计数查的是脚边 NEAR_LOADED_RADIUS 的立方体。
        let near = |axis: usize, origin: i32| {
            (feet[axis] - crate::NEAR_LOADED_RADIUS - origin) >> 4
                ..=(feet[axis] + crate::NEAR_LOADED_RADIUS - origin) >> 4
        };
        for sy in near(1, min_y) {
            for sz in near(2, origin_z) {
                for sx in near(0, origin_x) {
                    if in_grid([sx, sy, sz]) {
                        wanted_sections[grid([sx, sy, sz])] = true;
                    }
                }
            }
        }
        // 要解码的区块段按网格序排进槽位，每槽 16³ 个状态、4³ 个生物群系格，(y, z, x) 序。
        const UNDECODED: u32 = u32::MAX;
        let mut slot = vec![UNDECODED; shown.len()];
        let mut decoded: Vec<[i32; 3]> = Vec::new();
        for (g, wanted) in wanted_sections.iter().enumerate() {
            let g = g as i32;
            let local = [g % side, g / (side * side), g / side % side];
            if *wanted && present(local) {
                slot[g as usize] = decoded.len() as u32;
                decoded.push(local);
            }
        }
        drop(wanted_sections);

        // 第三遍：多线程解码选中的区块段。
        let mut states = vec![0u16; decoded.len() * 4096];
        let mut quarts = vec![0u32; decoded.len() * 64];
        let per = decoded.len().div_ceil(workers()).max(1);
        std::thread::scope(|scope| {
            let columns = &columns;
            for ((sections, states), quarts) in decoded
                .chunks(per)
                .zip(states.chunks_mut(per * 4096))
                .zip(quarts.chunks_mut(per * 64))
            {
                scope.spawn(move || {
                    use azalea::core::position::{ChunkSectionBiomePos, ChunkSectionBlockPos};
                    use azalea::registry::DataRegistry;
                    for (k, &[sx, sy, sz]) in sections.iter().enumerate() {
                        let section = &columns[(sz * side + sx) as usize]
                            .as_ref()
                            .expect("只解码已加载的区块柱")[sy as usize];
                        for (i, state) in states[k * 4096..(k + 1) * 4096].iter_mut().enumerate() {
                            let [x, y, z] = [i & 15, i >> 8, (i >> 4) & 15].map(|v| v as u8);
                            *state = section
                                .get_block_state(ChunkSectionBlockPos::new(x, y, z))
                                .id();
                        }
                        for (i, quart) in quarts[k * 64..(k + 1) * 64].iter_mut().enumerate() {
                            let [x, y, z] = [i & 3, i >> 4, (i >> 2) & 3].map(|v| v as u8);
                            *quart = section
                                .get_biome(ChunkSectionBiomePos { x, y, z })
                                .protocol_id();
                        }
                    }
                });
            }
        });

        // 方块坐标 → 状态数组下标；视距窗口外、没加载或没解码的格子为 `None`。
        let index = |x: i32, y: i32, z: i32| -> Option<usize> {
            let local = [x - origin_x, y - min_y, z - origin_z];
            if !in_grid(local.map(|v| v >> 4)) {
                return None;
            }
            match slot[grid(local.map(|v| v >> 4))] {
                UNDECODED => None,
                s => {
                    let [lx, ly, lz] = local.map(|v| (v & 15) as usize);
                    Some(s as usize * 4096 + (ly * 16 + lz) * 16 + lx)
                }
            }
        };
        let class_at = |x: i32, y: i32, z: i32| -> Option<RenderClass> {
            if y >= min_y + height {
                return Some(RenderClass::Air);
            }
            index(x, y, z).map(|i| render_class(states[i]))
        };

        let mut unloaded = 0;
        for y in feet[1] - crate::NEAR_LOADED_RADIUS..=feet[1] + crate::NEAR_LOADED_RADIUS {
            if !(min_y..min_y + height).contains(&y) {
                continue;
            }
            for z in feet[2] - crate::NEAR_LOADED_RADIUS..=feet[2] + crate::NEAR_LOADED_RADIUS {
                for x in feet[0] - crate::NEAR_LOADED_RADIUS..=feet[0] + crate::NEAR_LOADED_RADIUS {
                    if class_at(x, y, z).is_none() {
                        unloaded += 1;
                    }
                }
            }
        }

        // 第四遍：视野里的区块段只留至少一面没被挡住的方块。没加载的邻格按挡住算，否则
        // 视距边缘和未加载区块的边上会立起一整面墙。分段并行，再按 (y, z, x) 排回整体扫描序。
        const DIRECTIONS: [[i32; 3]; 6] = [
            [0, -1, 0],
            [0, 1, 0],
            [0, 0, -1],
            [0, 0, 1],
            [-1, 0, 0],
            [1, 0, 0],
        ];
        let shown_slots: Vec<usize> = (0..decoded.len())
            .filter(|&s| shown[grid(decoded[s])])
            .collect();
        let mut surface = parallel(&shown_slots, |slots, _, surface| {
            for &s in slots {
                let [sx, sy, sz] = decoded[s];
                let base = [origin_x + sx * 16, min_y + sy * 16, origin_z + sz * 16];
                for (i, &id) in states[s * 4096..(s + 1) * 4096].iter().enumerate() {
                    let class = render_class(id);
                    if class == RenderClass::Air {
                        continue;
                    }
                    let [x, y, z] = [
                        base[0] + (i & 15) as i32,
                        base[1] + (i >> 8) as i32,
                        base[2] + ((i >> 4) & 15) as i32,
                    ];
                    let mut covered = 0u8;
                    for (bit, [dx, dy, dz]) in DIRECTIONS.into_iter().enumerate() {
                        let hidden = match class_at(x + dx, y + dy, z + dz) {
                            None | Some(RenderClass::Opaque) => true,
                            Some(neighbour) => {
                                matches!(class, RenderClass::Water | RenderClass::Lava)
                                    && neighbour == class
                            }
                        };
                        if hidden {
                            covered |= 1 << bit;
                        }
                    }
                    if covered != 0b11_1111 {
                        surface.push((x, y, z, id, covered));
                    }
                }
            }
        });
        surface.sort_unstable_by_key(|&(x, y, z, _, _)| (y, z, x));

        // 第五遍：原版平滑光照要查的格子。每个表面方块周围一圈 27 格；露出的面另加
        // 面前第二层的四个侧格（`BlockModelLighter` 判断角格是否透光查的是那一层）。
        // 实体按眼睛所在格取光（原版 `getLightProbePosition`）。
        let mut needed = vec![false; states.len()];
        let mut mark = |x: i32, y: i32, z: i32| {
            if let Some(i) = index(x, y, z) {
                needed[i] = true;
            }
        };
        for &(x, y, z, _, covered) in &surface {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    for dx in -1..=1 {
                        mark(x + dx, y + dy, z + dz);
                    }
                }
            }
            for (bit, direction) in DIRECTIONS.into_iter().enumerate() {
                if covered & (1 << bit) != 0 {
                    continue;
                }
                for side in DIRECTIONS {
                    if side.iter().zip(direction).any(|(a, b)| *a != 0 && b != 0) {
                        continue;
                    }
                    mark(
                        x + 2 * direction[0] + side[0],
                        y + 2 * direction[1] + side[1],
                        z + 2 * direction[2] + side[2],
                    );
                }
            }
        }
        for entity in snapshot.entities.iter().filter(|entity| entity.valid) {
            mark(
                entity.position.x.floor() as i32,
                (entity.position.y + entity.height * 0.85).floor() as i32,
                entity.position.z.floor() as i32,
            );
        }
        let cells = parallel(&decoded, |sections, first_slot, cells| {
            for (k, &[sx, sy, sz]) in sections.iter().enumerate() {
                let s = first_slot + k;
                let light = lights[(sz * side + sx) as usize].as_ref();
                for i in (0..4096).filter(|i| needed[s * 4096 + i]) {
                    let [lx, ly, lz] = [i & 15, i >> 8, (i >> 4) & 15];
                    let y = min_y + sy * 16 + ly as i32;
                    let (sky_light, block_light) =
                        light.map_or((0, 0), |light| light.get([lx, lz], y, min_y));
                    let props = blocks::render_props(states[s * 4096 + i]);
                    cells.push(crate::LightCell {
                        position: [
                            origin_x + sx * 16 + lx as i32,
                            y,
                            origin_z + sz * 16 + lz as i32,
                        ],
                        sky_light,
                        block_light,
                        emission: props.emission,
                        dampening: props.dampening,
                        view_blocking: props.view_blocking,
                        solid_render: props.solid_render,
                        emissive: props.emissive,
                        full_collision: props.full_collision,
                    });
                }
            }
        });
        drop(needed);
        drop(states);

        // 第六遍：成像要查的生物群系格。原版染色在同一 y 上取 5×5 格的生物群系
        // （高草上半取下面一格），每格经 `BiomeManager.getBiome` 的模糊缩放落到相邻的
        // 8 个 quart 之一；天空等环境属性在相机处按 6³ quart 高斯加权。y 按原版夹进范围。
        let (quart_x0, quart_z0, quart_y0) = (origin_x >> 2, origin_z >> 2, min_y >> 2);
        let quart_y_range = [quart_y0, quart_y0 + layers * 4 - 1];
        let mut wanted = vec![false; quarts.len()];
        let mut want = |qx: i32, qy: i32, qz: i32| {
            let qy = qy.clamp(quart_y_range[0], quart_y_range[1]);
            let local = [qx - quart_x0, qy - quart_y0, qz - quart_z0];
            if !in_grid(local.map(|v| v >> 2)) {
                return;
            }
            let s = slot[grid(local.map(|v| v >> 2))];
            if s != UNDECODED {
                let [lx, ly, lz] = local.map(|v| (v & 3) as usize);
                wanted[s as usize * 64 + (ly * 4 + lz) * 4 + lx] = true;
            }
        };
        for &(x, y, z, _, _) in &surface {
            for qy in (y - 3) >> 2..=((y - 2) >> 2) + 1 {
                for qz in (z - 4) >> 2..=(z >> 2) + 1 {
                    for qx in (x - 4) >> 2..=(x >> 2) + 1 {
                        want(qx, qy, qz);
                    }
                }
            }
        }
        let eye = [position.x, position.y + crate::EYE_HEIGHT, position.z]
            .map(|v| (v * 0.25 - 0.5).floor() as i32);
        for qy in eye[1] - 2..=eye[1] + 3 {
            for qz in eye[2] - 2..=eye[2] + 3 {
                for qx in eye[0] - 2..=eye[0] + 3 {
                    want(qx, qy, qz);
                }
            }
        }
        let mut biomes = Vec::new();
        for (s, &[sx, sy, sz]) in decoded.iter().enumerate() {
            for i in (0..64).filter(|i| wanted[s * 64 + i]) {
                let Ok(biome) = u16::try_from(quarts[s * 64 + i]) else {
                    continue;
                };
                let [lx, ly, lz] = [i & 3, i >> 4, (i >> 2) & 3].map(|v| v as i32);
                biomes.push(crate::BiomeCell {
                    quart: [
                        quart_x0 + sx * 4 + lx,
                        quart_y0 + sy * 4 + ly,
                        quart_z0 + sz * 4 + lz,
                    ],
                    biome,
                });
            }
        }

        // 每种状态只解码一次（每个线程各自一份）。
        let blocks = parallel(&surface, |surface, _, blocks| {
            let mut palette = std::collections::HashMap::new();
            for &(x, y, z, id, covered) in surface {
                let position = crate::BlockPosition { x, y, z };
                let template = palette.entry(id).or_insert_with(|| {
                    azalea::block::BlockState::try_from(id)
                        .map(|state| blocks::snapshot_from_state(state, position.clone()))
                });
                let Ok(template) = template else {
                    continue;
                };
                let mut block = template.clone();
                block.position = position;
                blocks.push(crate::RegionBlock { block, covered });
            }
        });
        Ok(crate::BlockRegion {
            snapshot,
            blocks,
            unloaded,
            view_distance,
            horizon_height: if self
                .inner
                .is_flat
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                f64::from(min_y)
            } else {
                63.0
            },
            cells,
            biome_names,
            biomes,
            biome_quart_y: quart_y_range,
            biome_zoom_seed,
            clock_ticks,
        })
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

/// 成像拷贝用的线程数：与成像一致，最多 8 个。
fn workers() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8)
}

/// 把 `items` 切成连续的几块并行处理，各块的输出按原顺序拼接。`work` 收到块、
/// 块首在 `items` 里的下标和本块的输出。
fn parallel<T: Sync, R: Send>(
    items: &[T],
    work: impl Fn(&[T], usize, &mut Vec<R>) + Sync,
) -> Vec<R> {
    let per = items.len().div_ceil(workers()).max(1);
    let work = &work;
    let parts: Vec<Vec<R>> = std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(per)
            .enumerate()
            .map(|(n, chunk)| {
                scope.spawn(move || {
                    let mut output = Vec::new();
                    work(chunk, n * per, &mut output);
                    output
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("成像拷贝线程不 panic"))
            .collect()
    });
    parts.into_iter().flatten().collect()
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

    #[tokio::test]
    async fn explicit_stop_waits_for_owner_cleanup_and_is_repeatable() {
        let inner = Arc::new(Inner::new());
        inner.mining_job.begin(
            &inner,
            mining::MiningJob::new(vec![[10, 64, -3]], inner.now_tick()),
        );
        let (done_tx, done_rx) = watch::channel(false);
        let module = Module {
            inner: inner.clone(),
            done: done_rx,
        };

        let machine_inner = inner.clone();
        let machine = tokio::spawn(async move {
            // 刻意晚于 stop 建立 waiter，钉住 notify_one 的 retained-permit 语义。
            tokio::time::sleep(Duration::from_millis(10)).await;
            machine_inner.shutdown.notified().await;
            machine_inner.end_running_jobs();
            done_tx.send_replace(true);
        });

        module.stop("测试停机").await.unwrap();
        machine.await.unwrap();
        module.stop("重复停机").await.unwrap();

        assert!(module.jobs_in_flight().is_empty());
        let latest = module.latest();
        assert!(matches!(
            &latest.phase,
            ConnectionPhase::Stopped { reason } if reason == "测试停机"
        ));
        let ended: Vec<_> = latest
            .jobs
            .entries
            .iter()
            .map(|entry| match entry.fact {
                crate::JobFact::Mine { event, .. } => event,
            })
            .collect();
        assert_eq!(ended, vec![crate::MineEvent::ConnectionEnded]);
    }

    #[tokio::test]
    async fn closed_done_channel_without_true_never_claims_stopped() {
        let inner = Arc::new(Inner::new());
        let (done_tx, done_rx) = watch::channel(false);
        let module = Module {
            inner: inner.clone(),
            done: done_rx,
        };
        drop(done_tx);

        let error = module.stop("异常停机").await.unwrap_err();

        assert!(error.contains("异常退出"), "意外错误：{error}");
        assert!(
            !matches!(&inner.latest.read().phase, ConnectionPhase::Stopped { .. }),
            "没有 owner 完成事实时绝不能发布 Stopped"
        );
    }
}
