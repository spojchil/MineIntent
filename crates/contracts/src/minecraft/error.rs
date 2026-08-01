use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

use super::{AuthKind, BackendClose, BackendFailure};

/// 可由调用方稳定匹配的 backend 契约错误。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackendError {
    InvalidConfig {
        field: String,
        message: String,
    },
    UnsupportedVersion {
        expected: String,
        actual: String,
    },
    UnsupportedAuth {
        auth: AuthKind,
    },
    NotReady {
        state: String,
    },
    StaleEpoch {
        #[serde(rename = "boundEpoch")]
        bound_epoch: u64,
        #[serde(rename = "currentEpoch")]
        current_epoch: u64,
    },
    Cancelled {
        operation: String,
    },
    DeadlineExceeded {
        operation: String,
    },
    InvalidCommand {
        field: String,
        message: String,
    },
    BackendClosed {
        close: BackendClose,
    },
    BackendFailure {
        failure: BackendFailure,
    },
    SubscriptionClosed,
}

impl fmt::Display for BackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig { field, message } => {
                write!(formatter, "invalid config {field}: {message}")
            }
            Self::UnsupportedVersion { expected, actual } => {
                write!(
                    formatter,
                    "unsupported version {actual}; expected {expected}"
                )
            }
            Self::UnsupportedAuth { auth } => {
                write!(formatter, "unsupported authentication mode: {auth:?}")
            }
            Self::NotReady { state } => write!(formatter, "backend is not ready: {state}"),
            Self::StaleEpoch {
                bound_epoch,
                current_epoch,
            } => write!(
                formatter,
                "stale backend epoch {bound_epoch}; current epoch is {current_epoch}"
            ),
            Self::Cancelled { operation } => write!(formatter, "{operation} was cancelled"),
            Self::DeadlineExceeded { operation } => {
                write!(formatter, "{operation} exceeded its deadline")
            }
            Self::InvalidCommand { field, message } => {
                write!(formatter, "invalid command {field}: {message}")
            }
            Self::BackendClosed { close } => write!(formatter, "backend closed: {}", close.code),
            Self::BackendFailure { failure } => {
                write!(formatter, "backend failure: {}", failure.message)
            }
            Self::SubscriptionClosed => formatter.write_str("subscription is closed"),
        }
    }
}

impl Error for BackendError {}
