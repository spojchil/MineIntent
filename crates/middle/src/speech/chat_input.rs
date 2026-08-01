use mineintent_contracts::minecraft::{
    BackendEventEnvelope, BackendEventKind, ChatPosition, ProtocolChatEvent,
};

use super::{
    Addressing, AddressingEvidence, ChatInputContext, PlayerChatMessage, PlayerChatProtocol,
    PlayerChatSender, PlayerChatWorld,
};

pub fn interpret_player_chat(
    event: &BackendEventEnvelope<ProtocolChatEvent>,
    context: &ChatInputContext,
) -> Option<PlayerChatMessage> {
    if event.kind != BackendEventKind::Chat || event.payload.position != Some(ChatPosition::Chat) {
        return None;
    }
    let sender = event
        .payload
        .sender_username
        .as_deref()
        .filter(|sender| !sender.is_empty())?;

    let explicit_name = mentions_name(&event.payload.plain_text, &context.participant_username);
    let ongoing = context
        .conversation_active_with
        .as_deref()
        .is_some_and(|name| equal_name(name, sender));
    let mut other_players = context
        .online_player_usernames
        .iter()
        .filter(|name| !equal_name(name, &context.participant_username));
    let only_other = other_players.next();
    let single_party =
        only_other.is_some_and(|name| equal_name(name, sender)) && other_players.next().is_none();

    let mut evidence = Vec::with_capacity(3);
    if explicit_name {
        evidence.push(AddressingEvidence::ExplicitName);
    }
    if ongoing {
        evidence.push(AddressingEvidence::OngoingConversation);
    }
    if single_party {
        evidence.push(AddressingEvidence::SingleParty);
    }
    let addressed_to_participant = !evidence.is_empty();
    if evidence.is_empty() {
        evidence.push(AddressingEvidence::NotAddressed);
    }

    Some(PlayerChatMessage {
        protocol: PlayerChatProtocol::V1,
        source_event_id: event.id.clone(),
        occurred_at: event.occurred_at.clone(),
        sender: PlayerChatSender {
            username: sender.to_owned(),
        },
        text: event.payload.plain_text.clone(),
        verified: event.payload.verified,
        addressing: Addressing {
            addressed_to_participant,
            evidence,
        },
        world: PlayerChatWorld {
            world_id: event.world_id.clone(),
            dimension: event.dimension.clone(),
            connection_epoch: event.connection_epoch,
        },
    })
}

fn equal_name(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

fn mentions_name(text: &str, name: &str) -> bool {
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let name: Vec<char> = name.to_lowercase().chars().collect();
    if name.is_empty() || name.len() > text.len() {
        return false;
    }

    (0..=text.len() - name.len()).any(|start| {
        text[start..start + name.len()] == name
            && (start == 0 || is_name_prefix(text[start - 1]))
            && (start + name.len() == text.len() || is_name_suffix(text[start + name.len()]))
    })
}

fn is_name_prefix(character: char) -> bool {
    is_javascript_whitespace(character) || matches!(character, '@' | '＠' | '，' | ',' | '：' | ':')
}

fn is_name_suffix(character: char) -> bool {
    is_javascript_whitespace(character)
        || matches!(
            character,
            '，' | ',' | '：' | ':' | '！' | '!' | '.' | '?' | '？'
        )
}

fn is_javascript_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}
