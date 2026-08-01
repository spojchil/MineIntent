use std::time::{Duration, Instant};

use super::AgentError;

pub trait CancellationSignal: Send + Sync {
    fn cancellation_error(&self) -> Option<AgentError>;
}

#[derive(Clone, Copy, Debug)]
pub struct Deadline {
    expires_at: Instant,
}

impl Deadline {
    pub fn at(expires_at: Instant) -> Self {
        Self { expires_at }
    }

    pub fn after(now: Instant, duration: Duration) -> Self {
        Self::at(now + duration)
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
