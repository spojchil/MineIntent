use std::{
    any::Any,
    collections::VecDeque,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use thiserror::Error;
use tokio::{
    runtime::Handle,
    sync::Notify,
    task::JoinHandle,
    time::{sleep_until, Instant},
};

use super::{
    segment::{is_javascript_whitespace, SegmentChatError},
    segment_chat, SpeechEvent, SpeechRequest, SpeechTransport, DEFAULT_MAX_SEGMENT_LENGTH,
};

pub type SpeechEventHandler = Arc<dyn Fn(SpeechEvent) + Send + Sync + 'static>;

#[derive(Clone)]
pub struct SpeechSchedulerOptions {
    pub max_segment_length: usize,
    pub minimum_interval: Duration,
    pub on_event: SpeechEventHandler,
}

impl Default for SpeechSchedulerOptions {
    fn default() -> Self {
        Self {
            max_segment_length: DEFAULT_MAX_SEGMENT_LENGTH,
            minimum_interval: Duration::from_millis(1_000),
            on_event: Arc::new(|_| {}),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SpeechSchedulerBuildError {
    #[error("max segment length must be positive")]
    InvalidMaxSegmentLength,
    #[error("SpeechScheduler requires an active Tokio runtime")]
    RuntimeUnavailable,
    #[error("minimum interval is outside the supported Tokio instant range")]
    InvalidMinimumInterval,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SpeechScheduleError {
    #[error("speech request requires id and text")]
    InvalidRequest,
    #[error("duplicate speech request: {request_id}")]
    DuplicateRequest { request_id: String },
    #[error(transparent)]
    Segment(#[from] SegmentChatError),
}

pub struct SpeechScheduler<T>
where
    T: SpeechTransport + 'static,
{
    inner: Arc<SchedulerInner<T>>,
    worker: JoinHandle<()>,
}

struct SchedulerInner<T>
where
    T: SpeechTransport + 'static,
{
    transport: T,
    max_segment_length: usize,
    minimum_interval: Duration,
    on_event: SpeechEventHandler,
    changed: Notify,
    state: Mutex<SchedulerState>,
}

#[derive(Default)]
struct SchedulerState {
    queue: VecDeque<QueuedSpeech>,
    next_generation: u64,
    last_sent_at: Option<Instant>,
}

struct QueuedSpeech {
    request: SpeechRequest,
    segments: Vec<String>,
    next: usize,
    generation: u64,
    dispatching: bool,
    cancel_after_dispatch: Option<String>,
}

struct Delivery {
    request_id: String,
    text: String,
    segment: usize,
    generation: u64,
}

impl<T> SpeechScheduler<T>
where
    T: SpeechTransport + 'static,
{
    pub fn new(
        transport: T,
        options: SpeechSchedulerOptions,
    ) -> Result<Self, SpeechSchedulerBuildError> {
        if options.max_segment_length == 0 {
            return Err(SpeechSchedulerBuildError::InvalidMaxSegmentLength);
        }
        let runtime =
            Handle::try_current().map_err(|_| SpeechSchedulerBuildError::RuntimeUnavailable)?;
        if Instant::now()
            .checked_add(options.minimum_interval)
            .is_none()
        {
            return Err(SpeechSchedulerBuildError::InvalidMinimumInterval);
        }
        let inner = Arc::new(SchedulerInner {
            transport,
            max_segment_length: options.max_segment_length,
            minimum_interval: options.minimum_interval,
            on_event: options.on_event,
            changed: Notify::new(),
            state: Mutex::new(SchedulerState::default()),
        });
        let worker = runtime.spawn(worker_loop(Arc::clone(&inner)));
        Ok(Self { inner, worker })
    }

    /// Queues one request and synchronously emits `scheduled`; delivery always occurs from the
    /// worker after this method returns to the async runtime.
    pub fn schedule(&self, request: SpeechRequest) -> Result<usize, SpeechScheduleError> {
        if request.id.is_empty()
            || request
                .text
                .trim_matches(is_javascript_whitespace)
                .is_empty()
        {
            return Err(SpeechScheduleError::InvalidRequest);
        }
        {
            let state = lock_recover(&self.inner.state);
            if state
                .queue
                .iter()
                .any(|queued| queued.request.id == request.id)
            {
                return Err(SpeechScheduleError::DuplicateRequest {
                    request_id: request.id,
                });
            }
        }
        let segments = segment_chat(&request.text, self.inner.max_segment_length)?;

        let segment_count = segments.len();
        {
            let mut state = lock_recover(&self.inner.state);
            // Recheck after segmentation so concurrent callers cannot enqueue the same id.
            if state
                .queue
                .iter()
                .any(|queued| queued.request.id == request.id)
            {
                return Err(SpeechScheduleError::DuplicateRequest {
                    request_id: request.id,
                });
            }
            state.next_generation = state.next_generation.wrapping_add(1);
            let generation = state.next_generation;
            state.queue.push_back(QueuedSpeech {
                request: request.clone(),
                segments,
                next: 0,
                generation,
                dispatching: false,
                cancel_after_dispatch: None,
            });
        }

        (self.inner.on_event)(SpeechEvent::Scheduled {
            request_id: request.id,
            segments: segment_count,
        });
        self.inner.changed.notify_one();
        Ok(segment_count)
    }

    pub fn stop(&self) {
        self.stop_with_reason("scheduler_stopped");
    }

    pub fn stop_with_reason(&self, reason: impl Into<String>) {
        let reason = reason.into();
        let cancelled: Vec<String> = {
            let mut state = lock_recover(&self.inner.state);
            let mut retained = VecDeque::new();
            let mut cancelled = Vec::new();
            while let Some(mut queued) = state.queue.pop_front() {
                if queued.dispatching {
                    if queued.cancel_after_dispatch.is_none() {
                        queued.cancel_after_dispatch = Some(reason.clone());
                    }
                    retained.push_back(queued);
                } else {
                    cancelled.push(queued.request.id);
                }
            }
            state.queue = retained;
            cancelled
        };
        self.inner.changed.notify_one();
        for request_id in cancelled {
            (self.inner.on_event)(SpeechEvent::Cancelled {
                request_id,
                reason: reason.clone(),
            });
        }
    }
}

impl<T> Drop for SpeechScheduler<T>
where
    T: SpeechTransport + 'static,
{
    fn drop(&mut self) {
        self.worker.abort();
    }
}

async fn worker_loop<T>(inner: Arc<SchedulerInner<T>>)
where
    T: SpeechTransport + 'static,
{
    loop {
        let changed = inner.changed.notified();
        let deadline = {
            let state = lock_recover(&inner.state);
            if state.queue.is_empty() {
                None
            } else {
                Some(
                    state
                        .last_sent_at
                        .and_then(|sent_at| sent_at.checked_add(inner.minimum_interval))
                        .unwrap_or_else(Instant::now),
                )
            }
        };

        let Some(deadline) = deadline else {
            changed.await;
            continue;
        };
        tokio::select! {
            biased;
            _ = changed => continue,
            _ = sleep_until(deadline) => {}
        }
        // Even a zero-delay first segment is an asynchronous delivery turn.
        tokio::task::yield_now().await;

        let delivery = {
            let mut state = lock_recover(&inner.state);
            state.queue.front_mut().and_then(|queued| {
                if queued.dispatching {
                    return None;
                }
                let text = queued.segments.get(queued.next)?.clone();
                queued.dispatching = true;
                Some(Delivery {
                    request_id: queued.request.id.clone(),
                    text,
                    segment: queued.next,
                    generation: queued.generation,
                })
            })
        };
        let Some(delivery) = delivery else {
            continue;
        };

        let result = catch_unwind(AssertUnwindSafe(|| inner.transport.send(&delivery.text)))
            .map_err(|payload| panic_message(payload.as_ref()))
            .and_then(|result| result.map_err(|error| error.to_string()));
        let completion = {
            let mut state = lock_recover(&inner.state);
            let is_current = state.queue.front().is_some_and(|queued| {
                queued.generation == delivery.generation
                    && queued.next == delivery.segment
                    && queued.dispatching
            });
            if !is_current {
                None
            } else {
                match result {
                    Ok(()) => {
                        state.last_sent_at = Some(Instant::now());
                        if let Some(queued) = state.queue.front_mut() {
                            queued.next = queued.next.saturating_add(1);
                        }
                        Some((
                            SpeechEvent::Sent {
                                request_id: delivery.request_id,
                                segment: delivery.segment,
                                text: delivery.text,
                            },
                            true,
                        ))
                    }
                    Err(reason) => Some((
                        SpeechEvent::Failed {
                            request_id: delivery.request_id,
                            reason,
                        },
                        false,
                    )),
                }
            }
        };
        let Some((event, sent)) = completion else {
            continue;
        };
        (inner.on_event)(event);

        let cancellation = {
            let mut state = lock_recover(&inner.state);
            let is_current = state.queue.front().is_some_and(|queued| {
                queued.generation == delivery.generation && queued.dispatching
            });
            if !is_current {
                None
            } else if !sent {
                state.queue.pop_front();
                None
            } else {
                match state.queue.front_mut() {
                    Some(queued) if queued.next == queued.segments.len() => {
                        state.queue.pop_front();
                        None
                    }
                    Some(queued) => match queued.cancel_after_dispatch.take() {
                        Some(reason) => {
                            let request_id = queued.request.id.clone();
                            state.queue.pop_front();
                            Some(SpeechEvent::Cancelled { request_id, reason })
                        }
                        None => {
                            queued.dispatching = false;
                            None
                        }
                    },
                    None => None,
                }
            }
        };
        if let Some(event) = cancellation {
            (inner.on_event)(event);
        }
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "speech transport panicked".to_owned())
}

/// Scheduler locks protect only local queue bookkeeping. No await, transport call or event callback
/// occurs while held; poison is recovered so an unrelated panic is not propagated through later
/// scheduler calls.
fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
