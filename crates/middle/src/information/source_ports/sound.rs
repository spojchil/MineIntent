use serde::{Deserialize, Serialize};

use crate::information::geometry::{
    deserialize_finite, deserialize_optional_non_null, serialize_finite, RelativeDirection,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SoundObservation {
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub sound_name: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub category: Option<String>,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub distance: f64,
    pub direction: RelativeDirection,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub volume: f64,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub pitch: f64,
    pub observed_at: String,
}

pub trait SoundHistoryPort: Send + Sync {
    fn recent(&self, limit: f64) -> Vec<SoundObservation>;
    fn revision(&self) -> f64;
}
