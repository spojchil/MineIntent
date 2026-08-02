//! Information 协议、叶子 provider、runtime/tool-session control plane 与其基础设施。

pub mod access_policy;
pub mod context_composer;
pub mod contracts;
mod control;
pub mod cursor_store;
pub mod geometry;
pub mod providers;
pub mod ref_store;
pub mod registry;
pub mod runtime;
pub mod scope;
pub mod source_ports;
pub mod tool_session;
pub mod trace;

mod support;

pub use context_composer::{compose_passive_observations, InformationContextComposer};
pub use runtime::{InformationRuntime, InformationRuntimeInitError, InformationRuntimeOptions};
pub use support::{InformationClock, SystemInformationClock};
pub use tool_session::{
    InformationCatalogTool, InformationRuntimePort, InformationTool, InformationToolCallKind,
    InformationToolSession, InformationToolSessionInitError, InformationToolSessionUsage,
};
