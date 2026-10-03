use std::time::SystemTime;

use world::{
    ConnectionPhase, Epoch, InventorySlot, PickupEntry, StatusEffect, TickSnapshot, Vec3Value,
};

use super::*;

fn input_outcome(ended: world::InputEnd, ticks: u32) -> world::InputOutcome {
    world::InputOutcome {
        ticks,
        ended,
        from: [0.5, 64.0, 0.5],
        to: [0.5, 64.0, 0.5],
        yaw: 90.0,
        pitch: 0.0,
        keys: Default::default(),
        mouse: None,
        pressed_on: None,
        broken: Vec::new(),
        placed: Vec::new(),
        used: None,
        unconfirmed: None,
    }
}

#[test]
fn input_receipt_says_why_it_let_go_and_what_changed() {
    let mut outcome = input_outcome(world::InputEnd::BlockBroken, 19);
    outcome.pressed_on = Some(world::LookingAt::Block {
        name: "grass_block".to_owned(),
        position: [0, 63, 0],
        face: "up".to_owned(),
    });
    outcome.broken = vec!["grass_block".to_owned()];
    outcome.to = [0.5, 63.0, 0.5];
    let text = render_input_outcome(&outcome);
    assert!(
        text.starts_with("准星下的方块碎了，按住 0.9 秒时松手。"),
        "{text}"
    );
    assert!(
        text.contains("按下时准星对着 grass_block（0, 63, 0），命中上面。"),
        "{text}"
    );
    assert!(text.contains("挖碎了：grass_block。"), "{text}");
    assert!(text.contains("下降了 1.0 格。"), "{text}");
}

#[test]
fn input_receipt_for_a_tap_that_moved_nothing() {
    let mut outcome = input_outcome(world::InputEnd::Elapsed, 1);
    outcome.keys.jump = true;
    let text = render_input_outcome(&outcome);
    assert_eq!(
        text,
        "点按了一下，已松开。\n位置没有变。\n现在面朝西（yaw 90°，pitch 0°）。"
    );
}

fn snapshot() -> TickSnapshot {
    let mut snap = TickSnapshot::empty(Epoch(1), 100, ConnectionPhase::Ready);
    snap.world_meta.dimension = "minecraft:overworld".to_owned();
    snap.world_meta.day_time = 8_000;
    snap.self_state.entity_key = "self".to_owned();
    snap.self_state.username = "xiaoming".to_owned();
    snap.self_state.position = Vec3Value {
        x: 120.7,
        y: 64.0,
        z: -35.2,
    };
    snap.self_state.yaw = 90.0;
    snap.self_state.on_ground = true;
    snap.self_state.alive = true;
    snap.self_state.health = 18.0;
    snap.self_state.food = 15.0;
    snap
}

#[test]
fn situation_covers_identity_environment_position_vitals_in_order() {
    let text = render_situation(&snapshot());
    let lines: Vec<&str> = text.lines().collect();

    // 自称在最前：不知道自己叫什么的话，点名的聊天会被当成关于第三方的话，
    // 模型会旁观派给自己的任务。
    assert_eq!(
        lines[0],
        "你在这个世界里的名字是 xiaoming——别人叫这个名字就是在叫你。"
    );
    assert_eq!(lines[1], "主世界。");
    assert_eq!(lines[2], "位置 (120, 64, -36)，面朝西。");
    assert_eq!(lines[3], "准星没有对着够得着的方块或实体。");
    assert_eq!(lines[4], "生命 18/20，饥饿 15/20。");
    // 快捷栏与手持排在生命之后：都是「自己身上的事」。
    assert_eq!(lines[5], "快捷栏九格都是空的。副手：空。");
    assert_eq!(lines[6], "手持 hotbar 0：空手。");
    assert_eq!(lines.len(), 7, "{text}");
}

/// 用户名缺席（未就绪等）时不硬编一行空自称。
#[test]
fn identity_line_is_omitted_when_the_name_is_unknown() {
    let mut snap = snapshot();
    snap.self_state.username = String::new();
    let text = render_situation(&snap);
    assert!(!text.contains("名字是"), "{text}");
    assert!(text.starts_with("主世界"), "{text}");
}

#[test]
fn non_ready_phases_render_only_the_connection_fact() {
    let mut snap = snapshot();
    snap.phase = ConnectionPhase::Disconnected {
        reason: "服务器关闭".to_owned(),
    };
    assert_eq!(render_situation(&snap), "已断线：服务器关闭");

    snap.phase = ConnectionPhase::Connecting;
    assert_eq!(render_situation(&snap), "正在连接服务器。");
}

#[test]
fn weather_appears_only_when_it_rains() {
    let mut snap = snapshot();
    assert!(!render_environment(&snap).contains("雨"));

    snap.world_meta.rain_level = 1.0;
    assert_eq!(render_environment(&snap), "主世界，下着雨。");

    snap.world_meta.thunder_level = 1.0;
    assert_eq!(render_environment(&snap), "主世界，雷雨。");
}

#[test]
/// 时段**不进环境行**：洞内不可见，判据不过；
/// 26.1.2 客户端 45 项 F3 注册表里也没有一天内时刻这一项。
/// 时钟怎么变，这一行都不该跟着变。
fn day_time_never_reaches_the_environment_line() {
    let mut snap = snapshot();
    let baseline = render_environment(&snap);
    for day_time in [0, 6_000, 12_000, 18_000, 23_500, 24_500] {
        snap.world_meta.day_time = day_time;
        assert_eq!(
            render_environment(&snap),
            baseline,
            "day_time={day_time} 不该改变环境行"
        );
    }
    for word in ["清晨", "正午", "黄昏", "午夜", "黎明", "下午"] {
        assert!(!baseline.contains(word), "环境行不该有时段词：{baseline}");
    }
}

#[test]
fn vitals_mention_oxygen_effects_and_death_only_when_present() {
    let mut snap = snapshot();
    snap.self_state.health = 17.5;
    snap.self_state.oxygen = Some(120.0);
    snap.self_state.effects = vec![StatusEffect {
        name: "speed".to_owned(),
        amplifier: 1,
        duration_ticks: Some(400),
    }];
    assert_eq!(
        render_vitals(&snap),
        "生命 17.5/20，饥饿 15/20，氧气 120；效果：speed 2（20 秒）。"
    );

    snap.self_state.alive = false;
    assert_eq!(render_vitals(&snap), "你已经死亡。");
}

#[test]
fn inventory_shows_held_item_and_full_list() {
    let mut snap = snapshot();
    assert_eq!(render_inventory(&snap), "背包是空的。");

    snap.self_state.inventory.selected_hotbar_slot = 0;
    snap.self_state.inventory.slots = vec![
        InventorySlot {
            // 快照格号是菜单协议号：选中快捷格 0 = 菜单 36（格 0 是合成结果）。
            slot: 36,
            item_name: "iron_sword".to_owned(),
            count: 1,
            metadata: None,
            durability_used: None,
        },
        InventorySlot {
            slot: 9,
            item_name: "bread".to_owned(),
            count: 7,
            metadata: None,
            durability_used: None,
        },
    ];
    assert_eq!(
        render_inventory(&snap),
        "手持：iron_sword。背包：iron_sword ×1、bread ×7。"
    );
}

#[test]
fn wrap_degrees_matches_the_vanilla_half_open_range() {
    for (input, expected) in [
        (0.0, 0.0),
        (180.0, -180.0),
        (360.0, 0.0),
        (-270.0, 90.0),
        (450.0, 90.0),
    ] {
        assert_eq!(wrap_degrees(input), expected, "input {input}");
    }
    // 长时间游戏里 yaw 会累加到很大的数：它应与 126.76° 同朝向。
    assert!((wrap_degrees(-10_313.240_312_354_817) - 126.759_687_645_183).abs() < 1e-9);
    assert!(wrap_degrees(f64::NAN).is_nan());
    assert!(wrap_degrees(-0.0).is_sign_positive());
}

#[test]
fn damage_entries_say_the_drop_and_death_without_inventing_causes() {
    let entry = world::DamageEntry {
        seq: 2,
        tick: 100,
        occurred_at: SystemTime::now(),
        health_before: 17.0,
        health_after: 13.5,
        cause: None,
    };
    assert_eq!(
        render_damage_entry(&entry),
        "你受到了伤害，生命从 17 降到 13.5。"
    );

    let fatal = world::DamageEntry {
        health_after: 0.0,
        ..entry
    };
    assert!(render_damage_entry(&fatal).ends_with("你死了。"));
}

#[test]
fn player_menu_lists_sections_by_slot_address() {
    let mut snap = snapshot();
    snap.self_state.inventory.selected_hotbar_slot = 2;
    snap.self_state.inventory.slots = vec![
        slot(2, "oak_planks", 1),
        slot(6, "iron_chestplate", 1),
        slot(10, "diamond", 3),
        slot(38, "bread", 7),
    ];
    let text = render_player_menu(&snap);
    // 清单用**格位地址**：模型照这上面抄就能写 move，协议号不出现在可见面。
    assert!(
        text.contains("盔甲：armor chest=iron_chestplate ×1"),
        "{text}"
    );
    assert!(text.contains("主背包：pack 1=diamond ×3"), "{text}");
    assert!(text.contains("快捷栏：hotbar 2=bread ×7"), "{text}");
    assert!(text.contains("手持的是 hotbar 2（bread ×7）"), "{text}");
    // 随身合成画成网格，**空格也画出来**：模型摆配方时要能一眼看出哪格是空的。
    assert!(
        text.contains("随身合成 2×2（craft 0-3，行优先）："),
        "{text}"
    );
    assert!(text.contains("craft 0-1  [空][oak_planks ×1]"), "{text}");
    assert!(text.contains("craft 2-3  [空][空]"), "{text}");
    assert!(
        !text.contains("（36-44）"),
        "协议号不该出现在清单里：{text}"
    );
}

#[test]
fn inventory_changes_name_the_slot_by_address() {
    let entry = world::InventoryChangeEntry {
        seq: 1,
        tick: 10,
        occurred_at: SystemTime::now(),
        source: world::FactSource::ServerObserved,
        container_id: 0,
        slot: 12,
        item_name: Some("oak_planks".to_owned()),
        count: 4,
    };
    // 地址，不是协议号——这条通知和 move 的写法必须是同一套。
    assert_eq!(
        render_inventory_change(&entry),
        "物品栏 pack 3 当前是 oak_planks ×4。"
    );

    let emptied = world::InventoryChangeEntry {
        slot: 0,
        item_name: None,
        count: 0,
        ..entry
    };
    assert_eq!(
        render_inventory_change(&emptied),
        "合成结果格（result）当前是空的。"
    );
}

/// 工作台屏的地址空间：0 成品、1-9 摆料、10-36 主背包、37-45 快捷栏。
fn crafting_space() -> world::slots::SlotSpace {
    world::slots::SlotSpace::new(37, 45, None, world::slots::OwnArea::Crafting)
}

#[test]
fn crafting_menu_listing_draws_the_grid_including_empty_slots() {
    let mut snap = world::TickSnapshot::empty(world::Epoch(1), 1, world::ConnectionPhase::Ready);
    snap.self_state.inventory.space = crafting_space();
    snap.self_state.inventory.slots = vec![
        slot(0, "oak_button", 1),
        slot(5, "oak_planks", 1),
        slot(20, "diamond", 3),
        slot(45, "bread", 7),
    ];
    let text = render_container_menu(&snap, "crafting");
    assert!(text.contains("成品 result：oak_button ×1"), "{text}");
    // 3×3 逐行画出，行标给出这一行的地址区间——模型不必自己把一维数成二维。
    assert!(text.contains("摆料 3×3（craft 0-8，行优先）："), "{text}");
    assert!(text.contains("craft 0-2  [空][空][空]"), "{text}");
    assert!(
        text.contains("craft 3-5  [空][oak_planks ×1][空]"),
        "{text}"
    );
    assert!(text.contains("craft 6-8  [空][空][空]"), "{text}");
    assert!(text.contains("主背包：pack 10=diamond ×3"), "{text}");
    // 玩家屏里 45 是副手；工作台屏里 45 是快捷栏末格。
    assert!(text.contains("快捷栏：hotbar 8=bread ×7"), "{text}");
    // 协议号一个都不许漏出来——旧清单正是在这里把模型带偏了整整一局。
    for leaked in [
        "（1-9）",
        "（10-36）",
        "（37-45）",
        "5=oak_planks",
        "20=diamond",
    ] {
        assert!(!text.contains(leaked), "协议号漏进清单：{leaked}\n{text}");
    }
}

#[test]
fn known_containers_use_three_sections_and_unknown_ones_are_listed_flat() {
    let mut snap = world::TickSnapshot::empty(world::Epoch(1), 1, world::ConnectionPhase::Ready);
    snap.self_state.inventory.space = world::slots::SlotSpace::new(
        54,
        62,
        None,
        world::slots::OwnArea::Named("chest".to_owned()),
    );
    snap.self_state.inventory.slots = vec![slot(20, "diamond", 3)];
    let chest = render_container_menu(&snap, "generic_9x3");
    assert!(chest.contains("容器格：chest 20=diamond ×3"), "{chest}");
    assert!(chest.contains("主背包（pack 0-26）：空"), "{chest}");
    assert!(chest.contains("快捷栏（hotbar 0-8）：空"), "{chest}");
    // 未知种类逐格罗列，不猜段界；地址照样由映射生成。
    let unknown = render_container_menu(&snap, "modded_thing");
    assert!(
        unknown.contains("非空格位：chest 20=diamond ×3"),
        "{unknown}"
    );
}

#[test]
fn furnace_menu_listing_names_the_three_working_slots() {
    let mut snap = world::TickSnapshot::empty(world::Epoch(1), 1, world::ConnectionPhase::Ready);
    snap.self_state.inventory.space =
        world::slots::SlotSpace::new(30, 38, None, world::slots::OwnArea::Furnace);
    snap.self_state.inventory.slots = vec![
        slot(0, "raw_iron", 3),
        slot(1, "coal", 2),
        slot(10, "bread", 5),
        slot(31, "stick", 4),
    ];
    // 熔炉族三种同形：原料/燃料/成品 + 玩家区。
    for kind in ["furnace", "blast_furnace", "smoker"] {
        let text = render_container_menu(&snap, kind);
        assert!(text.contains("原料 smelt：raw_iron ×3"), "{kind}: {text}");
        assert!(text.contains("燃料 fuel：coal ×2"), "{kind}: {text}");
        assert!(text.contains("成品 result：空"), "{kind}: {text}");
        assert!(text.contains("主背包：pack 7=bread ×5"), "{kind}: {text}");
        assert!(text.contains("快捷栏：hotbar 1=stick ×4"), "{kind}: {text}");
        assert!(!text.contains("（3-29）"), "协议号漏进清单：{kind}: {text}");
    }
}

fn slot(slot: u32, name: &str, count: u32) -> InventorySlot {
    InventorySlot {
        slot,
        item_name: name.to_owned(),
        count,
        metadata: None,
        durability_used: None,
    }
}

#[test]
fn hotbar_names_filled_slots_and_counts_the_empty_ones() {
    let mut snap = snapshot();
    // 玩家屏：快捷栏 36-44，副手 45。
    snap.self_state.inventory.slots = vec![
        slot(36, "iron_pickaxe", 1),
        slot(39, "oak_log", 12),
        slot(45, "shield", 1),
        // 主背包的东西不该出现在快捷栏这一行里。
        slot(9, "bread", 7),
    ];
    let line = render_hotbar(&snap);
    assert_eq!(
        line,
        "快捷栏：hotbar 0=iron_pickaxe ×1、hotbar 3=oak_log ×12（其余 7 格空）。副手：shield ×1。"
    );
    assert!(!line.contains("bread"), "主背包不在快捷栏这一行");
}

#[test]
fn empty_hotbar_says_so_once_instead_of_nine_times() {
    let snap = snapshot();
    assert_eq!(render_hotbar(&snap), "快捷栏九格都是空的。副手：空。");
}

#[test]
fn held_line_reports_the_selected_slot_and_what_is_in_it() {
    let mut snap = snapshot();
    snap.self_state.inventory.slots = vec![slot(39, "oak_log", 12)];
    snap.self_state.inventory.selected_hotbar_slot = 3;
    assert_eq!(render_held(&snap), "手持 hotbar 3：oak_log ×12。");

    snap.self_state.inventory.selected_hotbar_slot = 5;
    assert_eq!(render_held(&snap), "手持 hotbar 5：空手。");
}

/// 开着容器时快捷栏整体后移一格（工作台屏 37-45）。写死玩家屏会把
/// 每一格都标错名字——这正是快照自带 `space` 要挡住的事。
#[test]
fn hotbar_follows_the_active_slot_space_not_the_player_screen() {
    let mut snap = snapshot();
    snap.self_state.inventory.space =
        world::slots::SlotSpace::new(37, 45, None, world::slots::OwnArea::Crafting);
    // 工作台屏里协议号 37 才是快捷栏第一格；36 是主背包最后一格。
    snap.self_state.inventory.slots = vec![slot(37, "iron_pickaxe", 1), slot(36, "bread", 7)];
    let line = render_hotbar(&snap);
    assert_eq!(
        line,
        "快捷栏：hotbar 0=iron_pickaxe ×1（其余 8 格空）。副手：空。"
    );
    assert!(
        !line.contains("bread"),
        "36 在工作台屏里是主背包，不是快捷栏"
    );
}

/// 处境逐行差异靠的是行分得准：切换栏位只该动「手持」那一行，
/// 快捷栏内容那一行不该跟着重发。
#[test]
fn switching_slots_changes_only_the_held_line() {
    let mut snap = snapshot();
    snap.self_state.inventory.slots = vec![slot(36, "iron_pickaxe", 1)];
    let before = render_situation_lines(&snap);
    snap.self_state.inventory.selected_hotbar_slot = 4;
    let after = render_situation_lines(&snap);

    let changed: Vec<SituationLine> = before
        .iter()
        .zip(after.iter())
        .filter(|((_, a), (_, b))| a != b)
        .map(|((line, _), _)| *line)
        .collect();
    assert_eq!(changed, vec![SituationLine::Held]);
}

fn pickup(
    seq: u64,
    by_self: bool,
    by: Option<&str>,
    item: Option<&str>,
    count: u32,
) -> PickupEntry {
    PickupEntry {
        seq,
        tick: 100,
        occurred_at: SystemTime::now(),
        by_self,
        by: by.map(str::to_owned),
        item_name: item.map(str::to_owned),
        count,
    }
}

/// 砍一棵树是一根一根地捡：五条「×1」不如一条「×5」。
#[test]
fn pickups_merge_by_item_within_one_delivery() {
    let entries: Vec<PickupEntry> = (0..5)
        .map(|seq| pickup(seq, true, None, Some("oak_log"), 1))
        .collect();
    assert_eq!(render_pickups(&entries), vec!["捡到了 oak_log ×5。"]);
}

/// 「我捡了 3 个」和「Alice 捡了 3 个」是两件完全不同的事，不许并成一条。
#[test]
fn pickups_never_merge_across_pickers() {
    let entries = vec![
        pickup(1, true, None, Some("iron_ore"), 2),
        pickup(2, false, Some("Alice"), Some("iron_ore"), 3),
        pickup(3, true, None, Some("iron_ore"), 1),
    ];
    assert_eq!(
        render_pickups(&entries),
        vec!["捡到了 iron_ore ×3。", "Alice 捡走了 iron_ore ×3。"]
    );
}

/// 拾取只说物品，**一个格号都不出现**。
#[test]
fn pickups_say_nothing_about_slots() {
    let lines = render_pickups(&[pickup(1, true, None, Some("oak_log"), 3)]);
    let text = lines.join("");
    for word in ["hotbar", "pack", "slot", "格"] {
        assert!(!text.contains(word), "拾取不该提格位：{text}");
    }
}

/// 掉落物实体的元数据还没到就认不出来。说不知道，不编一个名字。
#[test]
fn unknown_item_is_said_to_be_unknown_not_invented() {
    assert_eq!(
        render_pickups(&[pickup(1, true, None, None, 2)]),
        vec!["捡到了 2 件没认出来的东西。"]
    );
    assert_eq!(
        render_pickups(&[pickup(2, false, None, Some("bread"), 1)]),
        vec!["有人捡走了 bread ×1。"]
    );
}

/// 盔甲值 0 时整条省略——原版盔甲条在 0 点时本就隐藏，逐字对应。
/// 这条省略只对「原版 UI 自己也隐藏」的项成立，不推广成通用默认。
#[test]
fn armor_is_omitted_at_zero_and_shown_otherwise() {
    let mut snap = snapshot();
    assert_eq!(render_vitals(&snap), "生命 18/20，饥饿 15/20。");

    snap.self_state.armor = 8.0;
    assert_eq!(render_vitals(&snap), "生命 18/20，饥饿 15/20，盔甲 8。");
}

/// 准星是 F3 的 `LOOKING_AT_*`：免费常驻，说清楚对着哪一格、命中哪一面
/// （放置要贴在那一面上）。
#[test]
fn looking_at_names_the_block_and_the_face() {
    let mut snap = snapshot();
    snap.self_state.looking_at = Some(world::LookingAt::Block {
        name: "oak_log".to_owned(),
        position: [46, 70, 40],
        face: "up".to_owned(),
    });
    assert_eq!(
        render_looking_at(&snap),
        "准星对着 oak_log（46, 70, 40），命中上面。"
    );

    snap.self_state.looking_at = Some(world::LookingAt::Entity {
        kind: "minecraft:zombie".to_owned(),
        name: None,
    });
    assert_eq!(render_looking_at(&snap), "准星对着 minecraft:zombie。");

    snap.self_state.looking_at = Some(world::LookingAt::Entity {
        kind: "minecraft:player".to_owned(),
        name: Some("Alice".to_owned()),
    });
    assert_eq!(
        render_looking_at(&snap),
        "准星对着 Alice（minecraft:player）。"
    );
}

/// 够不着任何东西时也明说：处境只报变了的行，这一行要是消失，
/// 「转到空处」和「没变」就分不开了。
#[test]
fn looking_at_nothing_says_so() {
    let snap = snapshot();
    assert_eq!(render_looking_at(&snap), "准星没有对着够得着的方块或实体。");
    assert!(render_situation_lines(&snap)
        .iter()
        .any(|(line, _)| *line == SituationLine::LookingAt));
}

/// 按了鼠标却没对着东西，回执明说；没按鼠标就不提准星。
#[test]
fn input_receipt_says_when_the_click_hit_nothing() {
    let mut outcome = input_outcome(world::InputEnd::Elapsed, 100);
    outcome.mouse = Some(world::MouseButton::Left);
    let text = render_input_outcome(&outcome);
    assert!(
        text.contains("按下时准星没有对着够得着的方块或实体。"),
        "{text}"
    );

    outcome.mouse = None;
    assert!(!render_input_outcome(&outcome).contains("准星"));
}

/// 右键放下的方块写进回执：放置是瞬间动作，按客户端判断算数。
#[test]
fn input_receipt_lists_placed_blocks() {
    let mut outcome = input_outcome(world::InputEnd::Elapsed, 1);
    outcome.mouse = Some(world::MouseButton::Right);
    outcome.placed = vec![("cobblestone".to_owned(), [0, 150, 1])];
    let text = render_input_outcome(&outcome);
    assert!(
        text.contains("放下了：cobblestone（0, 150, 1）。"),
        "{text}"
    );
}

/// 等服务端开界面超时：说清楚是不确定，不是没开。
#[test]
fn input_receipt_explains_an_unconfirmed_screen() {
    let mut outcome = input_outcome(world::InputEnd::Elapsed, 1);
    outcome.mouse = Some(world::MouseButton::Right);
    outcome.unconfirmed = Some(world::Unconfirmed::Screen);
    let text = render_input_outcome(&outcome);
    assert!(text.contains("可能是延迟过高"), "{text}");
    assert!(text.contains("暂时无法确定"), "{text}");
}

/// 右键开门：客户端当场改的状态写进回执。
#[test]
fn input_receipt_names_what_a_block_use_changed() {
    let mut outcome = input_outcome(world::InputEnd::Elapsed, 1);
    outcome.mouse = Some(world::MouseButton::Right);
    outcome.used = Some(world::BlockUsed {
        block: "oak_door".to_owned(),
        position: [1, 64, 2],
        times: 1,
        changes: vec![("open".to_owned(), "false".to_owned(), "true".to_owned())],
    });
    let text = render_input_outcome(&outcome);
    assert!(
        text.contains("用了 oak_door（1, 64, 2）：open false→true。"),
        "{text}"
    );
}

/// 拉杆这类只有服务端改：限时内没变就说不确定，并提醒别再按。
#[test]
fn input_receipt_explains_an_unconfirmed_block_use() {
    let mut outcome = input_outcome(world::InputEnd::Elapsed, 1);
    outcome.mouse = Some(world::MouseButton::Right);
    outcome.used = Some(world::BlockUsed {
        block: "lever".to_owned(),
        position: [1, 64, 2],
        times: 1,
        changes: Vec::new(),
    });
    outcome.unconfirmed = Some(world::Unconfirmed::BlockUse);
    let text = render_input_outcome(&outcome);
    assert!(text.contains("用了 lever（1, 64, 2）"), "{text}");
    assert!(text.contains("可能是延迟过高"), "{text}");
    assert!(text.contains("别急着再按"), "{text}");
}

/// 手上瞬时键各有一句回执；对调超时要明说不确定。
#[test]
fn hand_receipts_say_what_is_in_hand() {
    let cobble = Some(("cobblestone".to_owned(), 12));
    assert_eq!(
        render_hand_outcome(&world::HandOutcome::Dropped {
            item: "cobblestone".to_owned(),
            count: 1
        }),
        "丢出了 cobblestone ×1。"
    );
    assert_eq!(
        render_hand_outcome(&world::HandOutcome::Swapped {
            main: None,
            offhand: cobble.clone()
        }),
        "主副手对调了。现在主手：空；副手：cobblestone ×12。"
    );
    assert_eq!(
        render_hand_outcome(&world::HandOutcome::Selected {
            slot: 3,
            held: cobble.clone()
        }),
        "换到 hotbar 3，手里：cobblestone ×12。"
    );
    let text = render_hand_outcome(&world::HandOutcome::SwapUnconfirmed {
        main: cobble,
        offhand: None,
    });
    assert!(text.contains("可能是延迟过高"), "{text}");
    assert!(text.contains("主手：cobblestone ×12；副手：空"), "{text}");
}

/// 只转视角、什么键都没按：不说成「点按了一下」。
#[test]
fn input_receipt_for_a_turn_only_says_no_key_was_pressed() {
    let outcome = input_outcome(world::InputEnd::Elapsed, 1);
    let text = render_input_outcome(&outcome);
    assert!(text.starts_with("只转了视角，没有按键。"), "{text}");

    let mut outcome = input_outcome(world::InputEnd::Elapsed, 1);
    outcome.keys.forward = true;
    let text = render_input_outcome(&outcome);
    assert!(text.starts_with("点按了一下，已松开。"), "{text}");
}

/// 朝向连续转身会累加（-225°），回执里归一到 [-180, 180)。
#[test]
fn input_receipt_wraps_the_yaw() {
    let mut outcome = input_outcome(world::InputEnd::Elapsed, 1);
    outcome.yaw = -225.0;
    let text = render_input_outcome(&outcome);
    assert!(text.contains("yaw 135°"), "{text}");

    outcome.yaw = -0.3;
    outcome.pitch = -0.15;
    let text = render_input_outcome(&outcome);
    assert!(text.contains("yaw 0°，pitch 0°"), "{text}");
}

/// 群系接在维度后面——同属「我在哪」，都是 F3 免费常驻的那一档。
#[test]
fn biome_rides_the_environment_line_after_the_dimension() {
    let mut snap = snapshot();
    assert_eq!(render_environment(&snap), "主世界。");
    snap.self_state.biome = Some("minecraft:taiga".to_owned());
    assert_eq!(render_environment(&snap), "主世界，minecraft:taiga。");
}
