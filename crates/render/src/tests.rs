use std::time::SystemTime;

use world::{
    ChatContent, ChatEntry, ConnectionPhase, EntitySnapshot, Epoch, FactSource, InventorySlot,
    PickupEntry, StatusEffect, TickSnapshot, Vec3Value,
};

use super::*;

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

fn entity(key: &str, entity_type: &str, x: f64, z: f64) -> EntitySnapshot {
    EntitySnapshot {
        entity_key: key.to_owned(),
        protocol_entity_id: 1,
        entity_type: entity_type.to_owned(),
        name: None,
        username: None,
        uuid: None,
        position: Vec3Value { x, y: 64.0, z },
        velocity: Vec3Value::default(),
        yaw: 0.0,
        pitch: 0.0,
        head_yaw: None,
        width: 0.6,
        height: 1.8,
        on_ground: true,
        pose: None,
        held_item_name: None,
        equipment: Vec::new(),
        valid: true,
    }
}

fn chat_entry(tick: u64) -> ChatEntry {
    ChatEntry {
        seq: tick,
        tick,
        occurred_at: SystemTime::now(),
        source: FactSource::ServerObserved,
        sender: None,
        content: ChatContent {
            plain_text: "hi".to_owned(),
            position: None,
            verified: None,
        },
    }
}

#[test]
fn situation_covers_identity_environment_position_vitals_in_order() {
    let text = render_situation(&snapshot(), (1, 100));
    let lines: Vec<&str> = text.lines().collect();

    // 自称在最前：不知道自己叫什么的话，点名的聊天会被当成关于第三方的话，
    // 模型会旁观派给自己的任务。
    assert_eq!(
        lines[0],
        "你在这个世界里的名字是 xiaoming——别人叫这个名字就是在叫你。"
    );
    assert_eq!(lines[1], "主世界。");
    assert_eq!(lines[2], "位置 (120, 64, -36)，面朝西。");
    assert_eq!(lines[3], "生命 18/20，饥饿 15/20。");
    // 快捷栏与手持排在生命之后、附近之前：都是「自己身上的事」。
    assert_eq!(lines[4], "快捷栏九格都是空的。副手：空。");
    assert_eq!(lines[5], "手持 hotbar 0：空手。");
    assert_eq!(lines.len(), 6, "没实体没未读时不该有第七行：{text}");
}

/// 用户名缺席（未就绪等）时不硬编一行空自称。
#[test]
fn identity_line_is_omitted_when_the_name_is_unknown() {
    let mut snap = snapshot();
    snap.self_state.username = String::new();
    let text = render_situation(&snap, (1, 100));
    assert!(!text.contains("名字是"), "{text}");
    assert!(text.starts_with("主世界"), "{text}");
}

#[test]
fn non_ready_phases_render_only_the_connection_fact() {
    let mut snap = snapshot();
    snap.phase = ConnectionPhase::Disconnected {
        reason: "服务器关闭".to_owned(),
    };
    assert_eq!(render_situation(&snap, (0, 0)), "已断线：服务器关闭");

    snap.phase = ConnectionPhase::Connecting;
    assert_eq!(render_situation(&snap, (0, 0)), "正在连接服务器。");
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
fn nearby_lists_players_individually_and_aggregates_same_type_mobs() {
    let mut snap = snapshot();
    let mut alice = entity("alice", "minecraft:player", 120.7, -38.2);
    alice.username = Some("Alice".to_owned());
    snap.entities = vec![
        entity("z1", "minecraft:zombie", 126.7, -35.2),
        alice,
        entity("z2", "minecraft:zombie", 130.7, -35.2),
        entity("self", "minecraft:player", 120.7, -35.2),
    ];

    let text = render_nearby(&snap);
    assert_eq!(
        text,
        "附近：玩家 Alice（3 格·北）；zombie ×2（最近 6 格·东）。"
    );
}

#[test]
fn empty_surroundings_render_nothing_instead_of_an_empty_header() {
    assert_eq!(render_nearby(&snapshot()), "");
}

#[test]
fn unread_chat_counts_entries_after_the_mark_and_resets_across_epochs() {
    let mut snap = snapshot();
    snap.chat.entries = vec![chat_entry(50), chat_entry(80), chat_entry(95)];

    assert_eq!(unread_chat_count(&snap, (1, 80)), 1);
    assert_eq!(unread_chat_count(&snap, (1, 95)), 0);
    // 水位停在上一条连接（纪元 0）：本连接整窗算新。
    assert_eq!(unread_chat_count(&snap, (0, 9_999)), 3);

    let text = render_situation(&snap, (1, 80));
    assert!(text.contains("聊天有 1 条新消息。"), "{text}");
    assert!(
        !render_situation(&snap, (1, 95)).contains("聊天"),
        "清零后不提聊天"
    );
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
fn job_entries_render_each_outcome_in_world_language() {
    let entry = |event| world::JobEntry {
        seq: 1,
        tick: 100,
        occurred_at: SystemTime::now(),
        id: world::JobId(7),
        fact: world::JobFact::Move {
            destination: [10, 64, -3],
            event,
        },
    };
    assert_eq!(
        render_job_entry(&entry(world::MoveEvent::Arrived)),
        "你到达了目的地 (10, 64, -3)。"
    );
    assert!(render_job_entry(&entry(world::MoveEvent::PathEnded)).contains("没能到达"));
    assert!(render_job_entry(&entry(world::MoveEvent::Stalled)).contains("卡住"));
    assert_eq!(
        render_job_entry(&entry(world::MoveEvent::Cancelled)),
        "你停下了移动。"
    );
    assert!(render_job_entry(&entry(world::MoveEvent::Replaced)).contains("顶替"));
    // 看门狗收槽也要有话说——沉默才是这套设计最怕的东西。
    assert!(render_job_entry(&entry(world::MoveEvent::TimedOut)).contains("没了下文"));
}

/// 「卡住」是进展不是终局：类型上就该分得开，呈现也不该说成结束了。
#[test]
fn stalling_is_progress_while_giving_up_is_terminal() {
    assert!(!world::MoveEvent::Stalled.is_terminal());
    assert!(world::MoveEvent::PathEnded.is_terminal());
    assert!(!world::MineEvent::Broke { done: 1, total: 3 }.is_terminal());
    assert!(world::MineEvent::Blocked { at: [1, 2, 3] }.is_terminal());
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
        InventorySlot {
            slot: 6,
            item_name: "iron_chestplate".to_owned(),
            count: 1,
            metadata: None,
            durability_used: None,
        },
        InventorySlot {
            slot: 10,
            item_name: "diamond".to_owned(),
            count: 3,
            metadata: None,
            durability_used: None,
        },
        InventorySlot {
            slot: 38,
            item_name: "bread".to_owned(),
            count: 7,
            metadata: None,
            durability_used: None,
        },
    ];
    let text = render_player_menu(&snap);
    // 清单用**格位地址**：模型照这上面抄就能写 move，协议号不出现在可见面。
    assert!(
        text.contains("盔甲：armor chest=iron_chestplate ×1"),
        "{text}"
    );
    assert!(text.contains("主背包：pack 1=diamond ×3"), "{text}");
    assert!(text.contains("快捷栏：hotbar 2=bread ×7"), "{text}");
    assert!(text.contains("随身合成（craft 0-3）：空"), "{text}");
    assert!(text.contains("手持的是 hotbar 2（bread ×7）"), "{text}");
    assert!(
        !text.contains("（36-44）"),
        "协议号不该出现在清单里：{text}"
    );
}

#[test]
fn inventory_changes_name_the_slot_and_call_out_the_craft_result() {
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
    assert_eq!(
        render_inventory_change(&entry),
        "物品栏格 12 当前是 oak_planks ×4。"
    );

    let emptied = world::InventoryChangeEntry {
        slot: 0,
        item_name: None,
        count: 0,
        ..entry.clone()
    };
    assert_eq!(
        render_inventory_change(&emptied),
        "合成结果格（0）当前是空的。"
    );

    // 容器格空间：措辞中性（0 号在工作台是成品格、在熔炉是原料格，
    // 语义随开屏清单给过，这里不扣帽子）。
    let crafted = world::InventoryChangeEntry {
        container_id: 3,
        slot: 0,
        item_name: Some("oak_button".to_owned()),
        count: 1,
        ..entry.clone()
    };
    assert_eq!(
        render_inventory_change(&crafted),
        "容器格 0 当前是 oak_button ×1。"
    );
    let in_container = world::InventoryChangeEntry {
        container_id: 3,
        ..entry
    };
    assert_eq!(
        render_inventory_change(&in_container),
        "容器格 12 当前是 oak_planks ×4。"
    );
}

#[test]
fn crafting_menu_listing_uses_the_crafting_slot_space() {
    let mut snap = world::TickSnapshot::empty(world::Epoch(1), 1, world::ConnectionPhase::Ready);
    snap.self_state.inventory.slots = vec![
        world::InventorySlot {
            slot: 0,
            item_name: "oak_button".to_owned(),
            count: 1,
            metadata: None,
            durability_used: None,
        },
        world::InventorySlot {
            slot: 5,
            item_name: "oak_planks".to_owned(),
            count: 1,
            metadata: None,
            durability_used: None,
        },
        world::InventorySlot {
            slot: 20,
            item_name: "diamond".to_owned(),
            count: 3,
            metadata: None,
            durability_used: None,
        },
        world::InventorySlot {
            slot: 45,
            item_name: "bread".to_owned(),
            count: 7,
            metadata: None,
            durability_used: None,
        },
    ];
    let text = render_container_menu(&snap, "crafting");
    assert!(text.contains("成品（0）：oak_button ×1"), "{text}");
    assert!(text.contains("摆料 3×3（1-9）：5=oak_planks ×1"), "{text}");
    assert!(text.contains("主背包（10-36）：20=diamond ×3"), "{text}");
    // 玩家屏里 45 是副手；工作台屏里 45 是快捷栏末格。
    assert!(text.contains("快捷栏（37-45）：45=bread ×7"), "{text}");

    // 已知容器按通用三段（箱子：27 容器格 + 主背包 + 快捷栏）。
    let chest = render_container_menu(&snap, "generic_9x3");
    assert!(chest.contains("容器格（0-26）"), "{chest}");
    assert!(chest.contains("主背包（27-53）"), "{chest}");
    assert!(chest.contains("快捷栏（54-62）"), "{chest}");
    // 未知种类逐格罗列，不猜段界。
    let unknown = render_container_menu(&snap, "modded_thing");
    assert!(unknown.contains("非空格位："), "{unknown}");
}

#[test]
fn furnace_menu_listing_names_the_three_working_slots() {
    let mut snap = world::TickSnapshot::empty(world::Epoch(1), 1, world::ConnectionPhase::Ready);
    snap.self_state.inventory.slots = vec![
        world::InventorySlot {
            slot: 0,
            item_name: "raw_iron".to_owned(),
            count: 3,
            metadata: None,
            durability_used: None,
        },
        world::InventorySlot {
            slot: 1,
            item_name: "coal".to_owned(),
            count: 2,
            metadata: None,
            durability_used: None,
        },
        world::InventorySlot {
            slot: 10,
            item_name: "bread".to_owned(),
            count: 5,
            metadata: None,
            durability_used: None,
        },
        world::InventorySlot {
            slot: 31,
            item_name: "stick".to_owned(),
            count: 4,
            metadata: None,
            durability_used: None,
        },
    ];
    // 熔炉族三种同形：原料/燃料/成品 + 玩家区（3-29 主背包、30-38 快捷栏）。
    for kind in ["furnace", "blast_furnace", "smoker"] {
        let text = render_container_menu(&snap, kind);
        assert!(text.contains("原料（0）：raw_iron ×3"), "{kind}: {text}");
        assert!(text.contains("燃料（1）：coal ×2"), "{kind}: {text}");
        assert!(text.contains("成品（2）：空"), "{kind}: {text}");
        assert!(
            text.contains("主背包（3-29）：10=bread ×5"),
            "{kind}: {text}"
        );
        assert!(
            text.contains("快捷栏（30-38）：31=stick ×4"),
            "{kind}: {text}"
        );
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
    let before = render_situation_lines(&snap, (1, 0));
    snap.self_state.inventory.selected_hotbar_slot = 4;
    let after = render_situation_lines(&snap, (1, 0));

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

/// 够不着任何东西时这一行不出现——原版此时也什么都不显示，
/// 而处境的空行本来就不进差异。
#[test]
fn looking_at_nothing_produces_no_line() {
    let snap = snapshot();
    assert_eq!(render_looking_at(&snap), "");
    assert!(
        !render_situation_lines(&snap, (1, 100))
            .iter()
            .any(|(line, _)| *line == SituationLine::LookingAt),
        "空的准星行不该进处境"
    );
}

/// 群系接在维度后面——同属「我在哪」，都是 F3 免费常驻的那一档。
#[test]
fn biome_rides_the_environment_line_after_the_dimension() {
    let mut snap = snapshot();
    assert_eq!(render_environment(&snap), "主世界。");
    snap.self_state.biome = Some("minecraft:taiga".to_owned());
    assert_eq!(render_environment(&snap), "主世界，minecraft:taiga。");
}
