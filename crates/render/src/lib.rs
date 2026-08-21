//! 丙·渲染：tick 快照 → 模型可读文字，全部纯函数。
//!
//! 呈现选择政策归此处（给多少、怎么说）；事实归快照（world），本层不添不删事实，
//! 只挑选与措辞。处境厚度按裁定为"中版"：环境 + 体征 + 位置朝向 + 周围实体概览
//! + 聊天未读数；方块级细节走感知的 scan，不进每轮开场。
//!
//! 同类实体聚合呈现（数量 + 最近距离方位）——压缩方向是同质聚合，不是截断。

use world::{ConnectionPhase, EntitySnapshot, PickupEntry, TickSnapshot, Window};

/// 处境的一行是哪一行。
///
/// 帧只投**变了的那几行**，所以呈现层必须按行给出身份，而不是只给一整段文本：
/// 「位置变了」和「天黑了」是两件事，合成一段就只能整段重发。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SituationLine {
    /// 非就绪时唯一的一行：只说连接事实。
    Connection,
    Identity,
    Environment,
    Position,
    Vitals,
    Hotbar,
    Held,
    Nearby,
    Unread,
}

/// 处境逐行拆解。`chat_read` 是聊天已读水位 (epoch, tick)，
/// 未读数 = 聊天窗里晚于水位的条数（重连换纪元后整窗算新）。
///
/// 空行不出现在结果里——「没话可说」与「说了一句空话」对差异比对是两回事。
pub fn render_situation_lines(
    snap: &TickSnapshot,
    chat_read: (u64, u64),
) -> Vec<(SituationLine, String)> {
    // 非就绪状态下世界数据是旧的，处境只说连接事实，不拿旧世界冒充现在。
    let connection = match &snap.phase {
        ConnectionPhase::Ready => None,
        ConnectionPhase::Connecting => Some("正在连接服务器。".to_owned()),
        ConnectionPhase::Disconnected { reason } => Some(format!("已断线：{reason}")),
        ConnectionPhase::Stopped { reason } => Some(format!("连接已停止：{reason}")),
    };
    if let Some(text) = connection {
        return vec![(SituationLine::Connection, text)];
    }

    let mut lines = vec![
        (SituationLine::Identity, render_self_identity(snap)),
        (SituationLine::Environment, render_environment(snap)),
        (SituationLine::Position, render_position(snap)),
        (SituationLine::Vitals, render_vitals(snap)),
        (SituationLine::Hotbar, render_hotbar(snap)),
        (SituationLine::Held, render_held(snap)),
        (SituationLine::Nearby, render_nearby(snap)),
    ];
    let unread = unread_chat_count(snap, chat_read);
    if unread > 0 {
        lines.push((SituationLine::Unread, format!("聊天有 {unread} 条新消息。")));
    }
    lines.retain(|(_, line)| !line.is_empty());
    lines
}

/// 处境全文（开局与压缩之后投的那一份）。逐行拆解的直接拼接——
/// 两者共用一个来源，不可能对不上。
pub fn render_situation(snap: &TickSnapshot, chat_read: (u64, u64)) -> String {
    render_situation_lines(snap, chat_read)
        .into_iter()
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// 自称一行：你在这个世界里叫什么。
///
/// 2026-08-18 实盘教训：人设是静态配置，不知道运行时用户名；处境此前也不自称。
/// 于是模型在聊天窗里读到「MineIntentBot joined the game」与「Alice: MineIntentBot
/// 帮我弄一把铁镐」，**两条都当成了关于第三方的话**——它给自己取名「小雨」，
/// 跟自己打了招呼，然后判断这活儿是派给别人的，自己旁观。整跑就此停滞。
///
/// 防自激那套（按 UUID 比对发言者）只挡得住「别把自己说的话当成别人说的」，
/// 挡不住「不知道自己是谁」。名字每轮随快照现拉，天然跟着用户名走。
pub fn render_self_identity(snap: &TickSnapshot) -> String {
    let name = &snap.self_state.username;
    if name.is_empty() {
        return String::new();
    }
    format!("你在这个世界里的名字是 {name}——别人叫这个名字就是在叫你。")
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
    // 盔甲值 0 时省略：**原版盔甲条在 0 点时本就整条隐藏**，与 UI 行为逐字
    // 对应（旧线 `中期更新-08.md` §1）。这条省略只对「原版 UI 自己也隐藏」的
    // 项成立，不是通用的默认省略。
    if this.armor > 0.0 {
        line.push_str(&format!("，盔甲 {}", trim_number(this.armor)));
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

/// 快捷栏一行：九格内容 + 副手。
///
/// 原裁定（旧线 `中期更新-08.md` §1）：hotbar 段 = 9 格全量 + 当前选中栏位 +
/// 主手物品 + 副手物品。这一段在迁进 Rust 时整个丢了，直到 2026-08-20 实盘
/// 模型自己说「不知道自己有什么」才补回来。
///
/// **不打开物品栏也看得见快捷栏**——它常显在 HUD 上，和血条同一档，
/// 所以进处境；主背包要开界面才看得见，不进（见 `render_player_menu`）。
///
/// 「全量」按本仓既有口径呈现：点名有东西的格，空格只报数——与
/// `render_player_menu` 同一套，不逐格写「空」。空格数本身也是信息
/// （还能往回捡多少东西），但九个「空」字不是。
pub fn render_hotbar(snap: &TickSnapshot) -> String {
    let inventory = &snap.self_state.inventory;
    let space = &inventory.space;
    let item_at = |slot: u16| -> Option<String> {
        inventory
            .slots
            .iter()
            .find(|entry| entry.slot == u32::from(slot))
            .map(|entry| format!("{} ×{}", entry.item_name, entry.count))
    };
    let mut filled = Vec::new();
    let mut empty = 0usize;
    for slot in space.hotbar_slots() {
        match item_at(slot) {
            Some(item) => filled.push(format!("{}={item}", space.describe(slot))),
            None => empty += 1,
        }
    }
    let offhand = space
        .offhand_slot()
        .and_then(item_at)
        .unwrap_or_else(|| "空".to_owned());
    if filled.is_empty() {
        return format!("快捷栏九格都是空的。副手：{offhand}。");
    }
    let mut line = format!("快捷栏：{}", filled.join("、"));
    if empty > 0 {
        line.push_str(&format!("（其余 {empty} 格空）"));
    }
    line.push_str(&format!("。副手：{offhand}。"));
    line
}

/// 手持一行：选中哪一格、手里是什么。
///
/// 与快捷栏分成两行，是因为两者变化频率差一个量级：切换栏位随时发生，
/// 格子内容要捡到/用完才变。合成一行的话，每切一次栏位都要重发整条快捷栏
/// ——处境走的是逐行差异，行分得越准，重复投递越少。
pub fn render_held(snap: &TickSnapshot) -> String {
    let inventory = &snap.self_state.inventory;
    let space = &inventory.space;
    let slot = space.hotbar_slot(inventory.selected_hotbar_slot);
    let address = space.describe(slot);
    match inventory
        .slots
        .iter()
        .find(|entry| entry.slot == u32::from(slot))
    {
        Some(entry) => format!("手持 {address}：{} ×{}。", entry.item_name, entry.count),
        None => format!("手持 {address}：空手。"),
    }
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
    // 清单用**格位地址**，不是协议号——模型要照这上面抄去写 move。
    // 地址由映射生成（`world::slots`），协议号只活在机器层里。
    //
    // 空间取自快照,不再写死玩家屏:开着工作台时快捷栏在 37-45,
    // 写死 `player()` 会把每一格都标错名字。
    let space = snap.self_state.inventory.space.clone();
    let addressed = |name: &str, range: std::ops::RangeInclusive<u32>| {
        let filled: Vec<String> = range
            .clone()
            .filter_map(|slot| {
                item_at(slot).map(|item| format!("{}={item}", space.describe(slot as u16)))
            })
            .collect();
        if filled.is_empty() {
            format!(
                "{name}（{}）：空",
                space.legend_of(*range.start() as u16, *range.end() as u16)
            )
        } else {
            format!("{name}：{}", filled.join("、"))
        }
    };
    let held_menu_slot = 36 + u32::from(inventory.selected_hotbar_slot);
    let mut lines = vec![
        match item_at(0) {
            Some(item) => format!("result：{item}"),
            None => "result：空".to_owned(),
        },
        addressed("随身合成", 1..=4),
        addressed("盔甲", 5..=8),
        addressed("主背包", 9..=35),
        addressed("快捷栏", 36..=44),
        match item_at(45) {
            Some(item) => format!("offhand：{item}"),
            None => "offhand：空".to_owned(),
        },
        format!(
            "手持的是 {}{}。",
            space.describe(held_menu_slot as u16),
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
    // **说「当前是」，不说「出现了」。**
    //
    // 写口的 ack 早于服务端确认 2~3 tick（2026-08-17 实测），所以这条通知到达时
    // 说的往往是**上一步之后**的状态。「出现了 X」是在断言一次转变——模型会拿它
    // 去对自己的第几个动作，对不上就以为系统在闪烁；实测里它因此认定合成回执
    // 「严重对不上」，只能靠反复关掉重开物品栏来盘点（2026-08-20 长跑，模型自述）。
    //
    // 「当前是 X」只是读数：晚一拍的读数只是旧读数，不是假事件；后一条自然覆盖前
    // 一条，不需要谁去合并或抑制。又是同一条纪律——机器给事实，不给解释。
    match &entry.item_name {
        Some(name) => format!("{place}当前是 {name} ×{}。", entry.count),
        None => format!("{place}当前是空的。"),
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
/// 容器开屏通知的整段文本。
///
/// 组合根与 `model_surface` 导出**共用这一份**——此前两边各拼各的，
/// 2026-08-17 把用法全文改成按需取时导出没跟着变，「保真导出」当场失真。
/// 装配是呈现选择，归本层；组合根只管副作用（占域、屏状态翻转）。
///
/// 不带用法全文：用法是静态文本，随开屏无条件投递等于每开一次就往会话区
/// 塞一份同样的几百字节。要看用法自己调 `container` 的 describe。
pub fn render_container_opened(snap: &TickSnapshot, kind: &str, chat_displaced: bool) -> String {
    let title = snap
        .open_screen
        .as_ref()
        .and_then(|screen| screen.title.clone())
        .map(|title| format!("「{title}」"))
        .unwrap_or_default();
    let mut text = format!("容器界面已打开（{kind}{title}）。");
    if chat_displaced {
        text.push_str("（聊天框被它顶掉了。）");
    }
    text.push('\n');
    text.push_str(&render_container_menu(snap, kind));
    text.push_str("\n\n（这种容器怎么用：{\"action\":\"describe\"}）");
    text
}

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
        // 准星几乎总有落点：非空气=第一个撞上的方块；空气=视线尽头那格
        // （看天/一路空到扫描边界/撞到未加载区），如实说穿。
        if world::is_air_name(&block.name) {
            lines.push(format!(
                "准星方向一路是空气，视线尽头 ({}, {}, {})。",
                block.position[0], block.position[1], block.position[2]
            ));
        } else {
            lines.push(format!(
                "准星对着：{} ({}, {}, {})。",
                world::visible_block_label(&block.name, &block.properties),
                block.position[0],
                block.position[1],
                block.position[2]
            ));
        }
    }
    if let Some(block) = &projection.standing_on_block {
        lines.push(format!(
            "脚下踩着：{}。",
            world::visible_block_label(&block.name, &block.properties)
        ));
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
        // 同标签聚合：数量 + 最近一处坐标（列表本身按距离从近到远）。
        // 标签=名称+白名单视觉属性——燃着与熄着的熔炉是两组，远处可辨。
        let mut groups: Vec<(String, usize, [i32; 3])> = Vec::new();
        for block in &projection.visible_blocks.blocks {
            let label = world::visible_block_label(&block.name, &block.properties);
            match groups.iter_mut().find(|(seen, ..)| *seen == label) {
                Some((_, count, _)) => *count += 1,
                None => groups.push((label, 1, block.position)),
            }
        }
        let described: Vec<String> = groups
            .into_iter()
            .map(|(label, count, [x, y, z])| {
                if count > 1 {
                    format!("{label} ×{count}（最近 {x},{y},{z}）")
                } else {
                    format!("{label}（{x},{y},{z}）")
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

/// 记忆库查询结果的呈现。
///
/// **只给方块事实**：挑选、聚合、坐标、方位距离归机器；「这是悬崖」「那片林子」
/// 那类解释归模型（维护者裁定 2026-08-20——机器产出解释，本质是替模型下结论）。
///
/// `matches` 要按距离从近到远给好；同标签聚合成一组，报数量与最近的那一处。
pub fn render_memory_matches(origin: [i32; 3], matches: &[([i32; 3], String)]) -> String {
    if matches.is_empty() {
        return "记忆里没有符合的方块。".to_owned();
    }
    let mut groups: Vec<(String, usize, [i32; 3])> = Vec::new();
    for (at, label) in matches {
        match groups.iter_mut().find(|(seen, ..)| seen == label) {
            Some((_, count, _)) => *count += 1,
            None => groups.push((label.clone(), 1, *at)),
        }
    }
    let described: Vec<String> = groups
        .into_iter()
        .map(|(label, count, at)| {
            let [x, y, z] = at;
            let where_ = format!(
                "{x},{y},{z}，{} 格·{}",
                block_distance(origin, at).round() as i64,
                compass_from_delta(x - origin[0], z - origin[2])
            );
            if count > 1 {
                format!("{label} ×{count}（最近 {where_}）")
            } else {
                format!("{label}（{where_}）")
            }
        })
        .collect();
    described.join("；")
}

fn block_distance(a: [i32; 3], b: [i32; 3]) -> f64 {
    let (dx, dy, dz) = (
        f64::from(b[0] - a[0]),
        f64::from(b[1] - a[1]),
        f64::from(b[2] - a[2]),
    );
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// 八向方位。x 东正、z 南正（原版坐标系）。
fn compass_from_delta(dx: i32, dz: i32) -> &'static str {
    if dx == 0 && dz == 0 {
        return "就在脚下";
    }
    let angle = f64::from(dx).atan2(-f64::from(dz)).to_degrees();
    let normalized = (angle + 360.0) % 360.0;
    match ((normalized + 22.5) / 45.0) as usize % 8 {
        0 => "北",
        1 => "东北",
        2 => "东",
        3 => "东南",
        4 => "南",
        5 => "西南",
        6 => "西",
        _ => "西北",
    }
}

/// 增量查看结果的呈现：git 式 diff（维护者裁定）。
///
/// 基线是记忆整体，不是「上一次报告」——比较不带时间性。每行是一个
/// 五元组 (±, x, y, z, 方块状态)：第四元不止名称，是名称+白名单视觉属性
/// （`furnace[facing=north,lit=true]`）——远处即可分辨熔炉燃灭，状态变化
/// 也能一撤一立显出来；`+` 该事实进入所见，`-` 该事实不再成立，与 git 同法。
/// 末注保住「缺席≠没有」语义。
pub fn render_block_changes(changes: &[world::BlockChange]) -> String {
    let quad = |at: &[i32; 3], fact: &world::BlockFact| {
        format!(
            "({}, {}, {}, {})",
            at[0],
            at[1],
            at[2],
            world::visible_block_label(&fact.name, &fact.properties)
        )
    };
    let mut lines = Vec::new();
    for change in changes {
        match change {
            world::BlockChange::Appeared { at, fact } => {
                lines.push(format!("+ {}", quad(at, fact)));
            }
            world::BlockChange::Changed { at, was, now } => {
                lines.push(format!("- {}", quad(at, was)));
                lines.push(format!("+ {}", quad(at, now)));
            }
            world::BlockChange::Vanished { at, was } => {
                lines.push(format!("- {}", quad(at, was)));
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
                seen.at[0],
                seen.at[1],
                seen.at[2],
                world::visible_block_label(&seen.name, &seen.properties)
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

/// 进行中的进展措辞。
///
/// 一趟远路是多段的——按自己观察到的地图规划，只能先走到知识边界，到了看到更多
/// 再往前。每段开始说一句这一程走到哪，模型才知道自己为什么走走停停；不说，它
/// 看到的就是「走了一段莫名其妙停下」，然后去 scan 找补。
///
/// **不解释为什么到此为止**：路径是被知识边界截断还是被超时截断，`is_partial`
/// 分不出，说了就是把未知讲成已知。
pub fn render_job_progress(job: &world::JobKind, progress: &world::JobProgress) -> String {
    match (job, progress) {
        (
            world::JobKind::MoveTo {
                destination: [dx, dy, dz],
            },
            world::JobProgress::Leg { to: [x, y, z] },
        ) => {
            if [*x, *y, *z] == [*dx, *dy, *dz] {
                format!("这一程直接走到 ({dx}, {dy}, {dz})。")
            } else {
                format!(
                    "去 ({dx}, {dy}, {dz})：这一程先走到 ({x}, {y}, {z})，到了再看能不能接着走。"
                )
            }
        }
        (world::JobKind::Mine { .. }, world::JobProgress::Mined { done, total }) => {
            format!(
                "挖掉了第 {done} 块，还剩 {} 块。",
                total.saturating_sub(*done)
            )
        }
        (world::JobKind::PillarUp { .. }, world::JobProgress::Pillared { done, total }) => {
            format!(
                "垫上并站稳了第 {done} 格，还剩 {} 格。",
                total.saturating_sub(*done)
            )
        }
        // 类别对不上就如实说破，不编。
        (job, progress) => format!("任务收到了不属于它的进展：{job:?} / {progress:?}。"),
    }
}

/// 任务变化的通知措辞。哪些值得投递是己的判据，这里只管怎么说。
pub fn render_job_entry(entry: &world::JobEntry) -> String {
    let outcome = match &entry.event {
        world::JobEvent::Finished(outcome) => *outcome,
        world::JobEvent::Progress(progress) => return render_job_progress(&entry.job, progress),
    };
    match &entry.job {
        world::JobKind::MoveTo {
            destination: [x, y, z],
        } => match outcome {
            world::JobOutcome::Arrived => format!("你到达了目的地 ({x}, {y}, {z})。"),
            world::JobOutcome::Replaced => "先前的移动被新的目标顶替了。".to_owned(),
            world::JobOutcome::Stopped => "你停下了移动。".to_owned(),
            world::JobOutcome::PathEnded => {
                format!("你没能到达 ({x}, {y}, {z})——路走到了尽头，目的地过不去。")
            }
            world::JobOutcome::Stalled => {
                format!("你在前往 ({x}, {y}, {z}) 的路上卡住了一阵子，一直没有进展。")
            }
            // 挖掘结局落在移动任务上是不可能的；真出现了如实说破，不编。
            other => format!("移动任务收到了不属于它的结局：{other:?}。"),
        },
        world::JobKind::Mine { targets, done } => {
            let total = targets.len();
            match outcome {
                world::JobOutcome::Mined => format!("你挖完了这一串 {total} 块方块。"),
                world::JobOutcome::MineBlocked => match targets.get(*done) {
                    Some([x, y, z]) => format!(
                        "挖到第 {} 块就卡住了：({x}, {y}, {z}) 迟迟不碎——多半是够不着、被挡住，或者手上的工具挖不动它。前面 {done} 块已经挖掉了。",
                        done + 1
                    ),
                    None => format!("挖掘卡住了（已挖掉 {done}/{total} 块）。"),
                },
                world::JobOutcome::Replaced => {
                    format!("先前的挖掘被新的队列顶替了（已挖掉 {done}/{total} 块）。")
                }
                world::JobOutcome::Stopped => {
                    format!("你停下了挖掘（已挖掉 {done}/{total} 块）。")
                }
                other => format!("挖掘任务收到了不属于它的结局：{other:?}。"),
            }
        }
        world::JobKind::PillarUp { total, done } => match outcome {
            world::JobOutcome::Pillared => format!("你往上垫了 {total} 格，站稳了。"),
            world::JobOutcome::PillarBlocked => format!(
                "垫到第 {} 格就卡住了（已垫上 {done} 格）——跳起来没能腾出脚下那格，或者方块没放上去。",
                done + 1
            ),
            world::JobOutcome::Replaced => {
                format!("先前的垫柱被新的顶替了（已垫上 {done}/{total} 格）。")
            }
            world::JobOutcome::Stopped => format!("你停下了垫柱（已垫上 {done}/{total} 格）。"),
            other => format!("垫柱任务收到了不属于它的结局：{other:?}。"),
        },
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

/// 拾取成句：捡到了什么。
///
/// **主语是物品，不是格位**（维护者裁定 2026-08-21）：玩家捡东西时看见的是
/// 物品飞过来、听见「啵」的一声，不是「第 4 格的数字变了」。落进主背包还是
/// 快捷栏都一样报——对「我现在有这个东西」这件事，两者没有区别。
///
/// 所以这里一个格号都不出现。格号是屏内读数的事（`render_inventory_change`），
/// 那条通道要开着界面才看得见。
///
/// 同一批里同名同来源的合并成一条：砍一棵树会一根一根地捡，五条
/// 「捡到了 oak_log ×1」不如一条「捡到了 oak_log ×5」。合并只按
/// (谁, 什么) 分组——**不跨人合并**，「我捡了 3 个」和「Alice 捡了 3 个」
/// 是两件完全不同的事。
pub fn render_pickups(entries: &[PickupEntry]) -> Vec<String> {
    // 保序聚合：按首次出现的顺序输出，不排序——事实的先后本身是信息。
    let mut order: Vec<(bool, Option<String>, Option<String>)> = Vec::new();
    let mut totals: Vec<u32> = Vec::new();
    for entry in entries {
        let key = (entry.by_self, entry.by.clone(), entry.item_name.clone());
        match order.iter().position(|seen| *seen == key) {
            Some(index) => totals[index] = totals[index].saturating_add(entry.count),
            None => {
                order.push(key);
                totals.push(entry.count);
            }
        }
    }
    order
        .into_iter()
        .zip(totals)
        .map(|((by_self, by, item_name), count)| {
            let what = match item_name {
                Some(name) => format!("{name} ×{count}"),
                // 掉落物实体的元数据还没到就认不出来。说不知道，不编一个名字。
                None => format!("{count} 件没认出来的东西"),
            };
            match (by_self, by) {
                (true, _) => format!("捡到了 {what}。"),
                // 用户名与汉字之间留空格，「有人」不留——它本来就是汉字。
                (false, Some(who)) => format!("{who} 捡走了 {what}。"),
                (false, None) => format!("有人捡走了 {what}。"),
            }
        })
        .collect()
}
