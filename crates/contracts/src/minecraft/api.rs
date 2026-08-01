use std::{future::Future, pin::Pin, sync::Arc};

use serde::{Deserialize, Serialize};

use super::{
    BackendError, BackendEventEnvelope, BackendState, BlockPosition, BlockReadResult,
    MinecraftSnapshotV1, ProtocolBlockEvent, ProtocolEntityEvent, ProtocolEntitySnapshot,
    ProtocolSoundPayload, SelfPose, ViewportRead,
};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackendReady {
    pub process_session_id: String,
    pub connection_epoch: u64,
    pub connection_attempt_id: String,
    pub snapshot: MinecraftSnapshotV1,
}

/// 逐调用取消抽象；具体 token、线程或 executor 不属于 contracts。
pub trait CancellationSignal: Send + Sync {
    fn is_cancelled(&self) -> bool;
}

/// 逐调用期限抽象；由宿主选择单调时钟和计时实现。
pub trait Deadline: Send + Sync {
    fn has_elapsed(&self) -> bool;
}

#[derive(Clone)]
pub struct OperationControl {
    cancellation: Arc<dyn CancellationSignal>,
    deadline: Option<Arc<dyn Deadline>>,
}

impl OperationControl {
    pub fn new(
        cancellation: Arc<dyn CancellationSignal>,
        deadline: Option<Arc<dyn Deadline>>,
    ) -> Self {
        Self {
            cancellation,
            deadline,
        }
    }

    pub fn cancellation(&self) -> &dyn CancellationSignal {
        self.cancellation.as_ref()
    }

    pub fn deadline(&self) -> Option<&dyn Deadline> {
        self.deadline.as_deref()
    }

    /// 供 adapter/backend 在进入一次操作前使用的统一边界检查。
    pub fn preflight(&self, operation: &str) -> Result<(), BackendError> {
        if self.cancellation.is_cancelled() {
            return Err(BackendError::Cancelled {
                operation: operation.to_owned(),
            });
        }
        if self.deadline().is_some_and(Deadline::has_elapsed) {
            return Err(BackendError::DeadlineExceeded {
                operation: operation.to_owned(),
            });
        }
        Ok(())
    }
}

/// 显式订阅所有权。实现必须有界；unsubscribe/close 后不得再交付回调。
pub trait Subscription: Send {
    fn unsubscribe(&mut self);
    fn is_closed(&self) -> bool;
}

pub trait BackendEventListener: Send + Sync {
    fn on_event(&self, event: BackendEventEnvelope);
}

#[derive(Clone, Debug, PartialEq)]
pub enum ObservationEvent {
    Entity(BackendEventEnvelope<ProtocolEntityEvent>),
    Block(BackendEventEnvelope<ProtocolBlockEvent>),
    Sound(BackendEventEnvelope<ProtocolSoundPayload>),
}

pub trait ObservationEventListener: Send + Sync {
    fn on_event(&self, event: ObservationEvent);
}

/// 创建时绑定一个 connection epoch；所有方法都必须先拒绝 stale epoch。
pub trait ProtocolObservationSource: Send + Sync {
    fn epoch(&self) -> u64;
    fn self_pose(&self) -> Result<SelfPose, BackendError>;
    fn list_tracked_entities(&self) -> Result<Vec<ProtocolEntitySnapshot>, BackendError>;
    fn read_block(&self, position: BlockPosition) -> Result<BlockReadResult, BackendError>;
    fn subscribe(
        &self,
        listener: Arc<dyn ObservationEventListener>,
    ) -> Result<Box<dyn Subscription>, BackendError>;
    fn read_viewport(
        &self,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<ViewportRead, BackendError>>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotorMoveDirection {
    Forward,
    Back,
    Left,
    Right,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LookRelativeRequest {
    /// Mouse-language degrees: positive is right.
    pub yaw_degrees: f64,
    /// Mouse-language degrees: positive is down.
    pub pitch_degrees: f64,
}

impl LookRelativeRequest {
    pub fn validate(&self) -> Result<(), BackendError> {
        for (field, value) in [
            ("yawDegrees", self.yaw_degrees),
            ("pitchDegrees", self.pitch_degrees),
        ] {
            if !value.is_finite() || value.abs() > 90.0 {
                return Err(BackendError::InvalidCommand {
                    field: field.to_owned(),
                    message: "must be finite and within ±90 degrees".to_owned(),
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoveInputRequest {
    pub directions: Vec<MotorMoveDirection>,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sprint: Option<bool>,
}

impl MoveInputRequest {
    pub fn validate(&self) -> Result<(), BackendError> {
        if self.directions.is_empty() || self.directions.len() > 4 {
            return Err(BackendError::InvalidCommand {
                field: "directions".to_owned(),
                message: "must contain from 1 to 4 direction keys".to_owned(),
            });
        }
        let mut unique = std::collections::HashSet::with_capacity(self.directions.len());
        if !self
            .directions
            .iter()
            .all(|direction| unique.insert(*direction))
        {
            return Err(BackendError::InvalidCommand {
                field: "directions".to_owned(),
                message: "must not contain duplicate keys".to_owned(),
            });
        }
        if !(50..=1_500).contains(&self.duration_ms) {
            return Err(BackendError::InvalidCommand {
                field: "durationMs".to_owned(),
                message: "must be an integer from 50 to 1500 milliseconds".to_owned(),
            });
        }
        Ok(())
    }
}

pub trait MinecraftMotorDriverApi: Send + Sync {
    fn look_relative(
        &self,
        request: LookRelativeRequest,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<(), BackendError>>;
    fn move_input(
        &self,
        request: MoveInputRequest,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<(), BackendError>>;
    /// Synchronous, idempotent and generation-safe release of all held controls.
    fn release_all(&self) -> Result<(), BackendError>;
}

pub trait MinecraftBackendApi: Send + Sync {
    /// Completes only at ready, cancellation, deadline or terminal failure.
    fn start(&self, control: OperationControl)
        -> BoxFuture<'_, Result<BackendReady, BackendError>>;
    /// Completes after owned resources are released and the final stopped state is visible.
    fn stop(
        &self,
        reason: String,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<(), BackendError>>;
    fn state(&self) -> BackendState;
    fn snapshot(&self) -> Result<MinecraftSnapshotV1, BackendError>;
    fn subscribe(
        &self,
        listener: Arc<dyn BackendEventListener>,
    ) -> Result<Box<dyn Subscription>, BackendError>;
    fn observation_source(&self) -> Result<Arc<dyn ProtocolObservationSource>, BackendError>;
    fn motor(&self) -> Result<Arc<dyn MinecraftMotorDriverApi>, BackendError>;
    fn send_chat(&self, message: String) -> Result<(), BackendError>;
}
