//! 玩家聊天输入解释与 speech 输出契约。
//!
//! 包含纯输入解释、文本分段，以及单 worker 的异步 FIFO speech scheduler。

mod chat_input;
mod contracts;
mod scheduler;
mod segment;

pub use chat_input::interpret_player_chat;
pub use contracts::{
    Addressing, AddressingEvidence, ChatInputContext, PlayerChatMessage, PlayerChatProtocol,
    PlayerChatSender, PlayerChatWorld, SpeechEvent, SpeechRequest, SpeechTransport,
};
pub use scheduler::{
    SpeechEventHandler, SpeechScheduleError, SpeechScheduler, SpeechSchedulerBuildError,
    SpeechSchedulerOptions,
};
pub use segment::{segment_chat, SegmentChatError, DEFAULT_MAX_SEGMENT_LENGTH};
