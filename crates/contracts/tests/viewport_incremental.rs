use std::collections::BTreeMap;

use mineintent_contracts::agent::{
    ViewportBaselineId, ViewportDeltaV1, ViewportIncrementalFrameError,
    ViewportIncrementalFrameMessageV1, ViewportIncrementalPayloadV1, ViewportKeyframeV1,
    ViewportScope, ViewportUnverifiedReason,
};
use mineintent_contracts::minecraft::ViewportCoordinateSystem;
use serde_json::json;

fn scope() -> ViewportScope {
    ViewportScope::new(
        "process-1",
        7,
        "world-1",
        "minecraft:overworld",
        "context-1",
        "exposed-face-v1",
    )
    .expect("valid scope")
}

#[test]
fn scope_carries_world_dimension_connection_and_algorithm_namespace() {
    let value = scope();
    assert_eq!(value.process_session_id, "process-1");
    assert_eq!(value.connection_epoch, 7);
    assert_eq!(value.world_id, "world-1");
    assert_eq!(value.dimension, "minecraft:overworld");
    assert_eq!(value.context_id, "context-1");
    assert_eq!(
        value.coordinates,
        ViewportCoordinateSystem::MinecraftWorldAbsolute
    );
    assert_eq!(value.algorithm_revision, "exposed-face-v1");
    assert!(ViewportScope::new("", 0, "world", "overworld", "context", "v1").is_err());
}

#[test]
fn delta_rejects_duplicate_keys_across_change_classes() {
    let delta = ViewportDeltaV1 {
        added: BTreeMap::from([(String::from("block:0,0,0"), json!("stone"))]),
        changed: BTreeMap::new(),
        confirmed_removed: vec![String::from("block:0,0,0")],
        unverified: BTreeMap::new(),
    };
    assert!(delta.validate().is_err());
}

#[test]
fn keyframe_and_delta_have_different_baseline_rules() {
    let keyframe = ViewportIncrementalFrameMessageV1::new(
        "2026-08-03T08:00:00Z",
        scope(),
        None,
        ViewportBaselineId::new(2, 1),
        ViewportIncrementalPayloadV1::Keyframe {
            viewport: ViewportKeyframeV1 {
                facts: BTreeMap::from([(String::from("block:0,0,0"), json!("stone"))]),
            },
            unverified: BTreeMap::new(),
            complete: true,
            omitted: 0,
        },
    )
    .expect("valid keyframe");
    let wire = serde_json::to_value(&keyframe).expect("keyframe wire");
    assert_eq!(wire["protocol"], "mineintent.viewport-frame.v3");
    assert_eq!(wire["payload"]["kind"], "keyframe");
    assert_eq!(wire["baselineId"]["epoch"], 2);

    let invalid = ViewportIncrementalFrameMessageV1::new(
        "2026-08-03T08:00:00Z",
        scope(),
        Some(ViewportBaselineId::new(2, 1)),
        ViewportBaselineId::new(2, 2),
        ViewportIncrementalPayloadV1::Keyframe {
            viewport: ViewportKeyframeV1::default(),
            unverified: BTreeMap::new(),
            complete: true,
            omitted: 0,
        },
    )
    .expect_err("keyframe cannot have a base");
    assert_eq!(invalid, ViewportIncrementalFrameError::KeyframeHasBase);

    let delta = ViewportIncrementalFrameMessageV1::new(
        "2026-08-03T08:00:01Z",
        scope(),
        Some(ViewportBaselineId::new(2, 1)),
        ViewportBaselineId::new(2, 2),
        ViewportIncrementalPayloadV1::Delta {
            delta: ViewportDeltaV1 {
                added: BTreeMap::new(),
                changed: BTreeMap::from([(String::from("block:0,0,0"), json!("air"))]),
                confirmed_removed: Vec::new(),
                unverified: BTreeMap::from([(
                    String::from("block:1,0,0"),
                    ViewportUnverifiedReason::Occluded,
                )]),
            },
            complete: false,
            omitted: 0,
        },
    )
    .expect("valid delta");
    let round_trip: ViewportIncrementalFrameMessageV1 =
        serde_json::from_value(serde_json::to_value(&delta).expect("delta wire"))
            .expect("delta round trip");
    assert_eq!(round_trip, delta);
}

#[test]
fn delta_requires_same_epoch_and_strictly_advancing_sequence() {
    let payload = ViewportIncrementalPayloadV1::Delta {
        delta: ViewportDeltaV1::default(),
        complete: true,
        omitted: 0,
    };
    let wrong_epoch = ViewportIncrementalFrameMessageV1::new(
        "at",
        scope(),
        Some(ViewportBaselineId::new(1, 4)),
        ViewportBaselineId::new(2, 5),
        payload.clone(),
    )
    .expect_err("epoch changes require a keyframe");
    assert_eq!(
        wrong_epoch,
        ViewportIncrementalFrameError::InvalidBaselineChain
    );

    let backwards = ViewportIncrementalFrameMessageV1::new(
        "at",
        scope(),
        Some(ViewportBaselineId::new(1, 4)),
        ViewportBaselineId::new(1, 4),
        payload,
    )
    .expect_err("delta sequence must advance");
    assert_eq!(
        backwards,
        ViewportIncrementalFrameError::InvalidBaselineChain
    );
}

#[test]
fn keyframe_is_strongly_typed_and_preserves_unverified_last_facts() {
    let keyframe = ViewportIncrementalFrameMessageV1::new(
        "at",
        scope(),
        None,
        ViewportBaselineId::new(0, 1),
        ViewportIncrementalPayloadV1::Keyframe {
            viewport: ViewportKeyframeV1 {
                facts: BTreeMap::from([(String::from("block:0,0,0"), json!("stone"))]),
            },
            unverified: BTreeMap::from([(
                String::from("block:0,0,0"),
                ViewportUnverifiedReason::Occluded,
            )]),
            complete: false,
            omitted: 0,
        },
    )
    .expect("an uncertain keyframe may retain its last known fact");
    assert!(serde_json::to_value(&keyframe).is_ok());

    let invalid = serde_json::json!({
        "protocol": "mineintent.viewport-frame.v3",
        "at": "at",
        "scope": serde_json::to_value(scope()).expect("scope"),
        "baselineId": { "epoch": 0, "sequence": 1 },
        "payload": {
            "kind": "keyframe",
            "viewport": { "facts": ["not-an-object"] },
            "complete": true,
            "omitted": 0
        }
    });
    assert!(serde_json::from_value::<ViewportIncrementalFrameMessageV1>(invalid).is_err());

    let invalid_key = ViewportIncrementalFrameMessageV1::new(
        "at",
        scope(),
        None,
        ViewportBaselineId::new(0, 1),
        ViewportIncrementalPayloadV1::Keyframe {
            viewport: ViewportKeyframeV1 {
                facts: BTreeMap::from([(String::from("bad\nkey"), json!("stone"))]),
            },
            unverified: BTreeMap::new(),
            complete: true,
            omitted: 0,
        },
    )
    .expect_err("invalid fact keys cannot form a wire frame");
    assert!(matches!(
        invalid_key,
        ViewportIncrementalFrameError::InvalidKeyframe(_)
    ));
}

#[test]
fn direct_deserialization_cannot_bypass_scope_or_completion_validation() {
    let invalid_scope = json!({
        "protocol": "mineintent.viewport-frame.v3",
        "at": "2026-08-03T08:00:00Z",
        "scope": {
            "processSessionId": "process-1",
            "connectionEpoch": 1,
            "worldId": "",
            "dimension": "minecraft:overworld",
            "contextId": "context-1",
            "coordinates": "minecraft_world_absolute",
            "algorithmRevision": "exposed-face-v1"
        },
        "baselineId": { "epoch": 0, "sequence": 1 },
        "payload": {
            "kind": "keyframe",
            "viewport": { "facts": {} },
            "complete": true,
            "omitted": 0
        }
    });
    assert!(serde_json::from_value::<ViewportIncrementalFrameMessageV1>(invalid_scope).is_err());

    let falsely_complete = json!({
        "protocol": "mineintent.viewport-frame.v3",
        "at": "2026-08-03T08:00:00Z",
        "scope": serde_json::to_value(scope()).expect("scope wire"),
        "baselineId": { "epoch": 0, "sequence": 1 },
        "payload": {
            "kind": "keyframe",
            "viewport": { "facts": {} },
            "unverified": { "block:0,64,0": "occluded" },
            "complete": true,
            "omitted": 0
        }
    });
    assert!(serde_json::from_value::<ViewportIncrementalFrameMessageV1>(falsely_complete).is_err());
}

#[test]
fn delta_rejects_a_zero_base_sequence() {
    let error = ViewportIncrementalFrameMessageV1::new(
        "2026-08-03T08:00:01Z",
        scope(),
        Some(ViewportBaselineId::new(3, 0)),
        ViewportBaselineId::new(3, 1),
        ViewportIncrementalPayloadV1::Delta {
            delta: ViewportDeltaV1::default(),
            complete: true,
            omitted: 0,
        },
    )
    .expect_err("zero is never an established baseline sequence");
    assert_eq!(error, ViewportIncrementalFrameError::InvalidBaselineChain);
}
