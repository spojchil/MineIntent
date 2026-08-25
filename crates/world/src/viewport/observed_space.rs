//! 一次投影的自由空间暂存：射线途经的空格先落在这里，再折进方块记忆。
//!
//! # 为什么是暂存而不是第二本记忆
//!
//! 三态（有东西／确认为空／没看过）的存储在 [`crate::BlockMemory`] 里，两种
//! 表示挂在同一个区段下。本类型只承担**一次投影的产出**：投影约十毫秒，而
//! 寻路器每 tick 要读那本记忆上千次——先写暂存、再短锁折进去，就不必在投影
//! 全程持有记忆的锁。
//!
//! # 为什么是位图不是行
//!
//! 一次视野的自由体积在数万格量级。按行存，一格是名字加属性的百字节量级；
//! 位图一格一位，一个 16³ 区段 4096 位 = **512 字节定长**，与看过多少次无关。
//! 而且射线之间大量重叠，位图顺带把重复标记去掉了。
//!
//! # 边界：这一位只装射线确认的空
//!
//! 只有**射线途经**的空格才置位。因此本表是真实观察体积的**子集**：看向天空
//! 那种射线尽头没有可见面的方向不会被标记。宁可少标不多标，与视口一贯的方向
//! 一致——少标是「还没看过」，多标是让同伴知道它没看过的事。
//!
//! 「身体走过的地方当时必然通得过」是另一条**更弱**的证据：水、草、花都通得过，
//! 但它们不是空气。那条证据不属于本表；真要用，最便宜的形式是同一张位图上加
//! 第二个位平面，而不是再开一本。
//!
//! # 与方块记忆的判据不对称
//!
//! **记录对称，采集不对称。**方块那一支的判据是露出面（埋在石头里的矿脉不进
//! 表），本表的判据是射线穿过（看天的方向、玻璃水树叶身后不进表）。两支都保守，
//! 但保守的方向和覆盖的集合都不同，别拿一支的缺失去推断另一支。

use std::collections::HashMap;

use super::SECTION_SIZE;

/// 一个区段 16×16×16 = 4096 格，一格一位 = 64 个 u64。
pub(super) const WORDS_PER_SECTION: usize = 4096 / 64;

/// 区段内的位下标，与原版体素次序一致：`y<<8 | z<<4 | x`。
pub(super) fn bit_index(at: [i32; 3]) -> usize {
    let x = (at[0] & 15) as usize;
    let y = (at[1] & 15) as usize;
    let z = (at[2] & 15) as usize;
    (y << 8) | (z << 4) | x
}

/// 所属区段坐标。除法要向下取整，负坐标不能用 `/`。
pub(super) fn section_of(at: [i32; 3]) -> [i32; 3] {
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

    /// 遍历标记过的每一格。
    pub fn iter(&self) -> impl Iterator<Item = [i32; 3]> + '_ {
        self.sections
            .iter()
            .flat_map(|(section, words)| set_bits(*section, words))
    }
}

/// 遍历一个区段位图里的置位，还原成世界坐标。
///
/// **按字走位，不按格走**：一个区段 64 个 `u64`，`word == 0` 一次比较跳过
/// 64 格，非零字用 `trailing_zeros` 逐位取。空旷区域近乎零成本——位图省的
/// 不只是内存，扫也快。
pub(super) fn set_bits(
    section: [i32; 3],
    words: &[u64; WORDS_PER_SECTION],
) -> impl Iterator<Item = [i32; 3]> + '_ {
    let base = [
        section[0] * SECTION_SIZE,
        section[1] * SECTION_SIZE,
        section[2] * SECTION_SIZE,
    ];
    words
        .iter()
        .enumerate()
        .filter(|(_, word)| **word != 0)
        .flat_map(move |(index, word)| {
            let mut rest = *word;
            std::iter::from_fn(move || {
                if rest == 0 {
                    return None;
                }
                let offset = rest.trailing_zeros() as usize;
                rest &= rest - 1;
                Some(position_of(base, index * 64 + offset))
            })
        })
}

/// 位下标还原成世界坐标。与 [`bit_index`] 互逆。
fn position_of(base: [i32; 3], bit: usize) -> [i32; 3] {
    [
        base[0] + (bit & 15) as i32,
        base[1] + (bit >> 8) as i32,
        base[2] + ((bit >> 4) & 15) as i32,
    ]
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
