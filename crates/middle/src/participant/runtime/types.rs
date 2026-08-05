//! Participant 的事件、生命周期、错误与运行配置类型。

use super::*;

#[derive(Clone, Debug)]
pub enum ParticipantInternalEvent {
    Fact {
        id: String,
        occurred_at: String,
        scope: ParticipantScope,
        event_type: String,
        summary: String,
    },
    ScopeChanged {
        id: String,
        occurred_at: String,
        scope: ParticipantScope,
        reason: String,
    },
    Closed {
        id: String,
        occurred_at: String,
        scope: ParticipantScope,
        reason: String,
    },
    Faulted {
        id: String,
        occurred_at: String,
        scope: ParticipantScope,
        code: String,
    },
    Stopped {
        id: String,
        occurred_at: String,
        scope: ParticipantScope,
        reason: String,
    },
}

/// Unified public event envelope. Both variants use the same runtime
/// admission/ordering path; backend listeners need not know about internal
/// fact producers.
#[derive(Clone, Debug)]
pub enum ParticipantEvent {
    Backend(BackendEventEnvelope),
    Internal(ParticipantInternalEvent),
}

impl ParticipantInternalEvent {
    pub(super) fn metadata(&self) -> (&str, &str, &ParticipantScope) {
        match self {
            Self::Fact {
                id,
                occurred_at,
                scope,
                ..
            }
            | Self::ScopeChanged {
                id,
                occurred_at,
                scope,
                ..
            }
            | Self::Closed {
                id,
                occurred_at,
                scope,
                ..
            }
            | Self::Faulted {
                id,
                occurred_at,
                scope,
                ..
            }
            | Self::Stopped {
                id,
                occurred_at,
                scope,
                ..
            } => (id, occurred_at, scope),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParticipantLifecycle {
    Created,
    Running,
    Stopping,
    Stopped,
    Faulted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParticipantAdmission {
    Recorded,
    WakeQueued { ordinal: u64 },
    Ignored,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParticipantFailureSource {
    Backend,
    Source,
    Journal,
    Model,
    Runtime,
    BodyRelease,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParticipantFailure {
    pub source: ParticipantFailureSource,
    pub code: String,
    pub summary: String,
    pub scope: Option<ParticipantScope>,
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum ParticipantSourceError {
    // 这里曾有一个 MissingLight：光照读不到就让整帧失败。它已被删除，因为
    // 「某个可观察量这一轮没读到」不是错误，是帧要如实表达的内容——留着它
    // 就等于允许任何一个可缺席的量把同伴打成失能。见 AgentFrameV5::light。
    #[error("participant source scope is stale: {0}")]
    StaleScope(String),
    #[error("opening frame source is invalid: {0}")]
    Invalid(String),
    #[error("opening frame source failed: {0}")]
    Failed(String),
}

#[derive(Debug, Error)]
pub enum ParticipantRuntimeError {
    #[error("participant runtime configuration is invalid: {0}")]
    InvalidConfig(String),
    #[error("participant runtime is not started")]
    NotStarted,
    #[error("participant runtime is already started")]
    AlreadyStarted,
    #[error("participant runtime is stopped")]
    Stopped,
    #[error("participant runtime is faulted")]
    Faulted,
    #[error("participant runtime event queue is closed")]
    QueueClosed,
    #[error("backend operation failed: {0}")]
    Backend(String),
    #[error("participant source failed: {0}")]
    Source(#[from] ParticipantSourceError),
    #[error("frame assembly failed: {0}")]
    Frame(#[from] AgentContextV5AssemblyError),
    #[error("memory read failed: {0}")]
    Memory(String),
    #[error("participant handler failed: {0}")]
    Handler(String),
}

pub struct ParticipantRuntimeConfig<R> {
    pub backend: Arc<dyn MinecraftBackendApi>,
    pub agent: Arc<R>,
    pub frame_source: Arc<dyn ParticipantFrameSource>,
    pub memory: Arc<dyn ParticipantMemorySource>,
    pub journal: Arc<dyn CapabilityJournal>,
    pub speech: Arc<dyn ParticipantSpeechPort>,
    pub debug: Arc<DebugStateStore>,
    pub clock: Arc<dyn ParticipantClock>,
    pub prompt_template: PromptTemplateRef,
    pub run_deadline: Duration,
    pub wake_registry: WakeRegistry,
    /// Stable process/session identity supplied by the app. It is included in
    /// every run id so reconnects and reconstructed runtimes cannot silently
    /// reuse the ordinal-only ids of an earlier instance.
    pub run_id_namespace: String,
}
