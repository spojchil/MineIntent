//! Participant-facing composition seams.
//!
//! This module contains only the adapters that turn the frozen backend API into
//! Information source ports. Participant runtime and wake policy remain outside
//! this module.

pub mod information_adapters;

pub use information_adapters::{
    BackendInformationAdapterBundle, BackendInformationScopeSource, BackendInventoryPort,
    BackendPerceptionPort, BackendSelfVitalsPort, SoundHistory,
};
