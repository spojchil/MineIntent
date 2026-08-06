use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentErrorCode {
    InvalidRequest,
    UnsupportedProtocol,
    InvalidToolDefinition,
    InvalidToolInvocation,
    DuplicateToolCapability,
    UnknownTool,
    ScopeInvalid,
    RunCancelled,
    DeadlineExceeded,
    ToolCallAlreadyHandled,
    ToolRunIsNotActive,
    ProviderFailed,
    ToolFailed,
    LimitExceeded,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentError {
    pub code: AgentErrorCode,
    pub summary: String,
}

impl AgentError {
    pub fn new(code: AgentErrorCode, summary: impl Into<String>) -> Self {
        Self {
            code,
            summary: summary.into(),
        }
    }

    pub fn run_cancelled() -> Self {
        Self::new(AgentErrorCode::RunCancelled, "run_cancelled")
    }

    pub fn deadline_exceeded() -> Self {
        Self::new(AgentErrorCode::DeadlineExceeded, "deadline_exceeded")
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.summary)
    }
}

impl fmt::Display for AgentErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = serde_json::to_value(self).map_err(|_| fmt::Error)?;
        formatter.write_str(value.as_str().ok_or(fmt::Error)?)
    }
}

impl std::error::Error for AgentError {}
