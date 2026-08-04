//! Incremental viewport metadata and delta wire types.
//!
//! The existing `viewport-frame.v1`/`v2` contracts are intentionally frozen.  This
//! module defines the separate metadata needed when a viewport payload is sent as
//! a change relative to a model-visible baseline.

use std::{collections::BTreeMap, fmt};

use serde::{
    de::Error as _,
    ser::{Error as _, SerializeStruct},
    Deserialize, Deserializer, Serialize, Serializer,
};
use serde_json::Value;

use crate::minecraft::ViewportCoordinateSystem;

/// Discriminator for the first incremental viewport frame envelope.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ViewportIncrementalFrameProtocol {
    #[default]
    #[serde(rename = "mineintent.viewport-frame.v3")]
    V3,
}

impl ViewportIncrementalFrameProtocol {
    pub const WIRE: &'static str = "mineintent.viewport-frame.v3";
}

/// The coordinate/world namespace in which a baseline is valid.
///
/// `world_id` is a world instance or server-session identity, not merely a
/// dimension name.  A dimension switch therefore changes this value even when
/// the player happens to stand at the same coordinates.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportScope {
    pub process_session_id: String,
    pub connection_epoch: u64,
    pub world_id: String,
    pub dimension: String,
    /// Identity of the model-visible context/transcript lineage that owns the
    /// baseline. Two concurrent contexts in the same world must not share it.
    pub context_id: String,
    pub coordinates: ViewportCoordinateSystem,
    pub algorithm_revision: String,
}

impl ViewportScope {
    pub fn new(
        process_session_id: impl Into<String>,
        connection_epoch: u64,
        world_id: impl Into<String>,
        dimension: impl Into<String>,
        context_id: impl Into<String>,
        algorithm_revision: impl Into<String>,
    ) -> Result<Self, ViewportScopeError> {
        let scope = Self {
            process_session_id: process_session_id.into(),
            connection_epoch,
            world_id: world_id.into(),
            dimension: dimension.into(),
            context_id: context_id.into(),
            coordinates: ViewportCoordinateSystem::MinecraftWorldAbsolute,
            algorithm_revision: algorithm_revision.into(),
        };
        scope.validate()?;
        Ok(scope)
    }

    pub fn validate(&self) -> Result<(), ViewportScopeError> {
        for (field, value) in [
            ("process_session_id", self.process_session_id.as_str()),
            ("world_id", self.world_id.as_str()),
            ("dimension", self.dimension.as_str()),
            ("context_id", self.context_id.as_str()),
            ("algorithm_revision", self.algorithm_revision.as_str()),
        ] {
            if value.trim().is_empty() || value.chars().any(char::is_control) {
                return Err(ViewportScopeError::InvalidField { field });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViewportScopeError {
    InvalidField { field: &'static str },
}

impl fmt::Display for ViewportScopeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidField { field } => {
                write!(
                    formatter,
                    "viewport scope field {field} must be non-empty and non-control"
                )
            }
        }
    }
}

impl std::error::Error for ViewportScopeError {}

/// An identity inside one mirror generation.  `epoch` is the mirror-chain
/// generation; it is deliberately distinct from `ViewportScope::connection_epoch`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportBaselineId {
    pub epoch: u64,
    pub sequence: u64,
}

impl ViewportBaselineId {
    pub const fn new(epoch: u64, sequence: u64) -> Self {
        Self { epoch, sequence }
    }
}

/// Why a previously known fact is not confirmed by the current observation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewportUnverifiedReason {
    OutsideViewport,
    Occluded,
    TooFar,
    ChunkNotLoaded,
    OutputBudget,
    NotObserved,
}

/// The model-facing change set.  Keys are producer-defined canonical fact keys
/// (for example `block:x:y:z`); the mirror never treats an omitted key as air.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportDeltaV1 {
    pub added: BTreeMap<String, Value>,
    pub changed: BTreeMap<String, Value>,
    pub confirmed_removed: Vec<String>,
    pub unverified: BTreeMap<String, ViewportUnverifiedReason>,
}

impl ViewportDeltaV1 {
    pub fn change_count(&self) -> usize {
        self.added.len() + self.changed.len() + self.confirmed_removed.len() + self.unverified.len()
    }

    pub fn validate(&self) -> Result<(), ViewportDeltaError> {
        let mut seen = std::collections::BTreeSet::new();
        for key in self
            .added
            .keys()
            .chain(self.changed.keys())
            .chain(self.unverified.keys())
        {
            if key.trim().is_empty() || key.chars().any(char::is_control) {
                return Err(ViewportDeltaError::InvalidKey);
            }
            if !seen.insert(key.as_str()) {
                return Err(ViewportDeltaError::DuplicateKey);
            }
        }
        for key in &self.confirmed_removed {
            if key.trim().is_empty() || key.chars().any(char::is_control) {
                return Err(ViewportDeltaError::InvalidKey);
            }
            if !seen.insert(key.as_str()) {
                return Err(ViewportDeltaError::DuplicateKey);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewportDeltaError {
    InvalidKey,
    DuplicateKey,
}

impl fmt::Display for ViewportDeltaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidKey => "viewport delta keys must be non-empty and non-control",
            Self::DuplicateKey => "viewport delta must not mention one key twice",
        })
    }
}

impl std::error::Error for ViewportDeltaError {}

/// Replayable facts carried by a keyframe.
///
/// Keeping this typed prevents the wire contract from accepting arbitrary JSON
/// that a receiver cannot later reduce into a baseline.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportKeyframeV1 {
    pub facts: BTreeMap<String, Value>,
}

impl ViewportKeyframeV1 {
    pub fn new(facts: BTreeMap<String, Value>) -> Result<Self, ViewportKeyframeError> {
        let keyframe = Self { facts };
        keyframe.validate()?;
        Ok(keyframe)
    }

    pub fn validate(&self) -> Result<(), ViewportKeyframeError> {
        for key in self.facts.keys() {
            if key.trim().is_empty() || key.chars().any(char::is_control) {
                return Err(ViewportKeyframeError::InvalidKey);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewportKeyframeError {
    InvalidKey,
}

impl fmt::Display for ViewportKeyframeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("viewport keyframe keys must be non-empty and non-control")
    }
}

impl std::error::Error for ViewportKeyframeError {}

/// A keyframe or delta payload carried by the v3 envelope.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, tag = "kind")]
pub enum ViewportIncrementalPayloadV1 {
    #[serde(rename = "keyframe")]
    Keyframe {
        viewport: ViewportKeyframeV1,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        unverified: BTreeMap<String, ViewportUnverifiedReason>,
        complete: bool,
        omitted: u64,
    },
    #[serde(rename = "delta")]
    Delta {
        delta: ViewportDeltaV1,
        complete: bool,
        omitted: u64,
    },
}

/// Independent user-message envelope for an incremental viewport frame.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewportIncrementalFrameMessageV1 {
    pub protocol: ViewportIncrementalFrameProtocol,
    pub at: String,
    pub scope: ViewportScope,
    pub base_baseline_id: Option<ViewportBaselineId>,
    pub baseline_id: ViewportBaselineId,
    pub payload: ViewportIncrementalPayloadV1,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawViewportIncrementalFrameMessageV1 {
    protocol: ViewportIncrementalFrameProtocol,
    at: String,
    scope: ViewportScope,
    #[serde(default)]
    base_baseline_id: Option<ViewportBaselineId>,
    baseline_id: ViewportBaselineId,
    payload: ViewportIncrementalPayloadV1,
}

impl ViewportIncrementalFrameMessageV1 {
    pub fn new(
        at: impl Into<String>,
        scope: ViewportScope,
        base_baseline_id: Option<ViewportBaselineId>,
        baseline_id: ViewportBaselineId,
        payload: ViewportIncrementalPayloadV1,
    ) -> Result<Self, ViewportIncrementalFrameError> {
        let message = Self {
            protocol: ViewportIncrementalFrameProtocol::V3,
            at: at.into(),
            scope,
            base_baseline_id,
            baseline_id,
            payload,
        };
        message.validate()?;
        Ok(message)
    }

    pub fn validate(&self) -> Result<(), ViewportIncrementalFrameError> {
        if self.at.trim().is_empty() || self.at.chars().any(char::is_control) {
            return Err(ViewportIncrementalFrameError::InvalidTimestamp);
        }
        self.scope
            .validate()
            .map_err(ViewportIncrementalFrameError::InvalidScope)?;
        if self.baseline_id.sequence == 0 {
            return Err(ViewportIncrementalFrameError::InvalidBaselineId);
        }
        match (&self.base_baseline_id, &self.payload) {
            (Some(base), ViewportIncrementalPayloadV1::Delta { .. }) => {
                if base.sequence == 0
                    || base.epoch != self.baseline_id.epoch
                    || base.sequence.checked_add(1) != Some(self.baseline_id.sequence)
                {
                    return Err(ViewportIncrementalFrameError::InvalidBaselineChain);
                }
            }
            (Some(_), ViewportIncrementalPayloadV1::Keyframe { .. }) => {
                return Err(ViewportIncrementalFrameError::KeyframeHasBase);
            }
            (None, ViewportIncrementalPayloadV1::Delta { .. }) => {
                return Err(ViewportIncrementalFrameError::DeltaMissingBase);
            }
            (None, ViewportIncrementalPayloadV1::Keyframe { .. }) => {}
        }
        match &self.payload {
            ViewportIncrementalPayloadV1::Keyframe {
                viewport,
                unverified,
                complete,
                omitted,
            } => {
                viewport
                    .validate()
                    .map_err(ViewportIncrementalFrameError::InvalidKeyframe)?;
                validate_unverified(unverified)?;
                if *complete && (*omitted > 0 || !unverified.is_empty()) {
                    return Err(ViewportIncrementalFrameError::IncompleteMarkedComplete);
                }
            }
            ViewportIncrementalPayloadV1::Delta {
                delta,
                complete,
                omitted,
            } => {
                delta
                    .validate()
                    .map_err(ViewportIncrementalFrameError::InvalidDelta)?;
                if *complete && (*omitted > 0 || !delta.unverified.is_empty()) {
                    return Err(ViewportIncrementalFrameError::IncompleteMarkedComplete);
                }
            }
        }
        Ok(())
    }
}

fn validate_unverified(
    unverified: &BTreeMap<String, ViewportUnverifiedReason>,
) -> Result<(), ViewportIncrementalFrameError> {
    for key in unverified.keys() {
        if key.trim().is_empty() || key.chars().any(char::is_control) {
            return Err(ViewportIncrementalFrameError::InvalidDelta(
                ViewportDeltaError::InvalidKey,
            ));
        }
    }
    Ok(())
}

impl Serialize for ViewportIncrementalFrameMessageV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(S::Error::custom)?;
        let mut state = serializer.serialize_struct("ViewportIncrementalFrameMessageV1", 6)?;
        state.serialize_field("protocol", &self.protocol)?;
        state.serialize_field("at", &self.at)?;
        state.serialize_field("scope", &self.scope)?;
        if let Some(base) = &self.base_baseline_id {
            state.serialize_field("baseBaselineId", base)?;
        }
        state.serialize_field("baselineId", &self.baseline_id)?;
        state.serialize_field("payload", &self.payload)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for ViewportIncrementalFrameMessageV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawViewportIncrementalFrameMessageV1::deserialize(deserializer)?;
        let message = Self {
            protocol: raw.protocol,
            at: raw.at,
            scope: raw.scope,
            base_baseline_id: raw.base_baseline_id,
            baseline_id: raw.baseline_id,
            payload: raw.payload,
        };
        message.validate().map_err(D::Error::custom)?;
        Ok(message)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViewportIncrementalFrameError {
    InvalidTimestamp,
    InvalidScope(ViewportScopeError),
    InvalidBaselineId,
    KeyframeHasBase,
    DeltaMissingBase,
    InvalidBaselineChain,
    InvalidKeyframe(ViewportKeyframeError),
    InvalidDelta(ViewportDeltaError),
    IncompleteMarkedComplete,
}

impl fmt::Display for ViewportIncrementalFrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTimestamp => formatter.write_str("viewport frame timestamp is invalid"),
            Self::InvalidScope(error) => error.fmt(formatter),
            Self::InvalidBaselineId => {
                formatter.write_str("viewport baseline sequence must be greater than zero")
            }
            Self::KeyframeHasBase => formatter.write_str("keyframe must not carry a base baseline"),
            Self::DeltaMissingBase => formatter.write_str("delta must carry a base baseline"),
            Self::InvalidBaselineChain => formatter.write_str(
                "delta baseline ids must remain within one epoch and advance exactly once",
            ),
            Self::InvalidKeyframe(error) => error.fmt(formatter),
            Self::InvalidDelta(error) => error.fmt(formatter),
            Self::IncompleteMarkedComplete => formatter.write_str(
                "a complete viewport frame must not contain omitted or unverified facts",
            ),
        }
    }
}

impl std::error::Error for ViewportIncrementalFrameError {}
