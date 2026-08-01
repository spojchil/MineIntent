//! The first six tests map one-to-one to `speech.test.ts`; the final two are explicitly additional
//! contract/characterization tests. The two asynchronous tests are mapped in `speech_scheduler.rs`.

use std::convert::Infallible;

use mineintent_contracts::minecraft::{
    BackendEventEnvelope, BackendEventKind, BackendEventMetadata, ChatPosition, FactSource,
    ProtocolChatEvent,
};
use mineintent_middle::speech::{
    interpret_player_chat, segment_chat, Addressing, AddressingEvidence, ChatInputContext,
    PlayerChatMessage, PlayerChatProtocol, SpeechEvent, SpeechRequest, SpeechTransport,
};
use serde_json::json;

#[test]
fn chat_input_records_sender_addressing_evidence_time_and_world_context() {
    let text = "mInEiNtEnTbOt，你在吗";
    let mut context = context();
    context.conversation_active_with = Some("SPOJCHIL".to_owned());
    let message = interpret_player_chat(&chat(text, Some("spojchil")), &context).unwrap();

    assert_eq!(message.protocol, PlayerChatProtocol::V1);
    assert_eq!(message.source_event_id, "chat-1");
    assert_eq!(message.sender.username, "spojchil");
    assert_eq!(message.text, text);
    assert_eq!(message.occurred_at, "2026-07-12T00:00:00.000Z");
    assert_eq!(message.verified, Some(true));
    assert_eq!(message.world.world_id, "world");
    assert_eq!(message.world.dimension.as_deref(), Some("overworld"));
    assert_eq!(message.world.connection_epoch, 3);
    assert_eq!(
        message.addressing,
        Addressing {
            addressed_to_participant: true,
            evidence: vec![
                AddressingEvidence::ExplicitName,
                AddressingEvidence::OngoingConversation,
                AddressingEvidence::SingleParty,
            ],
        }
    );
}

#[test]
fn addressing_is_symmetric_for_players_under_the_same_multiplayer_input_conditions() {
    let multiplayer = ChatInputContext {
        participant_username: "MineIntentBot".to_owned(),
        online_player_usernames: vec![
            "Alice".to_owned(),
            "Bob".to_owned(),
            "MineIntentBot".to_owned(),
        ],
        conversation_active_with: None,
    };
    let alice_named =
        interpret_player_chat(&chat("MineIntentBot，你在吗", Some("Alice")), &multiplayer).unwrap();
    let bob_named =
        interpret_player_chat(&chat("MineIntentBot，你在吗", Some("Bob")), &multiplayer).unwrap();
    assert_eq!(alice_named.sender.username, "Alice");
    assert_eq!(bob_named.sender.username, "Bob");
    assert_eq!(without_sender(&alice_named), without_sender(&bob_named));
    assert!(alice_named.addressing.addressed_to_participant);
    assert!(bob_named.addressing.addressed_to_participant);

    let alice_unaddressed =
        interpret_player_chat(&chat("今天天气不错", Some("Alice")), &multiplayer).unwrap();
    let bob_unaddressed =
        interpret_player_chat(&chat("今天天气不错", Some("Bob")), &multiplayer).unwrap();
    let not_addressed = Addressing {
        addressed_to_participant: false,
        evidence: vec![AddressingEvidence::NotAddressed],
    };
    assert_eq!(alice_unaddressed.addressing, not_addressed);
    assert_eq!(bob_unaddressed.addressing, not_addressed);
}

#[test]
fn sole_online_player_is_addressed_by_single_party_conditions_without_naming_participant() {
    let message = interpret_player_chat(
        &chat("今天天气不错", Some("Casey")),
        &ChatInputContext {
            participant_username: "MineIntentBot".to_owned(),
            online_player_usernames: vec!["Casey".to_owned(), "MineIntentBot".to_owned()],
            conversation_active_with: None,
        },
    )
    .unwrap();
    assert_eq!(
        message.addressing,
        Addressing {
            addressed_to_participant: true,
            evidence: vec![AddressingEvidence::SingleParty],
        }
    );
}

#[test]
fn straggler_chat_from_someone_other_than_sole_online_player_is_not_single_party_addressed() {
    let message = interpret_player_chat(
        &chat("今天天气不错", Some("Bob")),
        &ChatInputContext {
            participant_username: "MineIntentBot".to_owned(),
            online_player_usernames: vec!["Casey".to_owned(), "MineIntentBot".to_owned()],
            conversation_active_with: None,
        },
    )
    .unwrap();
    assert_eq!(
        message.addressing,
        Addressing {
            addressed_to_participant: false,
            evidence: vec![AddressingEvidence::NotAddressed],
        }
    );
}

#[test]
fn stop_wording_remains_ordinary_addressed_player_text() {
    let text = "MineIntentBot，停一下";
    let message = interpret_player_chat(&chat(text, Some("spojchil")), &context()).unwrap();
    assert_eq!(message.text, text);
    assert!(message.addressing.addressed_to_participant);
}

#[test]
fn segment_chat_respects_unicode_code_point_length_and_keeps_ordered_content() {
    let original = "这是第一句话。这是第二句话，需要被安全分开。";
    let segments = segment_chat(original, 10).unwrap();
    assert!(segments.iter().all(|segment| segment.chars().count() <= 10));
    assert_eq!(segments.concat().replace(' ', ""), original);

    let emoji = segment_chat("😀😀😀", 2).unwrap();
    assert_eq!(emoji, vec!["😀😀", "😀"]);
    assert_eq!(
        segment_chat("\0\r\n  你好\t\t世界\u{FEFF}", 256).unwrap(),
        vec!["你好 世界"]
    );
    assert!(segment_chat("\0\r\n", 256).is_err());
}

#[test]
fn additional_non_player_public_chat_and_missing_sender_are_filtered() {
    let context = context();

    let mut wrong_kind = chat("MineIntentBot，你在吗", Some("spojchil"));
    wrong_kind.kind = BackendEventKind::Sound;
    assert_eq!(interpret_player_chat(&wrong_kind, &context), None);

    let mut system = chat("MineIntentBot，你在吗", Some("spojchil"));
    system.payload.position = Some(ChatPosition::System);
    assert_eq!(interpret_player_chat(&system, &context), None);

    assert_eq!(interpret_player_chat(&chat("hello", None), &context), None);
    assert_eq!(
        interpret_player_chat(&chat("hello", Some("")), &context),
        None
    );
}

#[test]
fn additional_speech_contracts_are_closed_strict_and_optional_non_null() {
    for evidence in [
        "explicit_name",
        "explicit_reply",
        "single_party",
        "ongoing_conversation",
        "not_addressed",
    ] {
        assert!(serde_json::from_value::<AddressingEvidence>(json!(evidence)).is_ok());
    }
    assert!(serde_json::from_value::<AddressingEvidence>(json!("nearby")).is_err());

    let message = interpret_player_chat(&chat("hello", Some("spojchil")), &context()).unwrap();
    let mut omitted = serde_json::to_value(message).unwrap();
    omitted.as_object_mut().unwrap().remove("verified");
    omitted["world"]
        .as_object_mut()
        .unwrap()
        .remove("dimension");
    let parsed: PlayerChatMessage = serde_json::from_value(omitted.clone()).unwrap();
    assert_eq!(parsed.verified, None);
    assert_eq!(parsed.world.dimension, None);

    let mut null_verified = omitted.clone();
    null_verified["verified"] = json!(null);
    assert!(serde_json::from_value::<PlayerChatMessage>(null_verified).is_err());
    let mut null_dimension = omitted;
    null_dimension["world"]["dimension"] = json!(null);
    assert!(serde_json::from_value::<PlayerChatMessage>(null_dimension).is_err());
    let mut unknown_message = serde_json::to_value(
        interpret_player_chat(&chat("hello", Some("spojchil")), &context()).unwrap(),
    )
    .unwrap();
    unknown_message["callbackUrl"] = json!("http://127.0.0.1");
    assert!(serde_json::from_value::<PlayerChatMessage>(unknown_message).is_err());
    let mut wrong_protocol = serde_json::to_value(
        interpret_player_chat(&chat("hello", Some("spojchil")), &context()).unwrap(),
    )
    .unwrap();
    wrong_protocol["protocol"] = json!("mineintent.player-chat.v2");
    assert!(serde_json::from_value::<PlayerChatMessage>(wrong_protocol).is_err());

    let omitted_conversation: ChatInputContext = serde_json::from_value(json!({
        "participantUsername": "MineIntentBot",
        "onlinePlayerUsernames": ["spojchil", "MineIntentBot"]
    }))
    .unwrap();
    assert_eq!(omitted_conversation.conversation_active_with, None);
    assert!(serde_json::from_value::<ChatInputContext>(json!({
        "participantUsername": "MineIntentBot",
        "onlinePlayerUsernames": [],
        "conversationActiveWith": null
    }))
    .is_err());
    assert!(serde_json::from_value::<SpeechRequest>(json!({
        "id": "reply",
        "text": "hello"
    }))
    .is_ok());
    assert!(serde_json::from_value::<SpeechRequest>(json!({
        "id": "reply",
        "text": "hello",
        "callbackUrl": "http://127.0.0.1"
    }))
    .is_err());
    for event in [
        json!({"type": "scheduled", "requestId": "reply", "segments": 2}),
        json!({"type": "sent", "requestId": "reply", "segment": 0, "text": "hello"}),
        json!({"type": "cancelled", "requestId": "reply", "reason": "stopped"}),
        json!({"type": "failed", "requestId": "reply", "reason": "offline"}),
    ] {
        let parsed: SpeechEvent = serde_json::from_value(event.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), event);
    }
    assert!(serde_json::from_value::<SpeechEvent>(json!({
        "type": "scheduled",
        "requestId": "reply",
        "segments": 1,
        "delayMs": 1000
    }))
    .is_err());
    assert!(serde_json::from_value::<SpeechEvent>(json!({
        "type": "stopped",
        "requestId": "reply",
        "reason": "done"
    }))
    .is_err());

    struct NoopTransport;
    impl SpeechTransport for NoopTransport {
        type Error = Infallible;

        fn send(&self, _message: &str) -> Result<(), Self::Error> {
            Ok(())
        }
    }
    fn accepts_transport_object(_transport: &dyn SpeechTransport<Error = Infallible>) {}
    accepts_transport_object(&NoopTransport);
}

fn chat(text: &str, sender: Option<&str>) -> BackendEventEnvelope<ProtocolChatEvent> {
    BackendEventEnvelope::new(
        BackendEventMetadata {
            id: "chat-1".to_owned(),
            occurred_at: "2026-07-12T00:00:00.000Z".to_owned(),
            process_session_id: "session".to_owned(),
            connection_epoch: 3,
            connection_attempt_id: "attempt".to_owned(),
            world_id: "world".to_owned(),
            dimension: Some("overworld".to_owned()),
        },
        BackendEventKind::Chat,
        FactSource::ServerObserved,
        ProtocolChatEvent {
            sender_username: sender.map(str::to_owned),
            plain_text: text.to_owned(),
            position: Some(ChatPosition::Chat),
            verified: Some(true),
        },
    )
}

fn context() -> ChatInputContext {
    ChatInputContext {
        participant_username: "MineIntentBot".to_owned(),
        online_player_usernames: vec!["spojchil".to_owned(), "MineIntentBot".to_owned()],
        conversation_active_with: None,
    }
}

fn without_sender(message: &PlayerChatMessage) -> serde_json::Value {
    let mut value = serde_json::to_value(message).unwrap();
    value.as_object_mut().unwrap().remove("sender");
    value
}
