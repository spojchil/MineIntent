use azalea::{
    entity::inventory::Inventory,
    entity::{
        dimensions::EntityDimensions, Dead, EntityKindComponent, EntityUuid, LoadedBy, LocalEntity,
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

/// 三种空气的注册名。视口把它们当作“这一格没有东西”，
/// `transparent_hint` 也拿同一份名单当透明处理。
pub(crate) fn is_air_name(name: &str) -> bool {
    matches!(name, "air" | "cave_air" | "void_air")
}

fn transparent_hint(name: &str, outline_shape: &azalea::physics::collision::VoxelShape) -> bool {
    // Azalea 暴露了 outline/collision 几何，但 26.1 的方块注册表没有把
    // Mineflayer 的 `transparent` 布尔字段作为同名组件暴露。对常见的全体积
    // 透明块补充注册名提示，其余非完整轮廓按保守的“可能透光”处理。
    let named_transparent = is_air_name(name)
        || name.contains("glass")
        || name.ends_with("leaves")
        || name == "water"
        || name == "lava"
        || name == "powder_snow";
    named_transparent || !is_full_cube(outline_shape)
}

/// The protocol entity key shared by observations and incremental entity
/// events.  The connection epoch is part of the identity because Azalea can
/// reuse the same ECS entity on reconnect.
pub(crate) fn protocol_entity_key(connection_epoch: u64, protocol_entity_id: i32) -> String {
    format!("{connection_epoch}:{protocol_entity_id}")
}

/// Normalize an Azalea registry name at the backend boundary.  Both packet
/// producers and ECS snapshots must use this exact helper; `Debug` output is
/// not a registry name (for example, `DarkOakChestBoat` would lose its
/// separators when lowercased).
pub(crate) fn canonical_entity_type(name: &str) -> String {
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

/// 视口扫描热路径上唯一用得到的事实。
///
/// 一次全量投影要问十几万次「这一格挡不挡视线」，而每次问的都只有两位：
/// 是不是空气、透不透光。`ProtocolBlockSnapshot` 为回答这两位携带了
/// `String` + `Vec` + `BTreeMap` 三个持堆字段，光是从缓存里取一份拷贝，
/// 橡木楼梯就要 603 ns（实测见 `examples/viewport_cost.rs`）。
///
/// 这个类型是 `Copy` 的，缓存命中不分配。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockProbe {
    Loaded {
        /// 不是三种空气之一——也就是这一格“有东西”。
        visible: bool,
        transparent_hint: bool,
    },
    Unloaded,
    OutOfWorld,
}

impl BlockProbe {
    /// 从完整 DTO 折出探针。
    ///
    /// 给测试与合成读取器用。生产不该走这里——它已经付过建 DTO 的钱了；
    /// 生产用 [`block_probe`]。
    pub fn from_read(result: &BlockReadResult) -> Self {
        match result {
            BlockReadResult::Loaded { block } => Self::Loaded {
                visible: !is_air_name(&block.name),
                transparent_hint: block.transparent_hint,
            },
            BlockReadResult::Unloaded => Self::Unloaded,
            BlockReadResult::OutOfWorld => Self::OutOfWorld,
        }
    }
}

/// `BlockProbe::Loaded` 的两位，按 `state_id` 预先算好。
#[derive(Clone, Copy)]
struct ProbeFacts {
    visible: bool,
    transparent_hint: bool,
}

/// 按 `state_id` 索引的探针表。
///
/// 两位事实都是 `state_id` 的纯函数，所以整张表首次使用时算一遍就够了。
/// 建表本身不便宜（每个状态要 `Box::from` 一次、`to_aabbs()` 一次），
/// 但它只发生一次；之后每次探测都是一次数组下标。
/// `BlockStateIntegerRepr` 是 `u16`，表最大 64 KiB。
fn probe_table() -> &'static [ProbeFacts] {
    static TABLE: std::sync::OnceLock<Box<[ProbeFacts]>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        (0..=azalea::block::BlockState::MAX_STATE)
            .map(|state_id| {
                let Ok(state) = azalea::block::BlockState::try_from(state_id) else {
                    // 不该发生：迭代范围就是合法区间。真出现了就按最保守的
                    // 「有东西且不透光」处理，宁可少报可见块也不误报。
                    return ProbeFacts {
                        visible: true,
                        transparent_hint: false,
                    };
                };
                let block: Box<dyn azalea::block::BlockTrait> = Box::from(state);
                let name = block.id();
                ProbeFacts {
                    visible: !is_air_name(name),
                    transparent_hint: transparent_hint(name, state.outline_shape()),
                }
            })
            .collect()
    })
}

/// 不建 DTO 直接取探针：一次数组下标，零分配。
pub fn block_probe(state: azalea::block::BlockState) -> BlockProbe {
    let facts = probe_table()[usize::from(state.id())];
    BlockProbe::Loaded {
        visible: facts.visible,
        transparent_hint: facts.transparent_hint,
    }
}

pub fn capture_pose(bot: &Client) -> Option<PoseSnapshot> {
    bot.try_query_self::<(&Position, &Physics, &LookDirection), _>(|(position, physics, look)| {
        PoseSnapshot {
            position: Vec3Value::from_azalea(**position),
            velocity: Vec3Value::from_azalea(physics.velocity),
            yaw: look.y_rot(),
            pitch: look.x_rot(),
            on_ground: physics.on_ground(),
        }
    })
    .ok()
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
) -> Option<MinecraftSnapshotV1> {
    let pose = capture_pose(bot)?;
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

    Some(MinecraftSnapshotV1 {
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
    })
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
                            item_name: canonical_entity_type(&item.kind().to_string()),
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
/// Backwards-compatible capture entry point. The runtime uses the epoch-aware
/// companion below so incremental packet observations share the same key;
/// callers of this public helper retain the historical UUID/ECS fallback key.
pub fn capture_tracked_entities(bot: &Client) -> Vec<ProtocolEntitySnapshot> {
    capture_tracked_entities_impl(bot, None)
}

pub(crate) fn capture_tracked_entities_for_epoch(
    bot: &Client,
    connection_epoch: u64,
) -> Vec<ProtocolEntitySnapshot> {
    capture_tracked_entities_impl(bot, Some(connection_epoch))
}

fn capture_tracked_entities_impl(
    bot: &Client,
    connection_epoch: Option<u64>,
) -> Vec<ProtocolEntitySnapshot> {
    let owner_world = bot
        .try_query_self::<&WorldName, _>(|world_name| world_name.clone())
        .ok();
    let Some(owner_world) = owner_world else {
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
        Option<&Pose>,
    )>();
    let mut entities: Vec<_> = query
        .iter(&ecs)
        .filter_map(
            |(
                entity,
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
                if local.is_some()
                    || !entity_belongs_to_owner(loaded_by, bot.entity, world_name, &owner_world)
                {
                    // 与 MineIntent 的 `bot.entities` 语义一致：自身已经在
                    // MinecraftSnapshotV1.self 中，不重复作为附近实体返回。
                    return None;
                }
                let uuid = uuid.map(|value| (**value).to_string());
                let entity_key = connection_epoch
                    .map(|epoch| protocol_entity_key(epoch, **protocol_entity_id))
                    .unwrap_or_else(|| {
                        uuid.clone()
                            .unwrap_or_else(|| format!("ecs-{}", entity.to_bits()))
                    });
                Some(ProtocolEntitySnapshot {
                    entity_key,
                    protocol_entity_id: **protocol_entity_id,
                    name: None,
                    entity_type: kind
                        .map(|value| canonical_entity_type(&(**value).to_string()))
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

fn entity_belongs_to_owner(
    loaded_by: &LoadedBy,
    owner: azalea::ecs::entity::Entity,
    entity_world: &WorldName,
    owner_world: &WorldName,
) -> bool {
    loaded_by.contains(&owner) && entity_world == owner_world
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

    /// 探针表是绕开 DTO 的近路，所以它必须对**每一个** `BlockState` 都给出
    /// 与走完整 DTO 完全相同的答案。一处不符就是视口会看错世界。
    #[test]
    fn block_probe_agrees_with_the_full_dto_for_every_state() {
        let position = BlockPosition { x: 0, y: 0, z: 0 };
        for state_id in 0..=azalea::block::BlockState::MAX_STATE {
            let Ok(state) = azalea::block::BlockState::try_from(state_id) else {
                continue;
            };
            let full = BlockReadResult::Loaded {
                block: block_snapshot(position.clone(), state),
            };
            assert_eq!(
                block_probe(state),
                BlockProbe::from_read(&full),
                "state_id {state_id} 的探针与完整 DTO 不一致"
            );
        }
    }

    #[test]
    fn registry_names_match_mineintent_local_name_contract() {
        assert_eq!(canonical_entity_type("minecraft:dirt"), "dirt");
        assert_eq!(canonical_entity_type("stone"), "stone");
    }

    #[test]
    fn entity_key_and_type_normalization_match_the_packet_identity_contract() {
        assert_eq!(protocol_entity_key(7, 42), "7:42");
        assert_eq!(
            canonical_entity_type(
                &azalea::registry::builtin::EntityKind::DarkOakChestBoat.to_string()
            ),
            "dark_oak_chest_boat"
        );
    }

    #[test]
    fn capture_membership_excludes_deferred_remove_and_cross_client_entities() {
        let mut ecs = azalea::ecs::world::World::new();
        let owner = ecs.spawn_empty().id();
        let other_client = ecs.spawn_empty().id();
        let owner_world = WorldName::new("minecraft:overworld");
        let other_world = WorldName::new("minecraft:the_nether");
        let mut loaded_by = LoadedBy(std::collections::HashSet::from([owner]));

        assert!(entity_belongs_to_owner(
            &loaded_by,
            owner,
            &owner_world,
            &owner_world,
        ));

        // RemoveEntities removes the owner from LoadedBy before deferred ECS
        // despawn. The entity is still present, but a refresh must not revive
        // it into this client's observation.
        loaded_by.0.remove(&owner);
        assert!(!entity_belongs_to_owner(
            &loaded_by,
            owner,
            &owner_world,
            &owner_world,
        ));

        loaded_by.0.insert(other_client);
        assert!(!entity_belongs_to_owner(
            &loaded_by,
            owner,
            &owner_world,
            &owner_world,
        ));
        assert!(!entity_belongs_to_owner(
            &loaded_by,
            other_client,
            &other_world,
            &owner_world,
        ));
        assert!(entity_belongs_to_owner(
            &loaded_by,
            other_client,
            &owner_world,
            &owner_world,
        ));
    }
}
