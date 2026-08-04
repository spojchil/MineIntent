use std::collections::BTreeMap;

use serde::{
    de::Error as _,
    ser::{Error as _, SerializeMap, SerializeStruct, SerializeTuple},
    Deserialize, Deserializer, Serialize, Serializer,
};
use serde_json::Value;

use crate::information::{InformationOmission, SoundValues};

use super::tool::AgentContextProtocol;

/// The v3 discriminator is part of the context type, rather than an independent
/// value that can be paired with an arbitrary stable shape.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AgentContextProtocolV3;

impl AgentContextProtocolV3 {
    pub const WIRE: &'static str = "mineintent.agent-context.v3";
}

impl Serialize for AgentContextProtocolV3 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(Self::WIRE)
    }
}

impl<'de> Deserialize<'de> for AgentContextProtocolV3 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        (value == Self::WIRE)
            .then_some(Self)
            .ok_or_else(|| D::Error::custom("expected mineintent.agent-context.v3"))
    }
}

impl PartialEq<AgentContextProtocol> for AgentContextProtocolV3 {
    fn eq(&self, other: &AgentContextProtocol) -> bool {
        matches!(other, AgentContextProtocol::V3)
    }
}

impl PartialEq<AgentContextProtocolV3> for AgentContextProtocol {
    fn eq(&self, other: &AgentContextProtocolV3) -> bool {
        other == self
    }
}

/// The v4 discriminator is bound to [`StableContextV4`] by
/// [`AgentDecisionContextV4`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AgentContextProtocolV4;

impl AgentContextProtocolV4 {
    pub const WIRE: &'static str = "mineintent.agent-context.v4";
}

impl Serialize for AgentContextProtocolV4 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(Self::WIRE)
    }
}

impl<'de> Deserialize<'de> for AgentContextProtocolV4 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        (value == Self::WIRE)
            .then_some(Self)
            .ok_or_else(|| D::Error::custom("expected mineintent.agent-context.v4"))
    }
}

impl PartialEq<AgentContextProtocol> for AgentContextProtocolV4 {
    fn eq(&self, other: &AgentContextProtocol) -> bool {
        matches!(other, AgentContextProtocol::V4)
    }
}

impl PartialEq<AgentContextProtocolV4> for AgentContextProtocol {
    fn eq(&self, other: &AgentContextProtocolV4) -> bool {
        other == self
    }
}

/// v5 discriminator. The v3/v4 protocol types remain independent and replayable.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AgentContextProtocolV5;

impl AgentContextProtocolV5 {
    pub const WIRE: &'static str = "mineintent.agent-context.v5";
}

impl Serialize for AgentContextProtocolV5 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(Self::WIRE)
    }
}

impl<'de> Deserialize<'de> for AgentContextProtocolV5 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        (value == Self::WIRE)
            .then_some(Self)
            .ok_or_else(|| D::Error::custom("expected mineintent.agent-context.v5"))
    }
}

impl PartialEq<AgentContextProtocol> for AgentContextProtocolV5 {
    fn eq(&self, other: &AgentContextProtocol) -> bool {
        matches!(other, AgentContextProtocol::V5)
    }
}

impl PartialEq<AgentContextProtocolV5> for AgentContextProtocol {
    fn eq(&self, other: &AgentContextProtocolV5) -> bool {
        other == self
    }
}

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
pub struct StableContextV3<Memories = Value> {
    pub memories: Memories,
}

/// The v4 stable section is one complete memory document.  Keeping this as a
/// separate type prevents a generic v3 envelope from being re-used with the
/// v4 `memory` field (and vice versa).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StableContextV4 {
    pub memory: String,
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
pub struct AgentDecisionContextV3<
    Memories = Value,
    Status = Value,
    Inventory = Value,
    Sound = Value,
    Omission = Value,
> {
    pub protocol: AgentContextProtocolV3,
    pub stable: StableContextV3<Memories>,
    pub frame: AgentFrame<Status, Inventory, Sound, Omission>,
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
pub struct AgentDecisionContextV4<
    Status = Value,
    Inventory = Value,
    Sound = Value,
    Omission = Value,
> {
    pub protocol: AgentContextProtocolV4,
    pub stable: StableContextV4,
    pub frame: AgentFrame<Status, Inventory, Sound, Omission>,
}

/// Backward-compatible public v3 name. This alias resolves only to the v3
/// envelope; v4 remains [`AgentDecisionContextV4`] with its own stable type.
pub type AgentDecisionContext<
    Memories = Value,
    Status = Value,
    Inventory = Value,
    Sound = Value,
    Omission = Value,
> = AgentDecisionContextV3<Memories, Status, Inventory, Sound, Omission>;

/// Backward-compatible public v3 stable-section name.
pub type StableContext<Memories = Value> = StableContextV3<Memories>;

pub type JsonAgentDecisionContext = AgentDecisionContextV3<Value, Value, Value, Value, Value>;
pub type JsonAgentDecisionContextV4 = AgentDecisionContextV4<Value, Value, Value, Value>;

/// v5 keeps the v4 stable section byte-for-byte: one complete memory document.
pub type StableContextV5 = StableContextV4;

/// The model-visible pose in the v5 opening frame.  `self` is intentionally not
/// reused: the v5 wire names this section `pose`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentPoseV5 {
    #[serde(
        deserialize_with = "deserialize_finite_position",
        serialize_with = "serialize_finite_position"
    )]
    pub position: [f64; 3],
    #[serde(
        deserialize_with = "deserialize_finite",
        serialize_with = "serialize_finite"
    )]
    pub yaw_degrees: f64,
    #[serde(
        deserialize_with = "deserialize_finite",
        serialize_with = "serialize_finite"
    )]
    pub pitch_degrees: f64,
}

/// Health and food are always present when the optional status section is
/// present.  Armor is an original-game armor-point integer; zero is represented
/// by omission rather than by an explicit JSON zero.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentStatusV5 {
    pub health: f64,
    pub food: f64,
    pub armor: Option<u8>,
}

impl AgentStatusV5 {
    pub fn validate(&self) -> Result<(), String> {
        validate_health(self.health)?;
        validate_food(self.food)?;
        if let Some(armor) = self.armor {
            validate_armor(armor)?;
        }
        Ok(())
    }
}

impl Serialize for AgentStatusV5 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(S::Error::custom)?;
        let mut state = serializer
            .serialize_struct("AgentStatusV5", if self.armor.is_some() { 3 } else { 2 })?;
        state.serialize_field("health", &self.health)?;
        state.serialize_field("food", &self.food)?;
        if let Some(armor) = self.armor {
            state.serialize_field("armor", &armor)?;
        }
        state.end()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawAgentStatusV5 {
    #[serde(deserialize_with = "deserialize_health")]
    health: f64,
    #[serde(deserialize_with = "deserialize_food")]
    food: f64,
    #[serde(default, deserialize_with = "deserialize_optional_non_null")]
    armor: Option<u8>,
}

impl<'de> Deserialize<'de> for AgentStatusV5 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawAgentStatusV5::deserialize(deserializer)?;
        let status = Self {
            health: raw.health,
            food: raw.food,
            armor: raw.armor,
        };
        status.validate().map_err(D::Error::custom)?;
        Ok(status)
    }
}

pub const MAX_HOTBAR_SLOT: u8 = 8;

/// Compact `[itemName, count]` hotbar/off-hand tuple.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentItemStackV5(pub String, pub u32);

impl AgentItemStackV5 {
    pub fn new(item_name: impl Into<String>, count: u32) -> Result<Self, String> {
        let value = Self(item_name.into(), count);
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.0.is_empty() || self.0.chars().any(char::is_control) {
            return Err(
                "hotbar item name must be non-empty and free of control characters".to_owned(),
            );
        }
        if self.1 == 0 {
            return Err("hotbar item count must be a positive u32".to_owned());
        }
        Ok(())
    }
}

impl Serialize for AgentItemStackV5 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(S::Error::custom)?;
        let mut tuple = serializer.serialize_tuple(2)?;
        tuple.serialize_element(&self.0)?;
        tuple.serialize_element(&self.1)?;
        tuple.end()
    }
}

impl<'de> Deserialize<'de> for AgentItemStackV5 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (item_name, count) = <(String, u32)>::deserialize(deserializer)?;
        let value = Self(item_name, count);
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

/// Sparse nine-slot hotbar.  Numeric Rust keys are rendered as the exact JSON
/// strings `"0"` through `"8"`; empty slots are absent from the map.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentHotbarV5 {
    pub selected: u8,
    pub slots: BTreeMap<u8, AgentItemStackV5>,
    pub off_hand: Option<AgentItemStackV5>,
}

impl AgentHotbarV5 {
    pub fn validate(&self) -> Result<(), String> {
        if self.selected > MAX_HOTBAR_SLOT {
            return Err("hotbar selected must be between 0 and 8".to_owned());
        }
        for (slot, item) in &self.slots {
            if *slot > MAX_HOTBAR_SLOT {
                return Err("hotbar slot must be between 0 and 8".to_owned());
            }
            item.validate()?;
        }
        if let Some(item) = &self.off_hand {
            item.validate()?;
        }
        Ok(())
    }
}

impl Serialize for AgentHotbarV5 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(S::Error::custom)?;
        let mut map =
            serializer.serialize_map(Some(if self.off_hand.is_some() { 3 } else { 2 }))?;
        map.serialize_entry("selected", &self.selected)?;
        map.serialize_entry("slots", &HotbarSlots(&self.slots))?;
        if let Some(item) = &self.off_hand {
            map.serialize_entry("offHand", item)?;
        }
        map.end()
    }
}

struct HotbarSlots<'a>(&'a BTreeMap<u8, AgentItemStackV5>);

impl Serialize for HotbarSlots<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (slot, item) in self.0 {
            if *slot > MAX_HOTBAR_SLOT {
                return Err(S::Error::custom("hotbar slot must be between 0 and 8"));
            }
            map.serialize_entry(&slot.to_string(), item)?;
        }
        map.end()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawAgentHotbarV5 {
    selected: u8,
    slots: BTreeMap<String, AgentItemStackV5>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null")]
    off_hand: Option<AgentItemStackV5>,
}

impl<'de> Deserialize<'de> for AgentHotbarV5 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawAgentHotbarV5::deserialize(deserializer)?;
        let mut slots = BTreeMap::new();
        for (key, item) in raw.slots {
            let slot = parse_hotbar_slot(&key).map_err(D::Error::custom)?;
            if slots.insert(slot, item).is_some() {
                return Err(D::Error::custom(
                    "hotbar slots contain duplicate coordinates",
                ));
            }
        }
        let value = Self {
            selected: raw.selected,
            slots,
            off_hand: raw.off_hand,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AgentChatMovedMarkerV5 {
    #[serde(rename = "events")]
    Events,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentChatMessageV5 {
    #[serde(
        deserialize_with = "deserialize_non_empty_string",
        serialize_with = "serialize_non_empty_string"
    )]
    pub username: String,
    pub text: String,
    #[serde(
        deserialize_with = "deserialize_non_empty_string",
        serialize_with = "serialize_non_empty_string"
    )]
    pub at: String,
}

impl AgentChatMessageV5 {
    pub fn validate(&self) -> Result<(), String> {
        validate_text(&self.username, "chat username")?;
        validate_text(&self.at, "chat at")?;
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentChatMovedV5 {
    #[serde(
        deserialize_with = "deserialize_non_empty_string",
        serialize_with = "serialize_non_empty_string"
    )]
    pub username: String,
    #[serde(
        deserialize_with = "deserialize_non_empty_string",
        serialize_with = "serialize_non_empty_string"
    )]
    pub at: String,
    pub moved: AgentChatMovedMarkerV5,
}

impl AgentChatMovedV5 {
    pub fn validate(&self) -> Result<(), String> {
        validate_text(&self.username, "chat username")?;
        validate_text(&self.at, "chat at")?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AgentChatItemV5 {
    Message(AgentChatMessageV5),
    Moved(AgentChatMovedV5),
}

impl Serialize for AgentChatItemV5 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Message(message) => message.serialize(serializer),
            Self::Moved(moved) => moved.serialize(serializer),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawAgentChatItemV5 {
    #[serde(
        deserialize_with = "deserialize_non_empty_string",
        serialize_with = "serialize_non_empty_string"
    )]
    username: String,
    #[serde(default, deserialize_with = "deserialize_optional_non_null")]
    text: Option<String>,
    #[serde(
        deserialize_with = "deserialize_non_empty_string",
        serialize_with = "serialize_non_empty_string"
    )]
    at: String,
    #[serde(default, deserialize_with = "deserialize_optional_non_null")]
    moved: Option<AgentChatMovedMarkerV5>,
}

impl<'de> Deserialize<'de> for AgentChatItemV5 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawAgentChatItemV5::deserialize(deserializer)?;
        match (raw.text, raw.moved) {
            (Some(text), None) => Ok(Self::Message(AgentChatMessageV5 {
                username: raw.username,
                text,
                at: raw.at,
            })),
            (None, Some(moved)) => Ok(Self::Moved(AgentChatMovedV5 {
                username: raw.username,
                at: raw.at,
                moved,
            })),
            (Some(_), Some(_)) => Err(D::Error::custom(
                "chat item text and moved are mutually exclusive",
            )),
            (None, None) => Err(D::Error::custom("chat item requires text or moved")),
        }
    }
}

pub const MAX_CHAT_ITEMS_V5: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub struct AgentChatV5 {
    pub items: Vec<AgentChatItemV5>,
    pub omitted: u64,
}

impl AgentChatV5 {
    pub fn validate(&self) -> Result<(), String> {
        if self.items.is_empty() || self.items.len() > MAX_CHAT_ITEMS_V5 {
            return Err(format!(
                "chat items must contain between 1 and {MAX_CHAT_ITEMS_V5} entries"
            ));
        }
        for item in &self.items {
            match item {
                AgentChatItemV5::Message(message) => message.validate()?,
                AgentChatItemV5::Moved(moved) => moved.validate()?,
            }
        }
        Ok(())
    }
}

impl Serialize for AgentChatV5 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(S::Error::custom)?;
        let mut state = serializer.serialize_struct("AgentChatV5", 2)?;
        state.serialize_field("items", &self.items)?;
        state.serialize_field("omitted", &self.omitted)?;
        state.end()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawAgentChatV5 {
    items: Vec<AgentChatItemV5>,
    omitted: u64,
}

impl<'de> Deserialize<'de> for AgentChatV5 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawAgentChatV5::deserialize(deserializer)?;
        let value = Self {
            items: raw.items,
            omitted: raw.omitted,
        };
        value.validate().map_err(D::Error::custom)?;
        Ok(value)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AgentEventV5 {
    PlayerChat(AgentChatMessageV5),
    Summary { event_type: String, summary: String },
}

impl AgentEventV5 {
    pub fn player_chat(message: AgentChatMessageV5) -> Self {
        Self::PlayerChat(message)
    }

    pub fn summary(event_type: impl Into<String>, summary: impl Into<String>) -> Self {
        Self::Summary {
            event_type: event_type.into(),
            summary: summary.into(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::PlayerChat(message) => message.validate(),
            Self::Summary {
                event_type,
                summary,
            } => {
                validate_text(event_type, "event type")?;
                validate_text(summary, "event summary")?;
                if event_type == "player_chat" {
                    return Err("ordinary event type must not be player_chat".to_owned());
                }
                Ok(())
            }
        }
    }
}

impl Serialize for AgentEventV5 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(S::Error::custom)?;
        match self {
            Self::PlayerChat(message) => {
                let mut state = serializer.serialize_struct("AgentEventV5", 4)?;
                state.serialize_field("type", "player_chat")?;
                state.serialize_field("username", &message.username)?;
                state.serialize_field("text", &message.text)?;
                state.serialize_field("at", &message.at)?;
                state.end()
            }
            Self::Summary {
                event_type,
                summary,
            } => {
                let mut state = serializer.serialize_struct("AgentEventV5", 3)?;
                state.serialize_field("type", event_type)?;
                state.serialize_field("summary", summary)?;
                state.end()
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawAgentEventV5 {
    #[serde(deserialize_with = "deserialize_non_empty_string")]
    r#type: String,
    #[serde(default, deserialize_with = "deserialize_optional_non_null")]
    username: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null")]
    text: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null")]
    at: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_non_null")]
    summary: Option<String>,
}

impl<'de> Deserialize<'de> for AgentEventV5 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawAgentEventV5::deserialize(deserializer)?;
        if raw.r#type == "player_chat" {
            if raw.summary.is_some() {
                return Err(D::Error::custom(
                    "player_chat event must not contain summary",
                ));
            }
            let message = AgentChatMessageV5 {
                username: raw
                    .username
                    .ok_or_else(|| D::Error::missing_field("username"))?,
                text: raw.text.ok_or_else(|| D::Error::missing_field("text"))?,
                at: raw.at.ok_or_else(|| D::Error::missing_field("at"))?,
            };
            let event = Self::PlayerChat(message);
            event.validate().map_err(D::Error::custom)?;
            Ok(event)
        } else {
            if raw.username.is_some() || raw.text.is_some() || raw.at.is_some() {
                return Err(D::Error::custom(
                    "ordinary event must contain only type and summary",
                ));
            }
            let event = Self::Summary {
                event_type: raw.r#type,
                summary: raw
                    .summary
                    .ok_or_else(|| D::Error::missing_field("summary"))?,
            };
            event.validate().map_err(D::Error::custom)?;
            Ok(event)
        }
    }
}

/// Strict v5 opening frame.  No `player`, `self`, `inventory`, `timeOfDay`,
/// `effects`, or viewport field exists in this type.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentFrameV5 {
    #[serde(
        deserialize_with = "deserialize_non_empty_string",
        serialize_with = "serialize_non_empty_string"
    )]
    pub at: String,
    pub world: AgentWorldV5,
    pub pose: AgentPoseV5,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub status: Option<AgentStatusV5>,
    pub hotbar: AgentHotbarV5,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub chat: Option<AgentChatV5>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_sound",
        serialize_with = "serialize_optional_sound",
        skip_serializing_if = "Option::is_none"
    )]
    pub sound: Option<SoundValues>,
    #[serde(
        deserialize_with = "deserialize_light",
        serialize_with = "serialize_light"
    )]
    pub light: u8,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_empty_vec",
        serialize_with = "serialize_optional_non_empty_vec",
        skip_serializing_if = "Option::is_none"
    )]
    pub events: Option<Vec<AgentEventV5>>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_empty_omissions",
        serialize_with = "serialize_optional_non_empty_omissions",
        skip_serializing_if = "Option::is_none"
    )]
    pub omissions: Option<Vec<InformationOmission>>,
}

/// v5 world intentionally has only dimension.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentWorldV5 {
    #[serde(
        deserialize_with = "deserialize_non_empty_string",
        serialize_with = "serialize_non_empty_string"
    )]
    pub dimension: String,
}

pub type JsonAgentFrameV5 = AgentFrameV5;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentDecisionContextV5 {
    pub protocol: AgentContextProtocolV5,
    pub stable: StableContextV5,
    pub frame: AgentFrameV5,
}

pub type JsonAgentDecisionContextV5 = AgentDecisionContextV5;

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

fn deserialize_finite<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = f64::deserialize(deserializer)?;
    value
        .is_finite()
        .then_some(value)
        .ok_or_else(|| D::Error::custom("agent frame numbers must be finite"))
}

fn deserialize_finite_position<'de, D>(deserializer: D) -> Result<[f64; 3], D::Error>
where
    D: Deserializer<'de>,
{
    let value = <[f64; 3]>::deserialize(deserializer)?;
    value
        .iter()
        .all(|coordinate| coordinate.is_finite())
        .then_some(value)
        .ok_or_else(|| D::Error::custom("agent frame numbers must be finite"))
}

fn serialize_non_empty_string<S>(value: &String, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(S::Error::custom(
            "agent frame strings must be non-empty and free of control characters",
        ));
    }
    serializer.serialize_str(value)
}

fn deserialize_non_empty_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    validate_text(&value, "agent frame string").map_err(D::Error::custom)?;
    Ok(value)
}

fn validate_text(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().any(char::is_control) {
        Err(format!(
            "{label} must be non-empty and free of control characters"
        ))
    } else {
        Ok(())
    }
}

fn deserialize_health<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = f64::deserialize(deserializer)?;
    validate_health(value).map_err(D::Error::custom)?;
    Ok(value)
}

fn validate_health(value: f64) -> Result<(), String> {
    if value.is_finite() {
        Ok(())
    } else {
        Err("status health must be finite".to_owned())
    }
}

fn deserialize_food<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = f64::deserialize(deserializer)?;
    validate_food(value).map_err(D::Error::custom)?;
    Ok(value)
}

fn validate_food(value: f64) -> Result<(), String> {
    if value.is_finite() {
        Ok(())
    } else {
        Err("status food must be finite".to_owned())
    }
}

fn validate_armor(value: u8) -> Result<(), String> {
    if (1..=20).contains(&value) {
        Ok(())
    } else {
        Err("status armor must be an integer between 1 and 20 when present".to_owned())
    }
}

fn parse_hotbar_slot(value: &str) -> Result<u8, String> {
    if !matches!(value, "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8") {
        return Err("hotbar slot keys must be exactly strings \"0\" through \"8\"".to_owned());
    }
    value
        .parse::<u8>()
        .map_err(|_| "hotbar slot key is not a valid slot".to_owned())
}

fn serialize_light<S>(value: &u8, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if *value > 15 {
        return Err(S::Error::custom(
            "light must be an integer between 0 and 15",
        ));
    }
    serializer.serialize_u8(*value)
}

fn deserialize_light<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u8::deserialize(deserializer)?;
    if value > 15 {
        return Err(D::Error::custom(
            "light must be an integer between 0 and 15",
        ));
    }
    Ok(value)
}

fn value_contains_null(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.iter().any(value_contains_null),
        Value::Object(object) => object.values().any(value_contains_null),
        _ => false,
    }
}

fn validate_sound_values(value: &SoundValues) -> Result<(), String> {
    let Some(sounds) = value.recent_sounds.as_ref() else {
        return Err("sound requires a non-empty recentSounds collection".to_owned());
    };
    if sounds.is_empty() {
        return Err("sound requires a non-empty recentSounds collection".to_owned());
    }
    for sound in sounds {
        if !sound.distance.is_finite() || !sound.volume.is_finite() || !sound.pitch.is_finite() {
            return Err("sound numeric fields must be finite".to_owned());
        }
    }
    Ok(())
}

fn serialize_optional_sound<S>(
    value: &Option<SoundValues>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(value) => {
            validate_sound_values(value).map_err(S::Error::custom)?;
            value.serialize(serializer)
        }
        None => serializer.serialize_none(),
    }
}

fn deserialize_optional_sound<'de, D>(deserializer: D) -> Result<Option<SoundValues>, D::Error>
where
    D: Deserializer<'de>,
{
    let value: Option<Value> = deserialize_optional_non_null(deserializer)?;
    let Some(value) = value else {
        return Err(D::Error::custom("explicit null is not allowed"));
    };
    if value_contains_null(&value) {
        return Err(D::Error::custom("sound must not contain explicit null"));
    }
    let sound = serde_json::from_value::<SoundValues>(value).map_err(D::Error::custom)?;
    validate_sound_values(&sound).map_err(D::Error::custom)?;
    Ok(Some(sound))
}

fn serialize_optional_non_empty_vec<S, T>(
    value: &Option<Vec<T>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: Serialize,
{
    match value {
        Some(items) if !items.is_empty() => items.serialize(serializer),
        Some(_) => Err(S::Error::custom(
            "empty frame collections must be represented by an absent segment",
        )),
        None => serializer.serialize_none(),
    }
}

fn deserialize_optional_non_empty_vec<'de, D, T>(
    deserializer: D,
) -> Result<Option<Vec<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let value: Option<Vec<T>> = deserialize_optional_non_null(deserializer)?;
    match value {
        Some(items) if !items.is_empty() => Ok(Some(items)),
        Some(_) => Err(D::Error::custom(
            "empty frame collections must be represented by an absent segment",
        )),
        None => Err(D::Error::custom("explicit null is not allowed")),
    }
}

fn serialize_optional_non_empty_omissions<S>(
    value: &Option<Vec<InformationOmission>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(items) if !items.is_empty() => {
            let json = serde_json::to_value(items).map_err(S::Error::custom)?;
            if value_contains_null(&json) {
                return Err(S::Error::custom(
                    "information omissions must not contain explicit null",
                ));
            }
            items.serialize(serializer)
        }
        Some(_) => Err(S::Error::custom(
            "empty frame collections must be represented by an absent segment",
        )),
        None => serializer.serialize_none(),
    }
}

fn deserialize_optional_non_empty_omissions<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<InformationOmission>>, D::Error>
where
    D: Deserializer<'de>,
{
    let value: Option<Value> = deserialize_optional_non_null(deserializer)?;
    let Some(value) = value else {
        return Err(D::Error::custom("explicit null is not allowed"));
    };
    if value_contains_null(&value) {
        return Err(D::Error::custom(
            "information omissions must not contain explicit null",
        ));
    }
    let omissions =
        serde_json::from_value::<Vec<InformationOmission>>(value).map_err(D::Error::custom)?;
    if omissions.is_empty() {
        return Err(D::Error::custom(
            "empty frame collections must be represented by an absent segment",
        ));
    }
    Ok(Some(omissions))
}
