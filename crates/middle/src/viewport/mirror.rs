//! A model-context viewport mirror with an explicit, compare-and-commit baseline.
//!
//! This module deliberately sits above the backend viewport kernel.  The backend
//! owns world truth; this mirror owns only the facts that have been admitted to a
//! model context.  A proposal does not mutate the mirror.  The caller commits it
//! only after the corresponding frame has been put into the next model request.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex, MutexGuard},
};

use mineintent_contracts::agent::{
    ViewportBaselineId, ViewportDeltaV1, ViewportIncrementalFrameError,
    ViewportIncrementalFrameMessageV1, ViewportIncrementalPayloadV1, ViewportKeyframeV1,
    ViewportScope, ViewportUnverifiedReason,
};
use mineintent_contracts::minecraft::ViewportFullV2;
use serde_json::Value;
use thiserror::Error;

/// A completed observation supplied by a viewport producer.
///
/// `observed` contains only facts positively established by this read.  A key
/// absent from it is not an empty block and is not a removal.  Producers must
/// use `confirmed_removed` when they have an authoritative empty/removal
/// verdict, and `unverified` when visibility/loading/output budget prevents a
/// conclusion.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewportObservation {
    pub scope: ViewportScope,
    pub observed: BTreeMap<String, Value>,
    pub confirmed_removed: BTreeSet<String>,
    pub unverified: BTreeMap<String, ViewportUnverifiedReason>,
    pub truncated: bool,
}

impl ViewportObservation {
    pub fn empty(scope: ViewportScope) -> Self {
        Self {
            scope,
            observed: BTreeMap::new(),
            confirmed_removed: BTreeSet::new(),
            unverified: BTreeMap::new(),
            truncated: false,
        }
    }

    fn validate(&self) -> Result<(), ViewportMirrorError> {
        self.scope
            .validate()
            .map_err(|error| ViewportMirrorError::InvalidObservation(error.to_string()))?;

        let mut keys = BTreeSet::new();
        for key in self.observed.keys() {
            validate_key(key)?;
            keys.insert(key.as_str());
        }
        for key in &self.confirmed_removed {
            validate_key(key)?;
            if !keys.insert(key.as_str()) {
                return Err(ViewportMirrorError::ContradictoryObservation { key: key.clone() });
            }
        }
        for key in self.unverified.keys() {
            validate_key(key)?;
            if !keys.insert(key.as_str()) {
                return Err(ViewportMirrorError::ContradictoryObservation { key: key.clone() });
            }
        }
        Ok(())
    }

    /// Builds the positive block facts available in the current full viewport.
    ///
    /// `ViewportFullV2` has no authoritative vanished-block set, so absence is
    /// intentionally left for `ViewportMirror::propose_full` to classify as
    /// unverified rather than silently turning it into air/removal.
    pub fn from_full(
        scope: ViewportScope,
        viewport: &ViewportFullV2,
    ) -> Result<Self, ViewportMirrorError> {
        let mut observed = BTreeMap::new();
        for (block, x, y, z) in &viewport.visible_blocks.blocks {
            let key = block_fact_key(*x, *y, *z);
            let value = serde_json::to_value(block)
                .map_err(|error| ViewportMirrorError::Serialization(error.to_string()))?;
            observed.insert(key, value);
        }
        Ok(Self {
            scope,
            observed,
            confirmed_removed: BTreeSet::new(),
            unverified: BTreeMap::new(),
            truncated: viewport.visible_blocks.truncated,
        })
    }
}

/// Stable fact key for a world-absolute block voxel.
pub fn block_fact_key(x: i32, y: i32, z: i32) -> String {
    format!("block:{x},{y},{z}")
}

fn validate_key(key: &str) -> Result<(), ViewportMirrorError> {
    if key.trim().is_empty() || key.chars().any(char::is_control) {
        return Err(ViewportMirrorError::InvalidKey);
    }
    Ok(())
}

/// Output limits are applied to changes, not to the producer's scan.  A
/// truncated proposal commits only the entries it actually carries, so the
/// remaining changes are emitted by a later proposal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MirrorLimits {
    pub max_delta_changes: usize,
    pub max_keyframe_entries: usize,
}

impl Default for MirrorLimits {
    fn default() -> Self {
        Self {
            max_delta_changes: 128,
            max_keyframe_entries: 256,
        }
    }
}

/// The reason a proposal has to be represented as a keyframe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyframeReason {
    Initial,
    Forced,
}

/// A model-visible frame before it is committed to the mirror.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewportFrame {
    Keyframe {
        scope: ViewportScope,
        baseline_id: ViewportBaselineId,
        facts: BTreeMap<String, Value>,
        unverified: BTreeMap<String, ViewportUnverifiedReason>,
        omitted: usize,
        complete: bool,
        reason: KeyframeReason,
    },
    Delta {
        scope: ViewportScope,
        base_baseline_id: ViewportBaselineId,
        baseline_id: ViewportBaselineId,
        delta: ViewportDeltaV1,
        omitted: usize,
        complete: bool,
    },
}

impl ViewportFrame {
    pub fn scope(&self) -> &ViewportScope {
        match self {
            Self::Keyframe { scope, .. } | Self::Delta { scope, .. } => scope,
        }
    }

    pub fn baseline_id(&self) -> ViewportBaselineId {
        match self {
            Self::Keyframe { baseline_id, .. } | Self::Delta { baseline_id, .. } => *baseline_id,
        }
    }

    pub fn is_keyframe(&self) -> bool {
        matches!(self, Self::Keyframe { .. })
    }

    pub fn omitted(&self) -> usize {
        match self {
            Self::Keyframe { omitted, .. } | Self::Delta { omitted, .. } => *omitted,
        }
    }

    /// Converts the internal frame into the standalone v3 model message.
    ///
    /// Keyframes use a small object with a stable `facts` member so a receiver
    /// can replay the chain without knowing the backend DTO. Full pose/entity
    /// metadata remains on the ordinary viewport result until a later wire
    /// revision explicitly carries it here.
    pub fn to_incremental_message(
        &self,
        at: impl Into<String>,
    ) -> Result<ViewportIncrementalFrameMessageV1, ViewportMirrorError> {
        let payload = match self {
            Self::Keyframe {
                facts,
                unverified,
                complete,
                omitted,
                ..
            } => ViewportIncrementalPayloadV1::Keyframe {
                viewport: ViewportKeyframeV1::new(facts.clone())
                    .map_err(|error| ViewportMirrorError::InvalidKeyframe(error.to_string()))?,
                unverified: unverified.clone(),
                complete: *complete,
                omitted: (*omitted)
                    .try_into()
                    .map_err(|_| ViewportMirrorError::CountOverflow)?,
            },
            Self::Delta {
                delta,
                complete,
                omitted,
                ..
            } => ViewportIncrementalPayloadV1::Delta {
                delta: delta.clone(),
                complete: *complete,
                omitted: (*omitted)
                    .try_into()
                    .map_err(|_| ViewportMirrorError::CountOverflow)?,
            },
        };
        ViewportIncrementalFrameMessageV1::new(
            at,
            self.scope().clone(),
            match self {
                Self::Keyframe { .. } => None,
                Self::Delta {
                    base_baseline_id, ..
                } => Some(*base_baseline_id),
            },
            self.baseline_id(),
            payload,
        )
        .map_err(ViewportMirrorError::InvalidWire)
    }
}

/// The result of preparing a read.  It is safe to send `frame` to the model,
/// but the mirror advances only when `commit` succeeds.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingViewportFrame {
    frame: ViewportFrame,
    expected_generation: u64,
    base_baseline_id: Option<ViewportBaselineId>,
    next_facts: BTreeMap<String, FactState>,
    next_pending: PendingVerdicts,
    next_repair_required: bool,
}

impl PendingViewportFrame {
    pub fn frame(&self) -> &ViewportFrame {
        &self.frame
    }

    /// Builds and validates the wire message before the baseline is committed.
    /// Callers must publish/append this message successfully before passing the
    /// pending value to `ViewportMirror::commit`.
    pub fn to_incremental_message(
        &self,
        at: impl Into<String>,
    ) -> Result<ViewportIncrementalFrameMessageV1, ViewportMirrorError> {
        self.frame.to_incremental_message(at)
    }
}

/// A proposal can be empty without advancing the baseline.  `truncated` is
/// retained even for an empty proposal so a caller can schedule a later repair.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewportProposal {
    NoChange {
        baseline_id: Option<ViewportBaselineId>,
        truncated: bool,
    },
    Pending(Box<PendingViewportFrame>),
}

impl ViewportProposal {
    pub fn pending(self) -> Option<PendingViewportFrame> {
        match self {
            Self::NoChange { .. } => None,
            Self::Pending(frame) => Some(*frame),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum FactState {
    Observed(Value),
    Unverified {
        last_observed: Option<Value>,
        reason: ViewportUnverifiedReason,
    },
}

#[derive(Debug)]
struct MirrorState {
    scope: ViewportScope,
    epoch: u64,
    baseline_id: Option<ViewportBaselineId>,
    facts: BTreeMap<String, FactState>,
    pending: PendingVerdicts,
    repair_required: bool,
    generation: u64,
    force_keyframe: bool,
}

/// One mirror should be owned by one participant/context.  The internal lock
/// only protects that owner from concurrent proposals; it is not a global
/// world mirror.
#[derive(Clone, Debug)]
pub struct ViewportMirror {
    state: Arc<Mutex<MirrorState>>,
}

impl ViewportMirror {
    pub fn new(scope: ViewportScope) -> Result<Self, ViewportMirrorError> {
        scope
            .validate()
            .map_err(|error| ViewportMirrorError::InvalidObservation(error.to_string()))?;
        Ok(Self {
            state: Arc::new(Mutex::new(MirrorState {
                scope,
                epoch: 0,
                baseline_id: None,
                facts: BTreeMap::new(),
                pending: PendingVerdicts::default(),
                repair_required: false,
                generation: 0,
                force_keyframe: false,
            })),
        })
    }

    pub fn scope(&self) -> ViewportScope {
        self.lock().scope.clone()
    }

    pub fn baseline_id(&self) -> Option<ViewportBaselineId> {
        self.lock().baseline_id
    }

    pub fn epoch(&self) -> u64 {
        self.lock().epoch
    }

    /// Invalidates the current context while keeping the same world scope.
    pub fn invalidate(&self) -> Result<(), ViewportMirrorError> {
        let mut state = self.lock();
        state.epoch = state
            .epoch
            .checked_add(1)
            .ok_or(ViewportMirrorError::EpochExhausted)?;
        state.baseline_id = None;
        state.facts.clear();
        state.pending.clear();
        state.repair_required = false;
        state.force_keyframe = true;
        state.generation = state.generation.wrapping_add(1);
        Ok(())
    }

    /// Switches the world/dimension namespace and invalidates the old chain.
    pub fn switch_scope(&self, scope: ViewportScope) -> Result<(), ViewportMirrorError> {
        scope
            .validate()
            .map_err(|error| ViewportMirrorError::InvalidObservation(error.to_string()))?;
        let mut state = self.lock();
        state.epoch = state
            .epoch
            .checked_add(1)
            .ok_or(ViewportMirrorError::EpochExhausted)?;
        state.scope = scope;
        state.baseline_id = None;
        state.facts.clear();
        state.pending.clear();
        state.repair_required = false;
        state.force_keyframe = true;
        state.generation = state.generation.wrapping_add(1);
        Ok(())
    }

    /// Causes the next successfully committed observation to be a keyframe.
    pub fn force_keyframe(&self) {
        let mut state = self.lock();
        state.force_keyframe = true;
        state.generation = state.generation.wrapping_add(1);
    }

    /// Computes a frame without mutating the committed baseline.
    pub fn propose(
        &self,
        observation: ViewportObservation,
        limits: MirrorLimits,
    ) -> Result<ViewportProposal, ViewportMirrorError> {
        observation.validate()?;
        if limits.max_delta_changes == 0 || limits.max_keyframe_entries == 0 {
            return Err(ViewportMirrorError::InvalidLimits);
        }

        let state = self.lock();
        Self::propose_locked_state(&state, &observation, limits)
    }

    /// Computes a frame while the caller holds a stable mirror snapshot.
    fn propose_locked_state(
        state: &MirrorState,
        observation: &ViewportObservation,
        limits: MirrorLimits,
    ) -> Result<ViewportProposal, ViewportMirrorError> {
        if observation.scope != state.scope {
            return Err(ViewportMirrorError::ScopeMismatch {
                expected: Box::new(state.scope.clone()),
                actual: Box::new(observation.scope.clone()),
            });
        }

        let next_id = next_baseline_id(state)?;
        if state.baseline_id.is_none() || state.force_keyframe {
            return Ok(ViewportProposal::Pending(Box::new(prepare_keyframe(
                state,
                observation,
                limits,
                next_id,
            ))));
        }

        let (delta, next_facts, next_pending, omitted) =
            prepare_delta(&state.facts, &state.pending, observation, limits)?;
        let next_repair_required = observation.truncated
            || omitted > 0
            || next_facts
                .values()
                .any(|fact| matches!(fact, FactState::Unverified { .. }));
        if delta.change_count() == 0 && next_repair_required == state.repair_required {
            return Ok(ViewportProposal::NoChange {
                baseline_id: state.baseline_id,
                truncated: observation.truncated || omitted > 0,
            });
        }

        let base_baseline_id = state
            .baseline_id
            .expect("a non-keyframe proposal has a baseline");
        let complete = !next_repair_required;
        Ok(ViewportProposal::Pending(Box::new(PendingViewportFrame {
            frame: ViewportFrame::Delta {
                scope: state.scope.clone(),
                base_baseline_id,
                baseline_id: next_id,
                delta,
                omitted,
                complete,
            },
            expected_generation: state.generation,
            base_baseline_id: Some(base_baseline_id),
            next_facts,
            next_pending,
            next_repair_required,
        })))
    }

    /// Admits positive block facts from the existing full viewport reader.
    ///
    /// The current backend contract does not provide an authoritative vanished
    /// block set. Remembered facts omitted by this projection therefore become
    /// `unverified`, never `confirmed_removed`.
    pub fn propose_full(
        &self,
        viewport: &ViewportFullV2,
        limits: MirrorLimits,
    ) -> Result<ViewportProposal, ViewportMirrorError> {
        if limits.max_delta_changes == 0 || limits.max_keyframe_entries == 0 {
            return Err(ViewportMirrorError::InvalidLimits);
        }
        let state = self.lock();
        let mut observation = ViewportObservation::from_full(state.scope.clone(), viewport)?;
        if state.baseline_id.is_some() {
            let reason = if viewport.visible_blocks.truncated {
                ViewportUnverifiedReason::OutputBudget
            } else {
                ViewportUnverifiedReason::NotObserved
            };
            // A pending verdict is newer authoritative evidence than the
            // absence in this projection. Do not replace it with synthetic
            // uncertainty before the pending frame gets a chance to publish.
            for key in state.facts.keys() {
                if !state.pending.contains_key(key) && !observation.observed.contains_key(key) {
                    observation.unverified.insert(key.clone(), reason);
                }
            }
        }
        observation.validate()?;
        Self::propose_locked_state(&state, &observation, limits)
    }

    /// Commits a previously proposed frame if no competing proposal/reset won
    /// the baseline in the meantime.
    pub fn commit(
        &self,
        pending: PendingViewportFrame,
    ) -> Result<ViewportFrame, ViewportCommitError> {
        let mut state = self.lock();
        if pending.expected_generation != state.generation
            || pending.base_baseline_id != state.baseline_id
            || pending.frame.scope() != &state.scope
        {
            return Err(ViewportCommitError::StaleProposal);
        }
        state.facts = pending.next_facts;
        state.pending = pending.next_pending;
        state.repair_required = pending.next_repair_required;
        state.baseline_id = Some(pending.frame.baseline_id());
        state.force_keyframe = false;
        state.generation = state.generation.wrapping_add(1);
        Ok(pending.frame)
    }

    fn lock(&self) -> MutexGuard<'_, MirrorState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn next_baseline_id(state: &MirrorState) -> Result<ViewportBaselineId, ViewportMirrorError> {
    let sequence = state
        .baseline_id
        .map(|id| id.sequence)
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(ViewportMirrorError::SequenceExhausted)?;
    Ok(ViewportBaselineId::new(state.epoch, sequence))
}

fn prepare_keyframe(
    state: &MirrorState,
    observation: &ViewportObservation,
    limits: MirrorLimits,
    next_id: ViewportBaselineId,
) -> PendingViewportFrame {
    // A forced keyframe is a checkpoint of the mirror's logical target, not a
    // second interpretation of only the latest partial scan.  Absence remains
    // non-evidence, while an already queued verdict is folded into the target.
    let mut target = state.facts.clone();
    for (key, verdict) in state.pending.iter() {
        match verdict {
            PendingVerdict::Present(fact) => {
                target.insert(key.clone(), fact.clone());
            }
            PendingVerdict::ConfirmedRemoved => {
                target.remove(key);
            }
        }
    }
    for (key, value) in &observation.observed {
        target.insert(key.clone(), FactState::Observed(value.clone()));
    }
    for key in &observation.confirmed_removed {
        target.remove(key);
    }
    for (key, reason) in &observation.unverified {
        let last_observed = target.get(key).and_then(last_observed_value);
        target.insert(
            key.clone(),
            FactState::Unverified {
                last_observed,
                reason: *reason,
            },
        );
    }

    let mut facts = BTreeMap::new();
    let mut next_facts = BTreeMap::new();
    let mut unverified = BTreeMap::new();
    let mut next_pending = PendingVerdicts::default();
    for (key, fact) in target {
        if next_facts.len() >= limits.max_keyframe_entries {
            next_pending.upsert(key, PendingVerdict::Present(fact));
            continue;
        }
        if let FactState::Unverified { reason, .. } = &fact {
            unverified.insert(key.clone(), *reason);
        }
        if let FactState::Unverified {
            last_observed: Some(value),
            ..
        } = &fact
        {
            facts.insert(key.clone(), value.clone());
        }
        if let FactState::Observed(value) = &fact {
            facts.insert(key.clone(), value.clone());
        }
        next_facts.insert(key, fact);
    }
    let omitted = next_pending.len();
    let next_repair_required = observation.truncated || omitted > 0 || !unverified.is_empty();
    let complete = !next_repair_required;
    PendingViewportFrame {
        frame: ViewportFrame::Keyframe {
            scope: state.scope.clone(),
            baseline_id: next_id,
            facts,
            unverified,
            omitted,
            complete,
            reason: if state.force_keyframe {
                KeyframeReason::Forced
            } else {
                KeyframeReason::Initial
            },
        },
        expected_generation: state.generation,
        // The wire keyframe has no delta base, but the local compare-and-
        // commit still must protect the baseline that existed when it was
        // prepared (especially for a forced keyframe).
        base_baseline_id: state.baseline_id,
        next_facts,
        next_pending,
        next_repair_required,
    }
}

#[derive(Clone, Debug, PartialEq)]
enum PendingVerdict {
    Present(FactState),
    ConfirmedRemoved,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct PendingVerdicts {
    by_key: BTreeMap<String, PendingVerdict>,
    order: VecDeque<String>,
}

impl PendingVerdicts {
    fn len(&self) -> usize {
        self.by_key.len()
    }

    fn clear(&mut self) {
        self.by_key.clear();
        self.order.clear();
    }

    fn contains_key(&self, key: &str) -> bool {
        self.by_key.contains_key(key)
    }

    fn get(&self, key: &str) -> Option<&PendingVerdict> {
        self.by_key.get(key)
    }

    fn iter(&self) -> impl Iterator<Item = (&String, &PendingVerdict)> {
        self.by_key.iter()
    }

    /// Inserts a new verdict at the tail, or updates an existing verdict in
    /// place so fresh evidence cannot reset an older key's age.
    fn upsert(&mut self, key: String, verdict: PendingVerdict) {
        if !self.by_key.contains_key(&key) {
            self.order.push_back(key.clone());
        }
        self.by_key.insert(key, verdict);
    }

    fn remove(&mut self, key: &str) -> Option<PendingVerdict> {
        let verdict = self.by_key.remove(key);
        if verdict.is_some() {
            let position = self
                .order
                .iter()
                .position(|queued| queued == key)
                .expect("pending queue and map must stay in sync");
            self.order.remove(position);
        }
        verdict
    }

    fn first_keys(&self, limit: usize) -> Vec<String> {
        self.order.iter().take(limit).cloned().collect()
    }
}

type PreparedDelta = (
    ViewportDeltaV1,
    BTreeMap<String, FactState>,
    PendingVerdicts,
    usize,
);

fn prepare_delta(
    current: &BTreeMap<String, FactState>,
    pending: &PendingVerdicts,
    observation: &ViewportObservation,
    limits: MirrorLimits,
) -> Result<PreparedDelta, ViewportMirrorError> {
    let mut operations = pending.clone();

    for (key, value) in &observation.observed {
        set_desired(
            current,
            &mut operations,
            key,
            PendingVerdict::Present(FactState::Observed(value.clone())),
        );
    }

    for key in &observation.confirmed_removed {
        set_desired(
            current,
            &mut operations,
            key,
            PendingVerdict::ConfirmedRemoved,
        );
    }

    for (key, reason) in &observation.unverified {
        let last_observed = effective_fact_for_unverified(current, &operations, key)
            .as_ref()
            .and_then(last_observed_value);
        set_desired(
            current,
            &mut operations,
            key,
            PendingVerdict::Present(FactState::Unverified {
                last_observed,
                reason: *reason,
            }),
        );
    }

    let selected_keys = operations.first_keys(limits.max_delta_changes);
    let mut delta = ViewportDeltaV1::default();
    let mut next_facts = current.clone();

    for key in selected_keys {
        let verdict = operations
            .get(&key)
            .cloned()
            .expect("selected viewport verdict must still be pending");
        let remainder = apply_verdict(&key, verdict, &mut delta, &mut next_facts);
        match remainder {
            Some(verdict) => operations.upsert(key, verdict),
            None => {
                operations.remove(&key);
            }
        }
    }
    let omitted = operations.len();

    delta
        .validate()
        .map_err(|error| ViewportMirrorError::InvalidDelta(error.to_string()))?;
    Ok((delta, next_facts, operations, omitted))
}

fn set_desired(
    current: &BTreeMap<String, FactState>,
    operations: &mut PendingVerdicts,
    key: &str,
    desired: PendingVerdict,
) {
    let already_current = match &desired {
        PendingVerdict::Present(fact) => current.get(key) == Some(fact),
        PendingVerdict::ConfirmedRemoved => !current.contains_key(key),
    };
    if already_current {
        operations.remove(key);
    } else {
        operations.upsert(key.to_owned(), desired);
    }
}

fn effective_fact_for_unverified<'a>(
    current: &'a BTreeMap<String, FactState>,
    operations: &'a PendingVerdicts,
    key: &str,
) -> Option<FactState> {
    match operations.get(key) {
        Some(PendingVerdict::Present(fact)) => Some(fact.clone()),
        // An unverified verdict supersedes a queued removal conservatively:
        // until the removal is published, retain the committed last fact.
        Some(PendingVerdict::ConfirmedRemoved) | None => current.get(key).cloned(),
    }
}

fn last_observed_value(fact: &FactState) -> Option<Value> {
    match fact {
        FactState::Observed(value) => Some(value.clone()),
        FactState::Unverified { last_observed, .. } => last_observed.clone(),
    }
}

fn apply_verdict(
    key: &str,
    verdict: PendingVerdict,
    delta: &mut ViewportDeltaV1,
    next_facts: &mut BTreeMap<String, FactState>,
) -> Option<PendingVerdict> {
    match verdict {
        PendingVerdict::Present(FactState::Observed(value)) => {
            match next_facts.get(key) {
                None => {
                    delta.added.insert(key.to_owned(), value.clone());
                }
                Some(FactState::Observed(previous)) if previous == &value => return None,
                Some(FactState::Observed(_)) | Some(FactState::Unverified { .. }) => {
                    delta.changed.insert(key.to_owned(), value.clone());
                }
            }
            next_facts.insert(key.to_owned(), FactState::Observed(value));
            None
        }
        PendingVerdict::Present(FactState::Unverified {
            last_observed,
            reason,
        }) => {
            let current = next_facts.get(key).cloned();
            let target_value =
                last_observed.or_else(|| current.as_ref().and_then(last_observed_value));
            if let Some(value) = target_value.clone() {
                let current_value = current.as_ref().and_then(last_observed_value);
                if current_value.as_ref() != Some(&value) {
                    if current.is_some() {
                        delta.changed.insert(key.to_owned(), value.clone());
                    } else {
                        delta.added.insert(key.to_owned(), value.clone());
                    }
                    next_facts.insert(key.to_owned(), FactState::Observed(value));
                    return Some(PendingVerdict::Present(FactState::Unverified {
                        last_observed: target_value,
                        reason,
                    }));
                }
            }
            if matches!(
                current,
                Some(FactState::Unverified {
                    last_observed: ref current_last,
                    reason: current_reason,
                }) if current_last == &target_value && current_reason == reason
            ) {
                return None;
            }
            next_facts.insert(
                key.to_owned(),
                FactState::Unverified {
                    last_observed: target_value,
                    reason,
                },
            );
            delta.unverified.insert(key.to_owned(), reason);
            None
        }
        PendingVerdict::ConfirmedRemoved => {
            if next_facts.remove(key).is_some() {
                delta.confirmed_removed.push(key.to_owned());
            }
            None
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum ViewportMirrorError {
    #[error("viewport scope mismatch: expected {expected:?}, got {actual:?}")]
    ScopeMismatch {
        expected: Box<ViewportScope>,
        actual: Box<ViewportScope>,
    },
    #[error("viewport observation is invalid: {0}")]
    InvalidObservation(String),
    #[error("viewport observation contains an invalid key")]
    InvalidKey,
    #[error("viewport observation mentions one key in conflicting states: {key}")]
    ContradictoryObservation { key: String },
    #[error("viewport limits must be non-zero")]
    InvalidLimits,
    #[error("viewport mirror epoch exhausted")]
    EpochExhausted,
    #[error("viewport mirror sequence exhausted")]
    SequenceExhausted,
    #[error("viewport delta is invalid: {0}")]
    InvalidDelta(String),
    #[error("viewport keyframe is invalid: {0}")]
    InvalidKeyframe(String),
    #[error("viewport frame serialization failed: {0}")]
    Serialization(String),
    #[error("viewport frame count exceeds wire range")]
    CountOverflow,
    #[error("viewport wire message is invalid: {0}")]
    InvalidWire(ViewportIncrementalFrameError),
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ViewportCommitError {
    #[error("viewport proposal is stale or was invalidated")]
    StaleProposal,
}
