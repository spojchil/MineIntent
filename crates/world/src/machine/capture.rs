//! ECS → [`TickSnapshot`]：把 azalea 的组件直译成一帧快照。
//!
//! 直译无损、政策外置：这里不过滤、不判重要性、不做丢弃决策——
//! 给多少、怎么说归渲染层。世界方块不在快照里（见 [`super::blocks`]）。
//!
//! 断线或换维度的瞬间本地玩家实体可能已被移除，读不到就返回 `None`，
//! 不伪造坐标。

use std::sync::atomic::Ordering;
use std::time::SystemTime;

use azalea::entity::{
    dimensions::EntityDimensions, inventory::Inventory as InventoryComponent, Dead,
    EntityKindComponent, EntityUuid, LoadedBy, LocalEntity, LookDirection, Physics,
    Pose as PoseComponent, Position,
};
use azalea::player::GameProfileComponent;
use azalea::world::WorldName;
use azalea::Client;

use super::state::{Inner, EPOCH};
use crate::{
    ConnectionPhase, EntitySnapshot, ExperienceState, Inventory, InventorySlot, PlayerListEntry,
    SelfState, TickSnapshot, Vec3Value, Window,
};

/// 每 tick 的快照装配分段耗时。默认不开；`MINEINTENT_CAPTURE_TIMING=1` 才汇报。
///
/// 当初加它是为了查一桩嫌疑：实盘 `scan_changes` 比空闲基准慢几倍，怀疑是每
/// tick 采集占着 CPU、又在实体那段持 ECS 写锁。**实测洗清了嫌疑**——每 tick
/// 合计 0.13ms，20 次/秒不过 0.26%。留着不删：它是这条怀疑线的常驻证伪手段，
/// 下次再有人怀疑采集拖慢主循环，开个环境变量就能当场看数。
mod timing {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;

    pub(super) static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    static TICKS: AtomicU64 = AtomicU64::new(0);
    static ENTITIES_US: AtomicU64 = AtomicU64::new(0);
    static PLAYERS_US: AtomicU64 = AtomicU64::new(0);
    static INVENTORY_US: AtomicU64 = AtomicU64::new(0);
    static TOTAL_US: AtomicU64 = AtomicU64::new(0);
    static WARNED: AtomicBool = AtomicBool::new(false);

    pub(super) fn on() -> bool {
        *ENABLED.get_or_init(|| std::env::var("MINEINTENT_CAPTURE_TIMING").is_ok())
    }

    pub(super) fn record(
        entities: Duration,
        players: Duration,
        inventory: Duration,
        total: Duration,
    ) {
        let us = |d: Duration| d.as_micros() as u64;
        ENTITIES_US.fetch_add(us(entities), Ordering::Relaxed);
        PLAYERS_US.fetch_add(us(players), Ordering::Relaxed);
        INVENTORY_US.fetch_add(us(inventory), Ordering::Relaxed);
        TOTAL_US.fetch_add(us(total), Ordering::Relaxed);
        let n = TICKS.fetch_add(1, Ordering::Relaxed) + 1;
        // 每 200 tick（约 10 秒）一行。
        if !n.is_multiple_of(200) {
            return;
        }
        let avg = |slot: &AtomicU64| slot.load(Ordering::Relaxed) as f64 / n as f64 / 1000.0;
        println!(
            "[采集] {n} tick 均值：实体 {:.2}ms 玩家 {:.2}ms 物品栏 {:.2}ms 合计 {:.2}ms（每秒 20 次）",
            avg(&ENTITIES_US), avg(&PLAYERS_US), avg(&INVENTORY_US), avg(&TOTAL_US)
        );
        if avg(&TOTAL_US) > 5.0 && !WARNED.swap(true, Ordering::Relaxed) {
            println!("[采集] ⚠ 每 tick 超 5ms：20 次/秒即占用 10% 以上 CPU，且实体那段持 ECS 写锁");
        }
    }
}

/// 从 ECS 组装一帧快照。断线/换维度瞬间本地玩家实体可能已被移除——
/// 读不到就返回 None，不伪造坐标。
pub(super) fn assemble_snapshot(inner: &Inner, bot: &Client) -> Option<TickSnapshot> {
    let started = std::time::Instant::now();
    let pose = bot
        .try_query_self::<(&Position, &Physics, &LookDirection), _>(|(position, physics, look)| {
            (
                Vec3Value {
                    x: position.x,
                    y: position.y,
                    z: position.z,
                },
                Vec3Value {
                    x: physics.velocity.x,
                    y: physics.velocity.y,
                    z: physics.velocity.z,
                },
                f64::from(look.y_rot()),
                f64::from(look.x_rot()),
                physics.on_ground(),
            )
        })
        .ok()?;
    let (position, velocity, yaw, pitch, on_ground) = pose;

    let health = bot
        .get_component::<azalea::entity::metadata::Health>()
        .map(|value| f64::from(value.0))
        .unwrap_or(0.0);
    let hunger = bot.hunger();
    let experience = bot.experience();
    let alive = bot.get_component::<Dead>().is_none();

    let entities_elapsed;
    let players_elapsed;
    let inventory_started = std::time::Instant::now();
    let inventory_captured = capture_inventory(bot);
    let inventory_elapsed = inventory_started.elapsed();

    let self_state = SelfState {
        entity_key: bot.uuid().to_string(),
        username: bot.username(),
        position,
        velocity,
        yaw,
        pitch,
        on_ground,
        alive,
        health,
        food: f64::from(hunger.food),
        food_saturation: f64::from(hunger.saturation),
        oxygen: None,
        experience: Some(ExperienceState {
            level: experience.level,
            progress: f64::from(experience.progress),
            total: u64::from(experience.total),
        }),
        // 状态效果读取随后补；空集是"没读"不是"没有"，渲染层无效果时不着墨。
        effects: Vec::new(),
        inventory: inventory_captured,
    };

    let snapshot = TickSnapshot {
        epoch: EPOCH,
        tick: inner.tick.load(Ordering::Acquire),
        captured_at: SystemTime::now(),
        phase: ConnectionPhase::Ready,
        world_meta: inner.world_meta_now(),
        self_state,
        entities: {
            let at = std::time::Instant::now();
            let value = capture_entities(bot);
            entities_elapsed = at.elapsed();
            value
        },
        players: {
            let at = std::time::Instant::now();
            let value = capture_players(bot);
            players_elapsed = at.elapsed();
            value
        },
        chat: inner.chat_window_now(),
        // 声音窗：生产者未落位，先空。
        sounds: Window::default(),
        damage: inner.damage_window_now(),
        jobs: inner.jobs_window_now(),
        inventory_changes: inner.inventory_window_now(),
        open_screen: inner.open_screen.lock().clone(),
        screens: inner.screens_window_now(),
    };
    if timing::on() {
        timing::record(
            entities_elapsed,
            players_elapsed,
            inventory_elapsed,
            started.elapsed(),
        );
    }
    Some(snapshot)
}

/// azalea Menu 变体 → 屏种类直译名（snake_case，与注册名风格一致）。
pub(super) fn menu_kind_name(menu: &azalea::inventory::Menu) -> &'static str {
    use azalea::inventory::Menu;
    match menu {
        Menu::Player(_) => "player",
        Menu::Generic9x1 { .. } => "generic_9x1",
        Menu::Generic9x2 { .. } => "generic_9x2",
        Menu::Generic9x3 { .. } => "generic_9x3",
        Menu::Generic9x4 { .. } => "generic_9x4",
        Menu::Generic9x5 { .. } => "generic_9x5",
        Menu::Generic9x6 { .. } => "generic_9x6",
        Menu::Generic3x3 { .. } => "generic_3x3",
        Menu::Crafter3x3 { .. } => "crafter_3x3",
        Menu::Anvil { .. } => "anvil",
        Menu::Beacon { .. } => "beacon",
        Menu::BlastFurnace { .. } => "blast_furnace",
        Menu::BrewingStand { .. } => "brewing_stand",
        Menu::Crafting { .. } => "crafting",
        Menu::Enchantment { .. } => "enchantment",
        Menu::Furnace { .. } => "furnace",
        Menu::Grindstone { .. } => "grindstone",
        Menu::Hopper { .. } => "hopper",
        Menu::Lectern { .. } => "lectern",
        Menu::Loom { .. } => "loom",
        Menu::Merchant { .. } => "merchant",
        Menu::ShulkerBox { .. } => "shulker_box",
        Menu::Smithing { .. } => "smithing",
        Menu::Smoker { .. } => "smoker",
        Menu::CartographyTable { .. } => "cartography_table",
        Menu::Stonecutter { .. } => "stonecutter",
    }
}

fn capture_inventory(bot: &Client) -> Inventory {
    bot.get_component::<InventoryComponent>()
        .map(|inventory| {
            let slots = inventory
                .menu()
                .slots()
                .into_iter()
                .enumerate()
                .filter_map(|(slot, item)| {
                    if item.is_empty() {
                        None
                    } else {
                        Some(InventorySlot {
                            slot: slot as u32,
                            item_name: canonical_registry_name(&item.kind().to_string()),
                            count: item.count() as u32,
                            metadata: None,
                            durability_used: None,
                        })
                    }
                })
                .collect();
            Inventory {
                selected_hotbar_slot: inventory.selected_hotbar_slot,
                slots,
            }
        })
        .unwrap_or_default()
}

fn capture_players(bot: &Client) -> Vec<PlayerListEntry> {
    let mut players: Vec<_> = bot
        .tab_list()
        .into_iter()
        .map(|(uuid, info)| {
            let entity = bot.entity_by_uuid(uuid);
            let observed = entity.as_ref().and_then(|entity| {
                entity
                    .try_query_self::<(&Position, &LookDirection), _>(|(position, look)| {
                        (
                            Vec3Value {
                                x: position.x,
                                y: position.y,
                                z: position.z,
                            },
                            f64::from(look.y_rot()),
                            f64::from(look.x_rot()),
                        )
                    })
                    .ok()
            });
            let (position, yaw, pitch) = match observed {
                Some((position, yaw, pitch)) => (Some(position), Some(yaw), Some(pitch)),
                None => (None, None, None),
            };
            PlayerListEntry {
                player_key: uuid.to_string(),
                uuid: Some(uuid.to_string()),
                username: info.profile.name,
                listed: true,
                entity_tracked: entity.is_some(),
                position,
                yaw,
                pitch,
                held_item_name: None,
            }
        })
        .collect();
    players.sort_by(|left, right| left.player_key.cmp(&right.player_key));
    players
}

/// 读取当前客户端已知、仍在 ECS 中的实体；自身已在 self_state，不重复列出。
fn capture_entities(bot: &Client) -> Vec<EntitySnapshot> {
    let Ok(owner_world) = bot.try_query_self::<&WorldName, _>(|world_name| world_name.clone())
    else {
        return Vec::new();
    };
    let mut ecs = bot.ecs.write();
    let mut query = ecs.query::<(
        azalea::ecs::entity::Entity,
        &azalea::core::entity_id::MinecraftEntityId,
        &LoadedBy,
        &WorldName,
        &Position,
        &Physics,
        &LookDirection,
        Option<&EntityUuid>,
        Option<&EntityKindComponent>,
        Option<&GameProfileComponent>,
        Option<&Dead>,
        Option<&LocalEntity>,
        Option<&EntityDimensions>,
        Option<&PoseComponent>,
    )>();
    let mut entities: Vec<_> = query
        .iter(&ecs)
        .filter_map(
            |(
                _entity,
                protocol_entity_id,
                loaded_by,
                world_name,
                position,
                physics,
                look,
                uuid,
                kind,
                profile,
                dead,
                local,
                dimensions,
                pose,
            )| {
                if local.is_some() || !loaded_by.contains(&bot.entity) || world_name != &owner_world
                {
                    return None;
                }
                let uuid = uuid.map(|value| (**value).to_string());
                Some(EntitySnapshot {
                    entity_key: format!("{}:{}", EPOCH.0, **protocol_entity_id),
                    protocol_entity_id: **protocol_entity_id,
                    entity_type: kind
                        .map(|value| canonical_registry_name(&(**value).to_string()))
                        .unwrap_or_else(|| "unknown".to_owned()),
                    name: None,
                    username: profile.map(|value| value.name.clone()),
                    uuid,
                    position: Vec3Value {
                        x: position.x,
                        y: position.y,
                        z: position.z,
                    },
                    velocity: Vec3Value {
                        x: physics.velocity.x,
                        y: physics.velocity.y,
                        z: physics.velocity.z,
                    },
                    yaw: f64::from(look.y_rot()),
                    pitch: f64::from(look.x_rot()),
                    head_yaw: None,
                    width: dimensions.map_or(0.6, |value| f64::from(value.width)),
                    height: dimensions.map_or(1.8, |value| f64::from(value.height)),
                    on_ground: physics.on_ground(),
                    pose: pose.map(|value| format!("{value:?}").to_ascii_lowercase()),
                    held_item_name: None,
                    equipment: Vec::new(),
                    valid: dead.is_none(),
                })
            },
        )
        .collect();
    entities.sort_by(|left, right| left.entity_key.cmp(&right.entity_key));
    entities
}

/// azalea 注册名规范化：剥 `minecraft:` 前缀，与旧契约同法。
pub(super) fn canonical_registry_name(name: &str) -> String {
    name.strip_prefix("minecraft:").unwrap_or(name).to_owned()
}
