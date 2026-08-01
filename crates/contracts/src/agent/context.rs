use serde::{ser::Error as _, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use super::tool::AgentContextProtocol;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlayerMessage {
    pub username: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentWorld {
    pub dimension: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        serialize_with = "serialize_optional_finite",
        skip_serializing_if = "Option::is_none"
    )]
    pub time_of_day: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentSelf {
    #[serde(serialize_with = "serialize_finite_position")]
    pub position: [f64; 3],
    #[serde(serialize_with = "serialize_finite")]
    pub yaw_degrees: f64,
    #[serde(serialize_with = "serialize_finite")]
    pub pitch_degrees: f64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentEvent {
    pub r#type: String,
    pub summary: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(
    bound(
        deserialize = "Status: Deserialize<'de>, Inventory: Deserialize<'de>, Sound: Deserialize<'de>, Omission: Deserialize<'de>",
        serialize = "Status: Serialize, Inventory: Serialize, Sound: Serialize, Omission: Serialize"
    ),
    deny_unknown_fields,
    rename_all = "camelCase"
)]
pub struct AgentFrame<Status = Value, Inventory = Value, Sound = Value, Omission = Value> {
    pub at: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub player: Option<PlayerMessage>,
    pub world: AgentWorld,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        rename = "self",
        skip_serializing_if = "Option::is_none"
    )]
    pub self_state: Option<AgentSelf>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub status: Option<Status>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub inventory: Option<Inventory>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub sound: Option<Sound>,
    pub events: Vec<AgentEvent>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub omitted_events: Option<u64>,
    pub omissions: Vec<Omission>,
}

pub type JsonAgentFrame = AgentFrame<Value, Value, Value, Value>;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StableContext<Memories = Value> {
    pub memories: Memories,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(
    bound(
        deserialize = "Memories: Deserialize<'de>, Status: Deserialize<'de>, Inventory: Deserialize<'de>, Sound: Deserialize<'de>, Omission: Deserialize<'de>",
        serialize = "Memories: Serialize, Status: Serialize, Inventory: Serialize, Sound: Serialize, Omission: Serialize"
    ),
    deny_unknown_fields,
    rename_all = "camelCase"
)]
pub struct AgentDecisionContext<
    Memories = Value,
    Status = Value,
    Inventory = Value,
    Sound = Value,
    Omission = Value,
> {
    pub protocol: AgentContextProtocol,
    pub stable: StableContext<Memories>,
    pub frame: AgentFrame<Status, Inventory, Sound, Omission>,
}

pub type JsonAgentDecisionContext = AgentDecisionContext<Value, Value, Value, Value, Value>;

pub(super) fn deserialize_optional_non_null<'de, D, T>(
    deserializer: D,
) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)?.map_or_else(
        || Err(serde::de::Error::custom("explicit null is not allowed")),
        |value| Ok(Some(value)),
    )
}

fn serialize_finite<S>(value: &f64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if value.is_finite() {
        serializer.serialize_f64(*value)
    } else {
        Err(S::Error::custom("agent frame numbers must be finite"))
    }
}

fn serialize_optional_finite<S>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(value) => serialize_finite(value, serializer),
        None => serializer.serialize_none(),
    }
}

fn serialize_finite_position<S>(value: &[f64; 3], serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if value.iter().all(|coordinate| coordinate.is_finite()) {
        value.serialize(serializer)
    } else {
        Err(S::Error::custom("agent frame numbers must be finite"))
    }
}
