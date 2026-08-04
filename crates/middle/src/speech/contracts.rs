use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressingEvidence {
    ExplicitName,
    ExplicitReply,
    SingleParty,
    OngoingConversation,
    NotAddressed,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum PlayerChatProtocol {
    #[default]
    #[serde(rename = "mineintent.player-chat.v1")]
    V1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlayerChatSender {
    pub username: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Addressing {
    pub addressed_to_participant: bool,
    pub evidence: Vec<AddressingEvidence>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlayerChatWorld {
    pub world_id: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub dimension: Option<String>,
    pub connection_epoch: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlayerChatMessage {
    pub protocol: PlayerChatProtocol,
    pub source_event_id: String,
    pub occurred_at: String,
    pub sender: PlayerChatSender,
    pub text: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub verified: Option<bool>,
    pub addressing: Addressing,
    pub world: PlayerChatWorld,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ChatInputContext {
    pub participant_username: String,
    pub online_player_usernames: Vec<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub conversation_active_with: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SpeechRequest {
    pub id: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case", tag = "type")]
pub enum SpeechEvent {
    Scheduled {
        #[serde(rename = "requestId")]
        request_id: String,
        segments: usize,
    },
    Sent {
        #[serde(rename = "requestId")]
        request_id: String,
        segment: usize,
        text: String,
    },
    Cancelled {
        #[serde(rename = "requestId")]
        request_id: String,
        reason: String,
    },
    Failed {
        #[serde(rename = "requestId")]
        request_id: String,
        reason: String,
    },
}

/// Synchronous output port used by [`super::SpeechScheduler`]. Transport failures become
/// `SpeechEvent::Failed` events and do not stop later queued requests.
pub trait SpeechTransport: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    fn send(&self, message: &str) -> Result<(), Self::Error>;
}

fn deserialize_optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
