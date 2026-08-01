use serde::{Deserialize, Serialize};

use crate::information::geometry::{
    deserialize_finite, deserialize_optional_finite, deserialize_optional_non_null,
    serialize_finite, serialize_optional_finite, Point3,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PerceptionPose {
    pub position: Point3,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub yaw: f64,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub pitch: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PerceptionBlock {
    pub name: String,
    pub visible: bool,
    pub occludes: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PerceptionEntityCandidate {
    #[serde(rename = "type")]
    pub entity_type: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub name: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub username: Option<String>,
    pub position: Point3,
    #[serde(
        default,
        serialize_with = "serialize_optional_finite",
        deserialize_with = "deserialize_optional_finite",
        skip_serializing_if = "Option::is_none"
    )]
    pub width: Option<f64>,
    #[serde(
        default,
        serialize_with = "serialize_optional_finite",
        deserialize_with = "deserialize_optional_finite",
        skip_serializing_if = "Option::is_none"
    )]
    pub height: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PerceptionUnloaded {
    #[serde(rename = "unloaded")]
    Unloaded,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PerceptionBlockAt {
    Block(PerceptionBlock),
    Unloaded(PerceptionUnloaded),
}

pub trait PerceptionPort: Send + Sync {
    fn self_pose(&self) -> PerceptionPose;
    fn revision(&self) -> f64;
    fn block_at(&self, position: Point3) -> PerceptionBlockAt;
    fn nearby_entities(&self) -> Vec<PerceptionEntityCandidate>;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LookedAtBlock {
    pub name: String,
    pub position: Point3,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisibleEntity {
    #[serde(rename = "type")]
    pub entity_type: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub name: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub username: Option<String>,
    pub position: Point3,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub distance: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisibleBlock {
    pub name: String,
    pub position: Point3,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub distance: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisibleEntitiesResult {
    pub entities: Vec<VisibleEntity>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewFrustum {
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub vertical_half_angle: f64,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub horizontal_half_angle: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisibilityPredicate {
    BlockCentre,
    ExposedFace,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisibleBlocksOptions {
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub horizontal_radius: f64,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub vertical_radius: f64,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub max_distance: f64,
    pub frustum: ViewFrustum,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub limit: f64,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub predicate: Option<VisibilityPredicate>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub metrics: Option<ScanMetrics>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScanMetrics {
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub sections_tested: f64,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub sections_skipped: f64,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub voxels_examined: f64,
}
