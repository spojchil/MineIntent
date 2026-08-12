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

/// 从 ECS 组装一帧快照。断线/换维度瞬间本地玩家实体可能已被移除——
/// 读不到就返回 None，不伪造坐标。
pub(super) fn assemble_snapshot(inner: &Inner, bot: &Client) -> Option<TickSnapshot> {
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
        inventory: capture_inventory(bot),
    };

    Some(TickSnapshot {
        epoch: EPOCH,
        tick: inner.tick.load(Ordering::Acquire),
        captured_at: SystemTime::now(),
        phase: ConnectionPhase::Ready,
        world_meta: inner.world_meta_now(),
        self_state,
        entities: capture_entities(bot),
        players: capture_players(bot),
        chat: inner.chat_window_now(),
        // 声音窗：生产者未落位，先空。
        sounds: Window::default(),
        damage: inner.damage_window_now(),
        jobs: inner.jobs_window_now(),
    })
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
fn canonical_registry_name(name: &str) -> String {
    name.strip_prefix("minecraft:").unwrap_or(name).to_owned()
}
