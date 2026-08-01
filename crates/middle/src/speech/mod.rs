//! 玩家聊天输入解释与 speech 输出契约。
//!
//! 本批只包含纯输入解释与文本分段；异步发送队列、限速和 stop 留给后续批次。

mod chat_input;
mod contracts;
mod segment;

pub use chat_input::interpret_player_chat;
pub use contracts::{
    Addressing, AddressingEvidence, ChatInputContext, PlayerChatMessage, PlayerChatProtocol,
    PlayerChatSender, PlayerChatWorld, SpeechEvent, SpeechRequest, SpeechTransport,
};
pub use segment::{segment_chat, SegmentChatError, DEFAULT_MAX_SEGMENT_LENGTH};
