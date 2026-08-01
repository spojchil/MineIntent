use serde::{Deserialize, Serialize};

use crate::information::geometry::{
    deserialize_finite, deserialize_optional_finite, deserialize_optional_non_null,
    serialize_finite, serialize_optional_finite,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelfExperienceSnapshot {
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub level: f64,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub progress: f64,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub total: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelfEffectSnapshot {
    pub name: String,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub amplifier: f64,
    #[serde(
        default,
        serialize_with = "serialize_optional_finite",
        deserialize_with = "deserialize_optional_finite",
        skip_serializing_if = "Option::is_none"
    )]
    pub duration_ticks: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelfVitalsSnapshot {
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub health: f64,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub food: f64,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub food_saturation: f64,
    #[serde(
        default,
        serialize_with = "serialize_optional_finite",
        deserialize_with = "deserialize_optional_finite",
        skip_serializing_if = "Option::is_none"
    )]
    pub oxygen: Option<f64>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub experience: Option<SelfExperienceSnapshot>,
    pub effects: Vec<SelfEffectSnapshot>,
}

pub trait SelfVitalsPort: Send + Sync {
    fn current(&self) -> SelfVitalsSnapshot;
}
