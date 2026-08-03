use std::{collections::BTreeMap, fmt};

use mineintent_contracts::agent::{
    AgentChatItemV5, AgentChatMessageV5, AgentChatMovedMarkerV5, AgentChatMovedV5, AgentChatV5,
    AgentContextProtocolV5, AgentDecisionContextV5, AgentEventV5, AgentFrameV5, AgentHotbarV5,
    AgentPoseV5, AgentStatusV5, AgentWorldV5, JsonAgentDecisionContextV5, StableContextV5,
};
use mineintent_contracts::information::{InformationOmission, SoundValues};

/// Explicit, fake-backed inputs for the v5 frame assembler.  No backend or
/// Participant object is hidden here; a later runtime can populate this value
/// from its own registered segment sources.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentContextV5Input {
    pub memory: String,
    pub at: String,
    pub dimension: String,
    pub pose: AgentPoseV5,
    pub status: Option<AgentStatusV5>,
    pub hotbar: AgentHotbarV5,
    /// Unread chat in old-to-new order.  The assembler retains the newest
    /// eight entries and reports the number dropped before that window.
    pub unread_chat: Vec<AgentChatInputV5>,
    /// Records evicted by the source's bounded history before this input.
    pub unread_chat_omitted: u64,
    pub sound: Option<SoundValues>,
    pub light: u8,
    /// Cross-run/registered events, including an optional trigger chat.
    pub events: Vec<AgentContextV5EventInput>,
    pub omissions: Vec<InformationOmission>,
    /// Stable internal identity for the triggering chat.  The sequence and
    /// identity never cross the model-visible JSON boundary.
    pub trigger_chat: Option<AgentChatTriggerV5>,
}

/// Internal identity used while assembling a frame.  The sequence is an
/// input-side correlation key only; it is never serialized into the v5 wire.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentChatInputV5 {
    pub sequence: u64,
    pub message: AgentChatMessageV5,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentChatTriggerV5 {
    pub sequence: u64,
    pub message: AgentChatMessageV5,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AgentContextV5EventInput {
    PlayerChat {
        sequence: u64,
        message: AgentChatMessageV5,
    },
    Summary {
        event_type: String,
        summary: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentContextV5AssemblyError {
    InvalidInput(String),
    TriggerChatNotInUnreadWindow,
    DuplicateTriggerChat,
    DuplicatePlayerChatEvent,
    DuplicateChatSequence,
    PlayerChatEventDoesNotMatchUnread,
}

impl fmt::Display for AgentContextV5AssemblyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message) => formatter.write_str(message),
            Self::TriggerChatNotInUnreadWindow => {
                formatter.write_str("trigger chat sequence must occur in unread chat")
            }
            Self::DuplicateTriggerChat => {
                formatter.write_str("trigger player_chat event must occur exactly once")
            }
            Self::DuplicatePlayerChatEvent => {
                formatter.write_str("the same player_chat event must not occur twice")
            }
            Self::DuplicateChatSequence => {
                formatter.write_str("unread chat sequence must be unique")
            }
            Self::PlayerChatEventDoesNotMatchUnread => {
                formatter.write_str("player_chat event sequence must match unread chat")
            }
        }
    }
}

impl std::error::Error for AgentContextV5AssemblyError {}

#[derive(Clone, Copy, Debug, Default)]
pub struct AgentContextV5Assembler;

impl AgentContextV5Assembler {
    pub fn assemble(
        &self,
        input: AgentContextV5Input,
    ) -> Result<JsonAgentDecisionContextV5, AgentContextV5AssemblyError> {
        let AgentContextV5Input {
            memory,
            at,
            dimension,
            pose,
            status,
            hotbar,
            unread_chat,
            unread_chat_omitted,
            sound,
            light,
            events,
            omissions,
            trigger_chat,
        } = input;

        let mut chat_by_sequence = BTreeMap::new();
        for (index, chat) in unread_chat.iter().enumerate() {
            if chat_by_sequence.insert(chat.sequence, index).is_some() {
                return Err(AgentContextV5AssemblyError::DuplicateChatSequence);
            }
            chat.message
                .validate()
                .map_err(AgentContextV5AssemblyError::InvalidInput)?;
        }

        if let Some(trigger) = trigger_chat.as_ref() {
            let Some(index) = chat_by_sequence.get(&trigger.sequence).copied() else {
                return Err(AgentContextV5AssemblyError::TriggerChatNotInUnreadWindow);
            };
            if unread_chat[index].message != trigger.message {
                return Err(AgentContextV5AssemblyError::TriggerChatNotInUnreadWindow);
            }
        }

        let retained_start = unread_chat.len().saturating_sub(8);

        let mut player_chat_events = BTreeMap::new();
        let mut output_events =
            Vec::with_capacity(events.len() + usize::from(trigger_chat.is_some()));
        for event in events {
            match event {
                AgentContextV5EventInput::PlayerChat { sequence, message } => {
                    if player_chat_events
                        .insert(sequence, message.clone())
                        .is_some()
                    {
                        return Err(AgentContextV5AssemblyError::DuplicatePlayerChatEvent);
                    }
                    let Some(index) = chat_by_sequence.get(&sequence).copied() else {
                        return Err(AgentContextV5AssemblyError::PlayerChatEventDoesNotMatchUnread);
                    };
                    if unread_chat[index].message != message {
                        return Err(AgentContextV5AssemblyError::PlayerChatEventDoesNotMatchUnread);
                    }
                    output_events.push(AgentEventV5::player_chat(message));
                }
                AgentContextV5EventInput::Summary {
                    event_type,
                    summary,
                } => output_events.push(AgentEventV5::summary(event_type, summary)),
            }
        }

        if let Some(trigger) = trigger_chat.as_ref() {
            match player_chat_events.get(&trigger.sequence) {
                Some(message) if message == &trigger.message => {}
                Some(_) => return Err(AgentContextV5AssemblyError::DuplicateTriggerChat),
                None => {
                    player_chat_events.insert(trigger.sequence, trigger.message.clone());
                    output_events.push(AgentEventV5::player_chat(trigger.message.clone()));
                }
            }
        }

        let chat = if unread_chat.is_empty() {
            None
        } else {
            let items = unread_chat[retained_start..]
                .iter()
                .map(|chat| {
                    if player_chat_events.contains_key(&chat.sequence) {
                        AgentChatItemV5::Moved(AgentChatMovedV5 {
                            username: chat.message.username.clone(),
                            at: chat.message.at.clone(),
                            moved: AgentChatMovedMarkerV5::Events,
                        })
                    } else {
                        AgentChatItemV5::Message(chat.message.clone())
                    }
                })
                .collect();
            Some(AgentChatV5 {
                items,
                omitted: unread_chat_omitted.saturating_add(retained_start as u64),
            })
        };

        let events = (!output_events.is_empty()).then_some(output_events);
        let omissions = (!omissions.is_empty()).then_some(omissions);

        let context = AgentDecisionContextV5 {
            protocol: AgentContextProtocolV5,
            stable: StableContextV5 { memory },
            frame: AgentFrameV5 {
                at,
                world: AgentWorldV5 { dimension },
                pose,
                status,
                hotbar,
                chat,
                sound,
                light,
                events,
                omissions,
            },
        };

        serde_json::to_value(&context)
            .map_err(|error| AgentContextV5AssemblyError::InvalidInput(error.to_string()))?;
        Ok(context)
    }
}
