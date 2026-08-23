//! 视口增量核：方块记忆与逐格 diff。
//!
//! # 两半，用途已经分开
//!
//! **记忆**是主角：眼睛每 250ms 把合法可见的方块整份吸进来（`Module::absorb`），
//! 寻路读它、`blocks` 工具查它。它是同伴的「磁盘」，不进会话区，也不被压缩碰。
//!
//! **diff 现在没有主消费者**。方块不再推给模型，所以记忆只需要「把看见
//! 的收进来」，不需要知道哪些是新的——实测差异那一层是 +3.7ms / 35%，眼睛这条路
//! 已经不付这笔钱了。
//!
//! 保留它有两个理由，都写在这里免得被当成死代码删掉：
//!
//! 1. `scan` 工具的 `changes` 模式仍然在用——那是模型**主动要**的差异；
//! 2. **将来的订阅**（「盯着这个熔炉」「盯着我身后那片」）以它为原料。推与拉的死结
//!    解在那儿：默认拉取，关心的东西才订阅式推送。
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

/// 事实的模型可见标签（身份判据）：名称+白名单视觉属性。
fn visible_label(fact: &BlockFact) -> String {
    crate::block::visible_block_label(&fact.name, &fact.properties)
}

/// 一格的记忆条目：身份，加上**最后一次看到它的刻**。
///
/// 时间不进 [`BlockFact`]：那个类型的相等语义是 [`diff`] 的身份判据
/// （见 [`visible_label`]），掺进时间会让「同一块石头又看了一眼」变成一次变化。
#[derive(Clone, Debug)]
struct Entry {
    fact: BlockFact,
    last_seen: u64,
}

/// 方块记忆：位置 → 最后所见。只记非空气。
///
/// 「没有条目」**不等于**「那里是空的」——它同时是「从没看过」。这一位由
/// [`crate::ObservedSpace`] 补，两本合起来才是三态。
#[derive(Clone, Debug, Default)]
pub struct BlockMemory {
    facts: HashMap<[i32; 3], Entry>,
}

impl BlockMemory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, at: [i32; 3]) -> Option<&BlockFact> {
        self.facts.get(&at).map(|entry| &entry.fact)
    }

    /// 最后一次看到这一格是第几刻。
    ///
    /// 这具身体没有「同时」：它对世界的知识是不同时刻观测的编织物，
    /// 任何「当前场景」都是查询重构的。陈旧度因此不是附加信息，
    /// 是每条事实自带的一半——「三分钟前见过」和「刚刚看到」必须分得开。
    pub fn last_seen(&self, at: [i32; 3]) -> Option<u64> {
        self.facts.get(&at).map(|entry| entry.last_seen)
    }

    /// 遍历记住的每一格。
    ///
    /// 给**查询**用：模型问「附近有什么树」时，机器在这本记忆里找，而不是去翻
    /// 实时世界——只答得出观察过的东西，与合法信息边界天然一致。
    pub fn iter(&self) -> impl Iterator<Item = ([i32; 3], &BlockFact)> {
        self.facts.iter().map(|(at, entry)| (*at, &entry.fact))
    }

    /// 同 [`Self::iter`]，另带最后所见的刻。给需要呈现陈旧度的查询用。
    pub fn iter_seen(&self) -> impl Iterator<Item = ([i32; 3], &BlockFact, u64)> {
        self.facts
            .iter()
            .map(|(at, entry)| (*at, &entry.fact, entry.last_seen))
    }

    pub fn len(&self) -> usize {
        self.facts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
    }

    /// 推进记忆。只在变化确实送达模型之后调用（见模块头的推进纪律）。
    ///
    /// `at_tick` 是这次观察发生的刻，写进条目的 `last_seen`。
    pub fn apply(&mut self, changes: &[BlockChange], at_tick: u64) {
        for change in changes {
            match change {
                BlockChange::Appeared { at, fact } => {
                    self.facts.insert(
                        *at,
                        Entry {
                            fact: fact.clone(),
                            last_seen: at_tick,
                        },
                    );
                }
                BlockChange::Changed { at, now, .. } => {
                    self.facts.insert(
                        *at,
                        Entry {
                            fact: now.clone(),
                            last_seen: at_tick,
                        },
                    );
                }
                BlockChange::Vanished { at, .. } => {
                    self.facts.remove(at);
                }
            }
        }
    }
}

/// 观察源接入：把一次**已送达模型**的观察吸收进记忆。
///
/// 与 [`diff`]/[`BlockMemory::apply`] 的两步不同，吸收是单步 upsert——
/// 适用于观察本身就是模型收到的工具回执的场合（内核的 settled 通道
/// 保证已定回执必达模型，所以产出时即可上账）。増量帧走 diff/apply
/// 两步，工具回执走吸收，两者写同一本记忆。
impl BlockMemory {
    /// 吸收全量/扫描可见集：逐格 upsert。空气防御同 [`diff`]。
    /// 只上账正面观察；本次没列出的格不动（缺席不当空气）。
    pub fn absorb_visible(&mut self, visible: &[ViewportBlock], at_tick: u64) {
        for block in visible {
            if is_air_name(&block.name) {
                continue;
            }
            // 身份没变也要刷新 `last_seen`：又看了一眼，这条事实就没那么旧了。
            self.facts.insert(
                block.position,
                Entry {
                    fact: BlockFact::of(block),
                    last_seen: at_tick,
                },
            );
        }
    }

    /// 吸收定向结果：看见方块=upsert；亲眼见空=销账（消失确认）；
    /// 各种「看不见」不动记忆。
    pub fn absorb_directed(&mut self, projection: &super::DirectedProjection, at_tick: u64) {
        for seen in &projection.seen {
            if is_air_name(&seen.name) {
                self.facts.remove(&seen.at);
            } else {
                self.facts.insert(
                    seen.at,
                    Entry {
                        fact: BlockFact {
                            name: seen.name.clone(),
                            properties: seen.properties.clone(),
                        },
                        last_seen: at_tick,
                    },
                );
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
            // 身份=模型可见标签（名称+白名单视觉属性），与全量/定向同一种
            // 语言（承旧线 BlockInfo）。白名单外的协议属性不参与差异——
            // 比出来也只会是两行字面相同的 +/-。
            Some(was) if visible_label(was) != visible_label(&now) => {
                changes.push(BlockChange::Changed {
                    at: block.position,
                    was: was.clone(),
                    now,
                })
            }
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
        memory.apply(
            &[
                BlockChange::Appeared {
                    at: [0, 64, 0],
                    fact: fact("stone"),
                },
                BlockChange::Appeared {
                    at: [1, 64, 0],
                    fact: fact("dirt"),
                },
            ],
            0,
        );
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

    /// 身份=名称+白名单视觉属性：熔炉燃灭算变化，白名单外的协议属性不算。
    #[test]
    fn visible_state_changes_diff_but_protocol_properties_do_not() {
        let lit = |value: &str| ViewportBlock {
            name: "furnace".to_owned(),
            properties: BTreeMap::from([("lit".to_owned(), value.to_owned())]),
            position: [0, 64, 0],
        };
        let mut memory = BlockMemory::new();
        memory.absorb_visible(&[lit("false")], 0);
        // 燃起来了：白名单属性变 → Changed。
        let changes = diff(&memory, &[lit("true")], |_| true, |_| false);
        assert_eq!(changes.len(), 1);
        assert!(
            matches!(&changes[0], BlockChange::Changed { was, now, .. }
                if was.properties["lit"] == "false" && now.properties["lit"] == "true"),
            "{changes:?}"
        );
        // 白名单外的协议属性（如树叶 distance）变化：沉默。
        let internal = |value: &str| ViewportBlock {
            name: "oak_leaves".to_owned(),
            properties: BTreeMap::from([("distance".to_owned(), value.to_owned())]),
            position: [1, 70, 0],
        };
        let mut leaves = BlockMemory::new();
        leaves.absorb_visible(&[internal("1")], 0);
        let silent = diff(&leaves, &[internal("3")], |_| true, |_| false);
        assert!(silent.is_empty(), "{silent:?}");
    }

    /// 消失要亲眼可证：探针说空才报，说不清就沉默保留记忆。
    #[test]
    fn vanished_needs_eyewitness_proof_of_emptiness() {
        let mut memory = BlockMemory::new();
        memory.apply(
            &[
                BlockChange::Appeared {
                    at: [5, 64, 5],
                    fact: fact("chest"),
                },
                BlockChange::Appeared {
                    at: [6, 64, 5],
                    fact: fact("stone"),
                },
            ],
            0,
        );
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
        memory.apply(
            &[BlockChange::Appeared {
                at: [100, 64, 100],
                fact: fact("stone"),
            }],
            0,
        );
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
        memory.apply(&first, 0);
        let silent = diff(&memory, &visible, |_| true, |_| false);
        assert!(silent.is_empty());
        // 消失同理：apply Vanished 后条目移除。
        let vanished = diff(&memory, &[], |_| true, |_| true);
        memory.apply(&vanished, 0);
        assert!(memory.is_empty());
    }

    /// 吸收：可见集 upsert，定向见空销账，看不见不动。
    #[test]
    fn absorption_upserts_seen_and_erases_witnessed_empties() {
        use crate::viewport::{
            DirectedProjection, DirectedSeenBlock, DirectedUnseenBlock, DirectedWhy,
        };

        let mut memory = BlockMemory::new();
        memory.absorb_visible(&[block([0, 64, 0], "stone"), block([9, 64, 9], "air")], 0);
        assert_eq!(memory.get([0, 64, 0]), Some(&fact("stone")));
        assert_eq!(memory.get([9, 64, 9]), None, "空气不入账");

        // 定向：一格见到新方块、一格亲眼见空、一格被挡。
        memory.absorb_directed(
            &DirectedProjection {
                seen: vec![
                    DirectedSeenBlock {
                        at: [1, 64, 0],
                        name: "furnace".to_owned(),
                        properties: BTreeMap::new(),
                    },
                    DirectedSeenBlock {
                        at: [0, 64, 0],
                        name: "air".to_owned(),
                        properties: BTreeMap::new(),
                    },
                ],
                unseen: vec![DirectedUnseenBlock {
                    at: [2, 64, 0],
                    why: vec![DirectedWhy::Occluded],
                    distance: None,
                    max: None,
                    by: None,
                }],
            },
            0,
        );
        assert_eq!(memory.get([1, 64, 0]), Some(&fact("furnace")));
        assert_eq!(memory.get([0, 64, 0]), None, "亲眼见空销账");
        assert_eq!(memory.len(), 1);
    }

    /// 防御：可见集里混入空气不入账；Vanished 输出按坐标字典序确定。
    #[test]
    fn air_is_ignored_and_vanish_order_is_deterministic() {
        let mut memory = BlockMemory::new();
        memory.apply(
            &[
                BlockChange::Appeared {
                    at: [2, 64, 0],
                    fact: fact("stone"),
                },
                BlockChange::Appeared {
                    at: [1, 64, 0],
                    fact: fact("stone"),
                },
            ],
            0,
        );
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

#[cfg(test)]
mod staleness_tests {
    use super::*;

    fn block(at: [i32; 3], name: &str) -> ViewportBlock {
        ViewportBlock {
            name: name.to_owned(),
            properties: BTreeMap::new(),
            position: at,
        }
    }

    #[test]
    fn absorbing_records_the_tick_it_was_seen_at() {
        let mut memory = BlockMemory::new();
        memory.absorb_visible(&[block([0, 64, 0], "stone")], 4032);
        assert_eq!(memory.last_seen([0, 64, 0]), Some(4032));
        assert_eq!(memory.last_seen([1, 64, 0]), None, "没记过的格没有时间");
    }

    /// 又看了一眼：时间刷新，但**身份没变就不是一次变化**。
    /// 这正是时间不能进 `BlockFact` 的理由——进去了，每看一眼都会报变化。
    #[test]
    fn seeing_it_again_refreshes_time_without_becoming_a_change() {
        let mut memory = BlockMemory::new();
        memory.absorb_visible(&[block([0, 64, 0], "stone")], 100);

        let again = [block([0, 64, 0], "stone")];
        let changes = diff(&memory, &again, |_| true, |_| false);
        assert!(
            changes.is_empty(),
            "同一块石头再看一眼不是变化：{changes:?}"
        );

        memory.absorb_visible(&again, 200);
        assert_eq!(memory.last_seen([0, 64, 0]), Some(200), "时间要刷新");
    }

    /// 身份真的变了：既报变化，也刷新时间。
    #[test]
    fn a_real_change_updates_both_identity_and_time() {
        let mut memory = BlockMemory::new();
        memory.absorb_visible(&[block([0, 64, 0], "furnace")], 100);
        let now = [block([0, 64, 0], "stone")];
        let changes = diff(&memory, &now, |_| true, |_| false);
        assert_eq!(changes.len(), 1, "身份变了应当报一条：{changes:?}");
        memory.apply(&changes, 300);
        assert_eq!(
            memory.get([0, 64, 0]).map(|f| f.name.as_str()),
            Some("stone")
        );
        assert_eq!(memory.last_seen([0, 64, 0]), Some(300));
    }

    /// 遍历带时间：查询侧要能把陈旧度一并呈现出来。
    #[test]
    fn iter_seen_carries_the_tick() {
        let mut memory = BlockMemory::new();
        memory.absorb_visible(&[block([0, 64, 0], "stone")], 7);
        let seen: Vec<_> = memory.iter_seen().collect();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, [0, 64, 0]);
        assert_eq!(seen[0].2, 7);
    }
}
