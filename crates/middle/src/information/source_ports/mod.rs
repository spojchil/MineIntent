//! Read-only source contracts. Implementations and provider composition live outside this module.

mod inventory;
mod perception;
mod self_vitals;
mod sound;

pub use inventory::*;
pub use perception::*;
pub use self_vitals::*;
pub use sound::*;
