use serde::{Deserialize, Serialize};

/// Backend 对外可见的连接状态；字段与 TS oracle 一一对应。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackendState {
    Idle,
    Connecting {
        epoch: u64,
        #[serde(rename = "attemptId")]
        attempt_id: String,
        attempt: u32,
    },
    LoggingIn {
        epoch: u64,
        #[serde(rename = "attemptId")]
        attempt_id: String,
        attempt: u32,
    },
    Spawning {
        epoch: u64,
        #[serde(rename = "attemptId")]
        attempt_id: String,
        attempt: u32,
    },
    Ready {
        epoch: u64,
        #[serde(rename = "attemptId")]
        attempt_id: String,
        #[serde(rename = "readyAt")]
        ready_at: String,
    },
    Dead {
        epoch: u64,
        #[serde(rename = "attemptId")]
        attempt_id: String,
        #[serde(rename = "diedAt")]
        died_at: String,
    },
    Reconnecting {
        attempt: u32,
        #[serde(rename = "retryAt")]
        retry_at: String,
        #[serde(rename = "lastClose")]
        last_close: BackendClose,
    },
    Stopping {
        #[serde(skip_serializing_if = "Option::is_none")]
        epoch: Option<u64>,
        reason: String,
    },
    Stopped {
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Faulted {
        failure: BackendFailure,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackendClose {
    pub epoch: u64,
    /// RFC 3339 timestamp.
    pub at: String,
    pub code: String,
    pub retryable: bool,
    pub deliberate: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kick: Option<BackendKick>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<BackendCloseError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackendKick {
    pub text: String,
    pub during_login: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackendCloseError {
    pub name: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendFailureCode {
    InvalidConfig,
    UnsupportedVersion,
    UnsupportedAuth,
    AuthenticationFailed,
    PermissionDenied,
    ConnectionTimeout,
    LoginTimeout,
    SpawnTimeout,
    ProtocolError,
    ReconnectDisabled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackendFailure {
    pub code: BackendFailureCode,
    pub message: String,
    pub retryable: bool,
}
