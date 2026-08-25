//! 战争迷雾下的观察边界目标。
//!
//! 本模块只回答一个认识论问题：某个已经由 A* 证明可达的身体姿态附近，是否仍有
//! 没见过的体素。它不复刻 Azalea 的 parkour、游泳、跌落等 movement graph；候选
//! 姿态及到达它的动作是否合法，仍完全由寻路器负责。到达后外层环视，再用新观察
//! 重建下一段图。

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use azalea::pathfinder::goals::{BlockPosGoal, Goal};
use azalea::BlockPos;

use crate::{BlockMemory, Known};

/// 一个 viewpoint 的稳定 frontier 身份。
///
/// `unknown_mask` 的 24 位对应八个水平邻柱在支撑/脚/头三层里的未知格。固定相对
/// offset 已由 `stance` 定位，不必为 A* 检查的每个节点分配、排序一组世界坐标。
/// 它故意不是“动作依赖”：上游 default_move 还含多格 parkour、下落与水中移动，
/// 在这里手抄一份迟早漂移。A* 证明“能到 viewpoint”，环视证明“获得了什么新知识”。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct FrontierKey {
    stance: BlockPos,
    unknown_mask: u32,
}

impl FrontierKey {
    /// 从一份观察快照计算 key。附近没有未知格就不是 frontier。
    pub(super) fn from_memory(memory: &BlockMemory, stance: BlockPos) -> Option<Self> {
        let mut unknown_mask = 0_u32;
        for_each_viewpoint_dependency(stance, |bit, at| {
            if matches!(memory.state_at(at), Known::Unseen) {
                unknown_mask |= bit;
            }
        });
        if unknown_mask == 0 {
            return None;
        }
        Some(Self {
            stance,
            unknown_mask,
        })
    }

    #[cfg(test)]
    fn stance(&self) -> BlockPos {
        self.stance
    }

    #[cfg(test)]
    fn unseen_dependencies(&self) -> Vec<[i32; 3]> {
        let mut dependencies = Vec::with_capacity(self.unknown_mask.count_ones() as usize);
        for_each_viewpoint_dependency(self.stance, |bit, at| {
            if self.unknown_mask & bit != 0 {
                dependencies.push(at);
            }
        });
        dependencies.sort_unstable();
        dependencies
    }
}

/// 选择一个已知可达、能扩张观察知识边界的 stance。
///
/// `memory` 是构造时冻结的快照。目标谓词不会在一次 A* 搜索中随活地图漂移；新观察
/// 应由外层以新快照构造新目标。`destination` 只引导启发式，frontier 达成不等于最终
/// 精确目的地已经达成。
#[derive(Clone)]
pub(super) struct FrontierGoal {
    /// 构造时只复制一次冻结记忆的区段表；phase 与投递 effect 之间 clone goal 时
    /// 只增加 Arc 引用，不再重复复制整张 section map。
    memory: Arc<BlockMemory>,
    destination: BlockPos,
    start: BlockPos,
    exhausted: Arc<HashSet<FrontierKey>>,
}

impl fmt::Debug for FrontierGoal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrontierGoal")
            .field("destination", &self.destination)
            .field("start", &self.start)
            .field("memory_revision", &self.memory.revision())
            .field("known_blocks", &self.memory.len())
            .field("known_empty", &self.memory.known_empty_len())
            .field("exhausted_count", &self.exhausted.len())
            .finish()
    }
}

impl FrontierGoal {
    pub(super) fn new(
        memory: BlockMemory,
        destination: BlockPos,
        start: BlockPos,
        exhausted: HashSet<FrontierKey>,
    ) -> Self {
        Self {
            memory: Arc::new(memory),
            destination,
            start,
            exhausted: Arc::new(exhausted),
        }
    }

    /// 用本目标持有的快照计算一个 stance 当前的 frontier key。
    pub(super) fn key_at(&self, stance: BlockPos) -> Option<FrontierKey> {
        FrontierKey::from_memory(self.memory.as_ref(), stance)
    }
}

impl Goal for FrontierGoal {
    fn heuristic(&self, n: BlockPos) -> f32 {
        BlockPosGoal(self.destination).heuristic(n)
    }

    fn success(&self, n: BlockPos) -> bool {
        // 当前格即使紧邻未知也不能成为零步成功，否则外层会在同一 viewpoint 空转。
        if n == self.start {
            return false;
        }
        self.key_at(n)
            .is_some_and(|key| !self.exhausted.contains(&key))
    }
}

/// 遍历一个足够小、与具体动作集合解耦的观察邻域。只看相邻水平柱的支撑/脚/头：
/// 若把脚下更深的未知也算进来，每个实心地面上方都会成为永远看不穿的伪 frontier。
/// 更远的 parkour/深落点会在真正的边缘 viewpoint 环视后逐步进入记忆。
fn for_each_viewpoint_dependency(stance: BlockPos, mut visit: impl FnMut(u32, [i32; 3])) {
    let mut index = 0_u32;
    for dx in -1..=1 {
        for dz in -1..=1 {
            if dx == 0 && dz == 0 {
                continue;
            }
            for dy in -1..=1 {
                let bit = 1_u32 << index;
                index += 1;
                let (Some(x), Some(y), Some(z)) = (
                    stance.x.checked_add(dx),
                    stance.y.checked_add(dy),
                    stance.z.checked_add(dz),
                ) else {
                    continue;
                };
                visit(bit, [x, y, z]);
            }
        }
    }
    debug_assert_eq!(index, 24);
}

#[cfg(test)]
fn viewpoint_dependencies(stance: BlockPos) -> Vec<[i32; 3]> {
    let mut dependencies = Vec::with_capacity(24);
    for_each_viewpoint_dependency(stance, |_, at| dependencies.push(at));
    dependencies.sort_unstable();
    dependencies
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    fn observe_all_except(memory: &mut BlockMemory, stance: BlockPos, unseen: &[[i32; 3]]) {
        for at in viewpoint_dependencies(stance) {
            if !unseen.contains(&at) {
                memory.observe(at, None, 1);
            }
        }
    }

    fn goal(memory: BlockMemory, start: BlockPos, exhausted: HashSet<FrontierKey>) -> FrontierGoal {
        FrontierGoal::new(memory, pos(20, 64, 0), start, exhausted)
    }

    #[test]
    fn current_stance_is_never_a_successful_frontier() {
        let start = pos(0, 64, 0);
        let goal = goal(BlockMemory::new(), start, HashSet::new());

        let key = goal.key_at(start).expect("空快照四周确实存在未知边界");
        assert_eq!(key.unknown_mask.count_ones(), 24);
        assert!(!goal.success(start), "frontier 不能零步命中当前格");
    }

    #[test]
    fn heuristic_is_directed_toward_the_final_exact_destination() {
        let goal = goal(BlockMemory::new(), pos(0, 64, 0), HashSet::new());

        assert!(goal.heuristic(pos(15, 64, 0)) < goal.heuristic(pos(5, 64, 0)));
        assert_eq!(goal.heuristic(pos(20, 64, 0)), 0.0);
    }

    #[test]
    fn fork_exact_and_frontier_heuristics_are_safe_at_extreme_coordinates() {
        let low = pos(i32::MIN, i32::MIN, i32::MIN);
        let high = pos(i32::MAX, i32::MAX, i32::MAX);
        let exact = BlockPosGoal(high);
        assert!(exact.heuristic(low).is_finite());
        assert!(exact.heuristic(low) > 0.0);
        assert!(exact.success(high));
        assert!(!exact.success(low));

        let frontier = FrontierGoal::new(BlockMemory::new(), high, low, HashSet::new());
        assert!(frontier.heuristic(low).is_finite());
        for edge in [low, high] {
            let dependencies = viewpoint_dependencies(edge);
            assert_eq!(dependencies.len(), 6);
            assert_eq!(
                FrontierKey::from_memory(&BlockMemory::new(), edge)
                    .expect("极值坐标仍有六个合法相邻依赖")
                    .unknown_mask
                    .count_ones(),
                6
            );
            assert!(dependencies.windows(2).all(|pair| pair[0] < pair[1]));
            assert!(dependencies.iter().all(|at| {
                at[0].abs_diff(edge.x) <= 1
                    && at[1].abs_diff(edge.y) <= 1
                    && at[2].abs_diff(edge.z) <= 1
            }));
        }
    }

    #[test]
    fn fully_known_action_neighborhood_is_not_a_frontier() {
        let stance = pos(4, 64, 4);
        let mut memory = BlockMemory::new();
        observe_all_except(&mut memory, stance, &[]);
        let goal = goal(memory, pos(0, 64, 0), HashSet::new());

        assert_eq!(goal.key_at(stance), None);
        assert!(!goal.success(stance));
    }

    #[test]
    fn an_unknown_action_boundary_is_a_frontier_with_a_canonical_key() {
        let stance = pos(4, 64, 4);
        let unknown = [5, 64, 4];
        let mut memory = BlockMemory::new();
        observe_all_except(&mut memory, stance, &[unknown]);
        let goal = goal(memory, pos(0, 64, 0), HashSet::new());

        let key = goal.key_at(stance).expect("留有一个未知动作依赖");
        assert_eq!(key.stance(), stance);
        assert_eq!(key.unseen_dependencies(), vec![unknown]);
        assert!(key
            .unseen_dependencies()
            .windows(2)
            .all(|pair| pair[0] < pair[1]));
        assert!(goal.success(stance));
    }

    #[test]
    fn an_exhausted_key_cannot_succeed_again() {
        let stance = pos(4, 64, 4);
        let mut memory = BlockMemory::new();
        observe_all_except(&mut memory, stance, &[[5, 64, 4]]);
        let key = FrontierKey::from_memory(&memory, stance).expect("应是 frontier");
        let goal = goal(memory, pos(0, 64, 0), HashSet::from([key]));

        assert!(!goal.success(stance));
    }

    #[test]
    fn local_observation_changes_the_key_and_reopens_the_viewpoint() {
        let stance = pos(4, 64, 4);
        let first_unknown = [5, 64, 4];
        let second_unknown = [5, 65, 4];
        let mut before = BlockMemory::new();
        observe_all_except(&mut before, stance, &[first_unknown, second_unknown]);
        let old_key = FrontierKey::from_memory(&before, stance).expect("应是 frontier");

        let mut after = before.clone();
        after.observe(first_unknown, None, 2);
        let new_key = FrontierKey::from_memory(&after, stance).expect("仍余一个未知依赖");
        assert_ne!(new_key, old_key);
        assert_eq!(new_key.unseen_dependencies(), vec![second_unknown]);

        let goal = goal(after, pos(0, 64, 0), HashSet::from([old_key]));
        assert!(goal.success(stance), "局部知识变化后不能被旧 key 压住");
    }

    #[test]
    fn viewpoint_neighborhood_includes_neighbor_support_feet_and_head() {
        let stance = pos(4, 64, 4);
        let up_headroom = [5, 65, 4];
        let down_support = [5, 63, 4];
        let mut memory = BlockMemory::new();
        observe_all_except(&mut memory, stance, &[up_headroom, down_support]);

        let key = FrontierKey::from_memory(&memory, stance).expect("斜坡依赖应形成 frontier");
        assert_eq!(
            key.unseen_dependencies(),
            vec![down_support, up_headroom],
            "dy=-1/0/1 的未知依赖必须稳定编码"
        );
    }
}
