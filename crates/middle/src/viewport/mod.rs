//! Minecraft 视口的 middle 侧领域代码。
//!
//! 这些原本住在 `agent/` 下，但它们全部是领域产物：视口镜像、增量归约、
//! 轮末采样。放在通用循环旁边只会让「循环是否领域无关」这件事无法被检验。
//!
//! `mirror` 与 `reducer` 目前**生产路径不可达**（只有各自的测试在用）。移到这里
//! 是为了让这一点显而易见——它们是存货，处置另议，但不该继续占着 agent 的位置。

pub mod mirror;
pub mod reducer;
pub mod sampler;

pub use mirror::{
    block_fact_key, KeyframeReason, MirrorLimits, PendingViewportFrame, ViewportCommitError,
    ViewportFrame, ViewportMirror, ViewportMirrorError, ViewportObservation, ViewportProposal,
};
pub use reducer::{ViewportIncrementalReducer, ViewportReducedState, ViewportReplayError};
pub use sampler::{
    BackendRoundViewportSampler, FixedUtcTimestampSource, NoRoundViewportSampler,
    RoundViewportSampler, SystemUtcTimestampSource, UtcTimestampSource,
};
