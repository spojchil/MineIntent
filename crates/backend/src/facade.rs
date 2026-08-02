//! Production in-process facade for the frozen Minecraft backend contracts.
//!
//! The facade owns the runtime thread and exposes only the contract traits to
//! the composition root.  Runtime events are first consumed by that owner and
//! then pass through one bounded FIFO dispatcher; subscriptions never get an
//! independent unbounded queue or a callback thread of their own.

use std::{
    future::pending,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Weak,
    },
    thread::{self, JoinHandle, ThreadId},
    time::{Duration, Instant},
};

use mineintent_contracts::minecraft::{
    BackendError, BackendEventEnvelope, BackendEventListener, BackendEventPayload, BackendFailure,
    BackendFailureCode, BackendLifecyclePayload, BackendReady, BackendState, BlockPosition,
    BlockReadResult, BoxFuture, DirectedViewportError, DirectedViewportProjection, GameMode,
    LookRelativeRequest, MinecraftBackendApi, MinecraftBackendConfig, MinecraftMotorDriverApi,
    MinecraftSnapshotV1, MoveInputRequest, ObservationEventListener, OperationControl,
    ProtocolEntitySnapshot, ProtocolObservationSource, SelfPose, Subscription, ViewportRead,
};
use tokio::{runtime::Builder, sync::Notify, task::LocalSet};

#[cfg(test)]
use tokio::sync::mpsc as tokio_mpsc;

#[cfg(test)]
use mineintent_contracts::minecraft::FactSource;

use crate::{
    protocol::{BackendEventEnvelope as RuntimeEventEnvelope, MotorDirection},
    runtime::{
        run_with_handle, CommandCompletion, RunConfig, RuntimeHandle, RuntimeObservationSource,
    },
    snapshot as runtime_snapshot,
};

const DISPATCH_CAPACITY: usize = 256;
const UNSUBSCRIBE_WAIT: Duration = Duration::from_secs(2);

/// The public production facade.  Construction validates and normalizes the
/// frozen config; `start` owns the actual runtime attempt.
#[derive(Clone)]
pub struct MinecraftBackendFacade {
    inner: Arc<FacadeInner>,
}

impl MinecraftBackendFacade {
    pub fn new(config: MinecraftBackendConfig) -> Result<Self, BackendError> {
        let config = config.validate_and_normalize()?;
        Ok(Self {
            inner: FacadeInner::new(config, false)?,
        })
    }

    fn ensure_ready(&self, operation: &str) -> Result<Arc<RuntimeSession>, BackendError> {
        self.inner.ensure_ready(operation)
    }
}

impl MinecraftBackendApi for MinecraftBackendFacade {
    fn start(
        &self,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<BackendReady, BackendError>> {
        let admission = self.inner.admit_start();
        Box::pin(async move {
            let session = admission?;
            wait_for_start(session, control).await
        })
    }

    fn stop(
        &self,
        reason: String,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        let admission = self.inner.admit_stop(&reason);
        Box::pin(async move {
            let session = admission?;
            wait_for_stop(session, control).await
        })
    }

    fn state(&self) -> BackendState {
        self.inner.state()
    }

    fn snapshot(&self) -> Result<MinecraftSnapshotV1, BackendError> {
        let session = self.ensure_ready("snapshot")?;
        #[cfg(test)]
        if let Some(snapshot) = session.scripted_snapshot.lock().clone() {
            return Ok(snapshot);
        }
        let raw = session
            .handle
            .snapshot()
            .ok_or_else(|| BackendError::NotReady {
                state: "snapshot_unavailable".to_owned(),
            })?;
        convert_snapshot(raw)
    }

    fn subscribe(
        &self,
        listener: Arc<dyn BackendEventListener>,
    ) -> Result<Box<dyn Subscription>, BackendError> {
        self.inner.subscribe(listener)
    }

    fn observation_source(&self) -> Result<Arc<dyn ProtocolObservationSource>, BackendError> {
        let session = self.ensure_ready("observation_source")?;
        let source = session.handle.observation_source();
        let bound_epoch = source.epoch();
        let facade_source = FacadeObservationSource {
            inner: Arc::downgrade(&self.inner),
            session,
            source,
            bound_epoch,
        };
        Ok(Arc::new(facade_source))
    }

    fn motor(&self) -> Result<Arc<dyn MinecraftMotorDriverApi>, BackendError> {
        let session = self.ensure_ready("motor")?;
        Ok(Arc::new(FacadeMotor {
            inner: Arc::downgrade(&self.inner),
            bound_epoch: session.handle.connection_epoch(),
            session,
        }))
    }

    fn send_chat(&self, message: String) -> Result<(), BackendError> {
        let session = self.ensure_ready("send_chat")?;
        if message.is_empty() || message.contains(['\r', '\n', '\0']) {
            return Err(BackendError::InvalidCommand {
                field: "message".to_owned(),
                message: "must be non-empty single-line text".to_owned(),
            });
        }
        session
            .handle
            .send_chat(message)
            .map_err(|error| map_runtime_command_error(&session, "send_chat", error))
    }
}

struct FacadeInner {
    config: MinecraftBackendConfig,
    session: parking_lot::Mutex<Option<Arc<RuntimeSession>>>,
    next_session_id: AtomicU64,
    registry: parking_lot::Mutex<Vec<ListenerEntry>>,
    next_subscription_id: AtomicU64,
    dispatch_tx: SyncSender<DispatchMessage>,
    dispatch_normal_queued: parking_lot::Mutex<usize>,
    dispatch_capacity_cv: parking_lot::Condvar,
    dispatcher_join: parking_lot::Mutex<Option<JoinHandle<()>>>,
    closed: AtomicBool,
    #[cfg(test)]
    scripted: bool,
}

struct ListenerEntry {
    id: u64,
    listener: Arc<dyn BackendEventListener>,
    state: Arc<ListenerState>,
}

enum DispatchMessage {
    Event {
        session_id: u64,
        event: BackendEventEnvelope,
    },
    Shutdown,
}

impl FacadeInner {
    fn new(
        config: MinecraftBackendConfig,
        #[cfg(test)] scripted: bool,
        #[cfg(not(test))] _scripted: bool,
    ) -> Result<Arc<Self>, BackendError> {
        // Keep one bounded emergency slot for the single owned session's
        // terminal Stopped envelope. Ordinary events are still limited to
        // DISPATCH_CAPACITY by dispatch_normal_queued below.
        let (dispatch_tx, dispatch_rx) = mpsc::sync_channel(DISPATCH_CAPACITY + 1);
        let inner = Arc::new(Self {
            config,
            session: parking_lot::Mutex::new(None),
            next_session_id: AtomicU64::new(0),
            registry: parking_lot::Mutex::new(Vec::new()),
            next_subscription_id: AtomicU64::new(0),
            dispatch_tx,
            dispatch_normal_queued: parking_lot::Mutex::new(0),
            dispatch_capacity_cv: parking_lot::Condvar::new(),
            dispatcher_join: parking_lot::Mutex::new(None),
            closed: AtomicBool::new(false),
            #[cfg(test)]
            scripted,
        });
        let weak = Arc::downgrade(&inner);
        let join = thread::Builder::new()
            .name("mineintent-backend-dispatcher".to_owned())
            .spawn(move || dispatcher_loop(weak, dispatch_rx))
            .map_err(|error| thread_failure("dispatcher", error))?;
        *inner.dispatcher_join.lock() = Some(join);
        Ok(inner)
    }

    fn state(&self) -> BackendState {
        self.session
            .lock()
            .as_ref()
            .map(|session| session.handle.state())
            .unwrap_or(BackendState::Idle)
    }

    fn ensure_ready(&self, operation: &str) -> Result<Arc<RuntimeSession>, BackendError> {
        let session = self
            .session
            .lock()
            .clone()
            .ok_or_else(|| BackendError::NotReady {
                state: "idle".to_owned(),
            })?;
        let state = session.handle.state();
        if !matches!(state, BackendState::Ready { .. }) {
            return Err(session_error_for(&session, operation));
        }
        if !session.has_snapshot() {
            return Err(BackendError::NotReady {
                state: "snapshot_unavailable".to_owned(),
            });
        }
        Ok(session)
    }

    fn admit_start(self: &Arc<Self>) -> Result<Arc<RuntimeSession>, BackendError> {
        let session = {
            let mut current = self.session.lock();
            if current.is_some() {
                return Err(BackendError::InvalidCommand {
                    field: "lifecycle".to_owned(),
                    message: "start already has an owned attempt".to_owned(),
                });
            }
            let id = self.next_session_id.fetch_add(1, Ordering::AcqRel) + 1;
            let session = RuntimeSession::new(id, run_config(&self.config), self.is_scripted());
            *current = Some(session.clone());
            session
        };
        if let Err(error) = session.launch(self) {
            session.record_terminal(error.clone());
            return Err(error);
        }
        Ok(session)
    }

    fn admit_stop(self: &Arc<Self>, reason: &str) -> Result<Arc<RuntimeSession>, BackendError> {
        let (session, created_idle_session) = {
            let mut current = self.session.lock();
            if let Some(session) = current.as_ref() {
                (session.clone(), false)
            } else {
                let id = self.next_session_id.fetch_add(1, Ordering::AcqRel) + 1;
                let session = RuntimeSession::new(id, run_config(&self.config), self.is_scripted());
                *current = Some(session.clone());
                (session, true)
            }
        };
        if created_idle_session {
            session.stop_idle_without_start(self, reason);
            return Ok(session);
        }
        if session.worker.lock().is_none() && !session.worker_done.load(Ordering::Acquire) {
            if let Err(error) = session.launch(self) {
                session.record_terminal(error.clone());
                return Err(error);
            }
        }
        session.request_stop(self, reason);
        Ok(session)
    }

    fn current_session(&self) -> Option<Arc<RuntimeSession>> {
        self.session.lock().clone()
    }

    fn is_scripted(&self) -> bool {
        #[cfg(test)]
        {
            self.scripted
        }
        #[cfg(not(test))]
        {
            false
        }
    }

    fn route_event(&self, session_id: u64, event: BackendEventEnvelope) {
        let stopped = is_stopped_event(&event);
        if !stopped && !self.reserve_normal_dispatch_slot() {
            return;
        }
        let send_result = self
            .dispatch_tx
            .send(DispatchMessage::Event { session_id, event });
        if send_result.is_err() {
            if !stopped {
                self.release_normal_dispatch_slot();
            }
            if let Some(session) = self.current_session().filter(|s| s.id == session_id) {
                session.record_terminal(thread_failure_message("dispatcher is closed"));
            }
        }
    }

    fn reserve_normal_dispatch_slot(&self) -> bool {
        let mut queued = self.dispatch_normal_queued.lock();
        while *queued >= DISPATCH_CAPACITY && !self.closed.load(Ordering::Acquire) {
            self.dispatch_capacity_cv.wait(&mut queued);
        }
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        *queued += 1;
        true
    }

    fn release_normal_dispatch_slot(&self) {
        let mut queued = self.dispatch_normal_queued.lock();
        *queued = queued.saturating_sub(1);
        self.dispatch_capacity_cv.notify_one();
    }

    fn handle_event(&self, session_id: u64, event: BackendEventEnvelope) {
        let Some(_session) = self.current_session().filter(|s| s.id == session_id) else {
            return;
        };
        let deliveries = {
            let registry = self.registry.lock();
            registry
                .iter()
                .filter_map(|entry| {
                    entry.state.reserve().then(|| ListenerDelivery {
                        id: entry.id,
                        listener: entry.listener.clone(),
                        state: entry.state.clone(),
                    })
                })
                .collect::<Vec<_>>()
        };
        for delivery in deliveries {
            if !delivery.state.begin() {
                continue;
            }
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                delivery.listener.on_event(event.clone());
            }));
            delivery.state.finish();
            if result.is_err() {
                eprintln!(
                    "backend facade listener panic isolated: subscription_id={}",
                    delivery.id
                );
            }
        }
    }

    fn subscribe(
        self: &Arc<Self>,
        listener: Arc<dyn BackendEventListener>,
    ) -> Result<Box<dyn Subscription>, BackendError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(BackendError::SubscriptionClosed);
        }
        let id = self.next_subscription_id.fetch_add(1, Ordering::AcqRel) + 1;
        let state = Arc::new(ListenerState::new());
        self.registry.lock().push(ListenerEntry {
            id,
            listener,
            state: state.clone(),
        });
        Ok(Box::new(FacadeSubscription {
            inner: Arc::downgrade(self),
            id,
            state,
            closed: false,
        }))
    }

    fn remove_subscription(&self, id: u64, state: &Arc<ListenerState>) {
        {
            let mut registry = self.registry.lock();
            state.close();
            registry.retain(|entry| entry.id != id);
        }
        state.wait_quiescent();
    }

    #[cfg(test)]
    fn scripted_driver(self: &Arc<Self>) -> ScriptedDriver {
        ScriptedDriver {
            inner: self.clone(),
        }
    }
}

impl Drop for FacadeInner {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        self.dispatch_capacity_cv.notify_all();
        if let Some(session) = self.session.get_mut().take() {
            session.request_stop_without_inner("facade_dropped");
            session.join_worker_blocking();
        }
        let _ = self.dispatch_tx.send(DispatchMessage::Shutdown);
        if let Some(join) = self.dispatcher_join.get_mut().take() {
            if join.thread().id() != thread::current().id() {
                let _ = join.join();
            }
        }
    }
}

struct RuntimeSession {
    id: u64,
    handle: RuntimeHandle,
    run_config: RunConfig,
    outcome: parking_lot::Mutex<SessionOutcome>,
    notify: Notify,
    worker: parking_lot::Mutex<Option<JoinHandle<()>>>,
    worker_done: AtomicBool,
    joined: AtomicBool,
    joined_notify: Notify,
    worker_thread: parking_lot::Mutex<Option<ThreadId>>,
    #[cfg(test)]
    scripted_receiver:
        parking_lot::Mutex<Option<tokio_mpsc::UnboundedReceiver<RuntimeEventEnvelope>>>,
    #[cfg(test)]
    scripted_snapshot: parking_lot::Mutex<Option<MinecraftSnapshotV1>>,
}

#[derive(Default)]
struct SessionOutcome {
    ready: Option<Result<BackendReady, BackendError>>,
    terminal: Option<BackendError>,
}

impl RuntimeSession {
    fn new(id: u64, run_config: RunConfig, scripted: bool) -> Arc<Self> {
        let handle = RuntimeHandle::new(run_config.clone());
        #[cfg(test)]
        let scripted_receiver = scripted.then(|| handle.subscribe());
        Arc::new(Self {
            id,
            handle,
            run_config,
            outcome: parking_lot::Mutex::new(SessionOutcome::default()),
            notify: Notify::new(),
            worker: parking_lot::Mutex::new(None),
            worker_done: AtomicBool::new(scripted),
            joined: AtomicBool::new(scripted),
            joined_notify: Notify::new(),
            worker_thread: parking_lot::Mutex::new(None),
            #[cfg(test)]
            scripted_receiver: parking_lot::Mutex::new(scripted_receiver),
            #[cfg(test)]
            scripted_snapshot: parking_lot::Mutex::new(None),
        })
    }

    fn has_snapshot(&self) -> bool {
        if self.handle.snapshot().is_some() {
            return true;
        }
        #[cfg(test)]
        {
            return self.scripted_snapshot.lock().is_some();
        }
        #[cfg(not(test))]
        {
            false
        }
    }

    fn launch(self: &Arc<Self>, inner: &Arc<FacadeInner>) -> Result<(), BackendError> {
        if inner.is_scripted() {
            return Ok(());
        }
        let session = self.clone();
        let weak = Arc::downgrade(inner);
        let config = self.run_config.clone();
        let join = thread::Builder::new()
            .name(format!("mineintent-backend-runtime-{}", self.id))
            .spawn(move || {
                *session.worker_thread.lock() = Some(thread::current().id());
                let result = Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| thread_failure("runtime", error))
                    .and_then(|runtime| {
                        let local = LocalSet::new();
                        local.block_on(&runtime, runtime_worker(session.clone(), weak, config));
                        Ok(())
                    });
                if let Err(error) = result {
                    session.record_terminal(error);
                }
                session.mark_worker_done();
            })
            .map_err(|error| thread_failure("runtime", error))?;
        *self.worker.lock() = Some(join);
        Ok(())
    }

    fn request_stop(&self, inner: &FacadeInner, reason: &str) {
        self.request_stop_without_inner(reason);
        #[cfg(test)]
        if inner.is_scripted() {
            self.drain_scripted_events(inner);
        }
        #[cfg(not(test))]
        let _ = inner;
    }

    fn request_stop_without_inner(&self, reason: &str) {
        self.handle.stop(reason);
    }

    /// Idle stop is an admission-only transition. It uses the concrete
    /// RuntimeHandle to publish the strict `stopped` fact, but deliberately
    /// does not create the runtime worker or call `run_with_handle`.
    fn stop_idle_without_start(&self, inner: &FacadeInner, reason: &str) {
        let mut events = self.handle.subscribe();
        self.handle.stop(reason);
        self.worker_done.store(true, Ordering::Release);
        self.joined.store(true, Ordering::Release);
        while let Ok(event) = events.try_recv() {
            self.observe_event(&event);
            inner.route_event(self.id, event);
        }
        self.notify.notify_waiters();
    }

    fn mark_worker_done(&self) {
        self.worker_done.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    fn mark_joined(&self) {
        self.joined.store(true, Ordering::Release);
        self.joined_notify.notify_waiters();
        self.notify.notify_waiters();
    }

    fn join_worker_blocking(&self) {
        if self.joined.load(Ordering::Acquire) {
            return;
        }
        let join = self.worker.lock().take();
        if let Some(join) = join {
            if join.thread().id() == thread::current().id() {
                return;
            }
            let _ = join.join();
            self.mark_joined();
        } else if self.worker_done.load(Ordering::Acquire) {
            self.mark_joined();
        }
    }

    async fn join_worker(&self) -> Result<(), BackendError> {
        if self.joined.load(Ordering::Acquire) {
            return Ok(());
        }
        let join = self.worker.lock().take();
        if let Some(join) = join {
            if join.thread().id() == thread::current().id() {
                return Err(BackendError::BackendFailure {
                    failure: BackendFailure {
                        code: BackendFailureCode::ProtocolError,
                        message: "runtime worker attempted to join itself".to_owned(),
                        retryable: false,
                    },
                });
            }
            tokio::task::spawn_blocking(move || join.join())
                .await
                .map_err(|error| BackendError::BackendFailure {
                    failure: BackendFailure {
                        code: BackendFailureCode::ProtocolError,
                        message: format!("runtime worker join task failed: {error}"),
                        retryable: false,
                    },
                })?
                .map_err(|_| BackendError::BackendFailure {
                    failure: BackendFailure {
                        code: BackendFailureCode::ProtocolError,
                        message: "runtime worker panicked while joining".to_owned(),
                        retryable: false,
                    },
                })?;
            self.mark_joined();
            Ok(())
        } else {
            while !self.joined.load(Ordering::Acquire) {
                self.joined_notify.notified().await;
            }
            Ok(())
        }
    }

    fn observe_event(&self, event: &BackendEventEnvelope) {
        match &event.payload {
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Ready { .. }) => {
                let result = self.ready_from_event(event);
                let mut outcome = self.outcome.lock();
                if outcome.ready.is_none() {
                    if let Err(error) = &result {
                        outcome.terminal = Some(error.clone());
                    }
                    outcome.ready = Some(result);
                }
            }
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Faulted { failure }) => {
                self.record_terminal(BackendError::BackendFailure {
                    failure: failure.clone(),
                });
            }
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionClosed { close })
                if !close.retryable =>
            {
                self.record_terminal(BackendError::BackendClosed {
                    close: close.clone(),
                });
            }
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Stopped { .. }) => {
                let mut outcome = self.outcome.lock();
                if outcome.ready.is_none() && outcome.terminal.is_none() {
                    outcome.terminal = Some(BackendError::NotReady {
                        state: "stopped".to_owned(),
                    });
                }
            }
            _ => {}
        }
        self.notify.notify_waiters();
    }

    fn ready_from_event(&self, event: &BackendEventEnvelope) -> Result<BackendReady, BackendError> {
        let raw = {
            #[cfg(test)]
            if let Some(snapshot) = self.scripted_snapshot.lock().clone() {
                return ready_from_contract_snapshot(event, snapshot);
            }
            self.handle
                .snapshot()
                .ok_or_else(|| BackendError::BackendFailure {
                    failure: BackendFailure {
                        code: BackendFailureCode::ProtocolError,
                        message: "ready event has no runtime snapshot".to_owned(),
                        retryable: false,
                    },
                })?
        };
        let snapshot = convert_snapshot(raw)?;
        if snapshot.process_session_id != event.process_session_id
            || snapshot.connection_epoch != event.connection_epoch
            || snapshot.connection_attempt_id != event.connection_attempt_id
        {
            return Err(BackendError::BackendFailure {
                failure: BackendFailure {
                    code: BackendFailureCode::ProtocolError,
                    message: "ready snapshot identity does not match the ready event".to_owned(),
                    retryable: false,
                },
            });
        }
        Ok(BackendReady {
            process_session_id: event.process_session_id.clone(),
            connection_epoch: event.connection_epoch,
            connection_attempt_id: event.connection_attempt_id.clone(),
            snapshot,
        })
    }

    fn record_terminal(&self, error: BackendError) {
        let mut outcome = self.outcome.lock();
        if outcome.terminal.is_none() {
            outcome.terminal = Some(error);
        }
        self.notify.notify_waiters();
    }

    fn start_result(&self) -> Option<Result<BackendReady, BackendError>> {
        let outcome = self.outcome.lock();
        outcome
            .ready
            .clone()
            .or_else(|| outcome.terminal.clone().map(Err))
    }

    /// Stop completion is owned by the runtime admission state and worker
    /// join.  The envelope remains a separately queued FIFO fact; requiring
    /// `observe_event` to consume that envelope would deadlock a listener that
    /// synchronously waits for stop from inside the dispatcher callback.
    fn stop_resources_released(&self) -> bool {
        matches!(self.handle.state(), BackendState::Stopped { .. })
            && self.worker_done.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn set_scripted_snapshot(&self, snapshot: MinecraftSnapshotV1) {
        *self.scripted_snapshot.lock() = Some(snapshot);
    }

    #[cfg(test)]
    fn drain_scripted_events(&self, inner: &FacadeInner) {
        let mut events = Vec::new();
        if let Some(receiver) = self.scripted_receiver.lock().as_mut() {
            while let Ok(event) = receiver.try_recv() {
                events.push(event);
            }
        }
        for event in events {
            if is_ready_event(&event) {
                if let Some(snapshot) = self.scripted_snapshot.lock().as_mut() {
                    snapshot.process_session_id = event.process_session_id.clone();
                    snapshot.connection_epoch = event.connection_epoch;
                    snapshot.connection_attempt_id = event.connection_attempt_id.clone();
                }
            }
            self.observe_event(&event);
            inner.route_event(self.id, event);
        }
    }
}

async fn runtime_worker(session: Arc<RuntimeSession>, inner: Weak<FacadeInner>, config: RunConfig) {
    let mut events = session.handle.subscribe();
    let mut run = tokio::task::spawn_local(run_with_handle(session.handle.clone(), config));
    let mut run_finished = false;
    let mut run_failed = false;
    loop {
        if run_finished {
            let Some(event) = events.recv().await else {
                break;
            };
            let stopped = is_stopped_event(&event);
            session.observe_event(&event);
            if let Some(inner) = inner.upgrade() {
                inner.route_event(session.id, event);
            }
            if stopped {
                break;
            }
            continue;
        }
        tokio::select! {
            result = &mut run => {
                run_finished = true;
                match result {
                    Err(error) => {
                        run_failed = true;
                        session.record_terminal(BackendError::BackendFailure {
                            failure: BackendFailure {
                                code: BackendFailureCode::ProtocolError,
                                message: format!("runtime task failed: {error}"),
                                retryable: false,
                            },
                        });
                    }
                    Ok(Err(error)) => {
                        run_failed = true;
                        session.record_terminal(BackendError::BackendFailure {
                            failure: BackendFailure {
                                code: BackendFailureCode::ProtocolError,
                                message: format!("runtime exited with error: {error}"),
                                retryable: false,
                            },
                        });
                    }
                    Ok(Ok(())) if session.handle.state() == BackendState::Idle => {
                        run_failed = true;
                        session.record_terminal(BackendError::BackendFailure {
                            failure: BackendFailure {
                                code: BackendFailureCode::ProtocolError,
                                message: "runtime exited before a terminal lifecycle event".to_owned(),
                                retryable: false,
                            },
                        });
                    }
                    Ok(Ok(())) => {}
                }
                if run_failed {
                    break;
                }
            }
            event = events.recv() => {
                let Some(event) = event else { break; };
                session.observe_event(&event);
                if let Some(inner) = inner.upgrade() {
                    inner.route_event(session.id, event);
                }
            }
        }
    }
}

fn dispatcher_loop(weak: Weak<FacadeInner>, receiver: Receiver<DispatchMessage>) {
    while let Ok(message) = receiver.recv() {
        match message {
            DispatchMessage::Event { session_id, event } => {
                let Some(inner) = weak.upgrade() else {
                    continue;
                };
                if !is_stopped_event(&event) {
                    inner.release_normal_dispatch_slot();
                }
                inner.handle_event(session_id, event);
            }
            DispatchMessage::Shutdown => break,
        }
    }
}

struct ListenerDelivery {
    id: u64,
    listener: Arc<dyn BackendEventListener>,
    state: Arc<ListenerState>,
}

struct ListenerState {
    status: parking_lot::Mutex<ListenerStatus>,
    quiescent: parking_lot::Condvar,
}

struct ListenerStatus {
    closed: bool,
    pending: usize,
    active: usize,
    active_thread: Option<ThreadId>,
}

impl ListenerState {
    fn new() -> Self {
        Self {
            status: parking_lot::Mutex::new(ListenerStatus {
                closed: false,
                pending: 0,
                active: 0,
                active_thread: None,
            }),
            quiescent: parking_lot::Condvar::new(),
        }
    }

    fn reserve(&self) -> bool {
        let mut status = self.status.lock();
        if status.closed {
            return false;
        }
        status.pending += 1;
        true
    }

    fn begin(&self) -> bool {
        let mut status = self.status.lock();
        status.pending = status.pending.saturating_sub(1);
        if status.closed {
            self.quiescent.notify_all();
            return false;
        }
        status.active += 1;
        status.active_thread = Some(thread::current().id());
        true
    }

    fn finish(&self) {
        let mut status = self.status.lock();
        status.active = status.active.saturating_sub(1);
        if status.active == 0 {
            status.active_thread = None;
            self.quiescent.notify_all();
        }
    }

    fn close(&self) {
        self.status.lock().closed = true;
        self.quiescent.notify_all();
    }

    fn is_closed(&self) -> bool {
        self.status.lock().closed
    }

    fn wait_quiescent(&self) {
        let current = thread::current().id();
        let deadline = Instant::now() + UNSUBSCRIBE_WAIT;
        let mut status = self.status.lock();
        while status.active > 0 && status.active_thread != Some(current) {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            self.quiescent.wait_for(&mut status, deadline - now);
        }
    }
}

struct FacadeSubscription {
    inner: Weak<FacadeInner>,
    id: u64,
    state: Arc<ListenerState>,
    closed: bool,
}

impl FacadeSubscription {
    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Some(inner) = self.inner.upgrade() {
            inner.remove_subscription(self.id, &self.state);
        } else {
            self.state.close();
            self.state.wait_quiescent();
        }
    }
}

impl Subscription for FacadeSubscription {
    fn unsubscribe(&mut self) {
        self.close();
    }

    fn is_closed(&self) -> bool {
        self.closed || self.state.is_closed()
    }
}

impl Drop for FacadeSubscription {
    fn drop(&mut self) {
        self.close();
    }
}

async fn wait_for_start(
    session: Arc<RuntimeSession>,
    control: OperationControl,
) -> Result<BackendReady, BackendError> {
    loop {
        if let Err(error) = control.preflight("start") {
            session.request_stop_without_inner("start_controlled_stop");
            return Err(error);
        }
        if let Some(result) = session.start_result() {
            return result;
        }
        let cancellation = control.cancelled();
        let deadline = deadline_future(&control);
        tokio::pin!(cancellation);
        tokio::pin!(deadline);
        tokio::select! {
            biased;
            _ = &mut cancellation => {
                session.request_stop_without_inner("start_cancelled");
                return Err(BackendError::Cancelled { operation: "start".to_owned() });
            }
            _ = &mut deadline => {
                session.request_stop_without_inner("start_deadline");
                return Err(BackendError::DeadlineExceeded { operation: "start".to_owned() });
            }
            _ = session.notify.notified() => {}
        }
    }
}

async fn wait_for_stop(
    session: Arc<RuntimeSession>,
    control: OperationControl,
) -> Result<(), BackendError> {
    loop {
        control.preflight("stop")?;
        if session.stop_resources_released() {
            session.join_worker().await?;
            return Ok(());
        }
        let cancellation = control.cancelled();
        let deadline = deadline_future(&control);
        tokio::pin!(cancellation);
        tokio::pin!(deadline);
        tokio::select! {
            biased;
            _ = &mut cancellation => {
                return Err(BackendError::Cancelled { operation: "stop".to_owned() });
            }
            _ = &mut deadline => {
                return Err(BackendError::DeadlineExceeded { operation: "stop".to_owned() });
            }
            _ = session.notify.notified() => {}
        }
    }
}

fn deadline_future(control: &OperationControl) -> impl std::future::Future<Output = ()> + '_ {
    async move {
        if let Some(deadline) = control.deadline_elapsed() {
            deadline.await;
        } else {
            pending::<()>().await;
        }
    }
}

struct FacadeMotor {
    inner: Weak<FacadeInner>,
    session: Arc<RuntimeSession>,
    bound_epoch: u64,
}

impl FacadeMotor {
    fn ensure(&self, operation: &str) -> Result<Arc<FacadeInner>, BackendError> {
        let inner = self
            .inner
            .upgrade()
            .ok_or_else(|| BackendError::BackendFailure {
                failure: BackendFailure {
                    code: BackendFailureCode::ProtocolError,
                    message: "backend facade has been dropped".to_owned(),
                    retryable: false,
                },
            })?;
        inner.ensure_bound_ready(&self.session, self.bound_epoch, operation)?;
        Ok(inner)
    }
}

impl MinecraftMotorDriverApi for FacadeMotor {
    fn look_relative(
        &self,
        request: LookRelativeRequest,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        let validation = request.validate();
        let inner = self.inner.clone();
        let session = self.session.clone();
        let bound_epoch = self.bound_epoch;
        Box::pin(async move {
            validation?;
            let inner = inner.upgrade().ok_or_else(|| dropped_facade_error())?;
            inner.ensure_bound_ready(&session, bound_epoch, "look_relative")?;
            control.preflight("look_relative")?;
            let yaw = request.yaw_degrees as f32;
            let pitch = request.pitch_degrees as f32;
            if !yaw.is_finite() || !pitch.is_finite() {
                return Err(BackendError::InvalidCommand {
                    field: "degrees".to_owned(),
                    message: "cannot be represented by the runtime actuator".to_owned(),
                });
            }
            let completion = session
                .handle
                .look_relative(yaw, pitch)
                .map_err(|error| map_runtime_command_error(&session, "look_relative", error))?;
            await_command(completion, control, "look_relative").await
        })
    }

    fn move_input(
        &self,
        request: MoveInputRequest,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        let validation = request.validate();
        let inner = self.inner.clone();
        let session = self.session.clone();
        let bound_epoch = self.bound_epoch;
        Box::pin(async move {
            validation?;
            let inner = inner.upgrade().ok_or_else(|| dropped_facade_error())?;
            inner.ensure_bound_ready(&session, bound_epoch, "move_input")?;
            control.preflight("move_input")?;
            let directions = request
                .directions
                .iter()
                .copied()
                .map(runtime_direction)
                .collect();
            let completion = session
                .handle
                .move_input(directions, request.duration_ms, request.sprint, None, None)
                .map_err(|error| map_runtime_command_error(&session, "move_input", error))?;
            await_command(completion, control, "move_input").await
        })
    }

    fn release_all(&self) -> Result<(), BackendError> {
        self.ensure("release_all")?;
        let completion = self
            .session
            .handle
            .release_all()
            .map_err(|error| map_runtime_command_error(&self.session, "release_all", error))?;
        completion.wait_blocking()
    }
}

async fn await_command(
    completion: CommandCompletion,
    control: OperationControl,
    operation: &str,
) -> Result<(), BackendError> {
    let cancellation_handle = completion.cancellation_handle();
    if let Err(error) = control.preflight(operation) {
        cancellation_handle.cancel();
        cancellation_handle.wait_settled().await;
        return Err(error);
    }
    let completion_future = completion.wait();
    tokio::pin!(completion_future);
    let cancellation = control.cancelled();
    let deadline = deadline_future(&control);
    tokio::pin!(cancellation);
    tokio::pin!(deadline);
    tokio::select! {
        biased;
        _ = &mut cancellation => {
            cancellation_handle.cancel();
            cancellation_handle.wait_settled().await;
            Err(BackendError::Cancelled { operation: operation.to_owned() })
        }
        _ = &mut deadline => {
            cancellation_handle.cancel();
            cancellation_handle.wait_settled().await;
            Err(BackendError::DeadlineExceeded { operation: operation.to_owned() })
        }
        result = &mut completion_future => result,
    }
}

#[derive(Clone)]
struct FacadeObservationSource {
    inner: Weak<FacadeInner>,
    session: Arc<RuntimeSession>,
    source: RuntimeObservationSource,
    bound_epoch: u64,
}

impl FacadeObservationSource {
    fn ensure(&self, operation: &str) -> Result<(), BackendError> {
        let inner = self.inner.upgrade().ok_or_else(|| dropped_facade_error())?;
        inner.ensure_bound_ready(&self.session, self.bound_epoch, operation)
    }

    fn ensure_source_snapshot(&self, operation: &str) -> Result<(), BackendError> {
        self.ensure(operation)?;
        if !self.session.has_snapshot() {
            return Err(BackendError::NotReady {
                state: "snapshot_unavailable".to_owned(),
            });
        }
        Ok(())
    }
}

impl ProtocolObservationSource for FacadeObservationSource {
    fn epoch(&self) -> u64 {
        self.bound_epoch
    }

    fn self_pose(&self) -> Result<SelfPose, BackendError> {
        self.ensure_source_snapshot("self_pose")?;
        let value = self.source.self_pose()?;
        self.ensure_source_snapshot("self_pose")?;
        Ok(value)
    }

    fn list_tracked_entities(&self) -> Result<Vec<ProtocolEntitySnapshot>, BackendError> {
        self.ensure_source_snapshot("list_tracked_entities")?;
        let value = self.source.list_tracked_entities()?;
        self.ensure_source_snapshot("list_tracked_entities")?;
        Ok(value)
    }

    fn read_block(&self, position: BlockPosition) -> Result<BlockReadResult, BackendError> {
        self.ensure_source_snapshot("read_block")?;
        let value = self.source.read_block(position)?;
        self.ensure_source_snapshot("read_block")?;
        Ok(value)
    }

    fn subscribe(
        &self,
        listener: Arc<dyn ObservationEventListener>,
    ) -> Result<Box<dyn Subscription>, BackendError> {
        self.ensure_source_snapshot("subscribe")?;
        self.source
            .subscribe(listener)
            .map(|subscription| Box::new(ForwardingSubscription::new(subscription)) as _)
    }

    fn read_viewport(
        &self,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<ViewportRead, BackendError>> {
        let source = self.source.clone();
        let this = self.clone();
        Box::pin(async move {
            this.ensure_source_snapshot("read_viewport")?;
            let value = source.read_viewport(control).await?;
            this.ensure_source_snapshot("read_viewport")?;
            Ok(value)
        })
    }

    fn read_directed_viewport(
        &self,
        positions: Vec<BlockPosition>,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<DirectedViewportProjection, DirectedViewportError>> {
        let source = self.source.clone();
        let this = self.clone();
        Box::pin(async move {
            this.ensure_source_snapshot("read_directed_viewport")
                .map_err(DirectedViewportError::Backend)?;
            let value = source.read_directed_viewport(positions, control).await?;
            this.ensure_source_snapshot("read_directed_viewport")
                .map_err(DirectedViewportError::Backend)?;
            Ok(value)
        })
    }
}

struct ForwardingSubscription {
    inner: Option<Box<dyn Subscription>>,
}

impl ForwardingSubscription {
    fn new(inner: Box<dyn Subscription>) -> Self {
        Self { inner: Some(inner) }
    }
}

impl Subscription for ForwardingSubscription {
    fn unsubscribe(&mut self) {
        if let Some(inner) = self.inner.as_mut() {
            inner.unsubscribe();
        }
    }

    fn is_closed(&self) -> bool {
        self.inner.as_ref().is_none_or(|inner| inner.is_closed())
    }
}

impl Drop for ForwardingSubscription {
    fn drop(&mut self) {
        self.unsubscribe();
    }
}

impl FacadeInner {
    fn ensure_bound_ready(
        &self,
        session: &Arc<RuntimeSession>,
        bound_epoch: u64,
        operation: &str,
    ) -> Result<(), BackendError> {
        let current = self.current_session();
        if current
            .as_ref()
            .is_none_or(|current| current.id != session.id)
        {
            return Err(BackendError::StaleEpoch {
                bound_epoch,
                current_epoch: session.handle.connection_epoch(),
            });
        }
        let current_epoch = session.handle.connection_epoch();
        if current_epoch != bound_epoch {
            return Err(BackendError::StaleEpoch {
                bound_epoch,
                current_epoch,
            });
        }
        let state = session.handle.state();
        if !matches!(state, BackendState::Ready { .. }) {
            return Err(session_error_for(session, operation));
        }
        Ok(())
    }
}

fn run_config(config: &MinecraftBackendConfig) -> RunConfig {
    RunConfig {
        host: config.server.host.clone(),
        port: config.server.port,
        username: config.identity.username.clone(),
        world_id: config.world_id.clone(),
        // Production owns the lifecycle; the diagnostic duration must not
        // stop a facade session.  Keep the field valid for the runtime seam.
        duration: Duration::from_secs(86_400),
        reconnect_delay: Duration::from_millis(config.reconnect.initial_delay_ms),
        reconnect_enabled: config.reconnect.enabled,
        auto_stop: false,
        emit_stdout: false,
        initial_chat: None,
    }
}

fn runtime_direction(
    direction: mineintent_contracts::minecraft::MotorMoveDirection,
) -> MotorDirection {
    match direction {
        mineintent_contracts::minecraft::MotorMoveDirection::Forward => MotorDirection::Forward,
        mineintent_contracts::minecraft::MotorMoveDirection::Back => MotorDirection::Back,
        mineintent_contracts::minecraft::MotorMoveDirection::Left => MotorDirection::Left,
        mineintent_contracts::minecraft::MotorMoveDirection::Right => MotorDirection::Right,
    }
}

fn is_stopped_event(event: &RuntimeEventEnvelope) -> bool {
    matches!(
        event.payload,
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::Stopped { .. })
    )
}

#[cfg(test)]
fn is_ready_event(event: &RuntimeEventEnvelope) -> bool {
    matches!(
        event.payload,
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::Ready { .. })
    )
}

fn state_error_for(state: &BackendState, operation: &str) -> BackendError {
    match state {
        BackendState::Faulted { failure } => BackendError::BackendFailure {
            failure: failure.clone(),
        },
        BackendState::Stopped { .. } => BackendError::NotReady {
            state: "stopped".to_owned(),
        },
        _ => BackendError::NotReady {
            state: state_name(state, operation),
        },
    }
}

fn state_name(state: &BackendState, _operation: &str) -> String {
    match state {
        BackendState::Idle => "idle".to_owned(),
        BackendState::Connecting { .. } => "connecting".to_owned(),
        BackendState::LoggingIn { .. } => "logging_in".to_owned(),
        BackendState::Spawning { .. } => "spawning".to_owned(),
        BackendState::Ready { .. } => "ready".to_owned(),
        BackendState::Dead { .. } => "dead".to_owned(),
        BackendState::Reconnecting { .. } => "reconnecting".to_owned(),
        BackendState::Stopping { .. } => "stopping".to_owned(),
        BackendState::Stopped { .. } => "stopped".to_owned(),
        BackendState::Faulted { .. } => "faulted".to_owned(),
    }
}

fn session_error_for(session: &RuntimeSession, operation: &str) -> BackendError {
    let state = session.handle.state();
    if matches!(state, BackendState::Stopped { .. }) {
        if let Some(error) = session.outcome.lock().terminal.clone() {
            return error;
        }
    }
    state_error_for(&state, operation)
}

fn dropped_facade_error() -> BackendError {
    BackendError::BackendFailure {
        failure: BackendFailure {
            code: BackendFailureCode::ProtocolError,
            message: "backend facade has been dropped".to_owned(),
            retryable: false,
        },
    }
}

fn map_runtime_command_error(
    session: &RuntimeSession,
    operation: &str,
    error: String,
) -> BackendError {
    match session.handle.state() {
        BackendState::Faulted { failure } => BackendError::BackendFailure { failure },
        BackendState::Stopped { .. } | BackendState::Stopping { .. } => {
            session_error_for(session, operation)
        }
        _ => BackendError::BackendFailure {
            failure: BackendFailure {
                code: BackendFailureCode::ProtocolError,
                message: format!("{operation}: {error}"),
                retryable: true,
            },
        },
    }
}

fn thread_failure(kind: &str, error: std::io::Error) -> BackendError {
    thread_failure_message(format!("cannot start {kind} thread: {error}"))
}

fn thread_failure_message(message: impl Into<String>) -> BackendError {
    BackendError::BackendFailure {
        failure: BackendFailure {
            code: BackendFailureCode::ProtocolError,
            message: message.into(),
            retryable: false,
        },
    }
}

fn convert_snapshot(
    raw: runtime_snapshot::MinecraftSnapshotV1,
) -> Result<MinecraftSnapshotV1, BackendError> {
    let game_mode = match raw.world.game_mode.as_str() {
        "survival" => GameMode::Survival,
        "creative" => GameMode::Creative,
        "adventure" => GameMode::Adventure,
        "spectator" => GameMode::Spectator,
        other => {
            return Err(snapshot_conversion_error(format!(
                "unknown game mode {other}"
            )))
        }
    };
    let inventory = raw
        .inventory
        .slots
        .into_iter()
        .map(|slot| {
            let count = u32::try_from(slot.count).map_err(|_| {
                snapshot_conversion_error(format!("negative inventory count {}", slot.count))
            })?;
            Ok(mineintent_contracts::minecraft::InventorySlotSnapshot {
                slot: u32::try_from(slot.slot).map_err(|_| {
                    snapshot_conversion_error(format!("inventory slot {} overflows u32", slot.slot))
                })?,
                item_name: slot.item_name,
                count,
                metadata: None,
                durability_used: None,
            })
        })
        .collect::<Result<Vec<_>, BackendError>>()?;
    let tracked_players = raw
        .tracked_players
        .into_iter()
        .map(
            |player| mineintent_contracts::minecraft::TrackedPlayerSnapshot {
                player_key: player.player_key,
                uuid: (!player.uuid.is_empty()).then_some(player.uuid),
                username: player.username,
                listed: player.listed,
                entity_tracked: player.entity_tracked,
                position: player
                    .position
                    .map(|value| mineintent_contracts::minecraft::Vec3Value {
                        x: value.x,
                        y: value.y,
                        z: value.z,
                    }),
                yaw: player.yaw.map(f64::from),
                pitch: player.pitch.map(f64::from),
                held_item_name: None,
            },
        )
        .collect();
    let snapshot = MinecraftSnapshotV1 {
        protocol: mineintent_contracts::minecraft::SnapshotProtocol::V1,
        snapshot_revision: raw.snapshot_revision,
        lifecycle_revision: raw.lifecycle_revision,
        captured_at: raw.captured_at.to_rfc3339(),
        process_session_id: raw.process_session_id,
        connection_epoch: raw.connection_epoch,
        connection_attempt_id: raw.connection_attempt_id,
        world: mineintent_contracts::minecraft::WorldSnapshot {
            world_id: raw.world.world_id,
            dimension: raw.world.dimension,
            minecraft_version: raw.world.minecraft_version,
            protocol_version: raw.world.protocol_version,
            game_mode,
            difficulty: None,
            min_y: raw.world.min_y,
            height: raw.world.height,
            server_view_distance: None,
            time_of_day: None,
            is_raining: None,
        },
        self_snapshot: mineintent_contracts::minecraft::SelfSnapshot {
            entity_key: raw.self_snapshot.entity_key,
            username: raw.self_snapshot.username,
            position: mineintent_contracts::minecraft::Vec3Value {
                x: raw.self_snapshot.position.x,
                y: raw.self_snapshot.position.y,
                z: raw.self_snapshot.position.z,
            },
            velocity: mineintent_contracts::minecraft::Vec3Value {
                x: raw.self_snapshot.velocity.x,
                y: raw.self_snapshot.velocity.y,
                z: raw.self_snapshot.velocity.z,
            },
            yaw: f64::from(raw.self_snapshot.yaw),
            pitch: f64::from(raw.self_snapshot.pitch),
            on_ground: raw.self_snapshot.on_ground,
            alive: raw.self_snapshot.alive,
            health: f64::from(raw.self_snapshot.health),
            food: f64::from(raw.self_snapshot.food),
            food_saturation: f64::from(raw.self_snapshot.food_saturation),
            oxygen: None,
            experience: Some(mineintent_contracts::minecraft::ExperienceSnapshot {
                level: raw.self_snapshot.experience.level,
                progress: f64::from(raw.self_snapshot.experience.progress),
                total: u64::from(raw.self_snapshot.experience.total),
            }),
            effects: Vec::new(),
        },
        inventory: mineintent_contracts::minecraft::InventorySnapshot {
            selected_hotbar_slot: raw.inventory.selected_hotbar_slot,
            slots: inventory,
        },
        tracked_players,
    };
    snapshot.validate_target_axes()?;
    Ok(snapshot)
}

#[cfg(test)]
fn ready_from_contract_snapshot(
    event: &BackendEventEnvelope,
    snapshot: MinecraftSnapshotV1,
) -> Result<BackendReady, BackendError> {
    if snapshot.process_session_id != event.process_session_id
        || snapshot.connection_epoch != event.connection_epoch
        || snapshot.connection_attempt_id != event.connection_attempt_id
    {
        return Err(BackendError::BackendFailure {
            failure: BackendFailure {
                code: BackendFailureCode::ProtocolError,
                message: "scripted ready snapshot identity mismatch".to_owned(),
                retryable: false,
            },
        });
    }
    Ok(BackendReady {
        process_session_id: event.process_session_id.clone(),
        connection_epoch: event.connection_epoch,
        connection_attempt_id: event.connection_attempt_id.clone(),
        snapshot,
    })
}

fn snapshot_conversion_error(message: impl Into<String>) -> BackendError {
    BackendError::BackendFailure {
        failure: BackendFailure {
            code: BackendFailureCode::ProtocolError,
            message: format!("cannot convert runtime snapshot: {}", message.into()),
            retryable: false,
        },
    }
}

#[cfg(test)]
#[derive(Clone)]
struct ScriptedDriver {
    inner: Arc<FacadeInner>,
}

#[cfg(test)]
impl ScriptedDriver {
    fn session(&self) -> Arc<RuntimeSession> {
        self.inner
            .current_session()
            .expect("scripted facade should own a session")
    }

    fn snapshot(&self, snapshot: MinecraftSnapshotV1) {
        self.session().set_scripted_snapshot(snapshot);
    }

    fn emit(&self, source: FactSource, payload: BackendEventPayload) {
        let session = self.session();
        session.handle.test_drive_event(source, payload);
        session.drain_scripted_events(&self.inner);
    }
}

#[cfg(test)]
impl MinecraftBackendFacade {
    fn scripted(config: MinecraftBackendConfig) -> (Self, ScriptedDriver) {
        let config = config.validate_and_normalize().expect("valid test config");
        let inner = FacadeInner::new(config, true).expect("dispatcher thread");
        let driver = inner.scripted_driver();
        (Self { inner }, driver)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc as std_mpsc,
    };

    use mineintent_contracts::minecraft::{
        AuthKind, BackendEventListener, BackendEventPayload, BackendLifecyclePayload,
        BackendTimeouts, CancellationSignal, Deadline, InventorySlotSnapshot, InventorySnapshot,
        MinecraftIdentityConfig, MinecraftServerConfig, ReconnectPolicy, SelfSnapshot,
        SnapshotProtocol, Vec3Value, WorldSnapshot,
    };

    struct TestCancellation {
        cancelled: AtomicBool,
        notify: Notify,
    }

    impl TestCancellation {
        fn new(value: bool) -> Arc<Self> {
            Arc::new(Self {
                cancelled: AtomicBool::new(value),
                notify: Notify::new(),
            })
        }

        fn trigger(&self) {
            self.cancelled.store(true, Ordering::Release);
            self.notify.notify_waiters();
        }
    }

    impl CancellationSignal for TestCancellation {
        fn is_cancelled(&self) -> bool {
            self.cancelled.load(Ordering::Acquire)
        }

        fn cancelled(&self) -> BoxFuture<'_, ()> {
            Box::pin(async move {
                if !self.is_cancelled() {
                    self.notify.notified().await;
                }
            })
        }
    }

    struct TestDeadline {
        elapsed: AtomicBool,
        notify: Notify,
    }

    impl TestDeadline {
        fn new(value: bool) -> Arc<Self> {
            Arc::new(Self {
                elapsed: AtomicBool::new(value),
                notify: Notify::new(),
            })
        }
    }

    impl Deadline for TestDeadline {
        fn has_elapsed(&self) -> bool {
            self.elapsed.load(Ordering::Acquire)
        }

        fn elapsed(&self) -> BoxFuture<'_, ()> {
            Box::pin(async move {
                if !self.has_elapsed() {
                    self.notify.notified().await;
                }
            })
        }
    }

    fn control(
        cancelled: &Arc<TestCancellation>,
        deadline: Option<&Arc<TestDeadline>>,
    ) -> OperationControl {
        OperationControl::new(
            cancelled.clone(),
            deadline.map(|deadline| deadline.clone() as Arc<dyn Deadline>),
        )
    }

    fn never_control() -> OperationControl {
        control(&TestCancellation::new(false), None)
    }

    fn test_config() -> MinecraftBackendConfig {
        MinecraftBackendConfig {
            world_id: "facade-test-world".to_owned(),
            server: MinecraftServerConfig {
                host: "127.0.0.1".to_owned(),
                port: 25565,
                version: "26.1.2".to_owned(),
            },
            identity: MinecraftIdentityConfig {
                username: "FacadeTestBot".to_owned(),
                auth: AuthKind::Offline,
                profiles_folder: None,
            },
            timeouts: BackendTimeouts {
                connect_ms: 500,
                login_ms: 500,
                spawn_ms: 500,
                stop_ms: 500,
            },
            reconnect: ReconnectPolicy {
                enabled: false,
                initial_delay_ms: 1,
                multiplier: 1.0,
                max_delay_ms: 1,
                jitter_ratio: 0.0,
                stable_reset_ms: 1,
            },
        }
    }

    #[test]
    fn facade_production_config_keeps_runtime_events_in_process() {
        let runtime = run_config(&test_config());
        assert!(!runtime.auto_stop);
        assert!(!runtime.emit_stdout);
    }

    fn test_snapshot() -> MinecraftSnapshotV1 {
        MinecraftSnapshotV1 {
            protocol: SnapshotProtocol::V1,
            snapshot_revision: 7,
            lifecycle_revision: 3,
            captured_at: "2026-08-03T00:00:00Z".to_owned(),
            process_session_id: "scripted-session".to_owned(),
            connection_epoch: 0,
            connection_attempt_id: "attempt-0".to_owned(),
            world: WorldSnapshot {
                world_id: "facade-test-world".to_owned(),
                dimension: "minecraft:overworld".to_owned(),
                minecraft_version: "26.1.2".to_owned(),
                protocol_version: 775,
                game_mode: GameMode::Survival,
                difficulty: None,
                min_y: -64,
                height: 384,
                server_view_distance: None,
                time_of_day: None,
                is_raining: None,
            },
            self_snapshot: SelfSnapshot {
                entity_key: "self:FacadeTestBot".to_owned(),
                username: "FacadeTestBot".to_owned(),
                position: Vec3Value {
                    x: 0.0,
                    y: 64.0,
                    z: 0.0,
                },
                velocity: Vec3Value::default(),
                yaw: 0.0,
                pitch: 0.0,
                on_ground: true,
                alive: true,
                health: 20.0,
                food: 20.0,
                food_saturation: 5.0,
                oxygen: None,
                experience: None,
                effects: Vec::new(),
            },
            inventory: InventorySnapshot {
                selected_hotbar_slot: 0,
                slots: vec![InventorySlotSnapshot {
                    slot: 0,
                    item_name: "minecraft:air".to_owned(),
                    count: 0,
                    metadata: None,
                    durability_used: None,
                }],
            },
            tracked_players: Vec::new(),
        }
    }

    async fn ready_facade() -> (MinecraftBackendFacade, ScriptedDriver) {
        let (facade, driver) = MinecraftBackendFacade::scripted(test_config());
        let start = facade.start(never_control());
        driver.snapshot(test_snapshot());
        driver.emit(
            FactSource::ServerObserved,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Ready {
                snapshot_revision: 7,
            }),
        );
        tokio::time::timeout(Duration::from_secs(1), start)
            .await
            .expect("scripted ready must be bounded")
            .expect("scripted ready should succeed");
        (facade, driver)
    }

    async fn settle_next_command(driver: &ScriptedDriver, result: Result<(), BackendError>) {
        let session = driver.session();
        for _ in 0..100 {
            if session.handle.test_settle_next_command(result.clone()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("scripted command was not admitted within the test bound");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn facade_start_ready_and_initial_state_are_runtime_owned() {
        let (facade, driver) = MinecraftBackendFacade::scripted(test_config());
        assert_eq!(facade.state(), BackendState::Idle);
        let start = facade.start(never_control());
        driver.snapshot(test_snapshot());
        driver.emit(
            FactSource::ServerObserved,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Ready {
                snapshot_revision: 7,
            }),
        );
        let ready = tokio::time::timeout(Duration::from_secs(1), start)
            .await
            .expect("start should not hang")
            .expect("ready should be returned");
        assert_eq!(
            facade.state(),
            BackendState::Ready {
                epoch: ready.connection_epoch,
                attempt_id: ready.connection_attempt_id.clone(),
                ready_at: match facade.state() {
                    BackendState::Ready { ready_at, .. } => ready_at,
                    _ => unreachable!(),
                },
            }
        );
        assert_eq!(facade.snapshot().expect("snapshot").snapshot_revision, 7);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn facade_terminal_failure_maps_without_new_error_kind() {
        let (facade, driver) = MinecraftBackendFacade::scripted(test_config());
        let start = facade.start(never_control());
        let failure = BackendFailure {
            code: BackendFailureCode::AuthenticationFailed,
            message: "scripted auth failure".to_owned(),
            retryable: false,
        };
        driver.emit(
            FactSource::ServerObserved,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Faulted {
                failure: failure.clone(),
            }),
        );
        let result = tokio::time::timeout(Duration::from_secs(1), start)
            .await
            .expect("faulted start should be bounded");
        assert_eq!(result, Err(BackendError::BackendFailure { failure }));
        assert!(matches!(facade.state(), BackendState::Faulted { .. }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn facade_control_cancellation_wins_simultaneous_deadline() {
        let (facade, _driver) = MinecraftBackendFacade::scripted(test_config());
        let cancellation = TestCancellation::new(true);
        let deadline = TestDeadline::new(true);
        let result = facade.start(control(&cancellation, Some(&deadline))).await;
        assert_eq!(
            result,
            Err(BackendError::Cancelled {
                operation: "start".to_owned()
            })
        );
        facade
            .stop("cancelled-start".to_owned(), never_control())
            .await
            .expect("cancelled start must still be stoppable");
        assert!(matches!(facade.state(), BackendState::Stopped { .. }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn facade_start_deadline_is_explicit_and_stoppable() {
        let (facade, _driver) = MinecraftBackendFacade::scripted(test_config());
        let deadline = TestDeadline::new(true);
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            facade.start(control(&TestCancellation::new(false), Some(&deadline))),
        )
        .await
        .expect("deadline start should be bounded");
        assert_eq!(
            result,
            Err(BackendError::DeadlineExceeded {
                operation: "start".to_owned()
            })
        );
        facade
            .stop("deadline-start-cleanup".to_owned(), never_control())
            .await
            .expect("deadline start must leave an owned stoppable session");
        assert!(matches!(facade.state(), BackendState::Stopped { .. }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn facade_stop_is_idempotent_and_final_stopped_is_visible() {
        let (facade, driver) = ready_facade().await;
        let first = facade.stop("first".to_owned(), never_control());
        let second = facade.stop("second".to_owned(), never_control());
        tokio::time::timeout(Duration::from_secs(1), first)
            .await
            .expect("first stop should be bounded")
            .expect("first stop should succeed");
        tokio::time::timeout(Duration::from_secs(1), second)
            .await
            .expect("second stop should be bounded")
            .expect("second stop should be idempotent");
        assert!(matches!(facade.state(), BackendState::Stopped { .. }));
        assert!(matches!(driver.session().start_result(), Some(Ok(_))));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn facade_stopped_before_ready_closes_pending_start_without_fake_close() {
        let (facade, _driver) = MinecraftBackendFacade::scripted(test_config());
        let start = facade.start(never_control());
        let stop = facade.stop("stop-before-ready".to_owned(), never_control());
        let (start_result, stop_result) = tokio::join!(
            tokio::time::timeout(Duration::from_secs(1), start),
            tokio::time::timeout(Duration::from_secs(1), stop),
        );
        stop_result
            .expect("stop must be bounded")
            .expect("stop should succeed");
        assert_eq!(
            start_result
                .expect("pending start must be bounded")
                .expect_err("stopped before ready must fail closed"),
            BackendError::NotReady {
                state: "stopped".to_owned()
            }
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn facade_idle_stop_publishes_stopped_without_connection_or_worker() {
        let (facade, driver) = MinecraftBackendFacade::scripted(test_config());
        facade
            .stop("idle-stop".to_owned(), never_control())
            .await
            .expect("idle stop should complete");
        let session = driver.session();
        assert_eq!(session.handle.connection_epoch(), 0);
        assert!(session.worker.lock().is_none());
        assert!(session.worker_done.load(Ordering::Acquire));
        assert!(session.joined.load(Ordering::Acquire));
        assert!(matches!(facade.state(), BackendState::Stopped { .. }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn facade_concurrent_start_is_rejected_without_old_ready_replay() {
        let (facade, _driver) = MinecraftBackendFacade::scripted(test_config());
        let cancelled = TestCancellation::new(true);
        let first = facade.start(control(&cancelled, None));
        let second = facade.start(never_control()).await;
        assert!(matches!(
            second,
            Err(BackendError::InvalidCommand { field, .. }) if field == "lifecycle"
        ));
        assert!(matches!(first.await, Err(BackendError::Cancelled { .. })));
        facade
            .stop("concurrent-start-test".to_owned(), never_control())
            .await
            .expect("cleanup stop");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn facade_snapshot_and_source_reject_not_ready_and_stale_use() {
        let (facade, driver) = MinecraftBackendFacade::scripted(test_config());
        assert!(matches!(
            facade.snapshot(),
            Err(BackendError::NotReady { .. })
        ));
        let start = facade.start(never_control());
        assert!(matches!(
            facade.snapshot(),
            Err(BackendError::NotReady { .. })
        ));
        driver.snapshot(test_snapshot());
        driver.emit(
            FactSource::ServerObserved,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Ready {
                snapshot_revision: 7,
            }),
        );
        tokio::time::timeout(Duration::from_secs(1), start)
            .await
            .expect("ready")
            .expect("ready result");
        let source = facade
            .observation_source()
            .expect("ready source should be available");
        facade
            .stop("source-stale".to_owned(), never_control())
            .await
            .expect("stop");
        assert!(matches!(
            source.self_pose(),
            Err(BackendError::NotReady { state }) if state == "stopped"
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn facade_internal_lifecycle_observation_bypasses_blocked_public_callback() {
        let (facade, driver) = MinecraftBackendFacade::scripted(test_config());
        let start = facade.start(never_control());
        let (started, started_rx) = std_mpsc::channel();
        let release = Arc::new(Notify::new());
        let (events, events_rx) = std_mpsc::channel();
        let _subscription = facade
            .subscribe(Arc::new(GatedListener {
                started,
                release: release.clone(),
                events,
                first: AtomicBool::new(false),
            }))
            .expect("gated subscription");

        let first_driver = driver.clone();
        let first_emitter = thread::spawn(move || {
            first_driver.emit(
                FactSource::Commanded,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionRequested {
                    attempt: 0,
                }),
            );
        });
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first public callback must reach its gate");
        assert_eq!(
            events_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("first event id"),
            "event-1"
        );

        driver.snapshot(test_snapshot());
        driver.emit(
            FactSource::ServerObserved,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Ready {
                snapshot_revision: 7,
            }),
        );
        assert!(events_rx.try_recv().is_err());

        let ready = tokio::time::timeout(Duration::from_secs(1), start)
            .await
            .expect("internal ready observation must be bounded")
            .expect("ready must be returned before public callback gate opens");
        assert_eq!(ready.snapshot.snapshot_revision, 7);

        release.notify_one();
        assert_eq!(
            events_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("ready public event after gate"),
            "event-2"
        );
        first_emitter.join().expect("first event emitter");

        facade
            .stop("lifecycle-owner-test".to_owned(), never_control())
            .await
            .expect("cleanup stop");
    }

    struct RecordingListener {
        ids: Arc<parking_lot::Mutex<Vec<String>>>,
        sent: Option<std_mpsc::Sender<String>>,
    }

    impl BackendEventListener for RecordingListener {
        fn on_event(&self, event: BackendEventEnvelope) {
            self.ids.lock().push(event.id.clone());
            if let Some(sender) = &self.sent {
                let _ = sender.send(event.id);
            }
        }
    }

    struct GatedListener {
        started: std_mpsc::Sender<()>,
        release: Arc<Notify>,
        events: std_mpsc::Sender<String>,
        first: AtomicBool,
    }

    impl BackendEventListener for GatedListener {
        fn on_event(&self, event: BackendEventEnvelope) {
            let first = !self.first.swap(true, Ordering::AcqRel);
            let _ = self.events.send(event.id);
            if first {
                let _ = self.started.send(());
                futures_block_on(self.release.notified());
            }
        }
    }

    struct PanicListener;

    impl BackendEventListener for PanicListener {
        fn on_event(&self, _event: BackendEventEnvelope) {
            panic!("facade listener panic is an isolated test fault");
        }
    }

    struct ReentrantListener {
        facade: MinecraftBackendFacade,
        subscription: Arc<parking_lot::Mutex<Option<Box<dyn Subscription>>>>,
        called: Arc<AtomicBool>,
    }

    impl BackendEventListener for ReentrantListener {
        fn on_event(&self, _event: BackendEventEnvelope) {
            if self.called.swap(true, Ordering::AcqRel) {
                return;
            }
            let _ = self.facade.state();
            let _ = self.facade.subscribe(Arc::new(RecordingListener {
                ids: Arc::new(parking_lot::Mutex::new(Vec::new())),
                sent: None,
            }));
            if let Some(subscription) = self.subscription.lock().as_mut() {
                subscription.unsubscribe();
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn facade_subscription_fifo_panic_unsubscribe_and_reentry_are_bounded() {
        let (facade, driver) = MinecraftBackendFacade::scripted(test_config());
        let _start = facade.start(never_control());
        let ids = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let (sent, received) = std_mpsc::channel();
        let _recording = facade
            .subscribe(Arc::new(RecordingListener {
                ids: ids.clone(),
                sent: Some(sent),
            }))
            .expect("recording subscription");
        let _panic = facade
            .subscribe(Arc::new(PanicListener))
            .expect("panic subscription");
        let reentrant_slot = Arc::new(parking_lot::Mutex::new(None));
        let reentrant = facade
            .subscribe(Arc::new(ReentrantListener {
                facade: facade.clone(),
                subscription: reentrant_slot.clone(),
                called: Arc::new(AtomicBool::new(false)),
            }))
            .expect("reentrant subscription");
        *reentrant_slot.lock() = Some(reentrant);

        for payload in [
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionRequested {
                attempt: 0,
            }),
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected),
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::LoggedIn {
                version: "26.1.2".to_owned(),
                dimension: "minecraft:overworld".to_owned(),
            }),
        ] {
            driver.emit(FactSource::Commanded, payload);
        }
        let mut delivered = Vec::new();
        for _ in 0..3 {
            delivered.push(
                received
                    .recv_timeout(Duration::from_secs(1))
                    .expect("FIFO callback should arrive"),
            );
        }
        assert_eq!(delivered, vec!["event-1", "event-2", "event-3"]);
        assert_eq!(&*ids.lock(), &delivered);
        assert!(reentrant_slot
            .lock()
            .as_ref()
            .is_some_and(|s| s.is_closed()));
    }

    struct BlockingListener {
        started: std_mpsc::Sender<()>,
        release: Arc<Notify>,
        count: Arc<AtomicU64>,
    }

    struct SynchronousStopListener {
        facade: MinecraftBackendFacade,
        completed: std_mpsc::Sender<Result<Result<(), BackendError>, tokio::time::error::Elapsed>>,
        called: AtomicBool,
    }

    impl BackendEventListener for SynchronousStopListener {
        fn on_event(&self, _event: BackendEventEnvelope) {
            if self.called.swap(true, Ordering::AcqRel) {
                return;
            }
            let facade = self.facade.clone();
            let result = futures_block_on(async move {
                tokio::time::timeout(
                    Duration::from_millis(800),
                    facade.stop("callback-stop".to_owned(), never_control()),
                )
                .await
            });
            let _ = self.completed.send(result);
        }
    }

    struct SaturatedStopListener {
        facade: MinecraftBackendFacade,
        started: std_mpsc::Sender<()>,
        capacity_ready: parking_lot::Mutex<std_mpsc::Receiver<()>>,
        completed: std_mpsc::Sender<Result<Result<(), BackendError>, tokio::time::error::Elapsed>>,
        events: std_mpsc::Sender<String>,
        called: AtomicBool,
    }

    impl BackendEventListener for SaturatedStopListener {
        fn on_event(&self, event: BackendEventEnvelope) {
            let first = !self.called.swap(true, Ordering::AcqRel);
            let _ = self.events.send(event.id);
            if !first {
                return;
            }
            let _ = self.started.send(());
            self.capacity_ready
                .lock()
                .recv()
                .expect("filler must reach the bounded capacity");
            let facade = self.facade.clone();
            let result = futures_block_on(async move {
                tokio::time::timeout(
                    Duration::from_millis(800),
                    facade.stop("saturated-callback-stop".to_owned(), never_control()),
                )
                .await
            });
            let _ = self.completed.send(result);
        }
    }

    impl BackendEventListener for BlockingListener {
        fn on_event(&self, _event: BackendEventEnvelope) {
            let _ = self.started.send(());
            let guard = self.release.notified();
            futures_block_on(guard);
            self.count.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn futures_block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("callback helper runtime");
        runtime.block_on(future)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn facade_unsubscribe_linearizes_before_waiting_for_active_callback() {
        let (facade, driver) = MinecraftBackendFacade::scripted(test_config());
        let _start = facade.start(never_control());
        let (started, started_rx) = std_mpsc::channel();
        let release = Arc::new(Notify::new());
        let count = Arc::new(AtomicU64::new(0));
        let subscription = Arc::new(parking_lot::Mutex::new(Some(
            facade
                .subscribe(Arc::new(BlockingListener {
                    started,
                    release: release.clone(),
                    count: count.clone(),
                }))
                .expect("blocking subscription"),
        )));
        let emit_driver = driver.clone();
        let emitter = thread::spawn(move || {
            emit_driver.emit(
                FactSource::Commanded,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionRequested {
                    attempt: 0,
                }),
            );
        });
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("callback should start");
        let remove_slot = subscription.clone();
        let remover = thread::spawn(move || {
            remove_slot
                .lock()
                .as_mut()
                .expect("subscription remains owned")
                .unsubscribe();
        });
        thread::sleep(Duration::from_millis(20));
        release.notify_one();
        remover.join().expect("unsubscribe thread");
        emitter.join().expect("emitter thread");
        assert_eq!(count.load(Ordering::Acquire), 1);
        driver.emit(
            FactSource::Commanded,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected),
        );
        thread::sleep(Duration::from_millis(20));
        assert_eq!(count.load(Ordering::Acquire), 1);
        assert!(subscription
            .lock()
            .as_ref()
            .is_some_and(|subscription| subscription.is_closed()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn facade_callback_sync_stop_waits_on_runtime_state_without_dispatch_deadlock() {
        let (facade, driver) = MinecraftBackendFacade::scripted(test_config());
        let _start = facade.start(never_control());
        let (recorded, recorded_rx) = std_mpsc::channel();
        let _recording = facade
            .subscribe(Arc::new(RecordingListener {
                ids: Arc::new(parking_lot::Mutex::new(Vec::new())),
                sent: Some(recorded),
            }))
            .expect("recording subscription");
        let (completed, completed_rx) = std_mpsc::channel();
        let _stop_listener = facade
            .subscribe(Arc::new(SynchronousStopListener {
                facade: facade.clone(),
                completed,
                called: AtomicBool::new(false),
            }))
            .expect("synchronous stop subscription");

        let emitter = thread::spawn(move || {
            driver.emit(
                FactSource::Commanded,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionRequested {
                    attempt: 0,
                }),
            );
        });
        let stop_result = completed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("callback must synchronously complete stop within one second");
        assert_eq!(stop_result, Ok(Ok(())));
        emitter.join().expect("event emitter");

        assert!(matches!(facade.state(), BackendState::Stopped { .. }));
        let session = facade.inner.current_session().expect("owned session");
        assert!(session.worker_done.load(Ordering::Acquire));
        assert!(session.joined.load(Ordering::Acquire));

        let first = recorded_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("dispatcher must finish the triggering event");
        let stopped = recorded_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("dispatcher must subsequently deliver strict stopped FIFO event");
        assert_eq!(first, "event-1");
        assert_eq!(stopped, "event-2");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn facade_scripted_callback_sync_stop_completes_with_quiescent_bounded_dispatch_saturation(
    ) {
        let (facade, driver) = MinecraftBackendFacade::scripted(test_config());
        let _start = facade.start(never_control());
        let (started, started_rx) = std_mpsc::channel();
        let (capacity_ready, capacity_ready_rx) = std_mpsc::channel();
        let (filler_done, filler_done_rx) = std_mpsc::channel();
        let (completed, completed_rx) = std_mpsc::channel();
        let (events, events_rx) = std_mpsc::channel();
        let _subscription = facade
            .subscribe(Arc::new(SaturatedStopListener {
                facade: facade.clone(),
                started,
                capacity_ready: parking_lot::Mutex::new(capacity_ready_rx),
                completed,
                events,
                called: AtomicBool::new(false),
            }))
            .expect("saturated stop subscription");

        let first_driver = driver.clone();
        let first_emitter = thread::spawn(move || {
            first_driver.emit(
                FactSource::Commanded,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionRequested {
                    attempt: 0,
                }),
            );
        });
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("saturated callback must start");

        let fill_driver = driver.clone();
        let filler = thread::spawn(move || {
            for attempt in 0..DISPATCH_CAPACITY {
                fill_driver.emit(
                    FactSource::Commanded,
                    BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionRequested {
                        attempt: attempt as u32 + 1,
                    }),
                );
            }
            filler_done.send(()).expect("filler completion receiver");
        });
        if filler_done_rx.recv_timeout(Duration::from_secs(1)).is_err() {
            let _ = capacity_ready.send(());
            first_emitter.join().expect("first event emitter cleanup");
            filler.join().expect("bounded filler cleanup");
            panic!("bounded filler did not complete within one second");
        }
        filler.join().expect("bounded filler");
        capacity_ready
            .send(())
            .expect("saturated callback must remain alive");

        let stop_result = completed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("callback stop must complete at bounded saturation");
        assert_eq!(stop_result, Ok(Ok(())));
        first_emitter.join().expect("first event emitter");

        let expected_count = DISPATCH_CAPACITY + 2;
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut delivered = Vec::with_capacity(expected_count);
        while delivered.len() < expected_count {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "FIFO delivery exceeded its bound");
            delivered.push(
                events_rx
                    .recv_timeout(remaining)
                    .expect("bounded dispatcher must not lose an event"),
            );
        }
        let expected = (1..=expected_count)
            .map(|id| format!("event-{id}"))
            .collect::<Vec<_>>();
        assert_eq!(delivered, expected);
        assert!(matches!(facade.state(), BackendState::Stopped { .. }));
        let session = facade.inner.current_session().expect("owned session");
        assert!(session.worker_done.load(Ordering::Acquire));
        assert!(session.joined.load(Ordering::Acquire));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn facade_motor_completion_cancellation_and_release_are_generation_safe() {
        let (facade, driver) = ready_facade().await;
        let motor = facade.motor().expect("motor");
        let success_motor = motor.clone();
        let success = tokio::spawn(async move {
            success_motor
                .look_relative(
                    LookRelativeRequest {
                        yaw_degrees: 10.0,
                        pitch_degrees: -5.0,
                    },
                    never_control(),
                )
                .await
        });
        tokio::task::yield_now().await;
        settle_next_command(&driver, Ok(())).await;
        tokio::time::timeout(Duration::from_secs(1), success)
            .await
            .expect("look completion")
            .expect("look task")
            .expect("look should complete");

        let cancellation = TestCancellation::new(false);
        let cancel_motor = motor.clone();
        let cancel_control = control(&cancellation, None);
        let canceled = tokio::spawn(async move {
            cancel_motor
                .move_input(
                    MoveInputRequest {
                        directions: vec![
                            mineintent_contracts::minecraft::MotorMoveDirection::Forward,
                        ],
                        duration_ms: 100,
                        sprint: None,
                    },
                    cancel_control,
                )
                .await
        });
        for _ in 0..100 {
            if driver.session().handle.test_has_pending_command() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(driver.session().handle.test_has_pending_command());
        cancellation.trigger();
        let canceled_result = tokio::time::timeout(Duration::from_secs(1), canceled)
            .await
            .expect("cancelled move should settle")
            .expect("cancelled move task");
        assert_eq!(
            canceled_result,
            Err(BackendError::Cancelled {
                operation: "move_input".to_owned()
            })
        );

        let next_motor = motor.clone();
        let next = tokio::spawn(async move {
            next_motor
                .move_input(
                    MoveInputRequest {
                        directions: vec![
                            mineintent_contracts::minecraft::MotorMoveDirection::Right,
                        ],
                        duration_ms: 100,
                        sprint: None,
                    },
                    never_control(),
                )
                .await
        });
        settle_next_command(&driver, Ok(())).await;
        tokio::time::timeout(Duration::from_secs(1), next)
            .await
            .expect("new generation completion")
            .expect("new generation task")
            .expect("new move should complete");

        let release_motor = motor.clone();
        let release = tokio::task::spawn_blocking(move || release_motor.release_all());
        settle_next_command(&driver, Ok(())).await;
        tokio::time::timeout(Duration::from_secs(1), release)
            .await
            .expect("release_all should be bounded")
            .expect("release task")
            .expect("release_all should complete");
    }
}
