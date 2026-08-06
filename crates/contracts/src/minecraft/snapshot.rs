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
    /// 角度制。与原版、azalea `LookDirection::y_rot()` 同单位，未归一化：
    /// 连续转身会累加出 −10313 这样的值，要给人或模型看时用 [`wrap_degrees`]。
    pub yaw: f64,
    /// 角度制，原版把它钳在 ±90。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_snapshot_wire_has_no_frame_fact_fields() {
        let fixture = crate::minecraft::fixture_snapshot();
        let value = serde_json::to_value(&fixture).expect("snapshot fixture should serialize");

        assert_eq!(value["protocol"], "mineintent.minecraft.snapshot.v1");
        assert!(value.get("armor").is_none());
        assert!(value.get("light").is_none());
        assert_eq!(
            serde_json::from_value::<MinecraftSnapshotV1>(value).expect("fixture should roundtrip"),
            fixture
        );
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
    /// 角度制，未归一化。见 [`SelfSnapshot::yaw`]。
    pub yaw: f64,
    /// 角度制，钳在 ±90。
    pub pitch: f64,
    /// 角度制，未归一化。
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
    /// 角度制，未归一化。见 [`SelfSnapshot::yaw`]。
    pub yaw: f64,
    /// 角度制，钳在 ±90。
    pub pitch: f64,
}

/// 把角度收进 [−180, 180)，等价于原版 `Mth.wrapDegrees`。
///
/// 原版和 azalea 都不归一化 yaw：一直往同一个方向转，`y_rot` 会一路累加下去。
/// 内部算三角函数无所谓（周期函数），但**给模型看的角度必须归一化**——实盘里
/// 模型收到过 −10313.24°，它无从判断那和 126.76° 是同一个朝向，于是认定
/// 「转向没有生效」。原版自己在 F3 界面上显示的也是归一化后的值。
///
/// 非有限数原样返回：缺陷不该在这里被伪装成一个像样的角度，由调用方的有限性
/// 检查去处理。
pub fn wrap_degrees(value: f64) -> f64 {
    if !value.is_finite() {
        return value;
    }
    let mut wrapped = value % 360.0;
    if wrapped >= 180.0 {
        wrapped -= 360.0;
    }
    if wrapped < -180.0 {
        wrapped += 360.0;
    }
    // −0.0 与 0.0 是同一个朝向，但序列化出来是 "-0.0"，读的人会以为有区别。
    if wrapped == 0.0 {
        0.0
    } else {
        wrapped
    }
}

#[cfg(test)]
mod wrap_degrees_tests {
    use super::wrap_degrees;

    #[test]
    fn wraps_into_the_vanilla_half_open_range() {
        for (input, expected) in [
            (0.0, 0.0),
            (179.9, 179.9),
            (180.0, -180.0),
            (-180.0, -180.0),
            (360.0, 0.0),
            (-360.0, 0.0),
            (-270.0, 90.0),
            (450.0, 90.0),
        ] {
            assert_eq!(wrap_degrees(input), expected, "input {input}");
        }
    }

    #[test]
    fn wraps_the_value_the_live_run_actually_produced() {
        // 2026-08-05 实盘：模型收到的是 −10313.24°。它和 126.76° 是同一个朝向。
        let wrapped = wrap_degrees(-10_313.240_312_354_817);
        assert!(
            (wrapped - 126.759_687_645_183).abs() < 1e-9,
            "wrapped = {wrapped}"
        );
    }

    #[test]
    fn a_non_finite_angle_passes_through_instead_of_becoming_a_plausible_one() {
        assert!(wrap_degrees(f64::NAN).is_nan());
        assert_eq!(wrap_degrees(f64::INFINITY), f64::INFINITY);
    }

    #[test]
    fn negative_zero_is_reported_as_zero() {
        assert!(wrap_degrees(-0.0).is_sign_positive());
        assert!(wrap_degrees(-360.0).is_sign_positive());
    }
}
