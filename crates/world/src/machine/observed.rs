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
