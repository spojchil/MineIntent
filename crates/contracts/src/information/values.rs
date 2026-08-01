use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

use crate::minecraft::{BackendError, InventorySlotSnapshot, StatusEffectSnapshot, ViewportFrame};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationConnectionState {
    Disconnected,
    Connecting,
    Configuration,
    Play,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationScopeSnapshot {
    pub process_session_id: String,
    pub connection_state: InformationConnectionState,
    pub connection_epoch: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub world_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimension: Option<String>,
    pub ui_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen_instance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen_revision: Option<u64>,
    /// RFC 3339 timestamp.
    pub captured_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PassiveInterfaceId {
    #[serde(rename = "current_status")]
    CurrentStatus,
    #[serde(rename = "inventory_information")]
    InventoryInformation,
    #[serde(rename = "sound_information")]
    SoundInformation,
    #[serde(rename = "viewport_information")]
    ViewportInformation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationUnavailableReason {
    NotConnected,
    ScreenNotOpen,
    NotCurrentlyDisplayed,
    BlockedByReducedDebug,
    UnsupportedGameMode,
    PermissionRequired,
    NotSupported,
    NotExposed,
    StaleSelector,
    WrongWorld,
    WrongScreen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InformationOmissionReason {
    AudienceDenied,
    Unavailable,
    Partial,
    DeadlineExceeded,
    ScopeChanged,
    ProviderFailed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationFieldOmission {
    pub field: String,
    pub reason: InformationUnavailableReason,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InformationOmission {
    pub interface_id: PassiveInterfaceId,
    pub reason: InformationOmissionReason,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<InformationFieldOmission>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Effective passive wire values are partial: a provider can supply safe fields and report the
/// missing fields in `omissions` without inventing oxygen/experience or other facts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CurrentStatusValues {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub food: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub food_saturation: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oxygen: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experience_level: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_effects: Option<Vec<StatusEffectSnapshot>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InventoryValues {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_hotbar_slot: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slots: Option<Vec<InventorySlotSnapshot>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelativeDirection {
    Ahead,
    Right,
    Behind,
    Left,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SoundObservation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sound_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    pub distance: f64,
    pub direction: RelativeDirection,
    pub volume: f64,
    pub pitch: f64,
    /// RFC 3339 timestamp.
    pub observed_at: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SoundValues {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent_sounds: Option<Vec<SoundObservation>>,
}

/// Passive composition intentionally carries only the no-scan viewport frame.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PassiveViewportValues {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<ViewportFrame>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PassiveObservations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_status: Option<CurrentStatusValues>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inventory: Option<InventoryValues>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sound: Option<SoundValues>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport: Option<PassiveViewportValues>,
    pub omissions: Vec<InformationOmission>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case", deny_unknown_fields)]
pub enum InformationError {
    Cancelled {
        operation: String,
    },
    DeadlineExceeded {
        operation: String,
    },
    AudienceDenied {
        #[serde(rename = "interfaceId")]
        interface_id: PassiveInterfaceId,
    },
    ScopeChanged {
        #[serde(rename = "beforeEpoch")]
        before_epoch: u64,
        #[serde(rename = "afterEpoch")]
        after_epoch: u64,
    },
    BackendFailed {
        error: BackendError,
    },
    InvalidContract {
        message: String,
    },
}

impl fmt::Display for InformationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled { operation } => write!(formatter, "{operation} was cancelled"),
            Self::DeadlineExceeded { operation } => {
                write!(formatter, "{operation} exceeded its deadline")
            }
            Self::AudienceDenied { interface_id } => {
                write!(formatter, "access denied for {interface_id:?}")
            }
            Self::ScopeChanged {
                before_epoch,
                after_epoch,
            } => write!(
                formatter,
                "information scope changed from epoch {before_epoch} to {after_epoch}"
            ),
            Self::BackendFailed { error } => write!(formatter, "backend read failed: {error}"),
            Self::InvalidContract { message } => formatter.write_str(message),
        }
    }
}

impl Error for InformationError {}
