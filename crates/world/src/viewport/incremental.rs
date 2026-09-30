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
//! 存储形状见 [`BlockMemory`]：一张关系两种表示，按区段分片。
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
//! 「已见空气」的记法已经落位（[`BlockMemory::observe`] 的 `None` 分支），
//! 寻路合法域要的「同伴曾见的可通行空间」由它提供；接不接、怎么接是寻路
//! 那边的裁定，本层只负责把这一位记准。

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::block::is_air_name;

use super::observed_space::{bit_index, section_of, WORDS_PER_SECTION};
use super::{ObservedSpace, ViewportBlock};

/// 一格方块的最后所见身份（与 [`ViewportBlock`] 同构，不带坐标）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockFact {
    pub name: String,
    /// 观察发生时的注册表状态。寻路只读这份最后所见，不回读离屏世界。
    pub state_id: u32,
    pub properties: BTreeMap<String, String>,
}

impl BlockFact {
    fn of(block: &ViewportBlock) -> Self {
        Self {
            name: block.name.clone(),
            state_id: block.state_id,
            properties: block.properties.clone(),
        }
    }
}

/// 一格的记忆条目：身份，加上**最后一次看到它的刻**。
///
/// 时间不进 [`BlockFact`]：那个类型的相等语义是身份，掺进时间会让「同一块石头
/// 又看了一眼」变成一次变化。
#[derive(Clone, Debug)]
struct Entry {
    fact: BlockFact,
    last_seen: u64,
}

/// 一格观察的结论。**三态只从这一个出口给出**，调用方不必知道底下是两种表示。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Known<'a> {
    /// 看过，有东西（含水、草这类非空气方块）。
    Block(&'a BlockFact),
    /// 看过，是空的。
    Empty,
    /// 没看过。
    Unseen,
}

/// 一个 16³ 区段里的全部观察。
///
/// **同一张关系，两种表示**：`位置 → Option<方块事实>`，`Some` 走条目表、
/// `None` 走位图。分开存不是概念上有两样东西，是因为载荷差三个数量级——
/// 有东西那支一格约百字节，是空的那支一格一位，而后者的基数大得多。
#[derive(Clone, Debug)]
struct Section {
    /// 本区段内非空气格的最后所见。键是世界坐标，不折算，省去回算。
    facts: HashMap<[i32; 3], Entry>,
    /// 本区段内「确认为空」的位图，4096 位定长 512 字节。
    empty: Box<[u64; WORDS_PER_SECTION]>,
}

impl Default for Section {
    fn default() -> Self {
        Self {
            facts: HashMap::new(),
            empty: Box::new([0; WORDS_PER_SECTION]),
        }
    }
}

/// 方块记忆：同伴观察过的世界。
///
/// # 三态
///
/// | 条件 | 含义 |
/// |---|---|
/// | 条目表里有 | 看过，有东西 |
/// | 条目表没有、位图置位 | 看过，是空的 |
/// | 两边都没有 | 没看过 |
///
/// 「没看过」**不占存储**——它是两边都没有。所以三态只需要两种表示，这是设计
/// 本身的一部分，不是省出来的。读侧一律走 [`Self::state_at`]，不要自己拼。
///
/// # 按区段分片
///
/// 两种表示挂在同一个区段下，边界对齐。这么分买到三件事：**快照便宜**
/// （每片一个 `Arc`，克隆只复制指针，写时才分裂）、**盒扫可行**（只遍历与
/// 盒相交的区段，而不是全表筛）、**两支对称**（键相同、分片相同，查询侧
/// 的代价模型不会一边 O(1) 一边全扫）。
#[derive(Clone, Debug, Default)]
pub struct BlockMemory {
    sections: HashMap<[i32; 3], Arc<Section>>,
    /// 观察图的内容版本。只有三态或方块身份真正变化时才推进；重复看见同一事实
    /// 只刷新 `last_seen`，不会制造一次假的「地图变了」。寻路据此把重规划绑定到
    /// 新证据，而不是按定时器反复计算同一张图。
    revision: u64,
}

impl BlockMemory {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记一条观察。**这是唯一的写口。**
    ///
    /// `fact` 为 `None` 就是「亲眼看见这里是空的」——它不是删除，是一条载荷为
    /// 空的观察，所以要把位置上账而不只是销条目。旧实现把它写成「删占用记录」，
    /// 于是挖掉一块方块之后那一格会退回「没看过」，而我们明明看着它消失。
    /// 现在删除这个操作从 API 上消失了，只剩「观察到了什么」。
    pub fn observe(&mut self, at: [i32; 3], fact: Option<BlockFact>, tick: u64) {
        let changed = {
            let section = Arc::make_mut(self.sections.entry(section_of(at)).or_default());
            let bit = bit_index(at);
            match fact {
                Some(fact) => {
                    let changed = section
                        .facts
                        .get(&at)
                        .is_none_or(|entry| entry.fact != fact)
                        || section.empty[bit / 64] & (1_u64 << (bit % 64)) != 0;
                    section.empty[bit / 64] &= !(1_u64 << (bit % 64));
                    section.facts.insert(
                        at,
                        Entry {
                            fact,
                            last_seen: tick,
                        },
                    );
                    changed
                }
                None => {
                    let was_empty = section.empty[bit / 64] & (1_u64 << (bit % 64)) != 0;
                    let had_block = section.facts.remove(&at).is_some();
                    section.empty[bit / 64] |= 1_u64 << (bit % 64);
                    had_block || !was_empty
                }
            }
        };
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// 观察图的内容版本。它不是世界 tick；只表示寻路可用的知识有没有变。
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// 这一格现在算什么。三态的唯一读口。
    pub fn state_at(&self, at: [i32; 3]) -> Known<'_> {
        let Some(section) = self.sections.get(&section_of(at)) else {
            return Known::Unseen;
        };
        if let Some(entry) = section.facts.get(&at) {
            return Known::Block(&entry.fact);
        }
        let bit = bit_index(at);
        if section.empty[bit / 64] & (1_u64 << (bit % 64)) != 0 {
            Known::Empty
        } else {
            Known::Unseen
        }
    }

    pub fn get(&self, at: [i32; 3]) -> Option<&BlockFact> {
        match self.state_at(at) {
            Known::Block(fact) => Some(fact),
            _ => None,
        }
    }

    /// 最后一次看到这一格是第几刻。
    ///
    /// 这具身体没有「同时」：它对世界的知识是不同时刻观测的编织物，
    /// 任何「当前场景」都是查询重构的。
    pub fn last_seen(&self, at: [i32; 3]) -> Option<u64> {
        self.sections
            .get(&section_of(at))
            .and_then(|section| section.facts.get(&at))
            .map(|entry| entry.last_seen)
    }

    /// 遍历记住的每一格方块。
    ///
    /// 给**查询**用：模型问「附近有什么树」时，机器在这本记忆里找，而不是去翻
    /// 实时世界——只答得出观察过的东西，与合法信息边界天然一致。
    pub fn iter(&self) -> impl Iterator<Item = ([i32; 3], &BlockFact)> {
        self.sections
            .values()
            .flat_map(|section| section.facts.iter().map(|(at, entry)| (*at, &entry.fact)))
    }

    /// 同 [`Self::iter`]，另带最后所见的刻。
    pub fn iter_seen(&self) -> impl Iterator<Item = ([i32; 3], &BlockFact, u64)> {
        self.sections.values().flat_map(|section| {
            section
                .facts
                .iter()
                .map(|(at, entry)| (*at, &entry.fact, entry.last_seen))
        })
    }

    /// 记住的方块格数。**不含「确认为空」那一支**——那一支的基数不是一个量级，
    /// 混在一起报数会让「记住了多少」失去意义。
    pub fn len(&self) -> usize {
        self.sections
            .values()
            .map(|section| section.facts.len())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 确认为空的格数。诊断与覆盖率报告用，按需数位。
    pub fn known_empty_len(&self) -> usize {
        self.sections
            .values()
            .map(|section| {
                section
                    .empty
                    .iter()
                    .map(|word| word.count_ones() as usize)
                    .sum::<usize>()
            })
            .sum()
    }

    /// 一格都没观察过（两支都空）。与 [`Self::is_empty`] 不同：只记下过空气
    /// 的时候「方块数为零」成立，但「什么都没看过」不成立。
    pub fn nothing_observed(&self) -> bool {
        self.sections.is_empty()
    }

    /// 已分配的区段数。位图那一支乘 512 字节就是它的占用。
    pub fn section_count(&self) -> usize {
        self.sections.len()
    }
}

/// 观察源接入：眼睛把一次投影吸收进记忆，单步 upsert。
/// 两条吸收路径写的都是同一个 [`BlockMemory::observe`]。
impl BlockMemory {
    /// 吸收全量/扫描可见集：逐格 upsert。
    /// 只上账正面观察；本次没列出的格不动（缺席不当空气）。
    pub fn absorb_visible(&mut self, visible: &[ViewportBlock], at_tick: u64) {
        for block in visible {
            // 防御：空气不该出现在可见集里。真出现了也不当「确认为空」记——
            // 那一支的判据是射线穿过，不是这里。
            if is_air_name(&block.name) {
                continue;
            }
            // 身份没变也要刷新 `last_seen`：又看了一眼，这条事实就没那么旧了。
            self.observe(block.position, Some(BlockFact::of(block)), at_tick);
        }
    }

    /// 吸收一次投影的自由空间暂存：位图里的每一格都是一条「看过，是空的」。
    ///
    /// 暂存与记忆分开，是为了**不在投影全程持记忆的锁**——投影约十毫秒，而
    /// 寻路器每 tick 要读这本记忆上千次。
    pub fn absorb_empty(&mut self, space: &ObservedSpace, at_tick: u64) {
        for at in space.iter() {
            self.observe(at, None, at_tick);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(at: [i32; 3], name: &str) -> ViewportBlock {
        ViewportBlock {
            name: name.to_owned(),
            state_id: 1,
            properties: BTreeMap::new(),
            position: at,
        }
    }

    fn fact(name: &str) -> BlockFact {
        BlockFact {
            name: name.to_owned(),
            state_id: 1,
            properties: BTreeMap::new(),
        }
    }

    /// 「亲眼见空」不是删除：既销条目、又把那一格上账成「确认为空」。
    ///
    /// 旧实现只删条目，于是挖掉一块方块之后那一格退回「没看过」——我们明明
    /// 看着它消失。这条守着统一写口的核心不变量。
    #[test]
    fn seeing_air_records_an_observation_rather_than_deleting_one() {
        let mut memory = BlockMemory::new();
        memory.observe([3, 64, 7], Some(fact("stone")), 0);
        assert!(matches!(memory.state_at([3, 64, 7]), Known::Block(_)));

        memory.observe([3, 64, 7], None, 1);
        assert_eq!(memory.state_at([3, 64, 7]), Known::Empty);
        assert_eq!(memory.len(), 0, "条目要销掉");
        assert_eq!(memory.known_empty_len(), 1, "而且要上账成确认为空");
    }

    /// 反向也要收干净：空过的格子后来有了东西，位要清掉，否则两支同时成立。
    #[test]
    fn a_block_appearing_in_a_known_empty_cell_clears_the_bit() {
        let mut memory = BlockMemory::new();
        memory.observe([3, 64, 7], None, 0);
        memory.observe([3, 64, 7], Some(fact("dirt")), 1);

        assert!(matches!(memory.state_at([3, 64, 7]), Known::Block(_)));
        assert_eq!(memory.known_empty_len(), 0, "位没清 = 三态自相矛盾");
    }

    /// 分片的用处之一：快照是克隆指针，而且与原本互不影响。
    #[test]
    fn a_snapshot_is_cheap_and_does_not_follow_later_writes() {
        let mut memory = BlockMemory::new();
        memory.observe([0, 64, 0], Some(fact("stone")), 0);
        let snapshot = memory.clone();

        memory.observe([0, 64, 0], None, 1);
        memory.observe([0, 64, 1], Some(fact("dirt")), 1);

        assert!(matches!(snapshot.state_at([0, 64, 0]), Known::Block(_)));
        assert_eq!(snapshot.state_at([0, 64, 1]), Known::Unseen);
        assert_eq!(memory.state_at([0, 64, 0]), Known::Empty);
    }

    /// 寻路重规划只能由新知识触发：重看同一格只刷新时间，不能伪造地图变化。
    #[test]
    fn revision_only_advances_when_the_observed_state_changes() {
        let mut memory = BlockMemory::new();
        assert_eq!(memory.revision(), 0);

        memory.observe([0, 64, 0], Some(fact("stone")), 10);
        assert_eq!(memory.revision(), 1);
        memory.observe([0, 64, 0], Some(fact("stone")), 20);
        assert_eq!(memory.revision(), 1, "重复观察不是新地图");

        memory.observe([0, 64, 0], Some(fact("dirt")), 30);
        assert_eq!(memory.revision(), 2, "方块身份变化要推进版本");
        memory.observe([0, 64, 0], None, 40);
        assert_eq!(memory.revision(), 3, "看见空气要推进版本");
        memory.observe([0, 64, 0], None, 50);
        assert_eq!(memory.revision(), 3, "重复看见空气也不是新地图");

        let snapshot = memory.clone();
        memory.observe([1, 64, 0], None, 60);
        assert_eq!(snapshot.revision(), 3, "快照版本必须随快照冻结");
        assert_eq!(memory.revision(), 4);
    }

    /// 「记住的方块数为零」不等于「什么都没看过」——只见过空气也是看过。
    #[test]
    fn recording_only_air_is_still_having_observed_something() {
        let mut memory = BlockMemory::new();
        memory.observe([0, 64, 0], None, 0);
        assert_eq!(memory.len(), 0);
        assert!(memory.is_empty());
        assert!(!memory.nothing_observed());
    }

    /// 吸收：可见集 upsert；眼睛亲眼见空的格销账；本次没看到的不动。
    #[test]
    fn absorption_upserts_seen_and_erases_witnessed_empties() {
        let mut memory = BlockMemory::new();
        memory.absorb_visible(
            &[
                block([0, 64, 0], "stone"),
                block([1, 64, 0], "furnace"),
                block([9, 64, 9], "air"),
            ],
            0,
        );
        assert_eq!(memory.get([0, 64, 0]), Some(&fact("stone")));
        assert_eq!(memory.get([9, 64, 9]), None, "空气不入账");

        // 射线穿过了 [0,64,0]：那格现在亲眼可证为空。
        let mut space = ObservedSpace::new();
        space.mark([0, 64, 0]);
        memory.absorb_empty(&space, 1);
        assert_eq!(memory.get([0, 64, 0]), None, "亲眼见空销账");
        assert_eq!(
            memory.state_at([0, 64, 0]),
            Known::Empty,
            "销账留下的是确认为空，不是没看过"
        );
        assert_eq!(
            memory.get([1, 64, 0]),
            Some(&fact("furnace")),
            "本次没看到的不动"
        );
        assert_eq!(memory.len(), 1);
    }
}

#[cfg(test)]
mod staleness_tests {
    use super::*;

    fn block(at: [i32; 3], name: &str) -> ViewportBlock {
        ViewportBlock {
            name: name.to_owned(),
            state_id: 1,
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

        let before = memory.revision();

        memory.absorb_visible(&[block([0, 64, 0], "stone")], 200);
        assert_eq!(memory.revision(), before, "同一块石头再看一眼不是变化");
        assert_eq!(memory.last_seen([0, 64, 0]), Some(200), "时间要刷新");
    }

    /// 身份真的变了：既报变化，也刷新时间。
    #[test]
    fn a_real_change_updates_both_identity_and_time() {
        let mut memory = BlockMemory::new();
        memory.absorb_visible(&[block([0, 64, 0], "furnace")], 100);
        let before = memory.revision();
        memory.absorb_visible(&[block([0, 64, 0], "stone")], 300);
        assert!(memory.revision() > before, "身份变了就是一次变化");
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
