use std::collections::BTreeMap;

use mineintent_contracts::{
    agent::{fixtures, AgentChatMessageV5, AgentHotbarV5, AgentItemStackV5},
    agent::{AgentPoseV5, AgentStatusV5},
    information::{RelativeDirection, SoundObservation, SoundValues},
};
use mineintent_middle::agent::{
    AgentChatInputV5, AgentChatTriggerV5, AgentContextV5Assembler, AgentContextV5EventInput,
    AgentContextV5Input,
};
use serde_json::{json, Value};

#[test]
fn fake_backed_v5_assembler_matches_exact_fixture_and_deduplicates_trigger_once() {
    let bob = chat(10, "bob", "早");
    let alice = chat(11, "alice", "帮我看看农田");
    let input = input(
        vec![bob, alice.clone()],
        vec![AgentContextV5EventInput::PlayerChat {
            sequence: 11,
            message: alice.message.clone(),
        }],
        Some(AgentChatTriggerV5 {
            sequence: 11,
            message: alice.message,
        }),
    );

    let context = AgentContextV5Assembler.assemble(input).unwrap();
    assert_eq!(context, fixtures::agent_context_v5());
    let wire = serde_json::to_value(&context).unwrap();
    assert_eq!(wire, fixture_value());
    assert_eq!(wire.to_string().matches("帮我看看农田").count(), 1);
    assert!(wire["frame"]["chat"]["items"][1].get("text").is_none());
    assert!(wire.to_string().find("sequence").is_none());
}

#[test]
fn assembler_uses_stable_sequence_not_chat_text_and_keeps_window_order() {
    let first = chat(1, "same", "same text");
    let second = chat(2, "same", "same text");
    let context = AgentContextV5Assembler
        .assemble(input(
            vec![first, second.clone()],
            vec![AgentContextV5EventInput::PlayerChat {
                sequence: 2,
                message: second.message.clone(),
            }],
            Some(AgentChatTriggerV5 {
                sequence: 2,
                message: second.message,
            }),
        ))
        .unwrap();
    let wire = serde_json::to_value(context).unwrap();
    assert!(wire["frame"]["chat"]["items"][0].get("text").is_some());
    assert!(wire["frame"]["chat"]["items"][1].get("text").is_none());
    assert_eq!(wire["frame"]["events"][0]["username"], "same");
    assert_eq!(wire["frame"]["events"][0]["text"], "same text");
}

#[test]
fn assembler_marks_boundary_trigger_and_counts_older_trigger_as_omitted() {
    let chats = (0..10)
        .map(|sequence| {
            chat(
                sequence,
                &format!("user-{sequence}"),
                &format!("message-{sequence}"),
            )
        })
        .collect::<Vec<_>>();

    let boundary = chats[2].clone();
    let boundary_context = AgentContextV5Assembler
        .assemble(input(
            chats.clone(),
            vec![AgentContextV5EventInput::PlayerChat {
                sequence: boundary.sequence,
                message: boundary.message.clone(),
            }],
            Some(AgentChatTriggerV5 {
                sequence: boundary.sequence,
                message: boundary.message,
            }),
        ))
        .unwrap();
    let boundary_wire = serde_json::to_value(boundary_context).unwrap();
    assert_eq!(boundary_wire["frame"]["chat"]["omitted"], 2);
    assert_eq!(
        boundary_wire["frame"]["chat"]["items"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    assert!(boundary_wire["frame"]["chat"]["items"][0]
        .get("text")
        .is_none());
    assert_eq!(boundary_wire.to_string().matches("message-2").count(), 1);

    let old = chats[0].clone();
    let old_context = AgentContextV5Assembler
        .assemble(input(
            chats,
            vec![AgentContextV5EventInput::PlayerChat {
                sequence: old.sequence,
                message: old.message.clone(),
            }],
            Some(AgentChatTriggerV5 {
                sequence: old.sequence,
                message: old.message,
            }),
        ))
        .unwrap();
    let old_wire = serde_json::to_value(old_context).unwrap();
    assert_eq!(old_wire["frame"]["chat"]["omitted"], 2);
    assert_eq!(
        old_wire["frame"]["chat"]["items"].as_array().unwrap().len(),
        8
    );
    assert_eq!(old_wire["frame"]["events"][0]["text"], "message-0");
    assert_eq!(old_wire.to_string().matches("message-0").count(), 1);
}

#[test]
fn assembler_preserves_ordinary_events_and_absents_empty_chat_or_event_segments() {
    let ordinary = AgentContextV5EventInput::Summary {
        event_type: "damage".to_owned(),
        summary: "受到伤害".to_owned(),
    };
    let context = AgentContextV5Assembler
        .assemble(input(vec![], vec![ordinary], None))
        .unwrap();
    let wire = serde_json::to_value(context).unwrap();
    assert!(wire["frame"].get("chat").is_none());
    assert_eq!(
        wire["frame"]["events"],
        json!([{
            "type": "damage",
            "summary": "受到伤害"
        }])
    );

    let context = AgentContextV5Assembler
        .assemble(input(vec![], vec![], None))
        .unwrap();
    let wire = serde_json::to_value(context).unwrap();
    assert!(wire["frame"].get("chat").is_none());
    assert!(wire["frame"].get("events").is_none());
}

#[test]
fn assembler_rejects_duplicate_or_mismatched_trigger_identity() {
    let message = chat(4, "alice", "text");
    let duplicate = input(
        vec![message.clone()],
        vec![player_event(&message), player_event(&message)],
        Some(AgentChatTriggerV5 {
            sequence: 4,
            message: message.message.clone(),
        }),
    );
    assert!(AgentContextV5Assembler.assemble(duplicate).is_err());

    let missing = input(
        vec![message.clone()],
        vec![],
        Some(AgentChatTriggerV5 {
            sequence: 5,
            message: message.message.clone(),
        }),
    );
    assert!(AgentContextV5Assembler.assemble(missing).is_err());

    let mismatched = input(
        vec![message.clone()],
        vec![AgentContextV5EventInput::PlayerChat {
            sequence: 4,
            message: chat(4, "alice", "other text").message,
        }],
        None,
    );
    assert!(AgentContextV5Assembler.assemble(mismatched).is_err());
}

fn input(
    unread_chat: Vec<AgentChatInputV5>,
    events: Vec<AgentContextV5EventInput>,
    trigger_chat: Option<AgentChatTriggerV5>,
) -> AgentContextV5Input {
    let mut slots = BTreeMap::new();
    slots.insert(0, AgentItemStackV5::new("oak_log", 12).unwrap());
    slots.insert(2, AgentItemStackV5::new("iron_sword", 1).unwrap());
    AgentContextV5Input {
        memory: "玩家怕高".to_owned(),
        at: "2026-07-25T00:00:00Z".to_owned(),
        dimension: "minecraft:overworld".to_owned(),
        pose: AgentPoseV5 {
            position: [0.5, 64.0, -7.5],
            yaw_degrees: 0.0,
            pitch_degrees: 0.0,
        },
        status: Some(AgentStatusV5 {
            health: 20.0,
            food: 20.0,
            armor: Some(15),
        }),
        hotbar: AgentHotbarV5 {
            selected: 2,
            slots,
            off_hand: Some(AgentItemStackV5::new("shield", 1).unwrap()),
        },
        unread_chat,
        unread_chat_omitted: 0,
        sound: Some(SoundValues {
            recent_sounds: Some(vec![SoundObservation {
                sound_name: Some("block.note_block.harp".to_owned()),
                category: Some("record".to_owned()),
                distance: 4.5,
                direction: RelativeDirection::Ahead,
                volume: 1.0,
                pitch: 0.8,
                observed_at: "2026-08-01T00:00:20Z".to_owned(),
            }]),
        }),
        light: Some(12),
        events,
        omissions: vec![],
        trigger_chat,
    }
}

fn chat(sequence: u64, username: &str, text: &str) -> AgentChatInputV5 {
    AgentChatInputV5 {
        sequence,
        message: AgentChatMessageV5 {
            username: username.to_owned(),
            text: text.to_owned(),
            at: "2026-07-25T00:00:00Z".to_owned(),
        },
    }
}

fn player_event(chat: &AgentChatInputV5) -> AgentContextV5EventInput {
    AgentContextV5EventInput::PlayerChat {
        sequence: chat.sequence,
        message: chat.message.clone(),
    }
}

fn fixture_value() -> Value {
    serde_json::from_str(include_str!(
        "../../contracts/tests/testdata/agent-context.v5.json"
    ))
    .unwrap()
}
