//! 格位地址：**我们自己的一套**，不用原版协议号。
//!
//! # 为什么另起一套
//!
//! 协议号把同一个物理位置在不同屏下换成不同数字：快捷栏第 4 格在物品栏屏是 40、
//! 在工作台屏是 41，而 `select_slot` 又用 3。三套编号共用「快捷栏格」这一个说法，
//! 模型实测抱怨过「回执的格号和界面的格号是两套，我经常搞混」（2026-08-20 长跑，
//! 模型自述），而减错一位不会报错——直接拿错东西。
//!
//! 协议号还会随版本变。把它挡在机器层里，可见面就不会因为上游改布局而塌。
//!
//! # 形状：区名 + 区内序号
//!
//! ```text
//! hotbar 0-8     快捷栏（和 select_slot 的号一致，三套合一）
//! pack 0-26      主背包
//! armor head|chest|legs|feet
//! offhand
//! craft 0-3      随身合成（工作台屏是 craft 0-8）
//! result         成品格
//! <容器名> 0-N   容器自有区，按容器种类命名（chest / barrel / hopper …）
//! smelt / fuel   熔炉族的专名
//! ```
//!
//! 同一个物理位置**永远同一个名字**，开不开容器、开哪种容器都不变。
//!
//! # 布局是推出来的，不是查表
//!
//! 容器菜单恒定是「自有区 + 主背包 27 + 快捷栏 9」，所以
//! **自有区长度 = `hotbar_start - 27`**：玩家屏 36-27=9，工作台屏 37-27=10。
//! 加一种容器不需要动这里。

/// 当前屏的格位空间。由机器层从活动菜单几何 + 容器种类构造。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotSpace {
    /// 快捷栏第一格的协议号。
    hotbar_start: u16,
    /// 最大协议号（含）。
    max_slot: u16,
    /// 副手的协议号；容器屏一般没有。
    offhand: Option<u16>,
    /// 自有区的命名方式。
    own: OwnArea,
}

/// 自有区（协议号 0..hotbar_start-27）怎么叫。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnArea {
    /// 玩家屏：0 成品、1-4 随身合成、5-8 盔甲。
    Player,
    /// 工作台：0 成品、1-N 摆料。
    Crafting,
    /// 熔炉族：0 原料、1 燃料、2 成品。
    Furnace,
    /// 其余容器：整片按容器名编号，如 `chest 0-26`。
    Named(String),
}

/// 盔甲四件，按协议号顺序（5-8）。
const ARMOR: [&str; 4] = ["head", "chest", "legs", "feet"];

impl SlotSpace {
    pub fn new(hotbar_start: u16, max_slot: u16, offhand: Option<u16>, own: OwnArea) -> Self {
        Self {
            hotbar_start,
            max_slot,
            offhand,
            own,
        }
    }

    /// 玩家物品栏屏的空间。机器级探针把协议号转成地址时用。
    pub fn player() -> Self {
        Self::new(36, 45, Some(45), OwnArea::Player)
    }

    /// 自有区有多少格。布局恒定「自有区 + 主背包 27 + 快捷栏 9」。
    fn own_len(&self) -> u16 {
        self.hotbar_start.saturating_sub(27)
    }

    fn pack_start(&self) -> u16 {
        self.own_len()
    }

    /// 协议号 → 地址。给渲染用。
    pub fn describe(&self, slot: u16) -> String {
        if Some(slot) == self.offhand {
            return "offhand".to_owned();
        }
        if slot >= self.hotbar_start && slot < self.hotbar_start + 9 {
            return format!("hotbar {}", slot - self.hotbar_start);
        }
        if slot >= self.pack_start() && slot < self.hotbar_start {
            return format!("pack {}", slot - self.pack_start());
        }
        match &self.own {
            OwnArea::Player => match slot {
                0 => "result".to_owned(),
                1..=4 => format!("craft {}", slot - 1),
                5..=8 => format!("armor {}", ARMOR[(slot - 5) as usize]),
                _ => format!("slot {slot}"),
            },
            OwnArea::Crafting => match slot {
                0 => "result".to_owned(),
                _ => format!("craft {}", slot - 1),
            },
            OwnArea::Furnace => match slot {
                0 => "smelt".to_owned(),
                1 => "fuel".to_owned(),
                2 => "result".to_owned(),
                _ => format!("slot {slot}"),
            },
            OwnArea::Named(name) => format!("{name} {slot}"),
        }
    }

    /// 地址 → 协议号。拒绝时**给出这一屏认的写法**，不让模型猜。
    pub fn resolve(&self, text: &str) -> Result<u16, String> {
        let trimmed = text.trim();
        let (area, rest) = match trimmed.split_once(char::is_whitespace) {
            Some((area, rest)) => (area, rest.trim()),
            None => (trimmed, ""),
        };
        let index = || -> Result<u16, String> {
            rest.parse::<u16>()
                .map_err(|_| format!("「{trimmed}」缺少区内序号；{}", self.legend()))
        };
        match area {
            "hotbar" => self.bounded(self.hotbar_start, 9, index()?, "hotbar"),
            "pack" => self.bounded(self.pack_start(), 27, index()?, "pack"),
            "offhand" => self.offhand.ok_or_else(|| "这一屏够不到副手".to_owned()),
            "armor" => {
                let piece = ARMOR
                    .iter()
                    .position(|name| *name == rest)
                    .ok_or_else(|| format!("armor 只有 {}", ARMOR.join("/")))?;
                match self.own {
                    OwnArea::Player => Ok(5 + piece as u16),
                    _ => Err("这一屏够不到盔甲格".to_owned()),
                }
            }
            "result" => match self.own {
                OwnArea::Player | OwnArea::Crafting => Ok(0),
                OwnArea::Furnace => Ok(2),
                OwnArea::Named(_) => Err(format!("这一屏没有 result；{}", self.legend())),
            },
            "craft" => match self.own {
                OwnArea::Player => self.bounded(1, 4, index()?, "craft"),
                OwnArea::Crafting => self.bounded(1, self.own_len() - 1, index()?, "craft"),
                _ => Err(format!("这一屏没有 craft；{}", self.legend())),
            },
            "smelt" => match self.own {
                OwnArea::Furnace => Ok(0),
                _ => Err(format!("这一屏没有 smelt；{}", self.legend())),
            },
            "fuel" => match self.own {
                OwnArea::Furnace => Ok(1),
                _ => Err(format!("这一屏没有 fuel；{}", self.legend())),
            },
            other => match &self.own {
                OwnArea::Named(name) if name == other => {
                    self.bounded(0, self.own_len(), index()?, name)
                }
                _ => Err(format!("不认识的格区「{other}」；{}", self.legend())),
            },
        }
    }

    fn bounded(&self, start: u16, len: u16, index: u16, area: &str) -> Result<u16, String> {
        if index >= len {
            return Err(format!("{area} 只有 0-{}", len.saturating_sub(1)));
        }
        let slot = start + index;
        if slot > self.max_slot {
            return Err(format!("{area} {index} 超出这一屏范围"));
        }
        Ok(slot)
    }

    /// 一段协议号区间在这一屏叫什么（清单表头用）。首尾同区就写成 `a-b`。
    pub fn legend_of(&self, first: u16, last: u16) -> String {
        let head = self.describe(first);
        let tail = self.describe(last);
        match (head.rsplit_once(' '), tail.rsplit_once(' ')) {
            (Some((area, a)), Some((same, b))) if area == same => format!("{area} {a}-{b}"),
            _ if head == tail => head,
            _ => format!("{head}…{tail}"),
        }
    }

    /// 这一屏认哪些写法。拒绝时附上，省得模型试错。
    pub fn legend(&self) -> String {
        let mut areas = match &self.own {
            OwnArea::Player => vec![
                "result".to_owned(),
                "craft 0-3".to_owned(),
                format!("armor {}", ARMOR.join("/")),
            ],
            OwnArea::Crafting => vec![
                "result".to_owned(),
                format!("craft 0-{}", self.own_len().saturating_sub(2)),
            ],
            OwnArea::Furnace => {
                vec!["smelt".to_owned(), "fuel".to_owned(), "result".to_owned()]
            }
            OwnArea::Named(name) => {
                vec![format!("{name} 0-{}", self.own_len().saturating_sub(1))]
            }
        };
        areas.push("pack 0-26".to_owned());
        areas.push("hotbar 0-8".to_owned());
        if self.offhand.is_some() {
            areas.push("offhand".to_owned());
        }
        format!("这一屏认：{}", areas.join("、"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player() -> SlotSpace {
        SlotSpace::new(36, 45, Some(45), OwnArea::Player)
    }

    fn crafting() -> SlotSpace {
        SlotSpace::new(37, 45, None, OwnArea::Crafting)
    }

    fn chest() -> SlotSpace {
        SlotSpace::new(54, 62, None, OwnArea::Named("chest".to_owned()))
    }

    /// 全篇的要害：**同一个物理位置，不同屏下同一个名字**。
    #[test]
    fn the_same_place_keeps_its_name_across_screens() {
        assert_eq!(player().describe(40), "hotbar 4");
        assert_eq!(crafting().describe(41), "hotbar 4");
        assert_eq!(chest().describe(58), "hotbar 4");

        assert_eq!(player().resolve("hotbar 4"), Ok(40));
        assert_eq!(crafting().resolve("hotbar 4"), Ok(41));
        assert_eq!(chest().resolve("hotbar 4"), Ok(58));
    }

    /// 主背包同理：协议号差 18，名字不差。
    #[test]
    fn the_pack_is_stable_too() {
        assert_eq!(player().describe(9), "pack 0");
        assert_eq!(crafting().describe(10), "pack 0");
        assert_eq!(chest().describe(27), "pack 0");
    }

    #[test]
    fn describe_and_resolve_are_inverses() {
        for space in [player(), crafting(), chest()] {
            for slot in 0..=space.max_slot {
                let name = space.describe(slot);
                assert_eq!(space.resolve(&name), Ok(slot), "{name}");
            }
        }
    }

    #[test]
    fn own_areas_are_named_per_container() {
        assert_eq!(player().describe(0), "result");
        assert_eq!(player().describe(2), "craft 1");
        assert_eq!(player().describe(6), "armor chest");
        assert_eq!(crafting().describe(5), "craft 4");
        assert_eq!(chest().describe(3), "chest 3");

        let furnace = SlotSpace::new(30, 38, None, OwnArea::Furnace);
        assert_eq!(furnace.describe(0), "smelt");
        assert_eq!(furnace.describe(1), "fuel");
        assert_eq!(furnace.describe(2), "result");
    }

    /// 越界与不认识的区都要**说清这一屏认什么**，不让模型试错。
    #[test]
    fn refusals_say_what_this_screen_accepts() {
        let reason = player().resolve("hotbar 9").unwrap_err();
        assert!(reason.contains("0-8"), "{reason}");

        let reason = chest().resolve("craft 0").unwrap_err();
        assert!(reason.contains("chest 0-26"), "{reason}");

        let reason = crafting().resolve("offhand").unwrap_err();
        assert!(reason.contains("副手"), "{reason}");
    }
}
