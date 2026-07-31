use azalea::{
    entity::inventory::Inventory,
    entity::{
        dimensions::EntityDimensions, Dead, EntityKindComponent, EntityUuid, LocalEntity,
        LookDirection, Physics, Pose, Position,
    },
    local_player::LocalGameMode,
    local_player::WorldHolder,
    player::GameProfileComponent,
    world::WorldName,
    Client,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use azalea::physics::collision::BlockWithShape;

/// 与 MineIntent `MinecraftSnapshotV1` 对齐的快照协议版本。
pub const SNAPSHOT_PROTOCOL: &str = "mineintent.minecraft.snapshot.v1";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Vec3Value {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vec3Value {
    fn from_azalea(value: azalea::Vec3) -> Self {
        Self {
            x: value.x,
            y: value.y,
            z: value.z,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorldSnapshot {
    pub world_id: String,
    pub dimension: String,
    pub minecraft_version: String,
    pub protocol_version: u32,
    pub game_mode: String,
    pub min_y: i32,
    pub height: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExperienceSnapshot {
    pub level: u32,
    pub progress: f32,
    pub total: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelfSnapshot {
    pub entity_key: String,
    pub username: String,
    pub position: Vec3Value,
    pub velocity: Vec3Value,
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    pub alive: bool,
    pub health: f32,
    pub food: u32,
    pub food_saturation: f32,
    pub experience: ExperienceSnapshot,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InventorySlotSnapshot {
    pub slot: usize,
    pub item_name: String,
    pub count: i32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InventorySnapshot {
    pub selected_hotbar_slot: u8,
    pub slots: Vec<InventorySlotSnapshot>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrackedPlayerSnapshot {
    pub player_key: String,
    pub uuid: String,
    pub username: String,
    pub listed: bool,
    pub entity_tracked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Vec3Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yaw: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pitch: Option<f32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolEntitySnapshot {
    pub entity_key: String,
    pub protocol_entity_id: i32,
    #[serde(rename = "type")]
    pub entity_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    pub position: Vec3Value,
    pub velocity: Vec3Value,
    pub yaw: f32,
    pub pitch: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_yaw: Option<f32>,
    pub width: f32,
    pub height: f32,
    pub on_ground: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pose: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub held_item_name: Option<String>,
    pub equipment: Vec<EntityEquipmentSnapshot>,
    pub valid: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntityEquipmentSnapshot {
    pub slot: u8,
    pub item_name: String,
    pub count: i32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MinecraftSnapshotV1 {
    pub protocol: String,
    pub snapshot_revision: u64,
    pub lifecycle_revision: u64,
    pub captured_at: chrono::DateTime<chrono::Utc>,
    pub process_session_id: String,
    pub connection_epoch: u64,
    pub connection_attempt_id: String,
    pub world: WorldSnapshot,
    #[serde(rename = "self")]
    pub self_snapshot: SelfSnapshot,
    pub inventory: InventorySnapshot,
    pub tracked_players: Vec<TrackedPlayerSnapshot>,
}

impl MinecraftSnapshotV1 {
    /// 时间戳和修订号变化不应单独造成 `snapshot_changed` 事件。
    pub fn same_state_as(&self, other: &Self) -> bool {
        self.world == other.world
            && self.self_snapshot == other.self_snapshot
            && self.inventory == other.inventory
            && self.tracked_players == other.tracked_players
    }
}

/// 只读取当前姿态，用于记录“客户端预测”轨迹；它不被写入服务端事实快照。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PoseSnapshot {
    pub position: Vec3Value,
    pub velocity: Vec3Value,
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlockPosition {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolBlockSnapshot {
    pub position: BlockPosition,
    pub name: String,
    pub state_id: u32,
    pub properties: BTreeMap<String, String>,
    pub collision_shapes: Vec<[f64; 6]>,
    pub transparent_hint: bool,
    pub bounding_box: BlockBoundingBox,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockBoundingBox {
    Block,
    Empty,
}

/// 将 Azalea 的注册表状态转换为 MineIntent 观察边界使用的完整块 DTO。
///
/// `transparent_hint` 是观察层的保守提示，不把它误称为服务端的可见性结论；
/// 真正的“可见”仍需由上层从观察者位置做射线/暴露面判断。
pub fn block_snapshot(
    position: BlockPosition,
    state: azalea::block::BlockState,
) -> ProtocolBlockSnapshot {
    let block: Box<dyn azalea::block::BlockTrait> = Box::from(state);
    let collision_shape = state.collision_shape();
    let collision_shapes = collision_shape
        .to_aabbs()
        .into_iter()
        .map(|aabb| {
            [
                aabb.min.x, aabb.min.y, aabb.min.z, aabb.max.x, aabb.max.y, aabb.max.z,
            ]
        })
        .collect();
    let properties = block
        .property_map()
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();

    ProtocolBlockSnapshot {
        position,
        // MineIntent 的 DTO 使用 registry 的本地名（例如 `stone`、`air`），
        // 不在这里额外添加 `minecraft:` 命名空间前缀。
        name: block.id().to_owned(),
        state_id: state.id() as u32,
        properties,
        collision_shapes,
        transparent_hint: transparent_hint(block.id(), state.outline_shape()),
        bounding_box: if collision_shape.is_empty() {
            BlockBoundingBox::Empty
        } else {
            BlockBoundingBox::Block
        },
    }
}

fn transparent_hint(name: &str, outline_shape: &azalea::physics::collision::VoxelShape) -> bool {
    // Azalea 暴露了 outline/collision 几何，但 26.1 的方块注册表没有把
    // Mineflayer 的 `transparent` 布尔字段作为同名组件暴露。对常见的全体积
    // 透明块补充注册名提示，其余非完整轮廓按保守的“可能透光”处理。
    let named_transparent = name == "air"
        || name == "cave_air"
        || name == "void_air"
        || name.contains("glass")
        || name.ends_with("leaves")
        || name == "water"
        || name == "lava"
        || name == "powder_snow";
    named_transparent || !is_full_cube(outline_shape)
}

fn local_registry_name(name: &str) -> String {
    name.strip_prefix("minecraft:").unwrap_or(name).to_owned()
}

fn is_full_cube(shape: &azalea::physics::collision::VoxelShape) -> bool {
    let boxes = shape.to_aabbs();
    boxes.len() == 1
        && boxes[0].min.x == 0.0
        && boxes[0].min.y == 0.0
        && boxes[0].min.z == 0.0
        && boxes[0].max.x == 1.0
        && boxes[0].max.y == 1.0
        && boxes[0].max.z == 1.0
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum BlockReadResult {
    Loaded { block: ProtocolBlockSnapshot },
    Unloaded,
    OutOfWorld,
}

pub fn capture_pose(bot: &Client) -> PoseSnapshot {
    bot.query_self::<(&Position, &Physics, &LookDirection), _>(|(position, physics, look)| {
        PoseSnapshot {
            position: Vec3Value::from_azalea(**position),
            velocity: Vec3Value::from_azalea(physics.velocity),
            yaw: look.y_rot(),
            pitch: look.x_rot(),
            on_ground: physics.on_ground(),
        }
    })
}

pub fn capture(
    bot: &Client,
    world_id: &str,
    process_session_id: &str,
    connection_epoch: u64,
    connection_attempt_id: &str,
    snapshot_revision: u64,
    lifecycle_revision: u64,
    captured_at: chrono::DateTime<chrono::Utc>,
) -> MinecraftSnapshotV1 {
    let pose = capture_pose(bot);
    let (dimension, min_y, height) = bot
        .try_query_self::<(Option<&WorldName>, Option<&WorldHolder>), _>(|(world_name, holder)| {
            let dimension = world_name
                .map(ToString::to_string)
                .unwrap_or_else(|| "minecraft:overworld".to_owned());
            let (min_y, height) = holder
                .map(|holder| {
                    let world = holder.shared.read();
                    (world.chunks.min_y(), world.chunks.height())
                })
                .unwrap_or((0, 384));
            (dimension, min_y, height)
        })
        .unwrap_or_else(|_| ("minecraft:overworld".to_owned(), 0, 384));

    let health = bot
        .get_component::<azalea::entity::metadata::Health>()
        .map(|value| **value)
        .unwrap_or(0.0);
    let hunger = bot.hunger();
    let experience = bot.experience();
    let game_mode = bot
        .get_component::<LocalGameMode>()
        .map(|value| value.current.name().to_owned())
        .unwrap_or_else(|| "survival".to_owned());
    let alive = bot.get_component::<Dead>().is_none();

    MinecraftSnapshotV1 {
        protocol: SNAPSHOT_PROTOCOL.to_owned(),
        snapshot_revision,
        lifecycle_revision,
        captured_at,
        process_session_id: process_session_id.to_owned(),
        connection_epoch,
        connection_attempt_id: connection_attempt_id.to_owned(),
        world: WorldSnapshot {
            world_id: world_id.to_owned(),
            dimension,
            minecraft_version: "26.1.2".to_owned(),
            protocol_version: 775,
            game_mode,
            min_y,
            height,
        },
        self_snapshot: SelfSnapshot {
            entity_key: bot.uuid().to_string(),
            username: bot.username(),
            position: pose.position,
            velocity: pose.velocity,
            yaw: pose.yaw,
            pitch: pose.pitch,
            on_ground: pose.on_ground,
            alive,
            health,
            food: hunger.food,
            food_saturation: hunger.saturation,
            experience: ExperienceSnapshot {
                level: experience.level,
                progress: experience.progress,
                total: experience.total,
            },
        },
        inventory: capture_inventory(bot),
        tracked_players: capture_tracked_players(bot),
    }
}

fn capture_inventory(bot: &Client) -> InventorySnapshot {
    bot.get_component::<Inventory>()
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
                        Some(InventorySlotSnapshot {
                            slot,
                            item_name: local_registry_name(&item.kind().to_string()),
                            count: item.count(),
                        })
                    }
                })
                .collect();
            InventorySnapshot {
                selected_hotbar_slot: inventory.selected_hotbar_slot,
                slots,
            }
        })
        .unwrap_or(InventorySnapshot {
            selected_hotbar_slot: 0,
            slots: Vec::new(),
        })
}

fn capture_tracked_players(bot: &Client) -> Vec<TrackedPlayerSnapshot> {
    let mut players: Vec<_> = bot
        .tab_list()
        .into_iter()
        .map(|(uuid, info)| {
            let entity = bot.entity_by_uuid(uuid);
            let (position, yaw, pitch) = entity
                .as_ref()
                .and_then(|entity| {
                    entity
                        .try_query_self::<(&Position, &LookDirection), _>(|(position, look)| {
                            (
                                Vec3Value::from_azalea(**position),
                                look.y_rot(),
                                look.x_rot(),
                            )
                        })
                        .ok()
                })
                .map_or((None, None, None), |(position, yaw, pitch)| {
                    (Some(position), Some(yaw), Some(pitch))
                });
            TrackedPlayerSnapshot {
                player_key: uuid.to_string(),
                uuid: uuid.to_string(),
                username: info.profile.name,
                listed: true,
                entity_tracked: entity.is_some(),
                position,
                yaw,
                pitch,
            }
        })
        .collect();
    players.sort_by(|left, right| left.player_key.cmp(&right.player_key));
    players
}

/// 读取当前客户端已知、仍在 ECS 中的协议实体；未加载的实体不伪造为可见。
pub fn capture_tracked_entities(bot: &Client) -> Vec<ProtocolEntitySnapshot> {
    let mut ecs = bot.ecs.write();
    let mut query = ecs.query::<(
        azalea::ecs::entity::Entity,
        &azalea::core::entity_id::MinecraftEntityId,
        &Position,
        &Physics,
        &LookDirection,
        Option<&EntityUuid>,
        Option<&EntityKindComponent>,
        Option<&GameProfileComponent>,
        Option<&Dead>,
        Option<&LocalEntity>,
        Option<&EntityDimensions>,
        Option<&Pose>,
    )>();
    let mut entities: Vec<_> = query
        .iter(&ecs)
        .filter_map(
            |(
                entity,
                protocol_entity_id,
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
                if local.is_some() {
                    // 与 MineIntent 的 `bot.entities` 语义一致：自身已经在
                    // MinecraftSnapshotV1.self 中，不重复作为附近实体返回。
                    return None;
                }
                let uuid = uuid.map(|value| (**value).to_string());
                Some(ProtocolEntitySnapshot {
                    entity_key: uuid
                        .clone()
                        .unwrap_or_else(|| format!("ecs-{}", entity.to_bits())),
                    protocol_entity_id: **protocol_entity_id,
                    name: None,
                    entity_type: kind
                        .map(|value| local_registry_name(&(**value).to_string()))
                        .unwrap_or_else(|| "unknown".to_owned()),
                    username: profile.map(|value| value.name.clone()),
                    uuid,
                    position: Vec3Value::from_azalea(**position),
                    velocity: Vec3Value::from_azalea(physics.velocity),
                    yaw: look.y_rot(),
                    pitch: look.x_rot(),
                    head_yaw: None,
                    width: dimensions.map_or(0.6, |value| value.width),
                    height: dimensions.map_or(1.8, |value| value.height),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_serializes_with_mineintent_field_names() {
        let snapshot = MinecraftSnapshotV1 {
            protocol: SNAPSHOT_PROTOCOL.to_owned(),
            snapshot_revision: 1,
            lifecycle_revision: 1,
            captured_at: chrono::DateTime::parse_from_rfc3339("2026-08-01T00:00:00Z")
                .expect("时间应有效")
                .with_timezone(&chrono::Utc),
            process_session_id: "session".to_owned(),
            connection_epoch: 1,
            connection_attempt_id: "attempt-1".to_owned(),
            world: WorldSnapshot {
                world_id: "world".to_owned(),
                dimension: "minecraft:overworld".to_owned(),
                minecraft_version: "26.1.2".to_owned(),
                protocol_version: 775,
                game_mode: "survival".to_owned(),
                min_y: -64,
                height: 384,
            },
            self_snapshot: SelfSnapshot {
                entity_key: "uuid".to_owned(),
                username: "bot".to_owned(),
                position: Vec3Value {
                    x: 1.0,
                    y: 2.0,
                    z: 3.0,
                },
                velocity: Vec3Value {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                yaw: 0.0,
                pitch: 0.0,
                on_ground: true,
                alive: true,
                health: 20.0,
                food: 20,
                food_saturation: 5.0,
                experience: ExperienceSnapshot {
                    level: 0,
                    progress: 0.0,
                    total: 0,
                },
            },
            inventory: InventorySnapshot {
                selected_hotbar_slot: 0,
                slots: vec![],
            },
            tracked_players: vec![],
        };
        let value = serde_json::to_value(snapshot).expect("快照应能编码");
        assert_eq!(value["snapshotRevision"], 1);
        assert_eq!(value["self"]["foodSaturation"], 5.0);
        assert!(value.get("self_snapshot").is_none());
    }

    #[test]
    fn block_snapshot_keeps_geometry_and_state_metadata() {
        let block = block_snapshot(
            BlockPosition { x: 3, y: 64, z: -2 },
            azalea::block::BlockState::AIR,
        );
        assert_eq!(block.name, "air");
        assert_eq!(block.state_id, 0);
        assert!(block.collision_shapes.is_empty());
        assert_eq!(block.bounding_box, BlockBoundingBox::Empty);
        assert!(block.transparent_hint);
    }

    #[test]
    fn registry_names_match_mineintent_local_name_contract() {
        assert_eq!(local_registry_name("minecraft:dirt"), "dirt");
        assert_eq!(local_registry_name("stone"), "stone");
    }
}
