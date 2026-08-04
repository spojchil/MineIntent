//! Receiver-side replay for the experimental viewport-frame.v3 chain.
//!
//! A producer may finish reads in any order, but a model context can only apply a
//! delta whose base is the context's current baseline. This reducer makes that
//! rule executable: keyframes replace state, deltas compare-and-advance, and a
//! missing or out-of-order frame is rejected without mutating the state.

use std::collections::BTreeMap;

use mineintent_contracts::agent::{
    ViewportBaselineId, ViewportIncrementalFrameError, ViewportIncrementalFrameMessageV1,
    ViewportIncrementalPayloadV1, ViewportKeyframeV1, ViewportScope, ViewportScopeError,
    ViewportUnverifiedReason,
};
use serde_json::Value;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq)]
pub struct ViewportReducedState {
    pub scope: ViewportScope,
    pub baseline_id: ViewportBaselineId,
    pub facts: BTreeMap<String, Value>,
    pub unverified: BTreeMap<String, ViewportUnverifiedReason>,
    pub repair_required: bool,
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum ViewportReplayError {
    #[error("viewport frame wire validation failed: {0}")]
    InvalidFrame(ViewportIncrementalFrameError),
    #[error("viewport scope is invalid: {0}")]
    InvalidScope(ViewportScopeError),
    #[error("viewport frame scope does not match the receiver scope")]
    ScopeMismatch {
        expected: Box<ViewportScope>,
        actual: Box<ViewportScope>,
    },
    #[error("viewport delta arrived before a keyframe")]
    MissingBaseline,
    #[error("viewport delta base does not match the receiver baseline")]
    BaseMismatch {
        expected: ViewportBaselineId,
        actual: ViewportBaselineId,
    },
    #[error("viewport baseline has already been applied")]
    DuplicateBaseline(ViewportBaselineId),
    #[error("viewport baseline moves backwards")]
    BaselineRegression,
    #[error("viewport delta skips a baseline")]
    BaselineGap,
    #[error("viewport delta adds an already-known fact: {0}")]
    AddedExisting(String),
    #[error("viewport delta changes an unknown fact: {0}")]
    ChangedUnknown(String),
    #[error("viewport delta removes an unknown fact: {0}")]
    RemovedUnknown(String),
}

/// Applies v3 frames for one model context.
#[derive(Clone, Debug)]
pub struct ViewportIncrementalReducer {
    scope: ViewportScope,
    state: Option<ViewportReducedState>,
}

impl ViewportIncrementalReducer {
    pub fn new(scope: ViewportScope) -> Result<Self, ViewportReplayError> {
        scope
            .validate()
            .map_err(ViewportReplayError::InvalidScope)?;
        Ok(Self { scope, state: None })
    }

    pub fn scope(&self) -> &ViewportScope {
        &self.scope
    }

    pub fn state(&self) -> Option<&ViewportReducedState> {
        self.state.as_ref()
    }

    /// Explicitly moves the receiver to a new world/context namespace.
    /// A keyframe for the new scope must still be applied after this call.
    pub fn switch_scope(&mut self, scope: ViewportScope) -> Result<(), ViewportReplayError> {
        scope
            .validate()
            .map_err(ViewportReplayError::InvalidScope)?;
        self.scope = scope;
        self.state = None;
        Ok(())
    }

    /// Applies one frame atomically. On error, the reducer remains unchanged.
    pub fn apply(
        &mut self,
        frame: &ViewportIncrementalFrameMessageV1,
    ) -> Result<ViewportReducedState, ViewportReplayError> {
        frame
            .validate()
            .map_err(ViewportReplayError::InvalidFrame)?;
        if frame.scope != self.scope {
            return Err(ViewportReplayError::ScopeMismatch {
                expected: Box::new(self.scope.clone()),
                actual: Box::new(frame.scope.clone()),
            });
        }

        let next = match &frame.payload {
            ViewportIncrementalPayloadV1::Keyframe {
                viewport,
                unverified,
                complete,
                omitted,
                ..
            } => self.apply_keyframe(frame, viewport, unverified, *complete, *omitted)?,
            ViewportIncrementalPayloadV1::Delta {
                delta,
                complete,
                omitted,
            } => self.apply_delta(frame, delta, *complete, *omitted)?,
        };
        self.state = Some(next.clone());
        Ok(next)
    }

    fn apply_keyframe(
        &self,
        frame: &ViewportIncrementalFrameMessageV1,
        viewport: &ViewportKeyframeV1,
        unverified: &BTreeMap<String, ViewportUnverifiedReason>,
        complete: bool,
        omitted: u64,
    ) -> Result<ViewportReducedState, ViewportReplayError> {
        if frame.base_baseline_id.is_some() {
            return Err(ViewportReplayError::InvalidFrame(
                ViewportIncrementalFrameError::KeyframeHasBase,
            ));
        }
        if self
            .state
            .as_ref()
            .is_some_and(|state| frame.baseline_id <= state.baseline_id)
        {
            return Err(ViewportReplayError::BaselineRegression);
        }
        Ok(ViewportReducedState {
            scope: self.scope.clone(),
            baseline_id: frame.baseline_id,
            facts: viewport.facts.clone(),
            unverified: unverified.clone(),
            repair_required: !complete || omitted > 0,
        })
    }

    fn apply_delta(
        &self,
        frame: &ViewportIncrementalFrameMessageV1,
        delta: &mineintent_contracts::agent::ViewportDeltaV1,
        complete: bool,
        omitted: u64,
    ) -> Result<ViewportReducedState, ViewportReplayError> {
        let Some(current) = &self.state else {
            return Err(ViewportReplayError::MissingBaseline);
        };
        let Some(base) = frame.base_baseline_id else {
            return Err(ViewportReplayError::InvalidFrame(
                ViewportIncrementalFrameError::DeltaMissingBase,
            ));
        };
        if base != current.baseline_id {
            return Err(ViewportReplayError::BaseMismatch {
                expected: current.baseline_id,
                actual: base,
            });
        }
        if frame.baseline_id == current.baseline_id {
            return Err(ViewportReplayError::DuplicateBaseline(frame.baseline_id));
        }
        if frame.baseline_id.epoch != current.baseline_id.epoch
            || frame.baseline_id.sequence != current.baseline_id.sequence.saturating_add(1)
        {
            return Err(ViewportReplayError::BaselineGap);
        }

        let mut facts = current.facts.clone();
        let mut unverified = current.unverified.clone();
        for (key, value) in &delta.added {
            if facts.contains_key(key) || unverified.contains_key(key) {
                return Err(ViewportReplayError::AddedExisting(key.clone()));
            }
            facts.insert(key.clone(), value.clone());
        }
        for (key, value) in &delta.changed {
            if !facts.contains_key(key) && !unverified.contains_key(key) {
                return Err(ViewportReplayError::ChangedUnknown(key.clone()));
            }
            facts.insert(key.clone(), value.clone());
            unverified.remove(key);
        }
        for key in &delta.confirmed_removed {
            if facts.remove(key).is_none() && unverified.remove(key).is_none() {
                return Err(ViewportReplayError::RemovedUnknown(key.clone()));
            }
            unverified.remove(key);
        }
        for (key, reason) in &delta.unverified {
            unverified.insert(key.clone(), *reason);
        }

        let repair_required = !complete || omitted > 0 || !unverified.is_empty();
        Ok(ViewportReducedState {
            scope: self.scope.clone(),
            baseline_id: frame.baseline_id,
            facts,
            unverified,
            repair_required,
        })
    }
}
