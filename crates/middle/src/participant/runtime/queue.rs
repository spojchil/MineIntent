//! 有界事件队列：准入、四条 lane、溢出标记与清理编排。

use super::*;

pub(super) struct RuntimeState {
    pub(super) lifecycle: ParticipantLifecycle,
    pub(super) scope: Option<ParticipantScope>,
    pub(super) generation: u64,
    pub(super) next_ordinal: u64,
    pub(super) active: Option<ActiveRun>,
    pub(super) terminal_pending: bool,
    // There is no frozen backend seam which can prove that an evicted
    // process-session identity can never return. Keep non-sensitive digests
    // for this runtime lifetime instead of arbitrarily re-admitting an old
    // session after a fixed-size queue rolls over.
    pub(super) retired_process_sessions: std::collections::HashSet<String>,
    // A backend ConnectionClosed invalidates this exact scope while the
    // Participant remains Running for a possible reconnect. Keeping the
    // tombstone prevents an old same-epoch event from reactivating a scope
    // merely because `scope` is temporarily None.
    pub(super) closed_scope: Option<ParticipantScope>,
    pub(super) closed_connection_attempt_id: Option<String>,
    pub(super) active_connection_attempt_id: Option<String>,
}

pub(super) struct ActiveRun {
    pub(super) cancellation: Arc<ParticipantCancellation>,
    pub(super) abort: Option<AbortHandle>,
    pub(super) start_gate: Arc<ParticipantStartGate>,
}

#[derive(Clone)]
pub(super) struct WakeItem {
    pub(super) ordinal: u64,
    pub(super) scope: ParticipantScope,
    pub(super) occurred_at: String,
    pub(super) trigger: PlayerChatMessage,
    pub(super) trigger_retained: bool,
}

pub(super) struct WorkItem {
    pub(super) ticket: u64,
    pub(super) ordinal: u64,
    pub(super) generation: u64,
    pub(super) scope: ParticipantScope,
    pub(super) occurred_at: String,
    pub(super) event_id: String,
    pub(super) event_type: String,
    pub(super) wake: Option<WakeItem>,
    pub(super) scope_control: bool,
    pub(super) terminal: bool,
    pub(super) terminal_lifecycle: Option<ParticipantLifecycle>,
    pub(super) overflow: Option<OverflowInfo>,
}

#[derive(Clone)]
pub(super) struct OverflowInfo {
    pub(super) dropped_count: u64,
    pub(super) dropped_types: Vec<String>,
}

pub(super) struct OverflowEntry {
    pub(super) ticket: u64,
    pub(super) item: WorkItem,
    pub(super) dropped_count: u64,
    pub(super) dropped_types: Vec<String>,
}

pub(super) enum QueueAdmission {
    Accepted,
    Ignored,
    OrdinaryDropped { event_type: String },
}

/// Admission normally holds the runtime serial for the complete synchronous
/// admission transaction.  A bounded queue wait is the one exception: the
/// producer must not pin this guard while waiting for the worker to make room,
/// because body observationAfter needs the same serial to drain facts.
pub(super) struct AdmissionSerialGuard<'a> {
    pub(super) serial: &'a Mutex<()>,
    pub(super) guard: Option<MutexGuard<'a, ()>>,
}

impl<'a> AdmissionSerialGuard<'a> {
    pub(super) fn new(serial: &'a Mutex<()>) -> Self {
        Self {
            serial,
            guard: Some(lock(serial)),
        }
    }

    pub(super) fn release(&mut self) {
        drop(self.guard.take());
    }

    pub(super) fn reacquire(&mut self) {
        debug_assert!(self.guard.is_none());
        self.guard = Some(lock(self.serial));
    }
}

pub(super) struct ParticipantEventQueue {
    pub(super) state: Mutex<ParticipantEventQueueState>,
    pub(super) wake: Condvar,
    pub(super) notify: Notify,
    pub(super) waiter_notify: Notify,
}

pub(super) struct ParticipantEventQueueState {
    pub(super) ordinary: VecDeque<WorkItem>,
    pub(super) control: VecDeque<WorkItem>,
    pub(super) overflow: VecDeque<OverflowEntry>,
    pub(super) terminal: Option<WorkItem>,
    pub(super) next_ticket: u64,
    pub(super) next_admission: u64,
    pub(super) open_loss_segment: Option<u64>,
    pub(super) waiting_producers: usize,
    pub(super) closed: bool,
}

impl ParticipantEventQueue {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ParticipantEventQueueState {
                ordinary: VecDeque::new(),
                control: VecDeque::new(),
                overflow: VecDeque::new(),
                terminal: None,
                next_ticket: 1,
                next_admission: 1,
                open_loss_segment: None,
                waiting_producers: 0,
                closed: false,
            }),
            wake: Condvar::new(),
            notify: Notify::new(),
            waiter_notify: Notify::new(),
        })
    }

    pub(super) fn enqueue(
        &self,
        mut item: WorkItem,
        serial: &mut AdmissionSerialGuard<'_>,
        mut is_current: impl FnMut(&WorkItem) -> bool,
    ) -> Result<QueueAdmission, ParticipantRuntimeError> {
        let mut state = lock(&self.state);
        let ticket = state.next_ticket;
        state.next_ticket = state.next_ticket.saturating_add(1);
        item.ticket = ticket;

        loop {
            while !state.closed && state.next_admission != ticket {
                state.waiting_producers = state.waiting_producers.saturating_add(1);
                self.waiter_notify.notify_waiters();
                serial.release();
                state = self
                    .wake
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.waiting_producers = state.waiting_producers.saturating_sub(1);
                self.waiter_notify.notify_waiters();
                drop(state);
                serial.reacquire();
                state = lock(&self.state);
            }
            if state.closed {
                if state.next_admission == ticket {
                    state.next_admission = state.next_admission.saturating_add(1);
                    self.wake.notify_all();
                }
                return Err(ParticipantRuntimeError::QueueClosed);
            }

            // The queue lock is intentionally not held while checking runtime
            // scope/generation.  At this point the admission serial has been
            // reacquired after any wait, so a stale producer can cancel its
            // reserved ticket before publishing an old item.
            drop(state);
            let current = is_current(&item);
            state = lock(&self.state);
            if state.closed {
                if state.next_admission == ticket {
                    state.next_admission = state.next_admission.saturating_add(1);
                    self.wake.notify_all();
                }
                return Err(ParticipantRuntimeError::QueueClosed);
            }
            if state.next_admission != ticket {
                continue;
            }
            if !current {
                state.next_admission = state.next_admission.saturating_add(1);
                self.wake.notify_all();
                return Ok(QueueAdmission::Ignored);
            }

            if item.terminal {
                if state.terminal.is_none() {
                    state.terminal = Some(item);
                }
                state.open_loss_segment = None;
                self.commit_admission(&mut state);
                return Ok(QueueAdmission::Accepted);
            }

            if item.wake.is_some() || item.scope_control {
                if state.control.len() < PARTICIPANT_CONTROL_CAPACITY {
                    state.control.push_back(item);
                    state.open_loss_segment = None;
                    self.commit_admission(&mut state);
                    return Ok(QueueAdmission::Accepted);
                }
            } else if state.ordinary.len() < PARTICIPANT_ORDINARY_CAPACITY {
                state.ordinary.push_back(item);
                state.open_loss_segment = None;
                self.commit_admission(&mut state);
                return Ok(QueueAdmission::Accepted);
            } else {
                let event_type = item.event_type.clone();
                if state.open_loss_segment.is_some_and(|segment| {
                    state
                        .overflow
                        .back()
                        .is_some_and(|overflow| overflow.ticket == segment)
                }) {
                    if let Some(overflow) = state.overflow.back_mut() {
                        overflow.dropped_count = overflow.dropped_count.saturating_add(1);
                        add_overflow_type(&mut overflow.dropped_types, &event_type);
                    }
                    self.commit_admission(&mut state);
                    return Ok(QueueAdmission::OrdinaryDropped { event_type });
                }
                if state.overflow.len() < PARTICIPANT_OVERFLOW_CAPACITY {
                    let mut marker = item;
                    marker.event_id = format!("participant-overflow-{ticket}");
                    marker.event_type = "participant_events_omitted".to_owned();
                    marker.wake = None;
                    marker.scope_control = true;
                    marker.terminal = false;
                    marker.terminal_lifecycle = None;
                    marker.overflow = Some(OverflowInfo {
                        dropped_count: 1,
                        dropped_types: vec![event_type.clone()],
                    });
                    state.overflow.push_back(OverflowEntry {
                        ticket,
                        item: marker,
                        dropped_count: 1,
                        dropped_types: vec![event_type.clone()],
                    });
                    state.open_loss_segment = Some(ticket);
                    self.commit_admission(&mut state);
                    return Ok(QueueAdmission::OrdinaryDropped { event_type });
                }
            }

            state.waiting_producers = state.waiting_producers.saturating_add(1);
            self.waiter_notify.notify_waiters();
            serial.release();
            state = self
                .wake
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.waiting_producers = state.waiting_producers.saturating_sub(1);
            self.waiter_notify.notify_waiters();
            drop(state);
            serial.reacquire();
            state = lock(&self.state);
        }
    }

    pub(super) fn commit_admission(&self, state: &mut ParticipantEventQueueState) {
        state.next_admission = state.next_admission.saturating_add(1);
        self.wake.notify_all();
        self.notify.notify_one();
    }

    pub(super) async fn next(&self) -> Option<WorkItem> {
        loop {
            let notified = self.notify.notified();
            let result = {
                let mut state = lock(&self.state);
                if let Some(item) = state.pop_next() {
                    self.wake.notify_all();
                    Some(Some(item))
                } else if state.closed {
                    Some(None)
                } else {
                    None
                }
            };
            if let Some(item) = result {
                return item;
            }
            notified.await;
        }
    }

    pub(super) fn close_admission(&self) {
        let mut state = lock(&self.state);
        state.closed = true;
        self.wake.notify_all();
        self.notify.notify_waiters();
        self.waiter_notify.notify_waiters();
    }

    pub(super) async fn wait_for_waiters(&self, expected: usize) {
        loop {
            let notified = self.waiter_notify.notified();
            if lock(&self.state).waiting_producers >= expected {
                return;
            }
            notified.await;
        }
    }

    pub(super) fn counts(&self) -> (usize, usize, usize, usize, usize) {
        let state = lock(&self.state);
        (
            state.ordinary.len(),
            state.control.len(),
            state.overflow.len(),
            usize::from(state.terminal.is_some()),
            state.waiting_producers,
        )
    }
}

impl ParticipantEventQueueState {
    pub(super) fn pop_next(&mut self) -> Option<WorkItem> {
        let mut candidate: Option<(u64, QueueLane)> = None;
        if let Some(item) = self.ordinary.front() {
            candidate = Some((item.ticket, QueueLane::Ordinary));
        }
        if let Some(item) = self.control.front() {
            if candidate.is_none_or(|(ticket, _)| item.ticket < ticket) {
                candidate = Some((item.ticket, QueueLane::Control));
            }
        }
        if let Some(item) = self.overflow.front() {
            if candidate.is_none_or(|(ticket, _)| item.ticket < ticket) {
                candidate = Some((item.ticket, QueueLane::Overflow));
            }
        }
        if let Some(item) = self.terminal.as_ref() {
            if candidate.is_none_or(|(ticket, _)| item.ticket < ticket) {
                candidate = Some((item.ticket, QueueLane::Terminal));
            }
        }
        let (_, lane) = candidate?;
        match lane {
            QueueLane::Ordinary => self.ordinary.pop_front(),
            QueueLane::Control => self.control.pop_front(),
            QueueLane::Overflow => self.overflow.pop_front().map(|mut entry| {
                if self.open_loss_segment == Some(entry.ticket) {
                    self.open_loss_segment = None;
                }
                entry.item.overflow = Some(OverflowInfo {
                    dropped_count: entry.dropped_count,
                    dropped_types: entry.dropped_types,
                });
                entry.item
            }),
            QueueLane::Terminal => self.terminal.take(),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum QueueLane {
    Ordinary,
    Control,
    Overflow,
    Terminal,
}

pub(super) enum BackendAdmission {
    Accepted {
        generation: u64,
        ordinal: u64,
        cleanup: Cleanup,
        scope_control: bool,
        record_fact: bool,
        terminal: bool,
    },
    StaleIgnored,
}

pub(super) enum ExplicitAdmission {
    Accepted {
        generation: u64,
        ordinal: u64,
        cleanup: Cleanup,
        terminal_lifecycle: Option<ParticipantLifecycle>,
    },
    StaleIgnored,
}

pub(super) struct Cleanup {
    pub(super) required: bool,
    pub(super) cancellation: Option<Arc<ParticipantCancellation>>,
    pub(super) abort: Option<AbortHandle>,
    pub(super) start_gate: Option<Arc<ParticipantStartGate>>,
}

impl Cleanup {
    pub(super) fn empty() -> Self {
        Self {
            required: false,
            cancellation: None,
            abort: None,
            start_gate: None,
        }
    }

    pub(super) fn required() -> Self {
        Self {
            required: true,
            ..Self::empty()
        }
    }
}
