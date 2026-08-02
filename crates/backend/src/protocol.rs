use chrono::{DateTime, Utc};
use serde::{de::Error as DeserializeError, Deserialize, Serialize};
use serde_json::Value;

/// 后端事件跨进程边界的版本号。
pub const BACKEND_EVENT_PROTOCOL: &str = "mineintent.minecraft.backend-event.v1";
/// 后端命令跨进程边界的版本号。
pub const BACKEND_COMMAND_PROTOCOL: &str = "mineintent.minecraft.backend-command.v1";

pub fn now_utc() -> DateTime<Utc> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间早于 Unix epoch")
        .as_millis() as i64;
    DateTime::<Utc>::from_timestamp_millis(millis).expect("系统时间超出 chrono 支持范围")
}

/// 一条数据的来源。预测数据永远不能伪装成服务端事实。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactSource {
    Commanded,
    ClientPredicted,
    ServerObserved,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendEventKind {
    Lifecycle,
    KeepAlive,
    Chat,
    SelfState,
    World,
    PlayerList,
    SnapshotChanged,
    Entity,
    Block,
    Sound,
    Motor,
    Error,
}

/// 对齐 `MinecraftBackendApi` 事件语义的严格版本化信封。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackendEventEnvelope {
    pub protocol: String,
    pub id: String,
    pub kind: BackendEventKind,
    pub occurred_at: DateTime<Utc>,
    pub process_session_id: String,
    pub connection_epoch: u64,
    pub connection_attempt_id: String,
    pub world_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimension: Option<String>,
    pub source: FactSource,
    pub payload: Value,
}

impl BackendEventEnvelope {
    pub fn new(
        id: impl Into<String>,
        kind: BackendEventKind,
        process_session_id: impl Into<String>,
        connection_epoch: u64,
        connection_attempt_id: impl Into<String>,
        world_id: impl Into<String>,
        source: FactSource,
        payload: Value,
    ) -> Self {
        Self::new_with_dimension(
            id,
            kind,
            process_session_id,
            connection_epoch,
            connection_attempt_id,
            world_id,
            None,
            source,
            payload,
        )
    }

    pub fn new_with_dimension(
        id: impl Into<String>,
        kind: BackendEventKind,
        process_session_id: impl Into<String>,
        connection_epoch: u64,
        connection_attempt_id: impl Into<String>,
        world_id: impl Into<String>,
        dimension: Option<String>,
        source: FactSource,
        payload: Value,
    ) -> Self {
        Self {
            protocol: BACKEND_EVENT_PROTOCOL.to_owned(),
            id: id.into(),
            kind,
            occurred_at: now_utc(),
            process_session_id: process_session_id.into(),
            connection_epoch,
            connection_attempt_id: connection_attempt_id.into(),
            world_id: world_id.into(),
            dimension,
            source,
            payload,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MotorDirection {
    Forward,
    Back,
    Left,
    Right,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackendCommand {
    SendChat {
        message: String,
    },
    LookRelative {
        #[serde(rename = "yawDegrees")]
        yaw_degrees: f32,
        #[serde(rename = "pitchDegrees")]
        pitch_degrees: f32,
    },
    Move {
        directions: Vec<MotorDirection>,
        #[serde(rename = "durationMs")]
        duration_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        sprint: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        jump: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        crouch: Option<bool>,
    },
    ReleaseAll,
    /// 显式请求服务端重生；运行时不在死亡事件上自动调用它。
    Respawn,
}

impl<'de> Deserialize<'de> for BackendCommand {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // serde 对 internally-tagged enum 的未知字段处理并不稳定；这里按
        // 每个命令建立严格 wire struct，确保命令边界不会静默吞掉拼写错误。
        let value = Value::deserialize(deserializer)?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| D::Error::custom("命令缺少字符串字段 type"))?;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct SendChatWire {
            #[serde(rename = "type")]
            _kind: String,
            message: String,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct LookRelativeWire {
            #[serde(rename = "type")]
            _kind: String,
            #[serde(rename = "yawDegrees", alias = "yaw_degrees")]
            yaw_degrees: f32,
            #[serde(rename = "pitchDegrees", alias = "pitch_degrees")]
            pitch_degrees: f32,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct MoveWire {
            #[serde(rename = "type")]
            _kind: String,
            directions: Vec<MotorDirection>,
            #[serde(rename = "durationMs", alias = "duration_ms")]
            duration_ms: u64,
            sprint: Option<bool>,
            jump: Option<bool>,
            crouch: Option<bool>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ReleaseAllWire {
            #[serde(rename = "type")]
            _kind: String,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RespawnWire {
            #[serde(rename = "type")]
            _kind: String,
        }

        match kind {
            "send_chat" => {
                let wire: SendChatWire = serde_json::from_value(value)
                    .map_err(|error| D::Error::custom(error.to_string()))?;
                Ok(Self::SendChat {
                    message: wire.message,
                })
            }
            "look_relative" => {
                let wire: LookRelativeWire = serde_json::from_value(value)
                    .map_err(|error| D::Error::custom(error.to_string()))?;
                Ok(Self::LookRelative {
                    yaw_degrees: wire.yaw_degrees,
                    pitch_degrees: wire.pitch_degrees,
                })
            }
            "move" => {
                let wire: MoveWire = serde_json::from_value(value)
                    .map_err(|error| D::Error::custom(error.to_string()))?;
                Ok(Self::Move {
                    directions: wire.directions,
                    duration_ms: wire.duration_ms,
                    sprint: wire.sprint,
                    jump: wire.jump,
                    crouch: wire.crouch,
                })
            }
            "release_all" => {
                let wire: ReleaseAllWire = serde_json::from_value(value)
                    .map_err(|error| D::Error::custom(error.to_string()))?;
                let _ = wire._kind;
                Ok(Self::ReleaseAll)
            }
            "respawn" => {
                let wire: RespawnWire = serde_json::from_value(value)
                    .map_err(|error| D::Error::custom(error.to_string()))?;
                let _ = wire._kind;
                Ok(Self::Respawn)
            }
            other => Err(D::Error::custom(format!("未知命令类型：{other}"))),
        }
    }
}

/// 跨进程输入的严格信封。未知字段会被拒绝，避免静默接受错误命令。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackendCommandEnvelope {
    pub protocol: String,
    pub id: String,
    #[serde(alias = "issued_at")]
    pub issued_at: DateTime<Utc>,
    pub command: BackendCommand,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_round_trip_keeps_fact_source() {
        let event = BackendEventEnvelope::new(
            "event-1",
            BackendEventKind::KeepAlive,
            "session-1",
            1,
            "attempt-1",
            "world-1",
            FactSource::ServerObserved,
            serde_json::json!({"id": 42}),
        );
        let encoded = serde_json::to_string(&event).expect("事件应能编码");
        assert!(encoded.contains("processSessionId"));
        assert!(!encoded.contains("dimension"));
        let decoded: BackendEventEnvelope = serde_json::from_str(&encoded).expect("事件应能解码");
        assert_eq!(decoded.source, FactSource::ServerObserved);
        assert_eq!(decoded.payload["id"], 42);
        assert_eq!(decoded.protocol, BACKEND_EVENT_PROTOCOL);
    }

    #[test]
    fn event_dimension_is_captured_without_changing_the_v1_discriminator() {
        let event = BackendEventEnvelope::new_with_dimension(
            "event-2",
            BackendEventKind::Lifecycle,
            "session-1",
            1,
            "attempt-1",
            "world-1",
            Some("minecraft:overworld".to_owned()),
            FactSource::ServerObserved,
            serde_json::json!({"type":"transport_connected"}),
        );
        let encoded = serde_json::to_value(&event).expect("event should encode");
        assert_eq!(encoded["protocol"], BACKEND_EVENT_PROTOCOL);
        assert_eq!(encoded["dimension"], "minecraft:overworld");
        assert_eq!(event.protocol, BACKEND_EVENT_PROTOCOL);
    }

    #[test]
    fn command_rejects_unknown_keys() {
        let input = r#"{
            "protocol":"mineintent.minecraft.backend-command.v1",
            "id":"command-1",
            "issuedAt":"2026-08-01T00:00:00Z",
            "command":{"type":"release_all","unexpected":true}
        }"#;
        assert!(serde_json::from_str::<BackendCommandEnvelope>(input).is_err());
    }

    #[test]
    fn command_uses_camel_case_wire_fields() {
        let command = BackendCommand::Move {
            directions: vec![MotorDirection::Forward],
            duration_ms: 250,
            sprint: Some(false),
            jump: Some(false),
            crouch: Some(false),
        };
        let encoded = serde_json::to_value(&command).expect("命令应能编码");
        assert_eq!(encoded["durationMs"], 250);
        assert!(encoded.get("duration_ms").is_none());
        let decoded: BackendCommand = serde_json::from_value(encoded).expect("命令应能解码");
        assert!(matches!(
            decoded,
            BackendCommand::Move {
                duration_ms: 250,
                ..
            }
        ));
    }

    #[test]
    fn respawn_command_round_trips_as_explicit_wire_action() {
        let encoded = serde_json::to_value(BackendCommand::Respawn).expect("重生命令应能编码");
        assert_eq!(encoded["type"], "respawn");
        let decoded: BackendCommand = serde_json::from_value(encoded).expect("重生命令应能解码");
        assert!(matches!(decoded, BackendCommand::Respawn));
    }
}
