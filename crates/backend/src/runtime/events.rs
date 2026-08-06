//! Bounded runtime dispatch, subscriber queues, and callback leases.

use super::*;

pub(super) struct EventDispatchState {
    ordinary: VecDeque<RuntimeDispatchEntry>,
    pub(super) control: VecDeque<RuntimeDispatchEntry>,
    overflow: VecDeque<RuntimeDispatchOverflow>,
    terminal: Option<RuntimeDispatchEntry>,
    pub(super) next_sequence: u64,
    pub(super) next_admission: u64,
    open_loss_segment: Option<u64>,
    pub(super) drainer_active: bool,
}

impl Default for EventDispatchState {
    fn default() -> Self {
        Self {
            ordinary: VecDeque::new(),
            control: VecDeque::new(),
            overflow: VecDeque::new(),
            terminal: None,
            next_sequence: 1,
            next_admission: 1,
            open_loss_segment: None,
            drainer_active: false,
        }
    }
}

pub(super) struct RuntimeDispatchEntry {
    sequence: u64,
    event: BackendEventEnvelope,
}

pub(super) struct RuntimeDispatchOverflow {
    sequence: u64,
    template: BackendEventEnvelope,
    dropped_count: u64,
    dropped_kinds: Vec<BackendEventKind>,
}

pub(super) struct RuntimeDispatchPending {
    sequence: u64,
    event: BackendEventEnvelope,
}

pub(super) enum RuntimeDispatchAdmission {
    Accepted(bool),
    Wait,
    Cancelled,
}

#[derive(Clone, Copy)]
pub(super) enum RuntimeDispatchLane {
    Ordinary,
    Control,
    Overflow,
    Terminal,
}

pub(crate) const RUNTIME_DISPATCH_ORDINARY_CAPACITY: usize = 256;
pub(crate) const RUNTIME_DISPATCH_CONTROL_CAPACITY: usize = 512;
pub(crate) const RUNTIME_DISPATCH_OVERFLOW_CAPACITY: usize = 64;

impl EventDispatchState {
    /// Admit into the finite runtime-owner queue and elect exactly one
    /// drainer.  This is the admission immediately before the runtime broker;
    /// it must not become an unbounded spill queue while the broker is
    /// backpressured.  Ordinary reconstructable facts use the same contiguous
    /// loss-segment rule as the broker.  Control facts wait only while stop has
    /// not cancelled the producer, and stopped has a reserved terminal slot.
    pub(super) fn enqueue(
        &mut self,
        event: &mut Option<BackendEventEnvelope>,
        pending: &mut Option<RuntimeDispatchPending>,
        cancel: &AtomicBool,
    ) -> RuntimeDispatchAdmission {
        if pending.is_none() {
            let event = event.take().expect("dispatch event must be supplied once");
            let sequence = self.next_sequence;
            self.next_sequence = self.next_sequence.wrapping_add(1);
            *pending = Some(RuntimeDispatchPending { sequence, event });
        }
        let pending_sequence = pending
            .as_ref()
            .expect("dispatch pending event must exist")
            .sequence;
        if self.next_admission != pending_sequence {
            if cancel.load(Ordering::Acquire) {
                pending.take();
                self.advance_admission();
                return RuntimeDispatchAdmission::Cancelled;
            }
            return RuntimeDispatchAdmission::Wait;
        }
        let pending_event = &pending
            .as_ref()
            .expect("dispatch pending event must exist")
            .event;
        if is_runtime_terminal_event(pending_event) {
            let pending = pending.take().expect("terminal pending event");
            if self.terminal.is_none() {
                self.terminal = Some(RuntimeDispatchEntry {
                    sequence: pending.sequence,
                    event: pending.event,
                });
            }
            self.close_loss_segment();
            self.advance_admission();
            return RuntimeDispatchAdmission::Accepted(self.activate_drainer());
        }
        if is_runtime_droppable_event(pending_event) {
            if self.ordinary.len() < RUNTIME_DISPATCH_ORDINARY_CAPACITY {
                let pending = pending.take().expect("ordinary pending event");
                self.ordinary.push_back(RuntimeDispatchEntry {
                    sequence: pending.sequence,
                    event: pending.event,
                });
                self.close_loss_segment();
                self.advance_admission();
                return RuntimeDispatchAdmission::Accepted(self.activate_drainer());
            }
            if self.open_loss_segment.is_some_and(|sequence| {
                self.overflow
                    .back()
                    .is_some_and(|overflow| overflow.sequence == sequence)
            }) {
                self.record_overflow_loss(pending_event.kind);
                pending.take();
                self.advance_admission();
                return RuntimeDispatchAdmission::Accepted(false);
            }
            if self.overflow.len() < RUNTIME_DISPATCH_OVERFLOW_CAPACITY {
                let pending = pending.take().expect("overflow pending event");
                let kind = pending.event.kind;
                self.overflow.push_back(RuntimeDispatchOverflow {
                    sequence: pending.sequence,
                    template: pending.event,
                    dropped_count: 1,
                    dropped_kinds: vec![kind],
                });
                self.open_loss_segment = Some(pending_sequence);
                self.advance_admission();
                return RuntimeDispatchAdmission::Accepted(self.activate_drainer());
            }
        } else if self.control.len() < RUNTIME_DISPATCH_CONTROL_CAPACITY {
            let pending = pending.take().expect("control pending event");
            self.control.push_back(RuntimeDispatchEntry {
                sequence: pending.sequence,
                event: pending.event,
            });
            self.close_loss_segment();
            self.advance_admission();
            return RuntimeDispatchAdmission::Accepted(self.activate_drainer());
        }
        if cancel.load(Ordering::Acquire) {
            pending.take();
            self.advance_admission();
            RuntimeDispatchAdmission::Cancelled
        } else {
            RuntimeDispatchAdmission::Wait
        }
    }

    fn activate_drainer(&mut self) -> bool {
        if self.drainer_active {
            false
        } else {
            self.drainer_active = true;
            true
        }
    }

    fn advance_admission(&mut self) {
        self.next_admission = self.next_admission.wrapping_add(1);
    }

    fn close_loss_segment(&mut self) {
        self.open_loss_segment = None;
    }

    fn record_overflow_loss(&mut self, kind: BackendEventKind) {
        if let Some(overflow) = self.overflow.back_mut() {
            overflow.dropped_count = overflow.dropped_count.saturating_add(1);
            if !overflow.dropped_kinds.contains(&kind) {
                overflow.dropped_kinds.push(kind);
            }
        }
    }

    pub(super) fn pop_next(&mut self) -> Option<BackendEventEnvelope> {
        let mut candidate: Option<(u64, RuntimeDispatchLane)> = None;
        if let Some(entry) = self.ordinary.front() {
            candidate = Some((entry.sequence, RuntimeDispatchLane::Ordinary));
        }
        if let Some(entry) = self.control.front() {
            if candidate.is_none_or(|(sequence, _)| entry.sequence < sequence) {
                candidate = Some((entry.sequence, RuntimeDispatchLane::Control));
            }
        }
        if let Some(overflow) = self.overflow.front() {
            if candidate.is_none_or(|(sequence, _)| overflow.sequence < sequence) {
                candidate = Some((overflow.sequence, RuntimeDispatchLane::Overflow));
            }
        }
        if let Some(entry) = self.terminal.as_ref() {
            if candidate.is_none_or(|(sequence, _)| entry.sequence < sequence) {
                candidate = Some((entry.sequence, RuntimeDispatchLane::Terminal));
            }
        }
        let (_, lane) = candidate?;
        match lane {
            RuntimeDispatchLane::Ordinary => self.ordinary.pop_front().map(|entry| entry.event),
            RuntimeDispatchLane::Control => self.control.pop_front().map(|entry| entry.event),
            RuntimeDispatchLane::Terminal => self.terminal.take().map(|entry| entry.event),
            RuntimeDispatchLane::Overflow => {
                let overflow = self.overflow.pop_front()?;
                if self.open_loss_segment == Some(overflow.sequence) {
                    self.open_loss_segment = None;
                }
                Some(BackendEventEnvelope::from_payload(
                    mineintent_contracts::minecraft::BackendEventMetadata {
                        id: format!("runtime-dispatch-overflow-{}", overflow.sequence),
                        occurred_at: overflow.template.occurred_at,
                        process_session_id: overflow.template.process_session_id,
                        connection_epoch: overflow.template.connection_epoch,
                        connection_attempt_id: overflow.template.connection_attempt_id,
                        world_id: overflow.template.world_id,
                        dimension: overflow.template.dimension,
                    },
                    overflow.template.source,
                    BackendEventPayload::Overflow(
                        mineintent_contracts::minecraft::BackendOverflowPayload {
                            event_type: mineintent_contracts::minecraft::OverflowType::Overflow,
                            dropped_count: overflow.dropped_count,
                            dropped_kinds: overflow.dropped_kinds,
                        },
                    ),
                ))
            }
        }
    }

    #[cfg(test)]
    pub(super) fn queued_counts(&self) -> (usize, usize, usize, usize) {
        (
            self.ordinary.len(),
            self.control.len(),
            self.overflow.len(),
            usize::from(self.terminal.is_some()),
        )
    }
}

pub(super) const RUNTIME_BROKER_ORDINARY_CAPACITY: usize = 256;
pub(crate) const RUNTIME_BROKER_CONTROL_CAPACITY: usize = 512;
pub(super) const RUNTIME_BROKER_OVERFLOW_CAPACITY: usize = 64;

/// A finite subscriber queue between the runtime execution owner and the
/// facade's runtime-event broker.  The old unbounded Tokio channel allowed a
/// paused public callback to accumulate an unbounded upstream backlog.  This
/// queue keeps the same loss-position discipline as the public bridge: only
/// entity/block/sound facts may be dropped, control facts use bounded,
/// cancellation-aware backpressure, and a terminal event has its own slot.
pub(super) struct RuntimeEventQueue {
    state: parking_lot::Mutex<RuntimeEventQueueState>,
    wake: parking_lot::Condvar,
    notify: Notify,
    #[cfg(test)]
    backpressure_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

pub(super) struct RuntimeEventQueueState {
    ordinary: VecDeque<RuntimeEventEntry>,
    control: VecDeque<RuntimeEventEntry>,
    overflow: VecDeque<RuntimeOverflowEntry>,
    terminal: Option<RuntimeEventEntry>,
    next_sequence: u64,
    next_admission: u64,
    open_loss_segment: Option<u64>,
}

pub(super) struct RuntimeEventEntry {
    sequence: u64,
    event: BackendEventEnvelope,
}

pub(super) struct RuntimeOverflowEntry {
    sequence: u64,
    template: BackendEventEnvelope,
    dropped_count: u64,
    dropped_kinds: Vec<BackendEventKind>,
}

#[derive(Clone, Copy)]
pub(super) enum RuntimeQueueLane {
    Ordinary,
    Control,
    Overflow,
    Terminal,
}

/// Receiver returned by `RuntimeHandle::subscribe`.  It intentionally exposes
/// only bounded queue operations while retaining the small `recv`/`try_recv`
/// surface used by the runtime worker and deterministic tests.
pub struct RuntimeEventReceiver {
    pub(super) queue: Arc<RuntimeEventQueue>,
}

impl RuntimeEventQueue {
    pub(super) fn new(
        #[cfg(test)] backpressure_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: parking_lot::Mutex::new(RuntimeEventQueueState {
                ordinary: VecDeque::new(),
                control: VecDeque::new(),
                overflow: VecDeque::new(),
                terminal: None,
                next_sequence: 1,
                next_admission: 1,
                open_loss_segment: None,
            }),
            wake: parking_lot::Condvar::new(),
            notify: Notify::new(),
            #[cfg(test)]
            backpressure_hook: parking_lot::Mutex::new(backpressure_hook),
        })
    }

    pub(super) fn publish(&self, event: BackendEventEnvelope, cancel: &AtomicBool) -> bool {
        let mut state = self.state.lock();
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.wrapping_add(1);
        let mut event = Some(event);
        loop {
            if state.next_admission != sequence {
                if cancel.load(Ordering::Acquire) {
                    state.next_admission = state.next_admission.wrapping_add(1);
                    self.wake.notify_all();
                    return false;
                }
                #[cfg(test)]
                {
                    let hook = self.backpressure_hook.lock().take();
                    if let Some(hook) = hook {
                        hook();
                    }
                }
                self.wake.wait(&mut state);
                continue;
            }
            let pending_event = event
                .as_ref()
                .expect("runtime broker event must be supplied until admission");
            if is_runtime_terminal_event(pending_event) {
                let event = event.take().expect("terminal broker event");
                if state.terminal.is_none() {
                    state.terminal = Some(RuntimeEventEntry { sequence, event });
                }
                state.open_loss_segment = None;
                state.next_admission = state.next_admission.wrapping_add(1);
                self.wake.notify_all();
                self.notify.notify_one();
                return true;
            }
            if is_runtime_droppable_event(pending_event) {
                if state.ordinary.len() < RUNTIME_BROKER_ORDINARY_CAPACITY {
                    let event = event.take().expect("ordinary broker event");
                    state
                        .ordinary
                        .push_back(RuntimeEventEntry { sequence, event });
                    state.open_loss_segment = None;
                    state.next_admission = state.next_admission.wrapping_add(1);
                    self.wake.notify_all();
                    self.notify.notify_one();
                    return true;
                }
                if state.open_loss_segment.is_some_and(|segment| {
                    state
                        .overflow
                        .back()
                        .is_some_and(|overflow| overflow.sequence == segment)
                }) {
                    state.record_runtime_overflow(pending_event.kind);
                    event.take();
                    state.next_admission = state.next_admission.wrapping_add(1);
                    self.wake.notify_all();
                    return true;
                }
                if state.overflow.len() < RUNTIME_BROKER_OVERFLOW_CAPACITY {
                    let event = event.take().expect("overflow broker event");
                    let kind = event.kind;
                    state.overflow.push_back(RuntimeOverflowEntry {
                        sequence,
                        template: event,
                        dropped_count: 1,
                        dropped_kinds: vec![kind],
                    });
                    state.open_loss_segment = Some(sequence);
                    state.next_admission = state.next_admission.wrapping_add(1);
                    self.wake.notify_all();
                    self.notify.notify_one();
                    return true;
                }
            } else if state.control.len() < RUNTIME_BROKER_CONTROL_CAPACITY {
                let event = event.take().expect("control broker event");
                state
                    .control
                    .push_back(RuntimeEventEntry { sequence, event });
                state.open_loss_segment = None;
                state.next_admission = state.next_admission.wrapping_add(1);
                self.wake.notify_all();
                self.notify.notify_one();
                return true;
            }
            if cancel.load(Ordering::Acquire) {
                event.take();
                state.next_admission = state.next_admission.wrapping_add(1);
                self.wake.notify_all();
                return false;
            }
            #[cfg(test)]
            {
                let hook = self.backpressure_hook.lock().take();
                if let Some(hook) = hook {
                    hook();
                }
            }
            // This is finite broker backpressure, not an unbounded receive
            // queue.  Stop wakes this wait through wake_runtime_subscribers.
            self.wake.wait(&mut state);
        }
    }

    pub(super) fn pop(&self) -> Option<BackendEventEnvelope> {
        let mut state = self.state.lock();
        let event = state.pop_next();
        if event.is_some() {
            self.wake.notify_all();
        }
        event
    }

    async fn recv(&self) -> Option<BackendEventEnvelope> {
        loop {
            if let Some(event) = self.pop() {
                return Some(event);
            }
            let notified = self.notify.notified();
            if let Some(event) = self.pop() {
                return Some(event);
            }
            notified.await;
        }
    }

    pub(super) fn wake_all(&self) {
        self.wake.notify_all();
        self.notify.notify_waiters();
    }

    #[cfg(test)]
    pub(super) fn queued_count(&self) -> usize {
        let state = self.state.lock();
        state.ordinary.len()
            + state.control.len()
            + state.overflow.len()
            + usize::from(state.terminal.is_some())
    }
}

impl RuntimeEventReceiver {
    pub async fn recv(&mut self) -> Option<BackendEventEnvelope> {
        self.queue.recv().await
    }

    pub fn try_recv(
        &mut self,
    ) -> Result<BackendEventEnvelope, tokio::sync::mpsc::error::TryRecvError> {
        self.queue
            .pop()
            .ok_or(tokio::sync::mpsc::error::TryRecvError::Empty)
    }

    #[cfg(test)]
    pub(super) fn queued_count(&self) -> usize {
        self.queue.queued_count()
    }
}

impl RuntimeEventQueueState {
    fn record_runtime_overflow(&mut self, kind: BackendEventKind) {
        if let Some(overflow) = self.overflow.back_mut() {
            overflow.dropped_count = overflow.dropped_count.saturating_add(1);
            if !overflow.dropped_kinds.contains(&kind) {
                overflow.dropped_kinds.push(kind);
            }
        }
    }

    fn pop_next(&mut self) -> Option<BackendEventEnvelope> {
        let mut candidate: Option<(u64, RuntimeQueueLane)> = None;
        if let Some(entry) = self.ordinary.front() {
            candidate = Some((entry.sequence, RuntimeQueueLane::Ordinary));
        }
        if let Some(entry) = self.control.front() {
            if candidate.is_none_or(|(sequence, _)| entry.sequence < sequence) {
                candidate = Some((entry.sequence, RuntimeQueueLane::Control));
            }
        }
        if let Some(overflow) = self.overflow.front() {
            if candidate.is_none_or(|(sequence, _)| overflow.sequence < sequence) {
                candidate = Some((overflow.sequence, RuntimeQueueLane::Overflow));
            }
        }
        if let Some(entry) = self.terminal.as_ref() {
            if candidate.is_none_or(|(sequence, _)| entry.sequence < sequence) {
                candidate = Some((entry.sequence, RuntimeQueueLane::Terminal));
            }
        }
        match candidate?.1 {
            RuntimeQueueLane::Ordinary => self.ordinary.pop_front().map(|entry| entry.event),
            RuntimeQueueLane::Control => self.control.pop_front().map(|entry| entry.event),
            RuntimeQueueLane::Terminal => self.terminal.take().map(|entry| entry.event),
            RuntimeQueueLane::Overflow => {
                let overflow = self.overflow.pop_front()?;
                if self.open_loss_segment == Some(overflow.sequence) {
                    self.open_loss_segment = None;
                }
                Some(BackendEventEnvelope::from_payload(
                    mineintent_contracts::minecraft::BackendEventMetadata {
                        id: format!("runtime-overflow-{}", overflow.sequence),
                        occurred_at: overflow.template.occurred_at,
                        process_session_id: overflow.template.process_session_id,
                        connection_epoch: overflow.template.connection_epoch,
                        connection_attempt_id: overflow.template.connection_attempt_id,
                        world_id: overflow.template.world_id,
                        dimension: overflow.template.dimension,
                    },
                    overflow.template.source,
                    BackendEventPayload::Overflow(
                        mineintent_contracts::minecraft::BackendOverflowPayload {
                            event_type: mineintent_contracts::minecraft::OverflowType::Overflow,
                            dropped_count: overflow.dropped_count,
                            dropped_kinds: overflow.dropped_kinds,
                        },
                    ),
                ))
            }
        }
    }
}

fn is_runtime_droppable_event(event: &BackendEventEnvelope) -> bool {
    matches!(
        event.kind,
        BackendEventKind::Entity | BackendEventKind::Block | BackendEventKind::Sound
    )
}

fn is_runtime_terminal_event(event: &BackendEventEnvelope) -> bool {
    matches!(
        event.payload,
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::Stopped { .. })
    )
}

pub(super) struct ObservationSubscriber {
    pub(super) id: u64,
    pub(super) epoch: u64,
    pub(super) listener: Arc<dyn ObservationEventListener>,
    pub(super) state: Arc<ObservationSubscriptionState>,
}

pub(super) struct ObservationSubscriptionState {
    status: parking_lot::Mutex<ObservationSubscriptionStatus>,
    quiescent: parking_lot::Condvar,
}

#[derive(Default)]
pub(super) struct ObservationSubscriptionStatus {
    closed: bool,
    pending_callbacks: usize,
    active_callbacks: usize,
}

impl ObservationSubscriptionState {
    pub(super) fn new() -> Self {
        Self {
            status: parking_lot::Mutex::new(ObservationSubscriptionStatus::default()),
            quiescent: parking_lot::Condvar::new(),
        }
    }

    /// Reserve a callback while the registry lock is held. The reservation is
    /// later turned into an active callback outside that lock.
    pub(super) fn reserve_callback(&self) -> bool {
        let mut status = self.status.lock();
        if status.closed {
            return false;
        }
        status.pending_callbacks += 1;
        true
    }

    pub(super) fn start_callback(&self) -> bool {
        let mut status = self.status.lock();
        debug_assert!(status.pending_callbacks > 0);
        status.pending_callbacks = status.pending_callbacks.saturating_sub(1);
        if status.closed {
            self.quiescent.notify_all();
            return false;
        }
        status.active_callbacks += 1;
        true
    }

    pub(super) fn finish_callback(&self) {
        let mut status = self.status.lock();
        debug_assert!(status.active_callbacks > 0);
        status.active_callbacks = status.active_callbacks.saturating_sub(1);
        if status.active_callbacks == 0 && status.pending_callbacks == 0 {
            self.quiescent.notify_all();
        }
    }

    pub(super) fn close(&self) {
        self.status.lock().closed = true;
    }

    pub(super) fn is_closed(&self) -> bool {
        self.status.lock().closed
    }

    pub(super) fn wait_for_quiescence(&self) {
        let own_active_callbacks = current_observation_callback_count(self);
        let mut status = self.status.lock();
        // A pending reservation is deliberately not waited on: after `closed`
        // is set, `start_callback` consumes it and skips the listener. Waiting
        // for that reservation here would deadlock when listener A unsubscribes
        // listener B from the same dispatch pass before B starts.
        while status.active_callbacks > own_active_callbacks {
            self.quiescent.wait(&mut status);
        }
    }
}

pub(super) struct ObservationDelivery {
    pub(super) listener: Arc<dyn ObservationEventListener>,
    pub(super) state: Arc<ObservationSubscriptionState>,
    pub(super) id: u64,
}

thread_local! {
    static OBSERVATION_CALLBACK_STACK: std::cell::RefCell<Vec<usize>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn observation_state_key(state: &ObservationSubscriptionState) -> usize {
    state as *const ObservationSubscriptionState as usize
}

fn current_observation_callback_count(state: &ObservationSubscriptionState) -> usize {
    let key = observation_state_key(state);
    OBSERVATION_CALLBACK_STACK.with(|stack| {
        stack
            .borrow()
            .iter()
            .filter(|current| **current == key)
            .count()
    })
}

/// 一次订阅者回调的租约守卫。
///
/// `start_callback` 取走的那份 active 计数由本守卫的 `Drop` 归还——**不能**留给
/// 调用点手动调 `finish_callback`。实测过一次：把外层的 panic 捕获拿掉之后，
/// 一个 panic 的订阅者会跳过那行手动释放，`active_callbacks` 永不归零，此后任何
/// unsubscribe（含 `Drop`）都永久卡在 `wait_for_quiescence` 的 Condvar 上。
///
/// 租约必须 RAII，才能在正常返回与 unwind 两条路径上都归还。
pub(super) struct ObservationCallbackGuard {
    key: usize,
    state: Arc<ObservationSubscriptionState>,
}

impl ObservationCallbackGuard {
    pub(super) fn enter(state: Arc<ObservationSubscriptionState>) -> Self {
        let key = observation_state_key(state.as_ref());
        OBSERVATION_CALLBACK_STACK.with(|stack| stack.borrow_mut().push(key));
        Self { key, state }
    }
}

impl Drop for ObservationCallbackGuard {
    fn drop(&mut self) {
        OBSERVATION_CALLBACK_STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            debug_assert_eq!(stack.pop(), Some(self.key));
        });
        self.state.finish_callback();
    }
}

impl SharedRuntime {
    /// Construct and enqueue one event without draining. Callers may hold
    /// command admission; subscriber callbacks always run later, lock-free.
    pub(super) fn enqueue_event(&self, source: FactSource, payload: BackendEventPayload) -> bool {
        self.enqueue_event_at(source, payload, now_utc().to_rfc3339())
    }

    pub(super) fn enqueue_event_at(
        &self,
        source: FactSource,
        payload: BackendEventPayload,
        occurred_at: String,
    ) -> bool {
        let kind = payload.kind();
        if matches!(kind, BackendEventKind::Lifecycle) {
            self.lifecycle_revision.fetch_add(1, Ordering::AcqRel);
        }
        let mut dispatch = self.event_dispatch.lock();
        let event = {
            let mut writer = self.writer.lock();
            writer.emit_at(source, payload, occurred_at)
        };
        self.enqueue_dispatch_locked(&mut dispatch, event)
    }

    pub(super) fn enqueue_dispatch_locked(
        &self,
        dispatch: &mut parking_lot::MutexGuard<'_, EventDispatchState>,
        event: BackendEventEnvelope,
    ) -> bool {
        let mut event = Some(event);
        let mut pending = None;
        loop {
            match dispatch.enqueue(&mut event, &mut pending, &self.dispatch_cancelled) {
                RuntimeDispatchAdmission::Accepted(should_drain) => {
                    self.event_dispatch_wake.notify_all();
                    return should_drain;
                }
                RuntimeDispatchAdmission::Cancelled => {
                    self.event_dispatch_wake.notify_all();
                    return false;
                }
                RuntimeDispatchAdmission::Wait => {
                    #[cfg(test)]
                    {
                        let hook = self.event_dispatch_backpressure_hook.lock().take();
                        if let Some(hook) = hook {
                            hook();
                        }
                    }
                    self.event_dispatch_wake.wait(dispatch);
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) fn emit(&self, source: FactSource, payload: BackendEventPayload) {
        let should_drain = self.enqueue_event(source, payload);
        if should_drain {
            self.drain_events();
        }
    }

    /// Normal product/protocol events must linearize their admission check and
    /// queue insertion. Stop takes the same lock, so a losing late event is
    /// discarded before it can appear after `stopped`.
    pub(super) fn emit_if_running(&self, source: FactSource, payload: BackendEventPayload) -> bool {
        let should_drain = {
            let _admission = self.command_admission.lock();
            let Some(should_drain) = self.enqueue_event_if_running_locked(source, payload) else {
                return false;
            };
            should_drain
        };
        if should_drain {
            self.drain_events();
        }
        true
    }

    pub(super) fn enqueue_event_if_running_locked(
        &self,
        source: FactSource,
        payload: BackendEventPayload,
    ) -> Option<bool> {
        if !self.command_execution_allowed_without_lock() {
            return None;
        }
        #[cfg(test)]
        self.invoke_event_admission_hook();
        Some(self.enqueue_event(source, payload))
    }

    pub(super) fn lifecycle_event_allowed_without_lock(&self) -> bool {
        !self.stopping.load(Ordering::Acquire) && !self.stopped_reported.load(Ordering::Acquire)
    }

    /// 排水期间不持有 dispatch 或 observation registry 锁。callback 内重新
    /// emit 只会把事件追加到队尾，由当前 drainer 在本事件后继续处理。
    pub(super) fn drain_events(&self) {
        loop {
            let event = {
                let mut dispatch = self.event_dispatch.lock();
                let Some(event) = dispatch.pop_next() else {
                    dispatch.drainer_active = false;
                    self.event_dispatch_wake.notify_all();
                    return;
                };
                self.event_dispatch_wake.notify_all();
                event
            };
            self.broadcast_event(event);
        }
    }

    pub(super) fn broadcast_event(&self, event: BackendEventEnvelope) {
        // stdout is only the diagnostic process boundary.  The production
        // facade consumes the same FIFO through `subscribe` and explicitly
        // disables this side effect.
        if self.config.emit_stdout {
            match serde_json::to_string(&event) {
                Ok(line) => println!("{line}"),
                Err(error) => eprintln!("事件编码失败：{error}"),
            }
        }
        let subscribers = {
            let mut subscribers = self.subscribers.lock();
            subscribers.retain(|subscriber| Arc::strong_count(subscriber) > 1);
            subscribers.clone()
        };
        for subscriber in subscribers {
            let _ = subscriber.publish(event.clone(), &self.dispatch_cancelled);
        }
        #[cfg(test)]
        self.invoke_event_broadcast_hook();

        let observation_kind = matches!(
            event.kind,
            BackendEventKind::Entity | BackendEventKind::Block | BackendEventKind::Sound
        );
        if !observation_kind {
            return;
        }

        let Some(observation_event) = observation_event_from_backend(&event) else {
            return;
        };
        let deliveries = {
            let subscribers = self.observation_subscribers.lock();
            subscribers
                .iter()
                .filter(|subscriber| subscriber.epoch == event_epoch(&observation_event))
                .filter(|&subscriber| subscriber.state.reserve_callback())
                .map(|subscriber| ObservationDelivery {
                    listener: subscriber.listener.clone(),
                    state: subscriber.state.clone(),
                    id: subscriber.id,
                })
                .collect::<Vec<_>>()
        };

        for delivery in deliveries {
            if !delivery.state.start_callback() {
                continue;
            }
            // 租约由守卫的 Drop 归还，正常返回与 unwind 两条路径都走它。
            let _callback_guard = ObservationCallbackGuard::enter(delivery.state.clone());
            // 实验分支：不捕获。panic 照常传播，让崩溃现场自己说话。
            delivery.listener.on_event(observation_event.clone());
        }
    }

    /// Register a listener for one immutable connection epoch. Delivery
    /// rechecks that epoch before acquiring its callback lease.
    pub(super) fn add_observation_subscription(
        &self,
        epoch: u64,
        listener: Arc<dyn ObservationEventListener>,
    ) -> (u64, Arc<ObservationSubscriptionState>) {
        let id = self
            .next_observation_subscription_id
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        let state = Arc::new(ObservationSubscriptionState::new());
        self.observation_subscribers
            .lock()
            .push(ObservationSubscriber {
                id,
                epoch,
                listener,
                state: state.clone(),
            });
        (id, state)
    }

    pub(super) fn remove_observation_subscription(
        &self,
        id: u64,
        state: &ObservationSubscriptionState,
    ) {
        {
            let mut subscribers = self.observation_subscribers.lock();
            subscribers.retain(|subscriber| subscriber.id != id);
            state.close();
        }
        state.wait_for_quiescence();
    }
}

/// Owned typed subscription backed by the runtime's synchronous FIFO drainer.
/// Closing first removes the listener from the registry, then waits for any
/// callback that already acquired a lease to finish.
pub struct RuntimeObservationSubscription {
    pub(super) shared: Arc<SharedRuntime>,
    pub(super) id: u64,
    pub(super) state: Arc<ObservationSubscriptionState>,
    pub(super) closed: bool,
}

impl RuntimeObservationSubscription {
    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.shared
            .remove_observation_subscription(self.id, &self.state);
    }

    fn closed(&self) -> bool {
        self.closed || self.state.is_closed()
    }
}

impl Subscription for RuntimeObservationSubscription {
    fn unsubscribe(&mut self) {
        self.close();
    }

    fn is_closed(&self) -> bool {
        self.closed()
    }
}

impl Drop for RuntimeObservationSubscription {
    fn drop(&mut self) {
        self.close();
    }
}

fn event_epoch(event: &ObservationEvent) -> u64 {
    match event {
        ObservationEvent::Entity(event) => event.connection_epoch,
        ObservationEvent::Block(event) => event.connection_epoch,
        ObservationEvent::Sound(event) => event.connection_epoch,
    }
}
