use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Map, Value};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MoveDirection {
    Forward,
    Back,
    Left,
    Right,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MoveInputArguments {
    pub directions: Vec<MoveDirection>,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sprint: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMoveInputArguments {
    directions: Vec<MoveDirection>,
    duration_ms: u64,
    #[serde(default, deserialize_with = "deserialize_optional_non_null")]
    sprint: Option<bool>,
}

impl<'de> Deserialize<'de> for MoveInputArguments {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawMoveInputArguments::deserialize(deserializer)?;
        if !(1..=4).contains(&raw.directions.len()) {
            return Err(serde::de::Error::custom(
                "directions must contain 1..=4 movement keys",
            ));
        }
        let mut unique = HashSet::with_capacity(raw.directions.len());
        if !raw
            .directions
            .iter()
            .copied()
            .all(|direction| unique.insert(direction))
        {
            return Err(serde::de::Error::custom("movement keys must be unique"));
        }
        if !(50..=1_500).contains(&raw.duration_ms) {
            return Err(serde::de::Error::custom(
                "duration_ms must be an integer from 50 through 1500",
            ));
        }

        Ok(Self {
            directions: raw.directions,
            duration_ms: raw.duration_ms,
            sprint: raw.sprint,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ViewArguments {}

pub fn move_input_parameters_schema() -> Map<String, Value> {
    object(json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {
            "directions": {
                "description": "同时按住的移动键，方向相对当前朝向；斜走时把两个键放在这里。",
                "minItems": 1,
                "maxItems": 4,
                "type": "array",
                "items": {
                    "type": "string",
                    "enum": ["forward", "back", "left", "right"]
                },
                "uniqueItems": true
            },
            "duration_ms": {
                "description": "整组移动键共同按住的时长，毫秒。步行大约每 250 毫秒走一格。",
                "minimum": 50,
                "maximum": 1500,
                "type": "integer"
            },
            "sprint": {
                "description": "是否同时按住疾跑；同样时长内走得更远。",
                "type": "boolean"
            }
        },
        "required": ["directions", "duration_ms"],
        "additionalProperties": false
    }))
}

pub fn view_parameters_schema() -> Map<String, Value> {
    object(json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {},
        "additionalProperties": false
    }))
}

fn deserialize_optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)?.map_or_else(
        || Err(serde::de::Error::custom("explicit null is not allowed")),
        |value| Ok(Some(value)),
    )
}

fn object(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .cloned()
        .expect("frozen JSON schema is an object")
}
