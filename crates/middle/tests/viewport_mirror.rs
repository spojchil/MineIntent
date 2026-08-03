use std::collections::{BTreeMap, BTreeSet};

use mineintent_contracts::agent::{ViewportBaselineId, ViewportScope, ViewportUnverifiedReason};
use mineintent_contracts::minecraft::{
    BlockInfo, ViewportCoordinateSystem, ViewportFrame as MinecraftViewportFrame, ViewportFullV2,
    ViewportLegend, ViewportProtocolV2, ViewportSelfPose, VisibleBlocksView, VisibleEntitiesView,
};
use mineintent_middle::agent::{
    KeyframeReason, MirrorLimits, ViewportCommitError, ViewportFrame, ViewportMirror,
    ViewportObservation, ViewportProposal,
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
        confirmed_removed: BTreeSet::new(),
        unverified: BTreeMap::new(),
        truncated: false,
    }
}

fn commit_pending(mirror: &ViewportMirror, proposal: ViewportProposal) -> ViewportFrame {
    mirror
        .commit(proposal.pending().expect("pending frame"))
        .expect("commit succeeds")
}

fn full_view(blocks: Vec<(BlockInfo, i32, i32, i32)>, truncated: bool) -> ViewportFullV2 {
    ViewportFullV2 {
        protocol: ViewportProtocolV2::V2,
        frame: MinecraftViewportFrame {
            coordinates: ViewportCoordinateSystem::MinecraftWorldAbsolute,
            self_pose: ViewportSelfPose {
                position: [0.0, 64.0, 0.0],
                yaw_degrees: 0.0,
                pitch_degrees: 0.0,
            },
            legend: ViewportLegend {
                visible_entities: "entities".to_owned(),
                visible_blocks: "blocks".to_owned(),
            },
        },
        looked_at_block: None,
        visible_entities: VisibleEntitiesView {
            items: Vec::new(),
            truncated: false,
        },
        visible_blocks: VisibleBlocksView { blocks, truncated },
    }
}

#[test]
fn first_observation_is_keyframe_and_second_identical_observation_is_empty() {
    let scope = scope("minecraft:overworld");
    let mirror = ViewportMirror::new(scope.clone()).expect("mirror");
    let first = mirror
        .propose(
            observation(
                scope.clone(),
                &[
                    ("block:0,64,0", json!("stone")),
                    ("block:1,64,0", json!("air")),
                ],
            ),
            MirrorLimits::default(),
        )
        .expect("first proposal");
    let frame = commit_pending(&mirror, first);
    let ViewportFrame::Keyframe {
        reason,
        baseline_id,
        complete,
        ..
    } = frame
    else {
        panic!("first frame must be a keyframe")
    };
    assert_eq!(reason, KeyframeReason::Initial);
    assert_eq!(baseline_id, ViewportBaselineId::new(0, 1));
    assert!(complete);

    let second = mirror
        .propose(
            observation(
                scope,
                &[
                    ("block:0,64,0", json!("stone")),
                    ("block:1,64,0", json!("air")),
                ],
            ),
            MirrorLimits::default(),
        )
        .expect("second proposal");
    assert!(matches!(
        second,
        ViewportProposal::NoChange {
            baseline_id: Some(ViewportBaselineId {
                epoch: 0,
                sequence: 1
            }),
            truncated: false,
        }
    ));
}

#[test]
fn missing_fact_is_not_implicitly_air_and_confirmed_removal_is_explicit() {
    let scope = scope("minecraft:overworld");
    let mirror = ViewportMirror::new(scope.clone()).expect("mirror");
    commit_pending(
        &mirror,
        mirror
            .propose(
                observation(scope.clone(), &[("block:0,64,0", json!("stone"))]),
                MirrorLimits::default(),
            )
            .expect("initial"),
    );

    let absent = ViewportObservation::empty(scope.clone());
    assert!(matches!(
        mirror
            .propose(absent, MirrorLimits::default())
            .expect("absence"),
        ViewportProposal::NoChange { .. }
    ));

    let mut removed = ViewportObservation::empty(scope);
    removed.confirmed_removed.insert("block:0,64,0".to_owned());
    let frame = commit_pending(
        &mirror,
        mirror
            .propose(removed, MirrorLimits::default())
            .expect("removal"),
    );
    let ViewportFrame::Delta { delta, .. } = frame else {
        panic!("removal must be a delta")
    };
    assert_eq!(delta.confirmed_removed, vec!["block:0,64,0"]);
}

#[test]
fn full_view_adapter_uses_canonical_block_keys_and_never_infers_removal() {
    let scope = scope("minecraft:overworld");
    let mirror = ViewportMirror::new(scope).expect("mirror");
    let first = commit_pending(
        &mirror,
        mirror
            .propose_full(
                &full_view(vec![(BlockInfo::bare("stone"), -1, 64, 2)], false),
                MirrorLimits::default(),
            )
            .expect("full keyframe"),
    );
    let ViewportFrame::Keyframe { facts, .. } = first else {
        panic!("first full view must be a keyframe")
    };
    assert_eq!(facts["block:-1,64,2"], json!("stone"));

    let second = commit_pending(
        &mirror,
        mirror
            .propose_full(&full_view(Vec::new(), false), MirrorLimits::default())
            .expect("missing fact is uncertain"),
    );
    let ViewportFrame::Delta { delta, .. } = second else {
        panic!("uncertainty must be a delta")
    };
    assert!(delta.confirmed_removed.is_empty());
    assert_eq!(
        delta.unverified["block:-1,64,2"],
        ViewportUnverifiedReason::NotObserved
    );
}

#[test]
fn unverified_keeps_the_last_fact_and_reconfirmation_is_a_change() {
    let scope = scope("minecraft:overworld");
    let mirror = ViewportMirror::new(scope.clone()).expect("mirror");
    commit_pending(
        &mirror,
        mirror
            .propose(
                observation(scope.clone(), &[("block:0,64,0", json!("stone"))]),
                MirrorLimits::default(),
            )
            .expect("initial"),
    );

    let mut hidden = ViewportObservation::empty(scope.clone());
    hidden.unverified.insert(
        "block:0,64,0".to_owned(),
        ViewportUnverifiedReason::Occluded,
    );
    let hidden_frame = commit_pending(
        &mirror,
        mirror
            .propose(hidden, MirrorLimits::default())
            .expect("hidden"),
    );
    let ViewportFrame::Delta { delta, .. } = hidden_frame else {
        panic!("hidden fact must be a delta")
    };
    assert_eq!(
        delta.unverified.get("block:0,64,0"),
        Some(&ViewportUnverifiedReason::Occluded)
    );

    let reconfirmed = commit_pending(
        &mirror,
        mirror
            .propose(
                observation(scope, &[("block:0,64,0", json!("stone"))]),
                MirrorLimits::default(),
            )
            .expect("reconfirmation"),
    );
    let ViewportFrame::Delta { delta, .. } = reconfirmed else {
        panic!("reconfirmation must be visible")
    };
    assert_eq!(delta.changed.get("block:0,64,0"), Some(&json!("stone")));
}

#[test]
fn delta_budget_commits_only_emitted_changes_and_converges() {
    let scope = scope("minecraft:overworld");
    let mirror = ViewportMirror::new(scope.clone()).expect("mirror");
    commit_pending(
        &mirror,
        mirror
            .propose(
                observation(scope.clone(), &[]),
                MirrorLimits {
                    max_delta_changes: 2,
                    max_keyframe_entries: 8,
                },
            )
            .expect("initial"),
    );

    let current = observation(
        scope.clone(),
        &[
            ("block:0,64,0", json!("stone")),
            ("block:1,64,0", json!("dirt")),
            ("block:2,64,0", json!("sand")),
        ],
    );
    let limits = MirrorLimits {
        max_delta_changes: 2,
        max_keyframe_entries: 8,
    };
    let first = commit_pending(
        &mirror,
        mirror.propose(current.clone(), limits).expect("batch 1"),
    );
    let ViewportFrame::Delta {
        delta,
        omitted,
        complete,
        ..
    } = first
    else {
        panic!("change batch must be delta")
    };
    assert_eq!(delta.change_count(), 2);
    assert_eq!(omitted, 1);
    assert!(!complete);

    let second = commit_pending(
        &mirror,
        mirror.propose(current.clone(), limits).expect("batch 2"),
    );
    let ViewportFrame::Delta {
        delta,
        omitted,
        complete,
        ..
    } = second
    else {
        panic!("remaining change must be delta")
    };
    assert_eq!(delta.change_count(), 1);
    assert_eq!(omitted, 0);
    assert!(complete);

    assert!(matches!(
        mirror.propose(current, limits).expect("converged"),
        ViewportProposal::NoChange {
            truncated: false,
            ..
        }
    ));
}

#[test]
fn keyframe_overflow_is_queued_and_converges_without_repeating_the_observation() {
    let scope = scope("minecraft:overworld");
    let mirror = ViewportMirror::new(scope.clone()).expect("mirror");
    let limits = MirrorLimits {
        max_delta_changes: 1,
        max_keyframe_entries: 1,
    };
    let first = commit_pending(
        &mirror,
        mirror
            .propose(
                observation(
                    scope.clone(),
                    &[
                        ("block:0,64,0", json!("stone")),
                        ("block:1,64,0", json!("dirt")),
                        ("block:2,64,0", json!("sand")),
                    ],
                ),
                limits,
            )
            .expect("keyframe"),
    );
    assert_eq!(first.omitted(), 2);

    for expected_remaining in [1, 0] {
        let repair = commit_pending(
            &mirror,
            mirror
                .propose(ViewportObservation::empty(scope.clone()), limits)
                .expect("queued repair"),
        );
        let ViewportFrame::Delta { delta, omitted, .. } = repair else {
            panic!("keyframe overflow repairs must be deltas")
        };
        assert_eq!(delta.added.len(), 1);
        assert_eq!(omitted, expected_remaining);
    }
    assert!(matches!(
        mirror
            .propose(ViewportObservation::empty(scope), limits)
            .expect("converged"),
        ViewportProposal::NoChange { .. }
    ));
}

#[test]
fn forced_keyframe_preserves_committed_facts_when_the_new_scan_is_empty() {
    let scope = scope("minecraft:overworld");
    let mirror = ViewportMirror::new(scope.clone()).expect("mirror");
    commit_pending(
        &mirror,
        mirror
            .propose(
                observation(scope.clone(), &[("block:0,64,0", json!("stone"))]),
                MirrorLimits::default(),
            )
            .expect("initial"),
    );

    mirror.force_keyframe();
    let frame = commit_pending(
        &mirror,
        mirror
            .propose(ViewportObservation::empty(scope), MirrorLimits::default())
            .expect("forced keyframe"),
    );
    let ViewportFrame::Keyframe {
        facts,
        complete,
        reason,
        ..
    } = frame
    else {
        panic!("forced proposal must be a keyframe")
    };
    assert_eq!(facts["block:0,64,0"], json!("stone"));
    assert!(complete);
    assert_eq!(reason, KeyframeReason::Forced);
}

#[test]
fn complete_scan_emits_an_empty_delta_to_clear_repair_required() {
    let scope = scope("minecraft:overworld");
    let mirror = ViewportMirror::new(scope.clone()).expect("mirror");
    let limits = MirrorLimits::default();

    let mut truncated = observation(scope.clone(), &[("block:0,64,0", json!("stone"))]);
    truncated.truncated = true;
    let first = commit_pending(
        &mirror,
        mirror.propose(truncated, limits).expect("truncated"),
    );
    let ViewportFrame::Keyframe { complete, .. } = first else {
        panic!("first frame must be a keyframe")
    };
    assert!(!complete);

    let second = commit_pending(
        &mirror,
        mirror
            .propose(
                observation(scope.clone(), &[("block:0,64,0", json!("stone"))]),
                limits,
            )
            .expect("completion acknowledgement"),
    );
    let ViewportFrame::Delta {
        delta,
        complete,
        omitted,
        ..
    } = second
    else {
        panic!("completion acknowledgement must be a delta")
    };
    assert_eq!(delta.change_count(), 0);
    assert!(complete);
    assert_eq!(omitted, 0);
    assert!(matches!(
        mirror
            .propose(
                observation(scope, &[("block:0,64,0", json!("stone"))]),
                limits,
            )
            .expect("converged"),
        ViewportProposal::NoChange {
            truncated: false,
            ..
        }
    ));
}

#[test]
fn overflowed_confirmed_removal_survives_without_repeat_evidence() {
    let scope = scope("minecraft:overworld");
    let mirror = ViewportMirror::new(scope.clone()).expect("mirror");
    commit_pending(
        &mirror,
        mirror
            .propose(
                observation(scope.clone(), &[("block:9,64,0", json!("stone"))]),
                MirrorLimits::default(),
            )
            .expect("initial"),
    );

    let mut one_time = observation(scope.clone(), &[("block:0,64,0", json!("dirt"))]);
    one_time.confirmed_removed.insert("block:9,64,0".to_owned());
    let limits = MirrorLimits {
        max_delta_changes: 1,
        max_keyframe_entries: 8,
    };
    let first = commit_pending(
        &mirror,
        mirror.propose(one_time, limits).expect("bounded delta"),
    );
    let ViewportFrame::Delta { delta, omitted, .. } = first else {
        panic!("bounded change must be delta")
    };
    assert_eq!(delta.added.len(), 1);
    assert!(delta.confirmed_removed.is_empty());
    assert_eq!(omitted, 1);

    let repair = commit_pending(
        &mirror,
        mirror
            .propose_full(
                &full_view(vec![(BlockInfo::bare("dirt"), 0, 64, 0)], false),
                limits,
            )
            .expect("pending removal"),
    );
    let ViewportFrame::Delta { delta, omitted, .. } = repair else {
        panic!("pending removal must be delta")
    };
    assert_eq!(delta.confirmed_removed, vec!["block:9,64,0"]);
    assert_eq!(omitted, 0);
}

#[test]
fn pending_verdicts_are_fifo_and_same_key_updates_keep_their_age() {
    let scope = scope("minecraft:overworld");
    let mirror = ViewportMirror::new(scope.clone()).expect("mirror");
    let limits = MirrorLimits {
        max_delta_changes: 1,
        max_keyframe_entries: 8,
    };
    commit_pending(
        &mirror,
        mirror
            .propose(ViewportObservation::empty(scope.clone()), limits)
            .expect("initial"),
    );

    // z2 and z3 are older than every a* key introduced later. The latter must
    // not jump ahead merely because BTreeMap would sort it first.
    commit_pending(
        &mirror,
        mirror
            .propose(
                observation(
                    scope.clone(),
                    &[
                        ("block:z1", json!("one")),
                        ("block:z2", json!("two")),
                        ("block:z3", json!("three")),
                    ],
                ),
                limits,
            )
            .expect("z1"),
    );

    let second = commit_pending(
        &mirror,
        mirror
            .propose(
                observation(scope.clone(), &[("block:a0", json!("old"))]),
                limits,
            )
            .expect("z2"),
    );
    let ViewportFrame::Delta { delta, .. } = second else {
        panic!("expected the oldest queued z2 verdict")
    };
    assert!(delta.added.contains_key("block:z2"));

    let third = commit_pending(
        &mirror,
        mirror
            .propose(
                observation(
                    scope.clone(),
                    &[("block:a0", json!("updated")), ("block:a1", json!("one"))],
                ),
                limits,
            )
            .expect("z3"),
    );
    let ViewportFrame::Delta { delta, .. } = third else {
        panic!("expected the next oldest queued z3 verdict")
    };
    assert!(delta.added.contains_key("block:z3"));

    let fourth = commit_pending(
        &mirror,
        mirror
            .propose(
                observation(
                    scope.clone(),
                    &[("block:a0", json!("newest")), ("block:a2", json!("two"))],
                ),
                limits,
            )
            .expect("a0 after the old queue drains"),
    );
    let ViewportFrame::Delta { delta, .. } = fourth else {
        panic!("expected a FIFO a0 verdict")
    };
    assert_eq!(delta.added.get("block:a0"), Some(&json!("newest")));
}

#[test]
fn incomplete_frame_can_be_cleared_by_an_empty_complete_delta() {
    let scope = scope("minecraft:overworld");
    let mirror = ViewportMirror::new(scope.clone()).expect("mirror");
    let limits = MirrorLimits::default();
    let incomplete = commit_pending(
        &mirror,
        mirror
            .propose(
                ViewportObservation {
                    truncated: true,
                    ..ViewportObservation::empty(scope.clone())
                },
                limits,
            )
            .expect("incomplete keyframe"),
    );
    assert!(matches!(
        incomplete,
        ViewportFrame::Keyframe {
            complete: false,
            omitted: 0,
            ..
        }
    ));

    let completion = commit_pending(
        &mirror,
        mirror
            .propose(ViewportObservation::empty(scope.clone()), limits)
            .expect("completion acknowledgement"),
    );
    assert!(matches!(
        completion,
        ViewportFrame::Delta {
            complete: true,
            omitted: 0,
            delta: ref value,
            ..
        } if value.change_count() == 0
    ));
    assert!(matches!(
        mirror.propose(ViewportObservation::empty(scope), limits),
        Ok(ViewportProposal::NoChange { .. })
    ));
}

#[test]
fn concurrent_proposals_use_compare_and_commit_and_stale_one_is_rejected() {
    let scope = scope("minecraft:overworld");
    let mirror = ViewportMirror::new(scope.clone()).expect("mirror");
    let initial = mirror
        .propose(
            observation(scope.clone(), &[("block:0,64,0", json!("stone"))]),
            MirrorLimits::default(),
        )
        .expect("initial");
    commit_pending(&mirror, initial);

    let left = mirror
        .propose(
            observation(scope.clone(), &[("block:0,64,0", json!("dirt"))]),
            MirrorLimits::default(),
        )
        .expect("left proposal");
    let right = mirror
        .propose(
            observation(scope, &[("block:0,64,0", json!("sand"))]),
            MirrorLimits::default(),
        )
        .expect("right proposal");

    let left_pending = left.pending().expect("left pending");
    let right_pending = right.pending().expect("right pending");
    mirror.commit(left_pending).expect("one proposal commits");
    assert!(matches!(
        mirror.commit(right_pending),
        Err(ViewportCommitError::StaleProposal)
    ));
}

#[test]
fn switching_scope_invalidates_old_baseline_and_starts_new_epoch() {
    let overworld = scope("minecraft:overworld");
    let nether = scope("minecraft:the_nether");
    let mirror = ViewportMirror::new(overworld.clone()).expect("mirror");
    commit_pending(
        &mirror,
        mirror
            .propose(
                observation(overworld, &[("block:0,64,0", json!("stone"))]),
                MirrorLimits::default(),
            )
            .expect("initial"),
    );
    assert_eq!(mirror.epoch(), 0);
    mirror.switch_scope(nether.clone()).expect("scope switch");
    assert_eq!(mirror.epoch(), 1);
    assert_eq!(mirror.baseline_id(), None);

    let frame = commit_pending(
        &mirror,
        mirror
            .propose(
                observation(nether, &[("block:0,64,0", json!("netherrack"))]),
                MirrorLimits::default(),
            )
            .expect("new scope keyframe"),
    );
    let ViewportFrame::Keyframe { baseline_id, .. } = frame else {
        panic!("scope switch must force a keyframe")
    };
    assert_eq!(baseline_id, ViewportBaselineId::new(1, 1));
}

#[test]
fn invalidating_after_propose_makes_pending_frame_stale() {
    let scope = scope("minecraft:overworld");
    let mirror = ViewportMirror::new(scope.clone()).expect("mirror");
    let pending = mirror
        .propose(
            observation(scope, &[("block:0,64,0", json!("stone"))]),
            MirrorLimits::default(),
        )
        .expect("proposal")
        .pending()
        .expect("pending");
    mirror.invalidate().expect("invalidate");
    assert!(matches!(
        mirror.commit(pending),
        Err(ViewportCommitError::StaleProposal)
    ));
}

#[test]
fn wire_failure_before_publish_does_not_advance_the_baseline() {
    let scope = scope("minecraft:overworld");
    let mirror = ViewportMirror::new(scope.clone()).expect("mirror");
    let pending = mirror
        .propose(
            observation(scope, &[("block:0,64,0", json!("stone"))]),
            MirrorLimits::default(),
        )
        .expect("proposal")
        .pending()
        .expect("pending");
    assert!(pending.to_incremental_message("").is_err());
    assert_eq!(mirror.baseline_id(), None);
}
