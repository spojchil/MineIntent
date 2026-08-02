use mineintent_contracts::agent::{
    fixtures, AgentChatItemV5, AgentContextProtocol, AgentEventV5, JsonAgentDecisionContextV5,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

const FIXTURE: &str = include_str!("testdata/agent-context.v5.json");

#[test]
fn v5_fixture_is_exact_and_keeps_the_v4_stable_shape() {
    let context: JsonAgentDecisionContextV5 = serde_json::from_str(FIXTURE).unwrap();
    assert_eq!(context, fixtures::agent_context_v5());
    assert_eq!(context.protocol, AgentContextProtocol::V5);
    assert_eq!(context.stable.memory, "玩家怕高");
    assert_eq!(serde_json::to_value(&context).unwrap(), fixture_value());
    assert_eq!(context.frame.hotbar.selected, 2);
    assert_eq!(context.frame.hotbar.slots.len(), 2);
    assert!(matches!(
        context.frame.chat.as_ref().unwrap().items[1],
        AgentChatItemV5::Moved(_)
    ));
    assert!(matches!(
        context.frame.events.as_ref().unwrap().as_slice(),
        [AgentEventV5::PlayerChat(_)]
    ));

    for forbidden in [
        "player",
        "self",
        "viewport",
        "effects",
        "inventory",
        "timeOfDay",
    ] {
        assert!(
            context.frame_value().get(forbidden).is_none(),
            "{forbidden}"
        );
    }
}

#[test]
fn v5_rejects_unknown_fields_nulls_empty_segments_and_ranges() {
    let mut unknown = fixture_value();
    insert_unknown(&mut unknown, "profile");
    assert_rejected::<JsonAgentDecisionContextV5>(unknown);

    let mut unknown_frame = fixture_value();
    insert_unknown(&mut unknown_frame["frame"], "viewport");
    assert_rejected::<JsonAgentDecisionContextV5>(unknown_frame);

    for (path, key) in [
        ("world", "timeOfDay"),
        ("pose", "self"),
        ("status", "effects"),
        ("hotbar", "inventory"),
    ] {
        let mut value = fixture_value();
        insert_unknown(&mut value["frame"][path], key);
        assert_rejected::<JsonAgentDecisionContextV5>(value);
    }

    for path in ["status", "chat", "sound", "events", "omissions"] {
        let mut value = fixture_value();
        value["frame"][path] = Value::Null;
        assert_rejected::<JsonAgentDecisionContextV5>(value);
    }
    let mut null_off_hand = fixture_value();
    null_off_hand["frame"]["hotbar"]["offHand"] = Value::Null;
    assert_rejected::<JsonAgentDecisionContextV5>(null_off_hand);
    let mut null_armor = fixture_value();
    null_armor["frame"]["status"]["armor"] = Value::Null;
    assert_rejected::<JsonAgentDecisionContextV5>(null_armor);

    for (path, invalid) in [("selected", json!(9)), ("light", json!(16))] {
        let mut value = fixture_value();
        if path == "selected" {
            value["frame"]["hotbar"][path] = invalid;
        } else {
            value["frame"][path] = invalid;
        }
        assert_rejected::<JsonAgentDecisionContextV5>(value);
    }
    let mut negative_light = fixture_value();
    negative_light["frame"]["light"] = json!(-1);
    assert_rejected::<JsonAgentDecisionContextV5>(negative_light);

    let mut invalid_slot = fixture_value();
    invalid_slot["frame"]["hotbar"]["slots"]["9"] = json!(["stone", 1]);
    assert_rejected::<JsonAgentDecisionContextV5>(invalid_slot);
    let mut invalid_slot_key = fixture_value();
    invalid_slot_key["frame"]["hotbar"]["slots"]["00"] = json!(["stone", 1]);
    assert_rejected::<JsonAgentDecisionContextV5>(invalid_slot_key);
    for count in [json!(0), json!(1.5), json!(-1)] {
        let mut value = fixture_value();
        value["frame"]["hotbar"]["slots"]["0"][1] = count;
        assert_rejected::<JsonAgentDecisionContextV5>(value);
    }

    for count in [json!(65), json!(u32::MAX)] {
        let mut value = fixture_value();
        value["frame"]["hotbar"]["slots"]["0"][1] = count.clone();
        let decoded: JsonAgentDecisionContextV5 = serde_json::from_value(value).unwrap();
        assert_eq!(
            serde_json::to_value(decoded).unwrap()["frame"]["hotbar"]["slots"]["0"][1],
            count
        );
    }

    let mut armor_zero = fixture_value();
    armor_zero["frame"]["status"]["armor"] = json!(0);
    assert_rejected::<JsonAgentDecisionContextV5>(armor_zero);
    let mut armor_absent = fixture_value();
    armor_absent["frame"]["status"]
        .as_object_mut()
        .unwrap()
        .remove("armor");
    let decoded: JsonAgentDecisionContextV5 = serde_json::from_value(armor_absent).unwrap();
    assert!(decoded.frame.status.as_ref().unwrap().armor.is_none());
    let encoded = serde_json::to_value(decoded).unwrap();
    assert!(encoded["frame"]["status"].get("armor").is_none());

    for (path, empty) in [
        ("chat", json!({"items": [], "omitted": 0})),
        ("events", json!([])),
        ("omissions", json!([])),
        ("sound", json!({})),
    ] {
        let mut value = fixture_value();
        value["frame"][path] = empty;
        assert_rejected::<JsonAgentDecisionContextV5>(value);
    }
}

#[test]
fn v5_chat_xor_event_and_finite_rules_are_strict() {
    let mut too_many = fixture_value();
    let items = too_many["frame"]["chat"]["items"].as_array_mut().unwrap();
    while items.len() < 9 {
        items.push(json!({
            "username": "extra",
            "text": "message",
            "at": "2026-07-25T00:00:00Z"
        }));
    }
    assert_rejected::<JsonAgentDecisionContextV5>(too_many);

    let mut both = fixture_value();
    both["frame"]["chat"]["items"][0]["moved"] = json!("events");
    assert_rejected::<JsonAgentDecisionContextV5>(both);
    let mut neither = fixture_value();
    neither["frame"]["chat"]["items"][0]
        .as_object_mut()
        .unwrap()
        .remove("text");
    assert_rejected::<JsonAgentDecisionContextV5>(neither);
    let mut wrong_marker = fixture_value();
    wrong_marker["frame"]["chat"]["items"][1]["moved"] = json!("chat");
    assert_rejected::<JsonAgentDecisionContextV5>(wrong_marker);

    let mut ordinary = fixture_value();
    ordinary["frame"]["events"] = json!([{"type": "damage", "summary": "受到伤害"}]);
    let decoded: JsonAgentDecisionContextV5 = serde_json::from_value(ordinary).unwrap();
    assert!(matches!(
        decoded.frame.events.unwrap().as_slice(),
        [AgentEventV5::Summary { .. }]
    ));
    let mut ordinary_extra = fixture_value();
    ordinary_extra["frame"]["events"] = json!([{
        "type": "damage",
        "summary": "受到伤害",
        "text": "not allowed"
    }]);
    assert_rejected::<JsonAgentDecisionContextV5>(ordinary_extra);

    let mut invalid_player_chat = fixture_value();
    invalid_player_chat["frame"]["events"] = json!([{
        "type": "player_chat",
        "summary": "not full text"
    }]);
    assert_rejected::<JsonAgentDecisionContextV5>(invalid_player_chat);

    let mut frame = fixtures::agent_context_v5().frame;
    frame.pose.yaw_degrees = f64::NAN;
    assert!(serde_json::to_value(frame).is_err());
    let mut frame = fixtures::agent_context_v5().frame;
    frame.status.as_mut().unwrap().health = f64::INFINITY;
    assert!(serde_json::to_value(frame).is_err());
}

#[test]
fn v5_sound_and_omissions_reuse_strict_current_information_shapes() {
    let mut unknown_sound = fixture_value();
    unknown_sound["frame"]["sound"]["recentSounds"][0]["unexpected"] = json!(true);
    assert_rejected::<JsonAgentDecisionContextV5>(unknown_sound);

    for required in ["distance", "direction", "volume", "pitch", "observedAt"] {
        let mut missing = fixture_value();
        missing["frame"]["sound"]["recentSounds"][0]
            .as_object_mut()
            .unwrap()
            .remove(required);
        assert_rejected::<JsonAgentDecisionContextV5>(missing);
    }

    let mut empty_recent = fixture_value();
    empty_recent["frame"]["sound"]["recentSounds"] = json!([]);
    assert_rejected::<JsonAgentDecisionContextV5>(empty_recent);
    let mut null_optional_sound_field = fixture_value();
    null_optional_sound_field["frame"]["sound"]["recentSounds"][0]["soundName"] = Value::Null;
    assert_rejected::<JsonAgentDecisionContextV5>(null_optional_sound_field);

    let valid_omission = json!({
        "interfaceId": "current_status",
        "reason": "partial",
        "fields": [],
        "message": "oxygen is not exposed"
    });
    let mut with_omission = fixture_value();
    with_omission["frame"]["omissions"] = json!([valid_omission]);
    let decoded: JsonAgentDecisionContextV5 = serde_json::from_value(with_omission).unwrap();
    assert_eq!(decoded.frame.omissions.as_ref().unwrap().len(), 1);
    let encoded = serde_json::to_value(decoded).unwrap();
    assert_eq!(
        encoded["frame"]["omissions"][0]["interfaceId"],
        "current_status"
    );
    assert_eq!(encoded["frame"]["omissions"][0]["reason"], "partial");

    let mut unknown_omission = fixture_value();
    unknown_omission["frame"]["omissions"] = json!([{
        "interfaceId": "current_status",
        "reason": "partial",
        "future": true
    }]);
    assert_rejected::<JsonAgentDecisionContextV5>(unknown_omission);
    let mut unknown_omission_enum = fixture_value();
    unknown_omission_enum["frame"]["omissions"] = json!([{
        "interfaceId": "effects",
        "reason": "not_registered"
    }]);
    assert_rejected::<JsonAgentDecisionContextV5>(unknown_omission_enum);
    let mut null_omission_message = fixture_value();
    null_omission_message["frame"]["omissions"] = json!([{
        "interfaceId": "current_status",
        "reason": "partial",
        "message": null
    }]);
    assert_rejected::<JsonAgentDecisionContextV5>(null_omission_message);
}

#[test]
fn v3_v4_fixtures_still_replay_after_adding_v5() {
    let v3: mineintent_contracts::agent::JsonAgentDecisionContext =
        serde_json::from_str(include_str!("testdata/agent-context.v3.json")).unwrap();
    let v4: mineintent_contracts::agent::JsonAgentDecisionContextV4 =
        serde_json::from_str(include_str!("testdata/agent-context.v4.json")).unwrap();
    assert_eq!(
        serde_json::to_value(v3).unwrap()["protocol"],
        "mineintent.agent-context.v3"
    );
    assert_eq!(
        serde_json::to_value(v4).unwrap()["protocol"],
        "mineintent.agent-context.v4"
    );
}

fn fixture_value() -> Value {
    serde_json::from_str(FIXTURE).unwrap()
}

fn insert_unknown(value: &mut Value, key: &str) {
    value
        .as_object_mut()
        .unwrap()
        .insert(key.to_owned(), json!(true));
}

trait FrameValue {
    fn frame_value(&self) -> Value;
}

impl FrameValue for JsonAgentDecisionContextV5 {
    fn frame_value(&self) -> Value {
        serde_json::to_value(&self.frame).unwrap()
    }
}

fn assert_rejected<T: DeserializeOwned>(value: Value) {
    assert!(serde_json::from_value::<T>(value).is_err());
}
