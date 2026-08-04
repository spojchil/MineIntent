use serde::{de::DeserializeOwned, de::Error as _, Deserialize, Serialize};

use super::{
    BackendClose, BackendFailure, ProtocolBlockSnapshot, ProtocolEntitySnapshot, Vec3Value,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendEventProtocol {
    #[default]
    #[serde(rename = "mineintent.minecraft.backend-event.v2")]
    V2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactSource {
    Commanded,
    ClientPredicted,
    ServerObserved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendEventKind {
    Lifecycle,
    #[serde(rename = "self")]
    SelfState,
    World,
    Entity,
    Block,
    Sound,
    Chat,
    PlayerList,
    SnapshotChanged,
    Overflow,
}

impl BackendEventKind {
    pub const PRODUCT_KINDS: [Self; 9] = [
        Self::Lifecycle,
        Self::SelfState,
        Self::World,
        Self::Entity,
        Self::Block,
        Self::Sound,
        Self::Chat,
        Self::PlayerList,
        Self::SnapshotChanged,
    ];

    pub fn is_product_kind(self) -> bool {
        self != Self::Overflow
    }
}

/// Backend event v2 信封。
///
/// `dimension` 是事件发生时的事实快照，不得由订阅者用稍后的 snapshot 回填；只有
/// 尚未进入世界时才允许为 `None`。`source` 是必需的内部 provenance，模型可见
/// schema 不得直接暴露它。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackendEventEnvelope<T = BackendEventPayload> {
    pub protocol: BackendEventProtocol,
    pub id: String,
    pub kind: BackendEventKind,
    /// RFC 3339 timestamp.
    pub occurred_at: String,
    pub process_session_id: String,
    pub connection_epoch: u64,
    pub connection_attempt_id: String,
    pub world_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimension: Option<String>,
    pub source: FactSource,
    pub payload: T,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackendEventEnvelopeWire {
    protocol: BackendEventProtocol,
    id: String,
    kind: BackendEventKind,
    occurred_at: String,
    process_session_id: String,
    connection_epoch: u64,
    connection_attempt_id: String,
    world_id: String,
    #[serde(default)]
    dimension: Option<String>,
    source: FactSource,
    payload: serde_json::Value,
}

impl<'de, T> Deserialize<'de> for BackendEventEnvelope<T>
where
    T: DeserializeOwned,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = BackendEventEnvelopeWire::deserialize(deserializer)?;
        let payload_value = raw.payload;
        let strict_payload = serde_json::from_value::<BackendEventPayload>(payload_value.clone())
            .map_err(|error| {
            D::Error::custom(format!("invalid strict v2 event payload: {error}"))
        })?;
        if strict_payload.kind() != raw.kind {
            return Err(D::Error::custom(format!(
                "event kind {:?} does not match payload kind {:?}",
                raw.kind,
                strict_payload.kind()
            )));
        }
        let payload = serde_json::from_value::<T>(payload_value).map_err(|error| {
            D::Error::custom(format!("invalid typed v2 event payload: {error}"))
        })?;
        Ok(Self {
            protocol: raw.protocol,
            id: raw.id,
            kind: raw.kind,
            occurred_at: raw.occurred_at,
            process_session_id: raw.process_session_id,
            connection_epoch: raw.connection_epoch,
            connection_attempt_id: raw.connection_attempt_id,
            world_id: raw.world_id,
            dimension: raw.dimension,
            source: raw.source,
            payload,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendEventMetadata {
    pub id: String,
    pub occurred_at: String,
    pub process_session_id: String,
    pub connection_epoch: u64,
    pub connection_attempt_id: String,
    pub world_id: String,
    /// Must be captured when the event occurs.
    pub dimension: Option<String>,
}

impl<T> BackendEventEnvelope<T> {
    pub fn new(
        metadata: BackendEventMetadata,
        kind: BackendEventKind,
        source: FactSource,
        payload: T,
    ) -> Self {
        Self {
            protocol: BackendEventProtocol::V2,
            id: metadata.id,
            kind,
            occurred_at: metadata.occurred_at,
            process_session_id: metadata.process_session_id,
            connection_epoch: metadata.connection_epoch,
            connection_attempt_id: metadata.connection_attempt_id,
            world_id: metadata.world_id,
            dimension: metadata.dimension,
            source,
            payload,
        }
    }

    pub fn map_payload<U>(self, map: impl FnOnce(T) -> U) -> BackendEventEnvelope<U> {
        BackendEventEnvelope {
            protocol: self.protocol,
            id: self.id,
            kind: self.kind,
            occurred_at: self.occurred_at,
            process_session_id: self.process_session_id,
            connection_epoch: self.connection_epoch,
            connection_attempt_id: self.connection_attempt_id,
            world_id: self.world_id,
            dimension: self.dimension,
            source: self.source,
            payload: map(self.payload),
        }
    }
}

impl BackendEventEnvelope<BackendEventPayload> {
    pub fn from_payload(
        metadata: BackendEventMetadata,
        source: FactSource,
        payload: BackendEventPayload,
    ) -> Self {
        let kind = payload.kind();
        Self::new(metadata, kind, source, payload)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackendLifecyclePayload {
    ConnectionRequested {
        attempt: u32,
    },
    TransportConnected,
    LoggedIn {
        version: String,
        dimension: String,
    },
    Ready {
        #[serde(rename = "snapshotRevision")]
        snapshot_revision: u64,
    },
    Died,
    RespawnTransitionStarted {
        #[serde(rename = "fromDimension")]
        from_dimension: String,
    },
    Respawned {
        dimension: String,
    },
    DimensionChanged {
        from: String,
        to: String,
    },
    ReconnectScheduled {
        attempt: u32,
        #[serde(rename = "retryAt")]
        retry_at: String,
        #[serde(rename = "closeCode")]
        close_code: String,
    },
    ConnectionClosed {
        close: BackendClose,
    },
    Faulted {
        failure: BackendFailure,
    },
    Stopped {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProtocolEntityEvent {
    Spawned {
        entity: ProtocolEntitySnapshot,
    },
    Moved {
        entity: ProtocolEntitySnapshot,
    },
    Updated {
        entity: ProtocolEntitySnapshot,
        changed: Vec<String>,
    },
    Animation {
        #[serde(rename = "entityKey")]
        entity_key: String,
        animation: String,
    },
    Hurt {
        #[serde(rename = "entityKey")]
        entity_key: String,
        #[serde(
            rename = "possibleSourceEntityKey",
            skip_serializing_if = "Option::is_none"
        )]
        possible_source_entity_key: Option<String>,
    },
    Removed {
        #[serde(rename = "entityKey")]
        entity_key: String,
        last: ProtocolEntitySnapshot,
        reason: EntityRemovalReason,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityRemovalReason {
    ProtocolRemoved,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProtocolBlockEvent {
    Updated {
        #[serde(
            rename = "oldBlock",
            deserialize_with = "deserialize_required_nullable"
        )]
        old_block: Option<ProtocolBlockSnapshot>,
        #[serde(
            rename = "newBlock",
            deserialize_with = "deserialize_required_nullable"
        )]
        new_block: Option<ProtocolBlockSnapshot>,
    },
    ChunkLoaded {
        #[serde(rename = "chunkX")]
        chunk_x: i32,
        #[serde(rename = "chunkZ")]
        chunk_z: i32,
    },
    ChunkUnloaded {
        #[serde(rename = "chunkX")]
        chunk_x: i32,
        #[serde(rename = "chunkZ")]
        chunk_z: i32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolSoundSource {
    NamedSoundEffect,
    SoundEffect,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolSoundPayload {
    #[serde(rename = "type")]
    pub event_type: HeardSoundType,
    pub sound_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sound_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sound_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub source_position: Vec3Value,
    pub volume: f64,
    pub pitch: f64,
    pub protocol_source: ProtocolSoundSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeardSoundType {
    #[serde(rename = "heard")]
    Heard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatPosition {
    Chat,
    System,
    GameInfo,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolChatEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_username: Option<String>,
    pub plain_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<ChatPosition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProtocolSelfEvent {
    ServerPositionCorrection {
        #[serde(rename = "teleportId")]
        teleport_id: u32,
        position: Vec3Value,
        velocity: Vec3Value,
        yaw: f32,
        pitch: f32,
        relative: RelativeMovementFlags,
    },
}

fn deserialize_required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelativeMovementFlags {
    pub x: bool,
    pub y: bool,
    pub z: bool,
    pub yaw: bool,
    pub pitch: bool,
    pub delta_x: bool,
    pub delta_y: bool,
    pub delta_z: bool,
    pub rotate_delta: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProtocolWorldEvent {
    GameChanged {
        #[serde(skip_serializing_if = "Option::is_none")]
        dimension: Option<String>,
        #[serde(rename = "gameMode")]
        #[serde(skip_serializing_if = "Option::is_none")]
        game_mode: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProtocolPlayerListEvent {
    #[serde(rename = "player_list_add")]
    Add { uuid: String, username: String },
    #[serde(rename = "player_list_remove")]
    Remove { uuid: String, username: String },
    #[serde(rename = "player_list_update")]
    Update { uuid: String, username: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolSnapshotChangedEvent {
    pub group: String,
    pub snapshot_revision: u64,
}

/// Bounded subscribers receive this marker after reconstructable facts were coalesced/dropped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackendOverflowPayload {
    #[serde(rename = "type")]
    pub event_type: OverflowType,
    pub dropped_count: u64,
    pub dropped_kinds: Vec<BackendEventKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OverflowType {
    #[serde(rename = "overflow")]
    Overflow,
}

/// Strictly typed payload union for the global v2 stream.
///
/// The envelope's custom deserializer also checks that `kind` agrees with this
/// union, so a valid payload cannot be relabelled as another event kind.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum BackendEventPayload {
    Lifecycle(BackendLifecyclePayload),
    SelfState(ProtocolSelfEvent),
    World(ProtocolWorldEvent),
    Entity(ProtocolEntityEvent),
    Block(ProtocolBlockEvent),
    Sound(ProtocolSoundPayload),
    Chat(ProtocolChatEvent),
    PlayerList(ProtocolPlayerListEvent),
    SnapshotChanged(ProtocolSnapshotChangedEvent),
    Overflow(BackendOverflowPayload),
}

fn parse_strict_payload<T, E>(value: serde_json::Value, allowed: &[&str]) -> Result<T, E>
where
    T: DeserializeOwned,
    E: serde::de::Error,
{
    let object = value
        .as_object()
        .ok_or_else(|| E::custom("event payload must be an object"))?;
    if let Some(unknown) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(E::custom(format!("unknown event payload field: {unknown}")));
    }
    serde_json::from_value(value).map_err(|error| E::custom(error.to_string()))
}

impl<'de> Deserialize<'de> for BackendEventPayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| D::Error::custom("event payload must be an object"))?;
        let event_type = object.get("type").and_then(serde_json::Value::as_str);

        macro_rules! parse {
            ($type:ty, $allowed:expr, $variant:ident) => {
                ({
                    let allowed = $allowed;
                    Ok(Self::$variant(parse_strict_payload::<$type, D::Error>(
                        value, &allowed,
                    )?))
                })
            };
        }

        match event_type {
            Some(
                "connection_requested"
                | "transport_connected"
                | "logged_in"
                | "ready"
                | "died"
                | "respawn_transition_started"
                | "respawned"
                | "dimension_changed"
                | "reconnect_scheduled"
                | "connection_closed"
                | "faulted"
                | "stopped",
            ) => {
                let allowed = match event_type.expect("matched lifecycle discriminator") {
                    "connection_requested" => &["type", "attempt"][..],
                    "transport_connected" | "died" => &["type"][..],
                    "logged_in" => &["type", "version", "dimension"][..],
                    "ready" => &["type", "snapshotRevision"][..],
                    "respawn_transition_started" => &["type", "fromDimension"][..],
                    "respawned" => &["type", "dimension"][..],
                    "dimension_changed" => &["type", "from", "to"][..],
                    "reconnect_scheduled" => &["type", "attempt", "retryAt", "closeCode"][..],
                    "connection_closed" => &["type", "close"][..],
                    "faulted" => &["type", "failure"][..],
                    "stopped" => &["type", "reason"][..],
                    _ => unreachable!(),
                };
                parse!(BackendLifecyclePayload, allowed, Lifecycle)
            }
            Some("server_position_correction") => parse!(
                ProtocolSelfEvent,
                [
                    "type",
                    "teleportId",
                    "position",
                    "velocity",
                    "yaw",
                    "pitch",
                    "relative"
                ],
                SelfState
            ),
            Some("game_changed") => {
                parse!(ProtocolWorldEvent, ["type", "dimension", "gameMode"], World)
            }
            Some("player_list_add" | "player_list_remove" | "player_list_update") => parse!(
                ProtocolPlayerListEvent,
                ["type", "uuid", "username"],
                PlayerList
            ),
            Some("updated")
                if object.contains_key("oldBlock") || object.contains_key("newBlock") =>
            {
                parse!(ProtocolBlockEvent, ["type", "oldBlock", "newBlock"], Block)
            }
            Some("spawned" | "moved" | "updated" | "animation" | "hurt" | "removed") => {
                let allowed = match event_type.expect("matched entity discriminator") {
                    "spawned" | "moved" => &["type", "entity"][..],
                    "updated" => &["type", "entity", "changed"][..],
                    "animation" => &["type", "entityKey", "animation"][..],
                    "hurt" => &["type", "entityKey", "possibleSourceEntityKey"][..],
                    "removed" => &["type", "entityKey", "last", "reason"][..],
                    _ => unreachable!(),
                };
                parse!(ProtocolEntityEvent, allowed, Entity)
            }
            Some("chunk_loaded" | "chunk_unloaded") => {
                let allowed = match event_type.expect("matched block discriminator") {
                    "chunk_loaded" | "chunk_unloaded" => &["type", "chunkX", "chunkZ"][..],
                    "updated" => &["type", "oldBlock", "newBlock"][..],
                    _ => unreachable!(),
                };
                parse!(ProtocolBlockEvent, allowed, Block)
            }
            Some("heard") => parse!(
                ProtocolSoundPayload,
                [
                    "type",
                    "soundKey",
                    "soundName",
                    "soundId",
                    "category",
                    "sourcePosition",
                    "volume",
                    "pitch",
                    "protocolSource"
                ],
                Sound
            ),
            Some("overflow") => parse!(
                BackendOverflowPayload,
                ["type", "droppedCount", "droppedKinds"],
                Overflow
            ),
            None if object.contains_key("plainText") => parse!(
                ProtocolChatEvent,
                ["senderUsername", "plainText", "position", "verified"],
                Chat
            ),
            None if object.contains_key("group") || object.contains_key("snapshotRevision") => {
                parse!(
                    ProtocolSnapshotChangedEvent,
                    ["group", "snapshotRevision"],
                    SnapshotChanged
                )
            }
            Some(other) => Err(D::Error::custom(format!(
                "unknown event payload discriminator: {other}"
            ))),
            None => Err(D::Error::custom(
                "event payload is missing a recognized discriminator",
            )),
        }
    }
}

impl BackendEventPayload {
    pub fn kind(&self) -> BackendEventKind {
        match self {
            Self::Lifecycle(_) => BackendEventKind::Lifecycle,
            Self::SelfState(_) => BackendEventKind::SelfState,
            Self::World(_) => BackendEventKind::World,
            Self::Entity(_) => BackendEventKind::Entity,
            Self::Block(_) => BackendEventKind::Block,
            Self::Sound(_) => BackendEventKind::Sound,
            Self::Chat(_) => BackendEventKind::Chat,
            Self::PlayerList(_) => BackendEventKind::PlayerList,
            Self::SnapshotChanged(_) => BackendEventKind::SnapshotChanged,
            Self::Overflow(_) => BackendEventKind::Overflow,
        }
    }
}
