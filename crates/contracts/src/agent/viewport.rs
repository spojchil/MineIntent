use std::fmt;

use serde::{
    de::Error as _,
    ser::{Error as _, SerializeStruct},
    Deserialize, Deserializer, Serialize, Serializer,
};
use serde_json::Value;

use crate::minecraft::{ViewportFullV2, ViewportV2};

/// Discriminator for the independent user message appended at the end of a
/// body-tool batch.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ViewportFrameProtocol {
    #[default]
    #[serde(rename = "mineintent.viewport-frame.v1")]
    V1,
}

/// 轮末帧 v2 的 discriminator。v1 类型与 fixture 保持原样用于旧回放。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ViewportFrameProtocolV2 {
    #[default]
    #[serde(rename = "mineintent.viewport-frame.v2")]
    V2,
}

impl ViewportFrameProtocolV2 {
    pub const WIRE: &'static str = "mineintent.viewport-frame.v2";
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViewportFrameWireError {
    NullViewport,
    EmptyUnavailable,
    InvalidTimestamp,
    InvalidState,
}

impl fmt::Display for ViewportFrameWireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NullViewport => "viewport frame success requires a non-null viewport",
            Self::EmptyUnavailable => "viewport frame unavailable must be non-empty",
            Self::InvalidTimestamp => {
                "viewport frame at must be non-empty, non-whitespace, and free of control characters"
            }
            Self::InvalidState => "viewport frame has an invalid success/failure state",
        })
    }
}

impl std::error::Error for ViewportFrameWireError {}

/// The independent model-visible viewport-frame envelope.
///
/// The payload remains a generic JSON value so the agent layer does not copy
/// the backend viewport DTO or its BlockInfo serializer.  The private state
/// and custom serde implementation keep success and failure wires disjoint:
/// success has no `unavailable`, while failure has `viewport: null` and a
/// required non-empty `unavailable`.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewportFrameMessage {
    at: String,
    viewport: Option<Value>,
    unavailable: Option<String>,
}

impl ViewportFrameMessage {
    /// Validates the minimum wire hygiene for an assembly-provided timestamp.
    ///
    /// The agent layer deliberately does not impose a narrower date grammar;
    /// the sampler/assembly seam remains responsible for supplying a UTC
    /// timestamp.
    pub fn validate_at(at: &str) -> Result<(), ViewportFrameWireError> {
        if at.trim().is_empty() || at.chars().any(char::is_control) {
            return Err(ViewportFrameWireError::InvalidTimestamp);
        }
        Ok(())
    }

    pub fn success(at: impl Into<String>, viewport: Value) -> Result<Self, ViewportFrameWireError> {
        let at = at.into();
        Self::validate_at(&at)?;
        if viewport.is_null() {
            return Err(ViewportFrameWireError::NullViewport);
        }
        Ok(Self {
            at,
            viewport: Some(viewport),
            unavailable: None,
        })
    }

    pub fn unavailable(
        at: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<Self, ViewportFrameWireError> {
        let at = at.into();
        Self::validate_at(&at)?;
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(ViewportFrameWireError::EmptyUnavailable);
        }
        Ok(Self {
            at,
            viewport: None,
            unavailable: Some(reason),
        })
    }

    pub fn at(&self) -> &str {
        &self.at
    }

    pub fn viewport(&self) -> Option<&Value> {
        self.viewport.as_ref()
    }

    pub fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    pub fn is_unavailable(&self) -> bool {
        self.unavailable.is_some()
    }
}

impl Serialize for ViewportFrameMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Self::validate_at(&self.at).map_err(S::Error::custom)?;
        let mut state = serializer.serialize_struct(
            "ViewportFrameMessage",
            if self.unavailable.is_some() { 4 } else { 3 },
        )?;
        state.serialize_field("protocol", &ViewportFrameProtocol::V1)?;
        state.serialize_field("at", &self.at)?;

        match (&self.viewport, &self.unavailable) {
            (Some(viewport), None) if !viewport.is_null() => {
                state.serialize_field("viewport", viewport)?;
            }
            (None, Some(reason)) if !reason.trim().is_empty() => {
                state.serialize_field("viewport", &Value::Null)?;
                state.serialize_field("unavailable", reason)?;
            }
            (Some(_), None) => {
                return Err(S::Error::custom(ViewportFrameWireError::NullViewport));
            }
            (None, Some(_)) => {
                return Err(S::Error::custom(ViewportFrameWireError::EmptyUnavailable));
            }
            _ => return Err(S::Error::custom(ViewportFrameWireError::InvalidState)),
        }
        state.end()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawViewportFrameMessage {
    protocol: ViewportFrameProtocol,
    at: String,
    viewport: Value,
    #[serde(default, deserialize_with = "deserialize_optional_non_null")]
    unavailable: Option<String>,
}

impl<'de> Deserialize<'de> for ViewportFrameMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawViewportFrameMessage::deserialize(deserializer)?;
        Self::validate_at(&raw.at).map_err(D::Error::custom)?;
        let _ = raw.protocol;
        match (raw.viewport.is_null(), raw.unavailable) {
            (false, None) => Ok(Self {
                at: raw.at,
                viewport: Some(raw.viewport),
                unavailable: None,
            }),
            (true, Some(reason)) if !reason.trim().is_empty() => Ok(Self {
                at: raw.at,
                viewport: None,
                unavailable: Some(reason),
            }),
            (true, Some(_)) => Err(D::Error::custom(
                ViewportFrameWireError::EmptyUnavailable.to_string(),
            )),
            (true, None) => Err(D::Error::custom(
                ViewportFrameWireError::NullViewport.to_string(),
            )),
            (false, Some(_)) => Err(D::Error::custom(
                ViewportFrameWireError::InvalidState.to_string(),
            )),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViewportFrameV2WireError {
    NullViewport,
    EmptyUnavailable,
    InvalidTimestamp,
    InvalidViewportAnchor,
    DirectedViewportNotAllowed,
    InvalidState,
}

impl fmt::Display for ViewportFrameV2WireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NullViewport => "viewport frame success requires a non-null viewport",
            Self::EmptyUnavailable => "viewport frame unavailable must be non-empty",
            Self::InvalidTimestamp => {
                "viewport frame at must be non-empty, non-whitespace, and free of control characters"
            }
            Self::InvalidViewportAnchor => {
                "viewport frame success requires mineintent.viewport.v2"
            }
            Self::DirectedViewportNotAllowed => {
                "viewport frame success requires the full mineintent.viewport.v2 payload"
            }
            Self::InvalidState => "viewport frame has an invalid success/failure state",
        })
    }
}

impl std::error::Error for ViewportFrameV2WireError {}

/// v2 轮末帧。成功 payload 必须带 `mineintent.viewport.v2` 嵌套锚点；失败仍是
/// `viewport: null + unavailable` 的显式 unavailable 纪律。这个类型与 v1 并存，避免
/// 旧 transcript/fixture 在升级时被静默重写。
#[derive(Clone, Debug, PartialEq)]
pub struct ViewportFrameMessageV2 {
    at: String,
    viewport: Option<ViewportFullV2>,
    unavailable: Option<String>,
}

impl ViewportFrameMessageV2 {
    pub fn validate_at(at: &str) -> Result<(), ViewportFrameV2WireError> {
        if at.trim().is_empty() || at.chars().any(char::is_control) {
            return Err(ViewportFrameV2WireError::InvalidTimestamp);
        }
        Ok(())
    }

    pub fn success(
        at: impl Into<String>,
        viewport: Value,
    ) -> Result<Self, ViewportFrameV2WireError> {
        let at = at.into();
        Self::validate_at(&at)?;
        if viewport.is_null() {
            return Err(ViewportFrameV2WireError::NullViewport);
        }
        let viewport = serde_json::from_value::<ViewportV2>(viewport)
            .map_err(|_| ViewportFrameV2WireError::InvalidViewportAnchor)?;
        let ViewportV2::Full(viewport) = viewport else {
            return Err(ViewportFrameV2WireError::DirectedViewportNotAllowed);
        };
        Ok(Self {
            at,
            viewport: Some(viewport),
            unavailable: None,
        })
    }

    pub fn unavailable(
        at: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<Self, ViewportFrameV2WireError> {
        let at = at.into();
        Self::validate_at(&at)?;
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(ViewportFrameV2WireError::EmptyUnavailable);
        }
        Ok(Self {
            at,
            viewport: None,
            unavailable: Some(reason),
        })
    }

    pub fn at(&self) -> &str {
        &self.at
    }

    pub fn viewport(&self) -> Option<&ViewportFullV2> {
        self.viewport.as_ref()
    }

    pub fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    pub fn is_unavailable(&self) -> bool {
        self.unavailable.is_some()
    }
}

impl Serialize for ViewportFrameMessageV2 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Self::validate_at(&self.at).map_err(S::Error::custom)?;
        let mut state = serializer.serialize_struct(
            "ViewportFrameMessageV2",
            if self.unavailable.is_some() { 4 } else { 3 },
        )?;
        state.serialize_field("protocol", &ViewportFrameProtocolV2::V2)?;
        state.serialize_field("at", &self.at)?;
        match (&self.viewport, &self.unavailable) {
            (Some(viewport), None) => {
                state.serialize_field("viewport", viewport)?;
            }
            (None, Some(reason)) if !reason.trim().is_empty() => {
                state.serialize_field("viewport", &Value::Null)?;
                state.serialize_field("unavailable", reason)?;
            }
            (None, Some(_)) => {
                return Err(S::Error::custom(ViewportFrameV2WireError::EmptyUnavailable));
            }
            _ => return Err(S::Error::custom(ViewportFrameV2WireError::InvalidState)),
        }
        state.end()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawViewportFrameMessageV2 {
    protocol: ViewportFrameProtocolV2,
    at: String,
    viewport: Value,
    #[serde(default, deserialize_with = "deserialize_optional_non_null")]
    unavailable: Option<String>,
}

impl<'de> Deserialize<'de> for ViewportFrameMessageV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawViewportFrameMessageV2::deserialize(deserializer)?;
        Self::validate_at(&raw.at).map_err(D::Error::custom)?;
        let _ = raw.protocol;
        match (raw.viewport.is_null(), raw.unavailable) {
            (false, None) => {
                let viewport =
                    serde_json::from_value::<ViewportV2>(raw.viewport).map_err(|_| {
                        D::Error::custom(
                            ViewportFrameV2WireError::InvalidViewportAnchor.to_string(),
                        )
                    })?;
                let ViewportV2::Full(viewport) = viewport else {
                    return Err(D::Error::custom(
                        ViewportFrameV2WireError::DirectedViewportNotAllowed.to_string(),
                    ));
                };
                Ok(Self {
                    at: raw.at,
                    viewport: Some(viewport),
                    unavailable: None,
                })
            }
            (true, Some(reason)) if !reason.trim().is_empty() => Ok(Self {
                at: raw.at,
                viewport: None,
                unavailable: Some(reason),
            }),
            (true, Some(_)) => Err(D::Error::custom(
                ViewportFrameV2WireError::EmptyUnavailable.to_string(),
            )),
            (true, None) => Err(D::Error::custom(
                ViewportFrameV2WireError::NullViewport.to_string(),
            )),
            (false, Some(_)) => Err(D::Error::custom(
                ViewportFrameV2WireError::InvalidState.to_string(),
            )),
        }
    }
}

fn deserialize_optional_non_null<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?.map_or_else(
        || Err(D::Error::custom("explicit null is not allowed")),
        |value| Ok(Some(value)),
    )
}
