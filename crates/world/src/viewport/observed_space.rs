//! 观察过的空间：一格「我看过这里」的位，与 [`BlockMemory`] 合起来才是三态世界。
//!
//! # 为什么缺这一位
//!
//! [`BlockMemory`] 只记非空气（见 [`super::incremental`] 模块头），于是
//! `get() == None` 同时意味着**「从没看过」**和**「看过，是空的」**。这是把
//! 三值世界压成二值表，有损：空气的全部内容恰恰是「确认没有」，而这一位被丢了。
//!
//! 补上之后判据是三条：
//!
//! | 条件 | 含义 |
//! |---|---|
//! | 记忆有条目 | 有东西（含水、草这类**非空气**方块） |
//! | 记忆没有、本表置位 | **确认为空** |
//! | 本表未置位 | 没看过 |
//!
//! 消费者不止一个，也不是为了画图：寻路合法域要「同伴曾见的可通行空间」
//! （[`super::incremental`] 模块头早已为此留口），查询要能回答「北边有没有
//! 通路」的三种答案，勘探要知道边界在哪。
//!
//! # 为什么是位图不是行
//!
//! 一次视野的自由体积在数万格量级。按行存，一格是名字加属性的百字节量级；
//! 位图一格一位，一个 16³ 区段 4096 位 = **512 字节定长**，与看过多少次无关。
//! 差三个数量级，而且不随观察次数增长。
//!
//! 给模型的 SQL 面不受影响：那张表是每次查询现建的，空气用标量函数回答，
//! 不产生行——顺带也就不会撞上单次输出的行数上限。
//!
//! # 边界：这一位只装射线确认的空
//!
//! 只有**射线途经**的空格才置位。因此本表是真实观察体积的**子集**：看向天空
//! 那种射线尽头没有可见面的方向不会被标记。宁可少标不多标，与视口一贯的方向
//! 一致——少标是「还没看过」，多标是让同伴知道它没看过的事。
//!
//! 「身体走过的地方当时必然通得过」是另一条**更弱**的证据：水、草、花都通得过，
//! 但它们不是空气。那条证据不属于本表；要用得单独记，不能混进这一位，
//! 否则「这里能不能放方块」会拿到一个错的答案。

use std::collections::HashMap;

use super::SECTION_SIZE;

/// 一个区段 16×16×16 = 4096 格，一格一位 = 64 个 u64。
const WORDS_PER_SECTION: usize = 4096 / 64;

/// 区段内的位下标，与原版体素次序一致：`y<<8 | z<<4 | x`。
fn bit_index(at: [i32; 3]) -> usize {
    let x = (at[0] & 15) as usize;
    let y = (at[1] & 15) as usize;
    let z = (at[2] & 15) as usize;
    (y << 8) | (z << 4) | x
}

/// 所属区段坐标。除法要向下取整，负坐标不能用 `/`。
fn section_of(at: [i32; 3]) -> [i32; 3] {
    [
        at[0].div_euclid(SECTION_SIZE),
        at[1].div_euclid(SECTION_SIZE),
        at[2].div_euclid(SECTION_SIZE),
    ]
}

/// 已观察空间：按区段挂位图，只在真的看到过的区段上分配。
#[derive(Clone, Debug, Default)]
pub struct ObservedSpace {
    sections: HashMap<[i32; 3], Box<[u64; WORDS_PER_SECTION]>>,
}

impl ObservedSpace {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记下「这一格我看过」。重复标记是幂等的。
    pub fn mark(&mut self, at: [i32; 3]) {
        let bit = bit_index(at);
        let words = self
            .sections
            .entry(section_of(at))
            .or_insert_with(|| Box::new([0_u64; WORDS_PER_SECTION]));
        words[bit / 64] |= 1_u64 << (bit % 64);
    }

    /// 看过这一格吗。没看过的区段连位图都没分配，直接答否。
    pub fn contains(&self, at: [i32; 3]) -> bool {
        self.sections.get(&section_of(at)).is_some_and(|words| {
            let bit = bit_index(at);
            words[bit / 64] & (1_u64 << (bit % 64)) != 0
        })
    }

    /// 观察过的格数。用于诊断与覆盖率报告，不在热路径上——按需数位。
    pub fn len(&self) -> usize {
        self.sections
            .values()
            .map(|words| {
                words
                    .iter()
                    .map(|word| word.count_ones() as usize)
                    .sum::<usize>()
            })
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 已分配的区段数。乘 512 字节就是本表的占用。
    pub fn section_count(&self) -> usize {
        self.sections.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marked_cell_is_contained_and_its_neighbours_are_not() {
        let mut space = ObservedSpace::new();
        space.mark([10, 64, -3]);
        assert!(space.contains([10, 64, -3]));
        for neighbour in [[11, 64, -3], [10, 65, -3], [10, 64, -2]] {
            assert!(!space.contains(neighbour), "{neighbour:?} 没标记过");
        }
    }

    /// 负坐标：区段用 `div_euclid`，格内用 `& 15`。两者必须配套，
    /// 否则 x=-1 会被算进区段 0 的第 15 位，与 x=15 撞车。
    #[test]
    fn negative_coordinates_do_not_alias_positive_ones() {
        let mut space = ObservedSpace::new();
        space.mark([-1, -1, -1]);
        assert!(space.contains([-1, -1, -1]));
        assert!(!space.contains([15, 15, 15]), "负坐标不得与正坐标同位");
        assert_eq!(space.section_count(), 1);
        assert_eq!(space.len(), 1);
    }

    #[test]
    fn marking_twice_is_idempotent() {
        let mut space = ObservedSpace::new();
        space.mark([0, 0, 0]);
        space.mark([0, 0, 0]);
        assert_eq!(space.len(), 1);
    }

    /// 一个区段装满 4096 格仍然只占一份位图：定长是这个表的全部理由。
    #[test]
    fn a_full_section_costs_one_bitmap() {
        let mut space = ObservedSpace::new();
        for x in 0..16 {
            for y in 0..16 {
                for z in 0..16 {
                    space.mark([x, y, z]);
                }
            }
        }
        assert_eq!(space.len(), 4096);
        assert_eq!(space.section_count(), 1);
        assert_eq!(WORDS_PER_SECTION * 8, 512, "一个区段 512 字节");
    }

    /// 未观测的地方不分配：空间的大小随**看过多少**长，不随**世界多大**长。
    #[test]
    fn untouched_regions_allocate_nothing() {
        let space = ObservedSpace::new();
        assert!(space.is_empty());
        assert_eq!(space.section_count(), 0);
        assert!(!space.contains([1_000_000, 64, -1_000_000]));
    }

    /// 跨区段边界：x=15 与 x=16 分属两个区段，都要答对。
    #[test]
    fn cells_across_a_section_boundary_land_in_different_sections() {
        let mut space = ObservedSpace::new();
        space.mark([15, 0, 0]);
        space.mark([16, 0, 0]);
        assert!(space.contains([15, 0, 0]));
        assert!(space.contains([16, 0, 0]));
        assert_eq!(space.section_count(), 2);
    }
}
