//! Revisioned, redacted participant diagnostics and a loopback-only read server.

mod contracts;
mod debug_server;
mod debug_state;

pub use contracts::{
    DebugBodyState, DebugBodyTool, DebugContextSource, DebugContextSourceKind, DebugDecision,
    DebugDecisionStatus, DebugFailureSource, DebugFailureSummary, DebugInventoryItem,
    DebugStateInput, DebugStateProtocol, DebugStateUpdate, ParticipantDebugState,
    DEBUG_STATE_PROTOCOL, MAX_RECENT_FAILURES,
};
pub use debug_server::{
    DebugServerAddress, DebugServerError, LocalDebugServer, DEBUG_SERVER_HOST, DEFAULT_DEBUG_PORT,
};
pub use debug_state::{redact_sensitive, redact_sensitive_value, DebugSnapshot, DebugStateStore};

pub use contracts::{BackendState, PassiveObservations, Vec3Value};
