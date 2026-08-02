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

/// 当前 wire 上允许的 view 模式。
///
/// 这是一个有意闭合的枚举：未来模式可以在这里增加专用分支和校验，但在增加之前
/// 未知字符串必须在反序列化边界失败关闭。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewMode {
    Full,
    Directed,
}

/// Minecraft 方块体素的世界绝对坐标，不携带 backend/world 内部句柄。
pub type ViewPosition = (i32, i32, i32);

/// directed 输入的保守初始批量上限；它是可调的输入预算，不是永久产品裁定。
pub const MAX_DIRECTED_VIEW_POSITIONS: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ViewArguments {
    pub mode: ViewMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub positions: Option<Vec<ViewPosition>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawViewArguments {
    mode: ViewMode,
    #[serde(default, deserialize_with = "deserialize_optional_non_null")]
    positions: Option<Vec<ViewPosition>>,
}

impl<'de> Deserialize<'de> for ViewArguments {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawViewArguments::deserialize(deserializer)?;
        let positions = raw.positions;
        match (raw.mode, positions.as_ref()) {
            (ViewMode::Full, Some(_)) => {
                return Err(serde::de::Error::custom(
                    "full view does not accept positions",
                ));
            }
            (ViewMode::Directed, None) => {
                return Err(serde::de::Error::custom("directed view requires positions"));
            }
            (ViewMode::Directed, Some(positions)) if positions.is_empty() => {
                return Err(serde::de::Error::custom(
                    "directed view requires at least one position",
                ));
            }
            (ViewMode::Directed, Some(positions))
                if positions.len() > MAX_DIRECTED_VIEW_POSITIONS =>
            {
                return Err(serde::de::Error::custom(format!(
                    "directed view accepts at most {MAX_DIRECTED_VIEW_POSITIONS} positions"
                )));
            }
            (ViewMode::Full, None) | (ViewMode::Directed, Some(_)) => {}
        }

        Ok(Self {
            mode: raw.mode,
            positions,
        })
    }
}

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
        "description": "按 mode 读取视觉事实。full 只给出本次读取确认的正面证据；directed 只核对给定的方块体素坐标。",
        "type": "object",
        "properties": {
            "mode": {
                "description": "full 只给出本次读取确认的正面可见证据，结果可能因预算而截断；未列出不表示不可见或不存在。想核对未列出的坐标时使用 directed。directed 对 positions 逐坐标给出可见事实或不可见原因；不可见时绝不返回目标方块的身份或状态。",
                "type": "string",
                "enum": ["full", "directed"]
            },
            "positions": {
                "description": "仅 directed 使用；每项是 Minecraft 方块体素世界绝对坐标的整数三元组 [x, y, z]，不是内部句柄。directed 必须至少给一个坐标；full 不得提供 positions。",
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_DIRECTED_VIEW_POSITIONS,
                "items": {
                    "description": "一个 Minecraft 方块体素坐标 [x, y, z]；每个分量都是 i32 范围内的整数。",
                    "type": "array",
                    "minItems": 3,
                    "maxItems": 3,
                    "items": {
                        "type": "integer",
                        "minimum": i32::MIN,
                        "maximum": i32::MAX
                    }
                }
            }
        },
        "required": ["mode"],
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
