use serde::{Deserialize, Serialize};

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

/// Backend event v2 信封。
///
/// `dimension` 是事件发生时的事实快照，不得由订阅者用稍后的 snapshot 回填；只有
/// 尚未进入世界时才允许为 `None`。`source` 是必需的内部 provenance，模型可见
/// schema 不得直接暴露它。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackendEventEnvelope<T = serde_json::Value> {
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
        #[serde(rename = "oldBlock")]
        old_block: Option<ProtocolBlockSnapshot>,
        #[serde(rename = "newBlock")]
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
