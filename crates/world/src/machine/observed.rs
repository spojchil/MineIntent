//! 合法寻路：只按**自己观察过的**方块规划路线。
//!
//! 裁定与理由见 `docs/pathfinding-legality-decision.md`。这里只落实两件事。
//!
//! # 一、知不知道，看记忆；是什么，读世界
//!
//! `BlockMemory` 记的是「哪些格我看见过」（视锥 + 遮挡 + `ExposedFace` 判过的）。
//! 但它存的是名字与属性字符串，寻路要的是 `BlockState`（算碰撞体）。所以这里
//! **拿记忆判「知不知道」，拿世界取「是什么」**（维护者裁定，取读法乙）。
//!
//! 泄漏说清楚：你不在场时别人改了那一格，这里会报它的**现状**而不是你最后所见。
//! 兜底是走过去就会看到——观察当场进增量，azalea 的 `patch_path` 跟着修，
//! 所以泄漏窗口只有「还没走近」那一段。
//!
//! # 二、未知格的两个问题，答案相反
//!
//! 返回 `None` = 未知，azalea 那边按空气算：**可通行，但不可站立**。
//!
//! 「未知视为空气」对通行成立，对站立不成立——人得站在方块上。凭空假设有地板，
//! 规划出来的路会走进空中。
//!
//! 唯一的例外是**脚下那一格**：站着的时候身体知道自己有支撑，这是本体感觉不是
//! 视觉（`standingOnBlock` 当年被删说的是「别白给模型看」，寻路不是给模型看的）。
//! 没有这个例外，刚进服还没低头看过地面时会一步都走不了。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use azalea::block::BlockState;
use azalea::entity::{Physics, Position};
use azalea::pathfinder::world::{BlockSource, PathfinderBlockSource};
use azalea::BlockPos;
use parking_lot::RwLock;

use super::state::Inner;
use crate::BlockMemory;

pub(crate) struct ObservedBlocks {
    memory: Arc<std::sync::Mutex<BlockMemory>>,
    world: Arc<RwLock<azalea::world::World>>,
    /// 站着时脚下那一格。`None` = 悬空，没有本体感觉可用。
    floor: parking_lot::Mutex<Option<BlockPos>>,
    installed: AtomicBool,
}

impl ObservedBlocks {
    pub(crate) fn new(
        memory: Arc<std::sync::Mutex<BlockMemory>>,
        world: Arc<RwLock<azalea::world::World>>,
    ) -> Self {
        Self {
            memory,
            world,
            floor: parking_lot::Mutex::new(None),
            installed: AtomicBool::new(false),
        }
    }

    /// 每 tick 由连接层推进：站着就是脚下那格，悬空就是 `None`。
    pub(super) fn set_floor(&self, floor: Option<BlockPos>) {
        *self.floor.lock() = floor;
    }

    fn observed(&self, pos: BlockPos) -> bool {
        self.memory
            .lock()
            .map(|memory| memory.get([pos.x, pos.y, pos.z]).is_some())
            .unwrap_or(false)
    }
}

impl BlockSource for ObservedBlocks {
    fn get_block_state(&self, pos: BlockPos) -> Option<BlockState> {
        if !self.observed(pos) && *self.floor.lock() != Some(pos) {
            return None;
        }
        self.world.read().get_block_state(pos)
    }
}

/// 每 tick：首次装组件，之后只推进脚下那一格。
///
/// 装组件放在 tick 里而不是连接时，是因为世界模型要就绪之后才拿得到句柄，
/// 而 `Module::use_observed_pathfinding` 可能在任何时候被调用。
pub(super) fn tick(inner: &Inner, bot: &azalea::Client) {
    let Some(source) = inner.observed.lock().clone() else {
        return;
    };
    source.set_floor(floor_under(bot));
    if !source.installed.swap(true, Ordering::AcqRel) {
        bot.ecs
            .write()
            .entity_mut(bot.entity)
            .insert(PathfinderBlockSource(source));
    }
}

/// 站着时脚下那一格；悬空返回 `None`。
fn floor_under(bot: &azalea::Client) -> Option<BlockPos> {
    bot.try_query_self::<(&Position, &Physics), _>(|(position, physics)| {
        physics.on_ground().then(|| {
            BlockPos::new(
                position.x.floor() as i32,
                position.y.floor() as i32 - 1,
                position.z.floor() as i32,
            )
        })
    })
    .ok()
    .flatten()
}

/// 一次寻路的结果，只留能对比的三样。
#[derive(Debug)]
pub struct PathAttempt {
    /// 找到路了吗。
    pub found: bool,
    /// 路径节点数；没找到是 0。
    pub nodes: usize,
    /// 只走到一半（超时或够不着）。
    pub partial: bool,
    /// 算了多久。
    pub elapsed: std::time::Duration,
}

/// 同一个目标，**全量世界**与**只按观察过的地图**各算一次。
///
/// 这是诊断口，不是运行路径：它自己构造 `CalculatePathCtx` 直接调 A*，不经过
/// 插件、不产生任何移动。
pub(super) fn compare(
    inner: &Inner,
    memory: Arc<std::sync::Mutex<BlockMemory>>,
    start: BlockPos,
    goal: BlockPos,
) -> Result<(PathAttempt, PathAttempt), String> {
    use azalea::pathfinder::goals::BlockPosGoal;
    use azalea::pathfinder::mining::MiningCache;
    use azalea::pathfinder::{calculate_path, CalculatePathCtx, PathfinderOpts};

    let world = inner
        .world_handle
        .lock()
        .clone()
        .ok_or_else(|| "世界模型尚未就绪".to_owned())?;

    let observed = Arc::new(ObservedBlocks::new(memory, world.clone()));
    // 站在地上才有本体感觉；探针里直接按「起点脚下」给，与运行时同义。
    observed.set_floor(Some(start.down(1)));

    let run = |source: Option<Arc<dyn BlockSource>>| {
        let at = std::time::Instant::now();
        let found = calculate_path(CalculatePathCtx {
            entity: azalea::ecs::entity::Entity::PLACEHOLDER,
            start,
            goal: Arc::new(BlockPosGoal(goal)),
            world_lock: world.clone(),
            goto_id_atomic: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            mining_cache: MiningCache::new(None),
            custom_state: Default::default(),
            block_source: source,
            opts: PathfinderOpts::new().allow_mining(false),
        });
        let elapsed = at.elapsed();
        match found {
            Some(event) => PathAttempt {
                found: event.path.as_ref().is_some_and(|path| !path.is_empty()),
                nodes: event.path.as_ref().map(|path| path.len()).unwrap_or(0),
                partial: event.is_partial,
                elapsed,
            },
            None => PathAttempt {
                found: false,
                nodes: 0,
                partial: false,
                elapsed,
            },
        }
    };

    let full = run(None);
    let legal = run(Some(observed));
    Ok((full, legal))
}
