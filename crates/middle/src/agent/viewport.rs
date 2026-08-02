use mineintent_contracts::agent::{AgentError, AgentErrorCode, ContractFuture, ExecutionControl};
use serde::Serialize;
use serde_json::Value;

use super::transcript::utc_timestamp_now;

/// The assembly seam for a post-body round viewport read.
///
/// The timestamp is supplied by the sampler's explicit clock/backend source;
/// the agent loop never reads the wall clock for a model-visible frame. A
/// sampler receives the same control object as the model and tool calls.
pub trait RoundViewportSampler: Send + Sync {
    type Viewport: Serialize + Send + 'static;

    /// Supplies a UTC timestamp for the sample. Implementations must keep
    /// this synchronous seam panic-safe; the driver isolates a panic and uses
    /// the shared transcript UTC formatter for the unavailable frame.
    fn timestamp(&self) -> String;

    fn sample<'a>(
        &'a self,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<Self::Viewport, AgentError>>;
}

/// Explicit fallback used by the compatibility constructor. If a dispatcher
/// classifies a call as `Body` without supplying a sampler, the loop emits an
/// unavailable frame rather than silently dropping the observation attempt.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoRoundViewportSampler;

impl RoundViewportSampler for NoRoundViewportSampler {
    type Viewport = Value;

    fn timestamp(&self) -> String {
        utc_timestamp_now()
    }

    fn sample<'a>(
        &'a self,
        _control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<Self::Viewport, AgentError>> {
        Box::pin(async {
            Err(AgentError::new(
                AgentErrorCode::ToolFailed,
                "viewport_sampler_not_configured",
            ))
        })
    }
}
