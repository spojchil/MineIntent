use serde::{Deserialize, Serialize};

use crate::information::geometry::{
    deserialize_finite, deserialize_optional_finite, serialize_finite, serialize_optional_finite,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InventorySlotSnapshot {
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub slot: f64,
    pub item_name: String,
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub count: f64,
    #[serde(
        default,
        serialize_with = "serialize_optional_finite",
        deserialize_with = "deserialize_optional_finite",
        skip_serializing_if = "Option::is_none"
    )]
    pub metadata: Option<f64>,
    #[serde(
        default,
        serialize_with = "serialize_optional_finite",
        deserialize_with = "deserialize_optional_finite",
        skip_serializing_if = "Option::is_none"
    )]
    pub durability_used: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InventoryStateSnapshot {
    #[serde(
        serialize_with = "serialize_finite",
        deserialize_with = "deserialize_finite"
    )]
    pub selected_hotbar_slot: f64,
    pub slots: Vec<InventorySlotSnapshot>,
}

pub trait InventoryPort: Send + Sync {
    fn current(&self) -> InventoryStateSnapshot;
}
