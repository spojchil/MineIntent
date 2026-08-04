use std::{
    future::Future,
    pin::Pin,
    time::{Duration, Instant},
};

use super::{AgentError, AgentErrorCode};

pub trait CancellationSignal: Send + Sync {
    /// Returns the cancellation state without waiting.
    fn cancellation_error(&self) -> Option<AgentError>;

    /// Waits until cancellation and resolves with its structured error.
    ///
    /// Each call must be independently waitable. An already-cancelled signal should return a
    /// ready future; an active signal must retain the task waker so cancellation can wake blocked
    /// provider/runner work.
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = AgentError> + Send + '_>>;
}

#[derive(Clone, Copy, Debug)]
pub struct Deadline {
    expires_at: Instant,
}

impl Deadline {
    pub fn at(expires_at: Instant) -> Self {
        Self { expires_at }
    }

    pub fn after(now: Instant, duration: Duration) -> Result<Self, AgentError> {
        now.checked_add(duration)
            .map(Self::at)
            .ok_or_else(|| AgentError::new(AgentErrorCode::InvalidRequest, "deadline_out_of_range"))
    }

    pub fn expires_at(self) -> Instant {
        self.expires_at
    }

    pub fn remaining_at(self, now: Instant) -> Result<Duration, AgentError> {
        self.expires_at
            .checked_duration_since(now)
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(AgentError::deadline_exceeded)
    }
}

#[derive(Clone, Copy)]
pub struct ExecutionControl<'a> {
    cancellation: &'a dyn CancellationSignal,
    deadline: Deadline,
}

impl<'a> ExecutionControl<'a> {
    pub fn new(cancellation: &'a dyn CancellationSignal, deadline: Deadline) -> Self {
        Self {
            cancellation,
            deadline,
        }
    }

    pub fn cancellation(self) -> &'a dyn CancellationSignal {
        self.cancellation
    }

    /// Returns the waitable cancellation branch for an async execution boundary.
    ///
    /// Runtime implementations can select this future against a timer scheduled for
    /// [`Deadline::expires_at`]. After either branch wakes, call [`Self::check_at`] with the
    /// current instant; that preserves cancellation priority when both become ready together.
    pub fn cancelled(self) -> Pin<Box<dyn Future<Output = AgentError> + Send + 'a>> {
        self.cancellation.cancelled()
    }

    pub fn deadline(self) -> Deadline {
        self.deadline
    }

    pub fn check_at(self, now: Instant) -> Result<(), AgentError> {
        if let Some(error) = self.cancellation.cancellation_error() {
            return Err(error);
        }
        self.deadline.remaining_at(now).map(|_| ())
    }
}
