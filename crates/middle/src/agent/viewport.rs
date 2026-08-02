use mineintent_contracts::agent::{AgentError, AgentErrorCode, ContractFuture, ExecutionControl};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;

use super::transcript::utc_timestamp_now;
use crate::capability::ViewportReader;

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

/// 可注入的 UTC 时间源。sampler 不从 viewport fixture 或 backend 元数据取时间。
pub trait UtcTimestampSource: Send + Sync {
    fn now(&self) -> String;
}

impl<F> UtcTimestampSource for F
where
    F: Fn() -> String + Send + Sync,
{
    fn now(&self) -> String {
        self()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemUtcTimestampSource;

impl UtcTimestampSource for SystemUtcTimestampSource {
    fn now(&self) -> String {
        utc_timestamp_now()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixedUtcTimestampSource {
    timestamp: String,
}

impl FixedUtcTimestampSource {
    pub fn new(timestamp: impl Into<String>) -> Self {
        Self {
            timestamp: timestamp.into(),
        }
    }
}

impl UtcTimestampSource for FixedUtcTimestampSource {
    fn now(&self) -> String {
        self.timestamp.clone()
    }
}

/// 轮末真实 viewport sampler：沿用 capability 的同一 full reader，只返回 projection。
pub struct BackendRoundViewportSampler {
    reader: Arc<ViewportReader>,
    timestamp_source: Arc<dyn UtcTimestampSource>,
}

impl BackendRoundViewportSampler {
    pub fn new(reader: Arc<ViewportReader>) -> Self {
        Self::with_timestamp_source(reader, Arc::new(SystemUtcTimestampSource))
    }

    pub fn with_timestamp_source(
        reader: Arc<ViewportReader>,
        timestamp_source: Arc<dyn UtcTimestampSource>,
    ) -> Self {
        Self {
            reader,
            timestamp_source,
        }
    }
}

impl RoundViewportSampler for BackendRoundViewportSampler {
    type Viewport = mineintent_contracts::minecraft::ViewportProjection;

    fn timestamp(&self) -> String {
        self.timestamp_source.now()
    }

    fn sample<'a>(
        &'a self,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<Self::Viewport, AgentError>> {
        self.reader.read_full(control)
    }
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
