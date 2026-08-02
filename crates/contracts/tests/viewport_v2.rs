use mineintent_contracts::agent::{ViewportFrameMessageV2, ViewportFrameV2WireError};
use mineintent_contracts::minecraft::{
    present_directed_viewport_v2, BlockInfo, DirectedSeenBlock, DirectedUnseenBlock,
    DirectedViewportProjection, DirectedWhy, ViewportDirectedV2, ViewportProtocolV2, ViewportV2,
    MAX_DIRECTED_VIEW_POSITIONS,
};
use serde_json::{json, Value};

const FRAME_V1: &str = include_str!("testdata/viewport-frame.v1.json");
const FRAME_V2: &str = include_str!("testdata/viewport-frame.v2.json");

fn full_viewport() -> Value {
    json!({
        "protocol": "mineintent.viewport.v2",
        "frame": {
            "coordinates": "minecraft_world_absolute",
            "self": {
                "position": [0.5, 64.0, 0.5],
                "yawDegrees": 0.0,
                "pitchDegrees": 0.0
            },
            "legend": {
                "visibleEntities": "entities",
                "visibleBlocks": "blocks"
            }
        },
        "lookedAtBlock": null,
        "visibleEntities": {"items": [], "truncated": false},
        "visibleBlocks": {"blocks": [], "truncated": false}
    })
}

#[test]
fn viewport_frame_v2_rejects_nested_unknown_and_wrong_shape() {
    let fixture: Value = serde_json::from_str(FRAME_V2).unwrap();
    let success =
        ViewportFrameMessageV2::success("2026-08-03T00:00:00Z", fixture["viewport"].clone())
            .expect("canonical full viewport is accepted");
    let encoded = serde_json::to_value(success).expect("v2 frame serializes");
    assert_eq!(encoded, fixture);
    assert_eq!(encoded["protocol"], "mineintent.viewport-frame.v2");
    assert_eq!(encoded["viewport"]["protocol"], "mineintent.viewport.v2");

    let mut unknown = full_viewport();
    unknown["unexpected"] = json!(true);
    assert!(matches!(
        ViewportFrameMessageV2::success("2026-08-03T00:00:00Z", unknown),
        Err(ViewportFrameV2WireError::InvalidViewportAnchor)
    ));

    let mut wrong_shape = full_viewport();
    wrong_shape["frame"] = json!([]);
    assert!(ViewportFrameMessageV2::success("2026-08-03T00:00:00Z", wrong_shape).is_err());

    let mut missing_looked_at = full_viewport();
    missing_looked_at
        .as_object_mut()
        .unwrap()
        .remove("lookedAtBlock");
    assert!(ViewportFrameMessageV2::success("2026-08-03T00:00:00Z", missing_looked_at).is_err());

    let mut unknown_wire = serde_json::to_value(
        ViewportFrameMessageV2::success("2026-08-03T00:00:00Z", full_viewport()).unwrap(),
    )
    .unwrap();
    unknown_wire["viewport"]["nestedUnknown"] = json!(1);
    assert!(serde_json::from_value::<ViewportFrameMessageV2>(unknown_wire).is_err());

    let unavailable =
        ViewportFrameMessageV2::unavailable("2026-08-03T00:00:00Z", "viewport_read_failed")
            .unwrap();
    let unavailable_wire = serde_json::to_value(unavailable).unwrap();
    assert_eq!(unavailable_wire["viewport"], Value::Null);
    assert_eq!(unavailable_wire["unavailable"], "viewport_read_failed");
}

#[test]
fn viewport_frame_v1_fixture_still_replays_without_v2_rewriting() {
    let decoded: mineintent_contracts::agent::ViewportFrameMessage =
        serde_json::from_str(FRAME_V1).unwrap();
    let fixture: Value = serde_json::from_str(FRAME_V1).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), fixture);
}

#[test]
fn directed_viewport_v2_preserves_limit_duplicate_and_five_reason_validation() {
    let seen = DirectedSeenBlock {
        at: [0, 64, 0],
        block: BlockInfo::bare("stone"),
    };
    let too_many = ViewportDirectedV2 {
        protocol: ViewportProtocolV2::V2,
        seen: vec![seen.clone(); MAX_DIRECTED_VIEW_POSITIONS + 1],
        unseen: Vec::new(),
    };
    assert!(serde_json::to_value(too_many).is_err());
    let too_many_wire = json!({
        "protocol": "mineintent.viewport.v2",
        "seen": vec![json!({"at": [0, 64, 0], "block": "stone"}); MAX_DIRECTED_VIEW_POSITIONS + 1],
        "unseen": []
    });
    assert!(serde_json::from_value::<ViewportDirectedV2>(too_many_wire).is_err());

    let duplicate = ViewportDirectedV2 {
        protocol: ViewportProtocolV2::V2,
        seen: vec![seen],
        unseen: vec![DirectedUnseenBlock {
            at: [0, 64, 0],
            why: vec![DirectedWhy::OutOfWorld],
            distance: None,
            max: None,
            by: None,
        }],
    };
    assert!(serde_json::to_value(duplicate).is_err());
    let duplicate_wire = json!({
        "protocol": "mineintent.viewport.v2",
        "seen": [{"at": [0, 64, 0], "block": "stone"}],
        "unseen": [{"at": [0, 64, 0], "why": ["out_of_world"]}]
    });
    assert!(serde_json::from_value::<ViewportDirectedV2>(duplicate_wire).is_err());

    let invalid_reason = DirectedViewportProjection {
        seen: Vec::new(),
        unseen: vec![DirectedUnseenBlock {
            at: [0, 64, 0],
            why: Vec::new(),
            distance: None,
            max: None,
            by: None,
        }],
    };
    assert!(present_directed_viewport_v2(&invalid_reason).is_err());
    assert!(serde_json::from_value::<ViewportDirectedV2>(json!({
        "protocol": "mineintent.viewport.v2",
        "seen": [],
        "unseen": [{"at": [0, 64, 0], "why": []}]
    }))
    .is_err());

    let valid = DirectedViewportProjection {
        seen: Vec::new(),
        unseen: vec![DirectedUnseenBlock {
            at: [0, 64, 0],
            why: vec![DirectedWhy::OutOfWorld],
            distance: None,
            max: None,
            by: None,
        }],
    };
    let model = present_directed_viewport_v2(&valid).expect("valid directed projection");
    let decoded: ViewportV2 = serde_json::from_value(serde_json::to_value(model).unwrap())
        .expect("v2 directed projection round trips");
    assert!(matches!(decoded, ViewportV2::Directed(_)));
}
