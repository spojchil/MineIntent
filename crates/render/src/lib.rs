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
            .min_by(|a, b| {
                distance_between(this, a).total_cmp(&distance_between(this, b))
            })
            .expect("组内至少一个成员");
        let distance = distance_between(this, nearest);
        let place = format!("{} 格·{}", distance.round() as i64, bearing_word(this, nearest));
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
    let held = inventory
        .slots
        .iter()
        .find(|slot| slot.slot == u32::from(inventory.selected_hotbar_slot))
        .map(|slot| slot.item_name.as_str())
        .unwrap_or("空手");
    let items: Vec<String> = inventory
        .slots
        .iter()
        .map(|slot| format!("{} ×{}", slot.item_name, slot.count))
        .collect();
    format!("手持：{held}。背包：{}。", items.join("、"))
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
