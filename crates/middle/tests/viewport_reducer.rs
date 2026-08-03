use std::collections::BTreeMap;

use mineintent_contracts::agent::{
    ViewportBaselineId, ViewportDeltaV1, ViewportIncrementalFrameMessageV1,
    ViewportIncrementalPayloadV1, ViewportKeyframeV1, ViewportScope, ViewportUnverifiedReason,
};
use mineintent_middle::agent::{
    MirrorLimits, ViewportIncrementalReducer, ViewportMirror, ViewportObservation,
    ViewportReplayError,
};
use serde_json::json;

fn scope(dimension: &str) -> ViewportScope {
    ViewportScope::new(
        "process-1",
        1,
        "world-1",
        dimension,
        "context-1",
        "exposed-face-v1",
    )
    .expect("valid scope")
}

fn observation(scope: ViewportScope, facts: &[(&str, serde_json::Value)]) -> ViewportObservation {
    ViewportObservation {
        scope,
        observed: facts
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect(),
        confirmed_removed: Default::default(),
        unverified: BTreeMap::new(),
        truncated: false,
    }
}

fn commit_frame(
    mirror: &ViewportMirror,
    proposal: mineintent_middle::agent::ViewportProposal,
) -> mineintent_middle::agent::ViewportFrame {
    mirror
        .commit(proposal.pending().expect("pending"))
        .expect("commit")
}

#[test]
fn keyframe_then_deltas_replay_to_the_same_state() {
    let scope = scope("minecraft:overworld");
    let mirror = ViewportMirror::new(scope.clone()).expect("mirror");
    let mut reducer = ViewportIncrementalReducer::new(scope.clone()).expect("reducer");

    let first = commit_frame(
        &mirror,
        mirror
            .propose(
                observation(scope.clone(), &[("block:0,64,0", json!("stone"))]),
                MirrorLimits::default(),
            )
            .expect("keyframe"),
    );
    let first_message = first
        .to_incremental_message("2026-08-03T08:00:00Z")
        .expect("wire keyframe");
    let state = reducer.apply(&first_message).expect("apply keyframe");
    assert_eq!(state.facts["block:0,64,0"], json!("stone"));

    let second = commit_frame(
        &mirror,
        mirror
            .propose(
                observation(scope.clone(), &[("block:0,64,0", json!("dirt"))]),
                MirrorLimits::default(),
            )
            .expect("delta"),
    );
    let state = reducer
        .apply(
            &second
                .to_incremental_message("2026-08-03T08:00:01Z")
                .expect("wire delta"),
        )
        .expect("apply delta");
    assert_eq!(state.facts["block:0,64,0"], json!("dirt"));
}

#[test]
fn keyframe_unverified_overlay_replays_with_its_last_known_fact() {
    let scope = scope("minecraft:overworld");
    let mut reducer = ViewportIncrementalReducer::new(scope.clone()).expect("reducer");
    let frame = ViewportIncrementalFrameMessageV1::new(
        "2026-08-03T08:00:00Z",
        scope,
        None,
        ViewportBaselineId::new(0, 1),
        ViewportIncrementalPayloadV1::Keyframe {
            viewport: ViewportKeyframeV1 {
                facts: BTreeMap::from([(String::from("block:0,64,0"), json!("stone"))]),
            },
            unverified: BTreeMap::from([(
                String::from("block:0,64,0"),
                ViewportUnverifiedReason::Occluded,
            )]),
            complete: false,
            omitted: 0,
        },
    )
    .expect("valid uncertain keyframe");
    let state = reducer.apply(&frame).expect("replay keyframe");
    assert_eq!(state.facts["block:0,64,0"], json!("stone"));
    assert_eq!(
        state.unverified["block:0,64,0"],
        ViewportUnverifiedReason::Occluded
    );
    assert!(state.repair_required);
}

#[test]
fn out_of_order_delta_is_rejected_without_mutating_receiver() {
    let scope = scope("minecraft:overworld");
    let mirror = ViewportMirror::new(scope.clone()).expect("mirror");
    let mut reducer = ViewportIncrementalReducer::new(scope.clone()).expect("reducer");

    let first = commit_frame(
        &mirror,
        mirror
            .propose(
                observation(scope.clone(), &[("block:0,64,0", json!("stone"))]),
                MirrorLimits::default(),
            )
            .expect("first"),
    );
    reducer
        .apply(
            &first
                .to_incremental_message("2026-08-03T08:00:00Z")
                .expect("wire"),
        )
        .expect("apply first");

    let second = commit_frame(
        &mirror,
        mirror
            .propose(
                observation(scope.clone(), &[("block:0,64,0", json!("dirt"))]),
                MirrorLimits::default(),
            )
            .expect("second"),
    );
    let third = commit_frame(
        &mirror,
        mirror
            .propose(
                observation(scope.clone(), &[("block:0,64,0", json!("sand"))]),
                MirrorLimits::default(),
            )
            .expect("third"),
    );

    let third_message = third
        .to_incremental_message("2026-08-03T08:00:02Z")
        .expect("wire third");
    assert!(matches!(
        reducer.apply(&third_message),
        Err(ViewportReplayError::BaseMismatch { .. })
    ));
    assert_eq!(
        reducer.state().expect("state").facts["block:0,64,0"],
        json!("stone")
    );

    reducer
        .apply(
            &second
                .to_incremental_message("2026-08-03T08:00:01Z")
                .expect("wire second"),
        )
        .expect("apply second");
    reducer.apply(&third_message).expect("apply third");
    assert_eq!(
        reducer.state().expect("state").facts["block:0,64,0"],
        json!("sand")
    );
}

#[test]
fn wire_rejects_a_sequence_gap_before_reducer_replay() {
    let scope = scope("minecraft:overworld");
    let mirror = ViewportMirror::new(scope.clone()).expect("mirror");
    let mut reducer = ViewportIncrementalReducer::new(scope.clone()).expect("reducer");
    let first = commit_frame(
        &mirror,
        mirror
            .propose(
                observation(scope.clone(), &[("block:0,64,0", json!("stone"))]),
                MirrorLimits::default(),
            )
            .expect("first"),
    );
    reducer
        .apply(
            &first
                .to_incremental_message("2026-08-03T08:00:00Z")
                .expect("wire"),
        )
        .expect("apply first");

    let skipped = ViewportIncrementalFrameMessageV1::new(
        "2026-08-03T08:00:02Z",
        scope,
        Some(ViewportBaselineId::new(0, 1)),
        ViewportBaselineId::new(0, 3),
        ViewportIncrementalPayloadV1::Delta {
            delta: ViewportDeltaV1::default(),
            complete: true,
            omitted: 0,
        },
    )
    .expect_err("v3 only permits adjacent baseline ids");
    assert_eq!(
        skipped,
        mineintent_contracts::agent::ViewportIncrementalFrameError::InvalidBaselineChain
    );
    assert_eq!(
        reducer.state().expect("unchanged").baseline_id,
        ViewportBaselineId::new(0, 1)
    );
}

#[test]
fn complete_flag_cannot_clear_existing_unverified_state() {
    let scope = scope("minecraft:overworld");
    let mut reducer = ViewportIncrementalReducer::new(scope.clone()).expect("reducer");
    let keyframe = ViewportIncrementalFrameMessageV1::new(
        "2026-08-03T08:00:00Z",
        scope.clone(),
        None,
        ViewportBaselineId::new(0, 1),
        ViewportIncrementalPayloadV1::Keyframe {
            viewport: ViewportKeyframeV1 {
                facts: BTreeMap::from([(String::from("block:0,64,0"), json!("stone"))]),
            },
            unverified: BTreeMap::new(),
            complete: true,
            omitted: 0,
        },
    )
    .expect("keyframe");
    reducer.apply(&keyframe).expect("apply keyframe");

    let hidden = ViewportIncrementalFrameMessageV1::new(
        "2026-08-03T08:00:01Z",
        scope.clone(),
        Some(ViewportBaselineId::new(0, 1)),
        ViewportBaselineId::new(0, 2),
        ViewportIncrementalPayloadV1::Delta {
            delta: ViewportDeltaV1 {
                unverified: BTreeMap::from([(
                    "block:0,64,0".to_owned(),
                    ViewportUnverifiedReason::Occluded,
                )]),
                ..ViewportDeltaV1::default()
            },
            complete: false,
            omitted: 0,
        },
    )
    .expect("unverified delta");
    reducer.apply(&hidden).expect("apply unverified delta");

    let unrelated = ViewportIncrementalFrameMessageV1::new(
        "2026-08-03T08:00:02Z",
        scope,
        Some(ViewportBaselineId::new(0, 2)),
        ViewportBaselineId::new(0, 3),
        ViewportIncrementalPayloadV1::Delta {
            delta: ViewportDeltaV1::default(),
            complete: true,
            omitted: 0,
        },
    )
    .expect("syntactically complete delta");
    let state = reducer.apply(&unrelated).expect("apply unrelated delta");
    assert!(state.repair_required);
    assert_eq!(
        state.unverified["block:0,64,0"],
        ViewportUnverifiedReason::Occluded
    );
}

#[test]
fn unverified_delta_preserves_the_last_fact_until_reconfirmed() {
    let scope = scope("minecraft:overworld");
    let mirror = ViewportMirror::new(scope.clone()).expect("mirror");
    let mut reducer = ViewportIncrementalReducer::new(scope.clone()).expect("reducer");
    let first = commit_frame(
        &mirror,
        mirror
            .propose(
                observation(scope.clone(), &[("block:0,64,0", json!("stone"))]),
                MirrorLimits::default(),
            )
            .expect("first"),
    );
    reducer
        .apply(
            &first
                .to_incremental_message("2026-08-03T08:00:00Z")
                .expect("wire"),
        )
        .expect("apply first");
    let mut hidden = ViewportObservation::empty(scope.clone());
    hidden.unverified.insert(
        "block:0,64,0".to_owned(),
        ViewportUnverifiedReason::Occluded,
    );
    let hidden_frame = commit_frame(
        &mirror,
        mirror
            .propose(hidden, MirrorLimits::default())
            .expect("hidden"),
    );
    let state = reducer
        .apply(
            &hidden_frame
                .to_incremental_message("2026-08-03T08:00:01Z")
                .expect("wire"),
        )
        .expect("apply hidden");
    assert_eq!(state.facts["block:0,64,0"], json!("stone"));
    assert_eq!(
        state.unverified["block:0,64,0"],
        ViewportUnverifiedReason::Occluded
    );
    assert!(state.repair_required);

    let unrelated = commit_frame(
        &mirror,
        mirror
            .propose(
                observation(scope.clone(), &[("block:1,64,0", json!("dirt"))]),
                MirrorLimits::default(),
            )
            .expect("unrelated change"),
    );
    let state = reducer
        .apply(
            &unrelated
                .to_incremental_message("2026-08-03T08:00:02Z")
                .expect("wire"),
        )
        .expect("apply unrelated");
    assert!(state.repair_required);

    let reconfirmed = commit_frame(
        &mirror,
        mirror
            .propose(
                observation(
                    scope,
                    &[
                        ("block:0,64,0", json!("stone")),
                        ("block:1,64,0", json!("dirt")),
                    ],
                ),
                MirrorLimits::default(),
            )
            .expect("reconfirmed"),
    );
    let state = reducer
        .apply(
            &reconfirmed
                .to_incremental_message("2026-08-03T08:00:03Z")
                .expect("wire"),
        )
        .expect("apply reconfirmed");
    assert!(!state.repair_required);
}
