use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{BackendError, TARGET_MINECRAFT_VERSION, TARGET_PROTOCOL_VERSION};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotProtocol {
    #[default]
    #[serde(rename = "mineintent.minecraft.snapshot.v1")]
    V1,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vec3Value {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockPosition {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

pub type AabbValue = [f64; 6];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameMode {
    Survival,
    Creative,
    Adventure,
    Spectator,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Difficulty {
    Peaceful,
    Easy,
    Normal,
    Hard,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorldSnapshot {
    pub world_id: String,
    pub dimension: String,
    pub minecraft_version: String,
    pub protocol_version: u32,
    pub game_mode: GameMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<Difficulty>,
    pub min_y: i32,
    pub height: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_view_distance: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_of_day: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_raining: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatusEffectSnapshot {
    pub name: String,
    pub amplifier: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ticks: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExperienceSnapshot {
    pub level: u32,
    pub progress: f64,
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelfSnapshot {
    pub entity_key: String,
    pub username: String,
    pub position: Vec3Value,
    pub velocity: Vec3Value,
    /// Canonical snapshot/observation angle in radians.
    pub yaw: f64,
    /// Canonical snapshot/observation angle in radians.
    pub pitch: f64,
    pub on_ground: bool,
    pub alive: bool,
    pub health: f64,
    pub food: f64,
    pub food_saturation: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oxygen: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experience: Option<ExperienceSnapshot>,
    pub effects: Vec<StatusEffectSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InventorySlotSnapshot {
    pub slot: u32,
    pub item_name: String,
    pub count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub durability_used: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InventorySnapshot {
    pub selected_hotbar_slot: u8,
    pub slots: Vec<InventorySlotSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrackedPlayerSnapshot {
    pub player_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    pub username: String,
    pub listed: bool,
    pub entity_tracked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Vec3Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yaw: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pitch: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub held_item_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MinecraftSnapshotV1 {
    pub protocol: SnapshotProtocol,
    pub snapshot_revision: u64,
    pub lifecycle_revision: u64,
    /// RFC 3339 timestamp.
    pub captured_at: String,
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
    pub fn validate_target_axes(&self) -> Result<(), BackendError> {
        if self.world.minecraft_version != TARGET_MINECRAFT_VERSION {
            return Err(BackendError::UnsupportedVersion {
                expected: TARGET_MINECRAFT_VERSION.to_owned(),
                actual: self.world.minecraft_version.clone(),
            });
        }
        if self.world.protocol_version != TARGET_PROTOCOL_VERSION {
            return Err(BackendError::InvalidConfig {
                field: "world.protocolVersion".to_owned(),
                message: format!("must equal {TARGET_PROTOCOL_VERSION}"),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntityEquipmentSnapshot {
    pub slot: u32,
    pub item_name: String,
    pub count: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// Canonical angle in radians.
    pub yaw: f64,
    /// Canonical angle in radians.
    pub pitch: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_yaw: Option<f64>,
    pub width: f64,
    pub height: f64,
    pub on_ground: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pose: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub held_item_name: Option<String>,
    pub equipment: Vec<EntityEquipmentSnapshot>,
    pub valid: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BlockPropertyValue {
    String(String),
    Integer(i64),
    Number(f64),
    Boolean(bool),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolBlockSnapshot {
    pub position: BlockPosition,
    pub name: String,
    pub state_id: u32,
    pub properties: BTreeMap<String, BlockPropertyValue>,
    pub collision_shapes: Vec<AabbValue>,
    /// Authoritative protocol/registry fact; adapters must not infer this from the block name.
    pub transparent_hint: bool,
    pub bounding_box: BlockBoundingBox,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockBoundingBox {
    Block,
    Empty,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum BlockReadResult {
    Loaded { block: ProtocolBlockSnapshot },
    Unloaded,
    OutOfWorld,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelfPose {
    pub position: Vec3Value,
    pub velocity: Vec3Value,
    /// Radians.
    pub yaw: f64,
    /// Radians.
    pub pitch: f64,
}
