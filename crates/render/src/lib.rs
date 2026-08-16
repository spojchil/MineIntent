//! 丙·渲染：tick 快照 → 模型可读文字，全部纯函数。
//!
//! 呈现选择政策归此处（给多少、怎么说）；事实归快照（world），本层不添不删事实，
//! 只挑选与措辞。处境厚度按裁定为"中版"：环境 + 体征 + 位置朝向 + 周围实体概览
//! + 聊天未读数；方块级细节走感知的 scan，不进每轮开场。
//!
//! 同类实体聚合呈现（数量 + 最近距离方位）——压缩方向是同质聚合，不是截断。

use world::{ConnectionPhase, EntitySnapshot, TickSnapshot, Window};

/// 每轮开场处境的总装。`chat_read` 是聊天已读水位 (epoch, tick)，
/// 未读数 = 聊天窗里晚于水位的条数（重连换纪元后整窗算新）。
pub fn render_situation(snap: &TickSnapshot, chat_read: (u64, u64)) -> String {
    // 非就绪状态下世界数据是旧的，处境只说连接事实，不拿旧世界冒充现在。
    match &snap.phase {
        ConnectionPhase::Ready => {}
        ConnectionPhase::Connecting => return "正在连接服务器。".to_owned(),
        ConnectionPhase::Disconnected { reason } => {
            return format!("已断线：{reason}");
        }
        ConnectionPhase::Stopped { reason } => {
            return format!("连接已停止：{reason}");
        }
    }

    let mut lines = vec![
        render_environment(snap),
        render_position(snap),
        render_vitals(snap),
        render_nearby(snap),
    ];
    let unread = unread_chat_count(snap, chat_read);
    if unread > 0 {
        lines.push(format!("聊天有 {unread} 条新消息。"));
    }
    lines.retain(|line| !line.is_empty());
    lines.join("\n")
}

/// 环境一行：维度、时段，有雨雪雷才提天气。
pub fn render_environment(snap: &TickSnapshot) -> String {
    let meta = &snap.world_meta;
    let mut line = format!(
        "{}，{}",
        dimension_word(&meta.dimension),
        day_period_word(meta.day_time)
    );
    if meta.thunder_level >= 0.5 {
        line.push_str("，雷雨");
    } else if meta.rain_level >= 0.5 {
        line.push_str("，下着雨");
    }
    line.push('。');
    line
}

/// 位置一行：整数坐标 + 面朝（归一化后的八向）。
pub fn render_position(snap: &TickSnapshot) -> String {
    let this = &snap.self_state;
    let mut line = format!(
        "位置 ({}, {}, {})，面朝{}",
        this.position.x.floor() as i64,
        this.position.y.floor() as i64,
        this.position.z.floor() as i64,
        compass_word(this.yaw)
    );
    if !this.on_ground {
        line.push_str("，悬空");
    }
    line.push('。');
    line
}

/// 体征一行：生命、饥饿；氧气与状态效果只在有话可说时出现。
pub fn render_vitals(snap: &TickSnapshot) -> String {
    let this = &snap.self_state;
    if !this.alive {
        return "你已经死亡。".to_owned();
    }
    let mut line = format!(
        "生命 {}/20，饥饿 {}/20",
        trim_number(this.health),
        trim_number(this.food)
    );
    if let Some(oxygen) = this.oxygen {
        line.push_str(&format!("，氧气 {}", trim_number(oxygen)));
    }
    if !this.effects.is_empty() {
        let effects: Vec<String> = this
            .effects
            .iter()
            .map(|effect| {
                let mut text = effect.name.clone();
                if effect.amplifier > 0 {
                    text.push_str(&format!(" {}", effect.amplifier + 1));
                }
                if let Some(ticks) = effect.duration_ticks {
                    text.push_str(&format!("（{} 秒）", ticks / 20));
                }
                text
            })
            .collect();
        line.push_str(&format!("；效果：{}", effects.join("、")));
    }
    line.push('。');
    line
}

/// 周围实体概览：玩家逐个列出，其余同类聚合（数量 + 最近距离方位）。
/// 快照不过滤自身（直译义务在模块一），跳过自己是本层的呈现选择。
pub fn render_nearby(snap: &TickSnapshot) -> String {
    let this = &snap.self_state;
    let mut described: Vec<(f64, String)> = Vec::new();

    let mut groups: Vec<(&str, Vec<&EntitySnapshot>)> = Vec::new();
    for entity in &snap.entities {
        if entity.entity_key == this.entity_key {
            continue;
        }
        if let Some(username) = &entity.username {
            let distance = distance_between(this, entity);
            described.push((
                distance,
                format!(
                    "玩家 {username}（{} 格·{}）",
                    distance.round() as i64,
                    bearing_word(this, entity)
                ),
            ));
            continue;
        }
        let type_word = entity
            .entity_type
            .strip_prefix("minecraft:")
            .unwrap_or(&entity.entity_type);
        match groups.iter_mut().find(|(word, _)| *word == type_word) {
            Some((_, members)) => members.push(entity),
            None => groups.push((type_word, vec![entity])),
        }
    }

    for (type_word, members) in groups {
        let nearest = members
            .iter()
            .min_by(|a, b| distance_between(this, a).total_cmp(&distance_between(this, b)))
            .expect("组内至少一个成员");
        let distance = distance_between(this, nearest);
        let place = format!(
            "{} 格·{}",
            distance.round() as i64,
            bearing_word(this, nearest)
        );
        let text = if members.len() > 1 {
            format!("{type_word} ×{}（最近 {place}）", members.len())
        } else {
            format!("{type_word}（{place}）")
        };
        described.push((distance, text));
    }

    if described.is_empty() {
        return String::new();
    }
    described.sort_by(|a, b| a.0.total_cmp(&b.0));
    let texts: Vec<String> = described.into_iter().map(|(_, text)| text).collect();
    format!("附近：{}。", texts.join("；"))
}

/// 背包全文：手持 + 逐格清单。不进每轮处境，供工具面按需取用。
pub fn render_inventory(snap: &TickSnapshot) -> String {
    let inventory = &snap.self_state.inventory;
    if inventory.slots.is_empty() {
        return "背包是空的。".to_owned();
    }
    // 快照格号是菜单协议号：快捷栏在 36-44，选中格 0-8 要先换算。
    let held_menu_slot = 36 + u32::from(inventory.selected_hotbar_slot);
    let held = inventory
        .slots
        .iter()
        .find(|slot| slot.slot == held_menu_slot)
        .map(|slot| slot.item_name.as_str())
        .unwrap_or("空手");
    let items: Vec<String> = inventory
        .slots
        .iter()
        .map(|slot| format!("{} ×{}", slot.item_name, slot.count))
        .collect();
    format!("手持：{held}。背包：{}。", items.join("、"))
}

/// 物品栏屏全景：46 格按分区列出（协议号即格号），空格不逐一点名。
/// 用法说明归 screens（操作信息），这里只呈现内容。
pub fn render_player_menu(snap: &TickSnapshot) -> String {
    let inventory = &snap.self_state.inventory;
    let item_at = |slot: u32| -> Option<String> {
        inventory
            .slots
            .iter()
            .find(|entry| entry.slot == slot)
            .map(|entry| format!("{} ×{}", entry.item_name, entry.count))
    };
    let section = |name: &str, range: std::ops::RangeInclusive<u32>| -> String {
        let filled: Vec<String> = range
            .clone()
            .filter_map(|slot| item_at(slot).map(|text| format!("{slot}={text}")))
            .collect();
        if filled.is_empty() {
            format!("{name}（{}-{}）：空", range.start(), range.end())
        } else {
            format!(
                "{name}（{}-{}）：{}",
                range.start(),
                range.end(),
                filled.join("、")
            )
        }
    };
    let held_menu_slot = 36 + u32::from(inventory.selected_hotbar_slot);
    let mut lines = vec![
        match item_at(0) {
            Some(item) => format!("合成结果（0）：{item}"),
            None => "合成结果（0）：空".to_owned(),
        },
        section("随身合成", 1..=4),
        section("盔甲·头/胸/腿/脚", 5..=8),
        section("主背包", 9..=35),
        section("快捷栏", 36..=44),
        match item_at(45) {
            Some(item) => format!("副手（45）：{item}"),
            None => "副手（45）：空".to_owned(),
        },
        format!(
            "手持的是快捷栏格 {held_menu_slot}{}。",
            item_at(held_menu_slot)
                .map(|item| format!("（{item}）"))
                .unwrap_or_else(|| "（空手）".to_owned())
        ),
    ];
    lines.retain(|line| !line.is_empty());
    lines.join("\n")
}

/// 物品栏格位变化的通知措辞。合成结果格单独点名（维护者裁定：它的出现
/// 也算预期之外的变化）。容器 0 是物品栏屏；其他容器措辞保持中性
/// （0 号在工作台是成品格、在熔炉是原料格——语义已随开屏清单给过，
/// 这里不替格号扣帽子）。
pub fn render_inventory_change(entry: &world::InventoryChangeEntry) -> String {
    let place = match (entry.container_id, entry.slot) {
        (0, 0) => "合成结果格（0）".to_owned(),
        (0, slot) => format!("物品栏格 {slot} "),
        (_, slot) => format!("容器格 {slot} "),
    };
    match &entry.item_name {
        Some(name) => format!("{place}出现了 {name} ×{}。", entry.count),
        None => format!("{place}变空了。"),
    }
}

/// 每种容器**自有区**的格数（玩家区 36 格总在其后）。
/// 直译 azalea `declare_menus`（azalea-inventory/src/lib.rs 声明表）；
/// 加一种容器的清单支持 = 在这里加一行。
fn container_area_len(kind: &str) -> Option<u32> {
    Some(match kind {
        "generic_9x1" => 9,
        "generic_9x2" => 18,
        "generic_9x3" | "shulker_box" => 27,
        "generic_9x4" => 36,
        "generic_9x5" => 45,
        "generic_9x6" => 54,
        "generic_3x3" | "crafter_3x3" => 9,
        "anvil" | "blast_furnace" | "furnace" | "smoker" | "grindstone" | "merchant"
        | "cartography_table" => 3,
        "beacon" | "lectern" => 1,
        "brewing_stand" | "hopper" => 5,
        "crafting" => 10,
        "enchantment" | "stonecutter" => 2,
        "loom" => 4,
        "smithing" => 4,
        _ => return None,
    })
}

/// 容器屏的格位清单（格号即协议号，属于该容器的格空间）。
///
/// 语义段表是数据：工作台（成品/摆料）与熔炉族（原料/燃料/成品）有
/// 专属标注，其余已知容器按「容器格 + 主背包 + 快捷栏」通用三段
/// （尺寸查 [`container_area_len`]），未知种类退化为逐格罗列——
/// 直译原则，不装懂。
pub fn render_container_menu(snap: &TickSnapshot, kind: &str) -> String {
    let inventory = &snap.self_state.inventory;
    let item_at = |slot: u32| -> Option<String> {
        inventory
            .slots
            .iter()
            .find(|entry| entry.slot == slot)
            .map(|entry| format!("{} ×{}", entry.item_name, entry.count))
    };
    let section = |name: &str, range: std::ops::RangeInclusive<u32>| -> String {
        let filled: Vec<String> = range
            .clone()
            .filter_map(|slot| item_at(slot).map(|text| format!("{slot}={text}")))
            .collect();
        if filled.is_empty() {
            format!("{name}（{}-{}）：空", range.start(), range.end())
        } else {
            format!(
                "{name}（{}-{}）：{}",
                range.start(),
                range.end(),
                filled.join("、")
            )
        }
    };
    let single = |name: &str, slot: u32| -> String {
        match item_at(slot) {
            Some(item) => format!("{name}（{slot}）：{item}"),
            None => format!("{name}（{slot}）：空"),
        }
    };
    if kind == "crafting" {
        let lines = [
            single("成品", 0),
            section("摆料 3×3", 1..=9),
            section("主背包", 10..=36),
            section("快捷栏", 37..=45),
        ];
        return lines.join("\n");
    }
    if matches!(kind, "furnace" | "blast_furnace" | "smoker") {
        let lines = [
            single("原料", 0),
            single("燃料", 1),
            single("成品", 2),
            section("主背包", 3..=29),
            section("快捷栏", 30..=38),
        ];
        return lines.join("\n");
    }
    if let Some(own) = container_area_len(kind) {
        let lines = [
            section("容器格", 0..=own - 1),
            section("主背包", own..=own + 26),
            section("快捷栏", own + 27..=own + 35),
        ];
        return lines.join("\n");
    }
    // 未知种类：逐格罗列非空格位，不猜段界。
    let mut slots: Vec<String> = inventory
        .slots
        .iter()
        .map(|entry| format!("{}={} ×{}", entry.slot, entry.item_name, entry.count))
        .collect();
    slots.sort();
    if slots.is_empty() {
        "（全部格位为空）".to_owned()
    } else {
        format!("非空格位：{}", slots.join("、"))
    }
}

/// 视口全景的呈现：同名方块聚合（数量+最近位置），实体逐个列出。
/// 近距截断是临时政策（⑥），聚合是其非临时方向的第一步。
pub fn render_viewport(projection: &world::ViewportProjection) -> String {
    let mut lines = Vec::new();
    let pose = &projection.pose;
    lines.push(format!(
        "视角：位于 ({:.1}, {:.1}, {:.1})，面朝 {}°（俯仰 {}°）。",
        pose.position[0], pose.position[1], pose.position[2], pose.yaw_degrees, pose.pitch_degrees
    ));
    if let Some(block) = &projection.looked_at_block {
        lines.push(format!(
            "准星对着：{} ({}, {}, {})。",
            block.name, block.position[0], block.position[1], block.position[2]
        ));
    }
    if let Some(block) = &projection.standing_on_block {
        lines.push(format!("脚下踩着：{}。", block.name));
    }

    if projection.visible_entities.items.is_empty() {
        lines.push("视野里没有实体。".to_owned());
    } else {
        let entities: Vec<String> = projection
            .visible_entities
            .items
            .iter()
            .map(|entity| {
                let label = match &entity.player {
                    Some(player) => format!("玩家 {player}"),
                    None => entity.entity_type.clone(),
                };
                format!(
                    "{label}（{:.0}, {:.0}, {:.0}）",
                    entity.position[0], entity.position[1], entity.position[2]
                )
            })
            .collect();
        let mut line = format!("视野里的实体：{}", entities.join("；"));
        if projection.visible_entities.truncated {
            line.push_str("；更远处还有");
        }
        line.push('。');
        lines.push(line);
    }

    if projection.visible_blocks.blocks.is_empty() {
        lines.push("视野里没有可见方块（可能都被挡住或未加载）。".to_owned());
    } else {
        // 同名聚合：数量 + 最近一处坐标（列表本身按距离从近到远）。
        let mut groups: Vec<(&str, usize, [i32; 3])> = Vec::new();
        for block in &projection.visible_blocks.blocks {
            match groups.iter_mut().find(|(name, ..)| *name == block.name) {
                Some((_, count, _)) => *count += 1,
                None => groups.push((&block.name, 1, block.position)),
            }
        }
        let described: Vec<String> = groups
            .into_iter()
            .map(|(name, count, [x, y, z])| {
                if count > 1 {
                    format!("{name} ×{count}（最近 {x},{y},{z}）")
                } else {
                    format!("{name}（{x},{y},{z}）")
                }
            })
            .collect();
        let mut line = format!("可见方块：{}", described.join("；"));
        if projection.visible_blocks.truncated {
            line.push_str("；更远处已截断");
        }
        line.push('。');
        lines.push(line);
    }
    lines.join("\n")
}

/// 增量查看结果的呈现：git 式 diff（维护者裁定）。
///
/// 基线是记忆整体，不是「上一次报告」——比较不带时间性。每行是一个
/// 五元组 (±, x, y, z, 方块名)：`+` 该事实进入所见，`-` 该事实不再成立；
/// 同格换方块 = 一撤一立两行，与 git 同法。末注保住「缺席≠没有」语义。
pub fn render_block_changes(changes: &[world::BlockChange]) -> String {
    let quad = |at: &[i32; 3], name: &str| format!("({}, {}, {}, {name})", at[0], at[1], at[2]);
    let mut lines = Vec::new();
    for change in changes {
        match change {
            world::BlockChange::Appeared { at, fact } => {
                lines.push(format!("+ {}", quad(at, &fact.name)));
            }
            world::BlockChange::Changed { at, was, now } => {
                lines.push(format!("- {}", quad(at, &was.name)));
                lines.push(format!("+ {}", quad(at, &now.name)));
            }
            world::BlockChange::Vanished { at, was } => {
                lines.push(format!("- {}", quad(at, &was.name)));
            }
        }
    }
    if lines.is_empty() {
        lines.push("（与记忆一致；未列出≠没有，确认用 at）".to_owned());
    } else {
        lines.push("（相对你已见过的；未列出≠没有，确认用 at）".to_owned());
    }
    lines.join("\n")
}

/// 定向查看结果的呈现：逐坐标报可见/不可见与原因。
pub fn render_directed(projection: &world::DirectedProjection) -> String {
    let mut lines = Vec::new();
    for seen in &projection.seen {
        // 空气格照实报「空」：亲眼可证的空位是一等观察结果（消失确认靠它），
        // 不说「是 air」这种半生不熟的话。
        if world::is_air_name(&seen.name) {
            lines.push(format!(
                "({}, {}, {})：看得见，那里是空的。",
                seen.at[0], seen.at[1], seen.at[2]
            ));
        } else {
            lines.push(format!(
                "({}, {}, {})：看得见，是 {}。",
                seen.at[0], seen.at[1], seen.at[2], seen.name
            ));
        }
    }
    for unseen in &projection.unseen {
        let mut reasons = Vec::new();
        for why in &unseen.why {
            reasons.push(match why {
                world::DirectedWhy::OutsideFov => "在视野外".to_owned(),
                world::DirectedWhy::TooFar => match (unseen.distance, unseen.max) {
                    (Some(distance), Some(max)) => {
                        format!("太远（{distance:.0} 格，上限 {max:.0}）")
                    }
                    _ => "太远".to_owned(),
                },
                world::DirectedWhy::Occluded => match &unseen.by {
                    Some(occluder) => format!(
                        "被 {}（{}, {}, {}）挡住",
                        occluder.name, occluder.at[0], occluder.at[1], occluder.at[2]
                    ),
                    None => "被挡住".to_owned(),
                },
                world::DirectedWhy::ChunkNotLoaded => "那片区域尚未加载".to_owned(),
                world::DirectedWhy::OutOfWorld => "超出世界高度".to_owned(),
            });
        }
        lines.push(format!(
            "({}, {}, {})：看不见——{}。",
            unseen.at[0],
            unseen.at[1],
            unseen.at[2],
            reasons.join("，")
        ));
    }
    if lines.is_empty() {
        "（没有要查看的目标。）".to_owned()
    } else {
        lines.join("\n")
    }
}

/// 任务变化的通知措辞。哪些值得投递是己的判据，这里只管怎么说。
pub fn render_job_entry(entry: &world::JobEntry) -> String {
    let world::JobKind::MoveTo {
        destination: [x, y, z],
    } = &entry.job;
    match entry.outcome {
        world::JobOutcome::Arrived => format!("你到达了目的地 ({x}, {y}, {z})。"),
        world::JobOutcome::Replaced => "先前的移动被新的目标顶替了。".to_owned(),
        world::JobOutcome::Stopped => "你停下了移动。".to_owned(),
        world::JobOutcome::PathEnded => {
            format!("你没能到达 ({x}, {y}, {z})——路走到了尽头，目的地过不去。")
        }
        world::JobOutcome::Stalled => {
            format!("你在前往 ({x}, {y}, {z}) 的路上卡住了一阵子，一直没有进展。")
        }
    }
}

/// 受伤的通知措辞。伤因缺席（服务端不给）时不编。
pub fn render_damage_entry(entry: &world::DamageEntry) -> String {
    let mut line = format!(
        "你受到了伤害，生命从 {} 降到 {}",
        trim_number(f64::from(entry.health_before)),
        trim_number(f64::from(entry.health_after))
    );
    if let Some(cause) = &entry.cause {
        line.push_str(&format!("（{}）", cause.0));
    }
    line.push('。');
    if entry.health_after <= 0.0 {
        line.push_str("你死了。");
    }
    line
}

/// 聊天未读数：窗内晚于已读水位的条数。纪元不同则整窗算新。
pub fn unread_chat_count(snap: &TickSnapshot, chat_read: (u64, u64)) -> usize {
    let (read_epoch, read_tick) = chat_read;
    count_after(&snap.chat, snap.epoch.0, read_epoch, read_tick)
}

fn count_after<T: WindowTick>(
    window: &Window<T>,
    window_epoch: u64,
    read_epoch: u64,
    read_tick: u64,
) -> usize {
    if window_epoch != read_epoch {
        return window.entries.len();
    }
    window
        .entries
        .iter()
        .filter(|entry| entry.tick() > read_tick)
        .count()
}

trait WindowTick {
    fn tick(&self) -> u64;
}

impl WindowTick for world::ChatEntry {
    fn tick(&self) -> u64 {
        self.tick
    }
}

// 角度归一化随视口内核迁入 world；此处再导出维持渲染层的调用面。
pub use world::wrap_degrees;

/// 原版昼夜时钟到时段词。锚点：0 日出、6000 正午、12000 日落、18000 午夜。
fn day_period_word(day_time: u64) -> &'static str {
    match day_time % 24_000 {
        0..=999 => "清晨",
        1_000..=4_999 => "上午",
        5_000..=6_999 => "正午前后",
        7_000..=10_999 => "下午",
        11_000..=12_999 => "黄昏",
        13_000..=16_999 => "前半夜",
        17_000..=18_999 => "午夜前后",
        19_000..=22_999 => "后半夜",
        _ => "黎明前",
    }
}

fn dimension_word(dimension: &str) -> &str {
    match dimension {
        "minecraft:overworld" => "主世界",
        "minecraft:the_nether" => "下界",
        "minecraft:the_end" => "末地",
        other => other,
    }
}

/// 原版 yaw 约定：0=南(+z)，90=西(−x)，180=北，270=东。
fn compass_word(yaw: f64) -> &'static str {
    const WORDS: [&str; 8] = ["南", "西南", "西", "西北", "北", "东北", "东", "东南"];
    let normalized = wrap_degrees(yaw).rem_euclid(360.0);
    WORDS[(((normalized + 22.5) / 45.0) as usize) % 8]
}

/// 对方相对自己的方位（世界罗盘向，与自己面朝无关）。
fn bearing_word(this: &world::SelfState, entity: &EntitySnapshot) -> &'static str {
    let dx = entity.position.x - this.position.x;
    let dz = entity.position.z - this.position.z;
    compass_word((-dx).atan2(dz).to_degrees())
}

fn distance_between(this: &world::SelfState, entity: &EntitySnapshot) -> f64 {
    let dx = entity.position.x - this.position.x;
    let dy = entity.position.y - this.position.y;
    let dz = entity.position.z - this.position.z;
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// 18.0 显示成 18，17.5 保留一位小数。
fn trim_number(value: f64) -> String {
    if (value - value.round()).abs() < 1e-9 {
        format!("{}", value.round() as i64)
    } else {
        format!("{value:.1}")
    }
}

#[cfg(test)]
mod tests;
