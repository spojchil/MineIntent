//! Production Participant runtime.
//!
//! The runtime is intentionally a small composition layer.  Backend callbacks,
//! internal facts, and wake candidates enter one synchronous admission point;
//! the worker then journals those admitted events in order and runs only the
//! registered wake policies.  All model-visible facts come from the explicit
//! frame source rather than from a fallback snapshot assembled here.

use std::{
    collections::VecDeque,
    fmt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex, MutexGuard, Weak,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use mineintent_contracts::{
    agent::{
        AgentChatMessageV5, AgentError, AgentErrorCode, AgentHotbarV5, AgentPoseV5,
        AgentRunRequest, AgentRunner, AgentStatusV5, CancellationSignal, ContractFuture, Deadline,
        ExecutionControl, JsonAgentDecisionContextV5, JsonObject, PromptTemplateRef, RunId,
    },
    capability::ToolCapabilityRegistry,
    information::SoundValues,
    minecraft::{
        BackendEventEnvelope, BackendEventKind, BackendEventListener, BackendEventPayload,
        BackendLifecyclePayload, MinecraftBackendApi, MinecraftFrameFacts, ProtocolChatEvent,
        Subscription,
    },
};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{
    sync::{broadcast, watch, Mutex as AsyncMutex, Notify},
    task::{AbortHandle, JoinError, JoinHandle},
};
use uuid::Uuid;

use crate::{
    agent::{
        AgentChatInputV5, AgentChatTriggerV5, AgentContextV5Assembler, AgentContextV5AssemblyError,
        AgentContextV5EventInput, AgentContextV5Input, AgentModelRequest, ConcreteAgentRunner,
        ModelCompletion, RoundViewportSampler,
    },
    capability::{CapabilityJournal, RegistryToolDispatcher, SpeechSchedulerPort},
    memory::MemoryStore,
    speech::{
        interpret_player_chat, ChatInputContext, PlayerChatMessage, SpeechScheduler,
        SpeechTransport,
    },
    telemetry::{
        DebugDecision, DebugDecisionStatus, DebugFailureSource, DebugFailureSummary,
        DebugStateStore, DebugStateUpdate,
    },
};

const MAX_PENDING_FACTS: usize = 20;
const STOP_WORKER_SETTLE: Duration = Duration::from_millis(250);
// Participant work is smaller than the backend bridge because each admitted
// control item may retain a frame source snapshot and a journal payload while
// the worker is in model/journal I/O. These are explicit, tunable limits:
// ordinary facts are reconstructable, control is lossless, and overflow
// markers have their own bounded lane.
const PARTICIPANT_ORDINARY_CAPACITY: usize = 16;
const PARTICIPANT_CONTROL_CAPACITY: usize = 8;
const PARTICIPANT_OVERFLOW_CAPACITY: usize = 4;
const PARTICIPANT_MAX_OVERFLOW_TYPES: usize = 8;
const PARTICIPANT_MAX_PENDING_OMITTED_TYPES: usize = 8;

/// Scope identity used by every admitted fact and model run.
mod ingest;
mod ports;
mod queue;
mod support;
mod types;

// ports/types/ingest 含 crate 外可见类型，经此重导出保持原有公开路径不变；
// queue 与 support 全是 runtime 内部项，只在本模块内可见。
pub use ingest::*;
pub use ports::*;
use queue::*;
use support::*;
pub use types::*;
// support 内唯一跨模块使用的项，保持原有 pub(crate) 路径。
pub(crate) use support::safe_fact_event_type;

/// A single ordered Participant runtime.  Construct it in an application,
/// call `start_worker` after the application-owned backend is ready.
pub struct ParticipantRuntime<R>
where
    R: ParticipantAgentPort + 'static,
{
    backend: Arc<dyn MinecraftBackendApi>,
    agent: Arc<R>,
    frame_source: Arc<dyn ParticipantFrameSource>,
    memory: Arc<dyn ParticipantMemorySource>,
    journal: Arc<dyn CapabilityJournal>,
    speech: Arc<dyn ParticipantSpeechPort>,
    debug: Arc<DebugStateStore>,
    clock: Arc<dyn ParticipantClock>,
    prompt_template: PromptTemplateRef,
    run_deadline: Duration,
    wake_registry: WakeRegistry,
    assembler: AgentContextV5Assembler,
    admission_serial: Arc<Mutex<()>>,
    fact_owner: Arc<ParticipantFactOwner>,
    admission_observer: Mutex<Option<Arc<dyn ParticipantAdmissionObserver>>>,
    event_queue: Arc<ParticipantEventQueue>,
    admission_cancelled: AtomicBool,
    state: Mutex<RuntimeState>,
    worker: Mutex<Option<JoinHandle<()>>>,
    subscription: Mutex<Option<Box<dyn Subscription>>>,
    cleanup_serial: Mutex<()>,
    stop_serial: AsyncMutex<()>,
    stop_cleanup: watch::Sender<bool>,
    lifecycle_signal: watch::Sender<ParticipantLifecycle>,
    generation: watch::Sender<u64>,
    failures: broadcast::Sender<ParticipantFailure>,
    run_id_namespace_digest: String,
    run_id_instance_id: String,
    ingest_counters: IngestCounters,
    worker_gate: WorkerGate,
}

impl<R> ParticipantRuntime<R>
where
    R: ParticipantAgentPort + 'static,
{
    pub fn new(config: ParticipantRuntimeConfig<R>) -> Arc<Self> {
        Self::try_new(config).expect("participant runtime configuration must be valid")
    }

    pub fn try_new(
        config: ParticipantRuntimeConfig<R>,
    ) -> Result<Arc<Self>, ParticipantRuntimeError> {
        let namespace_length = config.run_id_namespace.chars().count();
        if !(1..=128).contains(&namespace_length)
            || config.run_id_namespace.chars().any(char::is_control)
        {
            return Err(ParticipantRuntimeError::InvalidConfig(
                "run_id_namespace must contain 1..=128 non-control characters".to_owned(),
            ));
        }
        let run_id_instance_id = Uuid::new_v4().simple().to_string();
        let run_id_namespace_digest = namespace_digest(&config.run_id_namespace);
        let max_run_id = format!(
            "p-{run_id_namespace_digest}-{run_id_instance_id}-{}-{}-{}",
            base36_u64(u64::MAX),
            base36_u64(u64::MAX),
            base36_u64(u64::MAX),
        );
        if max_run_id.chars().count() > 128 {
            return Err(ParticipantRuntimeError::InvalidConfig(
                "run id assembly exceeds the contract length limit".to_owned(),
            ));
        }
        let (stop_cleanup, _) = watch::channel(true);
        let (lifecycle_signal, _) = watch::channel(ParticipantLifecycle::Created);
        let (generation, _) = watch::channel(0_u64);
        let (failures, _) = broadcast::channel(32);
        let admission_serial = Arc::new(Mutex::new(()));
        let fact_owner = ParticipantFactOwner::new(Arc::clone(&admission_serial));
        Ok(Arc::new(Self {
            backend: config.backend,
            agent: config.agent,
            frame_source: config.frame_source,
            memory: config.memory,
            journal: config.journal,
            speech: config.speech,
            debug: config.debug,
            clock: config.clock,
            prompt_template: config.prompt_template,
            run_deadline: config.run_deadline,
            wake_registry: config.wake_registry,
            assembler: AgentContextV5Assembler,
            admission_serial,
            fact_owner,
            admission_observer: Mutex::new(None),
            event_queue: ParticipantEventQueue::new(),
            admission_cancelled: AtomicBool::new(false),
            state: Mutex::new(RuntimeState {
                lifecycle: ParticipantLifecycle::Created,
                scope: None,
                generation: 0,
                next_ordinal: 0,
                active: None,
                terminal_pending: false,
                retired_process_sessions: std::collections::HashSet::new(),
                closed_scope: None,
                closed_connection_attempt_id: None,
                active_connection_attempt_id: None,
            }),
            worker: Mutex::new(None),
            subscription: Mutex::new(None),
            cleanup_serial: Mutex::new(()),
            stop_serial: AsyncMutex::new(()),
            stop_cleanup,
            lifecycle_signal,
            generation,
            failures,
            run_id_namespace_digest,
            run_id_instance_id,
            ingest_counters: IngestCounters::default(),
            worker_gate: WorkerGate::default(),
        }))
    }

    pub fn lifecycle(&self) -> ParticipantLifecycle {
        lock(&self.state).lifecycle
    }

    pub fn current_scope(&self) -> Option<ParticipantScope> {
        lock(&self.state).scope.clone()
    }

    /// Current runtime generation for per-wake production source assembly.
    pub fn current_generation(&self) -> u64 {
        lock(&self.state).generation
    }

    /// A weak fact-owner port for a per-wake body observation source. The
    /// weak edge keeps an observation dispatcher from retaining a stopped
    /// runtime.
    pub fn fact_owner(&self) -> Weak<ParticipantFactOwner> {
        Arc::downgrade(&self.fact_owner)
    }

    pub fn wake_registry(&self) -> &WakeRegistry {
        &self.wake_registry
    }

    pub fn tool_definitions(&self) -> Vec<mineintent_contracts::agent::WireToolDefinition> {
        self.agent.definitions()
    }

    /// 未进 journal 的可重建普通事实的按类型计数。排障与停机汇总用。
    pub fn ingest_counters(&self) -> &IngestCounters {
        &self.ingest_counters
    }

    /// worker 单步闸门；仅饱和类测试使用，生产不调用即恒放行。
    pub fn worker_gate(&self) -> &WorkerGate {
        &self.worker_gate
    }

    pub fn subscribe_failures(&self) -> broadcast::Receiver<ParticipantFailure> {
        self.failures.subscribe()
    }

    pub fn debug_snapshot(&self) -> crate::telemetry::DebugSnapshot {
        self.debug.snapshot()
    }

    /// Deterministic saturation probe for the Participant integration tests.
    /// It intentionally exposes only bounded queue counts, not queue storage
    /// or admission mutation, and is not an application composition seam.
    #[doc(hidden)]
    pub fn queue_counts_for_test(&self) -> (usize, usize, usize, usize, usize) {
        self.event_queue.counts()
    }

    /// Installs the optional admission probe before a deterministic test
    /// drives a producer. It does not participate in the production wiring or
    /// the model-visible contract.
    #[doc(hidden)]
    pub fn install_admission_observer_for_test(
        &self,
        observer: Arc<dyn ParticipantAdmissionObserver>,
    ) {
        *lock(&self.admission_observer) = Some(observer);
    }

    /// Deterministic saturation probe for tests that need to establish that a
    /// producer is blocked on ticket/capacity before exercising cancellation.
    #[doc(hidden)]
    pub async fn wait_for_queue_waiters_for_test(&self, expected: usize) {
        self.event_queue.wait_for_waiters(expected).await;
    }

    /// Deterministic test probe that waits until the published generation
    /// reaches at least `expected`. Scope/generation invalidation is always
    /// published while the admission serial is still held, so observing the
    /// new generation deterministically proves that a pending older admission
    /// cannot have resolved yet (it still needs the serial to re-check).
    #[doc(hidden)]
    pub async fn wait_for_generation_for_test(&self, expected: u64) {
        let mut receiver = self.generation.subscribe();
        loop {
            if *receiver.borrow() >= expected {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }

    /// Starts the ordered worker and attaches the runtime to an already-owned
    /// in-process backend event stream. The application must drive the frozen
    /// backend `start(OperationControl)` before this call and
    /// `stop(reason, OperationControl)` after [`Self::stop`] returns. No model
    /// call is made here.
    pub fn start_worker(self: &Arc<Self>) -> Result<(), ParticipantRuntimeError> {
        let startup_result: Result<(), ParticipantRuntimeError> = {
            let _serial = lock(&self.admission_serial);
            let mut state = lock(&self.state);
            match state.lifecycle {
                ParticipantLifecycle::Created => {
                    state.lifecycle = ParticipantLifecycle::Running;
                    self.publish_lifecycle(ParticipantLifecycle::Running);
                }
                ParticipantLifecycle::Stopped => return Err(ParticipantRuntimeError::Stopped),
                ParticipantLifecycle::Faulted => return Err(ParticipantRuntimeError::Faulted),
                ParticipantLifecycle::Running | ParticipantLifecycle::Stopping => {
                    return Err(ParticipantRuntimeError::AlreadyStarted)
                }
            }
            match self.backend.capture_frame_facts() {
                Err(error) => Err(ParticipantRuntimeError::Backend(error.to_string())),
                Ok(facts) => match startup_scope(&facts) {
                    Err(message) => Err(ParticipantRuntimeError::Backend(message.to_owned())),
                    Ok(scope) => {
                        state.scope = Some(scope.clone());
                        state.active_connection_attempt_id =
                            Some(facts.snapshot.connection_attempt_id.clone());
                        state.closed_scope = None;
                        state.closed_connection_attempt_id = None;
                        self.fact_owner
                            .bind_scope(state.generation, state.scope.clone());
                        self.record_fact(
                            state.generation,
                            ParticipantFact {
                                id: format!("participant-started-{}", self.run_id_instance_id),
                                occurred_at: facts.snapshot.captured_at,
                                scope,
                                event_type: "participant.started".to_owned(),
                                summary: "AI 参与者已进入世界".to_owned(),
                            },
                        );
                        let runtime = Arc::clone(self);
                        let worker = tokio::spawn(async move { runtime.worker_loop().await });
                        *lock(&self.worker) = Some(worker);
                        Ok(())
                    }
                },
            }
        };
        if let Err(error) = startup_result {
            self.fail_runtime_sync(
                ParticipantFailureSource::Backend,
                "backend_startup_snapshot_failed",
                "backend startup snapshot failed",
                None,
            );
            self.rollback_startup_failure(&error);
            return Err(error);
        }
        self.debug.update(DebugStateUpdate {
            connection: Some(self.backend.state()),
            ..DebugStateUpdate::default()
        });

        let listener: Arc<dyn BackendEventListener> = self.clone();
        match self.backend.subscribe(listener) {
            Ok(subscription) => {
                let mut subscription = Some(subscription);
                let lifecycle = {
                    let _serial = lock(&self.admission_serial);
                    let lifecycle = lock(&self.state).lifecycle;
                    if lifecycle == ParticipantLifecycle::Running {
                        *lock(&self.subscription) = subscription.take();
                    }
                    lifecycle
                };
                if lifecycle == ParticipantLifecycle::Running {
                    Ok(())
                } else {
                    if let Some(mut subscription) = subscription {
                        subscription.unsubscribe();
                    }
                    Err(startup_lifecycle_error(lifecycle))
                }
            }
            Err(error) => {
                self.fail_runtime_sync(
                    ParticipantFailureSource::Backend,
                    "backend_subscribe_failed",
                    "backend subscription failed",
                    None,
                );
                self.rollback_startup_failure(&ParticipantRuntimeError::Backend(error.to_string()));
                Err(ParticipantRuntimeError::Backend(error.to_string()))
            }
        }
    }

    /// Synchronously admits a backend event and returns after any required
    /// scope invalidation, cancellation, body release, and speech cancellation
    /// have completed.  The async worker is only reached after this point.
    pub fn ingest_backend_event(
        &self,
        event: BackendEventEnvelope,
    ) -> Result<ParticipantAdmission, ParticipantRuntimeError> {
        let mut serial = AdmissionSerialGuard::new(&self.admission_serial);
        self.ensure_running()?;
        let scope = ParticipantScope::from_backend(&event);
        self.debug.update(DebugStateUpdate {
            connection: Some(self.backend.state()),
            ..DebugStateUpdate::default()
        });

        let BackendAdmission::Accepted {
            generation,
            ordinal,
            cleanup,
            scope_control,
            record_fact,
            terminal,
        } = self.admit_scope_for_backend(&event, &scope)
        else {
            return Ok(ParticipantAdmission::Ignored);
        };
        self.perform_cleanup(
            cleanup,
            "participant_scope_changed",
            AgentError::new(AgentErrorCode::ScopeInvalid, "participant_scope_changed"),
        );
        if terminal {
            self.stop_cleanup.send_replace(true);
        }

        // 作用域记账（上面的 admit + cleanup）照做——重连后第一条事件可能就是
        // 实体事件，作用域变更得由它带进来。但入队到此为止：见 support.rs 的
        // `backend_event_enters_queue`。实体与方块在队列里两条出路都是死的，
        // 而它们会把没点名的玩家聊天挤出同一条 16 格车道。
        if !backend_event_enters_queue(&event) {
            return Ok(ParticipantAdmission::Ignored);
        }

        let (event_type, wake_candidate) = self.evaluate_backend_wake(&event, &scope)?;
        let terminal_lifecycle = terminal.then(|| backend_terminal_lifecycle(&event));

        let pending_fact =
            (!terminal && record_fact && wake_candidate.is_none()).then(|| ParticipantFact {
                id: event.id.clone(),
                occurred_at: event.occurred_at.clone(),
                scope: scope.clone(),
                event_type: backend_fact_type(&event).to_owned(),
                summary: backend_event_summary(&event),
            });

        let retained_trigger = wake_candidate.clone();
        let mut trigger_retained = false;
        if let Some(trigger) = retained_trigger.as_ref() {
            self.frame_source.retain_trigger(&scope, trigger)?;
            trigger_retained = true;
        }
        let wake = wake_candidate.map(|trigger| WakeItem {
            ordinal,
            scope: scope.clone(),
            occurred_at: event.occurred_at.clone(),
            trigger,
            trigger_retained,
        });
        let has_wake = wake.is_some();
        let backend_control = backend_event_is_control(&event);
        let queue_admission = match self.enqueue_work(
            WorkItem {
                ticket: 0,
                ordinal,
                generation,
                scope: scope.clone(),
                occurred_at: event.occurred_at.clone(),
                event_id: event.id,
                event_type,
                wake,
                scope_control: scope_control || backend_control,
                terminal,
                terminal_lifecycle,
                overflow: None,
            },
            &mut serial,
        ) {
            Ok(admission) => admission,
            Err(error) => {
                if trigger_retained {
                    if let Some(trigger) = retained_trigger.as_ref() {
                        self.frame_source.release_trigger(&scope, trigger);
                    }
                }
                return Err(error);
            }
        };
        // 一次 match 走完三种准入结果。原先是「先 if Ignored 提前 return，再
        // match 剩下两种」，于是第二次 match 的 Ignored 分支不可达，只能靠
        // unreachable! 兜住一个编译器无法传播的不变量。合起来之后它不再需要存在。
        match queue_admission {
            QueueAdmission::Ignored => {
                if trigger_retained {
                    if let Some(trigger) = retained_trigger.as_ref() {
                        self.frame_source.release_trigger(&scope, trigger);
                    }
                }
                return Ok(ParticipantAdmission::Ignored);
            }
            QueueAdmission::Accepted => {
                if let Some(fact) = pending_fact {
                    self.notify_admission_observer(&fact.event_type);
                    self.record_fact(generation, fact);
                }
            }
            QueueAdmission::OrdinaryDropped { event_type } => {
                if pending_fact.is_some() {
                    self.record_pending_omission(generation, event_type);
                }
            }
        }
        Ok(match has_wake {
            true => ParticipantAdmission::WakeQueued { ordinal },
            false => ParticipantAdmission::Recorded,
        })
    }

    pub fn emit_internal(
        &self,
        event: ParticipantInternalEvent,
    ) -> Result<ParticipantAdmission, ParticipantRuntimeError> {
        let mut serial = AdmissionSerialGuard::new(&self.admission_serial);
        self.ensure_running()?;
        let (id, occurred_at, scope) = {
            let (id, occurred_at, scope) = event.metadata();
            (id.to_owned(), occurred_at.to_owned(), scope.clone())
        };
        let admission = match &event {
            ParticipantInternalEvent::ScopeChanged { .. } => {
                self.admit_explicit_scope(&scope, true, None)
            }
            // 终止事件按 lifecycle 分成两条臂，而不是先合并再在里面重新 match
            // 同一个值——那样第二次 match 的 `_` 分支不可达，只能靠 unreachable!
            // 兜住一个编译器无法传播的不变量。
            ParticipantInternalEvent::Faulted { .. } => {
                self.admit_explicit_scope(&scope, false, Some(ParticipantLifecycle::Faulted))
            }
            ParticipantInternalEvent::Closed { .. } | ParticipantInternalEvent::Stopped { .. } => {
                self.admit_explicit_scope(&scope, false, Some(ParticipantLifecycle::Stopped))
            }
            ParticipantInternalEvent::Fact { .. } => self.admit_explicit_scope(&scope, false, None),
        };
        let ExplicitAdmission::Accepted {
            generation,
            ordinal,
            cleanup,
            terminal_lifecycle,
        } = admission
        else {
            return Ok(ParticipantAdmission::Ignored);
        };
        let terminal = terminal_lifecycle.is_some();
        self.perform_cleanup(
            cleanup,
            "participant_internal_scope_changed",
            AgentError::new(AgentErrorCode::ScopeInvalid, "participant_scope_changed"),
        );
        if terminal {
            self.stop_cleanup.send_replace(true);
        }

        let internal_scope_control =
            matches!(&event, ParticipantInternalEvent::ScopeChanged { .. });
        let (event_type, summary) = match event {
            ParticipantInternalEvent::Fact {
                event_type,
                summary,
                ..
            } => (event_type, summary),
            ParticipantInternalEvent::ScopeChanged { reason, .. } => {
                ("scope_changed".to_owned(), bounded_summary(reason))
            }
            ParticipantInternalEvent::Closed { reason, .. } => {
                ("connection_closed".to_owned(), bounded_summary(reason))
            }
            ParticipantInternalEvent::Faulted { code, .. } => {
                ("backend_faulted".to_owned(), bounded_summary(code))
            }
            ParticipantInternalEvent::Stopped { reason, .. } => {
                ("backend_stopped".to_owned(), bounded_summary(reason))
            }
        };
        let pending_fact = (!terminal).then(|| ParticipantFact {
            id: id.clone(),
            occurred_at: occurred_at.clone(),
            scope: scope.clone(),
            event_type: event_type.clone(),
            summary: bounded_summary(summary),
        });
        let queue_admission = self.enqueue_work(
            WorkItem {
                ticket: 0,
                ordinal,
                generation,
                scope,
                occurred_at,
                event_id: id,
                event_type,
                wake: None,
                scope_control: internal_scope_control,
                terminal,
                terminal_lifecycle,
                overflow: None,
            },
            &mut serial,
        )?;
        // 与 admit_scope_for_backend 同形：一次 match 走完三种准入结果，
        // 不留需要 unreachable! 兜住的不可达分支。
        match queue_admission {
            QueueAdmission::Ignored => return Ok(ParticipantAdmission::Ignored),
            QueueAdmission::Accepted => {
                if let Some(fact) = pending_fact {
                    self.notify_admission_observer(&fact.event_type);
                    self.record_fact(generation, fact);
                }
            }
            QueueAdmission::OrdinaryDropped { event_type } => {
                if pending_fact.is_some() {
                    self.record_pending_omission(generation, event_type);
                }
            }
        }
        Ok(ParticipantAdmission::Recorded)
    }

    pub fn ingest_event(
        &self,
        event: ParticipantEvent,
    ) -> Result<ParticipantAdmission, ParticipantRuntimeError> {
        match event {
            ParticipantEvent::Backend(event) => self.ingest_backend_event(event),
            ParticipantEvent::Internal(event) => self.emit_internal(event),
        }
    }

    /// Performs the synchronous half of shutdown. It cancels the active run,
    /// invalidates queued work, cancels remaining speech, releases the body,
    /// and unsubscribes before any worker await is attempted.
    pub fn request_stop(&self) -> Result<bool, ParticipantRuntimeError> {
        self.admission_cancelled.store(true, Ordering::Release);
        self.event_queue.close_admission();
        let (should_wait, cleanup) = {
            let _serial = lock(&self.admission_serial);
            let should_wait = {
                let mut state = lock(&self.state);
                match state.lifecycle {
                    ParticipantLifecycle::Created => {
                        state.lifecycle = ParticipantLifecycle::Stopped;
                        self.publish_lifecycle(ParticipantLifecycle::Stopped);
                        self.stop_cleanup.send_replace(true);
                        false
                    }
                    ParticipantLifecycle::Stopped => false,
                    ParticipantLifecycle::Stopping => false,
                    ParticipantLifecycle::Running | ParticipantLifecycle::Faulted => {
                        state.lifecycle = ParticipantLifecycle::Stopping;
                        self.publish_lifecycle(ParticipantLifecycle::Stopping);
                        self.stop_cleanup.send_replace(false);
                        true
                    }
                }
            };
            if should_wait {
                (true, self.invalidate_generation())
            } else {
                (false, Cleanup::empty())
            }
        };
        if !should_wait {
            self.frame_source.release_retained_triggers();
            return Ok(false);
        }

        self.perform_cleanup(cleanup, "participant_stopped", AgentError::run_cancelled());
        self.frame_source.release_retained_triggers();
        if let Some(mut subscription) = lock(&self.subscription).take() {
            subscription.unsubscribe();
        }
        self.stop_cleanup.send_replace(true);
        Ok(true)
    }

    /// Completes shutdown after [`Self::request_stop`] has performed its
    /// synchronous invalidation. A worker receives a bounded chance to finish
    /// journal/queue cancellation; only then is the abort fallback used.
    pub async fn stop(&self) -> Result<(), ParticipantRuntimeError> {
        let _stop_owner = self.stop_serial.lock().await;
        let _ = self.request_stop()?;
        self.wait_for_stop_cleanup().await;

        let worker_handle = { lock(&self.worker).take() };
        if let Some(worker) = worker_handle {
            let abort = worker.abort_handle();
            let mut worker = worker;
            // 三种收场必须分开看，此前它们被合成了一个 `.is_err()`。
            //
            // worker 已经 panic 时，`&mut worker` 立刻就绪，`timeout` 返回的是
            // `Ok(Err(JoinError))`——`.is_err()` 为 false，于是既不 abort 也不
            // 绑定那个 JoinError，停机继续走到 `Ok(())`。结果是：参与者从 panic
            // 那一刻起就不再处理任何唤醒，而停机报告成功，journal 里一个字都没有。
            //
            // 这条路径正是 `toolloop/src/control.rs` 里「不做 panic 隔离，交给
            // 调用方的任务边界接成 JoinError 走失败流」所指的那个接管点。tokio
            // 确实接住了（进程活着），但接住之后没有人接手——删掉循环里的捕获，
            // 依据的就是这里会出声，所以这里必须真的出声。
            match tokio::time::timeout(STOP_WORKER_SETTLE, &mut worker).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => self.report_worker_join_failure(&error),
                Err(_elapsed) => {
                    abort.abort();
                    if let Err(error) = worker.await {
                        self.report_worker_join_failure(&error);
                    }
                }
            }
        }
        self.teardown_subscription();
        let mut state = lock(&self.state);
        state.lifecycle = ParticipantLifecycle::Stopped;
        state.terminal_pending = false;
        self.publish_lifecycle(ParticipantLifecycle::Stopped);
        Ok(())
    }

    async fn wait_for_stop_cleanup(&self) {
        let mut receiver = self.stop_cleanup.subscribe();
        loop {
            if *receiver.borrow() {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }

    fn rollback_startup_failure(&self, error: &ParticipantRuntimeError) {
        self.admission_cancelled.store(true, Ordering::Release);
        self.event_queue.close_admission();
        let cleanup = {
            let _serial = lock(&self.admission_serial);
            let mut state = lock(&self.state);
            if state.lifecycle != ParticipantLifecycle::Running {
                return;
            }
            state.lifecycle = ParticipantLifecycle::Faulted;
            self.publish_lifecycle(ParticipantLifecycle::Faulted);
            let mut cleanup = Cleanup::required();
            merge_active_cleanup(&mut cleanup, take_cleanup(&mut state));
            state.generation = state.generation.saturating_add(1);
            state.scope = None;
            state.terminal_pending = false;
            self.publish_generation(state.generation);
            self.fact_owner
                .bind_scope(state.generation, state.scope.clone());
            cleanup
        };
        self.perform_cleanup(
            cleanup,
            "participant_startup_failed",
            AgentError::new(AgentErrorCode::ScopeInvalid, handler_code(error)),
        );
        self.frame_source.release_retained_triggers();
    }

    fn ensure_running(&self) -> Result<(), ParticipantRuntimeError> {
        if self.admission_cancelled.load(Ordering::Acquire) {
            return Err(ParticipantRuntimeError::Stopped);
        }
        match lock(&self.state).lifecycle {
            ParticipantLifecycle::Created => Err(ParticipantRuntimeError::NotStarted),
            ParticipantLifecycle::Stopped | ParticipantLifecycle::Stopping => {
                Err(ParticipantRuntimeError::Stopped)
            }
            ParticipantLifecycle::Faulted => Err(ParticipantRuntimeError::Faulted),
            ParticipantLifecycle::Running => Ok(()),
        }
    }

    fn admit_scope_for_backend(
        &self,
        event: &BackendEventEnvelope,
        scope: &ParticipantScope,
    ) -> BackendAdmission {
        let terminal = backend_event_is_terminal(event);
        let scope_invalidation = backend_event_is_scope_invalidation(event);
        let reconnect_control = backend_event_is_reconnect_control(event);
        let connection_request = backend_event_is_connection_request(event);
        let transition = backend_event_is_scope_transition(event);
        let mut cleanup = Cleanup::empty();

        let mut state = lock(&self.state);

        // A close must belong to the currently active backend attempt. A
        // duplicate or late close cannot invalidate a later reconnect.
        if scope_invalidation {
            if state.scope.as_ref() != Some(scope)
                || state.active_connection_attempt_id.as_deref()
                    != Some(event.connection_attempt_id.as_str())
            {
                return BackendAdmission::StaleIgnored;
            }
            cleanup = Cleanup::required();
            merge_active_cleanup(&mut cleanup, take_cleanup(&mut state));
            state.generation = state.generation.saturating_add(1);
            state.scope = None;
            state.closed_scope = Some(scope.clone());
            state.closed_connection_attempt_id = Some(event.connection_attempt_id.clone());
            state.active_connection_attempt_id = None;
            self.publish_generation(state.generation);
            self.fact_owner
                .bind_scope(state.generation, state.scope.clone());
            state.next_ordinal = state.next_ordinal.saturating_add(1);
            return BackendAdmission::Accepted {
                generation: state.generation,
                ordinal: state.next_ordinal,
                cleanup,
                scope_control: true,
                record_fact: false,
                terminal: false,
            };
        }

        // ReconnectScheduled is journaled through the same FIFO, but it must
        // not reopen the closed scope or become a pending fact for the next
        // epoch.
        let reconnect_after_close = reconnect_control
            && state.scope.is_none()
            && state.closed_scope.as_ref() == Some(scope)
            && state.closed_connection_attempt_id.as_deref()
                == Some(event.connection_attempt_id.as_str());
        if reconnect_after_close {
            state.next_ordinal = state.next_ordinal.saturating_add(1);
            return BackendAdmission::Accepted {
                generation: state.generation,
                ordinal: state.next_ordinal,
                cleanup,
                scope_control: true,
                record_fact: false,
                terminal: false,
            };
        }

        let reopening_after_close = connection_request
            && state.scope.is_none()
            && state.closed_scope.as_ref().is_some_and(|closed| {
                if closed.process_session_id != scope.process_session_id {
                    return true;
                }
                let epoch_advanced = scope.connection_epoch > closed.connection_epoch;
                let attempt_changed = state
                    .closed_connection_attempt_id
                    .as_deref()
                    .is_some_and(|attempt| attempt != event.connection_attempt_id);
                epoch_advanced && attempt_changed
            });
        let terminal_after_close = terminal
            && state.scope.is_none()
            && state.closed_scope.as_ref() == Some(scope)
            && state.closed_connection_attempt_id.as_deref()
                == Some(event.connection_attempt_id.as_str());

        // Until a new ConnectionRequested opens a scope, no ordinary event
        // can create one after a close. A terminal envelope is the only other
        // accepted control path.
        if state.scope.is_none()
            && state.closed_scope.is_some()
            && !reopening_after_close
            && !terminal_after_close
        {
            return BackendAdmission::StaleIgnored;
        }
        if !reopening_after_close
            && !terminal_after_close
            && scope_is_stale(&state, scope, transition)
        {
            return BackendAdmission::StaleIgnored;
        }
        if !connection_request
            && state
                .active_connection_attempt_id
                .as_deref()
                .is_some_and(|attempt| attempt != event.connection_attempt_id)
        {
            return BackendAdmission::StaleIgnored;
        }

        let changed = state.scope.as_ref() != Some(scope)
            || state
                .active_connection_attempt_id
                .as_deref()
                .is_some_and(|attempt| attempt != event.connection_attempt_id);
        if state.scope.is_some() && changed {
            cleanup = Cleanup::required();
            merge_active_cleanup(&mut cleanup, take_cleanup(&mut state));
            state.generation = state.generation.saturating_add(1);
            self.publish_generation(state.generation);
        }
        if changed {
            let previous_process = state.scope.as_ref().and_then(|current| {
                (current.process_session_id != scope.process_session_id)
                    .then(|| current.process_session_id.clone())
            });
            if let Some(previous_process) = previous_process {
                retire_process_session(&mut state, &previous_process);
            }
            if let Some(closed) = state.closed_scope.take() {
                if closed.process_session_id != scope.process_session_id {
                    retire_process_session(&mut state, &closed.process_session_id);
                }
            }
            state.closed_connection_attempt_id = None;
            state.scope = Some(scope.clone());
            state.active_connection_attempt_id = Some(event.connection_attempt_id.clone());
        } else if state.active_connection_attempt_id.is_none() {
            // Internal scope facts may establish the structural scope before
            // the first backend envelope supplies its attempt identity. Bind
            // that identity without invalidating the already admitted facts.
            state.active_connection_attempt_id = Some(event.connection_attempt_id.clone());
        }
        if terminal {
            cleanup.required = true;
            merge_active_cleanup(&mut cleanup, take_cleanup(&mut state));
            state.generation = state.generation.saturating_add(1);
            state.scope = None;
            state.active_connection_attempt_id = None;
            self.publish_generation(state.generation);
            state.lifecycle = ParticipantLifecycle::Stopping;
            state.terminal_pending = true;
            self.publish_lifecycle(ParticipantLifecycle::Stopping);
        }
        self.fact_owner
            .bind_scope(state.generation, state.scope.clone());
        state.next_ordinal = state.next_ordinal.saturating_add(1);
        let ordinal = state.next_ordinal;
        BackendAdmission::Accepted {
            generation: state.generation,
            ordinal,
            cleanup,
            scope_control: false,
            // 见 support.rs `backend_event_is_fact`：事实队列只收推送通道该有的
            // 东西，世界长什么样走视口，不从这里灌。
            record_fact: !terminal && backend_event_is_fact(event),
            terminal,
        }
    }

    fn admit_explicit_scope(
        &self,
        scope: &ParticipantScope,
        allow_same_epoch_transition: bool,
        terminal_lifecycle: Option<ParticipantLifecycle>,
    ) -> ExplicitAdmission {
        let mut cleanup = Cleanup::empty();
        let mut state = lock(&self.state);
        if scope_is_stale(&state, scope, allow_same_epoch_transition) {
            return ExplicitAdmission::StaleIgnored;
        }
        if state.scope.as_ref() != Some(scope) {
            let previous_process = state.scope.as_ref().and_then(|current| {
                (current.process_session_id != scope.process_session_id)
                    .then(|| current.process_session_id.clone())
            });
            if let Some(previous_process) = previous_process {
                retire_process_session(&mut state, &previous_process);
            }
            if let Some(closed) = state.closed_scope.take() {
                if closed.process_session_id != scope.process_session_id {
                    retire_process_session(&mut state, &closed.process_session_id);
                }
            }
            state.closed_connection_attempt_id = None;
            cleanup = Cleanup::required();
            merge_active_cleanup(&mut cleanup, take_cleanup(&mut state));
            state.generation = state.generation.saturating_add(1);
            self.publish_generation(state.generation);
        }
        state.scope = Some(scope.clone());
        if let Some(terminal_lifecycle) = terminal_lifecycle {
            cleanup.required = true;
            merge_active_cleanup(&mut cleanup, take_cleanup(&mut state));
            state.generation = state.generation.saturating_add(1);
            state.scope = None;
            self.publish_generation(state.generation);
            state.lifecycle = ParticipantLifecycle::Stopping;
            state.terminal_pending = true;
            self.publish_lifecycle(ParticipantLifecycle::Stopping);
            self.fact_owner
                .bind_scope(state.generation, state.scope.clone());
            state.next_ordinal = state.next_ordinal.saturating_add(1);
            return ExplicitAdmission::Accepted {
                generation: state.generation,
                ordinal: state.next_ordinal,
                cleanup,
                terminal_lifecycle: Some(terminal_lifecycle),
            };
        }
        self.fact_owner
            .bind_scope(state.generation, state.scope.clone());
        state.next_ordinal = state.next_ordinal.saturating_add(1);
        ExplicitAdmission::Accepted {
            generation: state.generation,
            ordinal: state.next_ordinal,
            cleanup,
            terminal_lifecycle: None,
        }
    }

    fn evaluate_backend_wake(
        &self,
        event: &BackendEventEnvelope,
        scope: &ParticipantScope,
    ) -> Result<(String, Option<PlayerChatMessage>), ParticipantRuntimeError> {
        let event_type = backend_event_type(event).to_owned();
        if backend_event_is_terminal(event) {
            return Ok((event_type, None));
        }
        let Some(chat) = as_chat_event(event) else {
            return Ok((event_type, None));
        };
        let context = self.frame_source.chat_context(scope)?;
        let Some(message) = interpret_player_chat(&chat, &context) else {
            return Ok((event_type, None));
        };
        if self.wake_registry.addresses_player_chat(&message) {
            Ok((event_type, Some(message)))
        } else {
            Ok((event_type, None))
        }
    }

    fn record_fact(&self, generation: u64, fact: ParticipantFact) {
        self.fact_owner.record(generation, fact);
    }

    fn record_pending_omission(&self, generation: u64, event_type: String) {
        self.fact_owner.record_omission(generation, event_type);
    }

    fn notify_admission_observer(&self, event_type: &str) {
        let observer = lock(&self.admission_observer).clone();
        if let Some(observer) = observer {
            observer.after_work_admitted_before_fact(event_type);
        }
    }

    /// Drains facts at the opening-frame processing boundary, rather than at
    /// wake admission.  A queued wake therefore cannot claim facts which are
    /// still observable by an active run's body observationAfter.
    fn drain_pending_facts(
        &self,
        scope: &ParticipantScope,
        generation: u64,
    ) -> Option<(Vec<ParticipantFact>, u64, Vec<String>)> {
        let _serial = lock(&self.admission_serial);
        let state = lock(&self.state);
        if state.generation != generation
            || state.lifecycle != ParticipantLifecycle::Running
            || state.scope.as_ref() != Some(scope)
        {
            return None;
        }
        drop(state);
        let batch = self.fact_owner.drain_locked(scope, generation)?;
        Some((batch.facts, batch.omitted, batch.omitted_types))
    }

    fn enqueue_work(
        &self,
        item: WorkItem,
        serial: &mut AdmissionSerialGuard<'_>,
    ) -> Result<QueueAdmission, ParticipantRuntimeError> {
        self.event_queue
            .enqueue(item, serial, |item| self.admission_item_is_current(item))
    }

    fn invalidate_generation(&self) -> Cleanup {
        let mut state = lock(&self.state);
        let mut cleanup = Cleanup::required();
        merge_active_cleanup(&mut cleanup, take_cleanup(&mut state));
        state.generation = state.generation.saturating_add(1);
        state.scope = None;
        state.terminal_pending = false;
        self.publish_generation(state.generation);
        self.fact_owner
            .bind_scope(state.generation, state.scope.clone());
        cleanup
    }

    fn perform_cleanup(&self, cleanup: Cleanup, reason: &str, cancellation_error: AgentError) {
        if !cleanup.required {
            return;
        }
        let _cleanup_serial = lock(&self.cleanup_serial);
        if let Some(cancellation) = cleanup.cancellation {
            cancellation.cancel(cancellation_error);
        }
        if let Some(start_gate) = cleanup.start_gate {
            start_gate.open();
        }
        if let Some(abort) = cleanup.abort {
            abort.abort();
        }
        self.speech.cancel_remaining(reason);
        match self.backend.motor() {
            Ok(motor) => {
                if let Err(error) = motor.release_all() {
                    self.fail_runtime_sync(
                        ParticipantFailureSource::BodyRelease,
                        "body_release_failed",
                        "body release failed",
                        None,
                    );
                    let _ = error;
                }
            }
            Err(_error) => self.fail_runtime_sync(
                ParticipantFailureSource::BodyRelease,
                "body_motor_unavailable",
                "body motor unavailable during release",
                None,
            ),
        }
        self.debug.update(DebugStateUpdate {
            body: Some(None),
            current_body_tool: Some(None),
            ..DebugStateUpdate::default()
        });
    }

    fn fail_runtime_sync(
        &self,
        source: ParticipantFailureSource,
        code: &str,
        summary: &str,
        scope: Option<ParticipantScope>,
    ) {
        let failure = ParticipantFailure {
            source,
            code: code.to_owned(),
            summary: bounded_summary(summary),
            scope: scope.clone(),
        };
        let _ = self.failures.send(failure.clone());
        self.debug.failure(DebugFailureSummary {
            at: self.clock.now(),
            source: match &failure.source {
                ParticipantFailureSource::Backend => DebugFailureSource::Backend,
                ParticipantFailureSource::Source => DebugFailureSource::Runtime,
                ParticipantFailureSource::Journal => DebugFailureSource::Runtime,
                ParticipantFailureSource::Model => DebugFailureSource::Model,
                ParticipantFailureSource::Runtime => DebugFailureSource::Runtime,
                ParticipantFailureSource::BodyRelease => DebugFailureSource::BodyTool,
            },
            code: failure.code,
            summary: failure.summary,
        });
    }

    /// worker 任务非正常收场时出声。
    ///
    /// 取消不算失败——唯一的取消来自上面超时后我们自己发的 `abort()`，把它记成
    /// 缺陷会让每一次收不干净的停机都伪装成 panic。除此之外只剩 panic，那是缺
    /// 陷：worker 死了意味着此后没有任何唤醒被处理，必须留下痕迹。
    fn report_worker_join_failure(&self, error: &JoinError) {
        if error.is_cancelled() {
            return;
        }
        self.fail_runtime_sync(
            ParticipantFailureSource::Runtime,
            "participant_worker_panicked",
            &format!("participant worker panicked: {error}"),
            None,
        );
    }

    /// 事件入队路径的失败。与 worker 路径分开命名：前者会打死整个 runtime，
    /// 后者按 is_recoverable_wake_error 分类，排障时必须一眼可辨。
    fn report_admission_error(&self, error: ParticipantRuntimeError) {
        self.fail_runtime_sync(
            failure_source(&error),
            &format!("ingest:{}", handler_code(&error)),
            &handler_summary(&error),
            self.current_scope(),
        );
        self.journal_failure_detached(&error);
        self.mark_faulted_after_handler();
    }

    /// 单轮模型失败的落盘：与 runtime 级 participant.failure 分开，
    /// 便于事后区分「这一轮没成」与「同伴已经不再响应」。
    /// 记「这一轮的决定已经完成」。与 `model.failed` 成对，事后可区分
    /// 「这一轮做完了」「这一轮没成」「同伴不再响应」三种情况。
    async fn journal_decision_completed(&self, run_id: &RunId) {
        let payload = json!({ "runId": run_id.to_string() });
        if let Some(payload) = payload.as_object().cloned() {
            let _ = self
                .journal
                .append("model.decision.completed".to_owned(), payload)
                .await;
        }
    }

    fn journal_model_failure_detached(&self, summary: &str) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let journal = Arc::clone(&self.journal);
        let summary = bounded_summary(summary);
        handle.spawn(async move {
            let payload = json!({
                "code": "decision_failed",
                "summary": summary,
            });
            if let Some(payload) = payload.as_object().cloned() {
                let _ = journal.append("model.failed".to_owned(), payload).await;
            }
        });
    }

    fn journal_failure_detached(&self, error: &ParticipantRuntimeError) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let journal = Arc::clone(&self.journal);
        let code = handler_code(error).to_owned();
        handle.spawn(async move {
            let payload = json!({
                "code": code,
                "summary": "participant handler failure",
            });
            if let Some(payload) = payload.as_object().cloned() {
                let _ = journal
                    .append("participant.failure".to_owned(), payload)
                    .await;
            }
        });
    }

    fn is_normal_admission_race(&self, error: &ParticipantRuntimeError) -> bool {
        matches!(
            error,
            ParticipantRuntimeError::NotStarted
                | ParticipantRuntimeError::Stopped
                | ParticipantRuntimeError::Faulted
        ) || (matches!(error, ParticipantRuntimeError::QueueClosed)
            && (self.admission_cancelled.load(Ordering::Acquire)
                || self.lifecycle() != ParticipantLifecycle::Running))
    }

    async fn worker_loop(self: Arc<Self>) {
        loop {
            if self.worker_should_exit() {
                self.teardown_subscription();
                return;
            }
            let Some(item) = self.event_queue.next().await else {
                self.teardown_subscription();
                return;
            };
            if let Err(error) = self.process_item(item).await {
                self.fail_runtime_sync(
                    failure_source(&error),
                    handler_code(&error),
                    &handler_summary(&error),
                    self.current_scope(),
                );
                let _ = self.journal_failure(&error).await;
                if !is_recoverable_wake_error(&error) {
                    self.mark_faulted_after_handler();
                    self.teardown_subscription();
                    return;
                }
            }
        }
    }

    async fn process_item(&self, item: WorkItem) -> Result<(), ParticipantRuntimeError> {
        if !self.item_is_current(&item) {
            self.release_item_trigger(&item);
            return Ok(());
        }
        // 闸门放在原来 journal await 的位置：那里曾是 worker 唯一的逐条
        // 停顿点，饱和测试依赖的就是这个位置。
        self.worker_gate.pass().await;
        if let Err(error) = self.append_event_journal(&item).await {
            self.release_item_trigger(&item);
            return Err(error);
        }
        if !self.item_is_current(&item) {
            self.release_item_trigger(&item);
            return Ok(());
        }
        if item.terminal {
            let terminal_lifecycle = item.terminal_lifecycle.ok_or_else(|| {
                ParticipantRuntimeError::Handler("terminal item missing lifecycle".to_owned())
            })?;
            self.finish_terminal(terminal_lifecycle);
            return Ok(());
        }
        if item.wake.is_none() {
            return Ok(());
        }
        self.process_wake(item).await
    }

    fn release_item_trigger(&self, item: &WorkItem) {
        if let Some(wake) = item.wake.as_ref().filter(|wake| wake.trigger_retained) {
            self.frame_source
                .release_trigger(&wake.scope, &wake.trigger);
        }
    }

    async fn process_wake(&self, item: WorkItem) -> Result<(), ParticipantRuntimeError> {
        let wake = item
            .wake
            .as_ref()
            .ok_or_else(|| ParticipantRuntimeError::Handler("wake item missing".to_owned()))?;
        let capture_result = self.frame_source.capture(&wake.scope);
        if wake.trigger_retained {
            self.frame_source
                .release_trigger(&wake.scope, &wake.trigger);
        }
        let capture = capture_result?;
        let memory = self.read_memory(&item.generation).await?;
        if !self.generation_is_current(item.generation) {
            return Ok(());
        }
        let Some((facts, omitted, omitted_types)) =
            self.drain_pending_facts(&wake.scope, item.generation)
        else {
            return Ok(());
        };
        let context = self.assemble_frame(wake, capture, memory, facts, omitted, omitted_types)?;
        let run_id = RunId::new(format!(
            "p-{}-{}-{}-{}-{}",
            self.run_id_namespace_digest,
            self.run_id_instance_id,
            base36_u64(wake.scope.connection_epoch),
            base36_u64(item.generation),
            base36_u64(wake.ordinal),
        ))
        .map_err(|error| ParticipantRuntimeError::Handler(error.to_owned()))?;
        let request = AgentRunRequest {
            run_id: run_id.clone(),
            context,
            tools: self.agent.definitions(),
            prompt_template: self.prompt_template.clone(),
        };
        request
            .validate()
            .map_err(|error| ParticipantRuntimeError::Handler(error.to_string()))?;

        let deadline = Deadline::after(Instant::now(), self.run_deadline)
            .map_err(|error| ParticipantRuntimeError::Handler(error.to_string()))?;
        let cancellation = ParticipantCancellation::new();
        let start_gate = ParticipantStartGate::new();
        if !self.publish_active(
            item.generation,
            Arc::clone(&cancellation),
            Arc::clone(&start_gate),
        ) {
            return Ok(());
        }
        let agent = Arc::clone(&self.agent);
        let task_cancellation = Arc::clone(&cancellation);
        let task_start_gate = Arc::clone(&start_gate);
        let run_scope = wake.scope.clone();
        let run_generation = item.generation;
        let trigger_event_id = item.event_id.clone();
        let task: JoinHandle<Result<mineintent_contracts::agent::ModelRunResult, AgentError>> =
            tokio::spawn(async move {
                let start = tokio::select! {
                    biased;
                    error = task_cancellation.cancelled() => Err(error),
                    _ = task_start_gate.wait() => Ok(()),
                };
                start?;
                if let Some(error) = task_cancellation.cancellation_error() {
                    return Err(error);
                }
                let control = ExecutionControl::new(task_cancellation.as_ref(), deadline);
                agent
                    .run(
                        run_scope,
                        run_generation,
                        trigger_event_id,
                        request,
                        control,
                    )
                    .await
            });
        let abort = task.abort_handle();
        if !self.attach_active_abort(item.generation, &cancellation, abort.clone()) {
            cancellation.cancel(AgentError::new(
                AgentErrorCode::ScopeInvalid,
                "participant_scope_changed",
            ));
            start_gate.open();
            abort.abort();
            return Ok(());
        }
        start_gate.open();
        self.debug.update(DebugStateUpdate {
            decision: Some(Some(DebugDecision {
                status: DebugDecisionStatus::Running,
                run_id: Some(run_id.to_string()),
                model: None,
                started_at: Some(wake.occurred_at.clone()),
                context_sources: Vec::new(),
                retrieved_memory_ids: Vec::new(),
            })),
            ..DebugStateUpdate::default()
        });

        let result = tokio::select! {
            biased;
            _ = self.stopping_changed() => {
                cancellation.cancel(AgentError::run_cancelled());
                abort.abort();
                return Ok(());
            }
            _ = self.generation_changed(item.generation) => {
                cancellation.cancel(AgentError::new(AgentErrorCode::ScopeInvalid, "participant_scope_changed"));
                abort.abort();
                return Ok(());
            }
            _ = cancellation.cancelled() => {
                abort.abort();
                return Ok(());
            }
            joined = task => joined.map_err(|error| ParticipantRuntimeError::Handler(error.to_string()))?,
        };
        self.clear_active(item.generation, &cancellation);
        match result {
            Ok(_) => {
                self.debug.update(DebugStateUpdate {
                    decision: Some(Some(DebugDecision::idle())),
                    ..DebugStateUpdate::default()
                });
                // 「这一轮做完了决定」是产品事实，oracle runtime.ts:303 有、
                // Rust 侧一直缺。落盘失败不能把这一轮判失败——决定已经做出
                // 并且副作用已经发生，记账失败只该记账。
                self.journal_decision_completed(&run_id).await;
                Ok(())
            }
            Err(error) if is_normal_agent_error(&error) => {
                self.debug.update(DebugStateUpdate {
                    decision: Some(Some(DebugDecision::idle())),
                    ..DebugStateUpdate::default()
                });
                Ok(())
            }
            Err(error) => {
                self.debug.update(DebugStateUpdate {
                    decision: Some(Some(DebugDecision {
                        status: DebugDecisionStatus::Failed,
                        run_id: Some(run_id.to_string()),
                        model: None,
                        started_at: Some(wake.occurred_at.clone()),
                        context_sources: Vec::new(),
                        retrieved_memory_ids: Vec::new(),
                    })),
                    ..DebugStateUpdate::default()
                });
                // 模型侧失败终结的是这一轮，不是这个同伴（oracle
                // runtime.ts:311-314 同形：catch 住、记 model.decision_failed、
                // 继续接受下一次唤醒）。provider 的一次抖动不得让同伴永久失聪。
                let summary = format!("{}: {}", error.code, error.summary);
                // 终结的是这一轮，不是这个同伴——warn 而非 error，并把话说明白，
                // 免得读日志的人以为同伴已经死了。
                tracing::warn!(
                    target: "mineintent_middle",
                    code = %error.code,
                    summary = %error.summary,
                    "模型这一轮失败；本轮结束，同伴继续接受下一次唤醒"
                );
                self.fail_runtime_sync(
                    ParticipantFailureSource::Model,
                    "decision_failed",
                    &summary,
                    self.current_scope(),
                );
                self.journal_model_failure_detached(&summary);
                Ok(())
            }
        }
    }

    async fn append_event_journal(&self, item: &WorkItem) -> Result<(), ParticipantRuntimeError> {
        let Some(journal_type) = journal_type_for(item) else {
            // 可重建普通事实只计数。注意这里同时去掉了它们的 await：
            // 原实现每条摄入事件都要等一次 journal 落盘，这是 NEW-11
            // 有界 admission 设计中「journal 长期阻塞」那条假设的来源。
            self.ingest_counters.record(&item.event_type);
            return Ok(());
        };
        let payload = event_payload(item);
        let future = self.journal.append(journal_type.to_owned(), payload);
        tokio::pin!(future);
        if item.wake.is_none() && item.scope_control {
            return future
                .await
                .map_err(|error| ParticipantRuntimeError::Handler(error.to_string()));
        }
        tokio::select! {
            biased;
            _ = self.generation_changed(item.generation) => Ok(()),
            result = &mut future => result.map_err(|error| ParticipantRuntimeError::Handler(error.to_string())),
        }
    }

    async fn journal_failure(&self, error: &ParticipantRuntimeError) -> Result<(), ()> {
        let payload =
            json!({"code": handler_code(error), "summary": "participant handler failure"});
        let Some(payload) = payload.as_object().cloned() else {
            return Err(());
        };
        self.journal
            .append("participant.failure".to_owned(), payload)
            .await
            .map_err(|_| ())
    }

    async fn read_memory(&self, generation: &u64) -> Result<String, ParticipantRuntimeError> {
        let future = self.memory.read_full();
        tokio::pin!(future);
        tokio::select! {
            biased;
            _ = self.stopping_changed() => Ok(String::new()),
            _ = self.generation_changed(*generation) => Ok(String::new()),
            result = &mut future => result.map_err(ParticipantRuntimeError::Memory),
        }
    }

    /// 装配一轮决策的开场帧。**每次唤醒调用一次**，不是每进程一次——
    /// 调用点就在 `process_wake` 里。误读成「只在最初装配一次」会得出
    /// 「装配失败只影响启动」的错误结论；实际上它决定每一次唤醒能否成事。
    fn assemble_frame(
        &self,
        wake: &WakeItem,
        mut capture: ParticipantFrameCapture,
        memory: String,
        facts: Vec<ParticipantFact>,
        omitted: u64,
        omitted_types: Vec<String>,
    ) -> Result<JsonAgentDecisionContextV5, ParticipantRuntimeError> {
        // 纪律：可观察量缺席不构成装配失败。
        //
        // 这里曾经对 light 做 fail-closed，代价是同伴一死就永久失能——死后
        // 服务端收回全部区块，光照必然读不到，于是唤醒到得了、准入过得了，
        // 却卡在一个死亡期间本就不可能存在的事实上，而唯一的自救动作
        // （respawn）只能由模型发起，模型又永远等不到这一帧。
        //
        // 下面保留的三项都不是「观察缺席」：维度为空和维度与作用域不符是
        // 帧本身无效，触发聊天不在未读窗口内是唤醒的前提不成立。二者与
        // 「这一轮没看到某个量」是不同性质的问题，仍然必须失败。
        if capture.dimension.is_empty() {
            return Err(ParticipantSourceError::Invalid(
                "frame dimension must not be empty".to_owned(),
            )
            .into());
        }
        if wake.scope.dimension.as_deref() != Some(capture.dimension.as_str()) {
            return Err(ParticipantSourceError::Invalid(
                "frame dimension does not match event scope".to_owned(),
            )
            .into());
        }
        if let Some(status) = capture.status.as_mut() {
            if status.armor == Some(0) {
                status.armor = None;
            }
        }
        let Some(trigger_chat) = capture
            .unread_chat
            .iter()
            .find(|chat| {
                chat.message.username == wake.trigger.sender.username
                    && chat.message.text == wake.trigger.text
                    && chat.message.at == wake.trigger.occurred_at
            })
            .cloned()
        else {
            return Err(AgentContextV5AssemblyError::TriggerChatNotInUnreadWindow.into());
        };
        let trigger_sequence = trigger_chat.sequence;
        let trigger_message = trigger_chat.message;
        let duplicate_count = capture
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    AgentContextV5EventInput::PlayerChat {
                        sequence,
                        message,
                    } if *sequence == trigger_sequence
                        && same_chat_identity(message, &trigger_message)
                )
            })
            .count();
        if duplicate_count > 1 {
            return Err(AgentContextV5AssemblyError::DuplicatePlayerChatEvent.into());
        }
        if duplicate_count == 1 {
            capture.events.retain(|event| {
                !matches!(
                    event,
                    AgentContextV5EventInput::PlayerChat {
                        sequence,
                        message,
                    } if *sequence == trigger_sequence
                        && same_chat_identity(message, &trigger_message)
                )
            });
        }
        let mut events = capture.events;
        for fact in &facts {
            if fact.scope != wake.scope {
                continue;
            }
            events.push(AgentContextV5EventInput::Summary {
                event_type: safe_fact_event_type(&fact.event_type),
                summary: fact.summary.clone(),
            });
        }
        if omitted > 0 {
            let omitted_types = if omitted_types.is_empty() {
                String::new()
            } else {
                format!("; types={}", omitted_types.join(","))
            };
            events.push(AgentContextV5EventInput::Summary {
                event_type: "participant_events_omitted".to_owned(),
                summary: format!("{} pending events omitted{}", omitted, omitted_types),
            });
        }
        self.assembler
            .assemble(AgentContextV5Input {
                memory,
                at: capture.at,
                dimension: capture.dimension,
                pose: capture.pose,
                status: capture.status,
                hotbar: capture.hotbar,
                unread_chat: capture.unread_chat,
                unread_chat_omitted: capture.unread_chat_omitted,
                sound: capture.sound,
                light: capture.light,
                events,
                omissions: capture.omissions,
                trigger_chat: Some(AgentChatTriggerV5 {
                    sequence: trigger_sequence,
                    message: trigger_message,
                }),
            })
            .map_err(ParticipantRuntimeError::Frame)
    }

    fn item_is_current(&self, item: &WorkItem) -> bool {
        let state = lock(&self.state);
        item.terminal
            || (state.lifecycle == ParticipantLifecycle::Running
                && item.wake.is_none()
                && item.scope_control)
            || (state.generation == item.generation
                && state.lifecycle == ParticipantLifecycle::Running
                && state.scope.as_ref() == Some(&item.scope))
    }

    fn admission_item_is_current(&self, item: &WorkItem) -> bool {
        let state = lock(&self.state);
        item.terminal
            || (state.lifecycle == ParticipantLifecycle::Running
                && item.wake.is_none()
                && item.scope_control)
            || (state.lifecycle == ParticipantLifecycle::Running
                && state.generation == item.generation
                && state.scope.as_ref() == Some(&item.scope))
    }

    fn generation_is_current(&self, generation: u64) -> bool {
        lock(&self.state).generation == generation
    }

    async fn generation_changed(&self, generation: u64) {
        let mut receiver = self.generation.subscribe();
        loop {
            if *receiver.borrow() != generation {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }

    fn publish_generation(&self, generation: u64) {
        self.generation.send_replace(generation);
    }

    fn publish_lifecycle(&self, lifecycle: ParticipantLifecycle) {
        self.lifecycle_signal.send_replace(lifecycle);
    }

    async fn stopping_changed(&self) {
        let mut receiver = self.lifecycle_signal.subscribe();
        if self.is_stopping() {
            return;
        }
        let _ = receiver.changed().await;
    }

    fn worker_should_exit(&self) -> bool {
        let state = lock(&self.state);
        matches!(
            state.lifecycle,
            ParticipantLifecycle::Stopped | ParticipantLifecycle::Faulted
        ) || (state.lifecycle == ParticipantLifecycle::Stopping && !state.terminal_pending)
    }

    fn finish_terminal(&self, terminal_lifecycle: ParticipantLifecycle) {
        self.teardown_subscription();
        self.admission_cancelled.store(true, Ordering::Release);
        self.event_queue.close_admission();
        self.frame_source.release_retained_triggers();
        let mut state = lock(&self.state);
        if state.lifecycle == ParticipantLifecycle::Stopping && state.terminal_pending {
            state.lifecycle = terminal_lifecycle;
            state.terminal_pending = false;
            self.publish_lifecycle(terminal_lifecycle);
        }
    }

    fn teardown_subscription(&self) {
        if let Some(mut subscription) = lock(&self.subscription).take() {
            subscription.unsubscribe();
        }
    }

    fn publish_active(
        &self,
        generation: u64,
        cancellation: Arc<ParticipantCancellation>,
        start_gate: Arc<ParticipantStartGate>,
    ) -> bool {
        let mut state = lock(&self.state);
        if state.generation != generation || state.lifecycle != ParticipantLifecycle::Running {
            return false;
        }
        state.active = Some(ActiveRun {
            cancellation,
            abort: None,
            start_gate,
        });
        true
    }

    fn attach_active_abort(
        &self,
        generation: u64,
        cancellation: &Arc<ParticipantCancellation>,
        abort: AbortHandle,
    ) -> bool {
        let mut state = lock(&self.state);
        if state.generation != generation || state.lifecycle != ParticipantLifecycle::Running {
            return false;
        }
        let Some(active) = state.active.as_mut() else {
            return false;
        };
        if !Arc::ptr_eq(&active.cancellation, cancellation) {
            return false;
        }
        active.abort = Some(abort);
        true
    }

    fn clear_active(&self, generation: u64, cancellation: &Arc<ParticipantCancellation>) {
        let mut state = lock(&self.state);
        if state.generation == generation
            && state
                .active
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(&active.cancellation, cancellation))
        {
            state.active = None;
        }
    }

    fn is_stopping(&self) -> bool {
        matches!(
            lock(&self.state).lifecycle,
            ParticipantLifecycle::Stopping
                | ParticipantLifecycle::Stopped
                | ParticipantLifecycle::Faulted
        )
    }

    fn mark_faulted_after_handler(&self) {
        self.admission_cancelled.store(true, Ordering::Release);
        self.event_queue.close_admission();
        let cleanup = {
            let _serial = lock(&self.admission_serial);
            let mut state = lock(&self.state);
            if state.lifecycle == ParticipantLifecycle::Stopping
                || state.lifecycle == ParticipantLifecycle::Stopped
            {
                return;
            }
            state.lifecycle = ParticipantLifecycle::Faulted;
            self.publish_lifecycle(ParticipantLifecycle::Faulted);
            let mut cleanup = Cleanup::required();
            merge_active_cleanup(&mut cleanup, take_cleanup(&mut state));
            state.generation = state.generation.saturating_add(1);
            state.scope = None;
            state.terminal_pending = false;
            self.publish_generation(state.generation);
            self.fact_owner
                .bind_scope(state.generation, state.scope.clone());
            cleanup
        };
        self.perform_cleanup(
            cleanup,
            "participant_handler_failed",
            AgentError::new(AgentErrorCode::ScopeInvalid, "participant_handler_failed"),
        );
        self.frame_source.release_retained_triggers();
    }
}

impl<R> BackendEventListener for ParticipantRuntime<R>
where
    R: ParticipantAgentPort + 'static,
{
    fn on_event(&self, event: BackendEventEnvelope) {
        if let Err(error) = self.ingest_backend_event(event) {
            if self.is_normal_admission_race(&error) {
                return;
            }
            // 「什么算致命」必须只有一条规则：worker 路径按
            // is_recoverable_wake_error 分类，入队路径过去却把同一种瞬时错误
            // （如死亡期间的 source 读取失败）当致命，一次死亡就永久打死同伴。
            // 两处各判一套，与 oracle 注释点名的 sameScope 缺陷同型。
            if is_recoverable_wake_error(&error) {
                self.fail_runtime_sync(
                    failure_source(&error),
                    &format!("ingest:{}", handler_code(&error)),
                    &handler_summary(&error),
                    self.current_scope(),
                );
                return;
            }
            self.report_admission_error(error);
        }
    }
}

#[derive(Clone)]
struct ParticipantStartGate {
    opened: watch::Sender<bool>,
}

impl ParticipantStartGate {
    fn new() -> Arc<Self> {
        let (opened, _) = watch::channel(false);
        Arc::new(Self { opened })
    }

    fn open(&self) {
        self.opened.send_replace(true);
    }

    async fn wait(&self) {
        let mut receiver = self.opened.subscribe();
        loop {
            if *receiver.borrow() {
                return;
            }
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

struct ParticipantCancellation {
    error: watch::Sender<Option<AgentError>>,
}

impl ParticipantCancellation {
    fn new() -> Arc<Self> {
        let (error, _) = watch::channel(None);
        Arc::new(Self { error })
    }

    fn cancel(&self, error: AgentError) {
        let _ = self.error.send_if_modified(|current| {
            if current.is_none() {
                *current = Some(error.clone());
                true
            } else {
                false
            }
        });
    }
}

impl CancellationSignal for ParticipantCancellation {
    fn cancellation_error(&self) -> Option<AgentError> {
        self.error.borrow().clone()
    }

    fn cancelled(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = AgentError> + Send + '_>> {
        Box::pin(async move {
            let mut receiver = self.error.subscribe();
            loop {
                if let Some(error) = receiver.borrow().clone() {
                    return error;
                }
                if receiver.changed().await.is_err() {
                    return AgentError::run_cancelled();
                }
            }
        })
    }
}

fn take_cleanup(state: &mut RuntimeState) -> Cleanup {
    state
        .active
        .take()
        .map_or_else(Cleanup::empty, |active| Cleanup {
            required: true,
            cancellation: Some(active.cancellation),
            abort: active.abort,
            start_gate: Some(active.start_gate),
        })
}
