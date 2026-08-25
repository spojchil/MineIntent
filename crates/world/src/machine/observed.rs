//! 合法寻路：只按**自己观察过的**方块规划路线。这里落实两件事。
//!
//! # 一、知不知道与是什么，都看最后所见
//!
//! `BlockMemory` 同时记三态与观察当时的注册表 `state_id`。寻路从后者恢复碰撞体；
//! 离屏后别人改了那一格，不会被动作系统提前知道。再次看见时，眼睛更新记忆版本：
//! 下一次 A* 事务使用新快照；已经交给腿的路径只把新状态用于障碍检查和局部 patch。
//!
//! # 二、未知格不进入可执行图
//!
//! fork 允许 `BlockSource` 为 `None` 指定保守 fallback。本模块让未知格保持 `None`，
//! 并把 fallback 设成一个**仅供规划器使用**的不可通行、不可站立哨兵；它从不写进
//! 记忆，也不冒充世界里真的有火。于是执行图同时要求支撑、脚部与头部依赖已观察。
//!
//! 唯一的例外是**脚下那一格**：站着的时候身体知道自己有支撑，这是本体感觉不是
//! 视觉（`standingOnBlock` 当年被删说的是「别白给模型看」，寻路不是给模型看的）。
//! 没有这个例外，刚进服还没低头看过地面时会一步都走不了。

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use azalea::block::BlockState;
use azalea::entity::{LookDirection, Physics, Position};
use azalea::pathfinder::world::{BlockSource, PathfinderBlockSource};
use azalea::registry::builtin::BlockKind;
use azalea::BlockPos;
use parking_lot::RwLock;

use super::state::Inner;
use crate::{BlockMemory, Known};

pub(crate) struct ObservedBlocks {
    memory: Arc<std::sync::Mutex<BlockMemory>>,
    /// 当前 A* 事务或执行腿使用的图。搜索期间冻结；交给腿以后才允许原子刷新到
    /// 最新已观察状态，供障碍检查和局部 patch 使用。
    planning: parking_lot::RwLock<Arc<FrozenSource>>,
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
        let planning = memory
            .lock()
            .map(|memory| memory.clone())
            .unwrap_or_default();
        Self {
            memory,
            planning: parking_lot::RwLock::new(Arc::new(FrozenSource {
                memory: planning,
                proprioceptive_floor: None,
            })),
            world,
            floor: parking_lot::Mutex::new(None),
            installed: AtomicBool::new(false),
        }
    }

    /// 每 tick 由连接层推进：站着就是脚下那格，悬空就是 `None`。
    pub(super) fn set_floor(&self, floor: Option<BlockPos>) {
        *self.floor.lock() = floor;
    }

    /// 记忆里的状态是否会让当前自动寻路规则拒绝这个精确身体节点；拒绝就给标签。
    ///
    /// 记忆里没有这一格就答 `None`——「没看过」不能拿来拒绝模型，那是把未知
    /// 说成已知（W07a）。这是写口错误文本的低频路径；每 tick 判定应在同一份规划
    /// snapshot 上调用 [`stance_is_rejected`]，避免另锁活记忆和克隆标签。
    pub(crate) fn rejected_stance_at(&self, pos: BlockPos) -> Option<String> {
        let memory = self.memory.lock().ok()?;
        rejected_stance_name(&memory, pos).map(str::to_owned)
    }

    /// 给一次规划/探索使用的冻结知识面。捕获时只克隆一次 `BlockMemory` 的区段表；
    /// 此后 snapshot、规划源刷新与 effect 传递共享同一个 `Arc<FrozenSource>`。
    /// 规划期间不会每查一格都争用活记忆的互斥锁。
    pub(super) fn snapshot(&self) -> Option<PlanningSnapshot> {
        let source = self.capture_source()?;
        Some(PlanningSnapshot::new(source))
    }

    pub(super) fn freeze(&self, snapshot: &PlanningSnapshot) {
        *self.planning.write() = snapshot.source.clone();
    }

    /// A* 结果已经交给腿以后，障碍检查和局部 patch 应读最新**已观察**状态，而不是
    /// 永远读计算起点那一版。调用方传入本 tick 已经捕获的快照，避免同一轮再次克隆
    /// 记忆和读取本体支撑；这个刷新不供异步 A* 使用。
    pub(super) fn refresh_execution_source(&self, snapshot: &PlanningSnapshot) {
        *self.planning.write() = snapshot.source.clone();
    }

    fn capture_source(&self) -> Option<Arc<FrozenSource>> {
        let memory = self.memory.lock().ok()?.clone();
        let floor = *self.floor.lock();
        let proprioceptive_floor = floor.and_then(|pos| {
            matches!(memory.state_at([pos.x, pos.y, pos.z]), Known::Unseen)
                .then(|| {
                    self.world
                        .read()
                        .get_block_state(pos)
                        .map(|state| (pos, state))
                })
                .flatten()
        });
        Some(Arc::new(FrozenSource {
            memory,
            proprioceptive_floor,
        }))
    }
}

impl BlockSource for ObservedBlocks {
    fn get_block_state(&self, pos: BlockPos) -> Option<BlockState> {
        let planning = self.planning.read();
        let remembered = match planning.memory.state_at([pos.x, pos.y, pos.z]) {
            Known::Block(fact) => Some(remembered_state(fact.state_id)),
            Known::Empty => Some(Some(BlockState::AIR)),
            Known::Unseen => None,
        };
        match remembered {
            // 合法 state_id 必须能恢复；若记忆损坏就按屏障保守失败，不能退回 live world。
            Some(Some(state)) => Some(state),
            Some(None) => Some(unknown_barrier()),
            // 未知支撑的本体感觉例外。
            None if planning
                .proprioceptive_floor
                .is_some_and(|(floor, _)| floor == pos) =>
            {
                planning.proprioceptive_floor.map(|(_, state)| state)
            }
            None => None,
        }
    }

    fn missing_block_state(&self) -> BlockState {
        // Fire 的默认 state 恰好不可通行、不可站立、不是水；这里只拿这三个位
        // 语义当未知哨兵，绝不把它写入观察记忆。
        unknown_barrier()
    }
}

struct FrozenSource {
    memory: BlockMemory,
    proprioceptive_floor: Option<(BlockPos, BlockState)>,
}

fn unknown_barrier() -> BlockState {
    BlockKind::Fire.into()
}

fn remembered_state(state_id: u32) -> Option<BlockState> {
    u16::try_from(state_id)
        .ok()
        .and_then(|state_id| BlockState::try_from(state_id).ok())
}

fn state_is_accepted_stance(state: BlockState) -> bool {
    azalea::pathfinder::world::is_block_state_passable(state)
        || azalea::pathfinder::world::is_block_state_water(state)
}

fn rejected_stance_name(memory: &BlockMemory, pos: BlockPos) -> Option<&str> {
    let Known::Block(fact) = memory.state_at([pos.x, pos.y, pos.z]) else {
        return None;
    };
    let state = remembered_state(fact.state_id)?;
    (!state_is_accepted_stance(state)).then_some(fact.name.as_str())
}

/// 这份冻结记忆是否已经足以拒绝一个精确身体节点。未知与最后所见为空都不是拒绝
/// 证据；调用方可直接复用规划 snapshot，不必另锁活记忆或为错误文本分配字符串。
pub(super) fn stance_is_rejected(memory: &BlockMemory, pos: BlockPos) -> bool {
    rejected_stance_name(memory, pos).is_some()
}

/// 一份冻结规划图在同一本观察记忆线性历史中的无碰撞身份。
///
/// `BlockMemory::revision` 只在三态或完整 `BlockFact` 真正变化时推进；后者包含原始
/// `state_id`，所以楼梯朝向等动作语义已经在版本里。脚下本体支撑来自记忆之外，另以
/// 坐标和原始 state id 入键。这里不把整本记忆重新排序、哈希，也没有哈希碰撞。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct PlanningKey {
    revision: u64,
    floor: Option<(BlockPos, u32)>,
}

impl PlanningKey {
    fn new(memory: &BlockMemory, proprioceptive_floor: Option<(BlockPos, BlockState)>) -> Self {
        Self {
            revision: memory.revision(),
            floor: proprioceptive_floor.map(|(pos, state)| (pos, u32::from(state.id()))),
        }
    }

    #[cfg(test)]
    pub(super) const fn for_test(revision: u64) -> Self {
        Self {
            revision,
            floor: None,
        }
    }
}

/// 一次规划事务的冻结知识与碰撞源。
#[derive(Clone)]
pub(super) struct PlanningSnapshot {
    source: Arc<FrozenSource>,
}

impl PlanningSnapshot {
    fn new(source: Arc<FrozenSource>) -> Self {
        Self { source }
    }

    pub(super) fn memory(&self) -> &BlockMemory {
        &self.source.memory
    }

    pub(super) fn key(&self) -> PlanningKey {
        PlanningKey::new(&self.source.memory, self.source.proprioceptive_floor)
    }
}

impl fmt::Debug for PlanningSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlanningSnapshot")
            .field("revision", &self.source.memory.revision())
            .field("known_blocks", &self.source.memory.len())
            .field("known_empty", &self.source.memory.known_empty_len())
            .field("proprioceptive_floor", &self.source.proprioceptive_floor)
            .field("key", &self.key())
            .finish()
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

/// 以身体**此刻真实朝向**完成一次观察并推进同一本方块记忆。
///
/// 战争迷雾导航到知识边界后会主动转头；若仍依赖组合根的自适应定时扫描，繁忙时
/// 最久五秒才采到这一眼，状态机只能靠拍脑袋等待。这里复用完全相同的视锥、遮挡与
/// 三态写口，让「转头 → 看见 → 地图版本增长」成为一次可验证的导航步骤。
pub(super) fn scan_current_view(inner: &Inner, bot: &azalea::Client) -> Result<u64, String> {
    let source = inner
        .observed
        .lock()
        .clone()
        .ok_or_else(|| "尚未启用观察地图寻路".to_owned())?;
    let (position, yaw, pitch) = bot
        .try_query_self::<(&Position, &LookDirection), _>(|(position, look)| {
            (**position, f64::from(look.y_rot()), f64::from(look.x_rot()))
        })
        .map_err(|_| "读取当前位置与朝向失败".to_owned())?;
    let snapshot = inner.latest.read().clone();
    let pose = crate::viewport::Pose {
        position: crate::Vec3Value {
            x: position.x,
            y: position.y,
            z: position.z,
        },
        yaw,
        pitch,
    };
    let mut free_space = crate::ObservedSpace::new();
    let world = source.world.read();
    let projection = crate::viewport::project_observing(
        &pose,
        &snapshot.entities,
        crate::viewport::WorldReader::new(
            |position| super::blocks::probe_block_from_world(&world, position),
            |position| super::blocks::read_block_from_world(&world, position),
        ),
        &crate::ViewportOptions::for_memory(),
        || Ok(()),
        &mut free_space,
    )
    .map_err(|error| error.to_string())?;
    drop(world);

    let mut memory = source
        .memory
        .lock()
        .map_err(|_| "方块记忆锁中毒".to_owned())?;
    let tick = inner.now_tick();
    memory.absorb_empty(&free_space, tick);
    memory.absorb_visible(&projection.visible_blocks.blocks, tick);
    for block in [&projection.standing_on_block, &projection.looked_at_block]
        .into_iter()
        .flatten()
    {
        memory.absorb_visible(std::slice::from_ref(block), tick);
    }
    Ok(memory.revision())
}

/// 站着时脚下那一格；悬空返回 `None`。
fn floor_under(bot: &azalea::Client) -> Option<BlockPos> {
    bot.try_query_self::<(&Position, &Physics), _>(|(position, physics)| {
        physics
            .on_ground()
            .then(|| azalea::pathfinder::player_pos_to_block_pos(**position).down(1))
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

/// 目标语义。两种意图，判据不同，实盘里模型两种都在用。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoalKind {
    /// 身体恰好占住那一格。要站上某块方块时才是这个意思。
    Exact,
    /// 到那附近就算到（球形，含 y）。「去那边」是这个意思。
    Near(u32),
}

/// 同一个目标，**全量世界**与**只按观察过的地图**各算一次。
///
/// 这是诊断口，不是运行路径：它自己构造 `CalculatePathCtx` 直接调 A*，不经过
/// 插件、不产生任何移动。
///
/// `kind` 让同一次实验能把**目标语义**也当变量：模型给的坐标常常是实心方块
/// 或悬空点，`Exact` 下 A* 把可达集穷尽也命不中。要判「换成就近能不能救回来」，
/// 必须让两种语义跑在同一份地图、同一个起点上。
pub(super) fn compare(
    inner: &Inner,
    memory: Arc<std::sync::Mutex<BlockMemory>>,
    start: BlockPos,
    goal: BlockPos,
    kind: GoalKind,
) -> Result<(PathAttempt, PathAttempt), String> {
    use azalea::pathfinder::goals::{BlockPosGoal, Goal, RadiusGoal};
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
    let snapshot = observed
        .snapshot()
        .ok_or_else(|| "冻结观察地图失败".to_owned())?;
    observed.freeze(&snapshot);

    let make_goal = || -> Arc<dyn Goal> {
        match kind {
            GoalKind::Exact => Arc::new(BlockPosGoal(goal)),
            // 球心取格心：与 `RadiusGoal::success` 里对候选格所做的 `center()`
            // 一致，不然半径会被半格的偏移吃掉。
            GoalKind::Near(radius) => Arc::new(RadiusGoal::new(goal.center(), radius as f32)),
        }
    };

    let run = |source: Option<Arc<dyn BlockSource>>| {
        let at = std::time::Instant::now();
        let found = calculate_path(CalculatePathCtx {
            entity: azalea::ecs::entity::Entity::PLACEHOLDER,
            start,
            goal: make_goal(),
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
                // 起点已经满足 goal 时，正确答案是「完整路径，零节点」。节点非空
                // 不是成功判据；`is_partial=false` 才是 A* 对 goal 命中的声明。
                found: event.path.is_some() && !event.is_partial,
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn fact(kind: BlockKind) -> crate::BlockFact {
        let state: BlockState = kind.into();
        crate::BlockFact {
            name: format!("{kind:?}"),
            state_id: u32::from(state.id()),
            properties: BTreeMap::new(),
        }
    }

    #[test]
    fn the_unknown_sentinel_is_neither_passable_nor_standable() {
        let state = unknown_barrier();
        assert!(!azalea::pathfinder::world::is_block_state_passable(state));
        assert!(!azalea::pathfinder::world::is_block_state_standable(state));
        assert!(!azalea::pathfinder::world::is_block_state_water(state));
    }

    #[test]
    fn target_stance_policy_distinguishes_water_hazards_and_solids() {
        use azalea::physics::collision::BlockWithShape;

        for (kind, has_collision, is_water, accepted) in [
            (BlockKind::Water, false, true, true),
            (BlockKind::Fire, false, false, false),
            (BlockKind::Lava, false, false, false),
            (BlockKind::PowderSnow, false, false, false),
            (BlockKind::Stone, true, false, false),
        ] {
            let state: BlockState = kind.into();
            assert_eq!(
                !state.collision_shape().to_aabbs().is_empty(),
                has_collision,
                "{kind:?} collision"
            );
            assert!(!azalea::pathfinder::world::is_block_state_passable(state));
            assert_eq!(
                azalea::pathfinder::world::is_block_state_water(state),
                is_water,
                "{kind:?} water"
            );
            assert_eq!(
                state_is_accepted_stance(state),
                accepted,
                "{kind:?} accepted body node"
            );
        }
    }

    #[test]
    fn snapshot_stance_rejection_is_pure_and_only_uses_known_blocks() {
        let at = BlockPos::new(3, 64, 7);
        let mut memory = BlockMemory::new();
        assert!(!stance_is_rejected(&memory, at), "未知不是拒绝证据");

        memory.observe([at.x, at.y, at.z], None, 1);
        assert!(!stance_is_rejected(&memory, at), "最后所见为空可以占据");

        memory.observe([at.x, at.y, at.z], Some(fact(BlockKind::Stone)), 2);
        assert!(stance_is_rejected(&memory, at), "实心身体节点必须拒绝");

        memory.observe([at.x, at.y, at.z], Some(fact(BlockKind::Water)), 3);
        assert!(
            !stance_is_rejected(&memory, at),
            "水是当前策略接受的身体节点"
        );
    }

    #[test]
    fn planning_key_tracks_memory_changes_without_hashing_the_map() {
        let at = [3, 64, 7];
        let mut memory = BlockMemory::new();
        let unseen = PlanningKey::new(&memory, None);

        memory.observe(at, Some(fact(BlockKind::Stone)), 1);
        let stone = PlanningKey::new(&memory, None);
        assert_ne!(stone, unseen);

        memory.observe(at, Some(fact(BlockKind::Stone)), 2);
        assert_eq!(
            PlanningKey::new(&memory, None),
            stone,
            "重复观察同一事实不能制造新规划版本"
        );

        memory.observe(at, Some(fact(BlockKind::Dirt)), 3);
        let dirt = PlanningKey::new(&memory, None);
        assert_ne!(
            dirt, stone,
            "上游动作会读取楼梯朝向等原始状态，BlockFact 变化必须开放新规划"
        );

        memory.observe([4, 64, 7], None, 4);
        assert_ne!(
            PlanningKey::new(&memory, None),
            dirt,
            "Unknown 变成最后所见为空必须开放一次新规划"
        );
    }

    #[test]
    fn planning_snapshot_freezes_the_proprioceptive_floor_too() {
        let old_floor = BlockPos::new(3, 63, 7);
        let new_floor = BlockPos::new(4, 63, 7);
        let stone: BlockState = BlockKind::Stone.into();
        let dirt: BlockState = BlockKind::Dirt.into();
        let memory = BlockMemory::new();
        let source = ObservedBlocks::new(
            Arc::new(std::sync::Mutex::new(memory.clone())),
            Arc::new(RwLock::new(azalea::world::World::default())),
        );
        let frozen = Arc::new(FrozenSource {
            memory: memory.clone(),
            proprioceptive_floor: Some((old_floor, stone)),
        });
        let snapshot = PlanningSnapshot::new(frozen.clone());
        let cloned = snapshot.clone();
        assert!(
            Arc::ptr_eq(&snapshot.source, &cloned.source),
            "snapshot clone 必须只增加 FrozenSource 的 Arc 引用"
        );
        source.freeze(&snapshot);
        assert!(
            Arc::ptr_eq(&source.planning.read(), &snapshot.source),
            "freeze 必须共享同一份 FrozenSource"
        );

        source.set_floor(Some(new_floor));
        assert_eq!(source.get_block_state(old_floor), Some(stone));
        assert_eq!(source.get_block_state(new_floor), None);
        assert_eq!(source.missing_block_state(), unknown_barrier());
        assert_ne!(
            PlanningKey::new(&memory, Some((old_floor, stone))),
            PlanningKey::new(&memory, Some((new_floor, stone)))
        );
        assert_ne!(
            PlanningKey::new(&memory, Some((old_floor, stone))),
            PlanningKey::new(&memory, Some((old_floor, dirt))),
            "同一脚下坐标的碰撞状态变化也必须开放新计划"
        );
    }

    #[test]
    fn block_source_stays_on_its_frozen_snapshot_until_the_next_leg() {
        let at = [3, 64, 7];
        let stone: BlockState = BlockKind::Stone.into();
        let memory = Arc::new(std::sync::Mutex::new(BlockMemory::new()));
        memory
            .lock()
            .unwrap()
            .observe(at, Some(fact(BlockKind::Stone)), 1);
        let source = ObservedBlocks::new(
            memory.clone(),
            Arc::new(RwLock::new(azalea::world::World::default())),
        );
        let snapshot = source.snapshot().unwrap();
        source.freeze(&snapshot);

        memory.lock().unwrap().observe(at, None, 2);
        assert_eq!(
            source.get_block_state(BlockPos::new(at[0], at[1], at[2])),
            Some(stone),
            "活记忆变化不能渗进正在计算的 A*"
        );

        let next = source.snapshot().unwrap();
        source.freeze(&next);
        assert_eq!(
            source.get_block_state(BlockPos::new(at[0], at[1], at[2])),
            Some(BlockState::AIR)
        );
    }

    #[test]
    fn execution_source_can_refresh_after_astar_hands_off_the_path() {
        let at = [3, 64, 7];
        let position = BlockPos::new(at[0], at[1], at[2]);
        let stone: BlockState = BlockKind::Stone.into();
        let memory = Arc::new(std::sync::Mutex::new(BlockMemory::new()));
        memory.lock().unwrap().observe(at, None, 1);
        let source = ObservedBlocks::new(
            memory.clone(),
            Arc::new(RwLock::new(azalea::world::World::default())),
        );
        let snapshot = source.snapshot().unwrap();
        source.freeze(&snapshot);

        memory
            .lock()
            .unwrap()
            .observe(at, Some(fact(BlockKind::Stone)), 2);
        assert_eq!(source.get_block_state(position), Some(BlockState::AIR));

        let latest = source.snapshot().unwrap();
        source.refresh_execution_source(&latest);
        assert_eq!(source.get_block_state(position), Some(stone));
    }
}
