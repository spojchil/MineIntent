use serde::{Deserialize, Serialize};

use super::FactSource;

pub type WorldPosition = [f64; 3];
pub type VisibleBlockTuple = (String, i32, i32, i32);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewportCoordinateSystem {
    #[default]
    #[serde(rename = "minecraft_world_absolute")]
    MinecraftWorldAbsolute,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportFrame {
    pub coordinates: ViewportCoordinateSystem,
    #[serde(rename = "self")]
    pub self_pose: ViewportSelfPose,
    pub legend: ViewportLegend,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportSelfPose {
    pub position: WorldPosition,
    /// Degrees, unlike snapshot and observation pose angles.
    pub yaw_degrees: f64,
    /// Degrees, unlike snapshot and observation pose angles.
    pub pitch_degrees: f64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportLegend {
    pub visible_entities: String,
    pub visible_blocks: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportBlock {
    pub name: String,
    pub position: WorldPosition,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisibleEntityView {
    #[serde(rename = "type")]
    pub entity_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player: Option<String>,
    pub position: WorldPosition,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisibleEntitiesView {
    pub items: Vec<VisibleEntityView>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisibleBlocksView {
    pub blocks: Vec<VisibleBlockTuple>,
    pub truncated: bool,
}

/// backend 唯一 viewport kernel 的完整、一次性投影结果。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportProjection {
    pub frame: ViewportFrame,
    pub standing_on_block: Option<ViewportBlock>,
    pub looked_at_block: Option<ViewportBlock>,
    pub visible_entities: VisibleEntitiesView,
    pub visible_blocks: VisibleBlocksView,
}

/// 原子读取：三项必须来自同一次 backend capture，不允许 middle 分别读取。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportRead {
    pub projection: ViewportProjection,
    pub source: FactSource,
    pub revision: u64,
}
