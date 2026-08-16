use std::time::SystemTime;

use world::{
    ChatContent, ChatEntry, ConnectionPhase, EntitySnapshot, Epoch, FactSource, InventorySlot,
    StatusEffect, TickSnapshot, Vec3Value,
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
fn situation_covers_environment_position_vitals_in_order() {
    let text = render_situation(&snapshot(), (1, 100));
    let lines: Vec<&str> = text.lines().collect();

    assert_eq!(lines[0], "主世界，下午。");
    assert_eq!(lines[1], "位置 (120, 64, -36)，面朝西。");
    assert_eq!(lines[2], "生命 18/20，饥饿 15/20。");
    assert_eq!(lines.len(), 3, "没实体没未读时不该有第四行：{text}");
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
    assert_eq!(render_environment(&snap), "主世界，下午，下着雨。");

    snap.world_meta.thunder_level = 1.0;
    assert_eq!(render_environment(&snap), "主世界，下午，雷雨。");
}

#[test]
fn day_period_words_follow_vanilla_clock_anchors() {
    let mut snap = snapshot();
    for (day_time, expected) in [
        (0, "清晨"),
        (6_000, "正午前后"),
        (12_000, "黄昏"),
        (18_000, "午夜前后"),
        (23_500, "黎明前"),
        (24_000 + 500, "清晨"),
    ] {
        snap.world_meta.day_time = day_time;
        assert!(
            render_environment(&snap).contains(expected),
            "day_time={day_time} 应是{expected}：{}",
            render_environment(&snap)
        );
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
    // 2026-08-05 实盘出现过的累加值：与 126.76° 同朝向。
    assert!((wrap_degrees(-10_313.240_312_354_817) - 126.759_687_645_183).abs() < 1e-9);
    assert!(wrap_degrees(f64::NAN).is_nan());
    assert!(wrap_degrees(-0.0).is_sign_positive());
}

#[test]
fn job_entries_render_each_outcome_in_world_language() {
    let entry = |outcome| world::JobEntry {
        seq: 1,
        tick: 100,
        occurred_at: SystemTime::now(),
        job: world::JobKind::MoveTo {
            destination: [10, 64, -3],
        },
        outcome,
    };
    assert_eq!(
        render_job_entry(&entry(world::JobOutcome::Arrived)),
        "你到达了目的地 (10, 64, -3)。"
    );
    assert!(render_job_entry(&entry(world::JobOutcome::PathEnded)).contains("没能到达"));
    assert!(render_job_entry(&entry(world::JobOutcome::Stalled)).contains("卡住"));
    assert_eq!(
        render_job_entry(&entry(world::JobOutcome::Stopped)),
        "你停下了移动。"
    );
    assert!(render_job_entry(&entry(world::JobOutcome::Replaced)).contains("顶替"));
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
fn player_menu_lists_sections_by_protocol_slot() {
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
    assert!(text.contains("盔甲·头/胸/腿/脚（5-8）：6=iron_chestplate ×1"), "{text}");
    assert!(text.contains("主背包（9-35）：10=diamond ×3"), "{text}");
    assert!(text.contains("快捷栏（36-44）：38=bread ×7"), "{text}");
    assert!(text.contains("随身合成（1-4）：空"), "{text}");
    assert!(text.contains("手持的是快捷栏格 38（bread ×7）"), "{text}");
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
    assert_eq!(render_inventory_change(&entry), "物品栏格 12 出现了 oak_planks ×4。");

    let emptied = world::InventoryChangeEntry {
        slot: 0,
        item_name: None,
        count: 0,
        ..entry.clone()
    };
    assert_eq!(render_inventory_change(&emptied), "合成结果格（0）变空了。");

    // 容器格空间：成品格与普通格按工作台措辞。
    let crafted = world::InventoryChangeEntry {
        container_id: 3,
        slot: 0,
        item_name: Some("oak_button".to_owned()),
        count: 1,
        ..entry.clone()
    };
    assert_eq!(
        render_inventory_change(&crafted),
        "工作台成品格（0）出现了 oak_button ×1。"
    );
    let in_container = world::InventoryChangeEntry {
        container_id: 3,
        ..entry
    };
    assert_eq!(
        render_inventory_change(&in_container),
        "工作台格 12 出现了 oak_planks ×4。"
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
