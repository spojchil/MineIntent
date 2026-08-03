//! Participant-facing composition seams.
//!
//! The module exposes the production runtime together with the backend-to-
//! information adapters.  The runtime deliberately accepts explicit ports for
//! facts which are not yet available from the frozen backend snapshot (notably
//! light and armor).

pub mod information_adapters;
pub mod runtime;

pub use information_adapters::{
    BackendInformationAdapterBundle, BackendInformationScopeSource, BackendInventoryPort,
    BackendPerceptionPort, BackendSelfVitalsPort, SoundHistory,
};
pub use runtime::{
    ParticipantAdmission, ParticipantAgentAssembly, ParticipantAgentAssemblyError,
    ParticipantAgentFactory, ParticipantAgentPort, ParticipantClock, ParticipantEvent,
    ParticipantFailure, ParticipantFailureSource, ParticipantFrameCapture, ParticipantFrameSource,
    ParticipantInternalEvent, ParticipantLifecycle, ParticipantMemorySource,
    ParticipantRegistryBound, ParticipantRuntime, ParticipantRuntimeConfig,
    ParticipantRuntimeError, ParticipantScope, ParticipantScopedAgentRunner,
    ParticipantSourceError, ParticipantSpeechControl, ParticipantSpeechPort, SystemUtcClock,
    WakeKind, WakeRegistry, WakeRegistryError, WakeRule, WakeRuleCondition,
};
