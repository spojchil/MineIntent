//! 视口增量核：方块记忆与逐格 diff（纯函数层，暂不接线）。
//!
//! 记忆记录的是「已经成功送入模型上下文的事实」，不是第二份世界真相
//! （沿承《增量视口实验.md》@ a6034af 的立场）。因此推进纪律是硬的：
//! [`diff`] 只读不写；产出的变化只有在承载它的模型请求**确实发出**之后，
//! 才允许用 [`BlockMemory::apply`] 推进记忆——请求失败，记忆不动，
//! 下次 diff 自然重报。这是旧设计两阶段提交在单写者内核下的全部残留；
//! stale proposal、并行批准入等防御随旧运行时的并发一起退役。
//!
//! 判定表（缺席永远不当空气）：
//!
//! | 能看到 | 记忆里 | 结论 |
//! |---|---|---|
//! | 是 | 没有 | [`BlockChange::Appeared`] |
//! | 是 | 有，身份不同 | [`BlockChange::Changed`] |
//! | 亲眼可证为空 | 有 | [`BlockChange::Vanished`] |
//! | 否 | 有 | 沉默，记忆保留 |
//! | 是 | 有，相同 | 沉默 |
//!
//! 「亲眼可证为空」= 该格已加载为空气且射线通达——由调用方以探针提供
//! （几何在视口扫描侧，本层不复刻）。剪枝同理：`scope` 圈出本次扫描
//! 覆盖的几何范围，范围外的记忆连探针都不问，直接沉默。
//!
//! 未来第二个消费者：寻路合法域（「同伴曾见」的空间）也从这份记忆读，
//! 届时加「已见空气」的记法与「已投递」水位线，本层形状为此留有余地。

use std::collections::{BTreeMap, HashMap};

use crate::block::is_air_name;

use super::ViewportBlock;

/// 一格方块的最后所见身份（与 [`ViewportBlock`] 同构，不带坐标）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockFact {
    pub name: String,
    pub properties: BTreeMap<String, String>,
}

impl BlockFact {
    fn of(block: &ViewportBlock) -> Self {
        Self {
            name: block.name.clone(),
            properties: block.properties.clone(),
        }
    }
}

/// 方块记忆：位置 → 最后所见。只记非空气；空气=没有条目。
#[derive(Clone, Debug, Default)]
pub struct BlockMemory {
    facts: HashMap<[i32; 3], BlockFact>,
}

impl BlockMemory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, at: [i32; 3]) -> Option<&BlockFact> {
        self.facts.get(&at)
    }

    pub fn len(&self) -> usize {
        self.facts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
    }

    /// 推进记忆。只在变化确实送达模型之后调用（见模块头的推进纪律）。
    pub fn apply(&mut self, changes: &[BlockChange]) {
        for change in changes {
            match change {
                BlockChange::Appeared { at, fact } => {
                    self.facts.insert(*at, fact.clone());
                }
                BlockChange::Changed { at, now, .. } => {
                    self.facts.insert(*at, now.clone());
                }
                BlockChange::Vanished { at, .. } => {
                    self.facts.remove(at);
                }
            }
        }
    }
}

/// 一次 diff 的产出：三种亲眼可证的变化。
#[derive(Clone, Debug, PartialEq)]
pub enum BlockChange {
    /// 记忆没有、这次看见了。
    Appeared { at: [i32; 3], fact: BlockFact },
    /// 同一格身份变化（名字或属性）。
    Changed {
        at: [i32; 3],
        was: BlockFact,
        now: BlockFact,
    },
    /// 该格现在亲眼可证为空。
    Vanished { at: [i32; 3], was: BlockFact },
}

/// 对比当前可见集与记忆，产出变化清单。只读，不推进记忆。
///
/// - `visible`：本次扫描的可见方块（扫描侧已保证非空气、无重复）；
/// - `scope`：本次扫描覆盖的几何范围（剪枝——范围外的记忆不做任何判断）；
/// - `visibly_empty`：「该格现在亲眼可证为空吗」探针，只对
///   范围内、记忆有、可见集缺席的格调用。
///
/// 输出顺序确定：Appeared/Changed 按 `visible` 的既有排序（近到远），
/// Vanished 按坐标字典序。
pub fn diff(
    memory: &BlockMemory,
    visible: &[ViewportBlock],
    scope: impl Fn([i32; 3]) -> bool,
    mut visibly_empty: impl FnMut([i32; 3]) -> bool,
) -> Vec<BlockChange> {
    let mut changes = Vec::new();
    let mut seen_now: HashMap<[i32; 3], ()> = HashMap::with_capacity(visible.len());
    for block in visible {
        // 防御：空气不该出现在可见集里；真出现也不入账。
        if is_air_name(&block.name) {
            continue;
        }
        seen_now.insert(block.position, ());
        let now = BlockFact::of(block);
        match memory.get(block.position) {
            None => changes.push(BlockChange::Appeared {
                at: block.position,
                fact: now,
            }),
            Some(was) if *was != now => changes.push(BlockChange::Changed {
                at: block.position,
                was: was.clone(),
                now,
            }),
            Some(_) => {}
        }
    }

    let mut vanish_candidates: Vec<[i32; 3]> = memory
        .facts
        .keys()
        .filter(|at| !seen_now.contains_key(*at) && scope(**at))
        .copied()
        .collect();
    vanish_candidates.sort_unstable();
    for at in vanish_candidates {
        if visibly_empty(at) {
            let was = memory.get(at).expect("候选来自记忆本身").clone();
            changes.push(BlockChange::Vanished { at, was });
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    fn block(at: [i32; 3], name: &str) -> ViewportBlock {
        ViewportBlock {
            name: name.to_owned(),
            properties: BTreeMap::new(),
            position: at,
        }
    }

    fn fact(name: &str) -> BlockFact {
        BlockFact {
            name: name.to_owned(),
            properties: BTreeMap::new(),
        }
    }

    /// 判定表主线：新见入 Appeared、变身入 Changed、相同沉默。
    #[test]
    fn appeared_changed_and_silence_follow_the_table() {
        let mut memory = BlockMemory::new();
        memory.apply(&[
            BlockChange::Appeared {
                at: [0, 64, 0],
                fact: fact("stone"),
            },
            BlockChange::Appeared {
                at: [1, 64, 0],
                fact: fact("dirt"),
            },
        ]);
        let visible = vec![
            block([0, 64, 0], "stone"),   // 相同 → 沉默
            block([1, 64, 0], "furnace"), // 变身 → Changed
            block([2, 64, 0], "oak_log"), // 新见 → Appeared
        ];
        let changes = diff(&memory, &visible, |_| true, |_| false);
        assert_eq!(
            changes,
            vec![
                BlockChange::Changed {
                    at: [1, 64, 0],
                    was: fact("dirt"),
                    now: fact("furnace"),
                },
                BlockChange::Appeared {
                    at: [2, 64, 0],
                    fact: fact("oak_log"),
                },
            ]
        );
    }

    /// 消失要亲眼可证：探针说空才报，说不清就沉默保留记忆。
    #[test]
    fn vanished_needs_eyewitness_proof_of_emptiness() {
        let mut memory = BlockMemory::new();
        memory.apply(&[
            BlockChange::Appeared {
                at: [5, 64, 5],
                fact: fact("chest"),
            },
            BlockChange::Appeared {
                at: [6, 64, 5],
                fact: fact("stone"),
            },
        ]);
        // 两格都缺席；只有 [5,64,5] 亲眼可证为空。
        let changes = diff(&memory, &[], |_| true, |at| at == [5, 64, 5]);
        assert_eq!(
            changes,
            vec![BlockChange::Vanished {
                at: [5, 64, 5],
                was: fact("chest"),
            }]
        );
        // diff 不推进记忆：两格都还在。
        assert_eq!(memory.len(), 2);
    }

    /// 剪枝是隐私与工作量边界：范围外的记忆连探针都不该被问到。
    #[test]
    fn out_of_scope_memory_is_never_probed() {
        let mut memory = BlockMemory::new();
        memory.apply(&[BlockChange::Appeared {
            at: [100, 64, 100],
            fact: fact("stone"),
        }]);
        let probed = RefCell::new(Vec::new());
        let changes = diff(
            &memory,
            &[],
            |_| false, // 一切都在范围外
            |at| {
                probed.borrow_mut().push(at);
                true
            },
        );
        assert!(changes.is_empty());
        assert!(probed.borrow().is_empty(), "范围外不得触发探针");
    }

    /// 推进纪律：apply 之后同一观察再 diff 应无话可说；
    /// 不 apply（请求失败）则下次原样重报。
    #[test]
    fn apply_advances_and_skipping_apply_replays() {
        let mut memory = BlockMemory::new();
        let visible = vec![block([0, 64, 0], "stone")];
        let first = diff(&memory, &visible, |_| true, |_| false);
        assert_eq!(first.len(), 1);
        // 请求失败：不 apply，重报。
        let replay = diff(&memory, &visible, |_| true, |_| false);
        assert_eq!(replay, first);
        // 请求成功：apply 后沉默。
        memory.apply(&first);
        let silent = diff(&memory, &visible, |_| true, |_| false);
        assert!(silent.is_empty());
        // 消失同理：apply Vanished 后条目移除。
        let vanished = diff(&memory, &[], |_| true, |_| true);
        memory.apply(&vanished);
        assert!(memory.is_empty());
    }

    /// 防御：可见集里混入空气不入账；Vanished 输出按坐标字典序确定。
    #[test]
    fn air_is_ignored_and_vanish_order_is_deterministic() {
        let mut memory = BlockMemory::new();
        memory.apply(&[
            BlockChange::Appeared {
                at: [2, 64, 0],
                fact: fact("stone"),
            },
            BlockChange::Appeared {
                at: [1, 64, 0],
                fact: fact("stone"),
            },
        ]);
        let with_air = vec![block([9, 64, 9], "air")];
        let changes = diff(&memory, &with_air, |_| true, |_| true);
        assert_eq!(
            changes,
            vec![
                BlockChange::Vanished {
                    at: [1, 64, 0],
                    was: fact("stone"),
                },
                BlockChange::Vanished {
                    at: [2, 64, 0],
                    was: fact("stone"),
                },
            ]
        );
    }
}
