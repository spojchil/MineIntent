use std::{
    collections::{HashMap, VecDeque},
    error::Error,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use azalea::{
    accept_resource_packs::AcceptResourcePacksPlugin,
    app::{App, AppExit, Plugin, PluginGroup, PostUpdate, Update},
    auto_reconnect::AutoReconnectPlugin,
    auto_respawn::AutoRespawnPlugin,
    bot::DefaultBotPlugins,
    ecs::{
        message::{MessageReader, MessageWriter},
        prelude::{Add, Commands, On, Query, With},
        system::Res,
    },
    entity::{dimensions::EntityDimensions, Dead, LocalEntity, Physics, Position},
    prelude::{bevy_ecs, Account, Component, Resource},
    protocol::address::{ResolvedAddr, ServerAddr},
    swarm::{DefaultSwarmPlugins, Swarm, SwarmBuilder, SwarmEvent},
    Client, DefaultPlugins, Event, SprintDirection, WalkDirection,
};
use bevy_ecs::schedule::IntoScheduleConfigs;
use mineintent_contracts::capability::validate_directed_positions;
use mineintent_contracts::minecraft::{
    parse_block_property_value, BackendClose, BackendCloseError, BackendError,
    BackendEventEnvelope as ContractBackendEventEnvelope,
    BackendEventKind as ContractBackendEventKind,
    BackendEventMetadata as ContractBackendEventMetadata, BackendEventPayload, BackendFailure,
    BackendFailureCode, BackendKick, BackendLifecyclePayload, BackendState, BackendTimeouts,
    BlockBoundingBox as ContractBlockBoundingBox, BlockPosition as ContractBlockPosition,
    BlockReadResult as ContractBlockReadResult, BoxFuture, ChatPosition, DirectedViewportError,
    DirectedViewportProjection, EntityEquipmentSnapshot as ContractEntityEquipmentSnapshot,
    EntityRemovalReason as ContractEntityRemovalReason, FactSource as ContractFactSource,
    HeardSoundType as ContractHeardSoundType, ObservationEvent, ObservationEventListener,
    OperationControl, ProtocolBlockEvent as ContractProtocolBlockEvent,
    ProtocolBlockSnapshot as ContractProtocolBlockSnapshot,
    ProtocolChatEvent as ContractProtocolChatEvent,
    ProtocolEntityEvent as ContractProtocolEntityEvent,
    ProtocolEntitySnapshot as ContractProtocolEntitySnapshot, ProtocolObservationSource,
    ProtocolPlayerListEvent as ContractProtocolPlayerListEvent,
    ProtocolSelfEvent as ContractProtocolSelfEvent,
    ProtocolSnapshotChangedEvent as ContractProtocolSnapshotChangedEvent,
    ProtocolSoundPayload as ContractProtocolSoundPayload,
    ProtocolSoundSource as ContractProtocolSoundSource, ReconnectPolicy, RelativeMovementFlags,
    SelfPose as ContractSelfPose, Subscription, Vec3Value as ContractVec3Value,
    ViewportBlock as ContractViewportBlock, ViewportFrame as ContractViewportFrame,
    ViewportLegend as ContractViewportLegend, ViewportProjection as ContractViewportProjection,
    ViewportRead as ContractViewportRead, ViewportSelfPose as ContractViewportSelfPose,
    VisibleBlocksView as ContractVisibleBlocksView,
    VisibleEntitiesView as ContractVisibleEntitiesView,
    VisibleEntityView as ContractVisibleEntityView,
};
use tokio::sync::{oneshot, Notify};

use crate::{
    entity_events::{
        compact_pitch_radians, compact_rotation_radians, EntityIdentity, EntityMovePatch,
        EntityProducerCache, EntityProducerInput, EntityProducerToken, NormalizedEntityEvent,
        NormalizedEntitySnapshot,
    },
    protocol::{
        now_utc, BackendCommand, BackendCommandEnvelope, BackendEventEnvelope, BackendEventKind,
        FactSource, MotorDirection, BACKEND_COMMAND_PROTOCOL,
    },
    snapshot::{
        block_snapshot, canonical_entity_type, capture, BlockBoundingBox, BlockPosition,
        BlockReadResult, MinecraftSnapshotV1, PoseSnapshot, ProtocolBlockSnapshot,
        ProtocolEntitySnapshot, TrackedPlayerSnapshot, Vec3Value,
    },
    viewport::{
        project as project_viewport, project_directed as project_directed_viewport,
        project_with_checkpoint as project_viewport_with_checkpoint, ViewportBlock,
        ViewportOptions, ViewportProjection, WorldHeightBounds,
    },
};

#[derive(Clone, Debug)]
pub struct RunConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub world_id: String,
    pub duration: Duration,
    /// 传输阶段 deadline；与上层 `OperationControl` 的 deadline 相互独立。
    pub timeouts: BackendTimeouts,
    /// 传输级指数重连策略，字段与冻结 contracts DTO 一一对应。
    pub reconnect: ReconnectPolicy,
    /// The diagnostic CLI uses a finite duration; a composition root owns the
    /// backend lifecycle and disables this timer for a production facade.
    pub auto_stop: bool,
    /// The diagnostic CLI exposes the event stream on stdout.  In-process
    /// composition roots consume the same events through `subscribe` and keep
    /// this boundary silent.
    pub emit_stdout: bool,
    /// 仅用于本地验收 M2；正式集成通过 `RuntimeHandle::send_chat`。
    pub initial_chat: Option<String>,
}

/// 服务端先发送死亡/生命值更新、再在同一 tick 设置 waitingForRespawn；显式
/// respawn 若紧贴 DeathEvent 发出会落在这段窗口里。这个延迟只作用于上层已经
/// 明确请求的重生，不会在死亡事件上自动创建请求。
const RESPAWN_SETTLE_DELAY: Duration = Duration::from_millis(100);

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: 25565,
            username: "MineIntentBot".to_owned(),
            world_id: "paper-local-world".to_owned(),
            duration: Duration::from_secs(30),
            timeouts: BackendTimeouts {
                connect_ms: 10_000,
                login_ms: 20_000,
                spawn_ms: 30_000,
                stop_ms: 5_000,
            },
            reconnect: ReconnectPolicy {
                enabled: true,
                initial_delay_ms: 1_000,
                multiplier: 2.0,
                max_delay_ms: 30_000,
                jitter_ratio: 0.2,
                stable_reset_ms: 60_000,
            },
            auto_stop: true,
            emit_stdout: true,
            initial_chat: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RetrySchedule {
    delay: Duration,
    retry_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransportPhase {
    Connecting,
    LoggingIn,
    Spawning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PhaseDeadlineToken {
    epoch: u64,
    attempt: u64,
    phase: TransportPhase,
    generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StableResetToken {
    epoch: u64,
    attempt: u64,
    generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StopWatchdogToken {
    generation: u64,
}

/// 计算 TS oracle 的重连退避。`attempt` 是当前已经开始的 retry ordinal，因而首次
/// 重连使用 exponent 0。所有浮点中间值都在转入 `Duration` 前饱和，避免极大 multiplier
/// 或 ordinal 造成 panic/wrap。
fn reconnect_schedule_at(
    policy: &ReconnectPolicy,
    attempt: u64,
    random: f64,
    now: chrono::DateTime<chrono::Utc>,
) -> RetrySchedule {
    let exponent = attempt.saturating_sub(1) as f64;
    let initial = policy.initial_delay_ms as f64;
    let maximum = policy.max_delay_ms as f64;
    let grown = initial * policy.multiplier.powf(exponent);
    let base = if !grown.is_finite() {
        maximum
    } else {
        grown.min(maximum)
    };
    let random = if random.is_finite() {
        random.clamp(0.0, 1.0)
    } else {
        0.5
    };
    let jitter = (random * 2.0 - 1.0) * policy.jitter_ratio;
    let adjusted = base * (1.0 + jitter);
    let rounded = if !adjusted.is_finite() {
        maximum
    } else {
        // All valid policy values make the intended quantity non-negative.
        // Keep the guard explicit because this helper is also the runtime
        // boundary for manually constructed RunConfig values.
        (adjusted.max(0.0) + 0.5).floor()
    };
    let delay_ms = if !rounded.is_finite() || rounded >= u64::MAX as f64 {
        u64::MAX
    } else {
        rounded as u64
    };
    let delay = Duration::from_millis(delay_ms);
    let retry_at = retry_at_with_delay(now, delay_ms);
    RetrySchedule { delay, retry_at }
}

fn retry_at_with_delay(
    now: chrono::DateTime<chrono::Utc>,
    delay_ms: u64,
) -> chrono::DateTime<chrono::Utc> {
    let chrono_ms = i64::try_from(delay_ms).unwrap_or(i64::MAX);
    now.checked_add_signed(chrono::Duration::milliseconds(chrono_ms))
        .unwrap_or(chrono::DateTime::<chrono::Utc>::MAX_UTC)
}

fn next_reconnect_random(seed: &AtomicU64) -> f64 {
    // A small local seam is sufficient for jitter; it avoids adding a new
    // manifest dependency and can be replaced by a deterministic test seed.
    let mut current = seed.load(Ordering::Relaxed);
    loop {
        let next = current
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        match seed.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => return (next >> 11) as f64 / (1u64 << 53) as f64,
            Err(observed) => current = observed,
        }
    }
}

fn checked_atomic_increment(counter: &AtomicU64) -> Option<u64> {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let next = current.checked_add(1)?;
        match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Some(next),
            Err(observed) => current = observed,
        }
    }
}

struct EventWriter {
    next_id: u64,
    process_session_id: String,
    connection_epoch: u64,
    connection_attempt_id: String,
    world_id: String,
    dimension: Option<String>,
}

impl EventWriter {
    fn new(world_id: &str) -> Self {
        Self {
            next_id: 0,
            process_session_id: format!(
                "pid-{}-{}",
                std::process::id(),
                now_utc().timestamp_millis()
            ),
            connection_epoch: 0,
            connection_attempt_id: "attempt-0".to_owned(),
            world_id: world_id.to_owned(),
            dimension: None,
        }
    }

    fn new_attempt(&mut self, epoch: u64) {
        self.connection_epoch = epoch;
        self.connection_attempt_id = format!("attempt-{}", epoch);
        self.dimension = None;
    }

    fn set_dimension(&mut self, dimension: impl Into<String>) {
        self.dimension = Some(dimension.into());
    }

    fn context(&self) -> (String, u64, String) {
        (
            self.process_session_id.clone(),
            self.connection_epoch,
            self.connection_attempt_id.clone(),
        )
    }

    fn emit(&mut self, source: FactSource, payload: BackendEventPayload) -> BackendEventEnvelope {
        self.emit_at(source, payload, now_utc().to_rfc3339())
    }

    fn emit_at(
        &mut self,
        source: FactSource,
        payload: BackendEventPayload,
        occurred_at: String,
    ) -> BackendEventEnvelope {
        self.next_id += 1;
        BackendEventEnvelope::from_payload(
            mineintent_contracts::minecraft::BackendEventMetadata {
                id: format!("event-{}", self.next_id),
                occurred_at,
                process_session_id: self.process_session_id.clone(),
                connection_epoch: self.connection_epoch,
                connection_attempt_id: self.connection_attempt_id.clone(),
                world_id: self.world_id.clone(),
                dimension: self.dimension.clone(),
            },
            source,
            payload,
        )
    }
}

struct EventDispatchState {
    ordinary: VecDeque<RuntimeDispatchEntry>,
    control: VecDeque<RuntimeDispatchEntry>,
    overflow: VecDeque<RuntimeDispatchOverflow>,
    terminal: Option<RuntimeDispatchEntry>,
    next_sequence: u64,
    next_admission: u64,
    open_loss_segment: Option<u64>,
    drainer_active: bool,
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

struct RuntimeDispatchEntry {
    sequence: u64,
    event: BackendEventEnvelope,
}

struct RuntimeDispatchOverflow {
    sequence: u64,
    template: BackendEventEnvelope,
    dropped_count: u64,
    dropped_kinds: Vec<BackendEventKind>,
}

struct RuntimeDispatchPending {
    sequence: u64,
    event: BackendEventEnvelope,
}

enum RuntimeDispatchAdmission {
    Accepted(bool),
    Wait,
    Cancelled,
}

#[derive(Clone, Copy)]
enum RuntimeDispatchLane {
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
    fn enqueue(
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

    fn pop_next(&mut self) -> Option<BackendEventEnvelope> {
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
    fn queued_counts(&self) -> (usize, usize, usize, usize) {
        (
            self.ordinary.len(),
            self.control.len(),
            self.overflow.len(),
            usize::from(self.terminal.is_some()),
        )
    }
}

const RUNTIME_BROKER_ORDINARY_CAPACITY: usize = 256;
pub(crate) const RUNTIME_BROKER_CONTROL_CAPACITY: usize = 512;
const RUNTIME_BROKER_OVERFLOW_CAPACITY: usize = 64;

/// A finite subscriber queue between the runtime execution owner and the
/// facade's runtime-event broker.  The old unbounded Tokio channel allowed a
/// paused public callback to accumulate an unbounded upstream backlog.  This
/// queue keeps the same loss-position discipline as the public bridge: only
/// entity/block/sound facts may be dropped, control facts use bounded,
/// cancellation-aware backpressure, and a terminal event has its own slot.
struct RuntimeEventQueue {
    state: parking_lot::Mutex<RuntimeEventQueueState>,
    wake: parking_lot::Condvar,
    notify: Notify,
    #[cfg(test)]
    backpressure_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

struct RuntimeEventQueueState {
    ordinary: VecDeque<RuntimeEventEntry>,
    control: VecDeque<RuntimeEventEntry>,
    overflow: VecDeque<RuntimeOverflowEntry>,
    terminal: Option<RuntimeEventEntry>,
    next_sequence: u64,
    next_admission: u64,
    open_loss_segment: Option<u64>,
}

struct RuntimeEventEntry {
    sequence: u64,
    event: BackendEventEnvelope,
}

struct RuntimeOverflowEntry {
    sequence: u64,
    template: BackendEventEnvelope,
    dropped_count: u64,
    dropped_kinds: Vec<BackendEventKind>,
}

#[derive(Clone, Copy)]
enum RuntimeQueueLane {
    Ordinary,
    Control,
    Overflow,
    Terminal,
}

/// Receiver returned by `RuntimeHandle::subscribe`.  It intentionally exposes
/// only bounded queue operations while retaining the small `recv`/`try_recv`
/// surface used by the runtime worker and deterministic tests.
pub struct RuntimeEventReceiver {
    queue: Arc<RuntimeEventQueue>,
}

impl RuntimeEventQueue {
    fn new(#[cfg(test)] backpressure_hook: Option<Arc<dyn Fn() + Send + Sync>>) -> Arc<Self> {
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

    fn publish(&self, event: BackendEventEnvelope, cancel: &AtomicBool) -> bool {
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

    fn pop(&self) -> Option<BackendEventEnvelope> {
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

    fn wake_all(&self) {
        self.wake.notify_all();
        self.notify.notify_waiters();
    }

    #[cfg(test)]
    fn queued_count(&self) -> usize {
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
    fn queued_count(&self) -> usize {
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

struct ObservationSubscriber {
    id: u64,
    epoch: u64,
    listener: Arc<dyn ObservationEventListener>,
    state: Arc<ObservationSubscriptionState>,
}

struct ObservationSubscriptionState {
    status: parking_lot::Mutex<ObservationSubscriptionStatus>,
    quiescent: parking_lot::Condvar,
}

#[derive(Default)]
struct ObservationSubscriptionStatus {
    closed: bool,
    pending_callbacks: usize,
    active_callbacks: usize,
}

impl ObservationSubscriptionState {
    fn new() -> Self {
        Self {
            status: parking_lot::Mutex::new(ObservationSubscriptionStatus::default()),
            quiescent: parking_lot::Condvar::new(),
        }
    }

    /// Reserve a callback while the registry lock is held. The reservation is
    /// later turned into an active callback outside that lock.
    fn reserve_callback(&self) -> bool {
        let mut status = self.status.lock();
        if status.closed {
            return false;
        }
        status.pending_callbacks += 1;
        true
    }

    fn start_callback(&self) -> bool {
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

    fn finish_callback(&self) {
        let mut status = self.status.lock();
        debug_assert!(status.active_callbacks > 0);
        status.active_callbacks = status.active_callbacks.saturating_sub(1);
        if status.active_callbacks == 0 && status.pending_callbacks == 0 {
            self.quiescent.notify_all();
        }
    }

    fn close(&self) {
        self.status.lock().closed = true;
    }

    fn is_closed(&self) -> bool {
        self.status.lock().closed
    }

    fn wait_for_quiescence(&self) {
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

struct ObservationDelivery {
    listener: Arc<dyn ObservationEventListener>,
    state: Arc<ObservationSubscriptionState>,
    id: u64,
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

struct ObservationCallbackGuard {
    key: usize,
}

impl ObservationCallbackGuard {
    fn enter(state: &ObservationSubscriptionState) -> Self {
        let key = observation_state_key(state);
        OBSERVATION_CALLBACK_STACK.with(|stack| stack.borrow_mut().push(key));
        Self { key }
    }
}

impl Drop for ObservationCallbackGuard {
    fn drop(&mut self) {
        OBSERVATION_CALLBACK_STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            debug_assert_eq!(stack.pop(), Some(self.key));
        });
    }
}

type SharedWorld = Arc<parking_lot::RwLock<azalea::world::World>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LightSectionGeometry {
    min_light_section: i32,
    light_section_count: usize,
}

impl LightSectionGeometry {
    fn from_world(world: &azalea::world::World) -> Option<Self> {
        let min_y = world.chunks.min_y();
        let height = world.chunks.height();
        if height == 0 || height % 16 != 0 || min_y % 16 != 0 {
            return None;
        }
        let min_light_section = (min_y >> 4).checked_sub(1)?;
        let light_section_count = usize::try_from(height / 16 + 2).ok()?;
        (light_section_count > 0).then_some(Self {
            min_light_section,
            light_section_count,
        })
    }

    fn index_for_section_y(self, section_y: i32) -> Option<usize> {
        let index = section_y.checked_sub(self.min_light_section)?;
        let index = usize::try_from(index).ok()?;
        (index < self.light_section_count).then_some(index)
    }
}

#[derive(Clone, Debug, Default)]
struct CachedLightChunk {
    sky: Vec<Option<Box<[u8; 4096]>>>,
    block: Vec<Option<Box<[u8; 4096]>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LightCacheContext {
    epoch: u64,
    scope_generation: u64,
    dimension: String,
    has_skylight: Option<bool>,
    geometry: Option<LightSectionGeometry>,
}

#[derive(Clone, Debug, Default)]
struct LightCache {
    context: Option<LightCacheContext>,
    chunks: HashMap<(i32, i32), CachedLightChunk>,
}

impl LightCache {
    fn clear(&mut self) {
        self.context = None;
        self.chunks.clear();
    }

    fn reset_scope(
        &mut self,
        epoch: u64,
        scope_generation: u64,
        dimension: Option<String>,
        has_skylight: Option<bool>,
    ) {
        self.chunks.clear();
        self.context = dimension.map(|dimension| LightCacheContext {
            epoch,
            scope_generation,
            dimension,
            has_skylight,
            geometry: None,
        });
    }

    fn context_matches(
        context: &LightCacheContext,
        source: CanonicalSourceAdmission,
        dimension: &str,
    ) -> bool {
        context.epoch == source.epoch
            && context.scope_generation == source.scope_generation
            && context.dimension == dimension
    }

    fn ensure_context(
        &mut self,
        source: CanonicalSourceAdmission,
        dimension: String,
        has_skylight: Option<bool>,
        geometry: LightSectionGeometry,
    ) -> bool {
        let Some(context) = self.context.as_mut() else {
            self.context = Some(LightCacheContext {
                epoch: source.epoch,
                scope_generation: source.scope_generation,
                dimension,
                has_skylight,
                geometry: Some(geometry),
            });
            return true;
        };
        if !Self::context_matches(context, source, &dimension) {
            return false;
        }
        if context.has_skylight != has_skylight {
            // A dimension's skylight property is part of the same scope.  If
            // the registry proof changes underneath a packet, refuse the
            // packet instead of silently reinterpreting old layers.
            return false;
        }
        match context.geometry {
            Some(current) if current != geometry => false,
            None => {
                context.geometry = Some(geometry);
                true
            }
            Some(_) => true,
        }
    }

    fn apply_packet(
        &mut self,
        source: CanonicalSourceAdmission,
        dimension: String,
        has_skylight: Option<bool>,
        geometry: LightSectionGeometry,
        chunk_x: i32,
        chunk_z: i32,
        data: &azalea::protocol::packets::game::c_light_update::ClientboundLightUpdatePacketData,
        replace_chunk: bool,
    ) -> bool {
        if !self.ensure_context(source, dimension, has_skylight, geometry) {
            return false;
        }
        let chunk = self.chunks.entry((chunk_x, chunk_z)).or_default();
        if replace_chunk
            || chunk.sky.len() != geometry.light_section_count
            || chunk.block.len() != geometry.light_section_count
        {
            chunk.sky = vec![None; geometry.light_section_count];
            chunk.block = vec![None; geometry.light_section_count];
        }

        apply_light_layer_mask(
            &mut chunk.sky,
            &data.sky_y_mask,
            &data.empty_sky_y_mask,
            data.sky_updates.as_ref(),
            geometry.light_section_count,
            has_skylight == Some(false),
        );
        apply_light_layer_mask(
            &mut chunk.block,
            &data.block_y_mask,
            &data.empty_block_y_mask,
            data.block_updates.as_ref(),
            geometry.light_section_count,
            false,
        );
        true
    }

    fn remove_chunk(
        &mut self,
        source: CanonicalSourceAdmission,
        dimension: &str,
        chunk_x: i32,
        chunk_z: i32,
    ) -> bool {
        let Some(context) = self.context.as_ref() else {
            return false;
        };
        if !Self::context_matches(context, source, dimension) {
            return false;
        }
        self.chunks.remove(&(chunk_x, chunk_z));
        true
    }

    fn value_at(
        &self,
        position: &Vec3Value,
        epoch: u64,
        scope_generation: u64,
        dimension: &str,
    ) -> Option<u8> {
        let context = self.context.as_ref()?;
        if context.epoch != epoch
            || context.scope_generation != scope_generation
            || context.dimension != dimension
        {
            return None;
        }
        let x = floor_block_coordinate(position.x)?;
        let y = floor_block_coordinate(position.y)?;
        let z = floor_block_coordinate(position.z)?;
        let section_y = y.div_euclid(16);
        let section_index = context.geometry?.index_for_section_y(section_y)?;
        let chunk_x = x.div_euclid(16);
        let chunk_z = z.div_euclid(16);
        let local_x = usize::try_from(x.rem_euclid(16)).ok()?;
        let local_y = usize::try_from(y.rem_euclid(16)).ok()?;
        let local_z = usize::try_from(z.rem_euclid(16)).ok()?;
        let layer_index = (local_y << 8) | (local_z << 4) | local_x;
        let chunk = self.chunks.get(&(chunk_x, chunk_z));
        let sky = if context.has_skylight == Some(false) {
            Some(0)
        } else {
            chunk
                .and_then(|chunk| chunk.sky.get(section_index))
                .and_then(|layer| layer.as_ref())
                .and_then(|layer| layer.get(layer_index).copied())
        };
        let block = chunk
            .and_then(|chunk| chunk.block.get(section_index))
            .and_then(|layer| layer.as_ref())
            .and_then(|layer| layer.get(layer_index).copied());

        match (sky, block) {
            (Some(sky), Some(block)) => Some(sky.max(block)),
            (Some(15), None) | (None, Some(15)) => Some(15),
            _ => None,
        }
    }

    #[cfg(test)]
    fn layer(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        section_index: usize,
        sky: bool,
    ) -> Option<&Box<[u8; 4096]>> {
        self.chunks
            .get(&(chunk_x, chunk_z))
            .and_then(|chunk| {
                if sky {
                    chunk.sky.get(section_index)
                } else {
                    chunk.block.get(section_index)
                }
            })
            .and_then(Option::as_ref)
    }
}

fn apply_light_layer_mask(
    layers: &mut [Option<Box<[u8; 4096]>>],
    data_mask: &azalea::core::bitset::BitSet,
    empty_mask: &azalea::core::bitset::BitSet,
    updates: &[Box<[u8]>],
    light_section_count: usize,
    force_zero: bool,
) {
    let mut update_index = 0;
    for section_index in data_mask.iter_ones() {
        let update = updates.get(update_index);
        update_index += 1;
        if section_index >= light_section_count {
            continue;
        }
        layers[section_index] = if force_zero {
            Some(zero_light_layer())
        } else {
            update.and_then(|update| decode_light_layer(update))
        };
    }

    for section_index in empty_mask.iter_ones() {
        if section_index >= light_section_count || data_mask.get(section_index) == Some(true) {
            continue;
        }
        layers[section_index] = Some(zero_light_layer());
    }
}

fn zero_light_layer() -> Box<[u8; 4096]> {
    Box::new([0; 4096])
}

fn decode_light_layer(bytes: &[u8]) -> Option<Box<[u8; 4096]>> {
    if bytes.len() != 2048 {
        return None;
    }
    let mut layer = Box::new([0; 4096]);
    for local_y in 0..16 {
        for local_z in 0..16 {
            for local_x in 0..16 {
                let index = (local_y << 8) | (local_z << 4) | local_x;
                let packed = bytes[index >> 1];
                layer[index] = if index & 1 == 0 {
                    packed & 0x0f
                } else {
                    packed >> 4
                };
            }
        }
    }
    Some(layer)
}

fn floor_block_coordinate(value: f64) -> Option<i32> {
    if !value.is_finite() {
        return None;
    }
    let value = value.floor();
    (value >= f64::from(i32::MIN) && value <= f64::from(i32::MAX)).then_some(value as i32)
}

/// Reproduce `AttributeInstance`'s grouped operation order.  The packet's
/// modifier vector is first reduced to a Java-map-shaped last-write-wins map;
/// operation iteration is never used to determine the three group order.
fn calculate_armor_snapshot(
    snapshot: &azalea::protocol::packets::game::c_update_attributes::AttributeSnapshot,
) -> Option<u8> {
    calculate_armor_values(
        snapshot.base,
        &snapshot.modifiers,
        |modifier| modifier.id.clone(),
        |modifier| modifier.amount,
        |modifier| modifier.operation,
    )
}

fn calculate_armor_values<T, K, Id, Amount, Operation>(
    base: f64,
    modifiers: &[T],
    mut id: Id,
    mut amount: Amount,
    mut operation: Operation,
) -> Option<u8>
where
    K: PartialEq,
    Id: FnMut(&T) -> K,
    Amount: FnMut(&T) -> f64,
    Operation: FnMut(&T) -> azalea::core::attribute_modifier_operation::AttributeModifierOperation,
{
    use azalea::core::attribute_modifier_operation::AttributeModifierOperation;

    if !base.is_finite() {
        return None;
    }
    let mut modifier_indices = Vec::<usize>::new();
    for index in 0..modifiers.len() {
        if let Some(existing) = modifier_indices
            .iter()
            .position(|existing| id(&modifiers[*existing]) == id(&modifiers[index]))
        {
            modifier_indices[existing] = index;
        } else {
            modifier_indices.push(index);
        }
    }

    let mut add_value = base;
    let mut multiplied_base_sum: f64 = 0.0;
    let mut multiplied_total = Vec::new();
    for index in modifier_indices {
        let modifier_amount = amount(&modifiers[index]);
        if !modifier_amount.is_finite() {
            return None;
        }
        match operation(&modifiers[index]) {
            AttributeModifierOperation::AddValue => {
                add_value += modifier_amount;
                if !add_value.is_finite() {
                    return None;
                }
            }
            AttributeModifierOperation::AddMultipliedBase => {
                multiplied_base_sum += modifier_amount;
                if !multiplied_base_sum.is_finite() {
                    return None;
                }
            }
            AttributeModifierOperation::AddMultipliedTotal => {
                multiplied_total.push(modifier_amount);
            }
        }
    }

    let mut value = add_value + add_value * multiplied_base_sum;
    if !value.is_finite() {
        return None;
    }
    for amount in multiplied_total {
        let factor = 1.0 + amount;
        if !factor.is_finite() {
            return None;
        }
        value *= factor;
        if !value.is_finite() {
            return None;
        }
    }

    // The vanilla attribute sanitizer's lower bound is zero for armor.  The
    // public frame fact additionally follows the frozen 0..20 wire range.
    let sanitized = value.max(0.0);
    if !sanitized.is_finite() {
        return None;
    }
    Some(sanitized.floor().clamp(0.0, 20.0) as u8)
}

/// The observation values used by one viewport capture share one short-lived
/// generation lock. The world itself remains behind its own read/write lock;
/// this lock only binds the world handle, snapshot, source and entities to one
/// published capture.
struct ObservationState {
    world: Option<SharedWorld>,
    snapshot: Option<MinecraftSnapshotV1>,
    /// The producer scope that authored `snapshot`.  The public snapshot wire
    /// intentionally has no scope field; this private stamp prevents frame
    /// facts from being combined with a snapshot captured before Respawn.
    snapshot_scope_generation: u64,
    source: Option<FactSource>,
    tracked_entities: Vec<ProtocolEntitySnapshot>,
    /// Packet fields Azalea does not expose in the ECS capture, or a packet
    /// velocity which its handler intentionally leaves untouched.  This is a
    /// live-entity residual, not an event queue: it is cleared on scope/world
    /// reset and removed with the tracked entity.
    entity_residuals: Vec<EntityObservationResidual>,
    /// Armor is a connection fact.  It deliberately survives same-epoch
    /// Login/Respawn scope resets, but the epoch stamp makes a new connection
    /// automatically unavailable.
    armor: Option<u8>,
    armor_epoch: Option<u64>,
    light_cache: LightCache,
    generation: u64,
}

/// The packet fields that are not necessarily represented by the ECS capture
/// have an explicit authority transition.  In particular, a new Spawn or a
/// Teleport starts a new velocity authority; it must not inherit a residual
/// from the previous incarnation of the same protocol id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntityResidualAction {
    Retain,
    Update,
    Clear,
}

const ENTITY_OBSERVATION_RESIDUAL_CAPACITY: usize = 1024;

#[derive(Clone, Debug, PartialEq)]
struct EntityObservationResidual {
    entity_key: String,
    head_yaw: Option<f32>,
    velocity: Option<[f64; 3]>,
}

impl Default for ObservationState {
    fn default() -> Self {
        Self {
            world: None,
            snapshot: None,
            snapshot_scope_generation: 0,
            source: None,
            tracked_entities: Vec::new(),
            entity_residuals: Vec::new(),
            armor: None,
            armor_epoch: None,
            light_cache: LightCache::default(),
            generation: 0,
        }
    }
}

impl ObservationState {
    fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    fn clear_all_frame_facts(&mut self) {
        self.armor = None;
        self.armor_epoch = None;
        self.light_cache.clear();
    }

    fn clear_light_for_scope(
        &mut self,
        epoch: u64,
        scope_generation: u64,
        dimension: Option<String>,
        has_skylight: Option<bool>,
    ) {
        self.light_cache
            .reset_scope(epoch, scope_generation, dimension, has_skylight);
    }
}

enum ActiveMovementRegistration {
    Started { cancel_signal: Option<Arc<Notify>> },
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttemptAdmissionState {
    NotStarted,
    Reserved {
        epoch: u64,
        reconnect_token: Option<u64>,
        join_started_epoch: Option<u64>,
        /// The vendor `AttemptToken` admitted by the canonical
        /// `StartJoinServerEvent`, if the attempt was stamped. `None` is the
        /// legacy/unstamped path.
        attempt_token: Option<azalea::join::AttemptToken>,
    },
    Bound {
        epoch: u64,
        entity: bevy_ecs::entity::Entity,
        reconnect_token: Option<u64>,
        attempt_token: Option<azalea::join::AttemptToken>,
    },
    Closed {
        epoch: u64,
    },
}

impl Default for AttemptAdmissionState {
    fn default() -> Self {
        Self::NotStarted
    }
}

/// A backend-only fence for sources that Azalea does not stamp with a
/// connection identity.  `Entity` is reusable across reconnects, so after a
/// same-entity handoff there is no sound predicate that can distinguish a
/// delayed A message from a new B message.  The conservative state is to
/// reject all unstamped source messages after that point.  A stamped
/// reconnect-return token may still install the owner, but it does not clear
/// this fence; clearing it would falsely claim provenance that the vendor
/// event does not carry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct EntitySourceFence {
    last_bound_entity: Option<bevy_ecs::entity::Entity>,
    pending_rebind_entity: Option<bevy_ecs::entity::Entity>,
    ambiguous: bool,
}

impl EntitySourceFence {
    fn begin_attempt(&mut self) {
        self.pending_rebind_entity = self.last_bound_entity;
    }

    fn allows_unstamped(&self, entity: bevy_ecs::entity::Entity) -> bool {
        !self.ambiguous && self.pending_rebind_entity != Some(entity)
    }

    fn allows_unstamped_global(&self) -> bool {
        !self.ambiguous && self.pending_rebind_entity.is_none()
    }

    fn bind(&mut self, entity: bevy_ecs::entity::Entity) {
        if self.last_bound_entity == Some(entity) {
            self.ambiguous = true;
        }
        self.last_bound_entity = Some(entity);
        self.pending_rebind_entity = None;
    }
}

/// The short-lived identity captured when a canonical Azalea source is read.
/// Publication rechecks all three pieces under `command_admission`; an event
/// cannot be stamped with a later owner, epoch, or scope after a handoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CanonicalSourceAdmission {
    entity: bevy_ecs::entity::Entity,
    epoch: u64,
    scope_generation: u64,
    /// The vendor attempt token captured at the canonical source, if the
    /// source was stamped. `None` only for the legacy/unstamped fallback.
    attempt_token: Option<azalea::join::AttemptToken>,
}

/// One-to-one vendor `AttemptToken` ↔ backend epoch bindings for the whole
/// `RuntimeSession`.
///
/// Growth boundary: every successfully stamped join attempt adds at most one
/// pair of entries, and entries are intentionally never removed so that a
/// historical token can never be re-registered on a later epoch. The size is
/// therefore bounded by the number of stamped join attempts in the session.
#[derive(Default)]
struct SourceTokenBindings {
    token_to_epoch: std::collections::HashMap<azalea::join::AttemptToken, u64>,
    epoch_to_token: std::collections::HashMap<u64, azalea::join::AttemptToken>,
}

impl SourceTokenBindings {
    /// Register `token` for `epoch` exactly once. Returns `false` (without
    /// mutating anything) when the token is already bound to a different
    /// epoch or the epoch is already bound to a different token; the same
    /// pair is idempotent.
    fn bind(&mut self, token: azalea::join::AttemptToken, epoch: u64) -> bool {
        if let Some(bound_epoch) = self.token_to_epoch.get(&token) {
            return *bound_epoch == epoch;
        }
        if let Some(bound_token) = self.epoch_to_token.get(&epoch) {
            return *bound_token == token;
        }
        self.token_to_epoch.insert(token, epoch);
        self.epoch_to_token.insert(epoch, token);
        true
    }

    fn matches(&self, token: azalea::join::AttemptToken, epoch: u64) -> bool {
        self.token_to_epoch.get(&token) == Some(&epoch)
    }
}

#[derive(Default)]
struct EntityProducerRuntimeState {
    owner: Option<(bevy_ecs::entity::Entity, u64)>,
    scope_generation: u64,
    attempt: AttemptAdmissionState,
    /// A bounded hand-off from the canonical ECS ConnectionFailed source to
    /// Azalea's high-level event handler.  It only exists for the current
    /// pre-Init attempt and is cleared on every attempt transition.
    pending_connection_failure: Option<(bevy_ecs::entity::Entity, u64)>,
    source_fence: EntitySourceFence,
    source_token_bindings: SourceTokenBindings,
    cache: EntityProducerCache,
}

impl EntityProducerRuntimeState {
    fn reset_scope(&mut self, epoch: u64) {
        self.scope_generation = self.scope_generation.wrapping_add(1);
        self.cache.reset_scope(epoch);
    }

    fn deactivate_scope(&mut self) {
        self.scope_generation = self.scope_generation.wrapping_add(1);
        self.cache.deactivate_scope();
    }
}

#[cfg(test)]
mod entity_events_owner_tests {
    use super::*;

    /// The vendor `AttemptToken` bound by a test app's owner setup and used by
    /// its packet queue helpers, so stamped canonical sources are exercised
    /// end-to-end instead of falling back to the legacy fence.
    #[derive(Clone, Copy, Resource)]
    pub(super) struct TestAttemptToken(pub(super) azalea::join::AttemptToken);

    fn snapshot(epoch: u64, protocol_id: i32, x: f64) -> NormalizedEntitySnapshot {
        NormalizedEntitySnapshot {
            identity: EntityIdentity::new(epoch, protocol_id),
            entity_type: "minecraft:pig".to_owned(),
            uuid: Some(format!("entity-{protocol_id}")),
            name: None,
            username: None,
            position: [x, 64.0, -3.0],
            velocity: [0.25, 0.0, -0.5],
            yaw: 45.0,
            pitch: -11.25,
            head_yaw: Some(90.0),
            width: 0.9,
            height: 0.9,
            on_ground: true,
            pose: None,
            held_item_name: None,
            equipment: Vec::new(),
            valid: true,
        }
    }

    fn token(epoch: u64, admission: u64) -> EntityProducerToken {
        EntityProducerToken::new(epoch, format!("packet:{admission}"))
    }

    fn scope_snapshot(epoch: u64) -> MinecraftSnapshotV1 {
        MinecraftSnapshotV1 {
            protocol: crate::snapshot::SNAPSHOT_PROTOCOL.to_owned(),
            snapshot_revision: 1,
            lifecycle_revision: 1,
            captured_at: now_utc(),
            process_session_id: "scope-test".to_owned(),
            connection_epoch: epoch,
            connection_attempt_id: format!("attempt-{epoch}"),
            world: crate::snapshot::WorldSnapshot {
                world_id: "scope-world".to_owned(),
                dimension: "minecraft:overworld".to_owned(),
                minecraft_version: "26.1.2".to_owned(),
                protocol_version: 775,
                game_mode: "survival".to_owned(),
                min_y: -64,
                height: 384,
            },
            self_snapshot: crate::snapshot::SelfSnapshot {
                entity_key: "scope-self".to_owned(),
                username: "scope".to_owned(),
                position: Vec3Value {
                    x: 0.0,
                    y: 64.0,
                    z: 0.0,
                },
                velocity: Vec3Value {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                yaw: 0.0,
                pitch: 0.0,
                on_ground: true,
                alive: true,
                health: 20.0,
                food: 20,
                food_saturation: 5.0,
                experience: crate::snapshot::ExperienceSnapshot {
                    level: 0,
                    progress: 0.0,
                    total: 0,
                },
            },
            inventory: crate::snapshot::InventorySnapshot {
                selected_hotbar_slot: 0,
                slots: Vec::new(),
            },
            tracked_players: Vec::new(),
        }
    }

    #[test]
    fn owner_binding_rejects_late_a_and_preserves_b_epoch2_shadow() {
        let shared = Arc::new(SharedRuntime::new(RunConfig::default()));
        let mut world = bevy_ecs::world::World::new();
        let owner_a = world.spawn_empty().id();
        let owner_b = world.spawn_empty().id();

        assert!(shared.begin_connection_attempt());
        assert_eq!(
            shared.consume_attempt_for_transport_init_and_bind(owner_a),
            Some(1)
        );
        assert_eq!(shared.entity_producer_epoch_for(owner_a), Some(1));
        assert!(matches!(
            shared.apply_entity_input_for_owner(
                owner_a,
                1,
                EntityProducerInput::Spawn {
                    token: token(1, 1),
                    snapshot: snapshot(1, 7, 1.0),
                },
            ),
            Some(NormalizedEntityEvent::Spawned { .. })
        ));

        assert!(shared.begin_connection_attempt());
        assert_eq!(
            shared.consume_attempt_for_transport_init_and_bind(owner_b),
            Some(2)
        );
        assert_eq!(shared.entity_producer_epoch_for(owner_a), None);
        assert_eq!(shared.entity_producer_epoch_for(owner_b), Some(2));

        assert!(shared
            .apply_entity_input_for_owner(
                owner_a,
                1,
                EntityProducerInput::Spawn {
                    token: token(1, 2),
                    snapshot: snapshot(1, 8, 99.0),
                },
            )
            .is_none());

        let spawned = shared.apply_entity_input_for_owner(
            owner_b,
            2,
            EntityProducerInput::Spawn {
                token: token(2, 1),
                snapshot: snapshot(2, 7, 10.0),
            },
        );
        assert!(matches!(
            spawned,
            Some(NormalizedEntityEvent::Spawned { ref entity })
                if entity.entity_key() == "2:7" && entity.position[0] == 10.0
        ));

        // A late lifecycle message cannot reset or deactivate B's scope.
        assert!(!shared.reset_entity_scope_for_owner(owner_a));
        assert!(!shared.deactivate_entity_producer_owner(owner_a));
        assert_eq!(shared.entity_producer_epoch_for(owner_b), Some(2));

        let moved = shared.apply_entity_input_for_owner(
            owner_b,
            2,
            EntityProducerInput::Move {
                token: token(2, 2),
                patch: EntityMovePatch::relative(
                    EntityIdentity::new(2, 7),
                    Some([4096, 0, 0]),
                    None,
                    false,
                ),
            },
        );
        assert!(matches!(
            moved,
            Some(NormalizedEntityEvent::Moved { ref entity })
                if entity.entity_key() == "2:7"
                    && entity.position[0] == 11.0
                    && !entity.on_ground
        ));

        let removed = shared.apply_entity_input_for_owner(
            owner_b,
            2,
            EntityProducerInput::Remove {
                token: token(2, 3),
                entity: EntityIdentity::new(2, 7),
            },
        );
        assert!(matches!(
            removed,
            Some(NormalizedEntityEvent::Removed { entity, ref last })
                if entity.key() == "2:7" && last.position[0] == 11.0
        ));

        assert!(shared.deactivate_entity_producer_owner(owner_b));
        assert_eq!(shared.entity_producer_epoch_for(owner_b), None);
        assert!(shared
            .entity_producer
            .lock()
            .cache
            .apply(
                2,
                EntityProducerInput::Spawn {
                    token: token(2, 4),
                    snapshot: snapshot(2, 9, 12.0),
                },
            )
            .is_none());
        assert!(shared
            .apply_entity_input_for_owner(
                owner_b,
                2,
                EntityProducerInput::Spawn {
                    token: token(2, 5),
                    snapshot: snapshot(2, 9, 12.0),
                },
            )
            .is_none());
    }

    #[test]
    fn accepted_a_payload_is_dropped_after_b_binds_instead_of_using_b_envelope() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut world = bevy_ecs::world::World::new();
        let owner_a = world.spawn_empty().id();
        let owner_b = world.spawn_empty().id();

        assert!(handle.shared.begin_connection_attempt());
        let request_a = events.try_recv().expect("attempt 1 request");
        assert_eq!(request_a.connection_epoch, 1);
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(owner_a),
            Some(1)
        );
        let _transport_a = events.try_recv().expect("A transport connected");

        let after_apply = Arc::new(std::sync::Barrier::new(2));
        let release_publish = Arc::new(std::sync::Barrier::new(2));
        handle
            .shared
            .set_entity_publish_after_apply_hook(Some(Arc::new({
                let after_apply = after_apply.clone();
                let release_publish = release_publish.clone();
                move || {
                    after_apply.wait();
                    release_publish.wait();
                }
            })));

        let emitter_shared = handle.shared.clone();
        let emitter = std::thread::spawn(move || {
            emitter_shared.emit_entity_input(
                owner_a,
                1,
                EntityProducerInput::Spawn {
                    token: token(1, 1),
                    snapshot: snapshot(1, 7, 1.0),
                },
            )
        });
        after_apply.wait();

        // The old implementation would resume A after this complete switch
        // and let EventWriter stamp A's payload with epoch 2.
        assert!(handle.shared.begin_connection_attempt());
        let request_b = events.try_recv().expect("attempt 2 request");
        assert_eq!(request_b.connection_epoch, 2);
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(owner_b),
            Some(2)
        );
        let _transport_b = events.try_recv().expect("B transport connected");
        assert!(handle.shared.emit_entity_input(
            owner_b,
            2,
            EntityProducerInput::Spawn {
                token: token(2, 1),
                snapshot: snapshot(2, 7, 10.0),
            },
        ));
        let spawned_b = events.try_recv().expect("B spawn");
        assert_eq!(spawned_b.connection_epoch, 2);
        match spawned_b.payload {
            BackendEventPayload::Entity(ContractProtocolEntityEvent::Spawned { entity }) => {
                assert_eq!(entity.entity_key, "2:7");
                assert_eq!(entity.position.x, 10.0);
            }
            payload => panic!("expected B spawned payload, got {payload:?}"),
        }

        release_publish.wait();
        assert!(!emitter.join().expect("A emitter"));
        assert!(
            events.try_recv().is_err(),
            "accepted A payload must not appear under B metadata"
        );

        assert!(handle.shared.emit_entity_input(
            owner_b,
            2,
            EntityProducerInput::Move {
                token: token(2, 2),
                patch: EntityMovePatch::relative(
                    EntityIdentity::new(2, 7),
                    Some([4096, 0, 0]),
                    None,
                    false,
                ),
            },
        ));
        assert!(handle.shared.emit_entity_input(
            owner_b,
            2,
            EntityProducerInput::Remove {
                token: token(2, 3),
                entity: EntityIdentity::new(2, 7),
            },
        ));
        let moved_b = events.try_recv().expect("B move");
        let removed_b = events.try_recv().expect("B remove");
        assert_eq!(moved_b.connection_epoch, 2);
        assert_eq!(removed_b.connection_epoch, 2);
        match removed_b.payload {
            BackendEventPayload::Entity(ContractProtocolEntityEvent::Removed {
                entity_key,
                last,
                ..
            }) => {
                assert_eq!(entity_key, "2:7");
                assert_eq!(last.entity_key, "2:7");
                assert_eq!(last.position.x, 11.0);
            }
            payload => panic!("expected B removed payload, got {payload:?}"),
        }
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn init_owner_bind_and_attempt_transition_share_one_epoch_transaction() {
        let shared = Arc::new(SharedRuntime::new(RunConfig::default()));
        let mut world = bevy_ecs::world::World::new();
        let owner_a = world.spawn_empty().id();
        let owner_b = world.spawn_empty().id();

        assert!(shared.begin_connection_attempt());
        let bind_epoch_read = Arc::new(std::sync::Barrier::new(2));
        let release_bind = Arc::new(std::sync::Barrier::new(2));
        shared.set_entity_owner_bind_hook(Some(Arc::new({
            let bind_epoch_read = bind_epoch_read.clone();
            let release_bind = release_bind.clone();
            move || {
                bind_epoch_read.wait();
                release_bind.wait();
            }
        })));

        let init_shared = shared.clone();
        let init = std::thread::spawn(move || {
            init_shared.consume_attempt_for_transport_init_and_bind(owner_a)
        });
        bind_epoch_read.wait();
        assert!(
            shared.command_admission.try_lock().is_none(),
            "Init must retain lifecycle admission through owner installation"
        );
        assert_eq!(shared.writer.lock().connection_epoch, 1);

        let (transition_started_tx, transition_started_rx) = std::sync::mpsc::channel();
        let (transition_done_tx, transition_done_rx) = std::sync::mpsc::channel();
        let transition_shared = shared.clone();
        let transition = std::thread::spawn(move || {
            transition_started_tx
                .send(())
                .expect("attempt transition start signal");
            let result = transition_shared.begin_connection_attempt();
            transition_done_tx
                .send(result)
                .expect("attempt transition result");
        });
        transition_started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("attempt 2 must start competing");
        assert!(
            transition_done_rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err(),
            "attempt 2 must not advance while Init owns admission"
        );
        assert_eq!(shared.writer.lock().connection_epoch, 1);

        release_bind.wait();
        assert_eq!(init.join().expect("attempt 1 Init"), Some(1));
        assert!(transition_done_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("attempt 2 completes after Init transaction"));
        transition.join().expect("attempt 2 transition");

        assert_eq!(shared.writer.lock().connection_epoch, 2);
        assert_eq!(shared.entity_producer_epoch_for(owner_a), None);
        assert_eq!(
            shared.consume_attempt_for_transport_init_and_bind(owner_b),
            Some(2)
        );
        assert_eq!(shared.entity_producer_epoch_for(owner_b), Some(2));
    }

    #[test]
    fn swarm_disconnect_deactivates_current_owner_but_late_a_disconnect_cannot_clear_b() {
        let shared = Arc::new(SharedRuntime::new(RunConfig::default()));
        let mut world = bevy_ecs::world::World::new();
        let owner_a = world.spawn_empty().id();
        let owner_b = world.spawn_empty().id();

        assert!(shared.begin_connection_attempt());
        assert_eq!(
            shared.consume_attempt_for_transport_init_and_bind(owner_a),
            Some(1)
        );
        assert!(shared.begin_connection_attempt());
        assert_eq!(
            shared.consume_attempt_for_transport_init_and_bind(owner_b),
            Some(2)
        );
        assert_eq!(shared.entity_producer_epoch_for(owner_b), Some(2));

        // The entity-bearing canonical source closes B first.  The later
        // entity-less SwarmEvent is only the reconnect fallback.
        assert!(shared
            .admit_canonical_disconnected(owner_b, 2, Some("B canonical disconnect".to_owned()))
            .is_some());
        assert!(shared.claim_reconnect());
        assert_eq!(shared.entity_producer_epoch_for(owner_b), None);
        assert!(!shared.deactivate_entity_producer_owner(owner_a));
        assert!(shared
            .apply_entity_input_for_owner(
                owner_b,
                2,
                EntityProducerInput::Spawn {
                    token: token(2, 1),
                    snapshot: snapshot(2, 7, 10.0),
                },
            )
            .is_none());
    }

    #[test]
    fn late_entity_lifecycle_cannot_close_b_but_current_b_disconnect_closes_once() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut world = bevy_ecs::world::World::new();
        let owner_a = world.spawn_empty().id();
        let owner_b = world.spawn_empty().id();

        assert!(handle.shared.begin_connection_attempt());
        let _request_a = events.try_recv().expect("attempt 1 request");
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(owner_a),
            Some(1)
        );
        let _transport_a = events.try_recv().expect("A transport connected");
        assert!(handle.shared.begin_connection_attempt());
        let _request_b = events.try_recv().expect("attempt 2 request");
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(owner_b),
            Some(2)
        );
        let _transport_b = events.try_recv().expect("B transport connected");

        {
            let mut observation = handle.shared.observation.write();
            observation.generation = 77;
            observation.source = Some(FactSource::ServerObserved);
        }
        handle
            .send_chat("must survive stale lifecycle")
            .expect("pending command admission");

        assert!(handle
            .shared
            .admit_canonical_disconnected(owner_a, 1, Some("late A disconnect".to_owned()))
            .is_none());
        assert!(handle
            .shared
            .admit_canonical_connection_failed(owner_a, 1, "late A failure".to_owned())
            .is_none());

        assert_eq!(handle.shared.entity_producer_epoch_for(owner_b), Some(2));
        assert!(!handle.shared.disconnect_reported.load(Ordering::Acquire));
        assert!(handle.shared.last_close.lock().is_none());
        assert!(handle.shared.last_failure.lock().is_none());
        assert_eq!(handle.shared.observation.read().generation, 77);
        assert_eq!(
            handle.shared.observation.read().source,
            Some(FactSource::ServerObserved)
        );
        assert_eq!(handle.shared.commands.lock().len(), 1);
        assert!(events.try_recv().is_err());
        assert!(matches!(
            handle.state(),
            BackendState::LoggingIn { epoch: 2, .. }
        ));

        assert!(handle
            .shared
            .admit_canonical_disconnected(owner_b, 2, Some("current B disconnect".to_owned()))
            .is_some());
        let closed = events.try_recv().expect("current B close");
        assert_eq!(closed.connection_epoch, 2);
        assert_eq!(
            serde_json::to_value(&closed.payload).expect("close payload")["type"],
            "connection_closed"
        );
        assert!(events.try_recv().is_err(), "close must be emitted once");
        assert_eq!(handle.shared.entity_producer_epoch_for(owner_b), None);
        assert!(handle.shared.commands.lock().is_empty());
    }

    #[test]
    fn pre_init_connection_failed_claims_only_the_unbound_current_attempt() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut world = bevy_ecs::world::World::new();
        let owner = world.spawn_empty().id();

        assert!(handle.shared.begin_connection_attempt());
        let _request = events.try_recv().expect("pre-Init attempt request");
        assert!(handle.shared.admit_canonical_join_started(1));
        assert!(handle
            .shared
            .admit_canonical_connection_failed(owner, 1, "failed before Init".to_owned())
            .is_some());
        let closed = events.try_recv().expect("pre-Init failure close");
        assert_eq!(closed.connection_epoch, 1);
        assert_eq!(
            serde_json::to_value(&closed.payload).expect("close payload")["type"],
            "connection_closed"
        );
        assert!(handle
            .shared
            .admit_canonical_connection_failed(owner, 1, "duplicate pre-Init failure".to_owned())
            .is_none());
        assert!(handle
            .shared
            .take_canonical_connection_failure_followup(owner));
        assert!(!handle
            .shared
            .take_canonical_connection_failure_followup(owner));
        assert!(handle.shared.entity_producer_epoch_for(owner).is_none());
        assert!(matches!(
            handle.shared.entity_producer.lock().attempt,
            AttemptAdmissionState::Closed { epoch: 1 }
        ));
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn same_entity_reconnect_return_binds_without_init_and_publishes_spawn_move_remove() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut world = bevy_ecs::world::World::new();
        let entity = world.spawn_empty().id();

        assert!(handle.shared.begin_connection_attempt());
        let _request_a = events.try_recv().expect("A request");
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(entity),
            Some(1)
        );
        let _transport_a = events.try_recv().expect("A transport");
        assert!(handle
            .shared
            .admit_canonical_disconnected(entity, 1, Some("A ended".to_owned()))
            .is_some());
        let _close_a = events.try_recv().expect("A close");

        assert!(handle.shared.claim_reconnect());
        let reconnect_token = handle
            .shared
            .admit_reconnect_attempt()
            .expect("B reconnect admission");
        let _request_b = events.try_recv().expect("B request");

        // B has no Init.  The returned client consumes the reserved B
        // identity directly, before finish_reconnect_attempt can close it.
        assert_eq!(
            handle.shared.bind_reconnect_return(reconnect_token, entity),
            Some(2)
        );
        let transport_b = events.try_recv().expect("B transport");
        assert_eq!(transport_b.connection_epoch, 2);
        handle.shared.finish_reconnect_attempt(reconnect_token);
        assert_eq!(handle.shared.entity_producer_epoch_for(entity), Some(2));
        let source = handle.observation_source();

        // A late Init is an idempotent no-op after the return bind.
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(entity),
            Some(2)
        );
        assert!(events.try_recv().is_err());

        assert!(handle.shared.emit_entity_input(
            entity,
            2,
            EntityProducerInput::Spawn {
                token: token(2, 1),
                snapshot: snapshot(2, 7, 10.0),
            },
        ));
        let after_spawn = source.list_tracked_entities().expect("spawn observation");
        assert_eq!(after_spawn.len(), 1);
        assert_eq!(after_spawn[0].entity_key, "2:7");
        assert_eq!(after_spawn[0].entity_type, "minecraft:pig");
        assert_eq!(after_spawn[0].position.x, 10.0);
        assert_eq!(after_spawn[0].head_yaw, Some(90.0));
        assert!(handle.shared.emit_entity_input(
            entity,
            2,
            EntityProducerInput::Move {
                token: token(2, 2),
                patch: EntityMovePatch::relative(
                    EntityIdentity::new(2, 7),
                    Some([4096, 0, 0]),
                    None,
                    false,
                ),
            },
        ));
        let after_move = source.list_tracked_entities().expect("move observation");
        assert_eq!(after_move[0].entity_key, "2:7");
        assert_eq!(after_move[0].position.x, 11.0);
        assert_eq!(after_move[0].entity_type, "minecraft:pig");
        assert!(handle.shared.emit_entity_input(
            entity,
            2,
            EntityProducerInput::Remove {
                token: token(2, 3),
                entity: EntityIdentity::new(2, 7),
            },
        ));
        assert!(source
            .list_tracked_entities()
            .expect("remove observation")
            .is_empty());

        let spawned = events.try_recv().expect("B spawn");
        let moved = events.try_recv().expect("B move");
        let removed = events.try_recv().expect("B remove");
        assert_eq!(spawned.connection_epoch, 2);
        assert_eq!(moved.connection_epoch, 2);
        assert_eq!(removed.connection_epoch, 2);
        match (spawned.payload, moved.payload, removed.payload) {
            (
                BackendEventPayload::Entity(ContractProtocolEntityEvent::Spawned { entity: spawn }),
                BackendEventPayload::Entity(ContractProtocolEntityEvent::Moved { entity: move_ }),
                BackendEventPayload::Entity(ContractProtocolEntityEvent::Removed {
                    entity_key: remove_key,
                    last,
                    ..
                }),
            ) => {
                assert_eq!(spawn.entity_key, "2:7");
                assert_eq!(move_.entity_key, "2:7");
                assert_eq!(remove_key, "2:7");
                assert_eq!(last.entity_key, "2:7");
                assert_eq!(last.position.x, 11.0);
            }
            payloads => panic!("expected Spawn/Move/Remove, got {payloads:?}"),
        }
    }

    #[test]
    fn same_entity_late_a_source_epoch_is_inert_before_and_after_b_bind() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut world = bevy_ecs::world::World::new();
        let entity = world.spawn_empty().id();

        assert!(handle.shared.begin_connection_attempt());
        let _request_a = events.try_recv().expect("A request");
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(entity),
            Some(1)
        );
        let _transport_a = events.try_recv().expect("A transport");
        assert!(handle
            .shared
            .admit_canonical_disconnected(entity, 1, Some("A ended".to_owned()))
            .is_some());
        let _close_a = events.try_recv().expect("A close");

        assert!(handle.shared.claim_reconnect());
        let reconnect_token = handle
            .shared
            .admit_reconnect_attempt()
            .expect("B reconnect admission");
        let _request_b = events.try_recv().expect("B request");

        // B is reserved but not bound.  Explicit source epoch 1 is rejected;
        // Entity alone would incorrectly identify this as the same client.
        assert!(handle
            .shared
            .admit_canonical_disconnected(entity, 1, Some("late A".to_owned()))
            .is_none());
        assert!(handle
            .shared
            .admit_canonical_connection_failed(entity, 1, "late A failure".to_owned())
            .is_none());
        assert!(!handle
            .shared
            .admit_canonical_packet_source_for_epoch(entity, 1));

        assert_eq!(
            handle.shared.bind_reconnect_return(reconnect_token, entity),
            Some(2)
        );
        let _transport_b = events.try_recv().expect("B transport");
        handle.shared.finish_reconnect_attempt(reconnect_token);
        assert!(handle.shared.emit_entity_input(
            entity,
            2,
            EntityProducerInput::Spawn {
                token: token(2, 10),
                snapshot: snapshot(2, 7, 20.0),
            },
        ));
        let _spawn = events.try_recv().expect("B spawn");

        // The same stale-A evidence remains inert after B owns the entity.
        assert!(handle
            .shared
            .admit_canonical_disconnected(entity, 1, Some("late A 2".to_owned()))
            .is_none());
        assert!(handle
            .shared
            .admit_canonical_connection_failed(entity, 1, "late A failure 2".to_owned())
            .is_none());
        assert!(!handle
            .shared
            .admit_canonical_packet_source_for_epoch(entity, 1));
        assert!(!handle
            .shared
            .reset_entity_scope_for_owner_at_epoch(entity, 1));
        assert!(handle
            .shared
            .apply_entity_input_for_owner(
                entity,
                1,
                EntityProducerInput::Spawn {
                    token: token(1, 11),
                    snapshot: snapshot(1, 7, 999.0),
                },
            )
            .is_none());

        assert!(handle.shared.emit_entity_input(
            entity,
            2,
            EntityProducerInput::Move {
                token: token(2, 12),
                patch: EntityMovePatch::relative(
                    EntityIdentity::new(2, 7),
                    Some([4096, 0, 0]),
                    None,
                    true,
                ),
            },
        ));
        let moved = events.try_recv().expect("B move after stale A");
        match moved.payload {
            BackendEventPayload::Entity(ContractProtocolEntityEvent::Moved { entity }) => {
                assert_eq!(entity.entity_key, "2:7");
                assert_eq!(entity.position.x, 21.0);
            }
            payload => panic!("expected B moved payload, got {payload:?}"),
        }
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn same_entity_current_preinit_failure_and_bound_closes_preserve_reason_once() {
        let pre_init = RuntimeHandle::new(RunConfig::default());
        let mut pre_events = pre_init.subscribe();
        let mut world = bevy_ecs::world::World::new();
        let entity = world.spawn_empty().id();

        assert!(pre_init.shared.begin_connection_attempt());
        let _request = pre_events.try_recv().expect("pre-Init request");
        assert!(pre_init.shared.admit_canonical_join_started(1));
        assert!(pre_init
            .shared
            .admit_canonical_connection_failed(entity, 1, "pre-init exact error".to_owned())
            .is_some());
        let close = pre_events.try_recv().expect("pre-Init close");
        match close.payload {
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionClosed { close }) => {
                assert_eq!(close.code, "connection_failed");
                assert_eq!(
                    close.error.expect("failure error").message,
                    "pre-init exact error"
                );
            }
            payload => panic!("expected pre-Init close, got {payload:?}"),
        }
        assert!(pre_init
            .shared
            .admit_canonical_connection_failed(entity, 1, "duplicate".to_owned())
            .is_none());
        assert!(pre_init
            .shared
            .take_canonical_connection_failure_followup(entity));
        assert!(!pre_init
            .shared
            .take_canonical_connection_failure_followup(entity));
        assert!(pre_events.try_recv().is_err());

        let bound = RuntimeHandle::new(RunConfig::default());
        let mut bound_events = bound.subscribe();
        let mut bound_world = bevy_ecs::world::World::new();
        let bound_entity = bound_world.spawn_empty().id();
        assert!(bound.shared.begin_connection_attempt());
        let _request = bound_events.try_recv().expect("bound request");
        assert_eq!(
            bound
                .shared
                .consume_attempt_for_transport_init_and_bind(bound_entity),
            Some(1)
        );
        let _transport = bound_events.try_recv().expect("bound transport");
        assert!(bound
            .shared
            .admit_canonical_disconnected(
                bound_entity,
                1,
                Some("current B exact reason".to_owned()),
            )
            .is_some());
        let close = bound_events.try_recv().expect("bound close");
        match close.payload {
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionClosed { close }) => {
                assert_eq!(close.code, "unclassified_kick");
                assert_eq!(close.end_reason.as_deref(), Some("current B exact reason"));
            }
            payload => panic!("expected bound close, got {payload:?}"),
        }
        assert!(bound
            .shared
            .admit_canonical_disconnected(bound_entity, 1, Some("duplicate reason".to_owned()),)
            .is_none());
        assert!(bound_events.try_recv().is_err());
    }

    #[test]
    fn same_reserved_attempt_init_is_rejected_and_return_is_idempotent() {
        fn exercise(init_first: bool) {
            let handle = RuntimeHandle::new(RunConfig::default());
            let mut events = handle.subscribe();
            let mut world = bevy_ecs::world::World::new();
            let entity = world.spawn_empty().id();

            assert!(handle.shared.begin_connection_attempt());
            let _request_a = events.try_recv().expect("A request");
            assert_eq!(
                handle
                    .shared
                    .consume_attempt_for_transport_init_and_bind(entity),
                Some(1)
            );
            let _transport_a = events.try_recv().expect("A transport");
            assert!(handle
                .shared
                .admit_canonical_disconnected(entity, 1, Some("A ended".to_owned()))
                .is_some());
            let _close_a = events.try_recv().expect("A close");
            assert!(handle.shared.claim_reconnect());
            let reconnect_token = handle
                .shared
                .admit_reconnect_attempt()
                .expect("B admission");
            let _request_b = events.try_recv().expect("B request");

            if init_first {
                assert!(!handle.shared.admit_canonical_join_started(2));
                assert_eq!(
                    handle
                        .shared
                        .consume_attempt_for_transport_init_and_bind(entity),
                    None
                );
                assert_eq!(
                    handle.shared.bind_reconnect_return(reconnect_token, entity),
                    Some(2)
                );
                let _transport_b = events.try_recv().expect("B transport");
            } else {
                assert_eq!(
                    handle.shared.bind_reconnect_return(reconnect_token, entity),
                    Some(2)
                );
                let _transport_b = events.try_recv().expect("B transport");
                assert_eq!(
                    handle
                        .shared
                        .consume_attempt_for_transport_init_and_bind(entity),
                    Some(2)
                );
            }
            assert_eq!(
                handle.shared.bind_reconnect_return(reconnect_token, entity),
                Some(2)
            );
            assert_eq!(
                handle
                    .shared
                    .consume_attempt_for_transport_init_and_bind(entity),
                Some(2)
            );
            handle.shared.finish_reconnect_attempt(reconnect_token);
            assert_eq!(handle.shared.entity_producer_epoch_for(entity), Some(2));
            assert!(events.try_recv().is_err());
        }

        exercise(true);
        exercise(false);
    }

    fn synthetic_attempt_token() -> azalea::join::AttemptToken {
        azalea::join::AttemptToken::mint()
    }

    fn send_production_entity_packet(
        app: &mut App,
        owner: bevy_ecs::entity::Entity,
        packet: azalea::protocol::packets::game::ClientboundGamePacket,
    ) {
        queue_production_entity_packet(app, owner, packet);
        app.update();
    }

    fn queue_production_entity_packet(
        app: &mut App,
        owner: bevy_ecs::entity::Entity,
        packet: azalea::protocol::packets::game::ClientboundGamePacket,
    ) {
        let attempt_token = app.world().resource::<TestAttemptToken>().0;
        app.world_mut()
            .write_message(azalea::packet::game::ReceiveGamePacketEvent {
                entity: owner,
                packet: Arc::new(packet),
                attempt_token,
            });
    }

    fn production_add_packet(id: i32) -> azalea::protocol::packets::game::ClientboundGamePacket {
        azalea::protocol::packets::game::ClientboundGamePacket::AddEntity(
            azalea::protocol::packets::game::ClientboundAddEntity {
                id: id.into(),
                uuid: Default::default(),
                entity_type: azalea::registry::builtin::EntityKind::DarkOakChestBoat,
                position: azalea::Vec3::new(10.0, 64.0, 2.0),
                movement: azalea::core::delta::LpVec3::from_vec3(azalea::Vec3::new(
                    0.25, 0.0, -0.5,
                )),
                x_rot: -8,
                y_rot: 16,
                y_head_rot: 32,
                data: 0,
            },
        )
    }

    fn production_common_spawn_info(
        dimension: &str,
    ) -> azalea::protocol::packets::common::CommonPlayerSpawnInfo {
        use azalea::core::game_type::{GameMode, OptionalGameType};
        use azalea::protocol::packets::common::CommonPlayerSpawnInfo;
        use azalea::registry::data::DimensionKind;

        let dimension_type = <DimensionKind as azalea::registry::DataRegistry>::new_raw(0);
        CommonPlayerSpawnInfo {
            dimension_type,
            dimension: azalea::Identifier::from(dimension),
            seed: 0,
            game_type: GameMode::Survival,
            previous_game_type: OptionalGameType(None),
            is_debug: false,
            is_flat: false,
            last_death_location: None,
            portal_cooldown: 0,
            sea_level: 63,
        }
    }

    fn production_login_packet(
        player_id: i32,
        dimension: &str,
    ) -> azalea::protocol::packets::game::ClientboundGamePacket {
        azalea::protocol::packets::game::ClientboundGamePacket::Login(
            azalea::protocol::packets::game::ClientboundLogin {
                player_id: player_id.into(),
                hardcore: false,
                levels: Vec::new(),
                max_players: 1,
                chunk_radius: 8,
                simulation_distance: 8,
                reduced_debug_info: false,
                show_death_screen: true,
                do_limited_crafting: false,
                common: production_common_spawn_info(dimension),
                enforces_secure_chat: false,
            },
        )
    }

    fn production_respawn_packet(
        dimension: &str,
    ) -> azalea::protocol::packets::game::ClientboundGamePacket {
        azalea::protocol::packets::game::ClientboundGamePacket::Respawn(
            azalea::protocol::packets::game::ClientboundRespawn {
                common: production_common_spawn_info(dimension),
                data_to_keep: 0,
            },
        )
    }

    fn production_position_packets() -> [azalea::protocol::packets::game::ClientboundGamePacket; 6]
    {
        [
            azalea::protocol::packets::game::ClientboundGamePacket::MoveEntityPos(
                azalea::protocol::packets::game::ClientboundMoveEntityPos {
                    entity_id: 7.into(),
                    delta: azalea::core::delta::PositionDelta8 {
                        xa: 4096,
                        ya: 0,
                        za: 0,
                    },
                    on_ground: false,
                },
            ),
            azalea::protocol::packets::game::ClientboundGamePacket::TeleportEntity(
                azalea::protocol::packets::game::ClientboundTeleportEntity {
                    id: 7.into(),
                    change: azalea::protocol::common::movements::PositionMoveRotation {
                        pos: azalea::Vec3::new(20.0, 1.0, -4.0),
                        delta: azalea::Vec3::new(0.5, 0.0, 0.25),
                        look_direction: azalea::entity::LookDirection::new(90.0, -10.0),
                    },
                    relative: azalea::protocol::common::movements::RelativeMovements {
                        x: false,
                        y: true,
                        z: false,
                        y_rot: false,
                        x_rot: false,
                        delta_x: true,
                        delta_y: false,
                        delta_z: true,
                        rotate_delta: false,
                    },
                    on_ground: true,
                },
            ),
            azalea::protocol::packets::game::ClientboundGamePacket::EntityPositionSync(
                azalea::protocol::packets::game::ClientboundEntityPositionSync {
                    id: 7.into(),
                    values: azalea::protocol::common::movements::PositionMoveRotation {
                        pos: azalea::Vec3::new(30.0, 66.0, -5.0),
                        delta: azalea::Vec3::new(0.0, 1.0, 0.0),
                        look_direction: azalea::entity::LookDirection::new(120.0, -20.0),
                    },
                    on_ground: false,
                },
            ),
            azalea::protocol::packets::game::ClientboundGamePacket::SetEntityMotion(
                azalea::protocol::packets::game::ClientboundSetEntityMotion {
                    id: 7.into(),
                    delta: azalea::core::delta::LpVec3::from_vec3(azalea::Vec3::new(2.0, 3.0, 4.0)),
                },
            ),
            azalea::protocol::packets::game::ClientboundGamePacket::RotateHead(
                azalea::protocol::packets::game::ClientboundRotateHead {
                    entity_id: 7.into(),
                    y_head_rot: 64,
                },
            ),
            azalea::protocol::packets::game::ClientboundGamePacket::RemoveEntities(
                azalea::protocol::packets::game::ClientboundRemoveEntities {
                    entity_ids: vec![7.into()],
                },
            ),
        ]
    }

    #[test]
    fn production_packet_batch_keeps_each_callback_at_post_state() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut app = App::new();
        app.add_message::<azalea::packet::game::ReceiveGamePacketEvent>();
        let owner = app
            .world_mut()
            .spawn((LocalEntity, azalea::core::entity_id::MinecraftEntityId(99)))
            .id();
        app.insert_resource(SwarmState {
            shared: handle.shared.clone(),
        });
        app.add_systems(Update, produce_entity_packet_events);

        assert!(handle.shared.begin_connection_attempt());
        let _request = events.try_recv().expect("packet seam request");
        let test_token = synthetic_attempt_token();
        assert!(handle
            .shared
            .admit_canonical_join_started_with_token(1, Some(test_token)));
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind_with_token(owner, Some(test_token)),
            Some(1)
        );
        app.world_mut()
            .insert_resource(TestAttemptToken(test_token));
        let _transport = events.try_recv().expect("packet seam transport");
        let source = handle.observation_source();
        let states = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let _subscription = ProtocolObservationSource::subscribe(
            &source,
            Arc::new(ImmediateEntityObservationReader {
                source: source.clone(),
                states: states.clone(),
            }),
        )
        .expect("callback subscription");

        queue_production_entity_packet(&mut app, owner, production_add_packet(7));
        for packet in production_position_packets() {
            queue_production_entity_packet(&mut app, owner, packet);
        }
        app.update();

        let emitted = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert_eq!(
            emitted.len(),
            6,
            "SetEntityMotion must not emit an envelope"
        );
        assert!(matches!(
            &emitted[0].payload,
            BackendEventPayload::Entity(ContractProtocolEntityEvent::Spawned { entity })
                if entity.entity_key == "1:7"
        ));
        assert!(matches!(
            &emitted[1].payload,
            BackendEventPayload::Entity(ContractProtocolEntityEvent::Moved { entity })
                if entity.position.x == 11.0
        ));
        assert!(matches!(
            &emitted[2].payload,
            BackendEventPayload::Entity(ContractProtocolEntityEvent::Moved { entity })
                if entity.position.x == 20.0
        ));
        assert!(matches!(
            &emitted[3].payload,
            BackendEventPayload::Entity(ContractProtocolEntityEvent::Moved { entity })
                if entity.position.x == 30.0
        ));
        assert!(matches!(
            &emitted[4].payload,
            BackendEventPayload::Entity(ContractProtocolEntityEvent::Moved { entity })
                if entity.head_yaw.is_some_and(|value| {
                    (value - std::f64::consts::FRAC_PI_2).abs() < 1e-6
                })
        ));
        assert!(
            matches!(
                &emitted[5].payload,
                BackendEventPayload::Entity(ContractProtocolEntityEvent::Removed { last, .. })
                    if last.entity_key == "1:7" && (last.velocity.x - 2.0).abs() < 0.001
            ),
            "unexpected Remove payload: {:?}",
            emitted[5].payload
        );

        let states = states.lock();
        assert_eq!(states.len(), 6);
        assert_eq!(states[0][0].position.x, 10.0);
        assert_eq!(states[1][0].position.x, 11.0);
        assert_eq!(
            (
                states[2][0].position.x,
                states[2][0].position.y,
                states[2][0].position.z
            ),
            (20.0, 65.0, -4.0)
        );
        assert_eq!(
            (
                states[3][0].position.x,
                states[3][0].position.y,
                states[3][0].position.z
            ),
            (30.0, 66.0, -5.0)
        );
        assert!((states[3][0].velocity.y - 1.0).abs() < 1e-6);
        assert!((states[4][0].velocity.x - 2.0).abs() < 0.001);
        assert!((states[4][0].velocity.y - 3.0).abs() < 0.001);
        assert!((states[4][0].velocity.z - 4.0).abs() < 0.001);
        assert!(
            states[5].is_empty(),
            "Remove callback must see an empty list"
        );
    }

    #[test]
    fn production_packet_batch_login_respawn_add_preserves_dimension_order() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut app = App::new();
        app.add_message::<azalea::packet::game::ReceiveGamePacketEvent>();
        let owner = app
            .world_mut()
            .spawn((LocalEntity, azalea::core::entity_id::MinecraftEntityId(99)))
            .id();
        app.insert_resource(SwarmState {
            shared: handle.shared.clone(),
        });
        app.add_systems(Update, produce_entity_packet_events);

        assert!(handle.shared.begin_connection_attempt());
        let _request = events.try_recv().expect("packet seam request");
        let test_token = synthetic_attempt_token();
        assert!(handle
            .shared
            .admit_canonical_join_started_with_token(1, Some(test_token)));
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind_with_token(owner, Some(test_token)),
            Some(1)
        );
        app.world_mut()
            .insert_resource(TestAttemptToken(test_token));
        let _transport = events.try_recv().expect("packet seam transport");
        let source = handle.observation_source();

        queue_production_entity_packet(
            &mut app,
            owner,
            production_login_packet(99, "minecraft:overworld"),
        );
        queue_production_entity_packet(&mut app, owner, production_add_packet(7));
        app.update();

        let initial = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert_eq!(
            initial.len(),
            1,
            "initial Login must not invent a dimension change"
        );
        assert_eq!(initial[0].dimension.as_deref(), Some("minecraft:overworld"));
        assert!(matches!(
            &initial[0].payload,
            BackendEventPayload::Entity(ContractProtocolEntityEvent::Spawned { entity })
                if entity.entity_key == "1:7"
        ));

        queue_production_entity_packet(
            &mut app,
            owner,
            production_respawn_packet("minecraft:the_nether"),
        );
        queue_production_entity_packet(&mut app, owner, production_add_packet(8));
        app.update();

        let respawn = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert_eq!(respawn.len(), 2);
        assert!(matches!(
            &respawn[0].payload,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::DimensionChanged { from, to })
                if from == "minecraft:overworld" && to == "minecraft:the_nether"
        ));
        assert_eq!(
            respawn[0].dimension.as_deref(),
            Some("minecraft:the_nether")
        );
        assert!(matches!(
            &respawn[1].payload,
            BackendEventPayload::Entity(ContractProtocolEntityEvent::Spawned { entity })
                if entity.entity_key == "1:8"
        ));
        assert_eq!(
            respawn[1].dimension.as_deref(),
            Some("minecraft:the_nether")
        );
        let tracked = source.list_tracked_entities().expect("respawn observation");
        assert_eq!(tracked.len(), 1);
        assert_eq!(tracked[0].entity_key, "1:8");
    }

    #[test]
    fn production_packet_adapter_keeps_each_packet_post_state_and_excludes_self() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut app = App::new();
        app.add_message::<azalea::packet::game::ReceiveGamePacketEvent>();
        let owner = app
            .world_mut()
            .spawn((LocalEntity, azalea::core::entity_id::MinecraftEntityId(99)))
            .id();
        app.insert_resource(SwarmState {
            shared: handle.shared.clone(),
        });
        app.add_systems(Update, produce_entity_packet_events);

        assert!(handle.shared.begin_connection_attempt());
        let _request = events.try_recv().expect("packet seam request");
        let test_token = synthetic_attempt_token();
        assert!(handle
            .shared
            .admit_canonical_join_started_with_token(1, Some(test_token)));
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind_with_token(owner, Some(test_token)),
            Some(1)
        );
        app.world_mut()
            .insert_resource(TestAttemptToken(test_token));
        let _transport = events.try_recv().expect("packet seam transport");
        let source = handle.observation_source();

        send_production_entity_packet(&mut app, owner, production_add_packet(7));
        let spawned = source
            .list_tracked_entities()
            .expect("packet Add observation");
        assert_eq!(spawned.len(), 1);
        assert_eq!(spawned[0].entity_key, "1:7");
        assert_eq!(spawned[0].entity_type, "dark_oak_chest_boat");
        assert_eq!(spawned[0].position.x, 10.0);
        assert!(
            (spawned[0].head_yaw.expect("spawn head yaw") - std::f64::consts::FRAC_PI_4).abs()
                < 1e-6
        );
        assert!(matches!(
            events.try_recv().expect("Spawn envelope").payload,
            BackendEventPayload::Entity(ContractProtocolEntityEvent::Spawned { entity })
                if entity.entity_key == "1:7" && entity.entity_type == "dark_oak_chest_boat"
        ));

        send_production_entity_packet(
            &mut app,
            owner,
            azalea::protocol::packets::game::ClientboundGamePacket::MoveEntityPos(
                azalea::protocol::packets::game::ClientboundMoveEntityPos {
                    entity_id: 7.into(),
                    delta: azalea::core::delta::PositionDelta8 {
                        xa: 4096,
                        ya: 0,
                        za: 0,
                    },
                    on_ground: false,
                },
            ),
        );
        assert_eq!(source.list_tracked_entities().unwrap()[0].position.x, 11.0);
        assert!(matches!(
            events.try_recv().expect("relative Move envelope").payload,
            BackendEventPayload::Entity(ContractProtocolEntityEvent::Moved { entity })
                if entity.entity_key == "1:7" && entity.position.x == 11.0
        ));

        send_production_entity_packet(
            &mut app,
            owner,
            azalea::protocol::packets::game::ClientboundGamePacket::TeleportEntity(
                azalea::protocol::packets::game::ClientboundTeleportEntity {
                    id: 7.into(),
                    change: azalea::protocol::common::movements::PositionMoveRotation {
                        pos: azalea::Vec3::new(20.0, 1.0, -4.0),
                        delta: azalea::Vec3::new(0.5, 0.0, 0.25),
                        look_direction: azalea::entity::LookDirection::new(90.0, -10.0),
                    },
                    relative: azalea::protocol::common::movements::RelativeMovements {
                        x: false,
                        y: true,
                        z: false,
                        y_rot: false,
                        x_rot: false,
                        delta_x: true,
                        delta_y: false,
                        delta_z: true,
                        rotate_delta: false,
                    },
                    on_ground: true,
                },
            ),
        );
        let teleported = source.list_tracked_entities().unwrap();
        assert_eq!(
            (teleported[0].position.x, teleported[0].position.y),
            (20.0, 65.0)
        );
        assert_eq!(teleported[0].position.z, -4.0);
        assert!(matches!(
            events.try_recv().expect("Teleport envelope").payload,
            BackendEventPayload::Entity(ContractProtocolEntityEvent::Moved { entity })
                if entity.position.x == 20.0
        ));

        send_production_entity_packet(
            &mut app,
            owner,
            azalea::protocol::packets::game::ClientboundGamePacket::EntityPositionSync(
                azalea::protocol::packets::game::ClientboundEntityPositionSync {
                    id: 7.into(),
                    values: azalea::protocol::common::movements::PositionMoveRotation {
                        pos: azalea::Vec3::new(30.0, 66.0, -5.0),
                        delta: azalea::Vec3::new(0.0, 1.0, 0.0),
                        look_direction: azalea::entity::LookDirection::new(120.0, -20.0),
                    },
                    on_ground: false,
                },
            ),
        );
        let synced = source.list_tracked_entities().unwrap();
        assert_eq!(synced[0].position.x, 30.0);
        assert_eq!(synced[0].velocity.y, 1.0);
        assert!(matches!(
            events.try_recv().expect("PositionSync envelope").payload,
            BackendEventPayload::Entity(ContractProtocolEntityEvent::Moved { entity })
                if entity.position.x == 30.0
        ));

        send_production_entity_packet(
            &mut app,
            owner,
            azalea::protocol::packets::game::ClientboundGamePacket::RotateHead(
                azalea::protocol::packets::game::ClientboundRotateHead {
                    entity_id: 7.into(),
                    y_head_rot: 64,
                },
            ),
        );
        assert_eq!(
            source.list_tracked_entities().unwrap()[0].head_yaw,
            Some((std::f64::consts::FRAC_PI_2 as f32) as f64)
        );
        assert!(matches!(
            events.try_recv().expect("RotateHead envelope").payload,
            BackendEventPayload::Entity(ContractProtocolEntityEvent::Moved { entity })
                if entity
                    .head_yaw
                    .is_some_and(|value| (value - std::f64::consts::FRAC_PI_2).abs() < 1e-6)
        ));

        send_production_entity_packet(
            &mut app,
            owner,
            azalea::protocol::packets::game::ClientboundGamePacket::SetEntityMotion(
                azalea::protocol::packets::game::ClientboundSetEntityMotion {
                    id: 7.into(),
                    delta: azalea::core::delta::LpVec3::from_vec3(azalea::Vec3::new(2.0, 3.0, 4.0)),
                },
            ),
        );
        assert!((source.list_tracked_entities().unwrap()[0].velocity.x - 2.0).abs() < 0.001);
        assert!(
            events.try_recv().is_err(),
            "SetEntityMotion has no envelope"
        );

        send_production_entity_packet(
            &mut app,
            owner,
            azalea::protocol::packets::game::ClientboundGamePacket::RemoveEntities(
                azalea::protocol::packets::game::ClientboundRemoveEntities {
                    entity_ids: vec![7.into()],
                },
            ),
        );
        assert!(source.list_tracked_entities().unwrap().is_empty());
        match events.try_recv().expect("Remove envelope").payload {
            BackendEventPayload::Entity(ContractProtocolEntityEvent::Removed { last, .. }) => {
                assert_eq!(last.entity_key, "1:7");
                assert_eq!(last.position.x, 30.0);
                assert_eq!(
                    last.head_yaw,
                    Some((std::f64::consts::FRAC_PI_2 as f32) as f64)
                );
                assert!((last.velocity.x - 2.0).abs() < 0.001);
            }
            payload => panic!("expected Remove envelope, got {payload:?}"),
        }

        // Every entity packet branch is fail-closed for the local protocol id.
        for packet in [
            production_add_packet(99),
            azalea::protocol::packets::game::ClientboundGamePacket::MoveEntityPos(
                azalea::protocol::packets::game::ClientboundMoveEntityPos {
                    entity_id: 99.into(),
                    delta: Default::default(),
                    on_ground: false,
                },
            ),
            azalea::protocol::packets::game::ClientboundGamePacket::MoveEntityPosRot(
                azalea::protocol::packets::game::ClientboundMoveEntityPosRot {
                    entity_id: 99.into(),
                    delta: Default::default(),
                    look_direction: Default::default(),
                    on_ground: false,
                },
            ),
            azalea::protocol::packets::game::ClientboundGamePacket::MoveEntityRot(
                azalea::protocol::packets::game::ClientboundMoveEntityRot {
                    entity_id: 99.into(),
                    look_direction: Default::default(),
                    on_ground: false,
                },
            ),
            azalea::protocol::packets::game::ClientboundGamePacket::TeleportEntity(
                azalea::protocol::packets::game::ClientboundTeleportEntity {
                    id: 99.into(),
                    change: azalea::protocol::common::movements::PositionMoveRotation {
                        pos: azalea::Vec3::ZERO,
                        delta: azalea::Vec3::ZERO,
                        look_direction: Default::default(),
                    },
                    relative: Default::default(),
                    on_ground: false,
                },
            ),
            azalea::protocol::packets::game::ClientboundGamePacket::EntityPositionSync(
                azalea::protocol::packets::game::ClientboundEntityPositionSync {
                    id: 99.into(),
                    values: azalea::protocol::common::movements::PositionMoveRotation {
                        pos: azalea::Vec3::ZERO,
                        delta: azalea::Vec3::ZERO,
                        look_direction: Default::default(),
                    },
                    on_ground: false,
                },
            ),
            azalea::protocol::packets::game::ClientboundGamePacket::RotateHead(
                azalea::protocol::packets::game::ClientboundRotateHead {
                    entity_id: 99.into(),
                    y_head_rot: 1,
                },
            ),
            azalea::protocol::packets::game::ClientboundGamePacket::SetEntityMotion(
                azalea::protocol::packets::game::ClientboundSetEntityMotion {
                    id: 99.into(),
                    delta: azalea::core::delta::LpVec3::Zero,
                },
            ),
            azalea::protocol::packets::game::ClientboundGamePacket::RemoveEntities(
                azalea::protocol::packets::game::ClientboundRemoveEntities {
                    entity_ids: vec![99.into()],
                },
            ),
        ] {
            send_production_entity_packet(&mut app, owner, packet);
        }
        assert!(source.list_tracked_entities().unwrap().is_empty());
        assert!(events.try_recv().is_err());

        // If LocalEntity/MinecraftEntityId cannot be proven at the adapter's
        // schedule point, entity packets are rejected rather than fail-open.
        app.world_mut().entity_mut(owner).remove::<LocalEntity>();
        send_production_entity_packet(&mut app, owner, production_add_packet(8));
        assert!(source.list_tracked_entities().unwrap().is_empty());
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn observation_source_reads_rotate_and_motion_post_state_without_motion_envelope() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut world = bevy_ecs::world::World::new();
        let owner = world.spawn_empty().id();
        assert!(handle.shared.begin_connection_attempt());
        let _request = events.try_recv().expect("request");
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(owner),
            Some(1)
        );
        let _transport = events.try_recv().expect("transport");
        let source = handle.observation_source();

        assert!(handle.shared.emit_entity_input(
            owner,
            1,
            EntityProducerInput::Spawn {
                token: token(1, 1),
                snapshot: snapshot(1, 7, 1.0),
            },
        ));
        let _spawn = events.try_recv().expect("spawn");
        assert!(handle.shared.emit_entity_input(
            owner,
            1,
            EntityProducerInput::Move {
                token: token(1, 2),
                patch: EntityMovePatch::rotate_head(EntityIdentity::new(1, 7), 64),
            },
        ));
        assert_eq!(
            source.list_tracked_entities().unwrap()[0].head_yaw,
            Some((std::f64::consts::FRAC_PI_2 as f32) as f64)
        );
        let _rotate = events.try_recv().expect("rotate envelope");

        assert!(handle.shared.emit_entity_motion_residual(
            owner,
            1,
            EntityProducerToken::new(1, "set-motion:1"),
            EntityIdentity::new(1, 7),
            [7.0, 8.0, 9.0],
        ));
        let motion = source.list_tracked_entities().unwrap();
        assert_eq!(motion[0].velocity.x, 7.0);
        assert!(events.try_recv().is_err(), "motion has no entity envelope");

        assert!(handle.shared.begin_connection_attempt());
        let stale = BackendError::StaleEpoch {
            bound_epoch: 1,
            current_epoch: 2,
        };
        assert_eq!(source.list_tracked_entities(), Err(stale));
    }

    struct ImmediateEntityObservationReader {
        source: RuntimeObservationSource,
        states: Arc<parking_lot::Mutex<Vec<Vec<ContractProtocolEntitySnapshot>>>>,
    }

    impl ObservationEventListener for ImmediateEntityObservationReader {
        fn on_event(&self, _event: ObservationEvent) {
            self.states.lock().push(
                self.source
                    .list_tracked_entities()
                    .expect("callback observation"),
            );
        }
    }

    #[test]
    fn entity_callback_reads_spawn_move_rotate_remove_post_state_immediately() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut world = bevy_ecs::world::World::new();
        let owner = world.spawn_empty().id();
        assert!(handle.shared.begin_connection_attempt());
        let _request = events.try_recv().expect("request");
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(owner),
            Some(1)
        );
        let _transport = events.try_recv().expect("transport");
        let source = handle.observation_source();
        let states = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let _subscription = ProtocolObservationSource::subscribe(
            &source,
            Arc::new(ImmediateEntityObservationReader {
                source: source.clone(),
                states: states.clone(),
            }),
        )
        .expect("callback subscription");

        assert!(handle.shared.emit_entity_input(
            owner,
            1,
            EntityProducerInput::Spawn {
                token: token(1, 21),
                snapshot: snapshot(1, 7, 10.0),
            },
        ));
        assert!(handle.shared.emit_entity_input(
            owner,
            1,
            EntityProducerInput::Move {
                token: token(1, 22),
                patch: EntityMovePatch::relative(
                    EntityIdentity::new(1, 7),
                    Some([4096, 0, 0]),
                    None,
                    false,
                ),
            },
        ));
        assert!(handle.shared.emit_entity_input(
            owner,
            1,
            EntityProducerInput::Move {
                token: token(1, 23),
                patch: EntityMovePatch::rotate_head(EntityIdentity::new(1, 7), 64),
            },
        ));
        assert!(handle.shared.emit_entity_input(
            owner,
            1,
            EntityProducerInput::Remove {
                token: token(1, 24),
                entity: EntityIdentity::new(1, 7),
            },
        ));

        let states = states.lock();
        assert_eq!(states.len(), 4);
        assert_eq!(states[0][0].entity_key, "1:7");
        assert_eq!(states[0][0].position.x, 10.0);
        assert_eq!(states[1][0].position.x, 11.0);
        assert_eq!(
            states[2][0].head_yaw,
            Some((std::f64::consts::FRAC_PI_2 as f32) as f64)
        );
        assert!(states[3].is_empty());
    }

    #[test]
    fn refresh_merge_only_preserves_explicit_residuals_and_ecs_fields_advance() {
        let key = "4:7".to_owned();
        let mut captured = ProtocolEntitySnapshot {
            entity_key: key.clone(),
            protocol_entity_id: 7,
            entity_type: "old:shadow".to_owned(),
            name: None,
            username: None,
            uuid: Some("old-uuid".to_owned()),
            position: Vec3Value {
                x: 3.0,
                y: 64.0,
                z: 4.0,
            },
            velocity: Vec3Value {
                x: -0.25,
                y: 0.5,
                z: 0.75,
            },
            yaw: 0.125,
            pitch: -0.25,
            head_yaw: Some(0.5),
            width: 0.625,
            height: 1.875,
            on_ground: false,
            pose: Some("standing".to_owned()),
            held_item_name: None,
            equipment: Vec::new(),
            valid: true,
        };
        captured.entity_type = "ecs:dark_oak_chest_boat".to_owned();
        captured.uuid = Some("ecs-uuid".to_owned());
        captured.position.x = 99.0;
        captured.velocity.x = 6.0;
        captured.head_yaw = None;
        captured.pose = Some("crouching".to_owned());
        captured.on_ground = true;
        let mut residuals = vec![EntityObservationResidual {
            entity_key: key.clone(),
            head_yaw: Some(135.0),
            velocity: Some([8.0, 9.0, 10.0]),
        }];
        let merged = merge_refreshed_tracked_entities(vec![captured.clone()], &mut residuals, 4);
        assert_eq!(merged[0].position.x, 99.0);
        assert_eq!(merged[0].entity_type, "ecs:dark_oak_chest_boat");
        assert_eq!(merged[0].uuid.as_deref(), Some("ecs-uuid"));
        assert_eq!(merged[0].pose.as_deref(), Some("crouching"));
        assert!(merged[0].on_ground);
        assert_eq!(merged[0].head_yaw, Some(135.0));
        assert_eq!(merged[0].velocity.x, 8.0);

        let mut no_residuals = Vec::new();
        let no_residual =
            merge_refreshed_tracked_entities(vec![captured.clone()], &mut no_residuals, 4);
        assert_eq!(no_residual[0], captured);

        let handle = RuntimeHandle::new(RunConfig::default());
        let mut world = bevy_ecs::world::World::new();
        let owner = world.spawn_empty().id();
        assert!(handle.shared.begin_connection_attempt());
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(owner),
            Some(1)
        );
        {
            let mut observation = handle.shared.observation.write();
            observation.tracked_entities.push(captured);
            observation.entity_residuals = residuals;
        }
        assert!(handle
            .shared
            .reset_entity_scope_for_owner_at_epoch(owner, 1));
        let observation = handle.shared.observation.read();
        assert!(observation.tracked_entities.is_empty());
        assert!(observation.entity_residuals.is_empty());
    }

    #[test]
    fn remove_then_refresh_with_membership_excluded_capture_does_not_revive() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut world = bevy_ecs::world::World::new();
        let owner = world.spawn_empty().id();
        assert!(handle.shared.begin_connection_attempt());
        let _request = events.try_recv().expect("request");
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(owner),
            Some(1)
        );
        let _transport = events.try_recv().expect("transport");
        let source = handle.observation_source();
        let identity = EntityIdentity::new(1, 7);

        assert!(handle.shared.emit_entity_input(
            owner,
            1,
            EntityProducerInput::Spawn {
                token: token(1, 42),
                snapshot: snapshot(1, 7, 10.0),
            },
        ));
        let _spawn = events.try_recv().expect("spawn");
        assert!(handle.shared.emit_entity_motion_residual(
            owner,
            1,
            EntityProducerToken::new(1, "capture-remove:motion"),
            identity,
            [3.0, 4.0, 5.0],
        ));
        assert!(handle.shared.emit_entity_input(
            owner,
            1,
            EntityProducerInput::Remove {
                token: token(1, 43),
                entity: identity,
            },
        ));
        let _remove = events.try_recv().expect("remove");
        assert!(source.list_tracked_entities().unwrap().is_empty());
        {
            let observation = handle.shared.observation.read();
            assert!(observation.tracked_entities.is_empty());
            assert!(observation.entity_residuals.is_empty());
        }

        // This is the runtime half of the capture boundary: the membership
        // predicate supplies an empty capture while ECS deferred-despawn may
        // still leave the old entity addressable. Refresh must not resurrect
        // either the entity or a residual for it.
        let mut residuals = vec![EntityObservationResidual {
            entity_key: identity.key(),
            head_yaw: None,
            velocity: Some([3.0, 4.0, 5.0]),
        }];
        let refreshed = merge_refreshed_tracked_entities(Vec::new(), &mut residuals, 1);
        assert!(refreshed.is_empty());
        assert!(residuals.is_empty());
        assert!(source.list_tracked_entities().unwrap().is_empty());
    }

    #[test]
    fn set_motion_then_teleport_clears_velocity_before_refresh_and_spawn_reuse() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut world = bevy_ecs::world::World::new();
        let owner = world.spawn_empty().id();
        assert!(handle.shared.begin_connection_attempt());
        let _request = events.try_recv().expect("request");
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(owner),
            Some(1)
        );
        let _transport = events.try_recv().expect("transport");

        let identity = EntityIdentity::new(1, 7);
        assert!(handle.shared.emit_entity_input(
            owner,
            1,
            EntityProducerInput::Spawn {
                token: token(1, 31),
                snapshot: snapshot(1, 7, 1.0),
            },
        ));
        let _spawn = events.try_recv().expect("spawn");
        assert!(handle.shared.emit_entity_motion_residual(
            owner,
            1,
            EntityProducerToken::new(1, "set-motion:v1"),
            identity,
            [1.0, 2.0, 3.0],
        ));
        assert_eq!(
            handle.shared.observation.read().entity_residuals[0].velocity,
            Some([1.0, 2.0, 3.0])
        );

        assert!(handle.shared.emit_entity_input_with_velocity_residual(
            owner,
            1,
            EntityProducerInput::Move {
                token: token(1, 32),
                patch: EntityMovePatch::teleport(
                    identity,
                    [20.0, 65.0, 2.0],
                    [0.0, 0.0],
                    [false; 3],
                    [false; 2],
                    [4.0, 5.0, 6.0],
                    [false; 3],
                    false,
                    true,
                ),
            },
            EntityResidualAction::Clear,
        ));
        let _teleport = events.try_recv().expect("teleport");
        let observation = handle.shared.observation.read();
        assert_eq!(observation.tracked_entities[0].velocity.x, 4.0);
        assert!(!observation.entity_residuals.iter().any(|residual| {
            residual.entity_key == "1:7" && residual.velocity == Some([1.0, 2.0, 3.0])
        }));
        let mut captured = observation.tracked_entities.clone();
        captured[0].velocity.x = 9.0;
        let mut residuals = observation.entity_residuals.clone();
        drop(observation);
        let refreshed = merge_refreshed_tracked_entities(captured, &mut residuals, 1);
        assert_eq!(refreshed[0].velocity.x, 9.0);

        assert!(handle.shared.emit_entity_motion_residual(
            owner,
            1,
            EntityProducerToken::new(1, "set-motion:reuse-old"),
            identity,
            [7.0, 8.0, 9.0],
        ));
        assert!(handle.shared.emit_entity_input(
            owner,
            1,
            EntityProducerInput::Spawn {
                token: token(1, 33),
                snapshot: {
                    let mut reused = snapshot(1, 7, 40.0);
                    reused.velocity = [11.0, 12.0, 13.0];
                    reused
                },
            },
        ));
        let observation = handle.shared.observation.read();
        assert_eq!(observation.tracked_entities[0].velocity.x, 11.0);
        assert!(!observation.entity_residuals.iter().any(|residual| {
            residual.entity_key == "1:7" && residual.velocity == Some([7.0, 8.0, 9.0])
        }));
    }

    #[test]
    fn residuals_are_bounded_and_refresh_drops_orphans() {
        let mut observation = ObservationState::default();
        for id in 0..=ENTITY_OBSERVATION_RESIDUAL_CAPACITY {
            record_entity_residual(
                &mut observation,
                &format!("1:{id}"),
                None,
                Some([id as f64, 0.0, 0.0]),
                EntityResidualAction::Update,
            );
        }
        assert_eq!(
            observation.entity_residuals.len(),
            ENTITY_OBSERVATION_RESIDUAL_CAPACITY
        );
        assert!(!observation
            .entity_residuals
            .iter()
            .any(|residual| residual.entity_key == "1:0"));
        assert!(observation
            .entity_residuals
            .iter()
            .any(|residual| residual.entity_key == "1:1024"));

        let mut residuals = vec![
            EntityObservationResidual {
                entity_key: "1:7".to_owned(),
                head_yaw: None,
                velocity: Some([1.0, 0.0, 0.0]),
            },
            EntityObservationResidual {
                entity_key: "1:orphan".to_owned(),
                head_yaw: None,
                velocity: Some([2.0, 0.0, 0.0]),
            },
            EntityObservationResidual {
                entity_key: "2:stale".to_owned(),
                head_yaw: None,
                velocity: Some([3.0, 0.0, 0.0]),
            },
        ];
        let captured = vec![normalized_entity_snapshot_to_protocol(&snapshot(1, 7, 5.0))
            .expect("finite snapshot should convert")];
        let _ = merge_refreshed_tracked_entities(captured, &mut residuals, 1);
        assert_eq!(residuals.len(), 1);
        assert_eq!(residuals[0].entity_key, "1:7");
    }

    #[test]
    fn same_owner_epoch_reset_invalidates_an_apply_waiting_to_publish() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut world = bevy_ecs::world::World::new();
        let owner = world.spawn_empty().id();
        assert!(handle.shared.begin_connection_attempt());
        let _request = events.try_recv().expect("request");
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(owner),
            Some(1)
        );
        let _transport = events.try_recv().expect("transport");

        {
            let mut observation = handle.shared.observation.write();
            observation.world = Some(Arc::new(parking_lot::RwLock::new(
                azalea::world::World::default(),
            )));
            observation.snapshot = Some(scope_snapshot(1));
            observation.source = Some(FactSource::ServerObserved);
            observation
                .tracked_entities
                .push(normalized_entity_snapshot_to_protocol(&snapshot(1, 6, 0.0)).unwrap());
            observation
                .entity_residuals
                .push(EntityObservationResidual {
                    entity_key: "1:6".to_owned(),
                    head_yaw: None,
                    velocity: Some([1.0, 0.0, 0.0]),
                });
        }

        let after_apply = Arc::new(std::sync::Barrier::new(2));
        let release_publish = Arc::new(std::sync::Barrier::new(2));
        handle
            .shared
            .set_entity_publish_after_apply_hook(Some(Arc::new({
                let after_apply = after_apply.clone();
                let release_publish = release_publish.clone();
                move || {
                    after_apply.wait();
                    release_publish.wait();
                }
            })));

        let emitter_shared = handle.shared.clone();
        let emitter = std::thread::spawn(move || {
            emitter_shared.emit_entity_input(
                owner,
                1,
                EntityProducerInput::Spawn {
                    token: token(1, 41),
                    snapshot: snapshot(1, 7, 1.0),
                },
            )
        });
        after_apply.wait();

        assert!(handle
            .shared
            .reset_entity_scope_for_owner_at_epoch(owner, 1));
        release_publish.wait();
        assert!(!emitter.join().expect("publisher thread should finish"));
        assert!(
            events.try_recv().is_err(),
            "reset must suppress stale envelope"
        );
        let observation = handle.shared.observation.read();
        assert!(observation.world.is_none());
        assert!(observation.snapshot.is_none());
        assert!(observation.source.is_none());
        assert!(observation.tracked_entities.is_empty());
        assert!(observation.entity_residuals.is_empty());
    }
}

struct SharedRuntime {
    writer: parking_lot::Mutex<EventWriter>,
    event_dispatch: parking_lot::Mutex<EventDispatchState>,
    event_dispatch_wake: parking_lot::Condvar,
    swarm: parking_lot::Mutex<Option<Swarm>>,
    shutdown: Arc<Notify>,
    reconnect_cancel: Arc<Notify>,
    shutdown_requested: AtomicBool,
    stop_requested: AtomicBool,
    dispatch_cancelled: AtomicBool,
    config: RunConfig,
    commands: parking_lot::Mutex<VecDeque<QueuedCommand>>,
    subscribers: parking_lot::Mutex<Vec<Arc<RuntimeEventQueue>>>,
    observation_subscribers: parking_lot::Mutex<Vec<ObservationSubscriber>>,
    entity_producer: parking_lot::Mutex<EntityProducerRuntimeState>,
    entity_packet_admission: AtomicU64,
    sound_packet_sequence: AtomicU64,
    next_observation_subscription_id: AtomicU64,
    observation: parking_lot::RwLock<ObservationState>,
    /// Authoritative runtime lifecycle state.  The facade reads this value;
    /// it does not reconstruct a second lifecycle machine from callbacks.
    backend_state: parking_lot::RwLock<BackendState>,
    reported_dimension: parking_lot::Mutex<Option<String>>,
    snapshot_revision: AtomicU64,
    viewport_revision: AtomicU64,
    lifecycle_revision: AtomicU64,
    command_revision: AtomicU64,
    tick_revision: AtomicU64,
    movement_generation: AtomicU64,
    /// Serializes command admission with stop/disconnect marking.  The lock
    /// is deliberately held only while changing admission state; actuator
    /// calls and completion callbacks never run under it.
    command_admission: parking_lot::Mutex<()>,
    active_movement: AtomicBool,
    active_movement_id: parking_lot::Mutex<Option<String>>,
    active_movement_cancel_signal: parking_lot::Mutex<Option<Arc<Notify>>>,
    active_movement_completion: parking_lot::Mutex<Option<Arc<CommandCompletionState>>>,
    /// A Move can be between its active declaration and its first actuator
    /// call.  Stop must wait for that registration window to close before it
    /// emits stopped/shuts down.
    active_movement_registration: AtomicBool,
    timer_started: AtomicBool,
    initial_chat_sent: AtomicBool,
    death_reported: AtomicBool,
    disconnect_reported: AtomicBool,
    stopped_reported: AtomicBool,
    faulted_reported: AtomicBool,
    last_close: parking_lot::Mutex<Option<BackendClose>>,
    last_failure: parking_lot::Mutex<Option<BackendFailure>>,
    stop_reason: parking_lot::Mutex<Option<String>>,
    reconnect_pending: AtomicBool,
    reconnect_add_pending: AtomicBool,
    reconnect_attempt_token: AtomicU64,
    /// Retry ordinal is independent from the never-reset connection epoch.
    retry_ordinal: AtomicU64,
    reconnect_rng: AtomicU64,
    phase_generation: AtomicU64,
    phase_cancel: Arc<Notify>,
    stable_generation: AtomicU64,
    stable_cancel: Arc<Notify>,
    stop_watchdog_generation: AtomicU64,
    stop_watchdog_cancel: Arc<Notify>,
    active_client: parking_lot::Mutex<Option<Client>>,
    timers_enabled: AtomicBool,
    ready: AtomicBool,
    stopping: AtomicBool,
    #[cfg(test)]
    active_movement_registration_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    event_admission_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    finalize_stop_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    event_broadcast_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    event_dispatch_backpressure_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    stop_signal_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    stop_watchdog_completion_probe: parking_lot::Mutex<Option<oneshot::Sender<()>>>,
    #[cfg(test)]
    runtime_broker_backpressure_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    disconnect_cleanup_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    entity_publish_after_apply_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    entity_owner_bind_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    observation_write_boundary_hook: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    canonical_publication_probe: parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl SharedRuntime {
    fn new(config: RunConfig) -> Self {
        Self {
            writer: parking_lot::Mutex::new(EventWriter::new(&config.world_id)),
            event_dispatch: parking_lot::Mutex::new(EventDispatchState::default()),
            event_dispatch_wake: parking_lot::Condvar::new(),
            swarm: parking_lot::Mutex::new(None),
            shutdown: Arc::new(Notify::new()),
            reconnect_cancel: Arc::new(Notify::new()),
            shutdown_requested: AtomicBool::new(false),
            stop_requested: AtomicBool::new(false),
            dispatch_cancelled: AtomicBool::new(false),
            config,
            commands: parking_lot::Mutex::new(VecDeque::new()),
            subscribers: parking_lot::Mutex::new(Vec::new()),
            observation_subscribers: parking_lot::Mutex::new(Vec::new()),
            entity_producer: parking_lot::Mutex::new(EntityProducerRuntimeState::default()),
            entity_packet_admission: AtomicU64::new(0),
            sound_packet_sequence: AtomicU64::new(0),
            next_observation_subscription_id: AtomicU64::new(0),
            observation: parking_lot::RwLock::new(ObservationState::default()),
            backend_state: parking_lot::RwLock::new(BackendState::Idle),
            reported_dimension: parking_lot::Mutex::new(None),
            snapshot_revision: AtomicU64::new(0),
            viewport_revision: AtomicU64::new(0),
            lifecycle_revision: AtomicU64::new(0),
            command_revision: AtomicU64::new(0),
            tick_revision: AtomicU64::new(0),
            movement_generation: AtomicU64::new(0),
            command_admission: parking_lot::Mutex::new(()),
            active_movement: AtomicBool::new(false),
            active_movement_id: parking_lot::Mutex::new(None),
            active_movement_cancel_signal: parking_lot::Mutex::new(None),
            active_movement_completion: parking_lot::Mutex::new(None),
            active_movement_registration: AtomicBool::new(false),
            timer_started: AtomicBool::new(false),
            initial_chat_sent: AtomicBool::new(false),
            death_reported: AtomicBool::new(false),
            disconnect_reported: AtomicBool::new(false),
            stopped_reported: AtomicBool::new(false),
            faulted_reported: AtomicBool::new(false),
            last_close: parking_lot::Mutex::new(None),
            last_failure: parking_lot::Mutex::new(None),
            stop_reason: parking_lot::Mutex::new(None),
            reconnect_pending: AtomicBool::new(false),
            reconnect_add_pending: AtomicBool::new(false),
            reconnect_attempt_token: AtomicU64::new(0),
            retry_ordinal: AtomicU64::new(0),
            reconnect_rng: AtomicU64::new(0x4d494e45494e5441),
            phase_generation: AtomicU64::new(0),
            phase_cancel: Arc::new(Notify::new()),
            stable_generation: AtomicU64::new(0),
            stable_cancel: Arc::new(Notify::new()),
            stop_watchdog_generation: AtomicU64::new(0),
            stop_watchdog_cancel: Arc::new(Notify::new()),
            active_client: parking_lot::Mutex::new(None),
            timers_enabled: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            #[cfg(test)]
            active_movement_registration_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            event_admission_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            finalize_stop_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            event_broadcast_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            event_dispatch_backpressure_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            stop_signal_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            stop_watchdog_completion_probe: parking_lot::Mutex::new(None),
            #[cfg(test)]
            runtime_broker_backpressure_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            disconnect_cleanup_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            entity_publish_after_apply_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            entity_owner_bind_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            observation_write_boundary_hook: parking_lot::Mutex::new(None),
            #[cfg(test)]
            canonical_publication_probe: parking_lot::Mutex::new(None),
        }
    }

    fn set_backend_state(&self, state: BackendState) {
        *self.backend_state.write() = state;
    }

    fn backend_state(&self) -> BackendState {
        self.backend_state.read().clone()
    }

    fn connection_identity(&self) -> (u64, String, u32) {
        let writer = self.writer.lock();
        let retry_ordinal = self.retry_ordinal.load(Ordering::Acquire);
        (
            writer.connection_epoch,
            writer.connection_attempt_id.clone(),
            u32::try_from(retry_ordinal).unwrap_or(u32::MAX),
        )
    }

    fn arm_phase_deadline_locked(&self, phase: TransportPhase) -> Option<PhaseDeadlineToken> {
        let generation = checked_atomic_increment(&self.phase_generation)?;
        let epoch = self.writer.lock().connection_epoch;
        Some(PhaseDeadlineToken {
            epoch,
            attempt: self.retry_ordinal.load(Ordering::Acquire),
            phase,
            generation,
        })
    }

    fn arm_stable_reset_locked(&self) -> Option<StableResetToken> {
        let generation = checked_atomic_increment(&self.stable_generation)?;
        let epoch = self.writer.lock().connection_epoch;
        Some(StableResetToken {
            epoch,
            attempt: self.retry_ordinal.load(Ordering::Acquire),
            generation,
        })
    }

    fn invalidate_phase_locked(&self) {
        let _ = checked_atomic_increment(&self.phase_generation);
    }

    fn invalidate_stable_reset_locked(&self) {
        let _ = checked_atomic_increment(&self.stable_generation);
    }

    fn phase_timeout(&self, phase: TransportPhase) -> Duration {
        let millis = match phase {
            TransportPhase::Connecting => self.config.timeouts.connect_ms,
            TransportPhase::LoggingIn => self.config.timeouts.login_ms,
            TransportPhase::Spawning => self.config.timeouts.spawn_ms,
        };
        Duration::from_millis(millis)
    }

    fn phase_deadline_matches_locked(&self, token: PhaseDeadlineToken) -> bool {
        if self.stopping.load(Ordering::Acquire)
            || self.stop_requested.load(Ordering::Acquire)
            || self.stopped_reported.load(Ordering::Acquire)
            || self.disconnect_reported.load(Ordering::Acquire)
            || self.phase_generation.load(Ordering::Acquire) != token.generation
            || self.retry_ordinal.load(Ordering::Acquire) != token.attempt
            || self.writer.lock().connection_epoch != token.epoch
        {
            return false;
        }
        let attempt = u32::try_from(token.attempt).unwrap_or(u32::MAX);
        match self.backend_state() {
            BackendState::Connecting {
                epoch,
                attempt: state_attempt,
                ..
            } => {
                token.phase == TransportPhase::Connecting
                    && epoch == token.epoch
                    && state_attempt == attempt
            }
            BackendState::LoggingIn {
                epoch,
                attempt: state_attempt,
                ..
            } => {
                token.phase == TransportPhase::LoggingIn
                    && epoch == token.epoch
                    && state_attempt == attempt
            }
            BackendState::Spawning {
                epoch,
                attempt: state_attempt,
                ..
            } => {
                token.phase == TransportPhase::Spawning
                    && epoch == token.epoch
                    && state_attempt == attempt
            }
            _ => false,
        }
    }

    fn stable_reset_matches_locked(&self, token: StableResetToken) -> bool {
        if self.stopping.load(Ordering::Acquire)
            || self.stop_requested.load(Ordering::Acquire)
            || self.stopped_reported.load(Ordering::Acquire)
            || self.disconnect_reported.load(Ordering::Acquire)
            || self.stable_generation.load(Ordering::Acquire) != token.generation
            || self.retry_ordinal.load(Ordering::Acquire) != token.attempt
            || self.writer.lock().connection_epoch != token.epoch
        {
            return false;
        }
        matches!(
            self.backend_state(),
            BackendState::Ready { epoch, .. } if epoch == token.epoch
        )
    }

    fn spawn_phase_deadline(self: &Arc<Self>, token: PhaseDeadlineToken) {
        // The pre-Init connect phase is deliberately not scheduled here:
        // Azalea 0.16 does not expose a safe cancellation handle for its
        // already-polled add/start future.  Login and spawn begin only after
        // a Client/Init identity exists and are production-safe to cancel via
        // the active Client path.
        if token.phase == TransportPhase::Connecting || !self.timers_enabled.load(Ordering::Acquire)
        {
            return;
        }
        let shared = self.clone();
        let cancel = self.phase_cancel.clone();
        let duration = self.phase_timeout(token.phase);
        tokio::task::spawn_local(async move {
            tokio::select! {
                _ = tokio::time::sleep(duration) => shared.fire_phase_deadline(token),
                _ = cancel.notified() => {}
            }
        });
    }

    fn spawn_stable_reset(self: &Arc<Self>, token: StableResetToken) {
        if !self.timers_enabled.load(Ordering::Acquire) {
            return;
        }
        let shared = self.clone();
        let cancel = self.stable_cancel.clone();
        let duration = Duration::from_millis(self.config.reconnect.stable_reset_ms);
        tokio::task::spawn_local(async move {
            tokio::select! {
                _ = tokio::time::sleep(duration) => shared.fire_stable_reset(token),
                _ = cancel.notified() => {}
            }
        });
    }

    fn fire_stable_reset(&self, token: StableResetToken) {
        let _admission = self.command_admission.lock();
        if self.stable_reset_matches_locked(token) {
            self.retry_ordinal.store(0, Ordering::Release);
        }
    }

    fn timeout_close_evidence(phase: TransportPhase) -> CloseEvidence {
        let code = match phase {
            TransportPhase::Connecting => "connection_timeout",
            TransportPhase::LoggingIn => "login_timeout",
            TransportPhase::Spawning => "spawn_timeout",
        };
        CloseEvidence {
            code: code.to_owned(),
            retryable: true,
            deliberate: false,
            kick: None,
            error: None,
            end_reason: Some(code.to_owned()),
            failure: None,
        }
    }

    fn fire_phase_deadline(&self, token: PhaseDeadlineToken) {
        let (close, should_drain, duplicate_cleanup, client) = {
            let _admission = self.command_admission.lock();
            if !self.phase_deadline_matches_locked(token) {
                return;
            }
            self.invalidate_phase_locked();
            let client = self.active_client.lock().clone();
            let result = self
                .mark_disconnected_evidence_locked(Self::timeout_close_evidence(token.phase), None);
            (result.0, result.1, result.2, client)
        };
        self.phase_cancel.notify_waiters();
        if duplicate_cleanup {
            self.cancel_active_movement(true);
            self.cancel_pending_commands();
            self.clear_observations();
        }
        if should_drain {
            self.drain_events();
        }
        if close.deliberate || self.stopping.load(Ordering::Acquire) {
            return;
        }
        // A login/spawn timeout has an active Client.  Let Azalea's canonical
        // DisconnectEvent/SwarmEvent path supply the account/join options for
        // the one reconnect policy; this avoids a second lifecycle reducer.
        if let Some(client) = client {
            client.disconnect();
        }
        if !self.config.reconnect.enabled {
            self.emit_faulted(self.failure_for_close(&close));
            self.request_shutdown();
        }
    }

    #[cfg(test)]
    fn test_current_phase_token(&self, phase: TransportPhase) -> PhaseDeadlineToken {
        PhaseDeadlineToken {
            epoch: self.connection_epoch(),
            attempt: self.retry_ordinal.load(Ordering::Acquire),
            phase,
            generation: self.phase_generation.load(Ordering::Acquire),
        }
    }

    #[cfg(test)]
    fn test_current_stable_token(&self) -> StableResetToken {
        StableResetToken {
            epoch: self.connection_epoch(),
            attempt: self.retry_ordinal.load(Ordering::Acquire),
            generation: self.stable_generation.load(Ordering::Acquire),
        }
    }

    #[cfg(test)]
    fn test_current_stop_watchdog_token(&self) -> StopWatchdogToken {
        StopWatchdogToken {
            generation: self.stop_watchdog_generation.load(Ordering::Acquire),
        }
    }

    /// Caller holds `command_admission`, which also serializes every writer
    /// epoch transition. Installing the owner and resetting its shadow are
    /// therefore part of the same attempt identity transaction.
    fn bind_entity_producer_owner_locked(&self, entity: bevy_ecs::entity::Entity, epoch: u64) {
        #[cfg(test)]
        self.invoke_entity_owner_bind_hook();
        let mut producer = self.entity_producer.lock();
        producer.source_fence.bind(entity);
        producer.owner = Some((entity, epoch));
        producer.reset_scope(epoch);
    }

    #[cfg(test)]
    fn entity_producer_epoch_for(&self, entity: bevy_ecs::entity::Entity) -> Option<u64> {
        self.entity_producer
            .lock()
            .owner
            .and_then(|(owner, epoch)| (owner == entity).then_some(epoch))
    }

    #[cfg(test)]
    fn reset_entity_scope_for_owner(&self, entity: bevy_ecs::entity::Entity) -> bool {
        let mut producer = self.entity_producer.lock();
        let Some((owner, epoch)) = producer.owner else {
            return false;
        };
        if owner != entity {
            return false;
        }
        producer.reset_scope(epoch);
        true
    }

    #[cfg(test)]
    fn deactivate_entity_producer_owner(&self, entity: bevy_ecs::entity::Entity) -> bool {
        let mut producer = self.entity_producer.lock();
        if producer.owner.is_none_or(|(owner, _)| owner != entity) {
            return false;
        }
        producer.owner = None;
        producer.deactivate_scope();
        true
    }

    /// A swarm-level disconnect has no Bevy client entity to compare. It is a
    /// lifecycle-wide boundary, so its reconnect claim deactivates only the
    /// single owner that is current at that admission point. Entity-specific
    /// late disconnects continue through `deactivate_entity_producer_owner`.
    fn deactivate_current_entity_producer_owner(&self) -> bool {
        let mut producer = self.entity_producer.lock();
        if producer.owner.is_none() {
            return false;
        }
        producer.owner = None;
        producer.deactivate_scope();
        true
    }

    /// Claim an entity-specific lifecycle transition while holding
    /// `command_admission`. The caller supplies the epoch observed at the
    /// canonical ECS source. This is the source discriminator that an Azalea
    /// high-level `Event` lacks when the same Bevy entity is reused.
    fn admit_entity_lifecycle_owner_locked(
        &self,
        entity: bevy_ecs::entity::Entity,
        expected_epoch: u64,
        allow_unbound_attempt: bool,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> bool {
        let current_epoch = self.writer.lock().connection_epoch;
        if current_epoch != expected_epoch {
            return false;
        }
        let mut producer = self.entity_producer.lock();
        if let Some(token) = attempt_token {
            // Stamped source: the token must already be bound one-to-one to
            // this exact epoch. The legacy source fence does not apply.
            if !producer
                .source_token_bindings
                .matches(token, expected_epoch)
            {
                return false;
            }
        } else if !producer.source_fence.allows_unstamped(entity) {
            // The event carries no source epoch.  In the same-entity rebind
            // case this is deliberately fail-closed; using `expected_epoch`
            // from the current writer would stamp a possible late A event as
            // B.
            return false;
        }
        match producer.attempt {
            AttemptAdmissionState::Bound {
                epoch,
                entity: bound_entity,
                attempt_token: bound_attempt_token,
                ..
            } if epoch == expected_epoch
                && bound_entity == entity
                && bound_attempt_token == attempt_token
                && producer.owner == Some((entity, expected_epoch)) =>
            {
                producer.owner = None;
                producer.deactivate_scope();
                true
            }
            AttemptAdmissionState::Reserved {
                epoch,
                join_started_epoch,
                attempt_token: reserved_attempt_token,
                ..
            } if allow_unbound_attempt
                && epoch == expected_epoch
                && reserved_attempt_token == attempt_token
                && join_started_epoch == Some(expected_epoch) =>
            {
                producer.deactivate_scope();
                true
            }
            _ => false,
        }
    }

    fn next_entity_packet_admission(&self) -> u64 {
        self.entity_packet_admission.fetch_add(1, Ordering::AcqRel)
    }

    /// Admit an unstamped canonical Azalea source.  The returned context is
    /// intentionally short-lived: the producer must pass it back through
    /// `emit_canonical_observation_event`, which rechecks the same owner,
    /// epoch, scope generation, and source fence immediately before queue
    /// insertion.
    #[cfg(test)]
    fn admit_canonical_source(
        &self,
        entity: bevy_ecs::entity::Entity,
    ) -> Option<CanonicalSourceAdmission> {
        self.admit_canonical_source_with_token(entity, None)
    }

    /// Stamped variant of [`Self::admit_canonical_source`]. The vendor event
    /// token must already be bound one-to-one to the current owner's epoch;
    /// the legacy source fence is never consulted for a stamped source.
    fn admit_canonical_source_with_token(
        &self,
        entity: bevy_ecs::entity::Entity,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<CanonicalSourceAdmission> {
        let _admission = self.command_admission.lock();
        let epoch = self.writer.lock().connection_epoch;
        let producer = self.entity_producer.lock();
        let admitted = producer.owner == Some((entity, epoch))
            && match attempt_token {
                Some(token) => producer.source_token_bindings.matches(token, epoch),
                None => producer.source_fence.allows_unstamped(entity),
            };
        admitted.then_some(CanonicalSourceAdmission {
            entity,
            epoch,
            scope_generation: producer.scope_generation,
            attempt_token,
        })
    }

    fn canonical_source_still_valid_locked(&self, source: CanonicalSourceAdmission) -> bool {
        if self.writer.lock().connection_epoch != source.epoch {
            return false;
        }
        let producer = self.entity_producer.lock();
        producer.owner == Some((source.entity, source.epoch))
            && producer.scope_generation == source.scope_generation
            && match source.attempt_token {
                Some(token) => producer.source_token_bindings.matches(token, source.epoch),
                None => producer.source_fence.allows_unstamped(source.entity),
            }
    }

    /// Apply one self-armor packet at the packet's source position.  The
    /// reducer supplies only a source stamp from the current local owner; the
    /// admission lock is intentionally reacquired here before touching the
    /// observation generation.
    fn apply_armor_packet(
        &self,
        source: CanonicalSourceAdmission,
        values: &[azalea::protocol::packets::game::c_update_attributes::AttributeSnapshot],
    ) -> bool {
        let mut armor = None;
        let mut saw_armor = false;
        for value in values {
            if !matches!(value.attribute, azalea::registry::builtin::Attribute::Armor) {
                continue;
            }
            saw_armor = true;
            armor = calculate_armor_snapshot(value);
        }
        if !saw_armor {
            return false;
        }

        let _admission = self.command_admission.lock();
        if !self.canonical_source_still_valid_locked(source)
            || !self.command_execution_allowed_without_lock()
        {
            return false;
        }
        let mut observation = self.observation.write();
        observation.armor = armor;
        observation.armor_epoch = Some(source.epoch);
        observation.bump_generation();
        true
    }

    /// Apply light data without consulting a later Bevy scope.  `source` is
    /// the immutable stamp captured while the raw packet was at the reducer's
    /// cursor; a scope reset between the two checks therefore rejects the
    /// packet instead of relabeling it with the final scope.
    fn apply_light_packet(
        &self,
        source: CanonicalSourceAdmission,
        geometry: LightSectionGeometry,
        chunk_x: i32,
        chunk_z: i32,
        data: &azalea::protocol::packets::game::c_light_update::ClientboundLightUpdatePacketData,
        replace_chunk: bool,
    ) -> bool {
        let _admission = self.command_admission.lock();
        if !self.canonical_source_still_valid_locked(source)
            || !self.command_execution_allowed_without_lock()
        {
            return false;
        }
        let Some(dimension) = self.writer.lock().dimension.clone() else {
            return false;
        };
        let has_skylight = self
            .observation
            .read()
            .light_cache
            .context
            .as_ref()
            .and_then(|context| context.has_skylight);
        let mut observation = self.observation.write();
        if !observation.light_cache.apply_packet(
            source,
            dimension,
            has_skylight,
            geometry,
            chunk_x,
            chunk_z,
            data,
            replace_chunk,
        ) {
            return false;
        }
        observation.bump_generation();
        true
    }

    fn remove_light_chunk(
        &self,
        source: CanonicalSourceAdmission,
        chunk_x: i32,
        chunk_z: i32,
    ) -> bool {
        let _admission = self.command_admission.lock();
        if !self.canonical_source_still_valid_locked(source)
            || !self.command_execution_allowed_without_lock()
        {
            return false;
        }
        let Some(dimension) = self.writer.lock().dimension.clone() else {
            return false;
        };
        let mut observation = self.observation.write();
        if !observation
            .light_cache
            .remove_chunk(source, &dimension, chunk_x, chunk_z)
        {
            return false;
        }
        observation.bump_generation();
        true
    }

    #[cfg(test)]
    fn admit_canonical_packet_source_for_epoch(
        &self,
        entity: bevy_ecs::entity::Entity,
        source_epoch: u64,
    ) -> bool {
        let _admission = self.command_admission.lock();
        self.writer.lock().connection_epoch == source_epoch
            && self.entity_producer.lock().owner == Some((entity, source_epoch))
    }

    #[cfg(test)]
    fn reset_entity_scope_for_owner_at_epoch(
        &self,
        entity: bevy_ecs::entity::Entity,
        expected_epoch: u64,
    ) -> bool {
        self.reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
            entity,
            expected_epoch,
            None,
            None,
        )
    }

    /// Reset every public authority at a raw Login/Respawn packet boundary.
    /// The dimension, when supplied by that same packet, is admitted after
    /// the reset while the same command-admission lock is held, preserving
    /// packet order and preventing a late boundary from mutating a new owner.
    fn reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
        &self,
        entity: bevy_ecs::entity::Entity,
        expected_epoch: u64,
        dimension: Option<String>,
        has_skylight: Option<bool>,
    ) -> bool {
        let (accepted, should_drain) = {
            let _admission = self.command_admission.lock();
            if self.writer.lock().connection_epoch != expected_epoch {
                return false;
            }
            let mut producer = self.entity_producer.lock();
            if producer.owner != Some((entity, expected_epoch))
                || !producer.source_fence.allows_unstamped(entity)
            {
                return false;
            }
            producer.reset_scope(expected_epoch);
            let scope_generation = producer.scope_generation;
            drop(producer);

            {
                let mut observation = self.observation.write();
                observation.world = None;
                observation.snapshot = None;
                observation.snapshot_scope_generation = 0;
                observation.source = None;
                observation.tracked_entities.clear();
                observation.entity_residuals.clear();
                if observation.armor_epoch != Some(expected_epoch) {
                    observation.armor = None;
                    observation.armor_epoch = None;
                }
                observation.clear_light_for_scope(
                    expected_epoch,
                    scope_generation,
                    dimension.clone(),
                    has_skylight,
                );
                observation.bump_generation();
            }

            let should_drain = dimension
                .map(|dimension| {
                    let Some(previous) = self.set_dimension(dimension.clone()) else {
                        return false;
                    };
                    if previous == dimension {
                        return false;
                    }
                    self.enqueue_event(
                        FactSource::ServerObserved,
                        BackendEventPayload::Lifecycle(BackendLifecyclePayload::DimensionChanged {
                            from: previous,
                            to: dimension,
                        }),
                    )
                })
                .unwrap_or(false);
            (true, should_drain)
        };
        if should_drain {
            self.drain_events();
        }
        accepted
    }

    #[cfg(test)]
    fn apply_entity_input_for_owner(
        &self,
        owner: bevy_ecs::entity::Entity,
        epoch: u64,
        input: EntityProducerInput,
    ) -> Option<NormalizedEntityEvent> {
        self.apply_entity_input_for_owner_with_generation(owner, epoch, input)
            .map(|(event, _generation)| event)
    }

    fn apply_entity_input_for_owner_with_generation(
        &self,
        owner: bevy_ecs::entity::Entity,
        epoch: u64,
        input: EntityProducerInput,
    ) -> Option<(NormalizedEntityEvent, u64)> {
        let mut producer = self.entity_producer.lock();
        if producer.owner != Some((owner, epoch)) {
            return None;
        }
        producer
            .cache
            .apply(epoch, input)
            .map(|event| (event, producer.scope_generation))
    }

    fn emit_entity_input(
        &self,
        owner: bevy_ecs::entity::Entity,
        epoch: u64,
        input: EntityProducerInput,
    ) -> bool {
        let residual_action = match &input {
            EntityProducerInput::Spawn { .. } => EntityResidualAction::Clear,
            _ => EntityResidualAction::Retain,
        };
        self.emit_entity_input_with_velocity_residual(owner, epoch, input, residual_action)
    }

    fn emit_entity_input_with_velocity_residual(
        &self,
        owner: bevy_ecs::entity::Entity,
        epoch: u64,
        input: EntityProducerInput,
        residual_action: EntityResidualAction,
    ) -> bool {
        let normalized = self.apply_entity_input_for_owner_with_generation(owner, epoch, input);
        let Some((normalized, scope_generation)) = normalized else {
            return false;
        };
        #[cfg(test)]
        self.invoke_entity_publish_after_apply_hook();

        let should_drain = {
            let _admission = self.command_admission.lock();
            let producer = self.entity_producer.lock();
            if producer.owner != Some((owner, epoch))
                || producer.scope_generation != scope_generation
            {
                return false;
            }
            // The packet producer's shadow is the immediate post-state
            // authority.  Publish the same state into the public observation
            // list before queueing the event so an observation callback can
            // read it synchronously.
            if !self.apply_entity_observation_event_locked(epoch, &normalized, residual_action) {
                return false;
            }
            let payload =
                BackendEventPayload::Entity(normalized_entity_event_to_contract(normalized));
            let Some(should_drain) = self.enqueue_entity_event_if_running_locked(
                epoch,
                FactSource::ServerObserved,
                payload,
            ) else {
                return false;
            };
            // Keep the owner stable through envelope construction and queue
            // insertion. Queue draining and callbacks happen below, lock-free.
            drop(producer);
            should_drain
        };
        if should_drain {
            self.drain_events();
        }
        true
    }

    fn emit_entity_motion_residual(
        &self,
        owner: bevy_ecs::entity::Entity,
        epoch: u64,
        token: EntityProducerToken,
        entity: EntityIdentity,
        velocity: [f64; 3],
    ) -> bool {
        let normalized = {
            let mut producer = self.entity_producer.lock();
            if producer.owner != Some((owner, epoch)) {
                return false;
            }
            producer
                .cache
                .apply_velocity_residual(epoch, token, entity, velocity)
                .map(|snapshot| (snapshot, producer.scope_generation))
        };
        let Some((normalized, scope_generation)) = normalized else {
            return false;
        };
        let _admission = self.command_admission.lock();
        let producer = self.entity_producer.lock();
        if producer.owner != Some((owner, epoch)) || producer.scope_generation != scope_generation {
            return false;
        }
        let mut observation = self.observation.write();
        let Some(snapshot) = normalized_entity_snapshot_to_protocol(&normalized) else {
            return false;
        };
        upsert_entity_observation(&mut observation, snapshot);
        record_entity_residual(
            &mut observation,
            &normalized.entity_key(),
            None,
            Some(normalized.velocity),
            EntityResidualAction::Update,
        );
        true
    }

    /// Synchronize the public tracked-entity observation with the exact
    /// producer post-state that is about to be emitted.  The caller holds
    /// `command_admission` and the producer guard; callbacks/drain remain
    /// outside all of those locks.
    fn apply_entity_observation_event_locked(
        &self,
        epoch: u64,
        event: &NormalizedEntityEvent,
        residual_action: EntityResidualAction,
    ) -> bool {
        let mut observation = self.observation.write();
        match event {
            NormalizedEntityEvent::Spawned { entity }
            | NormalizedEntityEvent::Moved { entity }
            | NormalizedEntityEvent::Updated { entity, .. } => {
                let Some(snapshot) = normalized_entity_snapshot_to_protocol(entity) else {
                    return false;
                };
                if entity.identity.epoch != epoch {
                    return false;
                }
                let key = snapshot.entity_key.clone();
                upsert_entity_observation(&mut observation, snapshot);
                record_entity_residual(
                    &mut observation,
                    &key,
                    entity.head_yaw,
                    Some(entity.velocity),
                    residual_action,
                );
                true
            }
            NormalizedEntityEvent::Removed { entity, .. } => {
                if entity.epoch != epoch {
                    return false;
                }
                let key = entity.key();
                let before = observation.tracked_entities.len();
                let residual_before = observation.entity_residuals.len();
                observation
                    .tracked_entities
                    .retain(|snapshot| snapshot.entity_key != key);
                observation
                    .entity_residuals
                    .retain(|residual| residual.entity_key != key);
                if before != observation.tracked_entities.len()
                    || residual_before != observation.entity_residuals.len()
                {
                    observation.bump_generation();
                }
                true
            }
            NormalizedEntityEvent::Animation { .. } | NormalizedEntityEvent::Hurt { .. } => true,
        }
    }

    fn enqueue_entity_event_if_running_locked(
        &self,
        expected_epoch: u64,
        source: FactSource,
        payload: BackendEventPayload,
    ) -> Option<bool> {
        if !self.command_execution_allowed_without_lock() {
            return None;
        }
        #[cfg(test)]
        self.invoke_event_admission_hook();

        let mut dispatch = self.event_dispatch.lock();
        let event = {
            let mut writer = self.writer.lock();
            // Metadata is created while this exact check is protected by the
            // attempt admission lock; an entity payload can never be stamped
            // with a later connection's envelope epoch.
            if writer.connection_epoch != expected_epoch {
                return None;
            }
            writer.emit(source, payload)
        };
        Some(self.enqueue_dispatch_locked(&mut dispatch, event))
    }

    /// Publish one canonical block/sound observation after applying the
    /// source fact.  Admission and writer envelope construction share the
    /// command lock, while draining and callbacks happen only after all
    /// world/producer locks have been released by the caller.
    fn emit_canonical_observation_event(
        &self,
        source: CanonicalSourceAdmission,
        payload: BackendEventPayload,
    ) -> bool {
        // Test probe runs *outside* the command admission lock, so a
        // deterministic test can rebind the owner between source admission
        // and publication without deadlocking.
        #[cfg(test)]
        self.invoke_canonical_publication_probe();
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.canonical_source_still_valid_locked(source) {
                return false;
            }
            let Some(should_drain) = self.enqueue_entity_event_if_running_locked(
                source.epoch,
                FactSource::ServerObserved,
                payload,
            ) else {
                return false;
            };
            should_drain
        };
        if should_drain {
            self.drain_events();
        }
        true
    }

    fn emit_canonical_sound(
        &self,
        source: CanonicalSourceAdmission,
        sound_name: String,
        source_position: [f64; 3],
        volume: f64,
        pitch: f64,
    ) -> bool {
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.canonical_source_still_valid_locked(source)
                || !self.command_execution_allowed_without_lock()
            {
                return false;
            }
            let sound_sequence = self.sound_packet_sequence.fetch_add(1, Ordering::AcqRel) + 1;
            let payload = BackendEventPayload::Sound(ContractProtocolSoundPayload {
                event_type: ContractHeardSoundType::Heard,
                sound_key: format!("sound-{}-{sound_sequence}", source.epoch),
                sound_name: Some(sound_name),
                sound_id: None,
                category: None,
                source_position: ContractVec3Value {
                    x: source_position[0],
                    y: source_position[1],
                    z: source_position[2],
                },
                volume,
                pitch,
                protocol_source: ContractProtocolSoundSource::NamedSoundEffect,
            });
            let Some(should_drain) = self.enqueue_entity_event_if_running_locked(
                source.epoch,
                FactSource::ServerObserved,
                payload,
            ) else {
                return false;
            };
            should_drain
        };
        if should_drain {
            self.drain_events();
        }
        true
    }

    /// Construct and enqueue one event. The caller may hold command admission,
    /// but this function never drains the queue or invokes a subscriber.
    fn enqueue_event(&self, source: FactSource, payload: BackendEventPayload) -> bool {
        self.enqueue_event_at(source, payload, now_utc().to_rfc3339())
    }

    fn enqueue_event_at(
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

    fn enqueue_dispatch_locked(
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
    fn emit(&self, source: FactSource, payload: BackendEventPayload) {
        let should_drain = self.enqueue_event(source, payload);
        if should_drain {
            self.drain_events();
        }
    }

    /// Normal product/protocol events must linearize their admission check and
    /// queue insertion. Stop takes the same lock, so a losing late event is
    /// discarded before it can appear after `stopped`.
    fn emit_if_running(&self, source: FactSource, payload: BackendEventPayload) -> bool {
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

    fn enqueue_event_if_running_locked(
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

    fn lifecycle_event_allowed_without_lock(&self) -> bool {
        !self.stopping.load(Ordering::Acquire) && !self.stopped_reported.load(Ordering::Acquire)
    }

    /// 排水期间不持有 dispatch 或 observation registry 锁。callback 内重新
    /// emit 只会把事件追加到队尾，由当前 drainer 在本事件后继续处理。
    fn drain_events(&self) {
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

    fn broadcast_event(&self, event: BackendEventEnvelope) {
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
                .filter_map(|subscriber| {
                    subscriber
                        .state
                        .reserve_callback()
                        .then(|| ObservationDelivery {
                            listener: subscriber.listener.clone(),
                            state: subscriber.state.clone(),
                            id: subscriber.id,
                        })
                })
                .collect::<Vec<_>>()
        };

        for delivery in deliveries {
            if !delivery.state.start_callback() {
                continue;
            }
            let callback_guard = ObservationCallbackGuard::enter(&delivery.state);
            let callback_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                delivery.listener.on_event(observation_event.clone());
            }));
            drop(callback_guard);
            delivery.state.finish_callback();
            if callback_result.is_err() {
                eprintln!(
                    "observation listener panic isolated: subscription_id={}; other listeners continue",
                    delivery.id
                );
            }
        }
    }

    /// 在发起网络连接前分配身份，并保证该身份下的第一条生命周期事件就是
    /// `connection_requested`。dispatch 锁覆盖身份分配、事件编号和广播，避免
    /// 其他生产者把事件插入新 attempt 与请求事件之间。
    fn begin_connection_attempt(&self) -> bool {
        let should_drain = {
            let _admission = self.command_admission.lock();
            let Some(should_drain) = self.begin_connection_attempt_locked(None) else {
                return false;
            };
            should_drain
        };
        if should_drain {
            self.drain_events();
        }
        self.phase_cancel.notify_waiters();
        self.stable_cancel.notify_waiters();
        true
    }

    fn begin_connection_attempt_locked(&self, reconnect_token: Option<u64>) -> Option<bool> {
        if self.stopping.load(Ordering::Acquire) || self.stop_requested.load(Ordering::Acquire) {
            return None;
        }
        if let Some(token) = reconnect_token {
            if self.reconnect_attempt_token.load(Ordering::Acquire) != token {
                return None;
            }
            self.reconnect_add_pending.store(true, Ordering::Release);
        }
        let (epoch, retry_ordinal) = {
            let writer = self.writer.lock();
            (
                writer.connection_epoch.checked_add(1)?,
                self.retry_ordinal.load(Ordering::Acquire).checked_add(1)?,
            )
        };
        self.disconnect_reported.store(false, Ordering::Release);
        self.stopped_reported.store(false, Ordering::Release);
        self.faulted_reported.store(false, Ordering::Release);
        self.shutdown_requested.store(false, Ordering::Release);
        self.active_client.lock().take();
        self.invalidate_phase_locked();
        self.invalidate_stable_reset_locked();
        if !self.stop_requested.load(Ordering::Acquire) {
            self.dispatch_cancelled.store(false, Ordering::Release);
        }
        *self.stop_reason.lock() = None;
        *self.last_close.lock() = None;
        *self.last_failure.lock() = None;
        {
            let mut producer = self.entity_producer.lock();
            producer.source_fence.begin_attempt();
            producer.owner = None;
            producer.attempt = AttemptAdmissionState::NotStarted;
            producer.pending_connection_failure = None;
            producer.deactivate_scope();
        }
        self.clear_observations();
        self.lifecycle_revision.fetch_add(1, Ordering::AcqRel);

        let mut dispatch = self.event_dispatch.lock();
        let (event, epoch, attempt_id, attempt) = {
            let mut writer = self.writer.lock();
            writer.new_attempt(epoch);
            self.retry_ordinal.store(retry_ordinal, Ordering::Release);
            let attempt_id = writer.connection_attempt_id.clone();
            let attempt = u32::try_from(retry_ordinal).unwrap_or(u32::MAX);
            (
                writer.emit(
                    FactSource::Commanded,
                    BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionRequested {
                        attempt,
                    }),
                ),
                epoch,
                attempt_id,
                attempt,
            )
        };
        {
            let mut producer = self.entity_producer.lock();
            producer.attempt = AttemptAdmissionState::Reserved {
                epoch,
                reconnect_token,
                join_started_epoch: None,
                attempt_token: None,
            };
        }
        self.set_backend_state(BackendState::Connecting {
            epoch,
            attempt_id,
            attempt,
        });
        Some(self.enqueue_dispatch_locked(&mut dispatch, event))
    }

    /// `Event::Init` 消费连接发起前预留的身份，而不是再创建一个 epoch。
    /// 防御性 fallback 仍走同一入口，确保即使 Azalea 新增调用路径，也先有
    /// `connection_requested`，随后才发 transport 生命周期事件。
    #[cfg(test)]
    fn admit_canonical_join_started(&self, source_epoch: u64) -> bool {
        self.admit_canonical_join_started_with_token(source_epoch, None)
    }

    /// Stamped variant of [`Self::admit_canonical_join_started`]: the vendor
    /// `StartJoinServerEvent.attempt_token` is bound one-to-one to the
    /// reservation's epoch. The legacy source fence only applies to the
    /// tokenless path.
    fn admit_canonical_join_started_with_token(
        &self,
        source_epoch: u64,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> bool {
        let _admission = self.command_admission.lock();
        if self.writer.lock().connection_epoch != source_epoch {
            return false;
        }
        let mut producer = self.entity_producer.lock();
        if let Some(token) = attempt_token {
            // Stamped source: the token must not already belong to another
            // epoch and this epoch must not already belong to another token.
            if !producer.source_token_bindings.bind(token, source_epoch) {
                return false;
            }
        } else if !producer.source_fence.allows_unstamped_global() {
            // Init/StartJoin has no token.  In a same-entity reconnect window
            // it cannot safely consume the reservation belonging to B.
            return false;
        }
        let AttemptAdmissionState::Reserved {
            epoch,
            reconnect_token,
            join_started_epoch,
            attempt_token: reserved_token,
        } = producer.attempt
        else {
            return false;
        };
        if epoch != source_epoch
            || reserved_token.is_some_and(|reserved| Some(reserved) != attempt_token)
            || reconnect_token.is_some_and(|token| {
                self.reconnect_attempt_token.load(Ordering::Acquire) != token
                    || !self.reconnect_add_pending.load(Ordering::Acquire)
            })
        {
            return false;
        }
        if join_started_epoch == Some(source_epoch) {
            return true;
        }
        producer.attempt = AttemptAdmissionState::Reserved {
            epoch,
            reconnect_token,
            join_started_epoch: Some(source_epoch),
            attempt_token,
        };
        true
    }

    fn bind_reserved_attempt_locked(
        &self,
        entity: bevy_ecs::entity::Entity,
        expected_reconnect_token: Option<u64>,
        init_path: bool,
        expected_attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<(u64, bool, Option<PhaseDeadlineToken>)> {
        if self.stopping.load(Ordering::Acquire) {
            return None;
        }
        if let Some(token) = expected_reconnect_token {
            if !self.reconnect_add_pending.load(Ordering::Acquire)
                || self.reconnect_attempt_token.load(Ordering::Acquire) != token
            {
                return None;
            }
        }

        let epoch = self.writer.lock().connection_epoch;
        let mut producer = self.entity_producer.lock();
        let attempt = producer.attempt;
        let source_token = expected_attempt_token;
        if source_token.is_some_and(|token| !producer.source_token_bindings.matches(token, epoch)) {
            // A stamped client must already be bound one-to-one to this exact
            // epoch by its StartJoinServerEvent; a token from a different
            // attempt can never consume this reservation.
            return None;
        }
        if init_path
            && matches!(
                attempt,
                AttemptAdmissionState::Reserved {
                    reconnect_token: Some(_),
                    ..
                }
            )
        {
            // `Event::Init` has no reconnect token.  It must not consume B's
            // reservation, even when the returned client happens to use a
            // different Bevy entity.  Only the stamped reconnect-return path
            // can prove which reservation it belongs to.
            return None;
        }
        if init_path
            && matches!(attempt, AttemptAdmissionState::Reserved { .. })
            && source_token.is_none()
            && !producer.source_fence.allows_unstamped(entity)
        {
            return None;
        }
        if let Some(token) = source_token {
            // Idempotent re-registration: the binding was established by
            // StartJoinServerEvent and must still be exact.
            if !producer.source_token_bindings.bind(token, epoch) {
                return None;
            }
        }
        drop(producer);
        match attempt {
            AttemptAdmissionState::Reserved {
                epoch: reserved_epoch,
                reconnect_token,
                attempt_token: reserved_attempt_token,
                ..
            } if reserved_epoch == epoch
                && (init_path || reconnect_token == expected_reconnect_token)
                && reserved_attempt_token == source_token =>
            {
                self.clear_observations();
                self.bind_entity_producer_owner_locked(entity, epoch);
                let mut producer = self.entity_producer.lock();
                producer.attempt = AttemptAdmissionState::Bound {
                    epoch,
                    entity,
                    reconnect_token,
                    attempt_token: source_token,
                };
                drop(producer);
            }
            AttemptAdmissionState::Bound {
                epoch: bound_epoch,
                entity: bound_entity,
                reconnect_token,
                attempt_token: bound_attempt_token,
            } if bound_epoch == epoch
                && bound_entity == entity
                && (init_path || reconnect_token == expected_reconnect_token)
                && bound_attempt_token == source_token =>
            {
                return Some((epoch, false, None));
            }
            _ => return None,
        }

        if !self.command_execution_allowed_without_lock() {
            return Some((epoch, false, None));
        }
        let (attempt_id, attempt) = {
            let writer = self.writer.lock();
            (
                writer.connection_attempt_id.clone(),
                u32::try_from(self.retry_ordinal.load(Ordering::Acquire)).unwrap_or(u32::MAX),
            )
        };
        self.set_backend_state(BackendState::LoggingIn {
            epoch,
            attempt_id,
            attempt,
        });
        self.invalidate_stable_reset_locked();
        let phase_token = self.arm_phase_deadline_locked(TransportPhase::LoggingIn);
        let should_drain = self.enqueue_event(
            FactSource::ServerObserved,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected),
        );
        Some((epoch, should_drain, phase_token))
    }

    fn bind_reserved_attempt(
        self: &Arc<Self>,
        entity: bevy_ecs::entity::Entity,
        expected_reconnect_token: Option<u64>,
        init_path: bool,
        expected_attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<u64> {
        let (epoch, should_drain, phase_token) = {
            let _admission = self.command_admission.lock();
            self.bind_reserved_attempt_locked(
                entity,
                expected_reconnect_token,
                init_path,
                expected_attempt_token,
            )?
        };
        if should_drain {
            self.drain_events();
        }
        self.phase_cancel.notify_waiters();
        if let Some(token) = phase_token {
            self.spawn_phase_deadline(token);
        }
        Some(epoch)
    }

    #[cfg(test)]
    fn consume_attempt_for_transport_init_and_bind(
        self: &Arc<Self>,
        entity: bevy_ecs::entity::Entity,
    ) -> Option<u64> {
        self.consume_attempt_for_transport_init_and_bind_with_token(entity, None)
    }

    fn consume_attempt_for_transport_init_and_bind_with_token(
        self: &Arc<Self>,
        entity: bevy_ecs::entity::Entity,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<u64> {
        let (epoch, should_drain, phase_token) = {
            let _admission = self.command_admission.lock();
            if self.stopping.load(Ordering::Acquire) {
                return None;
            }

            let needs_fallback = {
                let producer = self.entity_producer.lock();
                matches!(producer.attempt, AttemptAdmissionState::NotStarted)
            };
            let mut should_drain = false;
            if needs_fallback {
                should_drain = self.begin_connection_attempt_locked(None)?;
            }
            self.disconnect_reported.store(false, Ordering::Release);
            let (epoch, bind_should_drain, phase_token) =
                self.bind_reserved_attempt_locked(entity, None, true, attempt_token)?;
            (epoch, should_drain || bind_should_drain, phase_token)
        };
        if should_drain {
            self.drain_events();
        }
        self.phase_cancel.notify_waiters();
        if let Some(token) = phase_token {
            self.spawn_phase_deadline(token);
        }
        Some(epoch)
    }

    #[cfg(test)]
    fn bind_reconnect_return(
        self: &Arc<Self>,
        token: u64,
        entity: bevy_ecs::entity::Entity,
    ) -> Option<u64> {
        self.bind_reconnect_return_with_token(token, entity, None)
    }

    fn bind_reconnect_return_with_token(
        self: &Arc<Self>,
        token: u64,
        entity: bevy_ecs::entity::Entity,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<u64> {
        self.bind_reserved_attempt(entity, Some(token), false, attempt_token)
    }

    #[cfg(test)]
    fn claim_reconnect(&self) -> bool {
        self.claim_reconnect_with_token(None)
    }

    fn claim_reconnect_with_token(
        &self,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> bool {
        let _admission = self.command_admission.lock();
        if self.stopping.load(Ordering::Acquire) || self.reconnect_pending.load(Ordering::Acquire) {
            return false;
        }
        // A SwarmEvent::Disconnect has no client entity and can also be
        // emitted by an old per-add copy task.  Once a current owner is bound,
        // only the canonical ECS disconnect admission may authorize it.  The
        // precise source runs before the high-level listener, so a real
        // current disconnect has already set this bit.
        let disconnect_reported = self.disconnect_reported.load(Ordering::Acquire);
        let current_epoch = self.writer.lock().connection_epoch;
        let (owner, attempt) = {
            let producer = self.entity_producer.lock();
            if let Some(token) = attempt_token {
                // Stamped swarm disconnect: the token must belong to the
                // current epoch. A stale A swarm copy can never claim B.
                if !producer.source_token_bindings.matches(token, current_epoch) {
                    return false;
                }
            } else if producer.source_fence.ambiguous
                || producer.source_fence.pending_rebind_entity.is_some()
            {
                // An entity-less swarm event without a token cannot prove
                // whether it belongs to A or B.  It must not close the
                // current B owner.
                return false;
            }
            (producer.owner, producer.attempt)
        };
        if !disconnect_reported
            && (owner.is_some()
                || matches!(
                    attempt,
                    AttemptAdmissionState::Reserved {
                        reconnect_token: Some(_),
                        ..
                    }
                ))
        {
            // A current bound attempt, or a reconnect reservation that is
            // already being added, must first receive its canonical close
            // evidence.  This is the finite barrier that keeps an old
            // entity-less swarm copy from claiming B.
            return false;
        }
        self.reconnect_pending.store(true, Ordering::Release);
        // SwarmEvent::Disconnect is the lifecycle-wide fallback and carries
        // no client entity. Deactivate the owner selected by this same
        // admission; a late entity-specific Event::Disconnect cannot use this
        // path because it remains owner-gated.
        self.deactivate_current_entity_producer_owner();
        true
    }

    fn admit_reconnect_attempt(&self) -> Option<u64> {
        let (token, should_drain) = {
            let _admission = self.command_admission.lock();
            if self.stopping.load(Ordering::Acquire)
                || !self.reconnect_pending.load(Ordering::Acquire)
            {
                return None;
            }
            let token = checked_atomic_increment(&self.reconnect_attempt_token)?;
            let Some(should_drain) = self.begin_connection_attempt_locked(Some(token)) else {
                return None;
            };
            (token, should_drain)
        };
        if should_drain {
            self.drain_events();
        }
        self.phase_cancel.notify_waiters();
        self.stable_cancel.notify_waiters();
        Some(token)
    }

    fn reconnect_add_is_allowed(&self, token: u64) -> bool {
        let _admission = self.command_admission.lock();
        !self.stopping.load(Ordering::Acquire)
            && self.reconnect_add_pending.load(Ordering::Acquire)
            && self.reconnect_attempt_token.load(Ordering::Acquire) == token
    }

    fn finish_reconnect_attempt(&self, token: u64) {
        let _admission = self.command_admission.lock();
        if self.reconnect_attempt_token.load(Ordering::Acquire) == token {
            self.reconnect_add_pending.store(false, Ordering::Release);
            let mut producer = self.entity_producer.lock();
            if matches!(
                producer.attempt,
                AttemptAdmissionState::Reserved {
                    reconnect_token: Some(current),
                    ..
                } if current == token
            ) {
                let epoch = self.writer.lock().connection_epoch;
                producer.attempt = AttemptAdmissionState::Closed { epoch };
                producer.deactivate_scope();
            }
        }
        self.reconnect_pending.store(false, Ordering::Release);
    }

    fn context(&self) -> (String, u64, String) {
        self.writer.lock().context()
    }

    fn set_dimension(&self, dimension: impl Into<String>) -> Option<String> {
        let dimension = dimension.into();
        self.writer.lock().set_dimension(dimension.clone());
        self.reported_dimension.lock().replace(dimension)
    }

    fn set_dimension_if_running(&self, dimension: impl Into<String>) -> bool {
        let _admission = self.command_admission.lock();
        if !self.command_execution_allowed_without_lock() {
            return false;
        }
        self.set_dimension(dimension);
        true
    }

    #[cfg(test)]
    fn observe_dimension(&self, dimension: impl Into<String>) {
        let dimension = dimension.into();
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock() {
                return;
            }
            let Some(previous) = self.set_dimension(dimension.clone()) else {
                return;
            };
            if previous == dimension {
                return;
            }
            self.enqueue_event(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::DimensionChanged {
                    from: previous,
                    to: dimension,
                }),
            )
        };
        if should_drain {
            self.drain_events();
        }
    }

    /// Consume the WorldLoaded boundary only when it still belongs to the
    /// current unstamped owner.  Dimension metadata and its optional event
    /// must share the same admission as the owner/fence check; a separate
    /// check-then-write would let a delayed A boundary update B.
    #[cfg(test)]
    fn observe_dimension_from_world_boundary(
        &self,
        entity: bevy_ecs::entity::Entity,
        dimension: impl Into<String>,
    ) -> bool {
        self.observe_dimension_from_world_boundary_with_token(entity, dimension, None)
    }

    fn observe_dimension_from_world_boundary_with_token(
        &self,
        entity: bevy_ecs::entity::Entity,
        dimension: impl Into<String>,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> bool {
        let dimension = dimension.into();
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock() {
                return false;
            }
            let epoch = self.writer.lock().connection_epoch;
            let producer = self.entity_producer.lock();
            if producer.owner != Some((entity, epoch)) {
                return false;
            }
            let admitted = match attempt_token {
                Some(token) => producer.source_token_bindings.matches(token, epoch),
                None => producer.source_fence.allows_unstamped(entity),
            };
            if !admitted {
                return false;
            }
            drop(producer);

            let Some(previous) = self.set_dimension(dimension.clone()) else {
                return true;
            };
            if previous == dimension {
                return true;
            }
            self.enqueue_event(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::DimensionChanged {
                    from: previous,
                    to: dimension,
                }),
            )
        };
        if should_drain {
            self.drain_events();
        }
        true
    }

    fn connection_epoch(&self) -> u64 {
        self.writer.lock().connection_epoch
    }

    fn add_observation_subscription(
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

    fn remove_observation_subscription(&self, id: u64, state: &ObservationSubscriptionState) {
        {
            let mut subscribers = self.observation_subscribers.lock();
            subscribers.retain(|subscriber| subscriber.id != id);
            state.close();
        }
        state.wait_for_quiescence();
    }

    fn set_swarm(&self, swarm: Swarm) -> bool {
        let _admission = self.command_admission.lock();
        if self.stopping.load(Ordering::Acquire) || self.stopped_reported.load(Ordering::Acquire) {
            return false;
        }
        *self.swarm.lock() = Some(swarm);
        true
    }

    fn set_active_client_if_current(&self, client: &Client) -> bool {
        let _admission = self.command_admission.lock();
        let epoch = self.writer.lock().connection_epoch;
        if !self.command_execution_allowed_without_lock()
            || self.entity_producer.lock().owner != Some((client.entity, epoch))
        {
            return false;
        }
        let producer = self.entity_producer.lock();
        let source_token_matches = match client.attempt_token() {
            Some(token) => producer.source_token_bindings.matches(token, epoch),
            None => producer.source_fence.allows_unstamped(client.entity),
        };
        if !source_token_matches {
            return false;
        }
        drop(producer);
        *self.active_client.lock() = Some(client.clone());
        true
    }

    /// High-level `Event`s arrive paired with their `Client`.  Every side
    /// effect must first prove that the client still belongs to the current
    /// bound owner: a stamped client's token must match the current epoch's
    /// one-to-one binding, and an unstamped (legacy) client is only admitted
    /// while the legacy source fence still allows it.
    fn client_is_current_owner(&self, client: &Client) -> bool {
        let _admission = self.command_admission.lock();
        let epoch = self.writer.lock().connection_epoch;
        let producer = self.entity_producer.lock();
        if producer.owner != Some((client.entity, epoch)) {
            return false;
        }
        match client.attempt_token() {
            Some(token) => producer.source_token_bindings.matches(token, epoch),
            None => producer.source_fence.allows_unstamped(client.entity),
        }
    }

    fn set_world_if_running(&self, world: SharedWorld) -> bool {
        let _admission = self.command_admission.lock();
        if !self.command_execution_allowed_without_lock() {
            return false;
        }
        let (current_epoch, current_dimension) = {
            let writer = self.writer.lock();
            (writer.connection_epoch, writer.dimension.clone())
        };
        let (current_scope_generation, current_owner_matches_epoch) = {
            let producer = self.entity_producer.lock();
            (
                producer.scope_generation,
                producer
                    .owner
                    .is_some_and(|(_, epoch)| epoch == current_epoch),
            )
        };
        let mut observation = self.observation.write();
        let replaced = observation
            .world
            .as_ref()
            .is_none_or(|current| !Arc::ptr_eq(current, &world));
        observation.world = Some(world);
        if replaced {
            observation.snapshot = None;
            observation.snapshot_scope_generation = 0;
            observation.source = None;
            observation.tracked_entities.clear();
            observation.entity_residuals.clear();
            let preserve_light = current_owner_matches_epoch
                && observation
                    .light_cache
                    .context
                    .as_ref()
                    .is_some_and(|context| {
                        context.epoch == current_epoch
                            && context.scope_generation == current_scope_generation
                            && current_dimension.as_deref() == Some(context.dimension.as_str())
                    });
            if !preserve_light {
                observation.light_cache.clear();
            }
        }
        observation.bump_generation();
        true
    }

    fn clear_observations(&self) {
        *self.reported_dimension.lock() = None;
        #[cfg(test)]
        self.invoke_observation_write_boundary_hook();
        let mut observation = self.observation.write();
        observation.world = None;
        observation.snapshot = None;
        observation.snapshot_scope_generation = 0;
        observation.source = None;
        observation.tracked_entities.clear();
        observation.entity_residuals.clear();
        observation.clear_all_frame_facts();
        observation.bump_generation();
    }

    fn close_evidence(&self, reason: Option<String>) -> CloseEvidence {
        let text = reason.clone().unwrap_or_default();
        let lower = text.to_ascii_lowercase();
        if text == "deliberate_stop" {
            return CloseEvidence {
                code: "deliberate_stop".to_owned(),
                retryable: false,
                deliberate: true,
                kick: None,
                error: None,
                end_reason: Some(text),
                failure: None,
            };
        }

        // A component attached to Event::Disconnect is already kick evidence;
        // its wording must not downgrade an unclassified kick to a retryable
        // ordinary connection end.
        let during_login = !self.ready.load(Ordering::Acquire);
        let server_shutdown = lower.contains("server_shutdown")
            || lower.contains("server shutdown")
            || lower.contains("server closed")
            || lower.contains("server restarting");
        if server_shutdown {
            return CloseEvidence {
                code: "server_shutdown".to_owned(),
                retryable: true,
                deliberate: false,
                kick: reason.map(|text| BackendKick { text, during_login }),
                error: None,
                end_reason: Some(text),
                failure: None,
            };
        }
        if lower.contains("banned")
            || lower.contains("whitelist")
            || lower.contains("invalid session")
            || lower.contains("authentication")
            || lower.contains("not authenticated")
        {
            let failure_code = if lower.contains("auth") || lower.contains("session") {
                BackendFailureCode::AuthenticationFailed
            } else {
                BackendFailureCode::PermissionDenied
            };
            return CloseEvidence {
                code: "permission_denied".to_owned(),
                retryable: false,
                deliberate: false,
                kick: Some(BackendKick {
                    text: text.clone(),
                    during_login,
                }),
                error: None,
                end_reason: Some(text.clone()),
                failure: Some(BackendFailure {
                    code: failure_code,
                    message: text,
                    retryable: false,
                }),
            };
        }
        if reason.is_some() {
            return CloseEvidence {
                code: "unclassified_kick".to_owned(),
                retryable: false,
                deliberate: false,
                kick: reason.map(|text| BackendKick { text, during_login }),
                error: None,
                end_reason: Some(text.clone()),
                failure: Some(BackendFailure {
                    code: BackendFailureCode::PermissionDenied,
                    message: text,
                    retryable: false,
                }),
            };
        }
        CloseEvidence {
            code: "connection_ended".to_owned(),
            retryable: true,
            deliberate: false,
            kick: None,
            error: None,
            end_reason: None,
            failure: None,
        }
    }

    #[cfg(test)]
    fn emit_transport_connected(&self) {
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock() {
                return;
            }
            let (epoch, attempt_id, attempt) = self.connection_identity();
            self.set_backend_state(BackendState::LoggingIn {
                epoch,
                attempt_id,
                attempt,
            });
            self.enqueue_event(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected),
            )
        };
        if should_drain {
            self.drain_events();
        }
    }

    fn emit_logged_in(self: &Arc<Self>, version: impl Into<String>, dimension: String) {
        let version = version.into();
        let (should_drain, phase_token) = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock() {
                return;
            }
            self.set_dimension(dimension.clone());
            let (epoch, attempt_id, attempt) = self.connection_identity();
            self.invalidate_stable_reset_locked();
            self.set_backend_state(BackendState::Spawning {
                epoch,
                attempt_id,
                attempt,
            });
            let phase_token = self.arm_phase_deadline_locked(TransportPhase::Spawning);
            let should_drain = self.enqueue_event(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::LoggedIn {
                    version,
                    dimension,
                }),
            );
            (should_drain, phase_token)
        };
        if should_drain {
            self.drain_events();
        }
        self.phase_cancel.notify_waiters();
        if let Some(token) = phase_token {
            self.spawn_phase_deadline(token);
        }
    }

    fn emit_ready(self: &Arc<Self>, snapshot_revision: u64) {
        let (should_drain, stable_token) = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock() {
                return;
            }
            self.ready.store(true, Ordering::Release);
            let (epoch, attempt_id, _) = self.connection_identity();
            let ready_at = now_utc().to_rfc3339();
            self.invalidate_phase_locked();
            let stable_token = self.arm_stable_reset_locked();
            self.set_backend_state(BackendState::Ready {
                epoch,
                attempt_id,
                ready_at: ready_at.clone(),
            });
            let should_drain = self.enqueue_event_at(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::Ready {
                    snapshot_revision,
                }),
                ready_at,
            );
            (should_drain, stable_token)
        };
        if should_drain {
            self.drain_events();
        }
        self.phase_cancel.notify_waiters();
        self.stable_cancel.notify_waiters();
        if let Some(token) = stable_token {
            self.spawn_stable_reset(token);
        }
    }

    #[cfg(test)]
    fn admit_death(&self) -> bool {
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock()
                || self.death_reported.swap(true, Ordering::AcqRel)
            {
                return false;
            }
            self.ready.store(false, Ordering::Release);
            let (epoch, attempt_id, _) = self.connection_identity();
            let died_at = now_utc().to_rfc3339();
            self.set_backend_state(BackendState::Dead {
                epoch,
                attempt_id,
                died_at: died_at.clone(),
            });
            self.enqueue_event_at(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::Died),
                died_at,
            )
        };
        if should_drain {
            self.drain_events();
        }
        true
    }

    /// Claim Death and finish all synchronous local movement cleanup before
    /// making `died` visible to subscribers. The event queue may already have
    /// another drainer, so enqueueing first and draining later would still let
    /// a re-entrant stop callback run before the physical release.
    fn admit_death_and_release(&self, release_inputs: impl FnOnce() -> bool) -> Option<bool> {
        let (released, should_drain) = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock()
                || self.death_reported.swap(true, Ordering::AcqRel)
            {
                return None;
            }
            self.ready.store(false, Ordering::Release);
            let (epoch, attempt_id, _) = self.connection_identity();
            let died_at = now_utc().to_rfc3339();
            self.set_backend_state(BackendState::Dead {
                epoch,
                attempt_id,
                died_at: died_at.clone(),
            });

            let movement_id = self.active_movement_id.lock().clone();
            let had_movement = movement_id.is_some()
                || self.active_movement_completion.lock().is_some()
                || self.active_movement_registration.load(Ordering::Acquire);
            let completion = self.active_movement_completion.lock().clone();
            let cancel_signal = self.active_movement_cancel_signal.lock().clone();
            if had_movement {
                self.movement_generation.fetch_add(1, Ordering::AcqRel);
                if let Some(completion) = completion.as_ref() {
                    completion.cancel("movement stopped by death".to_owned(), true);
                }
                if let Some(signal) = cancel_signal.as_ref() {
                    signal.notify_one();
                }
            }

            // This closure is synchronous and runs before `died` is enqueued;
            // no subscriber/callback can run while command admission is held.
            let released = release_inputs();
            self.active_movement.store(false, Ordering::Release);
            *self.active_movement_id.lock() = None;
            self.active_movement_cancel_signal.lock().take();
            self.active_movement_completion.lock().take();
            self.active_movement_registration
                .store(false, Ordering::Release);
            if let Some(completion) = completion {
                finish_command(
                    &Some(completion),
                    if released {
                        Err(BackendError::Cancelled {
                            operation: "movement stopped by death".to_owned(),
                        })
                    } else {
                        Err(command_component_failure("death move"))
                    },
                );
            }
            let should_drain = self.enqueue_event_at(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::Died),
                died_at,
            );
            (released, should_drain)
        };
        if should_drain {
            self.drain_events();
        }
        Some(released)
    }

    #[cfg(test)]
    fn emit_died(&self) {
        let _ = self.admit_death();
    }

    fn emit_respawn_transition_started(&self, from_dimension: String) {
        self.emit_if_running(
            FactSource::Commanded,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::RespawnTransitionStarted {
                from_dimension,
            }),
        );
    }

    fn emit_respawned(&self, dimension: String) {
        self.emit_if_running(
            FactSource::ServerObserved,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Respawned { dimension }),
        );
    }

    fn mark_disconnected(&self, reason: Option<String>) -> BackendClose {
        self.mark_disconnected_evidence(self.close_evidence(reason))
    }

    #[cfg(test)]
    fn mark_connection_failed(&self, error: String) -> BackendClose {
        self.mark_disconnected_evidence(CloseEvidence {
            code: "connection_failed".to_owned(),
            retryable: true,
            deliberate: false,
            kick: None,
            error: Some(BackendCloseError {
                name: "connection_failed".to_owned(),
                message: error.clone(),
                code: None,
            }),
            end_reason: Some(error.clone()),
            failure: Some(BackendFailure {
                code: BackendFailureCode::ProtocolError,
                message: error,
                retryable: true,
            }),
        })
    }

    fn mark_disconnected_evidence(&self, evidence: CloseEvidence) -> BackendClose {
        self.mark_disconnected_evidence_with_owner(evidence, None, None, false, None, None)
            .expect("unconditional disconnect admission cannot be rejected")
    }

    #[cfg(test)]
    fn admit_canonical_disconnected(
        &self,
        entity: bevy_ecs::entity::Entity,
        source_epoch: u64,
        reason: Option<String>,
    ) -> Option<BackendClose> {
        self.admit_canonical_disconnected_with_token(entity, source_epoch, reason, None)
    }

    fn admit_canonical_disconnected_with_token(
        &self,
        entity: bevy_ecs::entity::Entity,
        source_epoch: u64,
        reason: Option<String>,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<BackendClose> {
        self.mark_disconnected_evidence_with_owner(
            self.close_evidence(reason),
            Some(entity),
            Some(source_epoch),
            true,
            None,
            attempt_token,
        )
    }

    fn admit_canonical_disconnected_source_with_token(
        &self,
        entity: bevy_ecs::entity::Entity,
        reason: Option<String>,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<BackendClose> {
        let source_epoch = self.connection_epoch();
        self.admit_canonical_disconnected_with_token(entity, source_epoch, reason, attempt_token)
    }

    #[cfg(test)]
    fn admit_canonical_connection_failed(
        &self,
        entity: bevy_ecs::entity::Entity,
        source_epoch: u64,
        error: String,
    ) -> Option<BackendClose> {
        self.admit_canonical_connection_failed_with_token(entity, source_epoch, error, None)
    }

    fn admit_canonical_connection_failed_with_token(
        &self,
        entity: bevy_ecs::entity::Entity,
        source_epoch: u64,
        error: String,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<BackendClose> {
        self.mark_disconnected_evidence_with_owner(
            CloseEvidence {
                code: "connection_failed".to_owned(),
                retryable: true,
                deliberate: false,
                kick: None,
                error: Some(BackendCloseError {
                    name: "connection_failed".to_owned(),
                    message: error.clone(),
                    code: None,
                }),
                end_reason: Some(error.clone()),
                failure: Some(BackendFailure {
                    code: BackendFailureCode::ProtocolError,
                    message: error,
                    retryable: true,
                }),
            },
            Some(entity),
            Some(source_epoch),
            true,
            Some(entity),
            attempt_token,
        )
    }

    fn admit_canonical_connection_failed_source_with_token(
        &self,
        entity: bevy_ecs::entity::Entity,
        error: String,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<BackendClose> {
        let source_epoch = self.connection_epoch();
        self.admit_canonical_connection_failed_with_token(
            entity,
            source_epoch,
            error,
            attempt_token,
        )
    }

    #[cfg(test)]
    fn take_canonical_connection_failure_followup(&self, entity: bevy_ecs::entity::Entity) -> bool {
        self.take_canonical_connection_failure_followup_with_token(entity, None)
    }

    fn take_canonical_connection_failure_followup_with_token(
        &self,
        entity: bevy_ecs::entity::Entity,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> bool {
        let _admission = self.command_admission.lock();
        let epoch = self.writer.lock().connection_epoch;
        let mut producer = self.entity_producer.lock();
        let pending_matches = producer.pending_connection_failure == Some((entity, epoch))
            && attempt_token
                .is_none_or(|token| producer.source_token_bindings.matches(token, epoch));
        if pending_matches {
            producer.pending_connection_failure = None;
            true
        } else {
            false
        }
    }

    fn mark_disconnected_evidence_with_owner(
        &self,
        evidence: CloseEvidence,
        entity: Option<bevy_ecs::entity::Entity>,
        expected_epoch: Option<u64>,
        allow_unbound_attempt: bool,
        failure_entity: Option<bevy_ecs::entity::Entity>,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<BackendClose> {
        let (close, should_drain, duplicate_cleanup) = {
            let _admission = self.command_admission.lock();
            if expected_epoch.is_some_and(|epoch| self.connection_epoch() != epoch) {
                return None;
            }
            if let Some(entity) = entity {
                let source_epoch = expected_epoch.unwrap_or_else(|| self.connection_epoch());
                if !self.admit_entity_lifecycle_owner_locked(
                    entity,
                    source_epoch,
                    allow_unbound_attempt,
                    attempt_token,
                ) {
                    return None;
                }
            }
            self.mark_disconnected_evidence_locked(evidence, failure_entity)
        };

        // A duplicate can race a registration that is finishing after the
        // first disconnect. Repeating cleanup is harmless and helps that
        // registration converge, while the first close already completed its
        // mandatory cleanup under admission above.
        if duplicate_cleanup {
            self.cancel_active_movement(true);
            self.cancel_pending_commands();
            self.clear_observations();
        }
        if should_drain {
            self.drain_events();
        }
        Some(close)
    }

    /// Caller holds `command_admission`; owner claiming and global close
    /// admission therefore share one lifecycle linearization point.
    fn mark_disconnected_evidence_locked(
        &self,
        evidence: CloseEvidence,
        failure_entity: Option<bevy_ecs::entity::Entity>,
    ) -> (BackendClose, bool, bool) {
        if self.stopped_reported.load(Ordering::Acquire) {
            let close = self
                .last_close
                .lock()
                .clone()
                .unwrap_or_else(|| BackendClose {
                    epoch: self.connection_epoch(),
                    at: now_utc().to_rfc3339(),
                    code: "connection_ended".to_owned(),
                    retryable: true,
                    deliberate: false,
                    kick: None,
                    error: None,
                    end_reason: None,
                });
            return (close, false, false);
        }

        // Once stop has won admission, a late Azalea disconnect cannot
        // replace the caller's deliberate close evidence.
        let evidence = if self.stopping.load(Ordering::Acquire) && !evidence.deliberate {
            CloseEvidence {
                code: "deliberate_stop".to_owned(),
                retryable: false,
                deliberate: true,
                kick: None,
                error: None,
                end_reason: Some("deliberate_stop".to_owned()),
                failure: None,
            }
        } else {
            evidence
        };

        // Publish the disconnect bit and enqueue close under one admission
        // point. The queue is drained only after this lock is released.
        if self.disconnect_reported.swap(true, Ordering::AcqRel) {
            let close = self
                .last_close
                .lock()
                .clone()
                .unwrap_or_else(|| BackendClose {
                    epoch: self.connection_epoch(),
                    at: now_utc().to_rfc3339(),
                    code: "connection_ended".to_owned(),
                    retryable: true,
                    deliberate: false,
                    kick: None,
                    error: None,
                    end_reason: None,
                });
            (close, false, true)
        } else {
            self.ready.store(false, Ordering::Release);
            self.invalidate_phase_locked();
            self.invalidate_stable_reset_locked();
            self.active_client.lock().take();
            let close = BackendClose {
                epoch: self.connection_epoch(),
                at: now_utc().to_rfc3339(),
                code: evidence.code,
                retryable: evidence.retryable,
                deliberate: evidence.deliberate,
                kick: evidence.kick,
                error: evidence.error,
                end_reason: evidence.end_reason,
            };
            *self.last_close.lock() = Some(close.clone());
            *self.last_failure.lock() = evidence.failure;

            let mut producer = self.entity_producer.lock();
            producer.owner = None;
            producer.deactivate_scope();
            producer.attempt = AttemptAdmissionState::Closed { epoch: close.epoch };
            producer.pending_connection_failure = if close.code == "connection_failed" {
                failure_entity.map(|entity| (entity, close.epoch))
            } else {
                None
            };
            drop(producer);

            // Seal and clean the attempt before making its close visible.
            // Stop takes the same admission lock, so it cannot enqueue or
            // drain `stopped` between close admission and local cleanup.
            #[cfg(test)]
            self.invoke_disconnect_cleanup_hook();
            self.cancel_active_movement(true);
            self.cancel_pending_commands();
            self.clear_observations();

            let should_drain = self.enqueue_event(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionClosed {
                    close: close.clone(),
                }),
            );
            (close, should_drain, false)
        }
    }

    fn failure_for_close(&self, close: &BackendClose) -> BackendFailure {
        let recorded = self.last_failure.lock().clone();
        // A fatal classification is stronger than the reconnect policy.  In
        // particular, permission/auth/version failures must remain visible
        // instead of being rewritten as `reconnect_disabled`.
        if let Some(failure) = recorded.as_ref().filter(|failure| !failure.retryable) {
            return failure.clone();
        }
        if close.retryable && !self.config.reconnect.enabled {
            return BackendFailure {
                code: BackendFailureCode::ReconnectDisabled,
                message: format!("reconnect disabled after close {}", close.code),
                retryable: false,
            };
        }
        recorded.unwrap_or_else(|| BackendFailure {
            code: BackendFailureCode::ProtocolError,
            message: format!("backend closed with non-retryable code {}", close.code),
            retryable: false,
        })
    }

    fn emit_faulted(&self, failure: BackendFailure) -> bool {
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.lifecycle_event_allowed_without_lock()
                || self.faulted_reported.swap(true, Ordering::AcqRel)
            {
                return false;
            }
            self.set_backend_state(BackendState::Faulted {
                failure: failure.clone(),
            });
            self.enqueue_event(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::Faulted { failure }),
            )
        };
        if should_drain {
            self.drain_events();
        }
        true
    }

    fn emit_reconnect_scheduled(&self, close: &BackendClose) -> Option<Duration> {
        let current_attempt = self.retry_ordinal.load(Ordering::Acquire);
        let next_attempt = current_attempt.checked_add(1)?;
        let schedule = reconnect_schedule_at(
            &self.config.reconnect,
            current_attempt,
            next_reconnect_random(&self.reconnect_rng),
            now_utc(),
        );
        let retry_at = schedule.retry_at.to_rfc3339();
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.lifecycle_event_allowed_without_lock() {
                return None;
            }
            let attempt = u32::try_from(next_attempt).unwrap_or(u32::MAX);
            self.set_backend_state(BackendState::Reconnecting {
                attempt,
                retry_at: retry_at.clone(),
                last_close: close.clone(),
            });
            self.enqueue_event(
                FactSource::ClientPredicted,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::ReconnectScheduled {
                    attempt,
                    retry_at,
                    close_code: close.code.clone(),
                }),
            )
        };
        if should_drain {
            self.drain_events();
        }
        Some(schedule.delay)
    }

    fn exit_swarm(&self) -> bool {
        if let Some(swarm) = self.swarm.lock().clone() {
            swarm.exit();
            true
        } else {
            false
        }
    }

    fn request_shutdown(&self) {
        // `notify_one` 会保留一个 permit，即使 stop() 发生在 run() 开始
        // select 之前，也不会因为时序而永久等待。
        self.shutdown_requested.store(true, Ordering::Release);
        self.shutdown.notify_one();
        self.cancel_event_admission();
    }

    fn cancel_event_admission(&self) {
        self.dispatch_cancelled.store(true, Ordering::Release);
        self.event_dispatch_wake.notify_all();
        self.wake_runtime_subscribers();
    }

    fn wake_runtime_subscribers(&self) {
        let subscribers = self.subscribers.lock().clone();
        for subscriber in subscribers {
            subscriber.wake_all();
        }
    }

    #[cfg(test)]
    fn set_active_movement_registration_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.active_movement_registration_hook.lock() = hook;
    }

    #[cfg(test)]
    fn invoke_active_movement_registration_hook(&self) {
        let hook = self.active_movement_registration_hook.lock().take();
        if let Some(hook) = hook {
            // Never invoke a test seam while holding its registry lock.  The
            // hook intentionally may call stop() re-entrantly.
            hook();
        }
    }

    #[cfg(test)]
    fn set_event_admission_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.event_admission_hook.lock() = hook;
    }

    #[cfg(test)]
    fn invoke_event_admission_hook(&self) {
        let hook = self.event_admission_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    fn set_finalize_stop_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.finalize_stop_hook.lock() = hook;
    }

    #[cfg(test)]
    fn invoke_finalize_stop_hook(&self) {
        let hook = self.finalize_stop_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    fn set_event_broadcast_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.event_broadcast_hook.lock() = hook;
    }

    #[cfg(test)]
    fn invoke_event_broadcast_hook(&self) {
        let hook = self.event_broadcast_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    fn set_event_dispatch_backpressure_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.event_dispatch_backpressure_hook.lock() = hook;
    }

    #[cfg(test)]
    fn set_stop_signal_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.stop_signal_hook.lock() = hook;
    }

    #[cfg(test)]
    fn invoke_stop_signal_hook(&self) {
        let hook = self.stop_signal_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    fn set_runtime_broker_backpressure_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.runtime_broker_backpressure_hook.lock() = hook;
    }

    #[cfg(test)]
    fn event_dispatch_counts(&self) -> (usize, usize, usize, usize) {
        self.event_dispatch.lock().queued_counts()
    }

    #[cfg(test)]
    fn set_disconnect_cleanup_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.disconnect_cleanup_hook.lock() = hook;
    }

    #[cfg(test)]
    fn invoke_disconnect_cleanup_hook(&self) {
        let hook = self.disconnect_cleanup_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    fn set_entity_publish_after_apply_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.entity_publish_after_apply_hook.lock() = hook;
    }

    #[cfg(test)]
    fn invoke_entity_publish_after_apply_hook(&self) {
        let hook = self.entity_publish_after_apply_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    fn set_entity_owner_bind_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.entity_owner_bind_hook.lock() = hook;
    }

    #[cfg(test)]
    fn invoke_entity_owner_bind_hook(&self) {
        let hook = self.entity_owner_bind_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(test)]
    fn set_canonical_publication_probe(&self, probe: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.canonical_publication_probe.lock() = probe;
    }

    #[cfg(test)]
    fn invoke_canonical_publication_probe(&self) {
        let probe = self.canonical_publication_probe.lock().take();
        if let Some(probe) = probe {
            probe();
        }
    }

    #[cfg(test)]
    fn set_observation_write_boundary_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self.observation_write_boundary_hook.lock() = hook;
    }

    #[cfg(test)]
    fn invoke_observation_write_boundary_hook(&self) {
        let hook = self.observation_write_boundary_hook.lock().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    fn command_execution_allowed(&self) -> bool {
        let _admission = self.command_admission.lock();
        self.command_execution_allowed_without_lock()
    }

    fn with_command_admission<T>(&self, actuator: impl FnOnce() -> T) -> Result<T, ()> {
        let _admission = self.command_admission.lock();
        if !self.command_execution_allowed_without_lock() {
            return Err(());
        }
        // The closure contains only synchronous actuator operations.  It does
        // not await, emit events, or invoke completion callbacks while the
        // admission lock is held.
        Ok(actuator())
    }

    /// ConnectionFailed has already sealed the current attempt, so the normal
    /// command predicate (which rejects a disconnected attempt) is too
    /// narrow. This admits only the local disconnect actuator while keeping
    /// stop/stopped linearization on the same lock.
    fn with_disconnect_admission<T>(&self, actuator: impl FnOnce() -> T) -> Result<T, ()> {
        let _admission = self.command_admission.lock();
        if self.stopping.load(Ordering::Acquire) || self.stopped_reported.load(Ordering::Acquire) {
            return Err(());
        }
        Ok(actuator())
    }

    fn with_active_movement_admission<T>(
        &self,
        command_id: &str,
        generation: u64,
        completion: &Option<Arc<CommandCompletionState>>,
        actuator: impl FnOnce() -> T,
    ) -> Result<T, ()> {
        let _admission = self.command_admission.lock();
        if !self.command_execution_allowed_without_lock()
            || self.movement_generation.load(Ordering::Acquire) != generation
            || self.active_movement_id.lock().as_deref() != Some(command_id)
            || completion
                .as_ref()
                .is_some_and(|completion| completion.cancelled.load(Ordering::Acquire))
        {
            return Err(());
        }
        Ok(actuator())
    }

    fn finalize_stop_if_ready(&self) {
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.stopping.load(Ordering::Acquire)
                || self.active_movement_registration.load(Ordering::Acquire)
                || self.active_movement_completion.lock().is_some()
                || self.active_movement_id.lock().is_some()
            {
                return;
            }

            // The readiness check precedes taking the reason and both are
            // serialized with every cleanup caller. A second finalizer can no
            // longer observe an empty reason while the first one is about to
            // put it back.
            #[cfg(test)]
            self.invoke_finalize_stop_hook();

            let Some(reason) = self.stop_reason.lock().take() else {
                return;
            };
            self.enqueue_stopped_locked(reason, true)
        };
        if should_drain {
            self.drain_events();
        }
        self.request_shutdown();
    }

    /// 唯一的 Stopped reducer；正常 cleanup 与 watchdog 都通过这里发布终态。
    fn enqueue_stopped_locked(&self, reason: String, cancel_watchdog: bool) -> bool {
        if self.stopped_reported.swap(true, Ordering::AcqRel) {
            return false;
        }
        self.invalidate_stop_watchdog_locked();
        // This is a one-shot permit, not a broadcast.  `notify_one` retains the
        // permit when the watchdog task has been spawned but has not been polled
        // yet, so normal cleanup cannot leave the long stop timer holding the
        // runtime alive.  The unique Stopped gate above keeps forced cleanup or
        // duplicate finalizers from issuing another permit.
        if cancel_watchdog {
            self.stop_watchdog_cancel.notify_one();
        }
        self.set_backend_state(BackendState::Stopped {
            reason: Some(reason.clone()),
        });
        self.enqueue_event(
            FactSource::Commanded,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Stopped { reason }),
        )
    }

    fn enqueue_command_if_running(&self, command: BackendCommandEnvelope) -> Result<(), String> {
        let _admission = self.command_admission.lock();
        if self.stopping.load(Ordering::Acquire) {
            return Err("runtime is stopping".to_owned());
        }
        self.commands.lock().push_back(QueuedCommand {
            envelope: command,
            completion: None,
        });
        Ok(())
    }

    fn enqueue_command_with_completion_if_running(
        &self,
        command: BackendCommandEnvelope,
        completion: Arc<CommandCompletionState>,
    ) -> Result<(), String> {
        let _admission = self.command_admission.lock();
        if self.stopping.load(Ordering::Acquire) {
            return Err("runtime is stopping".to_owned());
        }
        self.commands.lock().push_back(QueuedCommand {
            envelope: command,
            completion: Some(completion),
        });
        Ok(())
    }

    #[cfg(test)]
    fn pop_command(&self) -> Option<QueuedCommand> {
        self.commands.lock().pop_front()
    }

    #[cfg(test)]
    fn requeue_front(&self, command: QueuedCommand) {
        self.commands.lock().push_front(command);
    }

    fn next_command_for_processing(&self) -> Option<QueuedCommand> {
        self.next_command_for_processing_with_hook(|| {})
    }

    fn next_command_for_processing_with_hook(
        &self,
        before_decision: impl FnOnce(),
    ) -> Option<QueuedCommand> {
        let mut commands = self.commands.lock();
        before_decision();
        let should_defer = commands.front().is_some_and(|command| {
            !self.ready.load(Ordering::Acquire)
                && !matches!(&command.envelope.command, BackendCommand::Respawn)
        });
        if should_defer {
            return None;
        }
        let command = commands.pop_front()?;
        Some(command)
    }

    fn cancel_pending_commands(&self) {
        for command in self.commands.lock().drain(..) {
            if let Some(completion) = command.completion {
                completion.cancel(format!("command:{}", command.envelope.id), false);
            }
        }
    }

    /// Declare a Move before touching the Azalea actuator.  The admission
    /// lock covers the only state transition that must be atomic with stop or
    /// disconnect: checking that the command may start and marking the
    /// registration window as live.  The rest of the registration is allowed
    /// to run without that lock so cancellation never waits on a callback or
    /// a bot operation.
    fn register_active_movement(
        &self,
        command_id: &str,
        generation: u64,
        duration_ms: u64,
        completion: &Option<Arc<CommandCompletionState>>,
    ) -> ActiveMovementRegistration {
        let admitted = {
            let _admission = self.command_admission.lock();
            if self.command_execution_allowed_without_lock() {
                self.active_movement_registration
                    .store(true, Ordering::Release);
                self.active_movement.store(true, Ordering::Release);
                *self.active_movement_id.lock() = Some(command_id.to_owned());
                true
            } else {
                false
            }
        };
        if !admitted {
            if let Some(completion) = completion {
                completion.cancel(format!("command:{command_id}"), false);
            }
            return ActiveMovementRegistration::Cancelled;
        }

        #[cfg(test)]
        self.invoke_active_movement_registration_hook();

        let cancel_signal = (duration_ms > 0).then(|| Arc::new(Notify::new()));
        *self.active_movement_cancel_signal.lock() = cancel_signal.clone();
        if let (Some(completion), Some(signal)) = (completion.as_ref(), cancel_signal.as_ref()) {
            completion.begin_active_release(signal.clone());
            *self.active_movement_completion.lock() = Some(completion.clone());
        }

        let cancelled = !self.command_execution_allowed()
            || completion
                .as_ref()
                .is_some_and(|completion| completion.cancelled.load(Ordering::Acquire));
        if cancelled {
            if let Some(completion) = completion {
                completion.cancel(format!("command:{command_id}"), true);
            }
            self.clear_registered_active_movement(
                command_id,
                generation,
                &cancel_signal,
                completion,
            );
            finish_command(
                completion,
                Err(BackendError::Cancelled {
                    operation: format!("command:{command_id}"),
                }),
            );
            self.finish_active_movement_registration();
            return ActiveMovementRegistration::Cancelled;
        }

        ActiveMovementRegistration::Started { cancel_signal }
    }

    fn command_execution_allowed_without_lock(&self) -> bool {
        !self.stopping.load(Ordering::Acquire) && !self.disconnect_reported.load(Ordering::Acquire)
    }

    fn arm_stop_watchdog_locked(&self) -> Option<StopWatchdogToken> {
        Some(StopWatchdogToken {
            generation: checked_atomic_increment(&self.stop_watchdog_generation)?,
        })
    }

    fn invalidate_stop_watchdog_locked(&self) {
        let _ = checked_atomic_increment(&self.stop_watchdog_generation);
    }

    fn spawn_stop_watchdog(self: &Arc<Self>, token: StopWatchdogToken) {
        if !self.timers_enabled.load(Ordering::Acquire) {
            return;
        }
        let shared = self.clone();
        let cancel = self.stop_watchdog_cancel.clone();
        let duration = Duration::from_millis(self.config.timeouts.stop_ms);
        #[cfg(test)]
        let completion_probe = self.stop_watchdog_completion_probe.lock().take();
        tokio::task::spawn_local(async move {
            tokio::select! {
                _ = tokio::time::sleep(duration) => shared.fire_stop_watchdog(token),
                _ = cancel.notified() => {}
            }
            #[cfg(test)]
            {
                // Drop the task's last RuntimeShared capture before resolving
                // the probe, so the test can also verify Arc lifetime rather
                // than merely observing a late inert callback.
                drop(shared);
                if let Some(probe) = completion_probe {
                    let _ = probe.send(());
                }
            }
        });
    }

    fn fire_stop_watchdog(&self, token: StopWatchdogToken) {
        let (reason, should_drain, client, swarm, completion, pending) = {
            let _admission = self.command_admission.lock();
            if !self.stopping.load(Ordering::Acquire)
                || self.stopped_reported.load(Ordering::Acquire)
                || self.stop_watchdog_generation.load(Ordering::Acquire) != token.generation
            {
                return;
            }
            self.invalidate_stop_watchdog_locked();
            let reason = self
                .stop_reason
                .lock()
                .take()
                .unwrap_or_else(|| "stop_watchdog".to_owned());
            self.phase_generation.store(u64::MAX, Ordering::Release);
            self.stable_generation.store(u64::MAX, Ordering::Release);
            self.reconnect_add_pending.store(false, Ordering::Release);
            self.reconnect_pending.store(false, Ordering::Release);
            let _ = checked_atomic_increment(&self.reconnect_attempt_token);
            self.ready.store(false, Ordering::Release);
            self.active_movement_registration
                .store(false, Ordering::Release);
            self.active_movement.store(false, Ordering::Release);
            self.movement_generation.store(u64::MAX, Ordering::Release);
            *self.active_movement_id.lock() = None;
            self.active_movement_cancel_signal.lock().take();
            let completion = self.active_movement_completion.lock().take();
            let client = self.active_client.lock().take();
            let swarm = self.swarm.lock().take();
            let pending = self
                .commands
                .lock()
                .drain(..)
                .filter_map(|command| command.completion)
                .collect::<Vec<_>>();
            // The watchdog task is already executing this forced path; do not
            // leave a cancellation permit behind for a hypothetical later
            // generation.  The unique reducer still seals Stopped exactly once.
            let should_drain = self.enqueue_stopped_locked(reason.clone(), false);
            (reason, should_drain, client, swarm, completion, pending)
        };

        self.phase_cancel.notify_waiters();
        self.stable_cancel.notify_waiters();
        self.reconnect_cancel.notify_one();
        if let Some(completion) = completion {
            completion.cancel("stop_watchdog".to_owned(), false);
            completion.finish(Err(BackendError::Cancelled {
                operation: "stop_watchdog".to_owned(),
            }));
        }
        for completion in pending {
            completion.cancel("stop_watchdog".to_owned(), false);
            completion.finish(Err(BackendError::Cancelled {
                operation: "stop_watchdog".to_owned(),
            }));
        }
        self.clear_observations();
        if let Some(client) = client {
            client.disconnect();
        }
        if let Some(swarm) = swarm {
            swarm.exit();
        }
        if should_drain {
            self.drain_events();
        }
        let _ = reason;
        self.request_shutdown();
    }

    fn clear_registered_active_movement(
        &self,
        command_id: &str,
        generation: u64,
        cancel_signal: &Option<Arc<Notify>>,
        completion: &Option<Arc<CommandCompletionState>>,
    ) -> bool {
        let owns_active_id = self.movement_generation.load(Ordering::Acquire) == generation
            && self.active_movement_id.lock().as_deref() == Some(command_id);
        if owns_active_id {
            self.active_movement.store(false, Ordering::Release);
            *self.active_movement_id.lock() = None;
        }

        if let Some(expected_signal) = cancel_signal {
            let mut current_signal = self.active_movement_cancel_signal.lock();
            if current_signal
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, expected_signal))
            {
                current_signal.take();
            }
        }
        if let Some(expected_completion) = completion {
            let mut current_completion = self.active_movement_completion.lock();
            if current_completion
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, expected_completion))
            {
                current_completion.take();
            }
        }
        owns_active_id
    }

    fn clear_idle_movement_state(&self, generation: u64) {
        let _admission = self.command_admission.lock();
        if self.movement_generation.load(Ordering::Acquire) != generation
            || self.active_movement_registration.load(Ordering::Acquire)
            || self.active_movement.load(Ordering::Acquire)
            || self.active_movement_id.lock().is_some()
        {
            return;
        }
        self.active_movement_cancel_signal.lock().take();
        self.active_movement_completion.lock().take();
    }

    fn finish_active_movement_registration(&self) {
        self.active_movement_registration
            .store(false, Ordering::Release);
        self.finalize_stop_if_ready();
    }

    fn cancel_registered_active_movement(
        &self,
        command_id: &str,
        generation: u64,
        cancel_signal: &Option<Arc<Notify>>,
        completion: &Option<Arc<CommandCompletionState>>,
    ) {
        if let Some(completion) = completion {
            completion.cancel(format!("command:{command_id}"), true);
        }
        self.clear_registered_active_movement(command_id, generation, cancel_signal, completion);
        finish_command(
            completion,
            Err(BackendError::Cancelled {
                operation: format!("command:{command_id}"),
            }),
        );
        self.active_movement_registration
            .store(false, Ordering::Release);
        self.finalize_stop_if_ready();
    }

    fn cancel_active_movement(
        &self,
        release_on_cancel: bool,
    ) -> Option<Arc<CommandCompletionState>> {
        if !release_on_cancel {
            self.movement_generation.fetch_add(1, Ordering::AcqRel);
        }
        let completion = self.active_movement_completion.lock().clone();
        let cancel_signal = self.active_movement_cancel_signal.lock().clone();
        if let Some(completion) = completion.as_ref() {
            completion.cancel(
                "movement superseded or stopped".to_owned(),
                release_on_cancel,
            );
        }
        if let Some(signal) = cancel_signal.as_ref() {
            signal.notify_one();
        }
        let deferred_release = release_on_cancel
            && (cancel_signal.is_some()
                || completion
                    .as_ref()
                    .is_some_and(|completion| completion.active_release.load(Ordering::Acquire)));
        if !deferred_release {
            self.active_movement.store(false, Ordering::Release);
            if let Some(completion) = completion.as_ref() {
                let mut active = self.active_movement_completion.lock();
                if active
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, completion))
                {
                    active.take();
                }
            }
            self.active_movement_cancel_signal.lock().take();
            *self.active_movement_id.lock() = None;
        } else {
            self.active_movement_cancel_signal.lock().take();
        }
        completion
    }

    fn refresh_snapshot(
        &self,
        bot: &Client,
        force: bool,
        source: FactSource,
    ) -> Option<MinecraftSnapshotV1> {
        let capture_generation = self.observation.read().generation;
        let (process_session_id, connection_epoch, connection_attempt_id) = self.context();
        let next_revision = self.snapshot_revision.load(Ordering::Acquire) + 1;
        let Some(candidate) = capture(
            bot,
            &self.config.world_id,
            &process_session_id,
            connection_epoch,
            &connection_attempt_id,
            next_revision,
            self.lifecycle_revision.load(Ordering::Acquire),
            now_utc(),
        ) else {
            // 断线/重连时 Azalea 会先移除本地玩家实体；此刻不能把“读不到”
            // 伪造成坐标，也不能调用 query_self 触发 panic。
            return None;
        };
        let entities = crate::snapshot::capture_tracked_entities_for_epoch(bot, connection_epoch);
        if self.connection_epoch() != connection_epoch {
            return None;
        }
        let _admission = self.command_admission.lock();
        if !self.command_execution_allowed_without_lock() {
            return None;
        }
        let scope_generation = self.entity_producer.lock().scope_generation;
        let mut observation = self.observation.write();
        if observation.generation != capture_generation
            || self.connection_epoch() != connection_epoch
        {
            return None;
        }
        let entities = merge_refreshed_tracked_entities(
            entities,
            &mut observation.entity_residuals,
            connection_epoch,
        );
        let changed = observation
            .snapshot
            .as_ref()
            .is_none_or(|previous| !previous.same_state_as(&candidate));
        observation.tracked_entities = entities;
        if force || changed {
            self.snapshot_revision
                .store(next_revision, Ordering::Release);
            observation.snapshot = Some(candidate.clone());
            observation.snapshot_scope_generation = scope_generation;
            observation.source = Some(source);
            observation.bump_generation();
            Some(candidate)
        } else {
            observation.bump_generation();
            None
        }
    }

    fn stored_snapshot(&self) -> Option<MinecraftSnapshotV1> {
        self.observation.read().snapshot.clone()
    }

    fn capture_frame_facts(&self) -> Option<RuntimeFrameFacts> {
        self.capture_frame_facts_locked(|| {}, || {})
    }

    fn capture_frame_facts_locked<F, G>(
        &self,
        before_values: F,
        after_boundary: G,
    ) -> Option<RuntimeFrameFacts>
    where
        F: FnOnce(),
        G: FnOnce(),
    {
        let observation = self.observation.read();
        let snapshot = observation.snapshot.clone()?;
        before_values();
        after_boundary();
        let armor = (observation.armor_epoch == Some(snapshot.connection_epoch))
            .then_some(observation.armor)
            .flatten();
        let light = observation.light_cache.value_at(
            &snapshot.self_snapshot.position,
            snapshot.connection_epoch,
            observation.snapshot_scope_generation,
            &snapshot.world.dimension,
        );
        Some(RuntimeFrameFacts {
            snapshot,
            armor,
            light,
        })
    }

    #[cfg(test)]
    fn capture_frame_facts_with_test_hooks<F, G>(
        &self,
        before_values: F,
        after_boundary: G,
    ) -> Option<RuntimeFrameFacts>
    where
        F: FnOnce(),
        G: FnOnce(),
    {
        self.capture_frame_facts_locked(before_values, after_boundary)
    }

    fn emit_snapshot(&self, snapshot: MinecraftSnapshotV1, source: FactSource) {
        self.emit_if_running(
            source,
            BackendEventPayload::SnapshotChanged(ContractProtocolSnapshotChangedEvent {
                group: "world".to_owned(),
                snapshot_revision: snapshot.snapshot_revision,
            }),
        );
    }

    fn initiate_stop(self: &Arc<Self>, reason: &str) {
        // Signal cancellation before taking command admission. A producer may
        // already hold that lock while waiting for this finite dispatch queue;
        // stop must be able to wake that producer instead of waiting for it.
        self.stop_requested.store(true, Ordering::Release);
        self.cancel_event_admission();
        #[cfg(test)]
        self.invoke_stop_signal_hook();
        let watchdog_token = {
            // Make stop admission atomic with the start of a Move
            // registration.  Once either side owns this short lock, the
            // other side has a clear linearization point and stopped cannot
            // be finalized in the registration gap.
            let _admission = self.command_admission.lock();
            if self.stopping.swap(true, Ordering::AcqRel) {
                return;
            }
            let reason = reason.to_owned();
            *self.stop_reason.lock() = Some(reason.clone());
            let epoch = self.connection_epoch();
            self.set_backend_state(BackendState::Stopping {
                epoch: (epoch != 0).then_some(epoch),
                reason,
            });
            self.reconnect_add_pending.store(false, Ordering::Release);
            let _ = checked_atomic_increment(&self.reconnect_attempt_token);
            self.reconnect_pending.store(false, Ordering::Release);
            self.ready.store(false, Ordering::Release);
            self.invalidate_phase_locked();
            self.invalidate_stable_reset_locked();
            self.arm_stop_watchdog_locked()
        };
        self.reconnect_cancel.notify_one();
        self.phase_cancel.notify_waiters();
        self.stable_cancel.notify_waiters();
        if let Some(token) = watchdog_token {
            self.spawn_stop_watchdog(token);
        }
        self.cancel_pending_commands();
        if self.connection_epoch() > 0 && !self.disconnect_reported.load(Ordering::Acquire) {
            self.mark_disconnected(Some("deliberate_stop".to_owned()));
        } else {
            self.cancel_active_movement(true);
        }
        self.exit_swarm();
        self.finalize_stop_if_ready();
    }
}

/// Runtime's internal representation of one atomic frame capture.  It is
/// converted to `mineintent_contracts::minecraft::MinecraftFrameFacts` by the
/// facade after this read lock has already captured all three values.
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeFrameFacts {
    pub snapshot: MinecraftSnapshotV1,
    pub armor: Option<u8>,
    pub light: Option<u8>,
}

/// 对齐 MineIntent `snapshot/subscribe/motor/sendChat` 边界的本地运行时句柄。
#[derive(Clone)]
pub struct RuntimeHandle {
    shared: Arc<SharedRuntime>,
}

impl RuntimeHandle {
    pub fn new(config: RunConfig) -> Self {
        Self {
            shared: Arc::new(SharedRuntime::new(config)),
        }
    }

    /// Read the lifecycle state owned by the real runtime admission paths.
    /// Facades should delegate to this value instead of maintaining a second
    /// state machine from a best-effort event subscription.
    pub fn state(&self) -> BackendState {
        self.shared.backend_state()
    }

    /// Return the epoch owned by the runtime admission state.  Facade-owned
    /// observation and motor handles use this value to reject a handle from a
    /// previous connection attempt before delegating to the concrete runtime
    /// seam.
    pub fn connection_epoch(&self) -> u64 {
        self.shared.connection_epoch()
    }

    pub fn snapshot(&self) -> Option<MinecraftSnapshotV1> {
        self.shared.stored_snapshot()
    }

    /// Capture snapshot, armor, and self-light from one observation read.
    pub fn capture_frame_facts(&self) -> Option<RuntimeFrameFacts> {
        self.shared.capture_frame_facts()
    }

    #[cfg(test)]
    pub(crate) fn test_install_frame_facts(
        &self,
        snapshot: MinecraftSnapshotV1,
        armor: Option<u8>,
        light: Option<u8>,
    ) {
        let scope_generation = self.shared.entity_producer.lock().scope_generation;
        let mut observation = self.shared.observation.write();
        observation.snapshot = Some(snapshot.clone());
        observation.snapshot_scope_generation = scope_generation;
        observation.armor = armor;
        observation.armor_epoch = Some(snapshot.connection_epoch);

        let geometry = LightSectionGeometry {
            min_light_section: (snapshot.world.min_y >> 4) - 1,
            light_section_count: (snapshot.world.height / 16 + 2) as usize,
        };
        observation.light_cache.reset_scope(
            snapshot.connection_epoch,
            scope_generation,
            Some(snapshot.world.dimension.clone()),
            Some(false),
        );
        observation.light_cache.context =
            observation.light_cache.context.take().map(|mut context| {
                context.geometry = Some(geometry);
                context
            });
        if let Some(light) = light {
            let Some(x) = floor_block_coordinate(snapshot.self_snapshot.position.x) else {
                return;
            };
            let Some(y) = floor_block_coordinate(snapshot.self_snapshot.position.y) else {
                return;
            };
            let Some(z) = floor_block_coordinate(snapshot.self_snapshot.position.z) else {
                return;
            };
            let Some(section_index) = geometry.index_for_section_y(y.div_euclid(16)) else {
                return;
            };
            let layer_index = ((y.rem_euclid(16) as usize) << 8)
                | ((z.rem_euclid(16) as usize) << 4)
                | (x.rem_euclid(16) as usize);
            let mut layer = Box::new([0; 4096]);
            layer[layer_index] = light;
            let mut chunk = CachedLightChunk {
                sky: vec![None; geometry.light_section_count],
                block: vec![None; geometry.light_section_count],
            };
            chunk.block[section_index] = Some(layer);
            observation
                .light_cache
                .chunks
                .insert((x.div_euclid(16), z.div_euclid(16)), chunk);
        }
        observation.bump_generation();
    }

    /// 返回当前 `snapshot()` 的事实来源；调用方不得把 `client_predicted`
    /// 快照当作服务端确认状态。
    pub fn snapshot_source(&self) -> Option<FactSource> {
        self.shared.observation.read().source
    }

    pub fn subscribe(&self) -> RuntimeEventReceiver {
        #[cfg(test)]
        let queue =
            RuntimeEventQueue::new(self.shared.runtime_broker_backpressure_hook.lock().clone());
        #[cfg(not(test))]
        let queue = RuntimeEventQueue::new();
        self.shared.subscribers.lock().push(queue.clone());
        RuntimeEventReceiver { queue }
    }

    pub fn observation_source(&self) -> RuntimeObservationSource {
        RuntimeObservationSource {
            shared: self.shared.clone(),
            bound_epoch: self.shared.connection_epoch(),
        }
    }

    pub fn send_command(&self, command: BackendCommandEnvelope) -> Result<(), String> {
        if command.protocol != BACKEND_COMMAND_PROTOCOL {
            return Err(format!(
                "不支持的命令协议：{}，期望 {}",
                command.protocol, BACKEND_COMMAND_PROTOCOL
            ));
        }
        validate_command(&command.command)?;
        self.shared.enqueue_command_if_running(command)
    }

    fn send_command_with_completion(
        &self,
        command: BackendCommand,
    ) -> Result<CommandCompletion, String> {
        validate_command(&command)?;
        let envelope = self.next_command(command);
        let (completion, state) = CommandCompletion::channel(envelope.id.clone());
        match self
            .shared
            .enqueue_command_with_completion_if_running(envelope, state.clone())
        {
            Ok(()) => Ok(completion),
            Err(error) => {
                state.cancel(format!("command:{}", completion.command_id), false);
                Err(error)
            }
        }
    }

    fn next_command(&self, command: BackendCommand) -> BackendCommandEnvelope {
        let id = self.shared.command_revision.fetch_add(1, Ordering::AcqRel) + 1;
        BackendCommandEnvelope {
            protocol: BACKEND_COMMAND_PROTOCOL.to_owned(),
            id: format!("command-{id}"),
            issued_at: now_utc(),
            command,
        }
    }

    pub fn send_chat(&self, message: impl Into<String>) -> Result<(), String> {
        self.send_command(self.next_command(BackendCommand::SendChat {
            message: message.into(),
        }))
    }

    /// 发送与主仓库 motor `lookRelative` 同语义的相对视角输入，并返回一次性完成 future。
    pub fn look_relative(
        &self,
        yaw_degrees: f32,
        pitch_degrees: f32,
    ) -> Result<CommandCompletion, String> {
        self.send_command_with_completion(BackendCommand::LookRelative {
            yaw_degrees,
            pitch_degrees,
        })
    }

    /// 发送按键式移动输入；校验范围与主仓库 motor 的 50–1500ms 边界一致，
    /// 并返回在释放动作完成时 resolve 的 future。
    pub fn move_input(
        &self,
        directions: Vec<MotorDirection>,
        duration_ms: u64,
        sprint: Option<bool>,
        jump: Option<bool>,
        crouch: Option<bool>,
    ) -> Result<CommandCompletion, String> {
        self.send_command_with_completion(BackendCommand::Move {
            directions,
            duration_ms,
            sprint,
            jump,
            crouch,
        })
    }

    /// 释放全部移动/跳跃/潜行输入。
    pub fn release_all(&self) -> Result<CommandCompletion, String> {
        self.send_command_with_completion(BackendCommand::ReleaseAll)
    }

    /// 显式请求服务端执行重生；死亡后不会由运行时自动触发。
    pub fn respawn(&self) -> Result<(), String> {
        self.send_command(self.next_command(BackendCommand::Respawn))
    }

    /// 主动结束运行时；停止动作本身会写入 `commanded` 事件。
    pub fn stop(&self, reason: &str) {
        self.shared.initiate_stop(reason);
    }

    #[cfg(test)]
    pub(crate) async fn test_wait_for_shutdown(&self) {
        self.shared.shutdown.notified().await;
    }

    #[cfg(test)]
    pub(crate) fn test_drive_event(&self, source: FactSource, payload: BackendEventPayload) {
        match payload.clone() {
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected) => {
                self.shared.emit_transport_connected();
            }
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::LoggedIn {
                version,
                dimension,
            }) => {
                self.shared.emit_logged_in(version, dimension);
            }
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Ready {
                snapshot_revision,
            }) => {
                self.shared.emit_ready(snapshot_revision);
            }
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Faulted { failure }) => {
                self.shared.emit_faulted(failure);
            }
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Stopped { ref reason }) => {
                self.shared.initiate_stop(reason);
            }
            _ => self.shared.emit(source, payload),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_set_event_dispatch_backpressure_hook(
        &self,
        hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        self.shared.set_event_dispatch_backpressure_hook(hook);
    }

    #[cfg(test)]
    pub(crate) fn test_set_stop_signal_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.shared.set_stop_signal_hook(hook);
    }

    #[cfg(test)]
    pub(crate) fn test_set_runtime_broker_backpressure_hook(
        &self,
        hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        self.shared.set_runtime_broker_backpressure_hook(hook);
    }

    #[cfg(test)]
    pub(crate) fn test_event_dispatch_counts(&self) -> (usize, usize, usize, usize) {
        self.shared.event_dispatch_counts()
    }

    #[cfg(test)]
    pub(crate) fn test_settle_next_command(&self, result: Result<(), BackendError>) -> bool {
        while let Some(command) = self.shared.pop_command() {
            if command
                .completion
                .as_ref()
                .is_some_and(|completion| completion.cancelled.load(Ordering::Acquire))
            {
                continue;
            }
            finish_command(&command.completion, result);
            return true;
        }
        false
    }

    #[cfg(test)]
    pub(crate) fn test_has_pending_command(&self) -> bool {
        !self.shared.commands.lock().is_empty()
    }
}

fn validate_command(command: &BackendCommand) -> Result<(), String> {
    match command {
        BackendCommand::SendChat { message } => {
            if message.is_empty() || message.contains(['\r', '\n', '\0']) {
                return Err("聊天消息必须是非空的单行文本".to_owned());
            }
        }
        BackendCommand::LookRelative {
            yaw_degrees,
            pitch_degrees,
        } => {
            if !yaw_degrees.is_finite() || yaw_degrees.abs() > 90.0 {
                return Err("相对 yaw 必须是 ±90 度以内的有限数".to_owned());
            }
            if !pitch_degrees.is_finite() || pitch_degrees.abs() > 90.0 {
                return Err("相对 pitch 必须是 ±90 度以内的有限数".to_owned());
            }
        }
        BackendCommand::Move {
            directions,
            duration_ms,
            ..
        } => {
            if directions.is_empty() || directions.len() > 4 {
                return Err("移动方向必须包含 1 到 4 个按键".to_owned());
            }
            if directions
                .iter()
                .enumerate()
                .any(|(index, direction)| directions[index + 1..].contains(direction))
            {
                return Err("移动方向不能重复".to_owned());
            }
            if !(50..=1_500).contains(duration_ms) {
                return Err("移动时长必须是 50 到 1500 毫秒".to_owned());
            }
        }
        BackendCommand::ReleaseAll | BackendCommand::Respawn => {}
    }
    Ok(())
}

/// 一个 observation source 的 owned typed subscription。
///
/// 回调由共享的 FIFO drainer 同步调用；每个订阅本身没有独立队列、后台转发
/// 任务或每订阅线程。关闭会先从 registry 线性化移除，再等待在途回调结束。
pub struct RuntimeObservationSubscription {
    shared: Arc<SharedRuntime>,
    id: u64,
    state: Arc<ObservationSubscriptionState>,
    closed: bool,
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

const MAX_VIEWPORT_CAPTURE_ATTEMPTS: usize = 3;

#[derive(Clone)]
struct ViewportCapture {
    generation: u64,
    world: SharedWorld,
    world_bounds: WorldHeightBounds,
    pose: PoseSnapshot,
    entities: Vec<ProtocolEntitySnapshot>,
    source: FactSource,
}

enum ViewportReadAttempt {
    Complete(ViewportReadComplete),
    Retry,
}

enum ViewportReadComplete {
    Full(ContractViewportRead),
    Directed(DirectedViewportProjection),
}

#[derive(Clone)]
enum ViewportProjectionRequest {
    Full,
    Directed(Vec<ContractBlockPosition>),
}

enum ViewportProjectionWorkerResult {
    Complete {
        capture: ViewportCapture,
        projection: ViewportKernelProjection,
    },
    Retry,
}

enum ViewportKernelProjection {
    Full(ViewportProjection),
    Directed(DirectedViewportProjection),
}

/// 对齐 MineIntent `ProtocolObservationSource` 的只读 concrete observation seam。
///
/// `bound_epoch` 是创建 source 时捕获的值；所有 observation 方法都在读前后检查它。
#[derive(Clone)]
pub struct RuntimeObservationSource {
    shared: Arc<SharedRuntime>,
    bound_epoch: u64,
}

impl RuntimeObservationSource {
    pub fn epoch(&self) -> u64 {
        self.bound_epoch
    }

    fn ensure_current_epoch(&self) -> Result<(), BackendError> {
        let current_epoch = self.shared.connection_epoch();
        if current_epoch != self.bound_epoch {
            return Err(BackendError::StaleEpoch {
                bound_epoch: self.bound_epoch,
                current_epoch,
            });
        }
        Ok(())
    }

    fn self_pose_snapshot(&self) -> Result<Option<PoseSnapshot>, BackendError> {
        self.ensure_current_epoch()?;
        let pose = self
            .shared
            .observation
            .read()
            .snapshot
            .as_ref()
            .map(|snapshot| PoseSnapshot {
                position: snapshot.self_snapshot.position.clone(),
                velocity: snapshot.self_snapshot.velocity.clone(),
                yaw: snapshot.self_snapshot.yaw,
                pitch: snapshot.self_snapshot.pitch,
                on_ground: snapshot.self_snapshot.on_ground,
            });
        self.ensure_current_epoch()?;
        Ok(pose)
    }

    pub fn self_pose(&self) -> Result<ContractSelfPose, BackendError> {
        let pose = self.self_pose_snapshot()?;
        self.ensure_current_epoch()?;
        let pose = pose.ok_or_else(|| BackendError::NotReady {
            state: "self_pose_unavailable".to_owned(),
        })?;
        Ok(contract_self_pose(pose))
    }

    pub fn snapshot_source(&self) -> Result<Option<FactSource>, BackendError> {
        self.ensure_current_epoch()?;
        let source = self.shared.observation.read().source;
        self.ensure_current_epoch()?;
        Ok(source)
    }

    pub fn list_tracked_players(&self) -> Result<Vec<TrackedPlayerSnapshot>, BackendError> {
        self.ensure_current_epoch()?;
        let observation = self.shared.observation.read();
        let players = observation
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.tracked_players.clone())
            .unwrap_or_default();
        self.ensure_current_epoch()?;
        Ok(players)
    }

    fn list_tracked_entities_snapshot(&self) -> Result<Vec<ProtocolEntitySnapshot>, BackendError> {
        self.ensure_current_epoch()?;
        let entities = self.shared.observation.read().tracked_entities.clone();
        self.ensure_current_epoch()?;
        Ok(entities)
    }

    pub fn list_tracked_entities(
        &self,
    ) -> Result<Vec<ContractProtocolEntitySnapshot>, BackendError> {
        let entities = self.list_tracked_entities_snapshot()?;
        let converted = entities
            .into_iter()
            .map(contract_entity_snapshot)
            .collect::<Result<Vec<_>, _>>()?;
        self.ensure_current_epoch()?;
        Ok(converted)
    }

    /// 对齐 MineIntent viewport 的只读投影；所有坐标仍是 Minecraft 世界绝对坐标。
    ///
    /// 投影不会把本地缓存的方块直接宣称为可见：它会对视锥内候选执行暴露面和
    /// 遮挡射线判断。这个旧方法保留可配置 kernel 的 backend seam，但不是 atomic
    /// VIEW-02 seam；它不会携带与 projection 同次 capture 的 source/revision。
    /// 需要三项一致结果时必须使用 `read_viewport(OperationControl)`。
    pub fn viewport(
        &self,
        options: &ViewportOptions,
    ) -> Result<Option<ViewportProjection>, BackendError> {
        self.ensure_current_epoch()?;
        let Some(pose) = self.self_pose_snapshot()? else {
            return Ok(None);
        };
        let entities = self.list_tracked_entities_snapshot()?;
        let Some(world) = self.shared.observation.read().world.clone() else {
            self.ensure_current_epoch()?;
            return Ok(None);
        };
        // 一次投影只读一个世界视图，避免候选扫描的每次体素访问都重新获取
        // RwLock；独立的 read_block() 仍保持短锁，供增量读取使用。
        let world = world.read();
        project_viewport(
            &pose,
            &entities,
            |position| read_block_from_world(&world, position),
            options,
        )
        .map(Some)
        .map_err(|message| BackendError::InvalidCommand {
            field: "viewport".to_owned(),
            message,
        })
        .and_then(|projection| {
            self.ensure_current_epoch()?;
            Ok(projection)
        })
    }

    /// 读取已加载世界中的绝对方块状态；结果不等于视线可见性。
    ///
    /// 上层 viewport 应基于 `transparentHint`、碰撞/轮廓几何和观察者姿态
    /// 做射线或暴露面判断，避免把“客户端缓存里有数据”误报成“玩家看到了”。
    pub fn read_block(
        &self,
        position: ContractBlockPosition,
    ) -> Result<ContractBlockReadResult, BackendError> {
        let result = self.read_block_with_post_read_hook(
            BlockPosition {
                x: position.x,
                y: position.y,
                z: position.z,
            },
            || {},
        )?;
        self.ensure_current_epoch()?;
        Ok(contract_block_read_result(result))
    }

    fn read_block_with_post_read_hook(
        &self,
        position: BlockPosition,
        after_read: impl FnOnce(),
    ) -> Result<BlockReadResult, BackendError> {
        self.ensure_current_epoch()?;
        let Some(world) = self.shared.observation.read().world.clone() else {
            after_read();
            self.ensure_current_epoch()?;
            return Ok(BlockReadResult::Unloaded);
        };
        let world = world.read();
        let result = read_block_from_world(&world, position);
        after_read();
        self.ensure_current_epoch()?;
        Ok(result)
    }

    /// Read one coherent viewport capture and attach its provenance and read
    /// revision. The default options deliberately stay in the backend kernel;
    /// callers that need custom options may use the legacy non-atomic `viewport`
    /// method, but cannot combine it with `snapshot_source()` to form this seam.
    pub fn read_viewport(
        &self,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<ContractViewportRead, BackendError>> {
        Box::pin(async move {
            control.preflight("read_viewport")?;
            let request = ViewportProjectionRequest::Full;
            for attempt in 0..MAX_VIEWPORT_CAPTURE_ATTEMPTS {
                match self
                    .read_viewport_attempt(&control, request.clone())
                    .await
                    .map_err(backend_error_from_directed)?
                {
                    ViewportReadAttempt::Complete(ViewportReadComplete::Full(read)) => {
                        return Ok(read)
                    }
                    ViewportReadAttempt::Complete(ViewportReadComplete::Directed(_)) => {
                        unreachable!("full request cannot produce directed projection")
                    }
                    ViewportReadAttempt::Retry if attempt + 1 < MAX_VIEWPORT_CAPTURE_ATTEMPTS => {
                        control.preflight("read_viewport")?;
                        tokio::task::yield_now().await;
                    }
                    ViewportReadAttempt::Retry => {}
                }
            }
            control.preflight("read_viewport")?;
            self.ensure_current_epoch()?;
            Err(BackendError::NotReady {
                state: "viewport_capture_changed".to_owned(),
            })
        })
    }

    /// Read directed coordinates against the same atomic capture and viewport kernel as full.
    /// The captured world height is the only metadata used for zero-read out-of-world geometry
    /// classification; a target read that independently returns `OutOfWorld` becomes a row.
    pub fn read_directed_viewport(
        &self,
        positions: Vec<ContractBlockPosition>,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<DirectedViewportProjection, DirectedViewportError>> {
        Box::pin(async move {
            control.preflight("read_directed_viewport")?;
            let tuples = positions
                .iter()
                .map(|position| (position.x, position.y, position.z))
                .collect::<Vec<_>>();
            validate_directed_positions(&tuples).map_err(|message| {
                DirectedViewportError::Backend(BackendError::InvalidCommand {
                    field: "positions".to_owned(),
                    message,
                })
            })?;
            let request = ViewportProjectionRequest::Directed(positions);
            for attempt in 0..MAX_VIEWPORT_CAPTURE_ATTEMPTS {
                match self
                    .read_viewport_attempt(&control, request.clone())
                    .await?
                {
                    ViewportReadAttempt::Complete(ViewportReadComplete::Directed(projection)) => {
                        return Ok(projection)
                    }
                    ViewportReadAttempt::Complete(ViewportReadComplete::Full(_)) => {
                        unreachable!("directed request cannot produce full projection")
                    }
                    ViewportReadAttempt::Retry if attempt + 1 < MAX_VIEWPORT_CAPTURE_ATTEMPTS => {
                        control.preflight("read_directed_viewport")?;
                        tokio::task::yield_now().await;
                    }
                    ViewportReadAttempt::Retry => {}
                }
            }
            control.preflight("read_directed_viewport")?;
            self.ensure_current_epoch()?;
            Err(DirectedViewportError::Backend(BackendError::NotReady {
                state: "viewport_capture_changed".to_owned(),
            }))
        })
    }

    async fn read_viewport_attempt(
        &self,
        control: &OperationControl,
        request: ViewportProjectionRequest,
    ) -> Result<ViewportReadAttempt, DirectedViewportError> {
        self.ensure_current_epoch()?;
        let operation = match &request {
            ViewportProjectionRequest::Full => "read_viewport",
            ViewportProjectionRequest::Directed(_) => "read_directed_viewport",
        };
        control.preflight(operation)?;

        let (world, initial_generation) = {
            let observation = self.shared.observation.read();
            let writer = self.shared.writer.lock();
            if writer.connection_epoch != self.bound_epoch {
                return Err(DirectedViewportError::Backend(BackendError::StaleEpoch {
                    bound_epoch: self.bound_epoch,
                    current_epoch: writer.connection_epoch,
                }));
            }
            if !self.shared.ready.load(Ordering::Acquire) {
                return Err(DirectedViewportError::Backend(BackendError::NotReady {
                    state: "not_ready".to_owned(),
                }));
            }
            if observation.snapshot.is_none() {
                return Err(DirectedViewportError::Backend(BackendError::NotReady {
                    state: "viewport_snapshot_unavailable".to_owned(),
                }));
            }
            if observation.source.is_none() {
                return Err(DirectedViewportError::Backend(BackendError::NotReady {
                    state: "viewport_source_unavailable".to_owned(),
                }));
            }
            let Some(world) = observation.world.clone() else {
                return Err(DirectedViewportError::Backend(BackendError::NotReady {
                    state: "viewport_world_unavailable".to_owned(),
                }));
            };
            (world, observation.generation)
        };

        control.preflight(operation)?;
        let projection_shared = self.shared.clone();
        let projection_world = world.clone();
        let projection_initial_generation = initial_generation;
        let projection_bound_epoch = self.bound_epoch;
        let projection_control = control.clone();
        let projection_request = request;
        let mut projection_task = tokio::task::spawn_blocking(move || {
            // Acquire the world-owned read guard before cloning the state
            // values. This makes the world view and the published metadata one
            // capture while keeping the shared observation lock short-lived.
            let world_read = projection_world.read();
            let capture = {
                let observation = projection_shared.observation.read();
                let writer = projection_shared.writer.lock();
                if writer.connection_epoch != projection_bound_epoch {
                    return Err(DirectedViewportError::Backend(BackendError::StaleEpoch {
                        bound_epoch: projection_bound_epoch,
                        current_epoch: writer.connection_epoch,
                    }));
                }
                if !projection_shared.ready.load(Ordering::Acquire) {
                    return Err(DirectedViewportError::Backend(BackendError::NotReady {
                        state: "not_ready".to_owned(),
                    }));
                }
                if observation.generation != projection_initial_generation
                    || !observation
                        .world
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &projection_world))
                {
                    return Ok(ViewportProjectionWorkerResult::Retry);
                }
                let Some(snapshot) = observation.snapshot.as_ref() else {
                    return Err(DirectedViewportError::Backend(BackendError::NotReady {
                        state: "viewport_snapshot_unavailable".to_owned(),
                    }));
                };
                if snapshot.connection_epoch != writer.connection_epoch {
                    return Err(DirectedViewportError::Backend(BackendError::NotReady {
                        state: "viewport_snapshot_epoch_mismatch".to_owned(),
                    }));
                }
                let Some(source) = observation.source else {
                    return Err(DirectedViewportError::Backend(BackendError::NotReady {
                        state: "viewport_source_unavailable".to_owned(),
                    }));
                };
                ViewportCapture {
                    generation: observation.generation,
                    world: projection_world.clone(),
                    world_bounds: WorldHeightBounds::new(
                        world_read.chunks.min_y(),
                        world_read.chunks.height(),
                    ),
                    pose: PoseSnapshot {
                        position: snapshot.self_snapshot.position.clone(),
                        velocity: snapshot.self_snapshot.velocity.clone(),
                        yaw: snapshot.self_snapshot.yaw,
                        pitch: snapshot.self_snapshot.pitch,
                        on_ground: snapshot.self_snapshot.on_ground,
                    },
                    entities: observation.tracked_entities.clone(),
                    source,
                }
            };
            projection_control.preflight(operation)?;
            let projection = match projection_request {
                ViewportProjectionRequest::Full => ViewportKernelProjection::Full(
                    project_viewport_with_checkpoint(
                        &capture.pose,
                        &capture.entities,
                        |position| read_block_from_world(&world_read, position),
                        &ViewportOptions::default(),
                        || projection_control.preflight(operation),
                    )
                    .map_err(DirectedViewportError::Backend)?,
                ),
                ViewportProjectionRequest::Directed(positions) => {
                    let positions = positions
                        .into_iter()
                        .map(|position| [position.x, position.y, position.z])
                        .collect::<Vec<_>>();
                    ViewportKernelProjection::Directed(project_directed_viewport(
                        &capture.pose,
                        &positions,
                        |position| read_block_from_world(&world_read, position),
                        &ViewportOptions::default(),
                        capture.world_bounds,
                        || projection_control.preflight(operation),
                    )?)
                }
            };
            Ok(ViewportProjectionWorkerResult::Complete {
                capture,
                projection,
            })
        });
        let cancellation = control.cancelled();
        let deadline = async {
            if let Some(deadline) = control.deadline_elapsed() {
                deadline.await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::pin!(cancellation);
        tokio::pin!(deadline);
        let worker_result = tokio::select! {
            result = &mut projection_task => result
                .map_err(|error| DirectedViewportError::Backend(BackendError::BackendFailure {
                    failure: BackendFailure {
                        code: BackendFailureCode::ProtocolError,
                        message: format!("viewport projection task failed: {error}"),
                        retryable: true,
                    },
                }))??,
            _ = &mut cancellation => {
                projection_task.abort();
                return Err(DirectedViewportError::Backend(control_wakeup_error(
                    control, operation,
                )));
            }
            _ = &mut deadline => {
                projection_task.abort();
                return Err(DirectedViewportError::Backend(control_wakeup_error(
                    control, operation,
                )));
            }
        };
        let (capture, projection) = match worker_result {
            ViewportProjectionWorkerResult::Complete {
                capture,
                projection,
            } => (capture, projection),
            ViewportProjectionWorkerResult::Retry => return Ok(ViewportReadAttempt::Retry),
        };
        control.preflight(operation)?;

        let observation = self.shared.observation.read();
        let writer = self.shared.writer.lock();
        if writer.connection_epoch != self.bound_epoch {
            return Err(DirectedViewportError::Backend(BackendError::StaleEpoch {
                bound_epoch: self.bound_epoch,
                current_epoch: writer.connection_epoch,
            }));
        }
        if !self.shared.ready.load(Ordering::Acquire) {
            return Err(DirectedViewportError::Backend(BackendError::NotReady {
                state: "not_ready".to_owned(),
            }));
        }
        if observation.generation != capture.generation
            || !observation
                .world
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &capture.world))
        {
            return Ok(ViewportReadAttempt::Retry);
        }
        if observation.source != Some(capture.source) {
            return Ok(ViewportReadAttempt::Retry);
        }
        let revision = self.shared.viewport_revision.fetch_add(1, Ordering::AcqRel) + 1;
        let complete = match projection {
            ViewportKernelProjection::Full(projection) => {
                ViewportReadComplete::Full(ContractViewportRead {
                    projection: contract_viewport_projection(projection),
                    source: contract_fact_source(capture.source),
                    revision,
                })
            }
            ViewportKernelProjection::Directed(projection) => {
                ViewportReadComplete::Directed(projection)
            }
        };
        Ok(ViewportReadAttempt::Complete(complete))
    }

    fn subscribe_listener(
        &self,
        listener: Arc<dyn ObservationEventListener>,
        post_register_hook: Option<&dyn Fn()>,
    ) -> Result<RuntimeObservationSubscription, BackendError> {
        self.ensure_current_epoch()?;
        let (id, state) = self
            .shared
            .add_observation_subscription(self.bound_epoch, listener);
        if let Some(hook) = post_register_hook {
            hook();
        }
        if let Err(error) = self.ensure_current_epoch() {
            self.shared.remove_observation_subscription(id, &state);
            return Err(error);
        }
        Ok(RuntimeObservationSubscription {
            shared: self.shared.clone(),
            id,
            state,
            closed: false,
        })
    }

    #[cfg(test)]
    fn subscribe_with_post_register_hook(
        &self,
        listener: Arc<dyn ObservationEventListener>,
        hook: impl Fn(),
    ) -> Result<RuntimeObservationSubscription, BackendError> {
        self.subscribe_listener(listener, Some(&hook))
    }
}

impl ProtocolObservationSource for RuntimeObservationSource {
    fn epoch(&self) -> u64 {
        RuntimeObservationSource::epoch(self)
    }

    fn self_pose(&self) -> Result<ContractSelfPose, BackendError> {
        RuntimeObservationSource::self_pose(self)
    }

    fn list_tracked_entities(&self) -> Result<Vec<ContractProtocolEntitySnapshot>, BackendError> {
        RuntimeObservationSource::list_tracked_entities(self)
    }

    fn read_block(
        &self,
        position: ContractBlockPosition,
    ) -> Result<ContractBlockReadResult, BackendError> {
        RuntimeObservationSource::read_block(self, position)
    }

    fn subscribe(
        &self,
        listener: Arc<dyn ObservationEventListener>,
    ) -> Result<Box<dyn Subscription>, BackendError> {
        self.subscribe_listener(listener, None)
            .map(|subscription| Box::new(subscription) as Box<dyn Subscription>)
    }

    fn read_viewport(
        &self,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<ContractViewportRead, BackendError>> {
        // Fully qualify the inherent method so the trait adapter cannot recurse.
        RuntimeObservationSource::read_viewport(self, control)
    }

    fn read_directed_viewport(
        &self,
        positions: Vec<ContractBlockPosition>,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<DirectedViewportProjection, DirectedViewportError>> {
        RuntimeObservationSource::read_directed_viewport(self, positions, control)
    }
}

fn contract_vec3(value: Vec3Value) -> ContractVec3Value {
    ContractVec3Value {
        x: value.x,
        y: value.y,
        z: value.z,
    }
}

fn contract_self_pose(pose: PoseSnapshot) -> ContractSelfPose {
    ContractSelfPose {
        position: contract_vec3(pose.position),
        velocity: contract_vec3(pose.velocity),
        yaw: f64::from(pose.yaw),
        pitch: f64::from(pose.pitch),
    }
}

fn dto_conversion_error(field: &str, message: impl Into<String>) -> BackendError {
    BackendError::BackendFailure {
        failure: BackendFailure {
            code: BackendFailureCode::ProtocolError,
            message: format!(
                "cannot convert backend observation DTO {field}: {}",
                message.into()
            ),
            retryable: false,
        },
    }
}

fn contract_entity_snapshot(
    entity: ProtocolEntitySnapshot,
) -> Result<ContractProtocolEntitySnapshot, BackendError> {
    let equipment = entity
        .equipment
        .into_iter()
        .map(|item| {
            let count = u32::try_from(item.count).map_err(|_| {
                dto_conversion_error(
                    "entity.equipment.count",
                    format!("negative item count {}", item.count),
                )
            })?;
            Ok(ContractEntityEquipmentSnapshot {
                slot: u32::from(item.slot),
                item_name: item.item_name,
                count,
            })
        })
        .collect::<Result<Vec<_>, BackendError>>()?;

    Ok(ContractProtocolEntitySnapshot {
        entity_key: entity.entity_key,
        protocol_entity_id: entity.protocol_entity_id,
        entity_type: entity.entity_type,
        name: entity.name,
        username: entity.username,
        uuid: entity.uuid,
        position: contract_vec3(entity.position),
        velocity: contract_vec3(entity.velocity),
        yaw: f64::from(entity.yaw),
        pitch: f64::from(entity.pitch),
        head_yaw: entity.head_yaw.map(f64::from),
        width: f64::from(entity.width),
        height: f64::from(entity.height),
        on_ground: entity.on_ground,
        pose: entity.pose,
        held_item_name: entity.held_item_name,
        equipment,
        valid: entity.valid,
    })
}

fn contract_block_snapshot(block: ProtocolBlockSnapshot) -> ContractProtocolBlockSnapshot {
    ContractProtocolBlockSnapshot {
        position: ContractBlockPosition {
            x: block.position.x,
            y: block.position.y,
            z: block.position.z,
        },
        name: block.name,
        state_id: block.state_id,
        properties: block
            .properties
            .into_iter()
            .map(|(key, value)| (key, parse_block_property_value(&value)))
            .collect(),
        collision_shapes: block.collision_shapes,
        transparent_hint: block.transparent_hint,
        bounding_box: match block.bounding_box {
            BlockBoundingBox::Block => ContractBlockBoundingBox::Block,
            BlockBoundingBox::Empty => ContractBlockBoundingBox::Empty,
        },
    }
}

fn contract_block_read_result(result: BlockReadResult) -> ContractBlockReadResult {
    match result {
        BlockReadResult::Loaded { block } => ContractBlockReadResult::Loaded {
            block: contract_block_snapshot(block),
        },
        BlockReadResult::Unloaded => ContractBlockReadResult::Unloaded,
        BlockReadResult::OutOfWorld => ContractBlockReadResult::OutOfWorld,
    }
}

fn contract_event_metadata(event: &BackendEventEnvelope) -> ContractBackendEventMetadata {
    ContractBackendEventMetadata {
        id: event.id.clone(),
        occurred_at: event.occurred_at.clone(),
        process_session_id: event.process_session_id.clone(),
        connection_epoch: event.connection_epoch,
        connection_attempt_id: event.connection_attempt_id.clone(),
        world_id: event.world_id.clone(),
        dimension: event.dimension.clone(),
    }
}

fn contract_event_kind(kind: BackendEventKind) -> ContractBackendEventKind {
    match kind {
        BackendEventKind::Entity => ContractBackendEventKind::Entity,
        BackendEventKind::Block => ContractBackendEventKind::Block,
        BackendEventKind::Sound => ContractBackendEventKind::Sound,
        _ => unreachable!("non-observation event cannot enter typed observation adapter"),
    }
}

fn observation_event_from_backend(event: &BackendEventEnvelope) -> Option<ObservationEvent> {
    let metadata = contract_event_metadata(event);
    let source = contract_fact_source(event.source);
    match (&event.kind, &event.payload) {
        (BackendEventKind::Entity, BackendEventPayload::Entity(payload)) => {
            Some(ObservationEvent::Entity(ContractBackendEventEnvelope::new(
                metadata,
                contract_event_kind(event.kind),
                source,
                payload.clone(),
            )))
        }
        (BackendEventKind::Block, BackendEventPayload::Block(payload)) => {
            Some(ObservationEvent::Block(ContractBackendEventEnvelope::new(
                metadata,
                contract_event_kind(event.kind),
                source,
                payload.clone(),
            )))
        }
        (BackendEventKind::Sound, BackendEventPayload::Sound(payload)) => {
            Some(ObservationEvent::Sound(ContractBackendEventEnvelope::new(
                metadata,
                contract_event_kind(event.kind),
                source,
                payload.clone(),
            )))
        }
        _ => None,
    }
}

struct CommandCompletionState {
    sender: parking_lot::Mutex<Option<oneshot::Sender<Result<(), BackendError>>>>,
    settled_result: parking_lot::Mutex<Option<Result<(), BackendError>>>,
    settled_cv: parking_lot::Condvar,
    /// Owns the single finishing transition. `settled` is published only
    /// after result, physical-release bookkeeping, and the oneshot have all
    /// been published under this ownership.
    finish_lock: parking_lot::Mutex<()>,
    cancelled: AtomicBool,
    active_release: AtomicBool,
    release_on_cancel: AtomicBool,
    cancel_signal: parking_lot::Mutex<Option<Arc<Notify>>>,
    settled: AtomicBool,
    settled_signal: Notify,
}

impl CommandCompletionState {
    fn finish(&self, result: Result<(), BackendError>) {
        let _finish = self.finish_lock.lock();
        if self.settled.load(Ordering::Acquire) {
            return;
        }
        *self.settled_result.lock() = Some(result.clone());
        self.active_release.store(false, Ordering::Release);
        if let Some(sender) = self.sender.lock().take() {
            let _ = sender.send(result);
        }
        // This is deliberately the last publication in the finishing
        // transition. Waiters that observe `settled` therefore also observe
        // the result and the completed physical-release bookkeeping.
        self.settled.store(true, Ordering::Release);
        self.settled_cv.notify_all();
        self.settled_signal.notify_one();
    }

    fn set_cancel_signal(&self, signal: Arc<Notify>) {
        let already_cancelled = self.cancelled.load(Ordering::Acquire);
        *self.cancel_signal.lock() = Some(signal.clone());
        if already_cancelled {
            signal.notify_one();
        }
    }

    fn begin_active_release(&self, signal: Arc<Notify>) {
        self.active_release.store(true, Ordering::Release);
        self.set_cancel_signal(signal);
    }

    #[cfg(test)]
    async fn wait_settled(&self) {
        while !self.settled.load(Ordering::Acquire) {
            self.settled_signal.notified().await;
        }
    }

    fn cancel(&self, operation: String, release_on_cancel: bool) {
        self.cancelled.store(true, Ordering::Release);
        self.release_on_cancel
            .fetch_or(release_on_cancel, Ordering::AcqRel);
        if let Some(signal) = self.cancel_signal.lock().as_ref() {
            signal.notify_one();
        }
        // An active Move owns the physical release.  Its task finishes the
        // oneshot only after inputs and active state have been cleared.  A
        // queued/superseded command has no physical work left and settles now.
        if !self.active_release.load(Ordering::Acquire)
            || !self.release_on_cancel.load(Ordering::Acquire)
        {
            self.finish(Err(BackendError::Cancelled { operation }));
        }
    }
}

/// Minimal command completion seam used by the runtime motor queue.
///
/// It is intentionally not a backend facade: callers only get the command id,
/// cancellation, and one ordered result for the queued motor action.
pub struct CommandCompletion {
    command_id: String,
    receiver: oneshot::Receiver<Result<(), BackendError>>,
    state: Arc<CommandCompletionState>,
}

impl CommandCompletion {
    fn channel(command_id: String) -> (Self, Arc<CommandCompletionState>) {
        let (sender, receiver) = oneshot::channel();
        let state = Arc::new(CommandCompletionState {
            sender: parking_lot::Mutex::new(Some(sender)),
            settled_result: parking_lot::Mutex::new(None),
            settled_cv: parking_lot::Condvar::new(),
            finish_lock: parking_lot::Mutex::new(()),
            cancelled: AtomicBool::new(false),
            active_release: AtomicBool::new(false),
            release_on_cancel: AtomicBool::new(false),
            cancel_signal: parking_lot::Mutex::new(None),
            settled: AtomicBool::new(false),
            settled_signal: Notify::new(),
        });
        (
            Self {
                command_id,
                receiver,
                state: state.clone(),
            },
            state,
        )
    }

    pub fn command_id(&self) -> &str {
        &self.command_id
    }

    pub fn cancel(&self) {
        self.state
            .cancel(format!("command:{}", self.command_id), true);
    }

    pub async fn wait(self) -> Result<(), BackendError> {
        match self.receiver.await {
            Ok(result) => result,
            Err(_) => Err(BackendError::Cancelled {
                operation: format!("command:{}", self.command_id),
            }),
        }
    }

    /// Synchronous companion used by the frozen `release_all` facade method.
    /// The condition variable is independent of Tokio, so this remains safe
    /// to call from a caller that happens to be inside another async runtime.
    pub(crate) fn wait_blocking(self) -> Result<(), BackendError> {
        let mut result = self.state.settled_result.lock();
        while result.is_none() {
            self.state.settled_cv.wait(&mut result);
        }
        result
            .take()
            .expect("settled result exists after completion wait")
    }

    pub(crate) fn cancellation_handle(&self) -> CommandCompletionCancellation {
        CommandCompletionCancellation(self.state.clone())
    }
}

#[derive(Clone)]
pub(crate) struct CommandCompletionCancellation(Arc<CommandCompletionState>);

impl CommandCompletionCancellation {
    pub(crate) fn cancel(&self) {
        self.0
            .cancel("command completion cancelled by caller".to_owned(), true);
    }

    pub(crate) async fn wait_settled(&self) {
        while !self.0.settled.load(Ordering::Acquire) {
            self.0.settled_signal.notified().await;
        }
    }
}

struct QueuedCommand {
    envelope: BackendCommandEnvelope,
    completion: Option<Arc<CommandCompletionState>>,
}

#[derive(Clone, Debug)]
struct CloseEvidence {
    code: String,
    retryable: bool,
    deliberate: bool,
    kick: Option<BackendKick>,
    error: Option<BackendCloseError>,
    end_reason: Option<String>,
    failure: Option<BackendFailure>,
}

fn event_epoch(event: &ObservationEvent) -> u64 {
    match event {
        ObservationEvent::Entity(event) => event.connection_epoch,
        ObservationEvent::Block(event) => event.connection_epoch,
        ObservationEvent::Sound(event) => event.connection_epoch,
    }
}

fn contract_fact_source(source: FactSource) -> ContractFactSource {
    match source {
        FactSource::Commanded => ContractFactSource::Commanded,
        FactSource::ClientPredicted => ContractFactSource::ClientPredicted,
        FactSource::ServerObserved => ContractFactSource::ServerObserved,
    }
}

fn control_wakeup_error(control: &OperationControl, operation: &str) -> BackendError {
    match control.preflight(operation) {
        Err(error) => error,
        Ok(()) => BackendError::BackendFailure {
            failure: BackendFailure {
                code: BackendFailureCode::ProtocolError,
                message: format!("{operation} control woke without cancellation or deadline"),
                retryable: true,
            },
        },
    }
}

fn backend_error_from_directed(error: DirectedViewportError) -> BackendError {
    match error {
        DirectedViewportError::Backend(error) => error,
        DirectedViewportError::OutOfWorld { .. } => BackendError::BackendFailure {
            failure: BackendFailure {
                code: BackendFailureCode::ProtocolError,
                message: "full viewport encountered an out-of-world ray coordinate".to_owned(),
                retryable: false,
            },
        },
    }
}

fn contract_viewport_projection(projection: ViewportProjection) -> ContractViewportProjection {
    ContractViewportProjection {
        frame: ContractViewportFrame {
            coordinates:
                mineintent_contracts::minecraft::ViewportCoordinateSystem::MinecraftWorldAbsolute,
            self_pose: ContractViewportSelfPose {
                position: projection.frame.self_pose.position,
                yaw_degrees: projection.frame.self_pose.yaw_degrees,
                pitch_degrees: projection.frame.self_pose.pitch_degrees,
            },
            legend: ContractViewportLegend {
                visible_entities: projection.frame.legend.visible_entities,
                visible_blocks: projection.frame.legend.visible_blocks,
            },
        },
        standing_on_block: projection.standing_on_block.map(contract_viewport_block),
        looked_at_block: projection.looked_at_block.map(contract_viewport_block),
        visible_entities: ContractVisibleEntitiesView {
            items: projection
                .visible_entities
                .items
                .into_iter()
                .map(|entity| ContractVisibleEntityView {
                    entity_type: entity.entity_type,
                    player: entity.player,
                    position: entity.position,
                })
                .collect(),
            truncated: projection.visible_entities.truncated,
        },
        visible_blocks: ContractVisibleBlocksView {
            blocks: projection.visible_blocks.blocks,
            truncated: projection.visible_blocks.truncated,
        },
    }
}

fn contract_viewport_block(block: ViewportBlock) -> ContractViewportBlock {
    ContractViewportBlock {
        block: block.block,
        position: block.position.map(f64::from),
    }
}

fn read_block_from_world(world: &azalea::world::World, position: BlockPosition) -> BlockReadResult {
    let block_position = azalea::BlockPos {
        x: position.x,
        y: position.y,
        z: position.z,
    };
    let y = i64::from(block_position.y);
    let min_y = i64::from(world.chunks.min_y());
    let max_y_exclusive = min_y + i64::from(world.chunks.height());
    if y < min_y || y >= max_y_exclusive {
        return BlockReadResult::OutOfWorld;
    }
    let Some(state) = world.get_block_state(block_position) else {
        return BlockReadResult::Unloaded;
    };
    BlockReadResult::Loaded {
        block: block_snapshot(position, state),
    }
}

fn finite_f32(value: f64) -> Option<f32> {
    let value = value as f32;
    value.is_finite().then_some(value)
}

fn finite_f64(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

fn normalized_entity_snapshot_to_protocol(
    snapshot: &NormalizedEntitySnapshot,
) -> Option<ProtocolEntitySnapshot> {
    let position = [
        finite_f64(snapshot.position[0])?,
        finite_f64(snapshot.position[1])?,
        finite_f64(snapshot.position[2])?,
    ];
    let velocity = [
        finite_f64(snapshot.velocity[0])?,
        finite_f64(snapshot.velocity[1])?,
        finite_f64(snapshot.velocity[2])?,
    ];
    let yaw = finite_f32(snapshot.yaw)?;
    let pitch = finite_f32(snapshot.pitch)?;
    let head_yaw = match snapshot.head_yaw {
        Some(value) => Some(finite_f32(value)?),
        None => None,
    };
    let width = finite_f32(snapshot.width)?;
    let height = finite_f32(snapshot.height)?;
    let equipment = snapshot
        .equipment
        .iter()
        .map(|(slot, item_name, count)| {
            Some(crate::snapshot::EntityEquipmentSnapshot {
                slot: u8::try_from(*slot).ok()?,
                item_name: item_name.clone(),
                count: i32::try_from(*count).ok()?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(ProtocolEntitySnapshot {
        entity_key: snapshot.entity_key(),
        protocol_entity_id: snapshot.identity.protocol_id,
        entity_type: snapshot.entity_type.clone(),
        name: snapshot.name.clone(),
        username: snapshot.username.clone(),
        uuid: snapshot.uuid.clone(),
        position: Vec3Value {
            x: position[0],
            y: position[1],
            z: position[2],
        },
        velocity: Vec3Value {
            x: velocity[0],
            y: velocity[1],
            z: velocity[2],
        },
        yaw,
        pitch,
        head_yaw,
        width,
        height,
        on_ground: snapshot.on_ground,
        pose: snapshot.pose.clone(),
        held_item_name: snapshot.held_item_name.clone(),
        equipment,
        valid: snapshot.valid,
    })
}

fn upsert_entity_observation(observation: &mut ObservationState, snapshot: ProtocolEntitySnapshot) {
    if let Some(existing) = observation
        .tracked_entities
        .iter_mut()
        .find(|existing| existing.entity_key == snapshot.entity_key)
    {
        *existing = snapshot;
    } else {
        observation.tracked_entities.push(snapshot);
    }
    observation
        .tracked_entities
        .sort_by(|left, right| left.entity_key.cmp(&right.entity_key));
    observation.bump_generation();
}

fn record_entity_residual(
    observation: &mut ObservationState,
    entity_key: &str,
    head_yaw: Option<f64>,
    velocity: Option<[f64; 3]>,
    action: EntityResidualAction,
) {
    if matches!(action, EntityResidualAction::Clear) {
        observation
            .entity_residuals
            .retain(|residual| residual.entity_key != entity_key);
    }

    let update_head_yaw = head_yaw.and_then(finite_f32);
    let update_velocity = match action {
        EntityResidualAction::Update => velocity,
        EntityResidualAction::Retain | EntityResidualAction::Clear => None,
    };
    if update_head_yaw.is_none() && update_velocity.is_none() {
        return;
    }

    if let Some(existing) = observation
        .entity_residuals
        .iter_mut()
        .find(|residual| residual.entity_key == entity_key)
    {
        if update_head_yaw.is_some() {
            existing.head_yaw = update_head_yaw;
        }
        if update_velocity.is_some() {
            existing.velocity = update_velocity;
        }
        return;
    }

    if observation.entity_residuals.len() >= ENTITY_OBSERVATION_RESIDUAL_CAPACITY {
        observation.entity_residuals.remove(0);
    }
    observation
        .entity_residuals
        .push(EntityObservationResidual {
            entity_key: entity_key.to_owned(),
            head_yaw: update_head_yaw,
            velocity: update_velocity,
        });
}

fn normalized_entity_snapshot_to_contract(
    snapshot: NormalizedEntitySnapshot,
) -> ContractProtocolEntitySnapshot {
    contract_entity_snapshot(
        normalized_entity_snapshot_to_protocol(&snapshot)
            .expect("entity producer admits only finite representable snapshots"),
    )
    .expect("normalized entity snapshot should satisfy contract conversion")
}

fn merge_refreshed_tracked_entities(
    mut captured: Vec<ProtocolEntitySnapshot>,
    residuals: &mut Vec<EntityObservationResidual>,
    connection_epoch: u64,
) -> Vec<ProtocolEntitySnapshot> {
    residuals.retain(|residual| {
        residual
            .entity_key
            .starts_with(&format!("{connection_epoch}:"))
            && captured
                .iter()
                .any(|snapshot| snapshot.entity_key == residual.entity_key)
    });
    for snapshot in &mut captured {
        if !snapshot
            .entity_key
            .starts_with(&format!("{connection_epoch}:"))
        {
            continue;
        }
        let Some(residual) = residuals
            .iter()
            .find(|current| current.entity_key == snapshot.entity_key)
        else {
            continue;
        };
        // ECS owns the fields Azalea captures (position, velocity, body look,
        // dimensions, pose, UUID, and on-ground). Only fields with an explicit
        // packet residual survive refresh: head rotation is not represented
        // by capture, and PositionSync/SetEntityMotion velocity is retained
        // only when the lower handler does not write it into Physics.
        if residual.head_yaw.is_some() {
            snapshot.head_yaw = residual.head_yaw;
        }
        if let Some(velocity) = residual.velocity {
            snapshot.velocity = Vec3Value {
                x: velocity[0],
                y: velocity[1],
                z: velocity[2],
            };
        }
    }
    captured.sort_by(|left, right| left.entity_key.cmp(&right.entity_key));
    captured
}

fn normalized_entity_event_to_contract(
    event: NormalizedEntityEvent,
) -> ContractProtocolEntityEvent {
    match event {
        NormalizedEntityEvent::Spawned { entity } => ContractProtocolEntityEvent::Spawned {
            entity: normalized_entity_snapshot_to_contract(entity),
        },
        NormalizedEntityEvent::Moved { entity } => ContractProtocolEntityEvent::Moved {
            entity: normalized_entity_snapshot_to_contract(entity),
        },
        NormalizedEntityEvent::Updated { entity, changed } => {
            ContractProtocolEntityEvent::Updated {
                entity: normalized_entity_snapshot_to_contract(entity),
                changed,
            }
        }
        NormalizedEntityEvent::Animation {
            entity, animation, ..
        } => ContractProtocolEntityEvent::Animation {
            entity_key: entity.key(),
            animation,
        },
        NormalizedEntityEvent::Hurt {
            entity,
            possible_source,
        } => ContractProtocolEntityEvent::Hurt {
            entity_key: entity.key(),
            possible_source_entity_key: possible_source.map(EntityIdentity::key),
        },
        NormalizedEntityEvent::Removed { entity, last } => ContractProtocolEntityEvent::Removed {
            entity_key: entity.key(),
            last: normalized_entity_snapshot_to_contract(last),
            reason: ContractEntityRemovalReason::ProtocolRemoved,
        },
    }
}

#[derive(Clone, Component)]
struct BotState {
    shared: Arc<SharedRuntime>,
}

impl Default for BotState {
    fn default() -> Self {
        Self {
            shared: Arc::new(SharedRuntime::new(RunConfig::default())),
        }
    }
}

#[derive(Clone, Resource)]
struct SwarmState {
    shared: Arc<SharedRuntime>,
}

impl Default for SwarmState {
    fn default() -> Self {
        Self {
            shared: Arc::new(SharedRuntime::new(RunConfig::default())),
        }
    }
}

/// 在 Azalea 自己的 ECS schedule 内发送退出消息，避免跨任务直接写消息时
/// 与 Bevy 的双缓冲消息更新时序竞争。
struct RuntimeShutdownPlugin;

/// Publishes the entity packet facts after Azalea's packet handler has
/// applied them to ECS.  RemoveEntities is intentionally handled from the
/// shadow cache only because its handler has already despawned the entities.
struct EntityProducerPlugin;

impl Plugin for EntityProducerPlugin {
    fn build(&self, app: &mut App) {
        // These readers sit on the canonical ECS side of Azalea's adapters.
        // The high-level Event channel has no attempt token when a reconnect
        // reuses an entity, so lifecycle admission is made before those
        // listeners copy events to LocalPlayerEvents.
        app.add_systems(
            Update,
            (
                admit_canonical_join_source.before(azalea::join::handle_start_join_server_event),
                admit_canonical_disconnect_source
                    .before(azalea::events::disconnect_listener)
                    .before(azalea::join::handle_start_join_server_event),
                admit_canonical_connection_failure_source
                    .after(azalea::join::poll_create_connection_task)
                    .before(azalea::events::connection_failed_listener),
            ),
        )
        // `read_packets` applies each raw packet to ECS and then writes the
        // ReceiveGamePacketEvent batch. Reading immediately after it in the
        // same PreUpdate keeps the source epoch tied to that ordered batch;
        // no packet message is allowed to sit across an attempt transition.
        .add_systems(
            azalea::app::PreUpdate,
            produce_entity_packet_events.after(azalea::connection::read_packets),
        );
    }
}

/// Completes the backend-owned block/chunk/sound observation seam around
/// Azalea's existing ordered packet queue and world handlers.  It deliberately
/// leaves light, armor, transport, and SoundEntity outside this slice.
struct BlockSoundProducerPlugin;

/// Immutable source stamps produced by the one ordered raw-packet reducer.
///
/// The payloads remain in Azalea's own queue/messages. These vectors contain
/// only one optional admission stamp per vendor queue item / ReceiveChunkEvent,
/// are consumed by the corresponding Update system, and are cleared on every
/// consumption even when their length does not match. A mismatch therefore
/// fails closed for observation without becoming a cross-tick spill queue.
#[derive(Component, Default)]
struct CanonicalPacketSourceMetadata {
    block_updates: Vec<Option<CanonicalSourceAdmission>>,
    chunk_loads: VecDeque<CanonicalChunkLoadStamp>,
}

#[derive(Clone, Copy)]
struct CanonicalChunkLoadStamp {
    source: Option<CanonicalSourceAdmission>,
    chunk_x: i32,
    chunk_z: i32,
}

impl Plugin for BlockSoundProducerPlugin {
    fn build(&self, app: &mut App) {
        // The entity producer is the sole ordered raw-packet reducer. It
        // stamps block/chunk items and publishes direct sound/unload facts at
        // their packet positions before any Login/Respawn scope transition
        // can be observed by a later raw item.
        app.add_observer(attach_canonical_packet_source_metadata)
            // Chunk loading must be observed after Azalea has completed its
            // ReceiveChunkEvent handler.  Block updates then replace the vendor
            // handler at the same ordering boundary, preserving packet order and
            // post-state callbacks one item at a time.
            .add_systems(
                Update,
                (produce_chunk_loaded_events, produce_block_update_events)
                    .chain()
                    .after(azalea::chunks::handle_receive_chunk_event)
                    .before(azalea::block_update::handle_block_update_event),
            );
    }
}

fn attach_canonical_packet_source_metadata(
    trigger: On<Add, azalea::block_update::QueuedServerBlockUpdates>,
    mut commands: Commands,
) {
    commands
        .entity(trigger.entity)
        .insert(CanonicalPacketSourceMetadata::default());
}

fn canonical_sound_name(
    sound: &azalea::registry::Holder<
        azalea::registry::builtin::SoundEvent,
        azalea::core::sound::CustomSound,
    >,
) -> Option<String> {
    let name = match sound {
        azalea::registry::Holder::Direct(custom) => custom.sound_id.to_string(),
        azalea::registry::Holder::Reference(known) => known.to_string(),
    };
    (!name.is_empty()).then_some(name)
}

fn canonical_sound_packet(
    packet: &azalea::protocol::packets::game::ClientboundSound,
) -> Option<(String, [f64; 3], f64, f64)> {
    let name = canonical_sound_name(&packet.sound)?;
    let volume = f64::from(packet.volume);
    let pitch = f64::from(packet.pitch);
    if !volume.is_finite() || volume < 0.0 || !pitch.is_finite() {
        return None;
    }
    Some((
        name,
        [
            f64::from(packet.x) / 8.0,
            f64::from(packet.y) / 8.0,
            f64::from(packet.z) / 8.0,
        ],
        volume,
        pitch,
    ))
}

fn prove_has_skylight(
    common: &azalea::protocol::packets::common::CommonPlayerSpawnInfo,
    holder: &azalea::local_player::WorldHolder,
) -> Option<bool> {
    let world = holder.shared.read();
    let (_, dimension_data) = common.dimension_type(&world.registries)?;
    let value = dimension_data._extra.get("has_skylight")?;
    match value {
        azalea::protocol::simdnbt::owned::NbtTag::Byte(value) => match *value {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        },
        _ => None,
    }
}

fn current_light_geometry(
    holder: &azalea::local_player::WorldHolder,
) -> Option<LightSectionGeometry> {
    let world = holder.shared.read();
    LightSectionGeometry::from_world(&world)
}

fn record_canonical_packet_source_metadata(
    metadata: &mut Query<&mut CanonicalPacketSourceMetadata>,
    event: &azalea::packet::game::ReceiveGamePacketEvent,
    source: Option<CanonicalSourceAdmission>,
) {
    let Ok(mut metadata) = metadata.get_mut(event.entity) else {
        // The Update consumer will see the missing component as a metadata
        // mismatch and apply the vendor payloads without publishing them.
        return;
    };
    match event.packet.as_ref() {
        azalea::protocol::packets::game::ClientboundGamePacket::BlockUpdate(_) => {
            metadata.block_updates.push(source);
        }
        azalea::protocol::packets::game::ClientboundGamePacket::SectionBlocksUpdate(packet) => {
            for _ in &packet.states {
                metadata.block_updates.push(source);
            }
        }
        azalea::protocol::packets::game::ClientboundGamePacket::LevelChunkWithLight(packet) => {
            metadata.chunk_loads.push_back(CanonicalChunkLoadStamp {
                source,
                chunk_x: packet.x,
                chunk_z: packet.z,
            });
        }
        _ => {}
    }
}

fn produce_chunk_loaded_events(
    mut events: MessageReader<azalea::chunks::ReceiveChunkEvent>,
    state: Res<SwarmState>,
    world_holders: Query<&azalea::local_player::WorldHolder>,
    mut source_metadata: Query<&mut CanonicalPacketSourceMetadata>,
) {
    let mut pending = Vec::new();
    let mut metadata_aligned = true;
    for event in events.read() {
        let stamp = source_metadata
            .get_mut(event.entity)
            .ok()
            .and_then(|mut metadata| metadata.chunk_loads.pop_front());
        if stamp.is_none_or(|stamp| {
            stamp.chunk_x != event.packet.x
                || stamp.chunk_z != event.packet.z
                || stamp
                    .source
                    .is_some_and(|source| source.entity != event.entity)
        }) {
            metadata_aligned = false;
        }
        pending.push((event, stamp));
    }
    // A missing or extra ReceiveChunkEvent is a metadata mismatch, never a
    // reason to retain stamps for a later tick.
    for mut metadata in source_metadata.iter_mut() {
        if !metadata.chunk_loads.is_empty() {
            metadata_aligned = false;
        }
        metadata.chunk_loads.clear();
    }
    if !metadata_aligned {
        return;
    }

    for (event, stamp) in pending {
        let Some(source) = stamp.and_then(|stamp| stamp.source) else {
            continue;
        };
        let loaded = world_holders.get(event.entity).ok().is_some_and(|holder| {
            holder
                .shared
                .read()
                .chunks
                .get(&azalea::core::position::ChunkPos::new(
                    event.packet.x,
                    event.packet.z,
                ))
                .is_some()
        });
        if !loaded {
            continue;
        }
        state.shared.emit_canonical_observation_event(
            source,
            BackendEventPayload::Block(ContractProtocolBlockEvent::ChunkLoaded {
                chunk_x: event.packet.x,
                chunk_z: event.packet.z,
            }),
        );
    }
}

fn produce_block_update_events(
    mut query: Query<(
        bevy_ecs::entity::Entity,
        &mut azalea::block_update::QueuedServerBlockUpdates,
        &azalea::local_player::WorldHolder,
        &mut azalea::interact::BlockStatePredictionHandler,
        Option<&mut CanonicalPacketSourceMetadata>,
    )>,
    state: Res<SwarmState>,
) {
    for (entity, mut queued, world_holder, mut prediction_handler, source_metadata) in
        query.iter_mut()
    {
        // This takes ownership of Azalea's existing ordered queue storage; it
        // is not a second shadow/spill queue.  Every item is applied in order
        // and published/drained before the next callback can run.
        let updates = std::mem::take(&mut queued.list);
        let block_stamps =
            source_metadata.map(|mut metadata| std::mem::take(&mut metadata.block_updates));
        let stamps_aligned = block_stamps.as_ref().is_some_and(|stamps| {
            stamps.len() == updates.len()
                && stamps
                    .iter()
                    .all(|stamp| stamp.is_none_or(|source| source.entity == entity))
        });
        for (index, (position, block_state)) in updates.into_iter().enumerate() {
            let old_block = {
                let world = world_holder.shared.read();
                match read_block_from_world(
                    &world,
                    BlockPosition {
                        x: position.x,
                        y: position.y,
                        z: position.z,
                    },
                ) {
                    BlockReadResult::Loaded { block } => Some(block),
                    BlockReadResult::Unloaded | BlockReadResult::OutOfWorld => None,
                }
            };

            // Match Azalea's vendor handler exactly: a prediction acknowledgement
            // consumes the server state without rewriting the world; otherwise
            // the packet state is written to the shared world.
            let prediction_consumed =
                prediction_handler.update_known_server_state(position, block_state);
            if !prediction_consumed {
                let world = world_holder.shared.read();
                world.chunks.set_block_state(position, block_state);
            }

            let source = stamps_aligned
                .then(|| {
                    block_stamps
                        .as_ref()
                        .and_then(|stamps| stamps.get(index).copied())
                })
                .flatten()
                .flatten();
            let Some(source) = source else {
                continue;
            };
            let new_block = block_snapshot(
                BlockPosition {
                    x: position.x,
                    y: position.y,
                    z: position.z,
                },
                block_state,
            );
            state.shared.emit_canonical_observation_event(
                source,
                BackendEventPayload::Block(ContractProtocolBlockEvent::Updated {
                    old_block: old_block.map(contract_block_snapshot),
                    new_block: Some(contract_block_snapshot(new_block)),
                }),
            );
        }
    }
}

fn admit_canonical_join_source(
    mut events: MessageReader<azalea::join::StartJoinServerEvent>,
    state: Res<SwarmState>,
) {
    for event in events.read() {
        let source_epoch = state.shared.connection_epoch();
        state
            .shared
            .admit_canonical_join_started_with_token(source_epoch, Some(event.attempt_token));
    }
}

fn admit_canonical_disconnect_source(
    mut events: MessageReader<azalea::disconnect::DisconnectEvent>,
    state: Res<SwarmState>,
) {
    for event in events.read() {
        state.shared.admit_canonical_disconnected_source_with_token(
            event.entity,
            event.reason.as_ref().map(ToString::to_string),
            event.attempt_token,
        );
    }
}

fn admit_canonical_connection_failure_source(
    mut events: MessageReader<azalea::join::ConnectionFailedEvent>,
    state: Res<SwarmState>,
) {
    for event in events.read() {
        state
            .shared
            .admit_canonical_connection_failed_source_with_token(
                event.entity,
                format!("{:?}", event.error),
                Some(event.attempt_token),
            );
    }
}

fn is_admitted_non_local_entity(local_protocol_id: Option<i32>, target_id: i32) -> bool {
    local_protocol_id.is_some_and(|local_id| local_id != target_id)
}

fn produce_entity_packet_events(
    mut packets: MessageReader<azalea::packet::game::ReceiveGamePacketEvent>,
    state: Res<SwarmState>,
    local_entities: Query<&azalea::core::entity_id::MinecraftEntityId, With<LocalEntity>>,
    world_holders: Query<&azalea::local_player::WorldHolder>,
    mut source_metadata: Query<&mut CanonicalPacketSourceMetadata>,
) {
    for event in packets.read() {
        let source = state
            .shared
            .admit_canonical_source_with_token(event.entity, Some(event.attempt_token));
        record_canonical_packet_source_metadata(&mut source_metadata, event, source);

        match event.packet.as_ref() {
            azalea::protocol::packets::game::ClientboundGamePacket::Sound(packet) => {
                if let (Some(source), Some((sound_name, source_position, volume, pitch))) =
                    (source, canonical_sound_packet(packet))
                {
                    state.shared.emit_canonical_sound(
                        source,
                        sound_name,
                        source_position,
                        volume,
                        pitch,
                    );
                }
                continue;
            }
            azalea::protocol::packets::game::ClientboundGamePacket::ForgetLevelChunk(packet) => {
                if let Some(source) = source {
                    let _ = state
                        .shared
                        .remove_light_chunk(source, packet.pos.x, packet.pos.z);
                    state.shared.emit_canonical_observation_event(
                        source,
                        BackendEventPayload::Block(ContractProtocolBlockEvent::ChunkUnloaded {
                            chunk_x: packet.pos.x,
                            chunk_z: packet.pos.z,
                        }),
                    );
                }
                continue;
            }
            // Azalea's typed packet holder cannot represent an unknown numeric
            // registry reference, and the Mineflayer oracle has no
            // soundEntity listener. Do not fabricate a SoundEffect payload or
            // attach SoundEntity to this seam.
            azalea::protocol::packets::game::ClientboundGamePacket::SoundEntity(_) => continue,
            azalea::protocol::packets::game::ClientboundGamePacket::LightUpdate(packet) => {
                if let (Some(source), Ok(_holder), Some(geometry)) = (
                    source,
                    world_holders.get(event.entity),
                    world_holders
                        .get(event.entity)
                        .ok()
                        .and_then(current_light_geometry),
                ) {
                    let _ = state.shared.apply_light_packet(
                        source,
                        geometry,
                        packet.x,
                        packet.z,
                        &packet.light_data,
                        false,
                    );
                }
                continue;
            }
            azalea::protocol::packets::game::ClientboundGamePacket::LevelChunkWithLight(packet) => {
                if let (Some(source), Ok(_holder), Some(geometry)) = (
                    source,
                    world_holders.get(event.entity),
                    world_holders
                        .get(event.entity)
                        .ok()
                        .and_then(current_light_geometry),
                ) {
                    let _ = state.shared.apply_light_packet(
                        source,
                        geometry,
                        packet.x,
                        packet.z,
                        &packet.light_data,
                        true,
                    );
                }
                continue;
            }
            _ => {}
        }

        let Some(epoch) = source.map(|source| source.epoch) else {
            // Block/chunk metadata was already recorded as None above. The
            // Update systems will still apply vendor payloads but publish no
            // observation from an invalid source.
            continue;
        };

        // Login and Respawn handlers emit WorldLoadedEvent on a separate
        // Bevy stream while connection::read_packets queues all received
        // packet messages until the raw-packet loop ends. These raw packet
        // variants are the authoritative boundary positions: reset the
        // complete scope and publish the packet's dimension before the next
        // packet in this same read batch is admitted.
        match event.packet.as_ref() {
            azalea::protocol::packets::game::ClientboundGamePacket::Login(packet) => {
                let has_skylight = world_holders
                    .get(event.entity)
                    .ok()
                    .and_then(|holder| prove_has_skylight(&packet.common, holder));
                state
                    .shared
                    .reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
                        event.entity,
                        epoch,
                        Some(packet.common.dimension.to_string()),
                        has_skylight,
                    );
                continue;
            }
            azalea::protocol::packets::game::ClientboundGamePacket::Respawn(packet) => {
                let has_skylight = world_holders
                    .get(event.entity)
                    .ok()
                    .and_then(|holder| prove_has_skylight(&packet.common, holder));
                state
                    .shared
                    .reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
                        event.entity,
                        epoch,
                        Some(packet.common.dimension.to_string()),
                        has_skylight,
                    );
                continue;
            }
            _ => {}
        }

        // The packet adapter is fail-closed: without both LocalEntity and its
        // protocol id we cannot prove that an entity packet is not self. The
        // login/respawn boundary above remains processable so it can establish
        // the next ECS scope before local identity is available again.
        let local_protocol_id = local_entities.get(event.entity).ok().map(|id| **id);

        if let azalea::protocol::packets::game::ClientboundGamePacket::UpdateAttributes(packet) =
            event.packet.as_ref()
        {
            if source.is_some_and(|source| {
                local_protocol_id == Some(packet.entity_id.0)
                    && state.shared.apply_armor_packet(source, &packet.values)
            }) {
                // The cache mutation, including an invalid armor result,
                // already happened under the canonical admission.
            }
            continue;
        }

        let admission = state.shared.next_entity_packet_admission();
        match event.packet.as_ref() {
            azalea::protocol::packets::game::ClientboundGamePacket::AddEntity(packet) => {
                if !is_admitted_non_local_entity(local_protocol_id, packet.id.0) {
                    continue;
                }
                let movement = packet.movement.to_vec3();
                let dimensions = EntityDimensions::from(packet.entity_type);
                let entity_type = canonical_entity_type(&packet.entity_type.to_string());
                state.shared.emit_entity_input(
                    event.entity,
                    epoch,
                    EntityProducerInput::Spawn {
                        token: EntityProducerToken::new(
                            epoch,
                            format!("packet-admission:{admission}:add"),
                        ),
                        snapshot: NormalizedEntitySnapshot {
                            identity: EntityIdentity::new(epoch, packet.id.0),
                            entity_type,
                            uuid: Some(packet.uuid.to_string()),
                            name: None,
                            username: None,
                            position: [packet.position.x, packet.position.y, packet.position.z],
                            velocity: [movement.x, movement.y, movement.z],
                            yaw: compact_rotation_radians(packet.y_rot),
                            pitch: compact_pitch_radians(packet.x_rot),
                            head_yaw: Some(compact_rotation_radians(packet.y_head_rot)),
                            width: f64::from(dimensions.width),
                            height: f64::from(dimensions.height),
                            on_ground: false,
                            pose: Some("standing".to_owned()),
                            held_item_name: None,
                            equipment: Vec::new(),
                            valid: true,
                        },
                    },
                );
            }
            azalea::protocol::packets::game::ClientboundGamePacket::MoveEntityPos(packet) => {
                if !is_admitted_non_local_entity(local_protocol_id, packet.entity_id.0) {
                    continue;
                }
                state.shared.emit_entity_input(
                    event.entity,
                    epoch,
                    EntityProducerInput::Move {
                        token: EntityProducerToken::new(
                            epoch,
                            format!("packet-admission:{admission}:move-pos"),
                        ),
                        patch: EntityMovePatch::relative(
                            EntityIdentity::new(epoch, packet.entity_id.0),
                            Some([
                                i64::from(packet.delta.xa),
                                i64::from(packet.delta.ya),
                                i64::from(packet.delta.za),
                            ]),
                            None,
                            packet.on_ground,
                        ),
                    },
                );
            }
            azalea::protocol::packets::game::ClientboundGamePacket::MoveEntityPosRot(packet) => {
                if !is_admitted_non_local_entity(local_protocol_id, packet.entity_id.0) {
                    continue;
                }
                state.shared.emit_entity_input(
                    event.entity,
                    epoch,
                    EntityProducerInput::Move {
                        token: EntityProducerToken::new(
                            epoch,
                            format!("packet-admission:{admission}:move-pos-rot"),
                        ),
                        patch: EntityMovePatch::relative(
                            EntityIdentity::new(epoch, packet.entity_id.0),
                            Some([
                                i64::from(packet.delta.xa),
                                i64::from(packet.delta.ya),
                                i64::from(packet.delta.za),
                            ]),
                            Some([packet.look_direction.y_rot, packet.look_direction.x_rot]),
                            packet.on_ground,
                        ),
                    },
                );
            }
            azalea::protocol::packets::game::ClientboundGamePacket::MoveEntityRot(packet) => {
                if !is_admitted_non_local_entity(local_protocol_id, packet.entity_id.0) {
                    continue;
                }
                state.shared.emit_entity_input(
                    event.entity,
                    epoch,
                    EntityProducerInput::Move {
                        token: EntityProducerToken::new(
                            epoch,
                            format!("packet-admission:{admission}:move-rot"),
                        ),
                        patch: EntityMovePatch::relative(
                            EntityIdentity::new(epoch, packet.entity_id.0),
                            None,
                            Some([packet.look_direction.y_rot, packet.look_direction.x_rot]),
                            packet.on_ground,
                        ),
                    },
                );
            }
            azalea::protocol::packets::game::ClientboundGamePacket::TeleportEntity(packet) => {
                if !is_admitted_non_local_entity(local_protocol_id, packet.id.0) {
                    continue;
                }
                state.shared.emit_entity_input_with_velocity_residual(
                    event.entity,
                    epoch,
                    EntityProducerInput::Move {
                        token: EntityProducerToken::new(
                            epoch,
                            format!("packet-admission:{admission}:teleport"),
                        ),
                        patch: EntityMovePatch::teleport(
                            EntityIdentity::new(epoch, packet.id.0),
                            [
                                packet.change.pos.x,
                                packet.change.pos.y,
                                packet.change.pos.z,
                            ],
                            [
                                f64::from(packet.change.look_direction.y_rot()),
                                f64::from(packet.change.look_direction.x_rot()),
                            ],
                            [packet.relative.x, packet.relative.y, packet.relative.z],
                            [packet.relative.y_rot, packet.relative.x_rot],
                            [
                                packet.change.delta.x,
                                packet.change.delta.y,
                                packet.change.delta.z,
                            ],
                            [
                                packet.relative.delta_x,
                                packet.relative.delta_y,
                                packet.relative.delta_z,
                            ],
                            packet.relative.rotate_delta,
                            packet.on_ground,
                        ),
                    },
                    EntityResidualAction::Clear,
                );
            }
            azalea::protocol::packets::game::ClientboundGamePacket::EntityPositionSync(packet) => {
                if !is_admitted_non_local_entity(local_protocol_id, packet.id.0) {
                    continue;
                }
                state.shared.emit_entity_input_with_velocity_residual(
                    event.entity,
                    epoch,
                    EntityProducerInput::Move {
                        token: EntityProducerToken::new(
                            epoch,
                            format!("packet-admission:{admission}:position-sync"),
                        ),
                        patch: EntityMovePatch::position_sync(
                            EntityIdentity::new(epoch, packet.id.0),
                            [
                                packet.values.pos.x,
                                packet.values.pos.y,
                                packet.values.pos.z,
                            ],
                            [
                                f64::from(packet.values.look_direction.y_rot()),
                                f64::from(packet.values.look_direction.x_rot()),
                            ],
                            [
                                packet.values.delta.x,
                                packet.values.delta.y,
                                packet.values.delta.z,
                            ],
                            packet.on_ground,
                        ),
                    },
                    EntityResidualAction::Update,
                );
            }
            azalea::protocol::packets::game::ClientboundGamePacket::RotateHead(packet) => {
                if !is_admitted_non_local_entity(local_protocol_id, packet.entity_id.0) {
                    continue;
                }
                state.shared.emit_entity_input(
                    event.entity,
                    epoch,
                    EntityProducerInput::Move {
                        token: EntityProducerToken::new(
                            epoch,
                            format!("packet-admission:{admission}:rotate-head"),
                        ),
                        patch: EntityMovePatch::rotate_head(
                            EntityIdentity::new(epoch, packet.entity_id.0),
                            packet.y_head_rot,
                        ),
                    },
                );
            }
            azalea::protocol::packets::game::ClientboundGamePacket::SetEntityMotion(packet) => {
                if !is_admitted_non_local_entity(local_protocol_id, packet.id.0) {
                    continue;
                }
                let velocity = packet.delta.to_vec3();
                state.shared.emit_entity_motion_residual(
                    event.entity,
                    epoch,
                    EntityProducerToken::new(
                        epoch,
                        format!("packet-admission:{admission}:set-motion"),
                    ),
                    EntityIdentity::new(epoch, packet.id.0),
                    [velocity.x, velocity.y, velocity.z],
                );
            }
            azalea::protocol::packets::game::ClientboundGamePacket::RemoveEntities(packet) => {
                for (index, id) in packet.entity_ids.iter().copied().enumerate() {
                    if !is_admitted_non_local_entity(local_protocol_id, id.0) {
                        continue;
                    }
                    state.shared.emit_entity_input(
                        event.entity,
                        epoch,
                        EntityProducerInput::Remove {
                            token: EntityProducerToken::new(
                                epoch,
                                format!("packet-admission:{admission}:remove:{id:?}:{index}"),
                            ),
                            entity: EntityIdentity::new(epoch, id.0),
                        },
                    );
                }
            }
            _ => {}
        }
    }
}

/// 只从 Azalea 的底层接收包消息中筛选服务端位置校正。
///
/// Azalea 的 `packet-event` feature 会把每一个游戏包再转发到高层
/// `LocalPlayerEvents` unbounded channel；对带区块流量的 26.1 服务器而言，
/// 这会制造无意义的积压。自有插件直接读取同一条 ECS message，只保留
/// `ClientboundPlayerPosition` 这一条 M4 需要的服务端事实。
struct ServerPositionCorrectionPlugin;

impl Plugin for ServerPositionCorrectionPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                record_server_position_corrections,
                reset_spawn_marker_on_world_loaded,
            ),
        );
        app.add_observer(record_respawn_packet);
    }
}

/// Azalea 的 `Spawn` 去重标记只在 `Login` 时清除；26.1 的跨维度/重生包走
/// `WorldLoadedEvent`，如果保留旧标记，新维度的区块加载不会再产生 Spawn。
/// 重置这两个加载边界后，下一批区块会重新进入标准 Spawn 处理，避免在这里
/// 复制一套快照或生命周期逻辑。
fn reset_spawn_marker_on_world_loaded(
    mut world_loaded: MessageReader<azalea::packet::game::WorldLoadedEvent>,
    mut commands: Commands,
    state: Res<SwarmState>,
) {
    for event in world_loaded.read() {
        if !state
            .shared
            .observe_dimension_from_world_boundary_with_token(
                event.entity,
                event.name.to_string(),
                Some(event.attempt_token),
            )
        {
            continue;
        }
        commands.entity(event.entity).remove::<(
            azalea::events::SentSpawnEvent,
            azalea::entity::InLoadedChunk,
        )>();
    }
}

fn record_respawn_packet(
    trigger: On<azalea::packet::game::SendGamePacketEvent>,
    state: Res<SwarmState>,
) {
    let azalea::protocol::packets::game::ServerboundGamePacket::ClientCommand(packet) =
        &trigger.event().packet
    else {
        return;
    };
    if !matches!(
        packet.action,
        azalea::protocol::packets::game::s_client_command::Action::PerformRespawn
    ) {
        return;
    }
    // 这是本地明确请求的重生过渡；只有后续 Spawn 才算服务端确认。
    let from_dimension = state
        .shared
        .writer
        .lock()
        .dimension
        .clone()
        .unwrap_or_else(|| "unknown".to_owned());
    state.shared.emit_respawn_transition_started(from_dimension);
}

fn record_server_position_corrections(
    mut packets: MessageReader<azalea::packet::game::ReceiveGamePacketEvent>,
    state: Res<SwarmState>,
) {
    for event in packets.read() {
        let azalea::protocol::packets::game::ClientboundGamePacket::PlayerPosition(packet) =
            event.packet.as_ref()
        else {
            continue;
        };
        let Some(source) = state
            .shared
            .admit_canonical_source_with_token(event.entity, Some(event.attempt_token))
        else {
            continue;
        };
        // 这是服务端主动校正玩家位置的协议事实；它不代表每个 tick
        // 都有一个服务端坐标包，因此客户端预测轨迹仍单独记录。
        state.shared.emit_canonical_observation_event(
            source,
            BackendEventPayload::SelfState(ContractProtocolSelfEvent::ServerPositionCorrection {
                teleport_id: packet.id,
                position: ContractVec3Value {
                    x: packet.change.pos.x,
                    y: packet.change.pos.y,
                    z: packet.change.pos.z,
                },
                velocity: ContractVec3Value {
                    x: packet.change.delta.x,
                    y: packet.change.delta.y,
                    z: packet.change.delta.z,
                },
                yaw: packet.change.look_direction.y_rot(),
                pitch: packet.change.look_direction.x_rot(),
                relative: RelativeMovementFlags {
                    x: packet.relative.x,
                    y: packet.relative.y,
                    z: packet.relative.z,
                    yaw: packet.relative.y_rot,
                    pitch: packet.relative.x_rot,
                    delta_x: packet.relative.delta_x,
                    delta_y: packet.relative.delta_y,
                    delta_z: packet.relative.delta_z,
                    rotate_delta: packet.relative.rotate_delta,
                },
            }),
        );
    }
}

impl Plugin for RuntimeShutdownPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, emit_app_exit_when_stopping);
        // 死亡后冻结本地物理状态必须在 Azalea 的常规 Update 查询完成后执行，
        // 否则会与更新碰撞盒、准星命中结果的系统产生无序写入警告。
        app.add_systems(PostUpdate, freeze_dead_local_player);
    }
}

/// 禁用自动重生时，死亡是一个需要保持的事实；冻结本地物理，避免死后
/// 因客户端重力继续把观察位置推进到世界边界之外。
fn freeze_dead_local_player(
    mut query: Query<(&mut Physics, &Position), (With<LocalEntity>, With<Dead>)>,
) {
    for (mut physics, position) in &mut query {
        physics.velocity = azalea::Vec3::ZERO;
        physics.set_on_ground(true);
        physics.set_old_pos(*position);
    }
}

fn emit_app_exit_when_stopping(mut app_exit: MessageWriter<AppExit>, state: Res<SwarmState>) {
    if state.shared.stopping.load(Ordering::Acquire) {
        app_exit.write(AppExit::Success);
    }
}

fn direction_for(directions: &[MotorDirection]) -> WalkDirection {
    let forward = directions.contains(&MotorDirection::Forward);
    let back = directions.contains(&MotorDirection::Back);
    let left = directions.contains(&MotorDirection::Left);
    let right = directions.contains(&MotorDirection::Right);
    match (forward, back, left, right) {
        (true, false, true, false) => WalkDirection::ForwardLeft,
        (true, false, false, true) => WalkDirection::ForwardRight,
        (false, true, true, false) => WalkDirection::BackwardLeft,
        (false, true, false, true) => WalkDirection::BackwardRight,
        (true, false, false, false) => WalkDirection::Forward,
        (false, true, false, false) => WalkDirection::Backward,
        (false, false, true, false) => WalkDirection::Left,
        (false, false, false, true) => WalkDirection::Right,
        _ => WalkDirection::None,
    }
}

fn sprint_direction(direction: WalkDirection) -> Option<SprintDirection> {
    match direction {
        WalkDirection::Forward => Some(SprintDirection::Forward),
        WalkDirection::ForwardLeft => Some(SprintDirection::ForwardLeft),
        WalkDirection::ForwardRight => Some(SprintDirection::ForwardRight),
        _ => None,
    }
}

/// 断线时本地玩家实体可能已经被 Azalea 移除；运动清理必须使用可失败查询。
fn try_set_movement_flags(bot: &Client, jumping: bool, crouching: bool) -> bool {
    bot.try_query_self::<(&mut azalea::entity::Jumping, &mut azalea::PhysicsState), _>(
        |(mut jumping_component, mut physics)| {
            **jumping_component = jumping;
            physics.trying_to_crouch = crouching;
        },
    )
    .is_ok()
}

fn finish_command(
    completion: &Option<Arc<CommandCompletionState>>,
    result: Result<(), BackendError>,
) {
    if let Some(completion) = completion {
        completion.finish(result);
    }
}

fn reject_command_after_stop(
    shared: &Arc<SharedRuntime>,
    command_id: &str,
    completion: &Option<Arc<CommandCompletionState>>,
) -> bool {
    if shared.command_execution_allowed() {
        return false;
    }
    finish_command(
        completion,
        Err(BackendError::Cancelled {
            operation: format!("command:{command_id}"),
        }),
    );
    true
}

fn command_component_failure(operation: &str) -> BackendError {
    BackendError::BackendFailure {
        failure: BackendFailure {
            code: BackendFailureCode::ProtocolError,
            message: format!("{operation} requires an active local player"),
            retryable: true,
        },
    }
}

/// Release one active Move and settle its completion only after the physical
/// release attempt and the shared active-state cleanup have both completed.
/// The actuator is injected so the ordering seam is testable without creating
/// an Azalea client; production supplies the real walk/flag release closure.
fn release_active_movement_and_finish(
    shared: &Arc<SharedRuntime>,
    command_id: &str,
    generation: u64,
    completion: &Option<Arc<CommandCompletionState>>,
    release_inputs: impl FnOnce() -> bool,
    failure_operation: &str,
    result_if_released: Result<(), BackendError>,
) {
    let released = {
        let _admission = shared.command_admission.lock();
        let owns_movement = shared.movement_generation.load(Ordering::Acquire) == generation
            && shared.active_movement_id.lock().as_deref() == Some(command_id);
        if !owns_movement {
            return;
        }
        // Serialize the physical release with command admission.  This is a
        // short synchronous actuator section; completion settlement and stop
        // finalization remain outside the lock.
        release_inputs()
    };
    let result = if released {
        result_if_released
    } else {
        Err(command_component_failure(failure_operation))
    };
    // Keep the active completion/id visible to stop until the physical release
    // result has been settled. A stop racing this section must defer stopped;
    // the generation/id checks below prevent an old task from clearing a new
    // movement that was admitted after its release.
    finish_command(completion, result);
    {
        let _admission = shared.command_admission.lock();
        if shared.clear_registered_active_movement(command_id, generation, &None, completion) {
            shared.active_movement_cancel_signal.lock().take();
        }
    }
    shared.finalize_stop_if_ready();
}

fn handle_command(bot: &Client, shared: &Arc<SharedRuntime>, queued: QueuedCommand) {
    let QueuedCommand {
        envelope,
        completion,
    } = queued;
    let command_id = envelope.id;
    if completion
        .as_ref()
        .is_some_and(|completion| completion.cancelled.load(Ordering::Acquire))
    {
        return;
    }
    if reject_command_after_stop(shared, &command_id, &completion) {
        return;
    }
    match envelope.command {
        BackendCommand::SendChat { message } => {
            match shared.with_command_admission(|| bot.chat(message)) {
                Ok(()) => finish_command(&completion, Ok(())),
                Err(()) => {
                    finish_command(
                        &completion,
                        Err(BackendError::Cancelled {
                            operation: format!("command:{command_id}"),
                        }),
                    );
                }
            }
        }
        BackendCommand::LookRelative {
            yaw_degrees,
            pitch_degrees,
        } => {
            let result = shared.with_command_admission(|| {
                let direction = bot.direction();
                bot.set_direction(
                    direction.y_rot() - yaw_degrees,
                    (direction.x_rot() - pitch_degrees).clamp(-90.0, 90.0),
                );
            });
            match result {
                Ok(()) => finish_command(&completion, Ok(())),
                Err(()) => finish_command(
                    &completion,
                    Err(BackendError::Cancelled {
                        operation: format!("command:{command_id}"),
                    }),
                ),
            }
        }
        BackendCommand::Move {
            directions,
            duration_ms,
            sprint,
            jump,
            crouch,
        } => {
            shared.cancel_active_movement(false);
            let direction = direction_for(&directions);
            let generation = shared.movement_generation.fetch_add(1, Ordering::AcqRel) + 1;
            let registration =
                shared.register_active_movement(&command_id, generation, duration_ms, &completion);
            let ActiveMovementRegistration::Started { cancel_signal } = registration else {
                return;
            };

            // The cancellation/generation check and the first actuator call
            // share one admission point. A cancellation that wins cannot
            // touch the bot; an actuator that wins leaves the same generation
            // for the release task to clean up.
            let actuator_result =
                shared.with_active_movement_admission(&command_id, generation, &completion, || {
                    if sprint.unwrap_or(false) {
                        if let Some(sprint_direction) = sprint_direction(direction) {
                            bot.sprint(sprint_direction);
                        } else {
                            bot.walk(direction);
                        }
                    } else {
                        bot.walk(direction);
                    }
                    if !try_set_movement_flags(bot, jump.unwrap_or(false), crouch.unwrap_or(false))
                    {
                        bot.walk(WalkDirection::None);
                        return false;
                    }
                    if duration_ms == 0 {
                        bot.walk(WalkDirection::None);
                    }
                    true
                });
            let started = match actuator_result {
                Ok(started) => started,
                Err(()) => {
                    shared.cancel_registered_active_movement(
                        &command_id,
                        generation,
                        &cancel_signal,
                        &completion,
                    );
                    return;
                }
            };

            if !started {
                shared.clear_registered_active_movement(
                    &command_id,
                    generation,
                    &cancel_signal,
                    &completion,
                );
                finish_command(&completion, Err(command_component_failure("move")));
                shared.finish_active_movement_registration();
                return;
            }

            if duration_ms == 0 {
                shared.clear_registered_active_movement(
                    &command_id,
                    generation,
                    &cancel_signal,
                    &completion,
                );
                finish_command(&completion, Ok(()));
                shared.finish_active_movement_registration();
            } else {
                let cancel_signal = cancel_signal.expect("duration-positive move signal");
                let bot_to_stop = bot.clone();
                let shared = shared.clone();
                let task_shared = shared.clone();
                let completion_for_task = completion.clone();
                tokio::task::spawn_local(async move {
                    let duration = tokio::time::sleep(Duration::from_millis(duration_ms));
                    tokio::pin!(duration);
                    tokio::select! {
                        _ = &mut duration => {
                            let cancelled = completion_for_task
                                .as_ref()
                                .is_some_and(|completion| completion.cancelled.load(Ordering::Acquire))
                                || task_shared.stopping.load(Ordering::Acquire);
                            release_active_movement_and_finish(
                                &task_shared,
                                &command_id,
                                generation,
                                &completion_for_task,
                                || {
                                    let released = try_set_movement_flags(&bot_to_stop, false, false);
                                    bot_to_stop.walk(WalkDirection::None);
                                    released
                                },
                                "move release",
                                if cancelled {
                                    Err(BackendError::Cancelled {
                                        operation: format!("command:{command_id}"),
                                    })
                                } else {
                                    Ok(())
                                },
                            );
                        }
                        _ = cancel_signal.notified() => {
                            release_active_movement_and_finish(
                                &task_shared,
                                &command_id,
                                generation,
                                &completion_for_task,
                                || {
                                    let released = try_set_movement_flags(&bot_to_stop, false, false);
                                    bot_to_stop.walk(WalkDirection::None);
                                    released
                                },
                                "cancel move",
                                Err(BackendError::Cancelled {
                                    operation: format!("command:{command_id}"),
                                }),
                            );
                        }
                    }
                });
                shared.finish_active_movement_registration();
            }
        }
        BackendCommand::ReleaseAll => {
            let previous_id = shared.active_movement_id.lock().clone();
            let previous_generation = shared.movement_generation.load(Ordering::Acquire);
            let previous_completion = shared
                .cancel_active_movement(true)
                .map(Some)
                .unwrap_or(None);
            if let Some(previous_id) = previous_id {
                release_active_movement_and_finish(
                    shared,
                    &previous_id,
                    previous_generation,
                    &previous_completion,
                    || {
                        let released = try_set_movement_flags(bot, false, false);
                        bot.walk(WalkDirection::None);
                        released
                    },
                    "release_all move",
                    Err(BackendError::Cancelled {
                        operation: format!("command:{previous_id}"),
                    }),
                );
            } else {
                let released = match shared.with_command_admission(|| {
                    let released = try_set_movement_flags(bot, false, false);
                    bot.walk(WalkDirection::None);
                    released
                }) {
                    Ok(released) => released,
                    Err(()) => {
                        finish_command(
                            &completion,
                            Err(BackendError::Cancelled {
                                operation: format!("command:{command_id}"),
                            }),
                        );
                        return;
                    }
                };
                shared.clear_idle_movement_state(previous_generation);
                finish_command(
                    &previous_completion,
                    if released {
                        Err(BackendError::Cancelled {
                            operation: "movement released by release_all".to_owned(),
                        })
                    } else {
                        Err(command_component_failure("release_all move"))
                    },
                );
            }
            match shared.with_command_admission(|| {
                let released = try_set_movement_flags(bot, false, false);
                bot.walk(WalkDirection::None);
                released
            }) {
                Ok(true) => finish_command(&completion, Ok(())),
                Ok(false) => {
                    finish_command(&completion, Err(command_component_failure("release_all")))
                }
                Err(()) => finish_command(
                    &completion,
                    Err(BackendError::Cancelled {
                        operation: format!("command:{command_id}"),
                    }),
                ),
            }
        }
        BackendCommand::Respawn => {
            // 服务端的死亡包与 waitingForRespawn 状态可能跨一个网络 tick；
            // 只延迟这一条已经明确请求的动作，避免请求在服务端状态切换前到达。
            // 仍走 Azalea 自带 RespawnPlugin 的消息链，保持实体绑定和 ECS 时序。
            let delayed_bot = bot.clone();
            let delayed_shared = shared.clone();
            tokio::task::spawn_local(async move {
                tokio::time::sleep(RESPAWN_SETTLE_DELAY).await;
                let _ = delayed_shared.with_command_admission(|| {
                    if delayed_bot
                        .try_query_self::<&LocalEntity, _>(|_| ())
                        .is_err()
                    {
                        return;
                    }
                    delayed_bot
                        .ecs
                        .write()
                        .write_message(azalea::respawn::PerformRespawnEvent {
                            entity: delayed_bot.entity,
                        });
                });
            });
            finish_command(&completion, Ok(()));
        }
    }
}

fn process_pending_commands(bot: &Client, shared: &Arc<SharedRuntime>) {
    // 连接建立前的命令保留在队列中，避免把 chat/motor 静默丢在握手阶段。
    if !bot.logged_in() {
        return;
    }
    while let Some(command) = shared.next_command_for_processing() {
        handle_command(bot, shared, command);
    }
}

async fn handle_client(bot: Client, event: Event, state: BotState) {
    let shared = &state.shared;
    if matches!(event, Event::Spawn | Event::Tick) {
        if !shared.command_execution_allowed() {
            return;
        }
        process_pending_commands(&bot, &state.shared);
    }
    match event {
        Event::Init => {
            // Swarm 重连在某些路径复用已有本地玩家事件发送器，不一定再次发出
            // Event::Init；重连调度器会预留 epoch，若 Init 到达则消费该预留，避免
            // 同一次握手被错误地记成两个 epoch。
            if shared
                .consume_attempt_for_transport_init_and_bind_with_token(
                    bot.entity,
                    bot.attempt_token(),
                )
                .is_none()
                || !shared.command_execution_allowed()
                || !shared.set_active_client_if_current(&bot)
            {
                return;
            }
        }
        Event::Login => {
            if !shared.client_is_current_owner(&bot) || !shared.command_execution_allowed() {
                return;
            }
            let dimension = bot
                .try_query_self::<Option<&azalea::world::WorldName>, _>(|world_name| {
                    world_name
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "minecraft:overworld".to_owned())
                })
                .unwrap_or_else(|_| "minecraft:overworld".to_owned());
            if !shared.command_execution_allowed() {
                return;
            }
            shared.emit_logged_in("26.1.2", dimension);
        }
        Event::Spawn => {
            if !shared.client_is_current_owner(&bot) || !shared.command_execution_allowed() {
                return;
            }
            let (spawn_allowed, was_dead) = {
                let _admission = shared.command_admission.lock();
                let was_dead = shared.death_reported.load(Ordering::Acquire);
                if !shared.command_execution_allowed_without_lock() {
                    (false, was_dead)
                } else {
                    shared.ready.store(true, Ordering::Release);
                    shared.death_reported.store(false, Ordering::Release);
                    (true, was_dead)
                }
            };
            if !spawn_allowed {
                return;
            }
            if !shared.set_world_if_running(bot.world()) {
                return;
            }
            let snapshot = shared.refresh_snapshot(&bot, true, FactSource::ServerObserved);
            if let Some(snapshot) = snapshot.as_ref() {
                if !shared.set_dimension_if_running(snapshot.world.dimension.clone()) {
                    return;
                }
            }
            if let Some(snapshot) = snapshot {
                if was_dead {
                    shared.emit_respawned(snapshot.world.dimension.clone());
                }
                shared.emit_ready(snapshot.snapshot_revision);
                shared.emit_snapshot(snapshot, FactSource::ServerObserved);
            } else {
                shared.emit_ready(shared.snapshot_revision.load(Ordering::Acquire));
            }

            let _ = shared.with_command_admission(|| {
                if !shared.initial_chat_sent.swap(true, Ordering::AcqRel) {
                    if let Some(message) = shared.config.initial_chat.clone() {
                        bot.chat(message);
                    }
                }
            });

            let start_timer = {
                let _admission = shared.command_admission.lock();
                shared.command_execution_allowed_without_lock()
                    && !shared.timer_started.swap(true, Ordering::AcqRel)
            };
            if start_timer && shared.config.auto_stop {
                let duration = shared.config.duration;
                let shared = state.shared.clone();
                tokio::task::spawn_local(async move {
                    tokio::time::sleep(duration).await;
                    shared.initiate_stop("duration_elapsed");
                });
            }
        }
        Event::KeepAlive(_id) => {}
        Event::Chat(packet) => {
            if !shared.client_is_current_owner(&bot) {
                return;
            }
            shared.emit_if_running(
                FactSource::ServerObserved,
                BackendEventPayload::Chat(ContractProtocolChatEvent {
                    sender_username: packet.sender(),
                    plain_text: packet.content(),
                    position: Some(ChatPosition::Chat),
                    verified: None,
                }),
            );
        }
        Event::Death(_) => {
            if !shared.client_is_current_owner(&bot) {
                return;
            }
            if shared
                .admit_death_and_release(|| {
                    let released = try_set_movement_flags(&bot, false, false);
                    bot.walk(WalkDirection::None);
                    released
                })
                .is_none()
            {
                return;
            }
            if let Some(snapshot) = shared.refresh_snapshot(&bot, true, FactSource::ServerObserved)
            {
                shared.emit_snapshot(snapshot, FactSource::ServerObserved);
            }
        }
        Event::Disconnect(_reason) => {
            // This channel has no attempt token. Canonical DisconnectEvent
            // admission already ran before the listener copied this event.
            // Disconnect 会由 Azalea 同步移除本地玩家的运动组件；此处只
            // 更新运行时状态，不再向已失效的实体投递 walk/jump/crouch 消息。
        }
        Event::ConnectionFailed(_error) => {
            // The canonical ECS source already admitted and closed this
            // exact entity/epoch.  A high-level event without that bounded
            // hand-off is source-less and must have no lifecycle side effect.
            if !shared.take_canonical_connection_failure_followup_with_token(
                bot.entity,
                bot.attempt_token(),
            ) {
                return;
            }
            // ConnectionFailed 不一定伴随单独的 swarm disconnect；显式断开让
            // 统一的 close/reconnect 分支接管，不把内部错误泄漏成旧 error kind。
            let _ = shared.with_disconnect_admission(|| bot.disconnect());
        }
        Event::AddPlayer(info) => {
            if !shared.client_is_current_owner(&bot) {
                return;
            }
            shared.emit_if_running(
                FactSource::ServerObserved,
                BackendEventPayload::PlayerList(ContractProtocolPlayerListEvent::Add {
                    uuid: info.uuid.to_string(),
                    username: info.profile.name,
                }),
            );
        }
        Event::RemovePlayer(info) => {
            if !shared.client_is_current_owner(&bot) {
                return;
            }
            shared.emit_if_running(
                FactSource::ServerObserved,
                BackendEventPayload::PlayerList(ContractProtocolPlayerListEvent::Remove {
                    uuid: info.uuid.to_string(),
                    username: info.profile.name,
                }),
            );
        }
        Event::UpdatePlayer(info) => {
            if !shared.client_is_current_owner(&bot) {
                return;
            }
            shared.emit_if_running(
                FactSource::ServerObserved,
                BackendEventPayload::PlayerList(ContractProtocolPlayerListEvent::Update {
                    uuid: info.uuid.to_string(),
                    username: info.profile.name,
                }),
            );
        }
        Event::ReceiveChunk(position) => {
            // ChunkLoaded is produced once from the completed canonical
            // ReceiveChunkEvent boundary.  The high-level Event::ReceiveChunk
            // is retained for Azalea's other high-level behavior but must not
            // produce a second observation envelope.
            let _ = position;
        }
        Event::Tick => {
            if shared.client_is_current_owner(&bot)
                && shared.command_execution_allowed()
                && shared.ready.load(Ordering::Acquire)
            {
                let tick = shared.tick_revision.fetch_add(1, Ordering::AcqRel);
                if tick % 5 != 0 {
                    return;
                }
                if let Some(snapshot) =
                    shared.refresh_snapshot(&bot, false, FactSource::ClientPredicted)
                {
                    // Tick 中的 Position/Physics 可能是 Azalea 本地物理预测；
                    // 不把它作为服务端事实发出，服务端事件仍单独保留为 observed。
                    shared.emit_snapshot(snapshot, FactSource::ClientPredicted);
                }
            }
        }
        _ => {}
    }
}

async fn handle_swarm(swarm: Swarm, event: SwarmEvent, state: SwarmState) {
    let shared = state.shared;
    if matches!(event, SwarmEvent::Init) {
        if !shared.set_swarm(swarm.clone()) {
            return;
        }
    }
    if let SwarmEvent::Disconnect(account, join_opts, attempt_token) = event {
        if !shared.claim_reconnect_with_token(attempt_token) {
            return;
        }
        if shared.stopping.load(Ordering::Acquire) {
            shared.reconnect_pending.store(false, Ordering::Release);
            return;
        }

        // SwarmEvent::Disconnect 是重连状态机的兜底边界：azalea 在复用
        // LocalPlayerEvents 时可能没有再发出 Event::Disconnect。
        let close = shared.mark_disconnected(None);
        if shared.stopping.load(Ordering::Acquire) || close.deliberate {
            shared.reconnect_pending.store(false, Ordering::Release);
            return;
        }
        if !close.retryable || !shared.config.reconnect.enabled {
            shared.emit_faulted(shared.failure_for_close(&close));
            shared.request_shutdown();
            shared.reconnect_pending.store(false, Ordering::Release);
            return;
        }
        let Some(delay) = shared.emit_reconnect_scheduled(&close) else {
            shared.finish_reconnect_attempt(0);
            return;
        };
        let reconnect_cancel = shared.reconnect_cancel.clone();
        tokio::task::spawn_local(async move {
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = reconnect_cancel.notified() => {
                    shared.finish_reconnect_attempt(0);
                    return;
                }
            }
            let Some(token) = shared.admit_reconnect_attempt() else {
                shared.finish_reconnect_attempt(0);
                return;
            };
            if !shared.reconnect_add_is_allowed(token) {
                shared.finish_reconnect_attempt(token);
                return;
            }
            let state = BotState {
                shared: shared.clone(),
            };
            // Do not drop add_with_opts after its first poll: it may already
            // have started Client::start_client. Stop invalidates the token
            // and exits the swarm; once this future returns, an invalid token
            // explicitly disconnects the returned client as the final guard.
            let client = swarm.add_with_opts(&account, state, &join_opts).await;
            // Bind while the reservation/token/stop admission is held by the
            // backend.  Do not perform a separate read-then-bind check: a
            // stop or token transition between those reads must invalidate
            // the returned client instead of installing its owner.
            let bound = shared
                .bind_reconnect_return_with_token(token, client.entity, client.attempt_token())
                .is_some();
            if !bound {
                client.disconnect();
            } else {
                let _ = shared.set_active_client_if_current(&client);
            }
            shared.finish_reconnect_attempt(token);
        });
    }
}

/// 启动 M1 连接/登录事件流，并在真实断线后按自有状态机再次加入。
pub async fn run(config: RunConfig) -> Result<(), Box<dyn Error + Send + Sync>> {
    let handle = RuntimeHandle::new(config.clone());
    run_with_handle(handle, config).await
}

/// 使用外部句柄启动运行时，供主仓库适配层调用 `snapshot/subscribe/motor/sendChat`。
pub async fn run_with_handle(
    handle: RuntimeHandle,
    config: RunConfig,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    validate_run_config(&config)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let shared = handle.shared.clone();
    shared.timers_enabled.store(true, Ordering::Release);
    if !shared.begin_connection_attempt() {
        return Ok(());
    }
    let account = Account::offline(&config.username);
    let socket: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    let address = ResolvedAddr {
        server: ServerAddr::from(socket),
        socket,
    };
    let shutdown = shared.shutdown.clone();
    let bot_state = BotState {
        shared: shared.clone(),
    };
    let swarm_state = SwarmState { shared };
    let plugins = (
        DefaultPlugins.build(),
        DefaultBotPlugins
            .build()
            .disable::<AutoRespawnPlugin>()
            .disable::<AcceptResourcePacksPlugin>()
            .disable::<AutoReconnectPlugin>(),
        EntityProducerPlugin,
        BlockSoundProducerPlugin,
        ServerPositionCorrectionPlugin,
        RuntimeShutdownPlugin,
        DefaultSwarmPlugins,
    );
    let start = SwarmBuilder::new_without_plugins()
        .add_plugins(plugins)
        .set_handler(handle_client)
        .set_swarm_handler(handle_swarm)
        .set_swarm_state(swarm_state)
        .add_account_with_state(account, bot_state)
        .reconnect_after(None)
        .start(&address);
    tokio::select! {
        _ = start => {}
        _ = shutdown.notified() => {
            // 先让 SwarmBuilder 自己尝试清理；若其内部仍在等待 AppExit，
            // 丢弃 start future 后由 Tokio runtime 回收剩余任务。
        }
    }
    Ok(())
}

fn validate_run_config(config: &RunConfig) -> Result<(), String> {
    if config.host.trim().is_empty() {
        return Err("服务器 host 不能为空".to_owned());
    }
    if config.port == 0 {
        return Err("服务器 port 不能为 0".to_owned());
    }
    if config.username.is_empty()
        || config.username.len() > 16
        || !config
            .username
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err("offline 用户名必须是 1–16 个 ASCII 字母、数字或下划线".to_owned());
    }
    if config.world_id.trim().is_empty() {
        return Err("world_id 不能为空".to_owned());
    }
    if config.timeouts.connect_ms == 0
        || config.timeouts.login_ms == 0
        || config.timeouts.spawn_ms == 0
        || config.timeouts.stop_ms == 0
    {
        return Err("transport timeout 必须大于 0".to_owned());
    }
    if !config.reconnect.multiplier.is_finite() || config.reconnect.multiplier < 1.0 {
        return Err("reconnect multiplier 必须是有限且不小于 1 的数".to_owned());
    }
    if !config.reconnect.jitter_ratio.is_finite()
        || !(0.0..=1.0).contains(&config.reconnect.jitter_ratio)
    {
        return Err("reconnect jitter ratio 必须在 0 到 1 之间".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        future::pending,
        sync::{atomic::AtomicUsize, mpsc as std_mpsc, Barrier, Condvar, Mutex as StdMutex},
        thread,
        time::Duration as StdDuration,
    };

    use super::*;
    use crate::snapshot::{ExperienceSnapshot, InventorySnapshot, SelfSnapshot, WorldSnapshot};
    use mineintent_contracts::minecraft::{
        BackendEventProtocol as ContractBackendEventProtocol,
        BlockPropertyValue as ContractBlockPropertyValue, CancellationSignal, Deadline,
        HeardSoundType as ContractHeardSoundType,
        ProtocolEntityEvent as ContractProtocolEntityEvent,
        ProtocolSoundPayload as ContractProtocolSoundPayload,
        ProtocolSoundSource as ContractProtocolSoundSource,
        ProtocolWorldEvent as ContractProtocolWorldEvent,
    };

    struct TestCancellation {
        checks: AtomicUsize,
        trigger_at: Option<usize>,
        cancel_on_trigger: bool,
        cancelled: AtomicBool,
        triggered: AtomicBool,
        action: Option<Arc<dyn Fn() + Send + Sync>>,
    }

    impl TestCancellation {
        fn new(
            initially_cancelled: bool,
            trigger_at: Option<usize>,
            cancel_on_trigger: bool,
            action: Option<Arc<dyn Fn() + Send + Sync>>,
        ) -> Arc<Self> {
            Arc::new(Self {
                checks: AtomicUsize::new(0),
                trigger_at,
                cancel_on_trigger,
                cancelled: AtomicBool::new(initially_cancelled),
                triggered: AtomicBool::new(false),
                action,
            })
        }
    }

    impl CancellationSignal for TestCancellation {
        fn is_cancelled(&self) -> bool {
            let check = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
            if self.trigger_at == Some(check) && !self.triggered.swap(true, Ordering::SeqCst) {
                if let Some(action) = &self.action {
                    action();
                }
                if self.cancel_on_trigger {
                    self.cancelled.store(true, Ordering::SeqCst);
                }
            }
            self.cancelled.load(Ordering::SeqCst)
        }

        fn cancelled(&self) -> BoxFuture<'_, ()> {
            Box::pin(pending())
        }
    }

    struct TestDeadline {
        checks: AtomicUsize,
        trigger_at: Option<usize>,
        elapsed: AtomicBool,
        triggered: AtomicBool,
        action: Option<Arc<dyn Fn() + Send + Sync>>,
    }

    impl TestDeadline {
        fn new(
            initially_elapsed: bool,
            trigger_at: Option<usize>,
            action: Option<Arc<dyn Fn() + Send + Sync>>,
        ) -> Arc<Self> {
            Arc::new(Self {
                checks: AtomicUsize::new(0),
                trigger_at,
                elapsed: AtomicBool::new(initially_elapsed),
                triggered: AtomicBool::new(false),
                action,
            })
        }
    }

    impl Deadline for TestDeadline {
        fn has_elapsed(&self) -> bool {
            let check = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
            if self.trigger_at == Some(check) && !self.triggered.swap(true, Ordering::SeqCst) {
                if let Some(action) = &self.action {
                    action();
                }
                self.elapsed.store(true, Ordering::SeqCst);
            }
            self.elapsed.load(Ordering::SeqCst)
        }

        fn elapsed(&self) -> BoxFuture<'_, ()> {
            Box::pin(pending())
        }
    }

    struct WorkerWakeCancellation {
        checks: AtomicUsize,
        started: Notify,
        worker_started: AtomicBool,
        cancelled: AtomicBool,
        wake: Arc<(StdMutex<()>, Condvar)>,
    }

    impl WorkerWakeCancellation {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                checks: AtomicUsize::new(0),
                started: Notify::new(),
                worker_started: AtomicBool::new(false),
                cancelled: AtomicBool::new(false),
                wake: Arc::new((StdMutex::new(()), Condvar::new())),
            })
        }
    }

    impl CancellationSignal for WorkerWakeCancellation {
        fn is_cancelled(&self) -> bool {
            let check = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
            if check == 4 {
                self.worker_started.store(true, Ordering::SeqCst);
                self.started.notify_one();
                let (lock, wake) = &*self.wake;
                let guard = lock.lock().expect("worker wake lock");
                if !self.cancelled.load(Ordering::SeqCst) {
                    let _ = wake
                        .wait_timeout(guard, StdDuration::from_millis(100))
                        .expect("worker wake wait");
                }
            }
            self.cancelled.load(Ordering::SeqCst)
        }

        fn cancelled(&self) -> BoxFuture<'_, ()> {
            Box::pin(async move {
                self.started.notified().await;
                self.cancelled.store(true, Ordering::SeqCst);
                self.wake.1.notify_all();
            })
        }
    }

    struct WorkerWakeDeadline {
        checks: AtomicUsize,
        started: Notify,
        worker_started: AtomicBool,
        elapsed: AtomicBool,
        wake: Arc<(StdMutex<()>, Condvar)>,
    }

    impl WorkerWakeDeadline {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                checks: AtomicUsize::new(0),
                started: Notify::new(),
                worker_started: AtomicBool::new(false),
                elapsed: AtomicBool::new(false),
                wake: Arc::new((StdMutex::new(()), Condvar::new())),
            })
        }
    }

    impl Deadline for WorkerWakeDeadline {
        fn has_elapsed(&self) -> bool {
            let check = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
            if check == 4 {
                self.worker_started.store(true, Ordering::SeqCst);
                self.started.notify_one();
                let (lock, wake) = &*self.wake;
                let guard = lock.lock().expect("worker wake lock");
                if !self.elapsed.load(Ordering::SeqCst) {
                    let _ = wake
                        .wait_timeout(guard, StdDuration::from_millis(100))
                        .expect("worker wake wait");
                }
            }
            self.elapsed.load(Ordering::SeqCst)
        }

        fn elapsed(&self) -> BoxFuture<'_, ()> {
            Box::pin(async move {
                self.started.notified().await;
                self.elapsed.store(true, Ordering::SeqCst);
                self.wake.1.notify_all();
            })
        }
    }

    fn test_control(
        cancellation: Arc<TestCancellation>,
        deadline: Option<Arc<TestDeadline>>,
    ) -> OperationControl {
        OperationControl::new(
            cancellation,
            deadline.map(|value| value as Arc<dyn Deadline>),
        )
    }

    fn empty_world() -> SharedWorld {
        Arc::new(parking_lot::RwLock::new(azalea::world::World::default()))
    }

    fn install_dimension_registry(
        shared_world: &SharedWorld,
        dimension: &str,
        has_skylight: Option<bool>,
    ) {
        let mut values = vec![
            (
                "height".into(),
                azalea::protocol::simdnbt::owned::NbtTag::Int(384),
            ),
            (
                "min_y".into(),
                azalea::protocol::simdnbt::owned::NbtTag::Int(-64),
            ),
        ];
        if let Some(has_skylight) = has_skylight {
            values.push((
                "has_skylight".into(),
                azalea::protocol::simdnbt::owned::NbtTag::Byte(i8::from(has_skylight)),
            ));
        }
        let entry = azalea::protocol::simdnbt::owned::NbtCompound::from_values(values);
        shared_world.write().registries.append(
            azalea::Identifier::from("minecraft:dimension_type"),
            vec![(azalea::Identifier::from(dimension), Some(entry))],
        );
    }

    fn install_viewport_observation(
        handle: &RuntimeHandle,
        snapshot: MinecraftSnapshotV1,
        source: FactSource,
        entities: Vec<ProtocolEntitySnapshot>,
        world: SharedWorld,
    ) {
        let scope_generation = handle.shared.entity_producer.lock().scope_generation;
        let mut observation = handle.shared.observation.write();
        observation.world = Some(world);
        observation.snapshot = Some(snapshot);
        observation.snapshot_scope_generation = scope_generation;
        observation.source = Some(source);
        observation.tracked_entities = entities;
        observation.bump_generation();
        handle.shared.ready.store(true, Ordering::Release);
    }

    fn ready_viewport_source() -> (RuntimeHandle, RuntimeObservationSource, SharedWorld) {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let world = empty_world();
        install_viewport_observation(
            &handle,
            observation_snapshot(1),
            FactSource::ServerObserved,
            vec![observation_entity("entity-7")],
            world.clone(),
        );
        let source = handle.observation_source();
        (handle, source, world)
    }

    #[test]
    fn command_validation_matches_motor_boundary() {
        assert!(validate_command(&BackendCommand::Move {
            directions: vec![MotorDirection::Forward, MotorDirection::Left],
            duration_ms: 1_500,
            sprint: Some(true),
            jump: Some(false),
            crouch: Some(false),
        })
        .is_ok());
        assert!(validate_command(&BackendCommand::Move {
            directions: vec![MotorDirection::Forward, MotorDirection::Forward],
            duration_ms: 100,
            sprint: None,
            jump: None,
            crouch: None,
        })
        .is_err());
        assert!(validate_command(&BackendCommand::Move {
            directions: vec![MotorDirection::Forward],
            duration_ms: 49,
            sprint: None,
            jump: None,
            crouch: None,
        })
        .is_err());
        assert!(validate_command(&BackendCommand::SendChat {
            message: "hello\nworld".to_owned(),
        })
        .is_err());
    }

    #[test]
    fn relative_look_validation_rejects_non_finite_angles() {
        assert!(validate_command(&BackendCommand::LookRelative {
            yaw_degrees: 90.0,
            pitch_degrees: -90.0,
        })
        .is_ok());
        assert!(validate_command(&BackendCommand::LookRelative {
            yaw_degrees: 90.1,
            pitch_degrees: 0.0,
        })
        .is_err());
        assert!(validate_command(&BackendCommand::LookRelative {
            yaw_degrees: f32::NAN,
            pitch_degrees: 0.0,
        })
        .is_err());
    }

    #[test]
    fn run_config_rejects_invalid_offline_username() {
        let mut config = RunConfig::default();
        config.username = "MineIntentUsernameTooLong".to_owned();
        assert!(validate_run_config(&config).is_err());
        config.username = "bad-name".to_owned();
        assert!(validate_run_config(&config).is_err());
        config.username = "MineM130Fresh".to_owned();
        assert!(validate_run_config(&config).is_ok());
    }

    #[tokio::test]
    async fn command_completion_seam_reports_fifo_success_failure_and_shutdown_cancel() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let first = handle
            .look_relative(10.0, -5.0)
            .expect("look command should enqueue");
        let second = handle
            .move_input(vec![MotorDirection::Forward], 100, None, None, None)
            .expect("move command should enqueue");
        let third = handle
            .release_all()
            .expect("release command should enqueue");
        assert_eq!(first.command_id(), "command-1");
        assert_eq!(second.command_id(), "command-2");
        assert_eq!(third.command_id(), "command-3");

        let first_queued = handle.shared.pop_command().expect("first queue item");
        let second_queued = handle.shared.pop_command().expect("second queue item");
        let third_queued = handle.shared.pop_command().expect("third queue item");
        assert_eq!(first_queued.envelope.id, "command-1");
        assert_eq!(second_queued.envelope.id, "command-2");
        assert_eq!(third_queued.envelope.id, "command-3");

        finish_command(&first_queued.completion, Ok(()));
        finish_command(
            &second_queued.completion,
            Err(BackendError::BackendFailure {
                failure: BackendFailure {
                    code: BackendFailureCode::ProtocolError,
                    message: "synthetic movement failure".to_owned(),
                    retryable: true,
                },
            }),
        );
        handle.shared.requeue_front(third_queued);
        handle.shared.cancel_pending_commands();

        assert_eq!(first.wait().await, Ok(()));
        assert!(matches!(
            second.wait().await,
            Err(BackendError::BackendFailure {
                failure: BackendFailure {
                    code: BackendFailureCode::ProtocolError,
                    ..
                }
            })
        ));
        assert!(matches!(
            third.wait().await,
            Err(BackendError::Cancelled { .. })
        ));
    }

    #[tokio::test]
    async fn runtime_stop_admission_linearizes_enqueue_and_settles_completion() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let producer_handle = handle.clone();
        let (result_tx, result_rx) = std_mpsc::channel();
        let producer = thread::spawn(move || {
            let result = producer_handle.look_relative(1.0, 2.0);
            result_tx
                .send(result)
                .expect("command producer result should be delivered");
        });

        for _ in 0..100_000 {
            if !handle.shared.commands.lock().is_empty() {
                break;
            }
            thread::yield_now();
        }
        assert!(
            !handle.shared.commands.lock().is_empty(),
            "producer must enqueue before stop wins the admission lock"
        );
        handle.stop("enqueue_race_stop");
        producer.join().expect("command producer should not panic");

        let completion = result_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("completion enqueue result");
        let completion = completion.expect("enqueue that won must return a completion");
        assert!(matches!(
            completion.wait().await,
            Err(BackendError::Cancelled { .. })
        ));
        assert!(handle.shared.commands.lock().is_empty());

        let stopped_first = RuntimeHandle::new(RunConfig::default());
        stopped_first.stop("already_stopped");
        assert!(stopped_first.send_chat("not queued").is_err());
        assert!(stopped_first.look_relative(0.0, 0.0).is_err());
        assert!(stopped_first.shared.commands.lock().is_empty());
    }

    #[test]
    fn global_stream_excludes_keep_alive_motor_error_and_commanded_chat() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();

        handle
            .send_chat("commanded chat stays local")
            .expect("commanded chat should enqueue locally");
        let _look = handle
            .look_relative(10.0, 5.0)
            .expect("look completion should enqueue locally");
        let _move = handle
            .move_input(vec![MotorDirection::Forward], 100, None, None, None)
            .expect("move completion should enqueue locally");
        let _release = handle
            .release_all()
            .expect("release completion should enqueue locally");

        assert!(events.try_recv().is_err());
        assert_eq!(handle.shared.commands.lock().len(), 4);
        assert_eq!(
            mineintent_contracts::minecraft::BackendEventKind::PRODUCT_KINDS.len(),
            9
        );
    }

    #[tokio::test]
    async fn runtime_stop_during_move_registration_cancels_before_any_actuator() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let (completion, state) = CommandCompletion::channel("command-registration".to_owned());
        let stop_handle = handle.clone();
        handle
            .shared
            .set_active_movement_registration_hook(Some(Arc::new(move || {
                stop_handle.stop("registration_stop")
            })));

        let generation = handle
            .shared
            .movement_generation
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        let registration = handle.shared.register_active_movement(
            "command-registration",
            generation,
            250,
            &Some(state.clone()),
        );

        assert!(matches!(
            registration,
            ActiveMovementRegistration::Cancelled
        ));
        assert!(!handle.shared.active_movement.load(Ordering::Acquire));
        assert!(handle.shared.active_movement_id.lock().is_none());
        assert!(handle.shared.active_movement_cancel_signal.lock().is_none());
        assert!(handle.shared.active_movement_completion.lock().is_none());
        assert!(!handle
            .shared
            .active_movement_registration
            .load(Ordering::Acquire));

        let closed = events.try_recv().expect("close precedes stopped");
        assert_eq!(payload_json(&closed)["type"], "connection_closed");
        let stopped = events
            .try_recv()
            .expect("stopped waits for registration cleanup");
        assert_eq!(payload_json(&stopped)["type"], "stopped");
        assert_eq!(payload_json(&stopped)["reason"], "registration_stop");
        assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
        assert!(matches!(
            completion.wait().await,
            Err(BackendError::Cancelled { .. })
        ));
    }

    #[test]
    fn runtime_stop_during_move_registration_without_completion_cleans_signal() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let stop_handle = handle.clone();
        handle
            .shared
            .set_active_movement_registration_hook(Some(Arc::new(move || {
                stop_handle.stop("registration_without_completion")
            })));
        let generation = handle
            .shared
            .movement_generation
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        let registration = handle.shared.register_active_movement(
            "command-registration-no-completion",
            generation,
            250,
            &None,
        );

        assert!(matches!(
            registration,
            ActiveMovementRegistration::Cancelled
        ));
        assert!(!handle.shared.active_movement.load(Ordering::Acquire));
        assert!(handle.shared.active_movement_id.lock().is_none());
        assert!(handle.shared.active_movement_cancel_signal.lock().is_none());
        assert!(handle.shared.active_movement_completion.lock().is_none());
        assert_eq!(
            payload_json(&events.try_recv().expect("close event"))["type"],
            "connection_closed"
        );
        assert_eq!(
            payload_json(&events.try_recv().expect("stopped event"))["type"],
            "stopped"
        );
        assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn active_move_completion_cancel_notifies_release_and_finishes_once() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let (completion, state) = CommandCompletion::channel("command-active".to_owned());
        let signal = Arc::new(Notify::new());
        state.begin_active_release(signal.clone());
        *handle.shared.active_movement_completion.lock() = Some(state.clone());
        handle.shared.active_movement.store(true, Ordering::Release);
        *handle.shared.active_movement_id.lock() = Some("command-active".to_owned());

        let release_attempted = Arc::new(AtomicBool::new(false));
        let release_saw_active = Arc::new(AtomicBool::new(false));
        let release_saw_pending_completion = Arc::new(AtomicBool::new(false));
        let release_attempted_by_task = release_attempted.clone();
        let release_saw_active_by_task = release_saw_active.clone();
        let release_saw_pending_by_task = release_saw_pending_completion.clone();
        let task_shared = handle.shared.clone();
        let task_state = state.clone();
        let release_waiter = tokio::spawn(async move {
            signal.notified().await;
            let completion_for_task = Some(task_state.clone());
            release_active_movement_and_finish(
                &task_shared,
                "command-active",
                0,
                &completion_for_task,
                || {
                    release_attempted_by_task.store(true, Ordering::Release);
                    release_saw_active_by_task.store(
                        task_shared.active_movement.load(Ordering::Acquire)
                            && task_shared.active_movement_id.lock().as_deref()
                                == Some("command-active"),
                        Ordering::Release,
                    );
                    release_saw_pending_by_task
                        .store(task_state.sender.lock().is_some(), Ordering::Release);
                    true
                },
                "cancel move",
                Err(BackendError::Cancelled {
                    operation: "command:command-active".to_owned(),
                }),
            );
        });

        completion.cancel();
        completion.cancel();
        assert!(
            state.sender.lock().is_some(),
            "active cancellation must defer completion until release"
        );
        tokio::time::timeout(StdDuration::from_secs(1), release_waiter)
            .await
            .expect("active movement cancellation must run the release seam")
            .expect("release waiter should not panic");

        assert!(release_attempted.load(Ordering::Acquire));
        assert!(release_saw_active.load(Ordering::Acquire));
        assert!(release_saw_pending_completion.load(Ordering::Acquire));
        assert!(!handle.shared.active_movement.load(Ordering::Acquire));
        assert!(handle.shared.active_movement_id.lock().is_none());
        assert!(state.sender.lock().is_none());
        assert!(matches!(
            completion.wait().await,
            Err(BackendError::Cancelled { .. })
        ));
    }

    #[test]
    fn active_move_cancellation_wins_first_actuator_admission() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let (completion, state) = CommandCompletion::channel("command-first-actuator".to_owned());
        let generation = handle
            .shared
            .movement_generation
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        let registration = handle.shared.register_active_movement(
            "command-first-actuator",
            generation,
            250,
            &Some(state.clone()),
        );
        assert!(matches!(
            registration,
            ActiveMovementRegistration::Started { .. }
        ));

        completion.cancel();
        let actuator_called = Arc::new(AtomicBool::new(false));
        let called = actuator_called.clone();
        let result = handle.shared.with_active_movement_admission(
            "command-first-actuator",
            generation,
            &Some(state.clone()),
            || {
                called.store(true, Ordering::Release);
            },
        );
        assert!(result.is_err());
        assert!(!actuator_called.load(Ordering::Acquire));
        let cancel_signal = handle.shared.active_movement_cancel_signal.lock().clone();
        handle.shared.cancel_registered_active_movement(
            "command-first-actuator",
            generation,
            &cancel_signal,
            &Some(state.clone()),
        );
        assert!(state.sender.lock().is_none());
    }

    #[test]
    fn not_ready_command_head_cannot_be_overtaken_by_a_later_enqueue() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let first = handle
            .look_relative(1.0, 2.0)
            .expect("first command should enqueue");
        let barrier = Arc::new(Barrier::new(2));
        let producer_started = Arc::new(AtomicBool::new(false));
        let producer_done = Arc::new(AtomicBool::new(false));
        let (producer_id_tx, producer_id_rx) = std_mpsc::channel();
        let producer_handle = handle.clone();
        let producer_barrier = barrier.clone();
        let producer_started_flag = producer_started.clone();
        let producer_done_flag = producer_done.clone();
        let producer = thread::spawn(move || {
            producer_barrier.wait();
            producer_started_flag.store(true, Ordering::Release);
            let second = producer_handle
                .release_all()
                .expect("concurrent later command should enqueue");
            producer_id_tx
                .send(second.command_id().to_owned())
                .expect("producer id should be delivered");
            producer_done_flag.store(true, Ordering::Release);
        });

        assert!(handle
            .shared
            .next_command_for_processing_with_hook(|| {
                barrier.wait();
                while !producer_started.load(Ordering::Acquire) {
                    thread::yield_now();
                }
                assert!(
                    !producer_done.load(Ordering::Acquire),
                    "producer must still be blocked by the queue lock"
                );
            })
            .is_none());
        producer
            .join()
            .expect("concurrent producer should not panic");
        let second_id = producer_id_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("later command id");

        let first_queued = handle.shared.pop_command().expect("deferred head");
        let second_queued = handle.shared.pop_command().expect("later queue item");
        assert_eq!(first_queued.envelope.id, first.command_id());
        assert_eq!(second_queued.envelope.id, second_id);
        handle.shared.requeue_front(second_queued);
        handle.shared.requeue_front(first_queued);
        handle.shared.cancel_pending_commands();
    }

    fn payload_json(event: &BackendEventEnvelope) -> serde_json::Value {
        serde_json::to_value(&event.payload).expect("strict v2 payload is serializable")
    }

    #[test]
    fn connection_request_preallocates_and_init_reuses_each_attempt_identity() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut owner_world = bevy_ecs::world::World::new();
        let first_owner = owner_world.spawn_empty().id();
        let second_owner = owner_world.spawn_empty().id();
        assert!(events.try_recv().is_err());

        handle.shared.begin_connection_attempt();
        let first_request = events.try_recv().expect("首次连接请求事件");
        assert_eq!(first_request.connection_epoch, 1);
        assert_eq!(first_request.connection_attempt_id, "attempt-1");
        let first_payload = payload_json(&first_request);
        assert_eq!(first_payload["type"], "connection_requested");
        assert_eq!(first_payload["attempt"], 1);
        assert!(first_request.dimension.is_none());

        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(first_owner),
            Some(1)
        );
        let first_init = events.try_recv().expect("首次连接初始化事件");
        assert_eq!(
            first_init.process_session_id,
            first_request.process_session_id
        );
        assert_eq!(first_init.connection_epoch, 1);
        assert_eq!(first_init.connection_attempt_id, "attempt-1");

        handle.shared.begin_connection_attempt();
        let second_request = events.try_recv().expect("重连请求事件");
        assert_eq!(
            second_request.process_session_id,
            first_request.process_session_id
        );
        assert_eq!(second_request.connection_epoch, 2);
        assert_eq!(second_request.connection_attempt_id, "attempt-2");
        let second_payload = payload_json(&second_request);
        assert_eq!(second_payload["type"], "connection_requested");
        assert_eq!(second_payload["attempt"], 2);
        assert!(second_request.dimension.is_none());

        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(second_owner),
            Some(2)
        );
        let _second_transport = events.try_recv().expect("second transport connected");
        assert_eq!(handle.shared.context().1, 2);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn event_dimension_is_captured_at_emit_time_and_cleared_for_a_new_attempt() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();

        handle.shared.begin_connection_attempt();
        let first_request = events.try_recv().expect("首次连接请求事件");
        assert!(first_request.dimension.is_none());

        assert_eq!(handle.shared.set_dimension("minecraft:overworld"), None);
        handle.shared.emit(
            FactSource::ServerObserved,
            BackendEventPayload::World(ContractProtocolWorldEvent::GameChanged {
                dimension: Some("minecraft:overworld".to_owned()),
                game_mode: Some("survival".to_owned()),
            }),
        );
        let world_event = events.try_recv().expect("世界事件");
        assert_eq!(
            world_event.dimension.as_deref(),
            Some("minecraft:overworld")
        );

        handle.shared.begin_connection_attempt();
        let second_request = events.try_recv().expect("重连请求事件");
        assert_eq!(second_request.connection_epoch, 2);
        assert!(second_request.dimension.is_none());
    }

    #[test]
    fn dimension_changed_event_carries_the_new_dimension() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("连接请求事件");

        handle.shared.observe_dimension("minecraft:overworld");
        assert!(events.try_recv().is_err());
        handle.shared.observe_dimension("minecraft:the_nether");

        let changed = events.try_recv().expect("维度变化事件");
        let changed_payload = payload_json(&changed);
        assert_eq!(changed_payload["type"], "dimension_changed");
        assert_eq!(changed_payload["from"], "minecraft:overworld");
        assert_eq!(changed_payload["to"], "minecraft:the_nether");
        assert_eq!(changed.dimension.as_deref(), Some("minecraft:the_nether"));
    }

    #[test]
    fn world_loaded_dimension_boundary_is_idempotent_for_the_bound_owner() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut world = bevy_ecs::world::World::new();
        let owner = world.spawn_empty().id();
        assert!(handle.shared.begin_connection_attempt());
        let _request = events.try_recv().expect("request");
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(owner),
            Some(1)
        );
        let _transport = events.try_recv().expect("transport");

        assert!(handle
            .shared
            .observe_dimension_from_world_boundary(owner, "minecraft:overworld"));
        assert!(events.try_recv().is_err());
        assert!(handle
            .shared
            .observe_dimension_from_world_boundary(owner, "minecraft:overworld"));
        assert!(events.try_recv().is_err());
        assert!(handle
            .shared
            .observe_dimension_from_world_boundary(owner, "minecraft:the_nether"));
        let changed = events.try_recv().expect("one dimension change");
        assert_eq!(payload_json(&changed)["type"], "dimension_changed");
        assert!(handle
            .shared
            .observe_dimension_from_world_boundary(owner, "minecraft:the_nether"));
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn runtime_ready_death_respawn_and_dimension_lifecycle_order_is_typed() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        handle.shared.emit_transport_connected();
        handle
            .shared
            .emit_logged_in("26.1.2", "minecraft:overworld".to_owned());
        handle.shared.emit_ready(7);
        handle.shared.emit_died();
        handle
            .shared
            .emit_respawn_transition_started("minecraft:overworld".to_owned());
        handle
            .shared
            .emit_respawned("minecraft:overworld".to_owned());
        handle.shared.observe_dimension("minecraft:the_nether");

        let emitted = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert_eq!(
            emitted
                .iter()
                .map(|event| payload_json(event)["type"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>(),
            vec![
                "transport_connected".to_owned(),
                "logged_in".to_owned(),
                "ready".to_owned(),
                "died".to_owned(),
                "respawn_transition_started".to_owned(),
                "respawned".to_owned(),
                "dimension_changed".to_owned(),
            ]
        );
        assert_eq!(payload_json(&emitted[1])["version"], "26.1.2");
        assert_eq!(
            payload_json(&emitted[1])["dimension"],
            "minecraft:overworld"
        );
        assert_eq!(payload_json(&emitted[2])["snapshotRevision"], 7);
        assert_eq!(
            payload_json(&emitted[4])["fromDimension"],
            "minecraft:overworld"
        );
        assert_eq!(
            payload_json(&emitted[5])["dimension"],
            "minecraft:overworld"
        );
        assert_eq!(payload_json(&emitted[6])["from"], "minecraft:overworld");
        assert_eq!(payload_json(&emitted[6])["to"], "minecraft:the_nether");
        assert_eq!(emitted[2].dimension.as_deref(), Some("minecraft:overworld"));
        assert_eq!(
            emitted[6].dimension.as_deref(),
            Some("minecraft:the_nether")
        );
    }

    #[test]
    fn lifecycle_state_timestamps_match_the_strict_v2_lifecycle_facts() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        handle.shared.emit_ready(7);
        let ready_event = events.try_recv().expect("ready event");
        let BackendState::Ready { ready_at, .. } = handle.state() else {
            panic!("ready state should be visible after ready admission");
        };
        assert_eq!(ready_at, ready_event.occurred_at);

        handle.shared.emit_died();
        let died_event = events.try_recv().expect("died event");
        let BackendState::Dead { died_at, .. } = handle.state() else {
            panic!("dead state should be visible after death admission");
        };
        assert_eq!(died_at, died_event.occurred_at);
    }

    #[test]
    fn stopped_runtime_rejects_late_attempt_and_ready_without_resurrection() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut owner_world = bevy_ecs::world::World::new();
        let owner = owner_world.spawn_empty().id();
        handle.stop("late_event_stop");
        let stopped = events.try_recv().expect("stopped event");
        assert_eq!(payload_json(&stopped)["type"], "stopped");
        assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));

        assert!(!handle.shared.begin_connection_attempt());
        assert!(handle
            .shared
            .consume_attempt_for_transport_init_and_bind(owner)
            .is_none());
        handle.shared.emit_transport_connected();
        handle
            .shared
            .emit_logged_in("26.1.2", "minecraft:overworld".to_owned());
        handle.shared.emit_ready(99);
        assert!(!handle.shared.ready.load(Ordering::Acquire));
        assert!(handle.shared.stopped_reported.load(Ordering::Acquire));
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn finalize_stop_ready_protocol_has_no_lost_wakeup_between_finalizers() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.stopping.store(true, Ordering::Release);
        *handle.shared.stop_reason.lock() = Some("lost_wakeup_regression".to_owned());

        let (hook_reached_tx, hook_reached_rx) = std_mpsc::channel();
        let (release_tx, release_rx) = std_mpsc::channel();
        let release_rx = Arc::new(StdMutex::new(Some(release_rx)));
        handle.shared.set_finalize_stop_hook(Some(Arc::new({
            let release_rx = release_rx.clone();
            move || {
                hook_reached_tx
                    .send(())
                    .expect("finalizer hook should be reached");
                release_rx
                    .lock()
                    .expect("release gate lock")
                    .take()
                    .expect("only the first finalizer owns the gate")
                    .recv()
                    .expect("release gate should open");
            }
        })));

        let first_shared = handle.shared.clone();
        let first = thread::spawn(move || first_shared.finalize_stop_if_ready());
        hook_reached_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("first finalizer must reach the readiness gate");

        let (second_attempt_tx, second_attempt_rx) = std_mpsc::channel();
        let (second_done_tx, second_done_rx) = std_mpsc::channel();
        let second_shared = handle.shared.clone();
        let second = thread::spawn(move || {
            second_attempt_tx
                .send(())
                .expect("second finalizer attempt should be observable");
            second_shared.finalize_stop_if_ready();
            second_done_tx
                .send(())
                .expect("second finalizer should finish");
        });
        second_attempt_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("second finalizer should attempt while first owns admission");
        assert!(
            second_done_rx.try_recv().is_err(),
            "second finalizer must wait instead of observing an empty reason"
        );

        release_tx.send(()).expect("release first finalizer");
        first.join().expect("first finalizer should not panic");
        second.join().expect("second finalizer should not panic");

        let stopped = events.try_recv().expect("stopped must not be lost");
        assert_eq!(payload_json(&stopped)["type"], "stopped");
        assert_eq!(payload_json(&stopped)["reason"], "lost_wakeup_regression");
        assert!(events.try_recv().is_err());
        assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
    }

    #[test]
    fn running_event_admission_enqueue_precedes_stop_terminal_event() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let (event_checked_tx, event_checked_rx) = std_mpsc::channel();
        let (release_tx, release_rx) = std_mpsc::channel();
        let release_rx = Arc::new(StdMutex::new(Some(release_rx)));
        handle.shared.set_event_admission_hook(Some(Arc::new({
            let release_rx = release_rx.clone();
            move || {
                event_checked_tx
                    .send(())
                    .expect("event admission hook should be reached");
                release_rx
                    .lock()
                    .expect("event release gate lock")
                    .take()
                    .expect("only one event admission owns the gate")
                    .recv()
                    .expect("event release gate should open");
            }
        })));

        let event_shared = handle.shared.clone();
        let emitter = thread::spawn(move || {
            event_shared.emit_if_running(
                FactSource::ServerObserved,
                BackendEventPayload::World(ContractProtocolWorldEvent::GameChanged {
                    dimension: Some("minecraft:overworld".to_owned()),
                    game_mode: Some("survival".to_owned()),
                }),
            )
        });
        event_checked_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("event must hold admission between check and enqueue");

        let (stop_attempt_tx, stop_attempt_rx) = std_mpsc::channel();
        let stop_handle = handle.clone();
        let stopper = thread::spawn(move || {
            stop_attempt_tx
                .send(())
                .expect("stop attempt should be observable");
            stop_handle.stop("event_admission_stop");
        });
        stop_attempt_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("stop must attempt while event owns admission");

        release_tx.send(()).expect("release event admission");
        assert!(emitter.join().expect("event emitter should not panic"));
        stopper.join().expect("stopper should not panic");

        let kinds = std::iter::from_fn(|| events.try_recv().ok())
            .map(|event| payload_json(&event)["type"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                "game_changed".to_owned(),
                "connection_closed".to_owned(),
                "stopped".to_owned()
            ]
        );
    }

    #[test]
    fn stop_cannot_overtake_first_disconnect_cleanup() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");
        handle.shared.observation.write().world = Some(empty_world());
        handle
            .send_chat("queued before disconnect")
            .expect("pre-disconnect command should queue");

        let (cleanup_reached_tx, cleanup_reached_rx) = std_mpsc::channel();
        let (release_cleanup_tx, release_cleanup_rx) = std_mpsc::channel();
        let release_cleanup_rx = Arc::new(StdMutex::new(Some(release_cleanup_rx)));
        handle.shared.set_disconnect_cleanup_hook(Some(Arc::new({
            let release_cleanup_rx = release_cleanup_rx.clone();
            move || {
                cleanup_reached_tx
                    .send(())
                    .expect("disconnect cleanup hook should be reached");
                release_cleanup_rx
                    .lock()
                    .expect("disconnect cleanup gate lock")
                    .take()
                    .expect("only the first disconnect owns the gate")
                    .recv()
                    .expect("disconnect cleanup gate should open");
            }
        })));

        let disconnect_shared = handle.shared.clone();
        let disconnect = thread::spawn(move || {
            disconnect_shared.mark_disconnected(Some("Server closed".to_owned()))
        });
        cleanup_reached_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("first disconnect must reach cleanup while holding admission");

        let (stop_attempt_tx, stop_attempt_rx) = std_mpsc::channel();
        let (stop_done_tx, stop_done_rx) = std_mpsc::channel();
        let stop_handle = handle.clone();
        let stopper = thread::spawn(move || {
            stop_attempt_tx
                .send(())
                .expect("stop attempt should be observable");
            stop_handle.stop("disconnect_cleanup_race");
            stop_done_tx.send(()).expect("stop completion signal");
        });
        stop_attempt_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("stop must attempt while disconnect owns admission");
        assert!(
            stop_done_rx
                .recv_timeout(StdDuration::from_millis(50))
                .is_err(),
            "stopped must not overtake first-disconnect cleanup"
        );
        assert!(
            events.try_recv().is_err(),
            "close is not visible before cleanup"
        );

        release_cleanup_tx
            .send(())
            .expect("release disconnect cleanup");
        let close = disconnect
            .join()
            .expect("disconnect thread should not panic");
        assert_eq!(close.code, "server_shutdown");
        stopper.join().expect("stop thread should not panic");
        stop_done_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("stop should finish after cleanup");

        let closed = events.try_recv().expect("close event after cleanup");
        let stopped = events.try_recv().expect("stopped event after cleanup");
        assert_eq!(payload_json(&closed)["type"], "connection_closed");
        assert_eq!(payload_json(&stopped)["type"], "stopped");
        assert!(handle.shared.observation.read().world.is_none());
        assert!(handle.shared.commands.lock().is_empty());
        assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn died_broadcast_reentrant_stop_has_no_post_stopped_actuator() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let callback_handle = handle.clone();
        handle
            .shared
            .set_event_broadcast_hook(Some(Arc::new(move || {
                callback_handle.stop("died_callback_stop");
            })));
        let actuator_called = Arc::new(AtomicBool::new(false));
        let actuator_after_stop = Arc::new(AtomicBool::new(false));
        let called = actuator_called.clone();
        let after_stop = actuator_after_stop.clone();
        let result = handle.shared.admit_death_and_release(|| {
            if handle.shared.stopping.load(Ordering::Acquire) {
                after_stop.store(true, Ordering::Release);
            }
            called.store(true, Ordering::Release);
            true
        });

        assert_eq!(result, Some(true));
        assert!(actuator_called.load(Ordering::Acquire));
        assert!(
            !actuator_after_stop.load(Ordering::Acquire),
            "Death actuator must finish before a re-entrant died callback can stop"
        );
        let kinds = std::iter::from_fn(|| events.try_recv().ok())
            .map(|event| payload_json(&event)["type"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                "died".to_owned(),
                "connection_closed".to_owned(),
                "stopped".to_owned()
            ]
        );
    }

    #[test]
    fn stop_wins_before_late_death_claim_without_actuator_or_event() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.stop("late_death_stop");
        let _stopped = events.try_recv().expect("stop event");
        let actuator_called = Arc::new(AtomicBool::new(false));
        let called = actuator_called.clone();
        assert_eq!(
            handle.shared.admit_death_and_release(|| {
                called.store(true, Ordering::Release);
                true
            }),
            None
        );
        assert!(!actuator_called.load(Ordering::Acquire));
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn stop_cancels_admitted_reconnect_add_intent_and_preserves_terminal_state() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle
            .shared
            .reconnect_pending
            .store(true, Ordering::Release);
        let token = handle
            .shared
            .admit_reconnect_attempt()
            .expect("reconnect should be admitted before stop");
        let request = events.try_recv().expect("reconnect connection request");
        assert_eq!(payload_json(&request)["type"], "connection_requested");
        assert!(handle.shared.reconnect_add_is_allowed(token));

        handle.stop("cancel_reconnect_add");
        assert!(!handle.shared.reconnect_add_is_allowed(token));
        assert!(!handle.shared.reconnect_add_pending.load(Ordering::Acquire));
        assert!(!handle.shared.reconnect_pending.load(Ordering::Acquire));
        assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
        assert!(handle.shared.stopped_reported.load(Ordering::Acquire));
        tokio::time::timeout(
            StdDuration::from_secs(1),
            handle.shared.reconnect_cancel.notified(),
        )
        .await
        .expect("stop must wake a pending reconnect add task");

        let event_count_after_stop = std::iter::from_fn(|| events.try_recv().ok()).count();
        assert_eq!(
            event_count_after_stop, 2,
            "close then stopped after admitted attempt"
        );
        assert!(!handle.shared.begin_connection_attempt());
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn stop_wins_before_reconnect_attempt_admission() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.stop("stop_before_reconnect");
        handle
            .shared
            .reconnect_pending
            .store(true, Ordering::Release);
        assert!(handle.shared.admit_reconnect_attempt().is_none());
        assert!(!handle.shared.reconnect_add_pending.load(Ordering::Acquire));
        assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
    }

    #[test]
    fn runtime_retryable_close_then_reconnect_reuses_the_sealed_close_code() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let close = handle
            .shared
            .mark_disconnected(Some("Server restarting".to_owned()));
        assert_eq!(close.code, "server_shutdown");
        assert!(close.retryable);
        assert!(!close.deliberate);
        handle.shared.emit_reconnect_scheduled(&close);

        let closed = events.try_recv().expect("connection closed event");
        let scheduled = events.try_recv().expect("reconnect scheduled event");
        assert_eq!(payload_json(&closed)["type"], "connection_closed");
        assert_eq!(payload_json(&closed)["close"]["code"], close.code);
        assert_eq!(payload_json(&scheduled)["type"], "reconnect_scheduled");
        assert_eq!(payload_json(&scheduled)["closeCode"], close.code);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn runtime_fatal_close_emits_faulted_without_reconnect_disabled_rewrite() {
        let mut config = RunConfig::default();
        config.reconnect.enabled = false;
        let handle = RuntimeHandle::new(config);
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let close = handle
            .shared
            .mark_disconnected(Some("You are banned from this server".to_owned()));
        let failure = handle.shared.failure_for_close(&close);
        assert_eq!(failure.code, BackendFailureCode::PermissionDenied);
        assert!(!failure.retryable);
        handle.shared.emit_faulted(failure);

        let closed = events.try_recv().expect("fatal close event");
        let faulted = events.try_recv().expect("faulted event");
        assert_eq!(payload_json(&closed)["type"], "connection_closed");
        assert_eq!(payload_json(&closed)["close"]["code"], "permission_denied");
        assert_eq!(payload_json(&faulted)["type"], "faulted");
        assert_eq!(
            payload_json(&faulted)["failure"]["code"],
            "permission_denied"
        );
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn runtime_invalid_session_close_uses_permission_denied_with_authentication_failure() {
        let mut config = RunConfig::default();
        config.reconnect.enabled = false;
        let handle = RuntimeHandle::new(config);
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let close = handle
            .shared
            .mark_disconnected(Some("Invalid session".to_owned()));
        assert_eq!(close.code, "permission_denied");
        assert!(!close.retryable);
        assert!(close.kick.is_some());
        let failure = handle.shared.failure_for_close(&close);
        assert_eq!(failure.code, BackendFailureCode::AuthenticationFailed);
        assert!(!failure.retryable);
        handle.shared.emit_faulted(failure);

        let closed = events.try_recv().expect("invalid-session close event");
        let faulted = events.try_recv().expect("invalid-session fault event");
        assert_eq!(payload_json(&closed)["close"]["code"], "permission_denied");
        assert_eq!(
            payload_json(&faulted)["failure"]["code"],
            "authentication_failed"
        );
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn runtime_disconnect_kick_text_does_not_infer_forced_timeout_or_version_codes() {
        let cases = [
            ("server_shutdown", "server_shutdown", true, None),
            (
                "Unsupported version",
                "unclassified_kick",
                false,
                Some(BackendFailureCode::PermissionDenied),
            ),
            (
                "Timed out",
                "unclassified_kick",
                false,
                Some(BackendFailureCode::PermissionDenied),
            ),
        ];
        for (reason, expected_code, retryable, failure_code) in cases {
            let handle = RuntimeHandle::new(RunConfig::default());
            handle.shared.begin_connection_attempt();
            let close = handle.shared.mark_disconnected(Some(reason.to_owned()));
            assert_eq!(close.code, expected_code, "reason={reason}");
            assert_eq!(close.retryable, retryable, "reason={reason}");
            assert_eq!(
                handle.shared.failure_for_close(&close).code,
                failure_code.unwrap_or(BackendFailureCode::ProtocolError),
                "reason={reason}"
            );
        }
    }

    #[test]
    fn runtime_unclassified_disconnect_component_is_fatal_not_reconnectable() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let close = handle
            .shared
            .mark_disconnected(Some("Removed by an administrator".to_owned()));
        assert_eq!(close.code, "unclassified_kick");
        assert!(!close.retryable);
        assert!(close.kick.is_some());
        let failure = handle.shared.failure_for_close(&close);
        assert_eq!(failure.code, BackendFailureCode::PermissionDenied);
        assert!(!failure.retryable);
        assert!(!handle.shared.config.reconnect.enabled || !close.retryable);
        handle.shared.emit_faulted(failure);

        let _closed = events.try_recv().expect("unclassified kick close event");
        let faulted = events.try_recv().expect("unclassified kick fault event");
        assert_eq!(payload_json(&faulted)["type"], "faulted");
        assert!(
            events.try_recv().is_err(),
            "fatal kick must not schedule reconnect"
        );
    }

    #[test]
    fn runtime_connection_failed_retains_error_and_disabled_retry_is_distinct() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");
        let close = handle
            .shared
            .mark_connection_failed("tcp reset by peer".to_owned());
        let closed = events.try_recv().expect("connection failed close event");
        assert_eq!(payload_json(&closed)["close"]["code"], "connection_failed");
        assert_eq!(
            payload_json(&closed)["close"]["error"]["message"],
            "tcp reset by peer"
        );
        assert_eq!(
            handle.shared.failure_for_close(&close).code,
            BackendFailureCode::ProtocolError
        );
    }

    #[test]
    fn runtime_retryable_close_with_disabled_reconnect_emits_reconnect_disabled() {
        let mut config = RunConfig::default();
        config.reconnect.enabled = false;
        let handle = RuntimeHandle::new(config);
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");
        let close = handle
            .shared
            .mark_disconnected(Some("Server closed".to_owned()));
        assert!(close.retryable);
        assert_eq!(
            handle.shared.failure_for_close(&close).code,
            BackendFailureCode::ReconnectDisabled
        );
    }

    #[test]
    fn runtime_expected_stop_closes_then_stops_after_local_cleanup_with_reason() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");
        handle.shared.set_dimension("minecraft:overworld");
        handle.shared.observation.write().world = Some(empty_world());

        handle.stop("operator_requested");
        let closed = events.try_recv().expect("expected close event");
        let stopped = events.try_recv().expect("stopped event");
        assert_eq!(payload_json(&closed)["type"], "connection_closed");
        assert_eq!(payload_json(&closed)["close"]["code"], "deliberate_stop");
        assert_eq!(payload_json(&closed)["close"]["deliberate"], true);
        assert_eq!(payload_json(&stopped)["type"], "stopped");
        assert_eq!(payload_json(&stopped)["reason"], "operator_requested");
        assert!(handle.shared.observation.read().world.is_none());
        assert!(handle.shared.pop_command().is_none());
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn runtime_active_stop_defers_stopped_until_release_seam_finishes() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let (completion, state) = CommandCompletion::channel("command-stop".to_owned());
        state.begin_active_release(Arc::new(Notify::new()));
        *handle.shared.active_movement_completion.lock() = Some(state.clone());
        handle.shared.active_movement.store(true, Ordering::Release);
        *handle.shared.active_movement_id.lock() = Some("command-stop".to_owned());

        handle.stop("operator_requested");
        let closed = events.try_recv().expect("close must precede deferred stop");
        assert_eq!(payload_json(&closed)["type"], "connection_closed");
        assert!(events.try_recv().is_err(), "stopped must wait for release");
        assert!(!handle.shared.shutdown_requested.load(Ordering::Acquire));
        assert!(state.sender.lock().is_some());

        let completion_for_release = Some(state.clone());
        release_active_movement_and_finish(
            &handle.shared,
            "command-stop",
            0,
            &completion_for_release,
            || {
                assert!(handle.shared.active_movement.load(Ordering::Acquire));
                assert!(state.sender.lock().is_some());
                true
            },
            "stop move",
            Err(BackendError::Cancelled {
                operation: "command:command-stop".to_owned(),
            }),
        );
        state.wait_settled().await;
        assert!(matches!(
            completion.wait().await,
            Err(BackendError::Cancelled { .. })
        ));
        assert!(!handle.shared.active_movement.load(Ordering::Acquire));
        assert!(handle.shared.active_movement_id.lock().is_none());
        assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
        let stopped = events.try_recv().expect("stopped after release");
        assert_eq!(payload_json(&stopped)["type"], "stopped");
        assert_eq!(payload_json(&stopped)["reason"], "operator_requested");

        handle.stop("duplicate_reason");
        release_active_movement_and_finish(
            &handle.shared,
            "command-stop",
            0,
            &completion_for_release,
            || panic!("a settled active move must not release twice"),
            "stop move",
            Err(BackendError::Cancelled {
                operation: "command:command-stop".to_owned(),
            }),
        );
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn command_completion_settled_signal_waits_for_release_and_result_publication() {
        for _ in 0..64 {
            let (completion, state) = CommandCompletion::channel("settlement-race".to_owned());
            state.begin_active_release(Arc::new(Notify::new()));
            let barrier = Arc::new(Barrier::new(2));
            let waiter_state = state.clone();
            let observed_state = waiter_state.clone();
            let waiter_barrier = barrier.clone();
            let waiter = thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("settlement waiter runtime");
                runtime.block_on(async move {
                    waiter_barrier.wait();
                    CommandCompletionCancellation(waiter_state.clone())
                        .wait_settled()
                        .await;
                });
                assert!(!observed_state.active_release.load(Ordering::Acquire));
                assert!(observed_state.settled_result.lock().is_some());
            });
            barrier.wait();
            state.finish(Ok(()));
            waiter
                .join()
                .expect("settlement waiter should not race early");
            assert_eq!(completion.wait_blocking(), Ok(()));
        }
    }

    #[tokio::test]
    async fn runtime_active_move_without_completion_stop_wakes_release_before_shutdown() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");

        let signal = Arc::new(Notify::new());
        *handle.shared.active_movement_cancel_signal.lock() = Some(signal.clone());
        handle.shared.active_movement.store(true, Ordering::Release);
        *handle.shared.active_movement_id.lock() = Some("command-no-completion".to_owned());
        let task_shared = handle.shared.clone();
        let released = Arc::new(AtomicBool::new(false));
        let released_by_task = released.clone();
        let release_task = tokio::spawn(async move {
            signal.notified().await;
            let no_completion = None;
            release_active_movement_and_finish(
                &task_shared,
                "command-no-completion",
                0,
                &no_completion,
                || {
                    assert!(task_shared.active_movement.load(Ordering::Acquire));
                    released_by_task.store(true, Ordering::Release);
                    true
                },
                "stop move",
                Err(BackendError::Cancelled {
                    operation: "command:command-no-completion".to_owned(),
                }),
            );
        });

        handle.stop("operator_requested");
        let closed = events.try_recv().expect("close must be emitted");
        assert_eq!(payload_json(&closed)["type"], "connection_closed");
        assert!(
            events.try_recv().is_err(),
            "stopped waits for no-completion release"
        );
        assert!(!handle.shared.shutdown_requested.load(Ordering::Acquire));
        assert!(handle.shared.active_movement.load(Ordering::Acquire));

        tokio::time::timeout(StdDuration::from_secs(1), release_task)
            .await
            .expect("no-completion move cancellation must wake promptly")
            .expect("release task should not panic");
        assert!(released.load(Ordering::Acquire));
        assert!(!handle.shared.active_movement.load(Ordering::Acquire));
        assert!(handle.shared.active_movement_id.lock().is_none());
        assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
        let stopped = events
            .try_recv()
            .expect("stopped after no-completion release");
        assert_eq!(payload_json(&stopped)["reason"], "operator_requested");
    }

    #[tokio::test]
    async fn runtime_active_move_without_completion_disconnect_wakes_release() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request");
        let signal = Arc::new(Notify::new());
        *handle.shared.active_movement_cancel_signal.lock() = Some(signal.clone());
        handle.shared.active_movement.store(true, Ordering::Release);
        *handle.shared.active_movement_id.lock() = Some("command-disconnect".to_owned());
        let task_shared = handle.shared.clone();
        let release_task = tokio::spawn(async move {
            signal.notified().await;
            let no_completion = None;
            release_active_movement_and_finish(
                &task_shared,
                "command-disconnect",
                0,
                &no_completion,
                || true,
                "disconnect move",
                Err(BackendError::Cancelled {
                    operation: "command:command-disconnect".to_owned(),
                }),
            );
        });

        let close = handle
            .shared
            .mark_disconnected(Some("Server closed".to_owned()));
        assert_eq!(close.code, "server_shutdown");
        assert_eq!(
            payload_json(&events.try_recv().expect("close event"))["type"],
            "connection_closed"
        );
        tokio::time::timeout(StdDuration::from_secs(1), release_task)
            .await
            .expect("disconnect must wake no-completion move")
            .expect("release task should not panic");
        assert!(!handle.shared.active_movement.load(Ordering::Acquire));
        assert!(handle.shared.active_movement_id.lock().is_none());
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn observation_callback_can_stop_without_deadlock_and_preserves_fifo() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let request = events.try_recv().expect("connection request event");
        assert_eq!(request.id, "event-1");

        let source = handle.observation_source();
        let stop_event_ids = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let (stop_returned_tx, stop_returned_rx) = std_mpsc::channel();
        let stop_listener = Arc::new(StopOnFirstListener {
            handle: handle.clone(),
            event_ids: stop_event_ids.clone(),
            stop_returned: stop_returned_tx,
            invoked: AtomicBool::new(false),
        });
        let (other_listener, other_events) = recording_listener();
        let _stop_subscription = ProtocolObservationSource::subscribe(&source, stop_listener)
            .expect("stop listener subscription should succeed");
        let _other_subscription = ProtocolObservationSource::subscribe(&source, other_listener)
            .expect("other listener subscription should succeed");

        let emitting_handle = handle.clone();
        let (emit_finished_tx, emit_finished_rx) = std_mpsc::channel();
        let emit_thread = thread::spawn(move || {
            emit_test_fact(&emitting_handle, BackendEventKind::Entity);
            emit_finished_tx
                .send(())
                .expect("emit completion should be observable");
        });
        emit_finished_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("callback stop and its drain must finish within the bound");
        stop_returned_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("stop must return from inside the callback");
        emit_thread.join().expect("event thread should not panic");

        let mut delivered = Vec::new();
        while let Ok(event) = events.try_recv() {
            delivered.push(event);
        }
        assert_eq!(
            delivered
                .iter()
                .map(|event| event.id.as_str())
                .collect::<Vec<_>>(),
            vec!["event-2", "event-3", "event-4"]
        );
        assert_eq!(payload_json(&delivered[0])["type"], "animation");
        assert_eq!(payload_json(&delivered[1])["type"], "connection_closed");
        assert_eq!(payload_json(&delivered[1])["close"]["deliberate"], true);
        assert_eq!(payload_json(&delivered[2])["type"], "stopped");
        assert_eq!(payload_json(&delivered[2])["reason"], "callback-stop");
        assert_eq!(&*stop_event_ids.lock(), &["event-2".to_owned()]);
        assert_eq!(
            other_events
                .lock()
                .iter()
                .map(observation_event_id)
                .collect::<Vec<_>>(),
            vec!["event-2"]
        );
    }

    #[test]
    fn nested_observation_emit_is_fifo_and_drained_before_top_level_returns() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        handle.shared.begin_connection_attempt();
        let _request = events.try_recv().expect("connection request event");

        let source = handle.observation_source();
        let nested_event_ids = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let nested_listener = Arc::new(NestedEmitListener {
            handle: handle.clone(),
            event_ids: nested_event_ids.clone(),
            emitted: AtomicBool::new(false),
        });
        let (other_listener, other_events) = recording_listener();
        let _nested_subscription = ProtocolObservationSource::subscribe(&source, nested_listener)
            .expect("nested listener subscription should succeed");
        let _other_subscription = ProtocolObservationSource::subscribe(&source, other_listener)
            .expect("other listener subscription should succeed");

        let emitting_handle = handle.clone();
        let (emit_finished_tx, emit_finished_rx) = std_mpsc::channel();
        let emit_thread = thread::spawn(move || {
            emit_test_fact(&emitting_handle, BackendEventKind::Entity);
            emit_finished_tx
                .send(())
                .expect("nested emit completion should be observable");
        });
        emit_finished_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("nested emit and its drain must finish within the bound");
        emit_thread
            .join()
            .expect("nested emit thread should not panic");

        assert_eq!(
            &*nested_event_ids.lock(),
            &[
                "event-2".to_owned(),
                "event-3".to_owned(),
                "event-4".to_owned()
            ]
        );
        assert_eq!(
            other_events
                .lock()
                .iter()
                .map(observation_event_id)
                .collect::<Vec<_>>(),
            vec!["event-2", "event-3", "event-4"]
        );
        let mut delivered = Vec::new();
        while let Ok(event) = events.try_recv() {
            delivered.push(event);
        }
        assert_eq!(
            delivered
                .iter()
                .map(|event| event.id.as_str())
                .collect::<Vec<_>>(),
            vec!["event-2", "event-3", "event-4"]
        );
        assert_eq!(delivered[0].kind, BackendEventKind::Entity);
        assert_eq!(delivered[1].kind, BackendEventKind::Entity);
        assert_eq!(delivered[2].kind, BackendEventKind::Block);
    }

    #[test]
    fn concurrent_producers_keep_connection_request_first_for_each_epoch() {
        const PRODUCER_COUNT: usize = 8;
        const EVENTS_PER_PRODUCER: usize = 32;
        const ATTEMPT_COUNT: usize = 40;

        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let start = Arc::new(Barrier::new(PRODUCER_COUNT + 2));
        let (worker_finished_tx, worker_finished_rx) = std_mpsc::channel();
        let mut workers = Vec::new();
        for producer in 0..PRODUCER_COUNT {
            let shared = handle.shared.clone();
            let start = start.clone();
            let worker_finished = worker_finished_tx.clone();
            workers.push(thread::spawn(move || {
                start.wait();
                for sequence in 0..EVENTS_PER_PRODUCER {
                    shared.emit(
                        FactSource::ServerObserved,
                        BackendEventPayload::World(ContractProtocolWorldEvent::GameChanged {
                            dimension: Some(format!("producer:{producer}")),
                            game_mode: Some(format!("sequence:{sequence}")),
                        }),
                    );
                }
                worker_finished
                    .send(())
                    .expect("producer completion should be observable");
            }));
        }
        let attempt_shared = handle.shared.clone();
        let attempt_start = start.clone();
        let attempt_finished = worker_finished_tx.clone();
        workers.push(thread::spawn(move || {
            attempt_start.wait();
            for _ in 0..ATTEMPT_COUNT {
                attempt_shared.begin_connection_attempt();
            }
            attempt_finished
                .send(())
                .expect("attempt completion should be observable");
        }));
        let worker_count = workers.len();
        drop(worker_finished_tx);
        start.wait();
        for _ in 0..worker_count {
            worker_finished_rx
                .recv_timeout(StdDuration::from_secs(1))
                .expect("all concurrent producers must finish within the bound");
        }
        for worker in workers {
            worker.join().expect("producer should not panic");
        }

        let mut delivered = Vec::new();
        while let Ok(event) = events.try_recv() {
            delivered.push(event);
        }
        assert_eq!(
            delivered.len(),
            PRODUCER_COUNT * EVENTS_PER_PRODUCER + ATTEMPT_COUNT
        );
        for (index, event) in delivered.iter().enumerate() {
            assert_eq!(event.id, format!("event-{}", index + 1));
        }

        let mut first_event_by_epoch = std::collections::BTreeMap::new();
        for event in &delivered {
            if event.connection_epoch > 0 {
                first_event_by_epoch
                    .entry(event.connection_epoch)
                    .or_insert(event);
            }
        }
        assert_eq!(first_event_by_epoch.len(), ATTEMPT_COUNT);
        for epoch in 1..=ATTEMPT_COUNT as u64 {
            let event = first_event_by_epoch
                .get(&epoch)
                .expect("every attempt epoch must have an event");
            assert_eq!(event.kind, BackendEventKind::Lifecycle);
            assert_eq!(event.connection_attempt_id, format!("attempt-{epoch}"));
            let payload = payload_json(event);
            assert_eq!(payload["type"], "connection_requested");
            assert_eq!(payload["attempt"], epoch);
        }
    }

    fn observation_snapshot(epoch: u64) -> MinecraftSnapshotV1 {
        MinecraftSnapshotV1 {
            protocol: "mineintent.minecraft.snapshot.v1".to_owned(),
            snapshot_revision: 1,
            lifecycle_revision: 1,
            captured_at: now_utc(),
            process_session_id: "test-session".to_owned(),
            connection_epoch: epoch,
            connection_attempt_id: format!("attempt-{epoch}"),
            world: WorldSnapshot {
                world_id: "test-world".to_owned(),
                dimension: "minecraft:overworld".to_owned(),
                minecraft_version: "26.1.2".to_owned(),
                protocol_version: 775,
                game_mode: "survival".to_owned(),
                min_y: -64,
                height: 384,
            },
            self_snapshot: SelfSnapshot {
                entity_key: "self".to_owned(),
                username: "MineIntentBot".to_owned(),
                position: Vec3Value {
                    x: 1.0,
                    y: 64.0,
                    z: 2.0,
                },
                velocity: Vec3Value {
                    x: -1.5,
                    y: 0.25,
                    z: 2.75,
                },
                yaw: 0.25,
                pitch: -0.1,
                on_ground: true,
                alive: true,
                health: 20.0,
                food: 20,
                food_saturation: 5.0,
                experience: ExperienceSnapshot {
                    level: 0,
                    progress: 0.0,
                    total: 0,
                },
            },
            inventory: InventorySnapshot {
                selected_hotbar_slot: 0,
                slots: Vec::new(),
            },
            tracked_players: Vec::new(),
        }
    }

    fn observation_entity(entity_key: &str) -> ProtocolEntitySnapshot {
        ProtocolEntitySnapshot {
            entity_key: entity_key.to_owned(),
            protocol_entity_id: 7,
            entity_type: "zombie".to_owned(),
            name: Some("zombie".to_owned()),
            username: None,
            uuid: None,
            position: Vec3Value {
                x: 3.0,
                y: 64.0,
                z: 4.0,
            },
            velocity: Vec3Value {
                x: -0.25,
                y: 0.5,
                z: 0.75,
            },
            yaw: 0.125,
            pitch: -0.25,
            head_yaw: Some(0.5),
            width: 0.625,
            height: 1.875,
            on_ground: false,
            pose: Some("standing".to_owned()),
            held_item_name: Some("iron_sword".to_owned()),
            equipment: vec![crate::snapshot::EntityEquipmentSnapshot {
                slot: 2,
                item_name: "iron_sword".to_owned(),
                count: 3,
            }],
            valid: true,
        }
    }

    fn snapshot_at(epoch: u64, x: f64, y: f64, z: f64) -> MinecraftSnapshotV1 {
        let mut snapshot = observation_snapshot(epoch);
        snapshot.self_snapshot.position = Vec3Value { x, y, z };
        snapshot
    }

    fn no_deadline_control() -> OperationControl {
        test_control(TestCancellation::new(false, None, false, None), None)
    }

    #[tokio::test]
    async fn read_viewport_returns_projection_source_and_unique_revision() {
        let (_handle, source, _world) = ready_viewport_source();

        let first = source
            .read_viewport(no_deadline_control())
            .await
            .expect("ready viewport read should succeed");
        assert_eq!(first.projection.frame.self_pose.position, [1.0, 64.0, 2.0]);
        assert_eq!(
            first.source,
            mineintent_contracts::minecraft::FactSource::ServerObserved
        );
        assert!(first.revision > 0);

        let second = source
            .read_viewport(no_deadline_control())
            .await
            .expect("second ready viewport read should succeed");
        assert!(second.revision > first.revision);
        assert_eq!(second.projection, first.projection);
        assert_eq!(second.source, first.source);
    }

    #[tokio::test]
    async fn read_directed_viewport_uses_atomic_capture_and_revision_discipline() {
        let (handle, source, _world) = ready_viewport_source();
        let position = ContractBlockPosition { x: 0, y: 64, z: -1 };
        let first = source
            .read_directed_viewport(vec![position], no_deadline_control())
            .await
            .expect("directed unloaded target should still return a strict result");
        assert!(first.seen.is_empty());
        assert_eq!(first.unseen.len(), 1);
        assert_eq!(first.unseen[0].at, [0, 64, -1]);
        assert!(first.unseen[0]
            .why
            .contains(&mineintent_contracts::minecraft::DirectedWhy::ChunkNotLoaded));
        assert!(first.unseen[0].by.is_none());
        let revision = handle.shared.viewport_revision.load(Ordering::Acquire);

        let second = source
            .read_directed_viewport(vec![position], no_deadline_control())
            .await
            .expect("second directed read should succeed");
        assert_eq!(second, first);
        assert_eq!(
            handle.shared.viewport_revision.load(Ordering::Acquire),
            revision + 1
        );
    }

    #[tokio::test]
    async fn read_directed_viewport_rejects_duplicates_and_serializes_out_of_world_rows() {
        let (handle, source, _world) = ready_viewport_source();
        let duplicate = source
            .read_directed_viewport(
                vec![
                    ContractBlockPosition { x: 0, y: 64, z: -1 },
                    ContractBlockPosition { x: 0, y: 64, z: -1 },
                ],
                no_deadline_control(),
            )
            .await;
        assert!(matches!(
            duplicate,
            Err(DirectedViewportError::Backend(BackendError::InvalidCommand {
                field,
                ..
            })) if field == "positions"
        ));

        {
            let mut observation = handle.shared.observation.write();
            observation.snapshot = Some(snapshot_at(1, 0.0, 9_999.0, 0.0));
            observation.bump_generation();
        }
        let out_of_world = handle
            .observation_source()
            .read_directed_viewport(
                vec![ContractBlockPosition {
                    x: 0,
                    y: 10_000,
                    z: -1,
                }],
                no_deadline_control(),
            )
            .await;
        let out_of_world = out_of_world.expect("out-of-world coordinates are row-wise answers");
        assert!(out_of_world.seen.is_empty());
        assert_eq!(out_of_world.unseen.len(), 1);
        assert_eq!(out_of_world.unseen[0].at, [0, 10_000, -1]);
        assert_eq!(
            out_of_world.unseen[0].why,
            [mineintent_contracts::minecraft::DirectedWhy::OutOfWorld]
        );
        assert!(out_of_world.unseen[0].by.is_none());
        assert!(serde_json::to_value(&out_of_world.unseen[0])
            .unwrap()
            .get("block")
            .is_none());
    }

    #[tokio::test]
    async fn read_directed_viewport_uses_current_world_height_bounds_rowwise() {
        let (handle, source, world) = ready_viewport_source();
        let (min_y, height) = {
            let world = world.read();
            (world.chunks.min_y(), world.chunks.height())
        };
        let upper_y = i32::try_from(i64::from(min_y) + i64::from(height))
            .expect("test world height upper bound fits i32 coordinates");

        let mut lower_snapshot = snapshot_at(1, 0.5, f64::from(min_y), 0.5);
        lower_snapshot.self_snapshot.pitch = -35.0;
        {
            let mut observation = handle.shared.observation.write();
            observation.snapshot = Some(lower_snapshot);
            observation.bump_generation();
        }
        let lower = source
            .read_directed_viewport(
                vec![
                    ContractBlockPosition {
                        x: 0,
                        y: min_y - 1,
                        z: -3,
                    },
                    ContractBlockPosition {
                        x: 0,
                        y: min_y,
                        z: -3,
                    },
                ],
                no_deadline_control(),
            )
            .await
            .expect("lower boundary should be answered per coordinate");
        assert_eq!(
            lower
                .unseen
                .iter()
                .find(|item| item.at == [0, min_y - 1, -3])
                .expect("lower out-of-world row")
                .why,
            [mineintent_contracts::minecraft::DirectedWhy::OutOfWorld]
        );
        assert!(lower
            .unseen
            .iter()
            .find(|item| item.at == [0, min_y, -3])
            .is_some_and(|item| {
                !item
                    .why
                    .contains(&mineintent_contracts::minecraft::DirectedWhy::OutOfWorld)
            }));

        let mut upper_snapshot = snapshot_at(1, 0.5, f64::from(upper_y - 1), 0.5);
        upper_snapshot.self_snapshot.pitch = 0.0;
        {
            let mut observation = handle.shared.observation.write();
            observation.snapshot = Some(upper_snapshot);
            observation.bump_generation();
        }
        let upper = source
            .read_directed_viewport(
                vec![
                    ContractBlockPosition {
                        x: 0,
                        y: upper_y - 1,
                        z: -3,
                    },
                    ContractBlockPosition {
                        x: 0,
                        y: upper_y,
                        z: -3,
                    },
                ],
                no_deadline_control(),
            )
            .await
            .expect("upper boundary should be answered per coordinate");
        assert_eq!(
            upper
                .unseen
                .iter()
                .find(|item| item.at == [0, upper_y, -3])
                .expect("upper out-of-world row")
                .why,
            [mineintent_contracts::minecraft::DirectedWhy::OutOfWorld]
        );
        assert!(upper
            .unseen
            .iter()
            .find(|item| item.at == [0, upper_y - 1, -3])
            .is_some_and(|item| {
                !item
                    .why
                    .contains(&mineintent_contracts::minecraft::DirectedWhy::OutOfWorld)
            }));
    }

    #[tokio::test]
    async fn read_directed_viewport_cancel_and_deadline_keep_full_kernel_boundaries() {
        let (_handle, source, _world) = ready_viewport_source();
        let cancelled = source
            .read_directed_viewport(
                vec![ContractBlockPosition { x: 0, y: 64, z: -1 }],
                test_control(TestCancellation::new(true, None, false, None), None),
            )
            .await;
        assert_eq!(
            cancelled,
            Err(DirectedViewportError::Backend(BackendError::Cancelled {
                operation: "read_directed_viewport".to_owned()
            }))
        );

        let deadline = TestDeadline::new(true, None, None);
        let expired = source
            .read_directed_viewport(
                vec![ContractBlockPosition { x: 0, y: 64, z: -1 }],
                test_control(
                    TestCancellation::new(false, None, false, None),
                    Some(deadline),
                ),
            )
            .await;
        assert_eq!(
            expired,
            Err(DirectedViewportError::Backend(
                BackendError::DeadlineExceeded {
                    operation: "read_directed_viewport".to_owned()
                }
            ))
        );
    }

    #[tokio::test]
    async fn read_directed_viewport_worker_wakeup_preserves_operation_name() {
        let (_handle, source, _world) = ready_viewport_source();
        let cancellation = WorkerWakeCancellation::new();
        let cancelled = tokio::time::timeout(
            Duration::from_secs(1),
            source.read_directed_viewport(
                vec![ContractBlockPosition { x: 0, y: 64, z: -1 }],
                OperationControl::new(cancellation.clone(), None),
            ),
        )
        .await
        .expect("worker cancellation test must be bounded");
        assert!(cancellation.worker_started.load(Ordering::SeqCst));
        assert_eq!(
            cancelled,
            Err(DirectedViewportError::Backend(BackendError::Cancelled {
                operation: "read_directed_viewport".to_owned()
            }))
        );

        let (_handle, source, _world) = ready_viewport_source();
        let deadline = WorkerWakeDeadline::new();
        let expired = tokio::time::timeout(
            Duration::from_secs(1),
            source.read_directed_viewport(
                vec![ContractBlockPosition { x: 0, y: 64, z: -1 }],
                OperationControl::new(
                    TestCancellation::new(false, None, false, None),
                    Some(deadline.clone()),
                ),
            ),
        )
        .await
        .expect("worker deadline test must be bounded");
        assert!(deadline.worker_started.load(Ordering::SeqCst));
        assert_eq!(
            expired,
            Err(DirectedViewportError::Backend(
                BackendError::DeadlineExceeded {
                    operation: "read_directed_viewport".to_owned()
                }
            ))
        );
    }

    #[tokio::test]
    async fn read_directed_viewport_retries_generation_and_rejects_stale_epoch_atomically() {
        let (handle, source, world) = ready_viewport_source();
        let replacement = snapshot_at(1, 9.0, 64.0, 10.0);
        let shared = handle.shared.clone();
        let action: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let mut observation = shared.observation.write();
            observation.snapshot = Some(replacement.clone());
            observation.source = Some(FactSource::ClientPredicted);
            observation.tracked_entities = vec![observation_entity("replacement")];
            observation.world = Some(world.clone());
            observation.bump_generation();
        });
        let trigger = TestCancellation::new(false, Some(4), false, Some(action));
        let read = source
            .read_directed_viewport(
                vec![ContractBlockPosition { x: 0, y: 64, z: -1 }],
                test_control(trigger, None),
            )
            .await
            .expect("changed directed capture should retry");
        assert_eq!(read.unseen[0].at, [0, 64, -1]);

        let (handle, source, _world) = ready_viewport_source();
        let shared = handle.shared.clone();
        let action: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            shared.begin_connection_attempt();
        });
        let trigger = TestCancellation::new(false, Some(4), false, Some(action));
        let stale = source
            .read_directed_viewport(
                vec![ContractBlockPosition { x: 0, y: 64, z: -1 }],
                test_control(trigger, None),
            )
            .await;
        assert!(matches!(
            stale,
            Err(DirectedViewportError::Backend(BackendError::StaleEpoch {
                bound_epoch: 1,
                current_epoch: 2,
            }))
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn read_viewport_revision_is_unique_for_concurrent_successes() {
        let (_handle, source, _world) = ready_viewport_source();
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let source = source.clone();
            tasks.push(tokio::spawn(async move {
                source
                    .read_viewport(no_deadline_control())
                    .await
                    .expect("concurrent viewport read should succeed")
                    .revision
            }));
        }

        let mut revisions = Vec::new();
        for task in tasks {
            revisions.push(task.await.expect("viewport task should not panic"));
        }
        revisions.sort_unstable();
        assert!(revisions.windows(2).all(|window| window[0] < window[1]));
    }

    #[tokio::test]
    async fn read_viewport_preflight_cancel_and_deadline_do_not_scan() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();

        let cancellation = TestCancellation::new(true, None, false, None);
        let cancelled = source
            .read_viewport(test_control(cancellation.clone(), None))
            .await;
        assert_eq!(
            cancelled,
            Err(BackendError::Cancelled {
                operation: "read_viewport".to_owned()
            })
        );
        assert_eq!(cancellation.checks.load(Ordering::SeqCst), 1);

        let deadline_cancellation = TestCancellation::new(false, None, false, None);
        let deadline = TestDeadline::new(true, None, None);
        let expired = source
            .read_viewport(test_control(
                deadline_cancellation.clone(),
                Some(deadline.clone()),
            ))
            .await;
        assert_eq!(
            expired,
            Err(BackendError::DeadlineExceeded {
                operation: "read_viewport".to_owned()
            })
        );
        assert_eq!(deadline_cancellation.checks.load(Ordering::SeqCst), 1);
        assert_eq!(deadline.checks.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn read_viewport_observes_cancellation_and_deadline_during_projection() {
        let (_handle, source, _world) = ready_viewport_source();

        let cancellation = TestCancellation::new(false, Some(7), true, None);
        let cancelled = source
            .read_viewport(test_control(cancellation.clone(), None))
            .await;
        assert_eq!(
            cancelled,
            Err(BackendError::Cancelled {
                operation: "read_viewport".to_owned()
            })
        );
        assert!(cancellation.checks.load(Ordering::SeqCst) >= 7);

        let deadline_cancellation = TestCancellation::new(false, None, false, None);
        let deadline = TestDeadline::new(false, Some(7), None);
        let expired = source
            .read_viewport(test_control(deadline_cancellation, Some(deadline.clone())))
            .await;
        assert_eq!(
            expired,
            Err(BackendError::DeadlineExceeded {
                operation: "read_viewport".to_owned()
            })
        );
        assert!(deadline.checks.load(Ordering::SeqCst) >= 7);
    }

    #[tokio::test]
    async fn read_viewport_retries_when_capture_generation_changes() {
        let (handle, source, world) = ready_viewport_source();
        let replacement = snapshot_at(1, 9.0, 64.0, 10.0);
        let shared = handle.shared.clone();
        let action: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let mut observation = shared.observation.write();
            observation.snapshot = Some(replacement.clone());
            observation.source = Some(FactSource::ClientPredicted);
            observation.tracked_entities = vec![observation_entity("replacement")];
            observation.world = Some(world.clone());
            observation.bump_generation();
        });
        let trigger = TestCancellation::new(false, Some(4), false, Some(action));

        let read = source
            .read_viewport(test_control(trigger, None))
            .await
            .expect("a changed capture should be retried, not mixed");
        assert_eq!(read.projection.frame.self_pose.position, [9.0, 64.0, 10.0]);
        assert_eq!(
            read.source,
            mineintent_contracts::minecraft::FactSource::ClientPredicted
        );
    }

    #[tokio::test]
    async fn read_viewport_rejects_epoch_change_after_capture() {
        let (handle, source, _world) = ready_viewport_source();
        let shared = handle.shared.clone();
        let action: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            shared.begin_connection_attempt();
        });
        let trigger = TestCancellation::new(false, Some(4), false, Some(action));

        let read = source.read_viewport(test_control(trigger, None)).await;
        assert_eq!(
            read,
            Err(BackendError::StaleEpoch {
                bound_epoch: 1,
                current_epoch: 2,
            })
        );
    }

    #[tokio::test]
    async fn read_viewport_rejects_missing_ready_capture_parts() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();
        handle.shared.ready.store(true, Ordering::Release);

        let missing_snapshot = source.read_viewport(no_deadline_control()).await;
        assert_eq!(
            missing_snapshot,
            Err(BackendError::NotReady {
                state: "viewport_snapshot_unavailable".to_owned()
            })
        );

        let world = empty_world();
        {
            let mut observation = handle.shared.observation.write();
            observation.snapshot = Some(observation_snapshot(1));
            observation.source = None;
            observation.world = Some(world.clone());
            observation.bump_generation();
        }
        let missing_source = source.read_viewport(no_deadline_control()).await;
        assert_eq!(
            missing_source,
            Err(BackendError::NotReady {
                state: "viewport_source_unavailable".to_owned()
            })
        );

        {
            let mut observation = handle.shared.observation.write();
            observation.source = Some(FactSource::ServerObserved);
            observation.bump_generation();
        }
        let ready = source.read_viewport(no_deadline_control()).await;
        assert!(ready.is_ok());

        {
            let mut observation = handle.shared.observation.write();
            observation.world = None;
            observation.bump_generation();
        }
        let missing_world = source.read_viewport(no_deadline_control()).await;
        assert_eq!(
            missing_world,
            Err(BackendError::NotReady {
                state: "viewport_world_unavailable".to_owned()
            })
        );
    }

    struct RecordingListener {
        events: Arc<parking_lot::Mutex<Vec<ObservationEvent>>>,
    }

    impl ObservationEventListener for RecordingListener {
        fn on_event(&self, event: ObservationEvent) {
            self.events.lock().push(event);
        }
    }

    fn recording_listener() -> (
        Arc<RecordingListener>,
        Arc<parking_lot::Mutex<Vec<ObservationEvent>>>,
    ) {
        let events = Arc::new(parking_lot::Mutex::new(Vec::new()));
        (
            Arc::new(RecordingListener {
                events: events.clone(),
            }),
            events,
        )
    }

    struct NoopListener;

    impl ObservationEventListener for NoopListener {
        fn on_event(&self, _event: ObservationEvent) {}
    }

    fn observation_event_id(event: &ObservationEvent) -> &str {
        match event {
            ObservationEvent::Entity(event) => &event.id,
            ObservationEvent::Block(event) => &event.id,
            ObservationEvent::Sound(event) => &event.id,
        }
    }

    struct StopOnFirstListener {
        handle: RuntimeHandle,
        event_ids: Arc<parking_lot::Mutex<Vec<String>>>,
        stop_returned: std_mpsc::Sender<()>,
        invoked: AtomicBool,
    }

    impl ObservationEventListener for StopOnFirstListener {
        fn on_event(&self, event: ObservationEvent) {
            self.event_ids
                .lock()
                .push(observation_event_id(&event).to_owned());
            if !self.invoked.swap(true, Ordering::SeqCst) {
                self.handle.stop("callback-stop");
                self.stop_returned
                    .send(())
                    .expect("stop completion should be observable");
            }
        }
    }

    struct NestedEmitListener {
        handle: RuntimeHandle,
        event_ids: Arc<parking_lot::Mutex<Vec<String>>>,
        emitted: AtomicBool,
    }

    impl ObservationEventListener for NestedEmitListener {
        fn on_event(&self, event: ObservationEvent) {
            self.event_ids
                .lock()
                .push(observation_event_id(&event).to_owned());
            if self.emitted.swap(true, Ordering::SeqCst) {
                return;
            }
            self.handle.shared.emit(
                FactSource::ServerObserved,
                valid_observation_payload(BackendEventKind::Entity),
            );
            self.handle.shared.emit(
                FactSource::ServerObserved,
                valid_observation_payload(BackendEventKind::Block),
            );
        }
    }

    struct PanicListener;

    impl ObservationEventListener for PanicListener {
        fn on_event(&self, _event: ObservationEvent) {
            panic!("observation listener test panic");
        }
    }

    struct ReentrantListener {
        source: RuntimeObservationSource,
        invoked: AtomicBool,
        pose: Arc<parking_lot::Mutex<Option<Result<ContractSelfPose, BackendError>>>>,
        block: Arc<parking_lot::Mutex<Option<Result<ContractBlockReadResult, BackendError>>>>,
        nested_subscription_succeeded: AtomicBool,
    }

    impl ObservationEventListener for ReentrantListener {
        fn on_event(&self, _event: ObservationEvent) {
            if self.invoked.swap(true, Ordering::SeqCst) {
                return;
            }
            *self.pose.lock() = Some(self.source.self_pose());
            *self.block.lock() =
                Some(
                    self.source
                        .read_block(ContractBlockPosition { x: 0, y: 64, z: 0 }),
                );
            let nested = ProtocolObservationSource::subscribe(&self.source, Arc::new(NoopListener));
            self.nested_subscription_succeeded
                .store(nested.is_ok(), Ordering::SeqCst);
        }
    }

    struct BlockingListener {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
        calls: Arc<AtomicUsize>,
    }

    impl ObservationEventListener for BlockingListener {
        fn on_event(&self, _event: ObservationEvent) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.wait();
            self.release.wait();
        }
    }

    struct AUnsubscribesBListener {
        b_subscription: Arc<StdMutex<Option<Box<dyn Subscription>>>>,
        calls: Arc<AtomicUsize>,
    }

    impl ObservationEventListener for AUnsubscribesBListener {
        fn on_event(&self, _event: ObservationEvent) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut subscription = self
                .b_subscription
                .lock()
                .expect("B subscription mutex should not be poisoned")
                .take()
                .expect("B subscription should be present during A callback");
            subscription.unsubscribe();
        }
    }

    fn valid_observation_payload(kind: BackendEventKind) -> BackendEventPayload {
        match kind {
            BackendEventKind::Entity => {
                BackendEventPayload::Entity(ContractProtocolEntityEvent::Animation {
                    entity_key: "entity-7".to_owned(),
                    animation: "swing".to_owned(),
                })
            }
            BackendEventKind::Block => {
                BackendEventPayload::Block(ContractProtocolBlockEvent::ChunkLoaded {
                    chunk_x: 3,
                    chunk_z: -4,
                })
            }
            BackendEventKind::Sound => BackendEventPayload::Sound(ContractProtocolSoundPayload {
                event_type: mineintent_contracts::minecraft::HeardSoundType::Heard,
                sound_key: "minecraft:block.note_block.harp".to_owned(),
                sound_name: Some("note_block.harp".to_owned()),
                sound_id: Some(12),
                category: Some("blocks".to_owned()),
                source_position: ContractVec3Value {
                    x: 1.5,
                    y: 64.25,
                    z: -2.0,
                },
                volume: 0.75,
                pitch: 1.25,
                protocol_source:
                    mineintent_contracts::minecraft::ProtocolSoundSource::NamedSoundEffect,
            }),
            BackendEventKind::Lifecycle => {
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected)
            }
            BackendEventKind::Chat => BackendEventPayload::Chat(ContractProtocolChatEvent {
                sender_username: Some("Alex".to_owned()),
                plain_text: "hello".to_owned(),
                position: Some(ChatPosition::Chat),
                verified: None,
            }),
            BackendEventKind::SelfState => BackendEventPayload::SelfState(
                ContractProtocolSelfEvent::ServerPositionCorrection {
                    teleport_id: 1,
                    position: ContractVec3Value {
                        x: 0.0,
                        y: 64.0,
                        z: 0.0,
                    },
                    velocity: ContractVec3Value {
                        x: 0.0,
                        y: 0.0,
                        z: 0.0,
                    },
                    yaw: 0.0,
                    pitch: 0.0,
                    relative: RelativeMovementFlags {
                        x: false,
                        y: false,
                        z: false,
                        yaw: false,
                        pitch: false,
                        delta_x: false,
                        delta_y: false,
                        delta_z: false,
                        rotate_delta: false,
                    },
                },
            ),
            BackendEventKind::World => {
                BackendEventPayload::World(ContractProtocolWorldEvent::GameChanged {
                    dimension: Some("minecraft:overworld".to_owned()),
                    game_mode: Some("survival".to_owned()),
                })
            }
            BackendEventKind::PlayerList => {
                BackendEventPayload::PlayerList(ContractProtocolPlayerListEvent::Add {
                    uuid: "uuid-1".to_owned(),
                    username: "Alex".to_owned(),
                })
            }
            BackendEventKind::SnapshotChanged => {
                BackendEventPayload::SnapshotChanged(ContractProtocolSnapshotChangedEvent {
                    group: "self".to_owned(),
                    snapshot_revision: 1,
                })
            }
            BackendEventKind::Overflow => BackendEventPayload::Overflow(
                mineintent_contracts::minecraft::BackendOverflowPayload {
                    event_type: mineintent_contracts::minecraft::OverflowType::Overflow,
                    dropped_count: 1,
                    dropped_kinds: vec![BackendEventKind::Entity],
                },
            ),
        }
    }

    fn emit_test_fact(handle: &RuntimeHandle, kind: BackendEventKind) {
        handle
            .shared
            .emit(FactSource::ServerObserved, valid_observation_payload(kind));
    }

    fn emit_and_capture(
        handle: &RuntimeHandle,
        payload: BackendEventPayload,
    ) -> BackendEventEnvelope {
        let mut events = handle.subscribe();
        handle.shared.emit(FactSource::ServerObserved, payload);
        events
            .try_recv()
            .expect("global v2 event should be emitted for adapter fixture")
    }

    fn contract_event_kind(event: &ObservationEvent) -> ContractBackendEventKind {
        match event {
            ObservationEvent::Entity(_) => ContractBackendEventKind::Entity,
            ObservationEvent::Block(_) => ContractBackendEventKind::Block,
            ObservationEvent::Sound(_) => ContractBackendEventKind::Sound,
        }
    }

    fn assert_metadata<T>(
        raw: &BackendEventEnvelope,
        typed: &ContractBackendEventEnvelope<T>,
        kind: ContractBackendEventKind,
    ) {
        assert_eq!(typed.protocol, ContractBackendEventProtocol::V2);
        assert_eq!(typed.id, raw.id);
        assert_eq!(typed.kind, kind);
        assert_eq!(typed.occurred_at, raw.occurred_at);
        assert_eq!(typed.process_session_id, raw.process_session_id);
        assert_eq!(typed.connection_epoch, raw.connection_epoch);
        assert_eq!(typed.connection_attempt_id, raw.connection_attempt_id);
        assert_eq!(typed.world_id, raw.world_id);
        assert_eq!(typed.dimension, raw.dimension);
        assert_eq!(typed.source, ContractFactSource::ServerObserved);
    }

    fn contract_entity_event_fixture() -> ContractProtocolEntityEvent {
        ContractProtocolEntityEvent::Spawned {
            entity: ContractProtocolEntitySnapshot {
                entity_key: "entity-7".to_owned(),
                protocol_entity_id: 7,
                entity_type: "zombie".to_owned(),
                name: Some("zombie".to_owned()),
                username: None,
                uuid: Some("uuid-7".to_owned()),
                position: ContractVec3Value {
                    x: 3.0,
                    y: 64.0,
                    z: 4.0,
                },
                velocity: ContractVec3Value {
                    x: -0.25,
                    y: 0.5,
                    z: 0.75,
                },
                yaw: 0.125,
                pitch: -0.25,
                head_yaw: Some(0.5),
                width: 0.625,
                height: 1.875,
                on_ground: false,
                pose: Some("standing".to_owned()),
                held_item_name: Some("iron_sword".to_owned()),
                equipment: vec![ContractEntityEquipmentSnapshot {
                    slot: 2,
                    item_name: "iron_sword".to_owned(),
                    count: 3,
                }],
                valid: true,
            },
        }
    }

    fn contract_block_event_fixture() -> ContractProtocolBlockEvent {
        ContractProtocolBlockEvent::Updated {
            old_block: None,
            new_block: Some(ContractProtocolBlockSnapshot {
                position: ContractBlockPosition { x: 3, y: 64, z: -2 },
                name: "stone".to_owned(),
                state_id: 42,
                properties: [(
                    "axis".to_owned(),
                    ContractBlockPropertyValue::String("y".to_owned()),
                )]
                .into_iter()
                .collect(),
                collision_shapes: vec![[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]],
                transparent_hint: false,
                bounding_box: ContractBlockBoundingBox::Block,
            }),
        }
    }

    fn contract_sound_fixture() -> ContractProtocolSoundPayload {
        ContractProtocolSoundPayload {
            event_type: ContractHeardSoundType::Heard,
            sound_key: "minecraft:block.note_block.harp".to_owned(),
            sound_name: Some("note_block.harp".to_owned()),
            sound_id: Some(12),
            category: Some("blocks".to_owned()),
            source_position: ContractVec3Value {
                x: 1.5,
                y: 64.25,
                z: -2.0,
            },
            volume: 0.75,
            pitch: 1.25,
            protocol_source: ContractProtocolSoundSource::NamedSoundEffect,
        }
    }

    fn backend_block_fixture() -> ProtocolBlockSnapshot {
        ProtocolBlockSnapshot {
            position: BlockPosition { x: 3, y: 64, z: -2 },
            name: "stone".to_owned(),
            state_id: 42,
            properties: [("axis".to_owned(), "y".to_owned())].into_iter().collect(),
            collision_shapes: vec![[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]],
            transparent_hint: false,
            bounding_box: BlockBoundingBox::Block,
        }
    }

    #[test]
    fn observation_source_binds_epoch_and_returns_structured_stale_errors() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();
        assert_eq!(source.epoch(), 1);

        {
            let mut observation = handle.shared.observation.write();
            observation.snapshot = Some(observation_snapshot(1));
            observation.source = Some(FactSource::ServerObserved);
            observation
                .tracked_entities
                .push(observation_entity("entity-7"));
            observation.bump_generation();
        }

        let pose = source
            .self_pose()
            .expect("current source pose should not be stale");
        assert_eq!(pose.position.x, 1.0);
        assert_eq!(source.list_tracked_entities().unwrap().len(), 1);
        assert_eq!(
            source
                .read_block(ContractBlockPosition { x: 0, y: 64, z: 0 })
                .expect("current source block read should not be stale"),
            ContractBlockReadResult::Unloaded
        );

        handle.shared.begin_connection_attempt();
        assert_eq!(source.epoch(), 1, "old source must keep its bound epoch");
        let stale = BackendError::StaleEpoch {
            bound_epoch: 1,
            current_epoch: 2,
        };
        let stale_wire = serde_json::to_value(&stale).expect("stale error should be structured");
        assert_eq!(stale_wire["code"], "stale_epoch");
        assert_eq!(stale_wire["boundEpoch"], 1);
        assert_eq!(stale_wire["currentEpoch"], 2);
        assert_eq!(source.self_pose(), Err(stale.clone()));
        assert_eq!(source.list_tracked_players(), Err(stale.clone()));
        assert_eq!(source.list_tracked_entities(), Err(stale.clone()));
        assert_eq!(
            source.read_block(ContractBlockPosition { x: 0, y: 64, z: 0 }),
            Err(stale.clone())
        );
        assert_eq!(source.snapshot_source(), Err(stale));
        assert!(matches!(
            ProtocolObservationSource::subscribe(&source, Arc::new(NoopListener)),
            Err(BackendError::StaleEpoch {
                bound_epoch: 1,
                current_epoch: 2,
            })
        ));
    }

    #[test]
    fn self_pose_without_snapshot_is_stable_not_ready() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();
        let expected = BackendError::NotReady {
            state: "self_pose_unavailable".to_owned(),
        };
        assert_eq!(source.self_pose(), Err(expected.clone()));
        assert_eq!(source.self_pose(), Err(expected));
    }

    #[test]
    fn read_block_unloaded_early_return_rechecks_epoch() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();

        let result =
            source.read_block_with_post_read_hook(BlockPosition { x: 0, y: 64, z: 0 }, || {
                handle.shared.begin_connection_attempt();
            });
        assert!(matches!(
            result,
            Err(BackendError::StaleEpoch {
                bound_epoch: 1,
                current_epoch: 2,
            })
        ));
    }

    #[test]
    fn protocol_observation_trait_object_maps_pose_entity_block_and_viewport_dto() {
        let (_handle, concrete_source, _world) = ready_viewport_source();
        let source: Arc<dyn ProtocolObservationSource> = Arc::new(concrete_source);

        let pose = source
            .self_pose()
            .expect("trait object pose should be ready");
        assert_eq!(
            pose.position,
            ContractVec3Value {
                x: 1.0,
                y: 64.0,
                z: 2.0
            }
        );
        assert_eq!(
            pose.velocity,
            ContractVec3Value {
                x: -1.5,
                y: 0.25,
                z: 2.75
            }
        );
        assert_eq!(pose.yaw, 0.25_f32 as f64);
        assert_eq!(pose.pitch, -0.1_f32 as f64);

        let entities = source
            .list_tracked_entities()
            .expect("trait object entity list should be ready");
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].entity_key, "entity-7");
        assert_eq!(entities[0].velocity.x, -0.25_f32 as f64);
        assert_eq!(entities[0].head_yaw, Some(0.5_f32 as f64));
        assert_eq!(entities[0].equipment[0].slot, 2);
        assert_eq!(entities[0].equipment[0].count, 3);

        assert_eq!(
            source
                .read_block(ContractBlockPosition { x: 0, y: 64, z: 0 })
                .expect("trait object block read should be ready"),
            ContractBlockReadResult::Unloaded
        );
        assert_eq!(
            source
                .read_block(ContractBlockPosition {
                    x: 0,
                    y: 10_000,
                    z: 0
                })
                .expect("out-of-world block read should be explicit"),
            ContractBlockReadResult::OutOfWorld
        );

        let converted = contract_block_snapshot(backend_block_fixture());
        assert_eq!(
            converted.position,
            ContractBlockPosition { x: 3, y: 64, z: -2 }
        );
        assert_eq!(converted.name, "stone");
        assert_eq!(converted.state_id, 42);
        assert_eq!(
            converted.properties["axis"],
            ContractBlockPropertyValue::String("y".to_owned())
        );
        assert_eq!(
            converted.collision_shapes,
            vec![[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]]
        );
        assert!(!converted.transparent_hint);
        assert_eq!(converted.bounding_box, ContractBlockBoundingBox::Block);
    }

    #[tokio::test]
    async fn protocol_observation_trait_object_delegates_atomic_viewport() {
        let (_handle, concrete_source, _world) = ready_viewport_source();
        let source: Arc<dyn ProtocolObservationSource> = Arc::new(concrete_source);
        let read = source
            .read_viewport(no_deadline_control())
            .await
            .expect("trait object viewport should delegate to atomic implementation");
        assert_eq!(read.projection.frame.self_pose.position, [1.0, 64.0, 2.0]);
        assert_eq!(read.source, ContractFactSource::ServerObserved);
        assert!(read.revision > 0);
    }

    #[test]
    fn observation_subscription_filters_kind_and_epoch_without_background_tasks() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        handle.shared.set_dimension("minecraft:overworld");
        let source = handle.observation_source();
        let (old_listener, old_events) = recording_listener();
        let mut old_subscription =
            ProtocolObservationSource::subscribe(&source, old_listener).expect("subscribe");
        assert_eq!(handle.shared.observation_subscribers.lock().len(), 1);

        for kind in [
            BackendEventKind::Entity,
            BackendEventKind::Block,
            BackendEventKind::Sound,
        ] {
            emit_test_fact(&handle, kind);
        }
        for kind in [
            BackendEventKind::Lifecycle,
            BackendEventKind::Chat,
            BackendEventKind::SelfState,
            BackendEventKind::World,
            BackendEventKind::PlayerList,
            BackendEventKind::SnapshotChanged,
            BackendEventKind::Overflow,
        ] {
            emit_test_fact(&handle, kind);
        }

        let observed = old_events.lock();
        assert_eq!(observed.len(), 3);
        assert_eq!(
            observed.iter().map(contract_event_kind).collect::<Vec<_>>(),
            vec![
                ContractBackendEventKind::Entity,
                ContractBackendEventKind::Block,
                ContractBackendEventKind::Sound
            ]
        );
        drop(observed);

        handle.shared.begin_connection_attempt();
        emit_test_fact(&handle, BackendEventKind::Entity);
        assert_eq!(old_events.lock().len(), 3);

        let new_source = handle.observation_source();
        let (new_listener, new_events) = recording_listener();
        let mut new_subscription = ProtocolObservationSource::subscribe(&new_source, new_listener)
            .expect("new epoch source subscribes");
        emit_test_fact(&handle, BackendEventKind::Block);
        assert_eq!(old_events.lock().len(), 3);
        assert_eq!(new_events.lock().len(), 1);
        assert!(matches!(
            &new_events.lock()[0],
            ObservationEvent::Block(event) if event.connection_epoch == 2
        ));

        old_subscription.unsubscribe();
        new_subscription.unsubscribe();
    }

    #[test]
    fn observation_subscription_registration_rechecks_epoch_after_reconnect_hook() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();
        let result = source.subscribe_with_post_register_hook(Arc::new(NoopListener), || {
            handle.shared.begin_connection_attempt();
        });
        assert!(matches!(
            result,
            Err(BackendError::StaleEpoch {
                bound_epoch: 1,
                current_epoch: 2,
            })
        ));
        assert_eq!(handle.shared.observation_subscribers.lock().len(), 0);

        let new_source = handle.observation_source();
        let subscription =
            ProtocolObservationSource::subscribe(&new_source, Arc::new(NoopListener))
                .expect("current epoch subscription should succeed");
        assert!(!subscription.is_closed());
        drop(subscription);
        assert_eq!(handle.shared.observation_subscribers.lock().len(), 0);
    }

    #[test]
    fn observation_subscription_unsubscribe_and_drop_release_listener_registry() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();
        let (listener, events) = recording_listener();
        let mut unsubscribed =
            ProtocolObservationSource::subscribe(&source, listener).expect("subscribe");
        assert_eq!(handle.shared.observation_subscribers.lock().len(), 1);
        unsubscribed.unsubscribe();
        unsubscribed.unsubscribe();
        assert!(unsubscribed.is_closed());
        assert_eq!(handle.shared.observation_subscribers.lock().len(), 0);
        emit_test_fact(&handle, BackendEventKind::Entity);
        assert!(
            events.lock().is_empty(),
            "unsubscribe must prevent delivery"
        );

        let (dropped_listener, _dropped_events) = recording_listener();
        let dropped = ProtocolObservationSource::subscribe(&source, dropped_listener)
            .expect("second subscription");
        assert_eq!(handle.shared.observation_subscribers.lock().len(), 1);
        drop(dropped);
        assert_eq!(handle.shared.observation_subscribers.lock().len(), 0);
    }

    #[test]
    fn observation_events_convert_entity_block_sound_to_v2_typed_payloads() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        handle.shared.set_dimension("minecraft:the_nether");
        let source = handle.observation_source();
        let (listener, events) = recording_listener();
        let _subscription =
            ProtocolObservationSource::subscribe(&source, listener).expect("subscribe");

        let expected_entity = contract_entity_event_fixture();
        let expected_block = contract_block_event_fixture();
        let expected_sound = contract_sound_fixture();
        let raw_entity = emit_and_capture(
            &handle,
            BackendEventPayload::Entity(expected_entity.clone()),
        );
        let raw_block =
            emit_and_capture(&handle, BackendEventPayload::Block(expected_block.clone()));
        let raw_sound =
            emit_and_capture(&handle, BackendEventPayload::Sound(expected_sound.clone()));

        let observed = events.lock();
        assert_eq!(observed.len(), 3);
        match &observed[0] {
            ObservationEvent::Entity(event) => {
                assert_metadata(&raw_entity, event, ContractBackendEventKind::Entity);
                assert_eq!(event.payload, expected_entity);
            }
            other => panic!("expected typed entity event, got {other:?}"),
        }
        match &observed[1] {
            ObservationEvent::Block(event) => {
                assert_metadata(&raw_block, event, ContractBackendEventKind::Block);
                assert_eq!(event.payload, expected_block);
            }
            other => panic!("expected typed block event, got {other:?}"),
        }
        match &observed[2] {
            ObservationEvent::Sound(event) => {
                assert_metadata(&raw_sound, event, ContractBackendEventKind::Sound);
                assert_eq!(event.payload, expected_sound);
            }
            other => panic!("expected typed sound event, got {other:?}"),
        }
    }

    #[test]
    fn typed_observation_payload_is_direct_and_kind_bound() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();
        let (listener, events) = recording_listener();
        let _subscription =
            ProtocolObservationSource::subscribe(&source, listener).expect("subscribe");

        assert!(events.lock().is_empty());
        emit_test_fact(&handle, BackendEventKind::Entity);
        assert_eq!(events.lock().len(), 1);
    }

    #[test]
    fn callback_panic_isolated_from_later_listeners_and_events() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();
        let _panic_subscription =
            ProtocolObservationSource::subscribe(&source, Arc::new(PanicListener))
                .expect("panic listener subscription should succeed");
        let (listener, events) = recording_listener();
        let _recording_subscription =
            ProtocolObservationSource::subscribe(&source, listener).expect("subscribe");

        emit_test_fact(&handle, BackendEventKind::Entity);
        emit_test_fact(&handle, BackendEventKind::Block);
        assert_eq!(events.lock().len(), 2);
    }

    #[test]
    fn callback_runs_outside_registry_lock_and_can_read_and_resubscribe() {
        let (handle, source, _world) = ready_viewport_source();
        let pose = Arc::new(parking_lot::Mutex::new(None));
        let block = Arc::new(parking_lot::Mutex::new(None));
        let listener = Arc::new(ReentrantListener {
            source: source.clone(),
            invoked: AtomicBool::new(false),
            pose: pose.clone(),
            block: block.clone(),
            nested_subscription_succeeded: AtomicBool::new(false),
        });
        let _subscription =
            ProtocolObservationSource::subscribe(&source, listener.clone()).expect("subscribe");

        emit_test_fact(&handle, BackendEventKind::Entity);
        assert!(listener
            .nested_subscription_succeeded
            .load(Ordering::SeqCst));
        assert!(pose.lock().as_ref().is_some_and(Result::is_ok));
        assert_eq!(
            block.lock().as_ref(),
            Some(&Ok(ContractBlockReadResult::Unloaded))
        );
    }

    #[test]
    fn unsubscribe_waits_for_active_callback_and_returns_before_no_new_delivery() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let calls = Arc::new(AtomicUsize::new(0));
        let listener = Arc::new(BlockingListener {
            entered: entered.clone(),
            release: release.clone(),
            calls: calls.clone(),
        });
        let subscription = ProtocolObservationSource::subscribe(&source, listener)
            .expect("blocking subscription should succeed");
        let holder = Arc::new(StdMutex::new(Some(subscription)));

        let emitting_handle = handle.clone();
        let emit_thread = thread::spawn(move || {
            emit_test_fact(&emitting_handle, BackendEventKind::Entity);
        });
        entered.wait();

        let (unsubscribed_tx, unsubscribed_rx) = std_mpsc::channel();
        let unsubscribe_holder = holder.clone();
        let unsubscribe_thread = thread::spawn(move || {
            let mut subscription = unsubscribe_holder
                .lock()
                .expect("subscription mutex should not be poisoned")
                .take()
                .expect("subscription should be owned by unsubscribe thread");
            subscription.unsubscribe();
            unsubscribed_tx
                .send(())
                .expect("unsubscribe completion should be observable");
        });
        assert!(unsubscribed_rx
            .recv_timeout(StdDuration::from_millis(100))
            .is_err());

        release.wait();
        unsubscribed_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("unsubscribe should return after active callback finishes");
        emit_thread.join().expect("event thread should not panic");
        unsubscribe_thread
            .join()
            .expect("unsubscribe thread should not panic");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(handle.shared.observation_subscribers.lock().len(), 0);

        emit_test_fact(&handle, BackendEventKind::Block);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn listener_a_can_unsubscribe_reserved_listener_b_without_deadlock_or_delivery() {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let source = handle.observation_source();
        let b_calls = Arc::new(AtomicUsize::new(0));
        let b_holder = Arc::new(StdMutex::new(None));
        let a_calls = Arc::new(AtomicUsize::new(0));
        let a_listener = Arc::new(AUnsubscribesBListener {
            b_subscription: b_holder.clone(),
            calls: a_calls.clone(),
        });
        let _a_subscription = ProtocolObservationSource::subscribe(&source, a_listener)
            .expect("A subscription should succeed");
        let b_listener = Arc::new(BlockingListener {
            entered: Arc::new(Barrier::new(1)),
            release: Arc::new(Barrier::new(1)),
            calls: b_calls.clone(),
        });
        let b_subscription = ProtocolObservationSource::subscribe(&source, b_listener)
            .expect("B subscription should succeed");
        *b_holder
            .lock()
            .expect("B subscription mutex should not be poisoned") = Some(b_subscription);

        let emitting_handle = handle.clone();
        let (emit_finished_tx, emit_finished_rx) = std_mpsc::channel();
        let emit_thread = thread::spawn(move || {
            emit_test_fact(&emitting_handle, BackendEventKind::Entity);
            emit_finished_tx
                .send(())
                .expect("emit completion should be observable");
        });
        emit_finished_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("A unsubscribing reserved B must not deadlock dispatch");
        emit_thread.join().expect("event thread should not panic");
        assert_eq!(a_calls.load(Ordering::SeqCst), 1);
        assert_eq!(b_calls.load(Ordering::SeqCst), 0);
        assert_eq!(handle.shared.observation_subscribers.lock().len(), 1);

        emit_test_fact(&handle, BackendEventKind::Block);
        assert_eq!(b_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn runtime_subscriber_queue_stays_bounded_under_unconsumed_high_frequency() {
        let mut config = RunConfig::default();
        config.emit_stdout = false;
        let handle = RuntimeHandle::new(config);
        let mut events = handle.subscribe();
        for _ in 0..10_000 {
            handle.test_drive_event(
                FactSource::ServerObserved,
                valid_observation_payload(BackendEventKind::Entity),
            );
            assert!(
                events.queued_count()
                    <= RUNTIME_BROKER_ORDINARY_CAPACITY
                        + RUNTIME_BROKER_CONTROL_CAPACITY
                        + RUNTIME_BROKER_OVERFLOW_CAPACITY
                        + 1
            );
        }
        handle.test_drive_event(
            FactSource::ServerObserved,
            BackendEventPayload::Chat(ContractProtocolChatEvent {
                sender_username: Some("Alex".to_owned()),
                plain_text: "upstream chat".to_owned(),
                position: Some(ChatPosition::Chat),
                verified: None,
            }),
        );
        let mut saw_chat = false;
        while let Ok(event) = events.try_recv() {
            if matches!(
                event.payload,
                BackendEventPayload::Chat(ContractProtocolChatEvent { ref plain_text, .. })
                    if plain_text == "upstream chat"
            ) {
                saw_chat = true;
            }
        }
        assert!(saw_chat, "bounded upstream must retain admitted chat");
    }

    #[test]
    fn runtime_broker_old_marker_pop_does_not_close_new_loss_segment() {
        let queue = RuntimeEventQueue::new(None);
        let cancel = AtomicBool::new(false);
        let mut writer = EventWriter::new("world");

        for _ in 0..RUNTIME_BROKER_ORDINARY_CAPACITY {
            assert!(queue.publish(
                writer.emit(
                    FactSource::ServerObserved,
                    valid_observation_payload(BackendEventKind::Entity)
                ),
                &cancel,
            ));
        }
        assert!(queue.publish(
            writer.emit(
                FactSource::ServerObserved,
                valid_observation_payload(BackendEventKind::Entity)
            ),
            &cancel,
        ));
        assert!(queue.publish(
            writer.emit(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected),
            ),
            &cancel,
        ));
        for _ in 0..RUNTIME_BROKER_ORDINARY_CAPACITY {
            queue.pop().expect("ordinary broker entry");
        }
        for _ in 0..RUNTIME_BROKER_ORDINARY_CAPACITY {
            assert!(queue.publish(
                writer.emit(
                    FactSource::ServerObserved,
                    valid_observation_payload(BackendEventKind::Entity)
                ),
                &cancel,
            ));
        }
        assert!(queue.publish(
            writer.emit(
                FactSource::ServerObserved,
                valid_observation_payload(BackendEventKind::Entity)
            ),
            &cancel,
        ));
        let first = queue.pop().expect("first broker marker");
        assert!(matches!(
            first.payload,
            BackendEventPayload::Overflow(payload) if payload.dropped_count == 1
        ));

        assert!(queue.publish(
            writer.emit(
                FactSource::ServerObserved,
                valid_observation_payload(BackendEventKind::Entity)
            ),
            &cancel,
        ));
        let control = queue.pop().expect("broker control after first marker");
        assert!(matches!(
            control.payload,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected)
        ));
        for _ in 0..RUNTIME_BROKER_ORDINARY_CAPACITY {
            queue.pop().expect("refilled ordinary broker entry");
        }
        let second = queue.pop().expect("second broker marker");
        assert!(matches!(
            second.payload,
            BackendEventPayload::Overflow(payload) if payload.dropped_count == 2
        ));
    }

    #[test]
    fn runtime_dispatch_old_marker_pop_does_not_close_new_loss_segment() {
        let mut state = EventDispatchState::default();
        let cancel = AtomicBool::new(false);
        let mut writer = EventWriter::new("world");

        for _ in 0..RUNTIME_DISPATCH_ORDINARY_CAPACITY {
            let mut event = Some(writer.emit(
                FactSource::ServerObserved,
                valid_observation_payload(BackendEventKind::Entity),
            ));
            let mut pending = None;
            assert!(matches!(
                state.enqueue(&mut event, &mut pending, &cancel),
                RuntimeDispatchAdmission::Accepted(_)
            ));
        }
        for payload in [
            valid_observation_payload(BackendEventKind::Entity),
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected),
        ] {
            let mut event = Some(writer.emit(FactSource::ServerObserved, payload));
            let mut pending = None;
            assert!(matches!(
                state.enqueue(&mut event, &mut pending, &cancel),
                RuntimeDispatchAdmission::Accepted(_)
            ));
        }

        for _ in 0..RUNTIME_DISPATCH_ORDINARY_CAPACITY {
            state.pop_next().expect("ordinary dispatch entry");
        }
        for _ in 0..RUNTIME_DISPATCH_ORDINARY_CAPACITY {
            let mut event = Some(writer.emit(
                FactSource::ServerObserved,
                valid_observation_payload(BackendEventKind::Entity),
            ));
            let mut pending = None;
            assert!(matches!(
                state.enqueue(&mut event, &mut pending, &cancel),
                RuntimeDispatchAdmission::Accepted(_)
            ));
        }
        let mut event = Some(writer.emit(
            FactSource::ServerObserved,
            valid_observation_payload(BackendEventKind::Entity),
        ));
        let mut pending = None;
        assert!(matches!(
            state.enqueue(&mut event, &mut pending, &cancel),
            RuntimeDispatchAdmission::Accepted(_)
        ));
        let first = state.pop_next().expect("first dispatch marker");
        assert!(matches!(
            first.payload,
            BackendEventPayload::Overflow(payload) if payload.dropped_count == 1
        ));

        let mut event = Some(writer.emit(
            FactSource::ServerObserved,
            valid_observation_payload(BackendEventKind::Entity),
        ));
        let mut pending = None;
        assert!(matches!(
            state.enqueue(&mut event, &mut pending, &cancel),
            RuntimeDispatchAdmission::Accepted(_)
        ));
        let control = state
            .pop_next()
            .expect("dispatch control after first marker");
        assert!(matches!(
            control.payload,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected)
        ));
        for _ in 0..RUNTIME_DISPATCH_ORDINARY_CAPACITY {
            state.pop_next().expect("refilled ordinary dispatch entry");
        }
        let second = state.pop_next().expect("second dispatch marker");
        assert!(matches!(
            second.payload,
            BackendEventPayload::Overflow(payload) if payload.dropped_count == 2
        ));
    }

    #[test]
    fn runtime_dispatch_waiting_ticket_cannot_be_overtaken_by_later_ordinary() {
        let state = Arc::new(parking_lot::Mutex::new(EventDispatchState::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let mut writer = EventWriter::new("world");
        {
            let mut guard = state.lock();
            for _ in 0..RUNTIME_DISPATCH_CONTROL_CAPACITY {
                let mut event = Some(writer.emit(
                    FactSource::ServerObserved,
                    BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected),
                ));
                let mut pending = None;
                assert!(matches!(
                    guard.enqueue(&mut event, &mut pending, &cancel),
                    RuntimeDispatchAdmission::Accepted(_)
                ));
            }
        }

        let first_event = writer.emit(
            FactSource::ServerObserved,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::LoggedIn {
                version: "test".to_owned(),
                dimension: "minecraft:overworld".to_owned(),
            }),
        );
        let later_event = writer.emit(
            FactSource::ServerObserved,
            valid_observation_payload(BackendEventKind::Entity),
        );
        let (first_waited, first_waited_received) = std_mpsc::channel();
        let (later_waited, later_waited_received) = std_mpsc::channel();
        let waiting_barrier = Arc::new(Barrier::new(3));
        let (first_release, first_release_received) = std_mpsc::channel();
        let (later_release, later_release_received) = std_mpsc::channel();
        let (admitted, admitted_received) = std_mpsc::channel();

        let first_state = state.clone();
        let first_cancel = cancel.clone();
        let first_waiting_barrier = waiting_barrier.clone();
        let first_admitted = admitted.clone();
        let first = thread::spawn(move || {
            let mut event = Some(first_event);
            let mut pending = None;
            let mut guard = first_state.lock();
            assert!(matches!(
                guard.enqueue(&mut event, &mut pending, &first_cancel),
                RuntimeDispatchAdmission::Wait
            ));
            first_waited.send(()).expect("first wait gate");
            drop(guard);
            first_waiting_barrier.wait();
            first_release_received
                .recv_timeout(StdDuration::from_secs(1))
                .expect("first admission release");
            let mut guard = first_state.lock();
            assert!(matches!(
                guard.enqueue(&mut event, &mut pending, &first_cancel),
                RuntimeDispatchAdmission::Accepted(_)
            ));
            first_admitted
                .send("control")
                .expect("first admission result");
        });

        first_waited_received
            .recv_timeout(StdDuration::from_secs(1))
            .expect("first control must wait at full control capacity");

        let later_state = state.clone();
        let later_cancel = cancel.clone();
        let later_waiting_barrier = waiting_barrier.clone();
        let later_admitted = admitted.clone();
        let later = thread::spawn(move || {
            let mut event = Some(later_event);
            let mut pending = None;
            let mut guard = later_state.lock();
            assert!(matches!(
                guard.enqueue(&mut event, &mut pending, &later_cancel),
                RuntimeDispatchAdmission::Wait
            ));
            later_waited.send(()).expect("later wait gate");
            drop(guard);
            later_waiting_barrier.wait();
            later_release_received
                .recv_timeout(StdDuration::from_secs(1))
                .expect("later admission release");
            let mut guard = later_state.lock();
            assert!(matches!(
                guard.enqueue(&mut event, &mut pending, &later_cancel),
                RuntimeDispatchAdmission::Accepted(_)
            ));
            later_admitted
                .send("ordinary")
                .expect("later admission result");
        });

        later_waited_received
            .recv_timeout(StdDuration::from_secs(1))
            .expect("later ordinary must wait behind the earlier control ticket");
        assert!(admitted_received.try_recv().is_err());

        {
            let mut guard = state.lock();
            guard.control.pop_front();
        }
        waiting_barrier.wait();
        first_release
            .send(())
            .expect("first admission release sender");
        assert_eq!(
            admitted_received
                .recv_timeout(StdDuration::from_secs(1))
                .expect("first admission after one control slot is released"),
            "control"
        );
        later_release
            .send(())
            .expect("later admission release sender");
        assert_eq!(
            admitted_received
                .recv_timeout(StdDuration::from_secs(1))
                .expect("later admission after the first ticket advances"),
            "ordinary"
        );
        first.join().expect("first waiter");
        later.join().expect("later waiter");
    }

    #[test]
    fn runtime_dispatch_stop_cancels_waiting_ticket_and_advances_turn() {
        let state = Arc::new(parking_lot::Mutex::new(EventDispatchState::default()));
        let wake = Arc::new(parking_lot::Condvar::new());
        let cancel = Arc::new(AtomicBool::new(false));
        let mut writer = EventWriter::new("world");
        {
            let mut guard = state.lock();
            for _ in 0..RUNTIME_DISPATCH_CONTROL_CAPACITY {
                let mut event = Some(writer.emit(
                    FactSource::ServerObserved,
                    BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected),
                ));
                let mut pending = None;
                assert!(matches!(
                    guard.enqueue(&mut event, &mut pending, &cancel),
                    RuntimeDispatchAdmission::Accepted(_)
                ));
            }
        }
        let first_event = writer.emit(
            FactSource::ServerObserved,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::LoggedIn {
                version: "test".to_owned(),
                dimension: "minecraft:overworld".to_owned(),
            }),
        );
        let later_event = writer.emit(
            FactSource::ServerObserved,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Ready {
                snapshot_revision: 1,
            }),
        );
        let (first_waited, first_waited_received) = std_mpsc::channel();
        let (later_waited, later_waited_received) = std_mpsc::channel();
        let (cancelled, cancelled_received) = std_mpsc::channel();
        let cancellation_barrier = Arc::new(Barrier::new(3));
        let first_state = state.clone();
        let first_cancel = cancel.clone();
        let first_cancelled = cancelled.clone();
        let first_cancellation_barrier = cancellation_barrier.clone();
        let first = thread::spawn(move || {
            let mut event = Some(first_event);
            let mut pending = None;
            loop {
                let mut guard = first_state.lock();
                match guard.enqueue(&mut event, &mut pending, &first_cancel) {
                    RuntimeDispatchAdmission::Wait => {
                        let _ = first_waited.send(());
                        drop(guard);
                        first_cancellation_barrier.wait();
                        continue;
                    }
                    RuntimeDispatchAdmission::Cancelled => {
                        first_cancelled.send("first").expect("first cancellation");
                        return;
                    }
                    RuntimeDispatchAdmission::Accepted(_) => {
                        panic!("stop-cancelled first waiter must not admit");
                    }
                }
            }
        });
        first_waited_received
            .recv_timeout(StdDuration::from_secs(1))
            .expect("first stop-cancelled waiter");

        let later_state = state.clone();
        let later_cancel = cancel.clone();
        let later_cancelled = cancelled.clone();
        let later_cancellation_barrier = cancellation_barrier.clone();
        let later = thread::spawn(move || {
            let mut event = Some(later_event);
            let mut pending = None;
            loop {
                let mut guard = later_state.lock();
                match guard.enqueue(&mut event, &mut pending, &later_cancel) {
                    RuntimeDispatchAdmission::Wait => {
                        let _ = later_waited.send(());
                        drop(guard);
                        later_cancellation_barrier.wait();
                        continue;
                    }
                    RuntimeDispatchAdmission::Cancelled => {
                        later_cancelled.send("later").expect("later cancellation");
                        return;
                    }
                    RuntimeDispatchAdmission::Accepted(_) => {
                        panic!("stop-cancelled later waiter must not admit");
                    }
                }
            }
        });
        later_waited_received
            .recv_timeout(StdDuration::from_secs(1))
            .expect("later stop-cancelled waiter");

        cancel.store(true, Ordering::Release);
        cancellation_barrier.wait();
        wake.notify_all();
        let mut cancelled_names = vec![
            cancelled_received
                .recv_timeout(StdDuration::from_secs(1))
                .expect("first waiter cancellation"),
            cancelled_received
                .recv_timeout(StdDuration::from_secs(1))
                .expect("later waiter cancellation"),
        ];
        cancelled_names.sort_unstable();
        assert_eq!(cancelled_names, vec!["first", "later"]);
        first.join().expect("first cancelled waiter");
        later.join().expect("later cancelled waiter");
        let state = state.lock();
        assert_eq!(state.next_admission, state.next_sequence);
    }

    #[test]
    fn runtime_stop_cancellation_is_not_reset_while_connection_admission_waits() {
        let mut config = RunConfig::default();
        config.emit_stdout = false;
        let handle = RuntimeHandle::new(config);

        let (broker_waiting, broker_waiting_received) = std_mpsc::channel();
        handle.test_set_runtime_broker_backpressure_hook(Some(Arc::new(move || {
            broker_waiting
                .send(())
                .expect("runtime broker wait gate should remain live");
        })));
        let mut events = handle.subscribe();
        let cancel = AtomicBool::new(false);
        let mut broker_writer = EventWriter::new("world");
        for _ in 0..RUNTIME_BROKER_CONTROL_CAPACITY {
            assert!(events.queue.publish(
                broker_writer.emit(
                    FactSource::ServerObserved,
                    BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected,),
                ),
                &cancel,
            ));
        }

        for index in 0..RUNTIME_DISPATCH_CONTROL_CAPACITY {
            let should_drain = handle.shared.enqueue_event(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionRequested {
                    attempt: 7,
                }),
            );
            assert_eq!(should_drain, index == 0);
        }
        assert_eq!(
            handle.test_event_dispatch_counts(),
            (0, RUNTIME_DISPATCH_CONTROL_CAPACITY, 0, 0)
        );

        let shared = handle.shared.clone();
        let drainer = thread::spawn(move || {
            shared.drain_events();
        });
        broker_waiting_received
            .recv_timeout(StdDuration::from_secs(1))
            .expect("runtime worker drainer must block at the full broker");
        assert!(!handle.shared.enqueue_event(
            FactSource::ServerObserved,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionRequested {
                attempt: 8
            },),
        ));
        assert_eq!(
            handle.test_event_dispatch_counts(),
            (0, RUNTIME_DISPATCH_CONTROL_CAPACITY, 0, 0)
        );

        let (dispatch_waiting, dispatch_waiting_received) = std_mpsc::channel();
        handle.test_set_event_dispatch_backpressure_hook(Some(Arc::new(move || {
            dispatch_waiting
                .send(())
                .expect("connection admission wait gate should remain live");
        })));
        let connection_shared = handle.shared.clone();
        let (connection_result, connection_result_received) = std_mpsc::channel();
        let (release_command, release_command_received) = std_mpsc::channel();
        let connection = thread::spawn(move || {
            let admission = connection_shared.command_admission.lock();
            let result = connection_shared.begin_connection_attempt_locked(None);
            connection_result
                .send(result)
                .expect("connection admission result should be observable");
            release_command_received
                .recv_timeout(StdDuration::from_secs(1))
                .expect("test must release the command admission");
            drop(admission);
        });
        dispatch_waiting_received
            .recv_timeout(StdDuration::from_secs(1))
            .expect("connection attempt must wait in internal dispatch admission");

        let (stop_signaled, stop_signaled_received) = std_mpsc::channel();
        handle.test_set_stop_signal_hook(Some(Arc::new(move || {
            stop_signaled
                .send(())
                .expect("stop signal gate should remain live");
        })));
        let stop_handle = handle.clone();
        let (stop_finished, stop_finished_received) = std_mpsc::channel();
        let stopper = thread::spawn(move || {
            stop_handle.stop("connection-admission-stop");
            stop_finished
                .send(())
                .expect("stop completion should be observable");
        });
        stop_signaled_received
            .recv_timeout(StdDuration::from_secs(1))
            .expect("stop must publish its monotonic cancellation before command lock");
        assert!(handle.shared.stop_requested.load(Ordering::Acquire));
        assert!(handle.shared.dispatch_cancelled.load(Ordering::Acquire));
        assert!(!handle.shared.shutdown_requested.load(Ordering::Acquire));

        assert_eq!(
            connection_result_received
                .recv_timeout(StdDuration::from_secs(1))
                .expect("connection admission must cancel while stop waits for its lock"),
            Some(false)
        );
        release_command
            .send(())
            .expect("connection command admission release");
        connection.join().expect("connection waiter");
        stop_finished_received
            .recv_timeout(StdDuration::from_secs(1))
            .expect("stop must finish after the cancelled admission releases");
        stopper.join().expect("stopper");
        drainer.join().expect("runtime drainer");

        let mut stopped_count = 0;
        while let Ok(event) = events.try_recv() {
            if matches!(
                event.payload,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::Stopped { .. })
            ) {
                stopped_count += 1;
            }
        }
        assert_eq!(
            stopped_count, 1,
            "stop must settle exactly one terminal event"
        );
        assert!(matches!(handle.state(), BackendState::Stopped { .. }));
    }

    #[test]
    fn runtime_dropped_receiver_is_pruned_before_control_backpressure() {
        let mut config = RunConfig::default();
        config.emit_stdout = false;
        let handle = RuntimeHandle::new(config);
        let dropped = handle.subscribe();
        drop(dropped);

        let producer_handle = handle.clone();
        let (finished, finished_received) = std_mpsc::channel();
        let producer = thread::spawn(move || {
            for attempt in 0..(RUNTIME_BROKER_CONTROL_CAPACITY * 2) {
                producer_handle.test_drive_event(
                    FactSource::ServerObserved,
                    BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionRequested {
                        attempt: attempt as u32,
                    }),
                );
            }
            finished.send(()).expect("dropped receiver producer result");
        });
        finished_received
            .recv_timeout(StdDuration::from_secs(1))
            .expect("dropped receiver must not create control backpressure");
        producer.join().expect("dropped receiver producer");
        assert_eq!(handle.shared.subscribers.lock().len(), 0);
    }

    struct ImmediateBlockObservationReader {
        source: RuntimeObservationSource,
        seen: Arc<parking_lot::Mutex<Vec<(Option<u32>, u32, Option<u32>)>>>,
    }

    impl ObservationEventListener for ImmediateBlockObservationReader {
        fn on_event(&self, event: ObservationEvent) {
            let ObservationEvent::Block(envelope) = event else {
                return;
            };
            let ContractProtocolBlockEvent::Updated {
                old_block,
                new_block,
            } = envelope.payload
            else {
                return;
            };
            let new_block = new_block.expect("accepted block update has a new block");
            let position = ContractBlockPosition {
                x: new_block.position.x,
                y: new_block.position.y,
                z: new_block.position.z,
            };
            let read_state_id = match self
                .source
                .read_block(position)
                .expect("callback block read")
            {
                ContractBlockReadResult::Loaded { block } => Some(block.state_id),
                ContractBlockReadResult::Unloaded => None,
                other => panic!("callback block read left the world height, got {other:?}"),
            };
            self.seen.lock().push((
                old_block.map(|block| block.state_id),
                new_block.state_id,
                read_state_id,
            ));
        }
    }

    fn producer_test_app() -> (
        RuntimeHandle,
        App,
        bevy_ecs::entity::Entity,
        SharedWorld,
        RuntimeObservationSource,
        RuntimeEventReceiver,
    ) {
        let (handle, app, owner, shared_world, source, events) = producer_test_app_without_world();
        assert!(handle.shared.set_world_if_running(shared_world.clone()));
        (handle, app, owner, shared_world, source, events)
    }

    fn producer_test_app_without_world() -> (
        RuntimeHandle,
        App,
        bevy_ecs::entity::Entity,
        SharedWorld,
        RuntimeObservationSource,
        RuntimeEventReceiver,
    ) {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut app = App::new();
        app.add_message::<azalea::packet::game::ReceiveGamePacketEvent>();
        app.add_message::<azalea::chunks::ReceiveChunkEvent>();
        let owner = app
            .world_mut()
            .spawn((LocalEntity, azalea::core::entity_id::MinecraftEntityId(99)))
            .id();
        let shared_world = empty_world();
        app.world_mut().entity_mut(owner).insert((
            azalea::local_player::WorldHolder::new(owner, shared_world.clone()),
            azalea::block_update::QueuedServerBlockUpdates::default(),
            azalea::interact::BlockStatePredictionHandler::default(),
            CanonicalPacketSourceMetadata::default(),
        ));
        app.insert_resource(SwarmState {
            shared: handle.shared.clone(),
        });
        app.add_systems(azalea::app::PreUpdate, produce_entity_packet_events);
        app.add_systems(
            Update,
            (
                azalea::chunks::handle_receive_chunk_event,
                azalea::block_update::handle_block_update_event,
            ),
        );
        app.add_plugins(BlockSoundProducerPlugin);

        assert!(handle.shared.begin_connection_attempt());
        let test_token = synthetic_attempt_token();
        assert!(handle
            .shared
            .admit_canonical_join_started_with_token(1, Some(test_token)));
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind_with_token(owner, Some(test_token)),
            Some(1)
        );
        app.world_mut()
            .insert_resource(super::entity_events_owner_tests::TestAttemptToken(
                test_token,
            ));
        let source = handle.observation_source();
        let events = handle.subscribe();
        (handle, app, owner, shared_world, source, events)
    }

    fn test_block_state(id: u32) -> azalea::block::BlockState {
        azalea::block::BlockState::try_from(id).expect("test block state id")
    }

    fn synthetic_attempt_token() -> azalea::join::AttemptToken {
        azalea::join::AttemptToken::mint()
    }

    fn install_shared_chunk(
        shared_world: &SharedWorld,
        pos: azalea::core::position::ChunkPos,
    ) -> Arc<parking_lot::RwLock<azalea::world::Chunk>> {
        shared_world
            .write()
            .chunks
            .upsert(pos, azalea::world::Chunk::default())
    }

    fn expose_shared_chunk(
        app: &mut App,
        owner: bevy_ecs::entity::Entity,
        pos: azalea::core::position::ChunkPos,
        chunk: Arc<parking_lot::RwLock<azalea::world::Chunk>>,
    ) {
        let holder = app
            .world_mut()
            .get::<azalea::local_player::WorldHolder>(owner)
            .expect("test world holder")
            .clone();
        holder.partial.write().chunks.limited_set(&pos, Some(chunk));
    }

    fn queue_production_block_packet(
        app: &mut App,
        owner: bevy_ecs::entity::Entity,
        position: azalea::BlockPos,
        state: azalea::block::BlockState,
    ) {
        let packet = azalea::protocol::packets::game::ClientboundGamePacket::BlockUpdate(
            azalea::protocol::packets::game::ClientboundBlockUpdate {
                pos: position,
                block_state: state,
            },
        );
        azalea::packet::game::process_packet(
            app.world_mut(),
            owner,
            &packet,
            synthetic_attempt_token(),
        );
        queue_producer_packet(app, owner, packet);
    }

    fn queue_producer_packet(
        app: &mut App,
        owner: bevy_ecs::entity::Entity,
        packet: azalea::protocol::packets::game::ClientboundGamePacket,
    ) {
        let attempt_token = app
            .world()
            .resource::<super::entity_events_owner_tests::TestAttemptToken>()
            .0;
        app.world_mut()
            .write_message(azalea::packet::game::ReceiveGamePacketEvent {
                entity: owner,
                packet: Arc::new(packet),
                attempt_token,
            });
    }

    fn block_events(events: &mut RuntimeEventReceiver) -> Vec<ContractProtocolBlockEvent> {
        std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event.payload {
                BackendEventPayload::Block(payload) => Some(payload),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn production_block_sound_updates_are_ordered_post_state_and_preserve_null_old() {
        let (handle, mut app, owner, shared_world, source, mut events) = producer_test_app();
        let position = azalea::BlockPos {
            x: -17,
            y: -46,
            z: 30,
        };
        let chunk_pos = azalea::core::position::ChunkPos::new(-2, 1);
        let chunk = install_shared_chunk(&shared_world, chunk_pos);
        expose_shared_chunk(&mut app, owner, chunk_pos, chunk);

        let state_a = test_block_state(1);
        let state_b = test_block_state(2);
        let state_c = test_block_state(3);
        shared_world
            .read()
            .chunks
            .set_block_state(position, state_a);

        let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let _subscription = ProtocolObservationSource::subscribe(
            &source,
            Arc::new(ImmediateBlockObservationReader {
                source: source.clone(),
                seen: seen.clone(),
            }),
        )
        .expect("block callback subscription");

        queue_production_block_packet(&mut app, owner, position, state_b);
        queue_production_block_packet(&mut app, owner, position, state_c);
        app.update();
        assert_eq!(
            *seen.lock(),
            vec![
                (
                    Some(u32::from(state_a.id())),
                    u32::from(state_b.id()),
                    Some(u32::from(state_b.id())),
                ),
                (
                    Some(u32::from(state_b.id())),
                    u32::from(state_c.id()),
                    Some(u32::from(state_c.id())),
                ),
            ]
        );
        queue_production_block_packet(
            &mut app,
            owner,
            azalea::BlockPos {
                x: 145,
                y: -46,
                z: 145,
            },
            test_block_state(4),
        );
        app.update();

        assert_eq!(
            *seen.lock(),
            vec![
                (
                    Some(u32::from(state_a.id())),
                    u32::from(state_b.id()),
                    Some(u32::from(state_b.id())),
                ),
                (
                    Some(u32::from(state_b.id())),
                    u32::from(state_c.id()),
                    Some(u32::from(state_c.id())),
                ),
                (None, 4, None),
            ]
        );
        let updates = block_events(&mut events)
            .into_iter()
            .filter_map(|event| match event {
                ContractProtocolBlockEvent::Updated {
                    old_block,
                    new_block,
                } => Some((old_block, new_block.expect("new block"))),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(updates.len(), 3);
        assert_eq!(
            updates[0].0.as_ref().expect("full old block").position,
            ContractBlockPosition {
                x: -17,
                y: -46,
                z: 30,
            }
        );
        assert_eq!(
            updates[0].1.position,
            updates[0].0.as_ref().unwrap().position
        );
        assert_eq!(updates[0].1.state_id, u32::from(state_b.id()));
        assert!(!updates[0].1.name.is_empty());
        assert!(
            updates[0].1.bounding_box == ContractBlockBoundingBox::Block
                || updates[0].1.bounding_box == ContractBlockBoundingBox::Empty
        );
        assert!(
            updates[2].0.is_none(),
            "unloaded oldBlock must be JSON null"
        );
        assert_eq!(handle.connection_epoch(), 1);
    }

    #[test]
    fn production_block_sound_section_updates_flatten_in_wire_order_with_negative_section() {
        let (_handle, mut app, owner, shared_world, _source, mut events) = producer_test_app();
        let chunk_pos = azalea::core::position::ChunkPos::new(-2, 1);
        let chunk = install_shared_chunk(&shared_world, chunk_pos);
        expose_shared_chunk(&mut app, owner, chunk_pos, chunk);
        let packet = azalea::protocol::packets::game::ClientboundGamePacket::SectionBlocksUpdate(
            azalea::protocol::packets::game::ClientboundSectionBlocksUpdate {
                section_pos: azalea::core::position::ChunkSectionPos { x: -2, y: -3, z: 1 },
                states: vec![
                    azalea::protocol::packets::game::c_section_blocks_update::BlockStateWithPosition {
                        pos: azalea::core::position::ChunkSectionBlockPos { x: 1, y: 2, z: 3 },
                        state: test_block_state(5),
                    },
                    azalea::protocol::packets::game::c_section_blocks_update::BlockStateWithPosition {
                        pos: azalea::core::position::ChunkSectionBlockPos { x: 15, y: 0, z: 0 },
                        state: test_block_state(6),
                    },
                ],
            },
        );
        azalea::packet::game::process_packet(
            app.world_mut(),
            owner,
            &packet,
            synthetic_attempt_token(),
        );
        queue_producer_packet(&mut app, owner, packet);
        app.update();

        let updates = block_events(&mut events)
            .into_iter()
            .filter_map(|event| match event {
                ContractProtocolBlockEvent::Updated { new_block, .. } => {
                    Some(new_block.expect("section new block"))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(updates.len(), 2);
        assert_eq!(
            updates
                .iter()
                .map(|block| (
                    block.position.x,
                    block.position.y,
                    block.position.z,
                    block.state_id
                ))
                .collect::<Vec<_>>(),
            vec![(-31, -46, 19, 5), (-17, -48, 16, 6)]
        );
    }

    fn sound_packet(
        sound: azalea::registry::Holder<
            azalea::registry::builtin::SoundEvent,
            azalea::core::sound::CustomSound,
        >,
        volume: f32,
        pitch: f32,
    ) -> azalea::protocol::packets::game::ClientboundGamePacket {
        azalea::protocol::packets::game::ClientboundGamePacket::Sound(
            azalea::protocol::packets::game::ClientboundSound {
                sound,
                source: azalea::protocol::packets::game::c_sound::SoundSource::Master,
                x: 9,
                y: -8,
                z: 16,
                volume,
                pitch,
                seed: 42,
            },
        )
    }

    fn empty_chunk_packet(
        x: i32,
        z: i32,
    ) -> azalea::protocol::packets::game::ClientboundGamePacket {
        azalea::protocol::packets::game::ClientboundGamePacket::LevelChunkWithLight(
            azalea::protocol::packets::game::ClientboundLevelChunkWithLight {
                x,
                z,
                chunk_data: azalea::protocol::packets::game::c_level_chunk_with_light::ClientboundLevelChunkPacketData {
                    heightmaps: Vec::new(),
                    data: Arc::new(Vec::<u8>::new().into_boxed_slice()),
                    block_entities: Vec::new(),
                },
                light_data: Default::default(),
            },
        )
    }

    fn packed_light_layer(values: &[(usize, u8)]) -> Box<[u8]> {
        let mut bytes = vec![0; 2048];
        for &(index, value) in values {
            assert!(index < 4096);
            assert!(value <= 15);
            let byte = &mut bytes[index >> 1];
            if index & 1 == 0 {
                *byte = (*byte & 0xf0) | value;
            } else {
                *byte = (*byte & 0x0f) | (value << 4);
            }
        }
        bytes.into_boxed_slice()
    }

    fn light_data_with_masks(
        sky_bits: &[usize],
        block_bits: &[usize],
        empty_sky_bits: &[usize],
        empty_block_bits: &[usize],
        sky_updates: Vec<Box<[u8]>>,
        block_updates: Vec<Box<[u8]>>,
    ) -> azalea::protocol::packets::game::c_light_update::ClientboundLightUpdatePacketData {
        let mut sky_y_mask = azalea::core::bitset::BitSet::new(64);
        for &bit in sky_bits {
            sky_y_mask.set(bit);
        }
        let mut block_y_mask = azalea::core::bitset::BitSet::new(64);
        for &bit in block_bits {
            block_y_mask.set(bit);
        }
        let mut empty_sky_y_mask = azalea::core::bitset::BitSet::new(64);
        for &bit in empty_sky_bits {
            empty_sky_y_mask.set(bit);
        }
        let mut empty_block_y_mask = azalea::core::bitset::BitSet::new(64);
        for &bit in empty_block_bits {
            empty_block_y_mask.set(bit);
        }
        azalea::protocol::packets::game::c_light_update::ClientboundLightUpdatePacketData {
            sky_y_mask,
            block_y_mask,
            empty_sky_y_mask,
            empty_block_y_mask,
            sky_updates: Arc::new(sky_updates.into_boxed_slice()),
            block_updates: Arc::new(block_updates.into_boxed_slice()),
        }
    }

    fn light_chunk_packet(
        x: i32,
        z: i32,
        light_data: azalea::protocol::packets::game::c_light_update::ClientboundLightUpdatePacketData,
    ) -> azalea::protocol::packets::game::ClientboundGamePacket {
        azalea::protocol::packets::game::ClientboundGamePacket::LevelChunkWithLight(
            azalea::protocol::packets::game::ClientboundLevelChunkWithLight {
                x,
                z,
                chunk_data: azalea::protocol::packets::game::c_level_chunk_with_light::ClientboundLevelChunkPacketData {
                    heightmaps: Vec::new(),
                    data: Arc::new(Vec::<u8>::new().into_boxed_slice()),
                    block_entities: Vec::new(),
                },
                light_data,
            },
        )
    }

    fn light_update_packet(
        x: i32,
        z: i32,
        light_data: azalea::protocol::packets::game::c_light_update::ClientboundLightUpdatePacketData,
    ) -> azalea::protocol::packets::game::ClientboundGamePacket {
        azalea::protocol::packets::game::ClientboundGamePacket::LightUpdate(
            azalea::protocol::packets::game::ClientboundLightUpdate { x, z, light_data },
        )
    }

    fn attributes_packet(
        entity_id: i32,
        values: Vec<azalea::protocol::packets::game::c_update_attributes::AttributeSnapshot>,
    ) -> azalea::protocol::packets::game::ClientboundGamePacket {
        azalea::protocol::packets::game::ClientboundGamePacket::UpdateAttributes(
            azalea::protocol::packets::game::ClientboundUpdateAttributes {
                entity_id: azalea::core::entity_id::MinecraftEntityId(entity_id),
                values,
            },
        )
    }

    fn armor_snapshot(
        attribute: azalea::registry::builtin::Attribute,
        base: f64,
    ) -> azalea::protocol::packets::game::c_update_attributes::AttributeSnapshot {
        azalea::protocol::packets::game::c_update_attributes::AttributeSnapshot {
            attribute,
            base,
            modifiers: Vec::new(),
        }
    }

    fn armor_snapshot_with_modifiers(
        base: f64,
        modifiers: Vec<azalea::inventory::components::AttributeModifier>,
    ) -> azalea::protocol::packets::game::c_update_attributes::AttributeSnapshot {
        azalea::protocol::packets::game::c_update_attributes::AttributeSnapshot {
            attribute: azalea::registry::builtin::Attribute::Armor,
            base,
            modifiers,
        }
    }

    #[derive(Clone, Copy)]
    struct TestArmorModifier {
        id: u8,
        amount: f64,
        operation: azalea::core::attribute_modifier_operation::AttributeModifierOperation,
    }

    fn test_armor_value(base: f64, modifiers: &[TestArmorModifier]) -> Option<u8> {
        calculate_armor_values(
            base,
            modifiers,
            |modifier| modifier.id,
            |modifier| modifier.amount,
            |modifier| modifier.operation,
        )
    }

    #[test]
    fn armor_formula_groups_operations_and_fails_closed_for_bad_values() {
        use azalea::core::attribute_modifier_operation::AttributeModifierOperation as Op;

        let modifiers = [
            TestArmorModifier {
                id: 3,
                amount: 0.25,
                operation: Op::AddMultipliedTotal,
            },
            TestArmorModifier {
                id: 2,
                amount: 0.5,
                operation: Op::AddMultipliedBase,
            },
            TestArmorModifier {
                id: 1,
                amount: 2.0,
                operation: Op::AddValue,
            },
        ];
        // d1 = 4 + 2 = 6; d3 = 6 + 6*0.5 = 9; d3 *= 1.25 = 11.25.
        assert_eq!(test_armor_value(4.0, &modifiers), Some(11));

        let duplicate_id = [
            TestArmorModifier {
                id: 1,
                amount: 1.0,
                operation: Op::AddValue,
            },
            TestArmorModifier {
                id: 1,
                amount: 3.0,
                operation: Op::AddValue,
            },
        ];
        assert_eq!(test_armor_value(10.0, &duplicate_id), Some(13));

        assert_eq!(test_armor_value(-5.0, &[]), Some(0));
        assert_eq!(test_armor_value(30.0, &[]), Some(20));
        assert_eq!(test_armor_value(0.0, &[]), Some(0));
        assert_eq!(test_armor_value(f64::NAN, &[]), None);
        assert_eq!(
            test_armor_value(
                1.0,
                &[TestArmorModifier {
                    id: 1,
                    amount: f64::INFINITY,
                    operation: Op::AddValue,
                }],
            ),
            None
        );
        assert_eq!(
            test_armor_value(
                f64::MAX,
                &[TestArmorModifier {
                    id: 1,
                    amount: f64::MAX,
                    operation: Op::AddValue,
                }],
            ),
            None
        );
        assert_eq!(
            test_armor_value(
                f64::MAX,
                &[TestArmorModifier {
                    id: 1,
                    amount: f64::MAX,
                    operation: Op::AddMultipliedTotal,
                }],
            ),
            None
        );
    }

    #[test]
    fn production_armor_reducer_is_local_ordered_and_epoch_bound() {
        let (handle, mut app, owner, _shared_world, _source, _events) = producer_test_app();

        queue_producer_packet(
            &mut app,
            owner,
            attributes_packet(
                98,
                vec![armor_snapshot(
                    azalea::registry::builtin::Attribute::Armor,
                    19.0,
                )],
            ),
        );
        app.update();
        assert_eq!(handle.shared.observation.read().armor, None);

        queue_producer_packet(
            &mut app,
            owner,
            attributes_packet(
                99,
                vec![
                    armor_snapshot(azalea::registry::builtin::Attribute::Armor, 4.0),
                    armor_snapshot(azalea::registry::builtin::Attribute::MaxHealth, 100.0),
                    armor_snapshot(azalea::registry::builtin::Attribute::Armor, 7.0),
                ],
            ),
        );
        app.update();
        assert_eq!(handle.shared.observation.read().armor, Some(7));

        queue_producer_packet(
            &mut app,
            owner,
            attributes_packet(
                99,
                vec![armor_snapshot(
                    azalea::registry::builtin::Attribute::MaxHealth,
                    100.0,
                )],
            ),
        );
        app.update();
        assert_eq!(handle.shared.observation.read().armor, Some(7));

        use azalea::core::attribute_modifier_operation::AttributeModifierOperation as Op;
        let duplicate_modifier_id = azalea::Identifier::from("test:duplicate");
        let grouped_packet = attributes_packet(
            99,
            vec![armor_snapshot_with_modifiers(
                10.0,
                vec![
                    azalea::inventory::components::AttributeModifier {
                        id: duplicate_modifier_id.clone(),
                        amount: 100.0,
                        operation: Op::AddValue,
                    },
                    azalea::inventory::components::AttributeModifier {
                        id: duplicate_modifier_id,
                        amount: 1.0,
                        operation: Op::AddValue,
                    },
                    azalea::inventory::components::AttributeModifier {
                        id: azalea::Identifier::from("test:base"),
                        amount: 0.5,
                        operation: Op::AddMultipliedBase,
                    },
                    azalea::inventory::components::AttributeModifier {
                        id: azalea::Identifier::from("test:total"),
                        amount: 0.1,
                        operation: Op::AddMultipliedTotal,
                    },
                ],
            )],
        );
        queue_producer_packet(&mut app, owner, grouped_packet);
        app.update();
        // The duplicate ID uses the last entry (1.0), then d1=11,
        // d3=16.5, and finally d3*=1.1 -> floor 18.
        assert_eq!(handle.shared.observation.read().armor, Some(18));

        queue_producer_packet(
            &mut app,
            owner,
            attributes_packet(
                99,
                vec![armor_snapshot_with_modifiers(
                    10.0,
                    vec![azalea::inventory::components::AttributeModifier {
                        id: azalea::Identifier::from("test:infinite"),
                        amount: f64::INFINITY,
                        operation: Op::AddMultipliedTotal,
                    }],
                )],
            ),
        );
        app.update();
        assert_eq!(handle.shared.observation.read().armor, None);

        for (base, expected) in [(-2.0, Some(0)), (25.0, Some(20)), (0.0, Some(0))] {
            queue_producer_packet(
                &mut app,
                owner,
                attributes_packet(
                    99,
                    vec![armor_snapshot(
                        azalea::registry::builtin::Attribute::Armor,
                        base,
                    )],
                ),
            );
            app.update();
            assert_eq!(handle.shared.observation.read().armor, expected);
        }

        queue_producer_packet(
            &mut app,
            owner,
            attributes_packet(
                99,
                vec![armor_snapshot(
                    azalea::registry::builtin::Attribute::Armor,
                    f64::NAN,
                )],
            ),
        );
        app.update();
        assert_eq!(handle.shared.observation.read().armor, None);

        assert!(handle
            .shared
            .reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
                owner,
                1,
                Some("minecraft:overworld".to_owned()),
                Some(true),
            ));
        assert_eq!(handle.shared.observation.read().armor, None);

        queue_producer_packet(
            &mut app,
            owner,
            attributes_packet(
                99,
                vec![armor_snapshot(
                    azalea::registry::builtin::Attribute::Armor,
                    6.0,
                )],
            ),
        );
        app.update();
        assert_eq!(handle.shared.observation.read().armor, Some(6));

        assert!(handle.shared.begin_connection_attempt());
        assert_eq!(handle.shared.observation.read().armor, None);
    }

    #[test]
    fn production_light_survives_first_world_attach_and_same_epoch_respawn_scope() {
        let (handle, mut app, owner, shared_world, _source, _events) =
            producer_test_app_without_world();
        let index = (3 << 8) | (2 << 4) | 1;
        let first_light = light_data_with_masks(
            &[1],
            &[1],
            &[],
            &[],
            vec![packed_light_layer(&[(index, 11)])],
            vec![packed_light_layer(&[(index, 4)])],
        );

        // This is the Login boundary equivalent used by the raw reducer:
        // it establishes the packet scope before the first chunk light packet.
        assert!(handle
            .shared
            .reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
                owner,
                1,
                Some("minecraft:overworld".to_owned()),
                Some(true),
            ));
        let generation_after_scope_reset = handle.shared.observation.read().generation;
        let first_chunk = light_chunk_packet(0, 0, first_light);
        azalea::packet::game::process_packet(
            app.world_mut(),
            owner,
            &first_chunk,
            synthetic_attempt_token(),
        );
        queue_producer_packet(&mut app, owner, first_chunk);
        app.update();

        assert_eq!(
            handle
                .shared
                .observation
                .read()
                .light_cache
                .context
                .as_ref()
                .and_then(|context| context.has_skylight),
            Some(true)
        );
        assert_eq!(
            handle.shared.observation.read().generation,
            generation_after_scope_reset + 1,
            "scope reset and full chunk apply must each publish a generation"
        );

        // The first Event::Spawn attaches the WorldHolder after the raw Login
        // and chunk packet. The existing implementation cleared the cache here.
        assert!(handle.shared.set_world_if_running(shared_world.clone()));
        let mut first_snapshot = snapshot_at(1, 1.25, -61.0, 2.75);
        first_snapshot.world.dimension = "minecraft:overworld".to_owned();
        install_viewport_observation(
            &handle,
            first_snapshot,
            FactSource::ServerObserved,
            Vec::new(),
            shared_world.clone(),
        );
        handle.shared.observation.write().snapshot_scope_generation =
            handle.shared.entity_producer.lock().scope_generation;
        let first_facts = handle
            .capture_frame_facts()
            .expect("first attached world should have a snapshot");
        assert_eq!(first_facts.light, Some(11));
        assert_eq!(
            handle
                .shared
                .observation
                .read()
                .light_cache
                .context
                .as_ref()
                .and_then(|context| context.has_skylight),
            Some(true)
        );

        // Same-epoch Respawn resets the light scope but preserves armor. A new
        // chunk in the new scope must likewise survive its first world attach.
        {
            let mut observation = handle.shared.observation.write();
            observation.armor = Some(7);
            observation.armor_epoch = Some(1);
            observation.bump_generation();
        }
        assert!(handle
            .shared
            .reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
                owner,
                1,
                Some("minecraft:the_nether".to_owned()),
                Some(true),
            ));
        {
            let observation = handle.shared.observation.read();
            assert!(observation.snapshot.is_none());
            assert!(observation.light_cache.chunks.is_empty());
            assert_eq!(observation.armor, Some(7));
            assert_eq!(observation.armor_epoch, Some(1));
        }
        let stale_attach_world = empty_world();
        assert!(handle
            .shared
            .set_world_if_running(stale_attach_world.clone()));
        assert!(handle
            .shared
            .observation
            .read()
            .light_cache
            .chunks
            .is_empty());
        assert!(handle
            .shared
            .reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
                owner,
                1,
                Some("minecraft:the_nether".to_owned()),
                Some(true),
            ));

        let second_index = 0;
        let second_light = light_data_with_masks(
            &[1],
            &[1],
            &[],
            &[],
            vec![packed_light_layer(&[(second_index, 9)])],
            vec![packed_light_layer(&[(second_index, 3)])],
        );
        let second_chunk = light_chunk_packet(0, 0, second_light);
        assert_eq!(
            LightSectionGeometry::from_world(&shared_world.read()),
            Some(LightSectionGeometry {
                min_light_section: -5,
                light_section_count: 26,
            })
        );
        assert!(handle.shared.admit_canonical_source(owner).is_some());
        let generation_before_second_light = handle.shared.observation.read().generation;
        azalea::packet::game::process_packet(
            app.world_mut(),
            owner,
            &second_chunk,
            synthetic_attempt_token(),
        );
        queue_producer_packet(&mut app, owner, second_chunk);
        app.update();
        assert_eq!(
            handle.shared.observation.read().generation,
            generation_before_second_light + 1
        );

        {
            let observation = handle.shared.observation.read();
            assert_eq!(
                observation.light_cache.context.as_ref().map(|context| (
                    &context.dimension,
                    context.scope_generation,
                    context.has_skylight
                )),
                Some((
                    &"minecraft:the_nether".to_owned(),
                    handle.shared.entity_producer.lock().scope_generation,
                    Some(true)
                ))
            );
            assert_eq!(
                observation
                    .light_cache
                    .chunks
                    .get(&(0, 0))
                    .and_then(|chunk| chunk.sky.get(1))
                    .and_then(Option::as_ref)
                    .map(|layer| layer[0]),
                Some(9)
            );
            assert_eq!(
                observation.light_cache.value_at(
                    &Vec3Value {
                        x: 0.25,
                        y: -64.0,
                        z: 0.25,
                    },
                    1,
                    handle.shared.entity_producer.lock().scope_generation,
                    "minecraft:the_nether",
                ),
                Some(9)
            );
        }

        let second_world = empty_world();
        assert!(handle.shared.set_world_if_running(second_world.clone()));
        let mut second_snapshot = snapshot_at(1, 0.25, -64.0, 0.25);
        second_snapshot.world.dimension = "minecraft:the_nether".to_owned();
        install_viewport_observation(
            &handle,
            second_snapshot,
            FactSource::ServerObserved,
            Vec::new(),
            second_world,
        );
        handle.shared.observation.write().snapshot_scope_generation =
            handle.shared.entity_producer.lock().scope_generation;
        let second_facts = handle
            .capture_frame_facts()
            .expect("respawned scope should have a snapshot");
        assert_eq!(second_facts.light, Some(9));
        assert_eq!(second_facts.armor, Some(7));
    }

    #[test]
    fn production_raw_scope_order_resets_old_light_keeps_armor_and_forgets_chunks() {
        let (handle, mut app, owner, shared_world, _source, _events) = producer_test_app();
        assert!(handle
            .shared
            .reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
                owner,
                1,
                Some("minecraft:overworld".to_owned()),
                Some(true),
            ));

        let old_chunk = light_chunk_packet(
            0,
            0,
            light_data_with_masks(
                &[1],
                &[1],
                &[],
                &[],
                vec![packed_light_layer(&[(0, 5)])],
                vec![packed_light_layer(&[(0, 2)])],
            ),
        );
        let armor = attributes_packet(
            99,
            vec![armor_snapshot(
                azalea::registry::builtin::Attribute::Armor,
                8.0,
            )],
        );
        install_dimension_registry(&shared_world, "minecraft:the_nether", Some(false));
        let respawn = reducer_respawn_packet("minecraft:the_nether");
        let new_chunk = light_chunk_packet(
            1,
            0,
            light_data_with_masks(
                &[1],
                &[1],
                &[],
                &[],
                vec![packed_light_layer(&[(0, 7)])],
                vec![packed_light_layer(&[(0, 3)])],
            ),
        );
        let partial_update = light_update_packet(
            1,
            0,
            light_data_with_masks(
                &[1],
                &[1],
                &[],
                &[],
                vec![packed_light_layer(&[(0, 12)])],
                vec![packed_light_layer(&[(0, 12)])],
            ),
        );

        for packet in [old_chunk, armor, respawn, new_chunk, partial_update] {
            if let azalea::protocol::packets::game::ClientboundGamePacket::LevelChunkWithLight(_) =
                &packet
            {
                azalea::packet::game::process_packet(
                    app.world_mut(),
                    owner,
                    &packet,
                    synthetic_attempt_token(),
                );
            }
            queue_producer_packet(&mut app, owner, packet);
        }
        app.update();

        let observation = handle.shared.observation.read();
        let context = observation
            .light_cache
            .context
            .as_ref()
            .expect("post-respawn light context");
        assert_eq!(context.dimension, "minecraft:the_nether");
        assert_eq!(context.has_skylight, Some(false));
        assert!(!observation.light_cache.chunks.contains_key(&(0, 0)));
        assert_eq!(observation.armor, Some(8));
        drop(observation);

        let new_world = empty_world();
        assert!(handle.shared.set_world_if_running(new_world.clone()));
        let mut snapshot = snapshot_at(1, 16.25, -64.0, 0.25);
        snapshot.world.dimension = "minecraft:the_nether".to_owned();
        install_viewport_observation(
            &handle,
            snapshot,
            FactSource::ServerObserved,
            Vec::new(),
            new_world,
        );
        let facts = handle
            .capture_frame_facts()
            .expect("new scope snapshot should capture");
        assert_eq!(facts.light, Some(12));
        assert_eq!(facts.armor, Some(8));

        let forget = azalea::protocol::packets::game::ClientboundGamePacket::ForgetLevelChunk(
            azalea::protocol::packets::game::ClientboundForgetLevelChunk {
                pos: azalea::core::position::ChunkPos::new(1, 0),
            },
        );
        azalea::packet::game::process_packet(
            app.world_mut(),
            owner,
            &forget,
            synthetic_attempt_token(),
        );
        queue_producer_packet(&mut app, owner, forget);
        app.update();
        assert!(handle
            .shared
            .observation
            .read()
            .light_cache
            .chunks
            .is_empty());
        assert_eq!(
            handle
                .capture_frame_facts()
                .expect("snapshot remains")
                .light,
            None
        );

        // A new connection epoch physically clears the frame cache and cannot
        // expose either value from the old owner, even before a new snapshot.
        assert!(handle.shared.begin_connection_attempt());
        let observation = handle.shared.observation.read();
        assert!(observation.light_cache.chunks.is_empty());
        assert_eq!(observation.armor, None);
        assert!(observation.snapshot.is_none());
        drop(shared_world);
    }

    #[test]
    fn light_cache_mutations_and_scope_resets_share_observation_generation() {
        let (handle, mut app, owner, shared_world, _source, _events) =
            producer_test_app_without_world();
        assert!(handle
            .shared
            .reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
                owner,
                1,
                Some("minecraft:overworld".to_owned()),
                Some(true),
            ));
        let generation_after_login = handle.shared.observation.read().generation;

        let full_chunk = light_chunk_packet(
            0,
            0,
            light_data_with_masks(
                &[1],
                &[1],
                &[],
                &[],
                vec![packed_light_layer(&[(0, 10)])],
                vec![packed_light_layer(&[(0, 2)])],
            ),
        );
        azalea::packet::game::process_packet(
            app.world_mut(),
            owner,
            &full_chunk,
            synthetic_attempt_token(),
        );
        queue_producer_packet(&mut app, owner, full_chunk);
        app.update();
        let generation_after_full_chunk = handle.shared.observation.read().generation;
        assert_eq!(generation_after_full_chunk, generation_after_login + 1);

        let partial = light_update_packet(
            0,
            0,
            light_data_with_masks(
                &[],
                &[1],
                &[],
                &[],
                Vec::new(),
                vec![packed_light_layer(&[(0, 6)])],
            ),
        );
        queue_producer_packet(&mut app, owner, partial);
        app.update();
        let generation_after_partial = handle.shared.observation.read().generation;
        assert_eq!(generation_after_partial, generation_after_full_chunk + 1);

        let forget = azalea::protocol::packets::game::ClientboundGamePacket::ForgetLevelChunk(
            azalea::protocol::packets::game::ClientboundForgetLevelChunk {
                pos: azalea::core::position::ChunkPos::new(0, 0),
            },
        );
        queue_producer_packet(&mut app, owner, forget);
        app.update();
        let generation_after_forget = handle.shared.observation.read().generation;
        assert_eq!(generation_after_forget, generation_after_partial + 1);

        assert!(handle
            .shared
            .reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
                owner,
                1,
                Some("minecraft:the_nether".to_owned()),
                Some(false),
            ));
        assert_eq!(
            handle.shared.observation.read().generation,
            generation_after_forget + 1
        );
        drop(shared_world);
    }

    #[test]
    fn frame_facts_capture_cannot_mix_with_an_epoch_reset_at_the_read_boundary() {
        let handle = RuntimeHandle::new(RunConfig::default());
        assert!(handle.shared.begin_connection_attempt());
        let snapshot = snapshot_at(1, 0.25, -64.0, 0.25);
        handle.test_install_frame_facts(snapshot.clone(), Some(9), Some(12));
        let generation_before_capture = handle.shared.observation.read().generation;

        let (start_reset, start_reset_received) = std_mpsc::channel();
        let (about_to_reset, about_to_reset_received) = std_mpsc::channel();
        let (write_boundary, write_boundary_received) = std_mpsc::channel();
        let (reset_finished, reset_finished_received) = std_mpsc::channel();
        let reset_completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let write_gate = Arc::new((parking_lot::Mutex::new(false), parking_lot::Condvar::new()));
        let hook_write_gate = write_gate.clone();
        handle
            .shared
            .set_observation_write_boundary_hook(Some(Arc::new(move || {
                write_boundary
                    .send(())
                    .expect("reset should reach the observation write boundary");
                let (allowed, wake) = &*hook_write_gate;
                let mut allowed = allowed.lock();
                while !*allowed {
                    wake.wait(&mut allowed);
                }
            })));

        let reset_completed_by_thread = reset_completed.clone();
        let shared = handle.shared.clone();
        let reset = thread::spawn(move || {
            start_reset_received
                .recv()
                .expect("reset must wait for an active capture read guard");
            about_to_reset
                .send(())
                .expect("reset should report before entering reset/write");
            let result = shared.begin_connection_attempt();
            reset_completed_by_thread.store(true, std::sync::atomic::Ordering::SeqCst);
            reset_finished
                .send(result)
                .expect("epoch reset result should remain observable");
        });

        let probe_shared = handle.shared.clone();
        let captured = handle
            .shared
            .capture_frame_facts_with_test_hooks(
                || {
                    start_reset
                        .send(())
                        .expect("capture must start reset after taking its read guard");
                    about_to_reset_received
                        .recv_timeout(StdDuration::from_secs(1))
                        .expect("reset should report before entering reset/write");
                    write_boundary_received
                        .recv_timeout(StdDuration::from_secs(1))
                        .expect("reset should reach the observation write boundary");
                    assert!(
                        !reset_completed.load(std::sync::atomic::Ordering::SeqCst),
                        "reset cannot complete while the capture read guard is held"
                    );
                    let (allowed, wake) = &*write_gate;
                    *allowed.lock() = true;
                    wake.notify_one();
                },
                move || {
                    assert!(
                        probe_shared.observation.try_write().is_none(),
                        "capture must retain its observation read guard through fact reads"
                    );
                },
            )
            .expect("the old frame should be captured coherently");

        assert_eq!(captured.snapshot, snapshot);
        assert_eq!(captured.armor, Some(9));
        assert_eq!(captured.light, Some(12));
        assert!(reset_finished_received
            .recv_timeout(StdDuration::from_secs(1))
            .expect("epoch reset should complete after capture"));
        reset.join().expect("epoch reset thread");
        assert!(handle.capture_frame_facts().is_none());
        assert!(handle.shared.observation.read().generation > generation_before_capture);
    }

    #[test]
    fn light_cache_decodes_nibbles_masks_retain_bad_layers_and_full_replace() {
        let mut ecs_world = bevy_ecs::world::World::new();
        let entity = ecs_world.spawn_empty().id();
        let source = CanonicalSourceAdmission {
            entity,
            epoch: 1,
            scope_generation: 4,
            attempt_token: None,
        };
        let geometry = LightSectionGeometry {
            min_light_section: -5,
            light_section_count: 26,
        };
        let default_world = azalea::world::World::default();
        assert_eq!(
            LightSectionGeometry::from_world(&default_world),
            Some(geometry)
        );
        let mut cache = LightCache::default();

        let low_index = (3 << 8) | (2 << 4);
        let high_index = low_index | 1;
        let first = light_data_with_masks(
            &[1],
            &[1],
            &[],
            &[],
            vec![packed_light_layer(&[(low_index, 2), (high_index, 13)])],
            vec![packed_light_layer(&[(low_index, 7), (high_index, 4)])],
        );
        assert!(cache.apply_packet(
            source,
            "minecraft:overworld".to_owned(),
            Some(true),
            geometry,
            0,
            0,
            &first,
            true,
        ));
        assert_eq!(
            cache.value_at(
                &Vec3Value {
                    x: 0.0,
                    y: -61.0,
                    z: 2.0,
                },
                1,
                4,
                "minecraft:overworld",
            ),
            Some(7)
        );
        assert_eq!(
            cache.value_at(
                &Vec3Value {
                    x: 1.0,
                    y: -61.0,
                    z: 2.0,
                },
                1,
                4,
                "minecraft:overworld",
            ),
            Some(13)
        );

        let empty = light_data_with_masks(&[], &[], &[2], &[2], Vec::new(), Vec::new());
        assert!(cache.apply_packet(
            source,
            "minecraft:overworld".to_owned(),
            Some(true),
            geometry,
            0,
            0,
            &empty,
            false,
        ));
        assert_eq!(cache.layer(0, 0, 2, true).map(|layer| layer[0]), Some(0));
        assert_eq!(cache.layer(0, 0, 2, false).map(|layer| layer[0]), Some(0));

        // Empty masks do not erase an ordinary layer; a data mask on the same
        // bit wins over its empty counterpart.
        let partial = light_data_with_masks(
            &[2],
            &[2],
            &[2],
            &[2],
            vec![packed_light_layer(&[(0, 8)])],
            vec![packed_light_layer(&[(0, 3)])],
        );
        assert!(cache.apply_packet(
            source,
            "minecraft:overworld".to_owned(),
            Some(true),
            geometry,
            0,
            0,
            &partial,
            false,
        ));
        assert_eq!(cache.layer(0, 0, 2, true).map(|layer| layer[0]), Some(8));
        assert_eq!(cache.layer(0, 0, 2, false).map(|layer| layer[0]), Some(3));
        assert_eq!(cache.layer(0, 0, 1, true).map(|layer| layer[0]), Some(0));

        // A bad first array must not shift the valid second array onto the
        // first bit. Missing arrays similarly make only their own layer unknown.
        let bad_then_valid = light_data_with_masks(
            &[1, 2],
            &[],
            &[],
            &[],
            vec![
                vec![0; 2047].into_boxed_slice(),
                packed_light_layer(&[(0, 9)]),
            ],
            Vec::new(),
        );
        assert!(cache.apply_packet(
            source,
            "minecraft:overworld".to_owned(),
            Some(true),
            geometry,
            0,
            0,
            &bad_then_valid,
            false,
        ));
        assert!(cache.layer(0, 0, 1, true).is_none());
        assert_eq!(cache.layer(0, 0, 2, true).map(|layer| layer[0]), Some(9));

        let missing_second = light_data_with_masks(
            &[1, 2],
            &[],
            &[],
            &[],
            vec![packed_light_layer(&[(0, 6)])],
            Vec::new(),
        );
        assert!(cache.apply_packet(
            source,
            "minecraft:overworld".to_owned(),
            Some(true),
            geometry,
            0,
            0,
            &missing_second,
            false,
        ));
        assert_eq!(cache.layer(0, 0, 1, true).map(|layer| layer[0]), Some(6));
        assert!(cache.layer(0, 0, 2, true).is_none());

        let out_of_range = light_data_with_masks(
            &[30],
            &[],
            &[],
            &[],
            vec![packed_light_layer(&[(0, 15)])],
            Vec::new(),
        );
        assert!(cache.apply_packet(
            source,
            "minecraft:overworld".to_owned(),
            Some(true),
            geometry,
            0,
            0,
            &out_of_range,
            false,
        ));
        assert!(cache.layer(0, 0, 30, true).is_none());

        let retain = azalea::protocol::packets::game::c_light_update::ClientboundLightUpdatePacketData::default();
        assert!(cache.apply_packet(
            source,
            "minecraft:overworld".to_owned(),
            Some(true),
            geometry,
            0,
            0,
            &retain,
            false,
        ));
        assert_eq!(cache.layer(0, 0, 1, true).map(|layer| layer[0]), Some(6));

        let replace = light_data_with_masks(&[], &[], &[], &[], Vec::new(), Vec::new());
        assert!(cache.apply_packet(
            source,
            "minecraft:overworld".to_owned(),
            Some(true),
            geometry,
            0,
            0,
            &replace,
            true,
        ));
        assert!(cache.layer(0, 0, 1, true).is_none());
        assert!(cache.remove_chunk(source, "minecraft:overworld", 0, 0));
        assert!(cache.chunks.is_empty());
    }

    #[test]
    fn light_cache_unknown_max_skylight_false_and_floor_rules_are_fail_closed() {
        let mut ecs_world = bevy_ecs::world::World::new();
        let entity = ecs_world.spawn_empty().id();
        let source = CanonicalSourceAdmission {
            entity,
            epoch: 1,
            scope_generation: 1,
            attempt_token: None,
        };
        let geometry = LightSectionGeometry {
            min_light_section: -5,
            light_section_count: 26,
        };
        let mut cache = LightCache::default();

        let below_fifteen = light_data_with_masks(
            &[1],
            &[],
            &[],
            &[],
            vec![packed_light_layer(&[(0, 14)])],
            Vec::new(),
        );
        assert!(cache.apply_packet(
            source,
            "minecraft:overworld".to_owned(),
            Some(true),
            geometry,
            0,
            0,
            &below_fifteen,
            true,
        ));
        assert_eq!(
            cache.value_at(
                &Vec3Value {
                    x: 0.0,
                    y: -64.0,
                    z: 0.0,
                },
                1,
                1,
                "minecraft:overworld",
            ),
            None
        );

        let fifteen = light_data_with_masks(
            &[1],
            &[],
            &[],
            &[],
            vec![packed_light_layer(&[(0, 15)])],
            Vec::new(),
        );
        assert!(cache.apply_packet(
            source,
            "minecraft:overworld".to_owned(),
            Some(true),
            geometry,
            0,
            0,
            &fifteen,
            true,
        ));
        assert_eq!(
            cache.value_at(
                &Vec3Value {
                    x: 0.0,
                    y: -64.0,
                    z: 0.0,
                },
                1,
                1,
                "minecraft:overworld",
            ),
            Some(15)
        );

        cache.reset_scope(1, 2, Some("minecraft:nether".to_owned()), Some(false));
        let no_sky = light_data_with_masks(
            &[1],
            &[1],
            &[],
            &[],
            vec![vec![0; 2047].into_boxed_slice()],
            vec![packed_light_layer(&[(0, 3)])],
        );
        let source = CanonicalSourceAdmission {
            scope_generation: 2,
            ..source
        };
        assert!(cache.apply_packet(
            source,
            "minecraft:nether".to_owned(),
            Some(false),
            geometry,
            0,
            0,
            &no_sky,
            true,
        ));
        assert_eq!(cache.layer(0, 0, 1, true).map(|layer| layer[0]), Some(0));
        assert_eq!(
            cache.value_at(
                &Vec3Value {
                    x: 0.0,
                    y: -64.0,
                    z: 0.0,
                },
                1,
                2,
                "minecraft:nether",
            ),
            Some(3)
        );
        assert_eq!(floor_block_coordinate(-0.1), Some(-1));
        assert_eq!(floor_block_coordinate(15.9), Some(15));
        assert_eq!(floor_block_coordinate(f64::NAN), None);
        assert_eq!(floor_block_coordinate(f64::INFINITY), None);
    }

    fn reducer_common_spawn_info(
        dimension: &str,
    ) -> azalea::protocol::packets::common::CommonPlayerSpawnInfo {
        use azalea::core::game_type::{GameMode, OptionalGameType};
        use azalea::protocol::packets::common::CommonPlayerSpawnInfo;
        use azalea::registry::data::DimensionKind;

        CommonPlayerSpawnInfo {
            dimension_type: <DimensionKind as azalea::registry::DataRegistry>::new_raw(0),
            dimension: azalea::Identifier::from(dimension),
            seed: 0,
            game_type: GameMode::Survival,
            previous_game_type: OptionalGameType(None),
            is_debug: false,
            is_flat: false,
            last_death_location: None,
            portal_cooldown: 0,
            sea_level: 63,
        }
    }

    fn reducer_respawn_packet(
        dimension: &str,
    ) -> azalea::protocol::packets::game::ClientboundGamePacket {
        let common = reducer_common_spawn_info(dimension);
        azalea::protocol::packets::game::ClientboundGamePacket::Respawn(
            azalea::protocol::packets::game::ClientboundRespawn {
                common,
                data_to_keep: 0,
            },
        )
    }

    #[test]
    fn registry_dimension_extra_proves_login_respawn_skylight_semantics() {
        let mut ecs_world = bevy_ecs::world::World::new();
        let owner = ecs_world.spawn_empty().id();
        let common = reducer_common_spawn_info("minecraft:overworld");

        for (proof, expected) in [
            (Some(true), Some(true)),
            (Some(false), Some(false)),
            (None, None),
        ] {
            let shared_world = empty_world();
            install_dimension_registry(&shared_world, "minecraft:overworld", proof);
            let holder = azalea::local_player::WorldHolder::new(owner, shared_world);
            assert_eq!(prove_has_skylight(&common, &holder), expected);
        }
    }

    fn sound_event_metadata(events: &mut RuntimeEventReceiver) -> Vec<(Option<String>, String)> {
        std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| {
                let dimension = event.dimension.clone();
                match event.payload {
                    BackendEventPayload::Sound(payload) => Some((dimension, payload.sound_name?)),
                    _ => None,
                }
            })
            .collect()
    }

    #[test]
    fn production_block_sound_raw_reducer_preserves_sound_scope_at_packet_position() {
        let run = |packets: Vec<azalea::protocol::packets::game::ClientboundGamePacket>| {
            let (_handle, mut app, owner, _shared_world, _source, mut events) = producer_test_app();
            for packet in packets {
                queue_producer_packet(&mut app, owner, packet);
            }
            app.update();
            sound_event_metadata(&mut events)
        };

        assert_eq!(
            run(vec![
                sound_packet(
                    azalea::registry::Holder::Direct(azalea::core::sound::CustomSound {
                        sound_id: azalea::Identifier::from("custom:old"),
                        range: None,
                    }),
                    1.0,
                    1.0,
                ),
                reducer_respawn_packet("minecraft:the_nether"),
                sound_packet(
                    azalea::registry::Holder::Direct(azalea::core::sound::CustomSound {
                        sound_id: azalea::Identifier::from("custom:new"),
                        range: None,
                    }),
                    1.0,
                    1.0,
                ),
            ]),
            vec![
                (None, "custom:old".to_owned()),
                (
                    Some("minecraft:the_nether".to_owned()),
                    "custom:new".to_owned()
                ),
            ]
        );
        assert_eq!(
            run(vec![
                reducer_respawn_packet("minecraft:the_nether"),
                sound_packet(
                    azalea::registry::Holder::Direct(azalea::core::sound::CustomSound {
                        sound_id: azalea::Identifier::from("custom:new-only"),
                        range: None,
                    }),
                    1.0,
                    1.0,
                ),
            ]),
            vec![(
                Some("minecraft:the_nether".to_owned()),
                "custom:new-only".to_owned()
            )]
        );
    }

    #[test]
    fn production_block_sound_raw_reducer_stamps_block_and_chunk_items_across_respawn() {
        let (_handle, mut app, owner, shared_world, _source, mut events) = producer_test_app();
        let old_chunk_pos = azalea::core::position::ChunkPos::new(-2, 1);
        let new_chunk_pos = azalea::core::position::ChunkPos::new(1, -2);
        let old_chunk = install_shared_chunk(&shared_world, old_chunk_pos);
        let new_chunk = install_shared_chunk(&shared_world, new_chunk_pos);
        let old_position = azalea::BlockPos {
            x: -17,
            y: -46,
            z: 30,
        };
        let new_position = azalea::BlockPos {
            x: 17,
            y: -46,
            z: -30,
        };
        let old_state = test_block_state(7);
        let new_state = test_block_state(8);
        let old_block = azalea::protocol::packets::game::ClientboundGamePacket::BlockUpdate(
            azalea::protocol::packets::game::ClientboundBlockUpdate {
                pos: old_position,
                block_state: old_state,
            },
        );
        let new_block = azalea::protocol::packets::game::ClientboundGamePacket::BlockUpdate(
            azalea::protocol::packets::game::ClientboundBlockUpdate {
                pos: new_position,
                block_state: new_state,
            },
        );
        let old_chunk_packet = empty_chunk_packet(old_chunk_pos.x, old_chunk_pos.z);
        let new_chunk_packet = empty_chunk_packet(new_chunk_pos.x, new_chunk_pos.z);

        azalea::packet::game::process_packet(
            app.world_mut(),
            owner,
            &old_block,
            synthetic_attempt_token(),
        );
        queue_producer_packet(&mut app, owner, old_block);
        azalea::packet::game::process_packet(
            app.world_mut(),
            owner,
            &old_chunk_packet,
            synthetic_attempt_token(),
        );
        queue_producer_packet(&mut app, owner, old_chunk_packet);
        queue_producer_packet(
            &mut app,
            owner,
            reducer_respawn_packet("minecraft:the_nether"),
        );
        azalea::packet::game::process_packet(
            app.world_mut(),
            owner,
            &new_chunk_packet,
            synthetic_attempt_token(),
        );
        queue_producer_packet(&mut app, owner, new_chunk_packet);
        azalea::packet::game::process_packet(
            app.world_mut(),
            owner,
            &new_block,
            synthetic_attempt_token(),
        );
        queue_producer_packet(&mut app, owner, new_block);
        app.update();

        let block_payloads = block_events(&mut events);
        assert_eq!(
            block_payloads
                .iter()
                .filter_map(|event| match event {
                    ContractProtocolBlockEvent::Updated { new_block, .. } => {
                        new_block.as_ref().map(|block| block.state_id)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![u32::from(new_state.id())]
        );
        assert_eq!(
            block_payloads
                .iter()
                .filter_map(|event| match event {
                    ContractProtocolBlockEvent::ChunkLoaded { chunk_x, chunk_z } => {
                        Some((*chunk_x, *chunk_z))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![(new_chunk_pos.x, new_chunk_pos.z)]
        );
        assert_eq!(
            shared_world.read().get_block_state(old_position),
            Some(old_state)
        );
        assert_eq!(
            shared_world.read().get_block_state(new_position),
            Some(new_state)
        );
        drop(old_chunk);
        drop(new_chunk);
    }

    #[test]
    fn production_block_sound_named_direct_and_reference_are_canonical_and_invalid_is_dropped() {
        let (_handle, mut app, owner, _shared_world, _source, mut events) = producer_test_app();
        queue_producer_packet(
            &mut app,
            owner,
            sound_packet(
                azalea::registry::Holder::Direct(azalea::core::sound::CustomSound {
                    sound_id: azalea::Identifier::from("custom:bell"),
                    range: None,
                }),
                0.75,
                1.25,
            ),
        );
        queue_producer_packet(
            &mut app,
            owner,
            sound_packet(
                azalea::registry::Holder::Reference(
                    azalea::registry::builtin::SoundEvent::AmbientCave,
                ),
                1.0,
                0.5,
            ),
        );
        for (volume, pitch) in [
            (f32::NAN, 1.0),
            (f32::INFINITY, 1.0),
            (-1.0, 1.0),
            (1.0, f32::NAN),
            (1.0, f32::INFINITY),
        ] {
            queue_producer_packet(
                &mut app,
                owner,
                sound_packet(
                    azalea::registry::Holder::Reference(
                        azalea::registry::builtin::SoundEvent::AmbientCave,
                    ),
                    volume,
                    pitch,
                ),
            );
        }
        app.update();

        let sounds = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event.payload {
                BackendEventPayload::Sound(payload) => Some(payload),
                _ => None,
            })
            .collect::<Vec<ContractProtocolSoundPayload>>();
        assert_eq!(sounds.len(), 2);
        assert_eq!(sounds[0].event_type, ContractHeardSoundType::Heard);
        assert_eq!(sounds[0].sound_name.as_deref(), Some("custom:bell"));
        assert_eq!(
            sounds[1].sound_name.as_deref(),
            Some("minecraft:ambient.cave")
        );
        for sound in &sounds {
            assert!(sound.sound_id.is_none());
            assert!(sound.category.is_none());
            assert_eq!(
                sound.protocol_source,
                ContractProtocolSoundSource::NamedSoundEffect
            );
            assert!(!sound.sound_key.is_empty());
            assert_eq!(
                (
                    sound.source_position.x,
                    sound.source_position.y,
                    sound.source_position.z
                ),
                (1.125, -1.0, 2.0)
            );
        }
        assert_ne!(sounds[0].sound_key, sounds[1].sound_key);
        assert_eq!((sounds[0].volume, sounds[0].pitch), (0.75, 1.25));
        assert_eq!((sounds[1].volume, sounds[1].pitch), (1.0, 0.5));
    }

    #[test]
    fn production_block_sound_chunk_loaded_and_unloaded_are_once_and_negative_coordinates_are_preserved(
    ) {
        let (handle, mut app, owner, shared_world, _source, mut events) = producer_test_app();
        let chunk_pos = azalea::core::position::ChunkPos::new(-1, -2);
        let chunk = install_shared_chunk(&shared_world, chunk_pos);
        let chunk_packet = azalea::protocol::packets::game::ClientboundGamePacket::LevelChunkWithLight(
            azalea::protocol::packets::game::ClientboundLevelChunkWithLight {
                x: chunk_pos.x,
                z: chunk_pos.z,
                chunk_data: azalea::protocol::packets::game::c_level_chunk_with_light::ClientboundLevelChunkPacketData {
                    heightmaps: Vec::new(),
                    data: Arc::new(Vec::<u8>::new().into_boxed_slice()),
                    block_entities: Vec::new(),
                },
                light_data: Default::default(),
            },
        );
        azalea::packet::game::process_packet(
            app.world_mut(),
            owner,
            &chunk_packet,
            synthetic_attempt_token(),
        );
        queue_producer_packet(&mut app, owner, chunk_packet);
        app.update();
        assert!(matches!(
            block_events(&mut events).as_slice(),
            [ContractProtocolBlockEvent::ChunkLoaded {
                chunk_x: -1,
                chunk_z: -2
            }]
        ));
        app.update();
        assert!(block_events(&mut events).is_empty());
        drop(chunk);

        let forget = azalea::protocol::packets::game::ClientboundGamePacket::ForgetLevelChunk(
            azalea::protocol::packets::game::ClientboundForgetLevelChunk { pos: chunk_pos },
        );
        azalea::packet::game::process_packet(
            app.world_mut(),
            owner,
            &forget,
            synthetic_attempt_token(),
        );
        queue_producer_packet(&mut app, owner, forget);
        app.update();
        assert!(matches!(
            block_events(&mut events).as_slice(),
            [ContractProtocolBlockEvent::ChunkUnloaded {
                chunk_x: -1,
                chunk_z: -2
            }]
        ));
        assert_eq!(handle.connection_epoch(), 1);
    }

    #[test]
    fn canonical_observation_late_scope_publication_is_fail_closed() {
        let (handle, _app, owner, _shared_world, _source, mut events) = producer_test_app();
        let source = handle
            .shared
            .admit_canonical_source(owner)
            .expect("current owner admission");
        assert!(handle
            .shared
            .reset_entity_scope_for_owner_at_epoch(owner, source.epoch));
        assert!(!handle.shared.emit_canonical_observation_event(
            source,
            BackendEventPayload::Block(ContractProtocolBlockEvent::ChunkLoaded {
                chunk_x: -1,
                chunk_z: -2,
            }),
        ));
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn transport_retry_schedule_matches_ts_oracle_boundaries() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-03T00:00:00Z")
            .expect("fixed clock")
            .with_timezone(&chrono::Utc);
        let policy = ReconnectPolicy {
            enabled: true,
            initial_delay_ms: 100,
            multiplier: 2.0,
            max_delay_ms: 1_000,
            jitter_ratio: 0.0,
            stable_reset_ms: 60_000,
        };
        assert_eq!(
            reconnect_schedule_at(&policy, 1, 0.5, now).delay,
            Duration::from_millis(100)
        );
        assert_eq!(
            reconnect_schedule_at(&policy, 2, 0.5, now).delay,
            Duration::from_millis(200)
        );
        assert_eq!(
            reconnect_schedule_at(&policy, 4, 0.5, now).delay,
            Duration::from_millis(800)
        );
        let clamped = reconnect_schedule_at(&policy, 5, 0.5, now);
        assert_eq!(clamped.delay, Duration::from_millis(1_000));
        assert_eq!(
            clamped
                .retry_at
                .signed_duration_since(now)
                .num_milliseconds(),
            1_000
        );

        let initial_larger_than_max = ReconnectPolicy {
            initial_delay_ms: 5_000,
            max_delay_ms: 1_000,
            ..policy.clone()
        };
        assert_eq!(
            reconnect_schedule_at(&initial_larger_than_max, 1, 0.5, now).delay,
            Duration::from_millis(1_000)
        );

        let jitter = ReconnectPolicy {
            initial_delay_ms: 100,
            max_delay_ms: 1_000,
            jitter_ratio: 0.2,
            ..policy.clone()
        };
        assert_eq!(
            reconnect_schedule_at(&jitter, 1, 0.0, now).delay,
            Duration::from_millis(80)
        );
        assert_eq!(
            reconnect_schedule_at(&jitter, 1, 0.5, now).delay,
            Duration::from_millis(100)
        );
        assert_eq!(
            reconnect_schedule_at(&jitter, 1, 0.999_999, now).delay,
            Duration::from_millis(120)
        );

        let full_jitter = ReconnectPolicy {
            initial_delay_ms: 1,
            max_delay_ms: 10,
            jitter_ratio: 1.0,
            ..policy.clone()
        };
        assert_eq!(
            reconnect_schedule_at(&full_jitter, 1, 0.0, now).delay,
            Duration::ZERO
        );
        assert_eq!(
            reconnect_schedule_at(&full_jitter, 1, 0.5, now).delay,
            Duration::from_millis(1)
        );
        assert_eq!(
            reconnect_schedule_at(&full_jitter, 1, 0.75, now).delay,
            Duration::from_millis(2)
        );

        let huge = ReconnectPolicy {
            initial_delay_ms: u64::MAX,
            max_delay_ms: u64::MAX,
            multiplier: f64::MAX,
            jitter_ratio: 1.0,
            ..policy
        };
        assert_eq!(
            reconnect_schedule_at(&huge, u64::MAX, 1.0, now).delay,
            Duration::from_millis(u64::MAX)
        );
        assert_eq!(
            reconnect_schedule_at(&huge, u64::MAX, 1.0, now).retry_at,
            chrono::DateTime::<chrono::Utc>::MAX_UTC
        );
    }

    #[test]
    fn transport_retry_ordinal_matches_scheduled_and_next_request_while_epoch_stays_monotonic() {
        let mut config = RunConfig::default();
        config.reconnect.jitter_ratio = 0.0;
        let handle = RuntimeHandle::new(config);
        let mut events = handle.subscribe();
        assert!(handle.shared.begin_connection_attempt());
        let first_request = events.try_recv().expect("first request");
        assert_eq!(first_request.connection_epoch, 1);
        assert_eq!(payload_json(&first_request)["attempt"], 1);

        let close = handle.shared.mark_disconnected(None);
        let _closed = events.try_recv().expect("first close");
        assert_eq!(
            handle.shared.emit_reconnect_scheduled(&close),
            Some(Duration::from_millis(1_000))
        );
        let scheduled = events.try_recv().expect("reconnect schedule");
        assert_eq!(scheduled.connection_epoch, 1);
        assert_eq!(payload_json(&scheduled)["attempt"], 2);

        assert!(handle.shared.begin_connection_attempt());
        let second_request = events.try_recv().expect("second request");
        assert_eq!(second_request.connection_epoch, 2);
        assert_eq!(payload_json(&second_request)["attempt"], 2);
        assert_eq!(handle.shared.retry_ordinal.load(Ordering::Acquire), 2);
    }

    #[test]
    fn stable_reset_requires_exact_ready_epoch_attempt_and_generation() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut world = bevy_ecs::world::World::new();
        let owner = world.spawn_empty().id();
        let owner_b = world.spawn_empty().id();
        let mut events = handle.subscribe();

        assert!(handle.shared.begin_connection_attempt());
        let _request_a = events.try_recv().expect("request A");
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(owner),
            Some(1)
        );
        let _transport_a = events.try_recv().expect("transport A");
        handle
            .shared
            .emit_logged_in("26.1.2", "minecraft:overworld".to_owned());
        let _logged_in_a = events.try_recv().expect("login A");
        handle.shared.emit_ready(1);
        let _ready_a = events.try_recv().expect("ready A");
        let stable_a = handle.shared.test_current_stable_token();

        let _close_a = handle.shared.mark_disconnected(None);
        let _closed_a = events.try_recv().expect("close A");
        assert!(handle.shared.begin_connection_attempt());
        let _request_b = events.try_recv().expect("request B");
        assert_eq!(handle.shared.retry_ordinal.load(Ordering::Acquire), 2);
        handle.shared.fire_stable_reset(stable_a);
        assert_eq!(handle.shared.retry_ordinal.load(Ordering::Acquire), 2);

        assert!(handle
            .shared
            .consume_attempt_for_transport_init_and_bind(owner_b)
            .is_some());
        let _transport_b = events.try_recv().expect("transport B");
        handle
            .shared
            .emit_logged_in("26.1.2", "minecraft:overworld".to_owned());
        let _logged_in_b = events.try_recv().expect("login B");
        handle.shared.emit_ready(2);
        let _ready_b = events.try_recv().expect("ready B");
        let stable_b = handle.shared.test_current_stable_token();
        handle.shared.fire_stable_reset(stable_b);
        assert_eq!(handle.shared.retry_ordinal.load(Ordering::Acquire), 0);

        handle
            .shared
            .emit_logged_in("26.1.2", "minecraft:overworld".to_owned());
        let _logged_in_again = events.try_recv().expect("non-ready transition");
        handle.shared.fire_stable_reset(stable_b);
        assert_eq!(handle.shared.retry_ordinal.load(Ordering::Acquire), 0);

        let stable_non_ready = handle.shared.test_current_stable_token();
        handle.stop("stable-stop");
        handle.shared.fire_stable_reset(stable_non_ready);
        assert_eq!(handle.shared.retry_ordinal.load(Ordering::Acquire), 0);
    }

    #[test]
    fn phase_deadlines_have_distinct_codes_unique_close_and_disabled_fault_priority() {
        for (phase, expected_code) in [
            (TransportPhase::Connecting, "connection_timeout"),
            (TransportPhase::LoggingIn, "login_timeout"),
            (TransportPhase::Spawning, "spawn_timeout"),
        ] {
            let mut config = RunConfig::default();
            config.reconnect.enabled = false;
            let handle = RuntimeHandle::new(config);
            let mut events = handle.subscribe();
            let mut world = bevy_ecs::world::World::new();
            let owner = world.spawn_empty().id();
            assert!(handle.shared.begin_connection_attempt());
            let _request = events.try_recv().expect("request");
            if phase != TransportPhase::Connecting {
                assert!(handle
                    .shared
                    .consume_attempt_for_transport_init_and_bind(owner)
                    .is_some());
                let _transport = events.try_recv().expect("transport");
            }
            if phase == TransportPhase::Spawning {
                handle
                    .shared
                    .emit_logged_in("26.1.2", "minecraft:overworld".to_owned());
                let _login = events.try_recv().expect("login");
            }
            let token = handle.shared.test_current_phase_token(phase);
            handle.shared.fire_phase_deadline(token);
            let closed = events.try_recv().expect("timeout close");
            assert_eq!(payload_json(&closed)["close"]["code"], expected_code);
            let faulted = events.try_recv().expect("disabled fault");
            assert_eq!(payload_json(&faulted)["type"], "faulted");
            assert_eq!(
                payload_json(&faulted)["failure"]["code"],
                "reconnect_disabled"
            );
            handle.shared.fire_phase_deadline(token);
            assert!(events.try_recv().is_err(), "late timeout must be inert");
        }
    }

    #[test]
    fn late_phase_timeout_and_disconnect_stop_races_are_admission_gated() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut world = bevy_ecs::world::World::new();
        let owner_a = world.spawn_empty().id();
        let owner_b = world.spawn_empty().id();

        assert!(handle.shared.begin_connection_attempt());
        let _request_a = events.try_recv().expect("request A");
        assert!(handle
            .shared
            .consume_attempt_for_transport_init_and_bind(owner_a)
            .is_some());
        let _transport_a = events.try_recv().expect("transport A");
        let timeout_a = handle
            .shared
            .test_current_phase_token(TransportPhase::LoggingIn);
        assert!(handle.shared.begin_connection_attempt());
        let _request_b = events.try_recv().expect("request B");
        assert_eq!(handle.shared.connection_epoch(), 2);
        handle.shared.fire_phase_deadline(timeout_a);
        assert!(
            events.try_recv().is_err(),
            "late A timeout must not close B"
        );

        assert!(handle
            .shared
            .consume_attempt_for_transport_init_and_bind(owner_b)
            .is_some());
        let _transport_b = events.try_recv().expect("transport B");
        let timeout_b = handle
            .shared
            .test_current_phase_token(TransportPhase::LoggingIn);
        let close_b = handle.shared.mark_disconnected(None);
        let closed_b = events.try_recv().expect("disconnect close B");
        assert_eq!(payload_json(&closed_b)["close"]["code"], close_b.code);
        handle.shared.fire_phase_deadline(timeout_b);
        assert!(
            events.try_recv().is_err(),
            "late timeout after disconnect is inert"
        );

        let stop_handle = RuntimeHandle::new(RunConfig::default());
        let mut stop_events = stop_handle.subscribe();
        assert!(stop_handle.shared.begin_connection_attempt());
        let _stop_request = stop_events.try_recv().expect("stop request");
        assert!(stop_handle
            .shared
            .consume_attempt_for_transport_init_and_bind(owner_a)
            .is_some());
        let _stop_transport = stop_events.try_recv().expect("stop transport");
        let timeout_before_stop = stop_handle
            .shared
            .test_current_phase_token(TransportPhase::LoggingIn);
        stop_handle.stop("timeout-stop-race");
        let _stop_close = stop_events.try_recv().expect("stop close");
        let _stopped = stop_events.try_recv().expect("stopped");
        stop_handle.shared.fire_phase_deadline(timeout_before_stop);
        assert!(
            stop_events.try_recv().is_err(),
            "late timeout after stop is inert"
        );
    }

    #[tokio::test]
    async fn runtime_shaped_timer_tasks_install_progress_and_cancel_on_phase_and_stop() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut config = RunConfig::default();
                config.timeouts.login_ms = 1;
                config.timeouts.spawn_ms = 1;
                config.timeouts.stop_ms = 1;
                config.reconnect.stable_reset_ms = 1_000;
                let handle = RuntimeHandle::new(config);
                handle.shared.timers_enabled.store(true, Ordering::Release);
                let mut world = bevy_ecs::world::World::new();
                let owner = world.spawn_empty().id();
                let mut events = handle.subscribe();

                assert!(handle.shared.begin_connection_attempt());
                let _request = events.try_recv().expect("request");
                assert!(handle
                    .shared
                    .consume_attempt_for_transport_init_and_bind(owner)
                    .is_some());
                let _transport = events.try_recv().expect("transport");
                // The real login timer was installed; advancing to the next
                // phase invalidates it before it can close the attempt.
                handle
                    .shared
                    .emit_logged_in("26.1.2", "minecraft:overworld".to_owned());
                let _logged_in = events.try_recv().expect("logged in");
                handle.shared.emit_ready(1);
                let _ready = events.try_recv().expect("ready");
                tokio::time::sleep(Duration::from_millis(5)).await;
                assert!(
                    events.try_recv().is_err(),
                    "cancelled login/spawn timers stay inert"
                );

                let watchdog_handle = RuntimeHandle::new(RunConfig {
                    timeouts: BackendTimeouts {
                        connect_ms: 1,
                        login_ms: 1,
                        spawn_ms: 1,
                        stop_ms: 1,
                    },
                    ..RunConfig::default()
                });
                watchdog_handle
                    .shared
                    .timers_enabled
                    .store(true, Ordering::Release);
                let mut watchdog_events = watchdog_handle.subscribe();
                assert!(watchdog_handle.shared.begin_connection_attempt());
                let _watchdog_request = watchdog_events.try_recv().expect("watchdog request");
                let (completion, state) = CommandCompletion::channel("runtime-watchdog".to_owned());
                state.begin_active_release(Arc::new(Notify::new()));
                *watchdog_handle.shared.active_movement_completion.lock() = Some(state);
                *watchdog_handle.shared.active_movement_id.lock() =
                    Some("runtime-watchdog".to_owned());
                watchdog_handle
                    .shared
                    .active_movement
                    .store(true, Ordering::Release);
                watchdog_handle.stop("runtime-watchdog");
                let _watchdog_close = watchdog_events.try_recv().expect("watchdog close");
                tokio::time::sleep(Duration::from_millis(5)).await;
                assert!(completion.wait().await.is_err());
                let stopped = watchdog_events.try_recv().expect("watchdog stopped");
                assert_eq!(payload_json(&stopped)["type"], "stopped");
                assert!(watchdog_events.try_recv().is_err());
            })
            .await;
    }

    #[test]
    fn normal_stop_invalidates_watchdog_and_publishes_stopped_once() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        assert!(handle.shared.begin_connection_attempt());
        let _request = events.try_recv().expect("request");
        handle.stop("normal-stop");
        let _closed = events.try_recv().expect("close");
        let stopped = events.try_recv().expect("stopped");
        assert_eq!(payload_json(&stopped)["type"], "stopped");
        let token = handle.shared.test_current_stop_watchdog_token();
        handle.shared.fire_stop_watchdog(token);
        assert!(
            events.try_recv().is_err(),
            "cancelled watchdog must not duplicate stopped"
        );
    }

    #[tokio::test]
    async fn normal_stop_cancels_watchdog_task_before_large_stop_deadline() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut config = RunConfig::default();
                config.timeouts.stop_ms = 60_000;
                let handle = RuntimeHandle::new(config);
                handle.shared.timers_enabled.store(true, Ordering::Release);
                let (probe_sender, probe_receiver) = oneshot::channel();
                *handle.shared.stop_watchdog_completion_probe.lock() = Some(probe_sender);
                let weak_shared = Arc::downgrade(&handle.shared);
                let mut events = handle.subscribe();

                assert!(handle.shared.begin_connection_attempt());
                let _request = events.try_recv().expect("request");
                // initiate_stop spawns the watchdog and immediately completes
                // normal cleanup in this case.  No yield occurs between those
                // operations, so this also exercises Notify's pre-first-poll
                // permit retention.
                handle.stop("normal-stop-task");
                let _closed = events.try_recv().expect("close");
                let stopped = events.try_recv().expect("stopped");
                assert_eq!(payload_json(&stopped)["type"], "stopped");
                drop(events);
                drop(handle);

                tokio::time::timeout(Duration::from_secs(1), probe_receiver)
                    .await
                    .expect("normal cleanup must cancel the large watchdog without sleeping")
                    .expect("watchdog completion probe");
                assert!(
                    weak_shared.upgrade().is_none(),
                    "completed watchdog task must not retain RuntimeShared"
                );
            })
            .await;
    }

    #[tokio::test]
    async fn stop_watchdog_forces_pending_movement_and_has_no_fault_or_reconnect() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        assert!(handle.shared.begin_connection_attempt());
        let _request = events.try_recv().expect("request");
        let (completion, state) = CommandCompletion::channel("watchdog-move".to_owned());
        state.begin_active_release(Arc::new(Notify::new()));
        *handle.shared.active_movement_completion.lock() = Some(state);
        *handle.shared.active_movement_id.lock() = Some("watchdog-move".to_owned());
        handle.shared.active_movement.store(true, Ordering::Release);

        handle.stop("watchdog-stop");
        let _closed = events.try_recv().expect("close");
        let token = handle.shared.test_current_stop_watchdog_token();
        assert!(
            events.try_recv().is_err(),
            "stopped waits for forced cleanup"
        );
        handle.shared.fire_stop_watchdog(token);
        assert!(completion.wait().await.is_err());
        let stopped = events.try_recv().expect("forced stopped");
        assert_eq!(payload_json(&stopped)["type"], "stopped");
        assert!(events.try_recv().is_err());
        assert!(matches!(handle.state(), BackendState::Stopped { .. }));
    }

    // ================= NEW-13 V2: AttemptToken ↔ epoch binding =================

    fn stamped_block_app() -> (
        RuntimeHandle,
        App,
        bevy_ecs::entity::Entity,
        RuntimeEventReceiver,
    ) {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut app = App::new();
        app.add_message::<azalea::packet::game::ReceiveGamePacketEvent>();
        app.add_message::<azalea::chunks::ReceiveChunkEvent>();
        let owner = app
            .world_mut()
            .spawn((LocalEntity, azalea::core::entity_id::MinecraftEntityId(99)))
            .id();
        app.world_mut().entity_mut(owner).insert((
            azalea::local_player::WorldHolder::new(owner, empty_world()),
            azalea::block_update::QueuedServerBlockUpdates::default(),
            azalea::interact::BlockStatePredictionHandler::default(),
            CanonicalPacketSourceMetadata::default(),
        ));
        app.insert_resource(SwarmState {
            shared: handle.shared.clone(),
        });
        app.add_systems(azalea::app::PreUpdate, produce_entity_packet_events);
        app.add_systems(
            Update,
            (
                azalea::chunks::handle_receive_chunk_event,
                azalea::block_update::handle_block_update_event,
            ),
        );
        app.add_plugins(BlockSoundProducerPlugin);
        let events = handle.subscribe();
        (handle, app, owner, events)
    }

    fn bind_stamped_attempt(
        handle: &RuntimeHandle,
        app: &mut App,
        owner: bevy_ecs::entity::Entity,
        token: azalea::join::AttemptToken,
    ) {
        let epoch = handle.shared.connection_epoch();
        assert!(handle
            .shared
            .admit_canonical_join_started_with_token(epoch, Some(token)));
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind_with_token(owner, Some(token)),
            Some(epoch)
        );
        app.world_mut()
            .insert_resource(super::entity_events_owner_tests::TestAttemptToken(token));
    }

    fn block_update_packet(
        position: azalea::BlockPos,
        state_id: u32,
    ) -> azalea::protocol::packets::game::ClientboundGamePacket {
        azalea::protocol::packets::game::ClientboundGamePacket::BlockUpdate(
            azalea::protocol::packets::game::ClientboundBlockUpdate {
                pos: position,
                block_state: test_block_state(state_id),
            },
        )
    }

    fn drain_events(receiver: &mut RuntimeEventReceiver) -> Vec<BackendEventEnvelope> {
        std::iter::from_fn(|| receiver.try_recv().ok()).collect()
    }

    #[test]
    fn stamped_rebind_restores_production_and_late_a_packets_are_rejected() {
        let (handle, mut app, owner, mut events) = stamped_block_app();
        assert!(handle.shared.begin_connection_attempt());
        let token_a = azalea::join::AttemptToken::mint();
        bind_stamped_attempt(&handle, &mut app, owner, token_a);
        let _ = drain_events(&mut events);

        // A packet is admitted on epoch 1 through the stamped source path.
        queue_production_block_packet(
            &mut app,
            owner,
            azalea::BlockPos { x: 0, y: 0, z: 0 },
            test_block_state(1),
        );
        app.update();
        assert_eq!(block_events(&mut events).len(), 1);

        assert!(handle
            .shared
            .admit_canonical_disconnected_with_token(owner, 1, None, Some(token_a))
            .is_some());
        let _ = drain_events(&mut events);

        // B reuses the same entity. The legacy fence becomes ambiguous, but
        // stamped B sources must still reach production.
        assert!(handle.shared.begin_connection_attempt());
        let token_b = azalea::join::AttemptToken::mint();
        bind_stamped_attempt(&handle, &mut app, owner, token_b);
        let _ = drain_events(&mut events);

        queue_production_block_packet(
            &mut app,
            owner,
            azalea::BlockPos { x: 1, y: 0, z: 0 },
            test_block_state(2),
        );
        app.update();
        assert_eq!(
            block_events(&mut events).len(),
            1,
            "stamped B must be accepted even after same-entity reuse made the fence ambiguous"
        );

        // A late A packet is rejected at the source admission; even if a
        // packet were queued, no envelope may be produced for it.
        assert!(
            handle
                .shared
                .admit_canonical_source_with_token(owner, Some(token_a))
                .is_none(),
            "a late A packet must be rejected at the stamped source admission"
        );
        app.world_mut()
            .write_message(azalea::packet::game::ReceiveGamePacketEvent {
                entity: owner,
                packet: Arc::new(block_update_packet(
                    azalea::BlockPos { x: 2, y: 0, z: 0 },
                    3,
                )),
                attempt_token: token_a,
            });
        app.update();
        assert!(
            block_events(&mut events).is_empty(),
            "a late A packet must not publish into B's epoch"
        );
    }

    #[test]
    fn late_a_lifecycle_sources_cannot_claim_b() {
        let (handle, mut app, owner, mut events) = stamped_block_app();
        assert!(handle.shared.begin_connection_attempt());
        let token_a = azalea::join::AttemptToken::mint();
        bind_stamped_attempt(&handle, &mut app, owner, token_a);
        let _ = drain_events(&mut events);
        assert!(handle
            .shared
            .admit_canonical_disconnected_with_token(owner, 1, None, Some(token_a))
            .is_some());
        let _ = drain_events(&mut events);

        assert!(handle.shared.begin_connection_attempt());
        let token_b = azalea::join::AttemptToken::mint();
        bind_stamped_attempt(&handle, &mut app, owner, token_b);
        let _ = drain_events(&mut events);
        let epoch = handle.shared.connection_epoch();
        assert_eq!(epoch, 2);

        // Stamped packet admission: A rejected, B accepted.
        assert!(handle
            .shared
            .admit_canonical_source_with_token(owner, Some(token_a))
            .is_none());
        let source_b = handle
            .shared
            .admit_canonical_source_with_token(owner, Some(token_b))
            .expect("stamped B source must be admitted");
        assert_eq!(source_b.attempt_token, Some(token_b));

        // WorldLoaded boundary: A rejected, B accepted.
        assert!(!handle
            .shared
            .observe_dimension_from_world_boundary_with_token(
                owner,
                "minecraft:the_nether",
                Some(token_a),
            ));
        assert!(handle
            .shared
            .observe_dimension_from_world_boundary_with_token(
                owner,
                "minecraft:the_nether",
                Some(token_b),
            ));

        // Disconnect: late A cannot close B; matching B closes normally.
        assert!(handle
            .shared
            .admit_canonical_disconnected_with_token(owner, 2, None, Some(token_a))
            .is_none());
        assert!(handle
            .shared
            .admit_canonical_disconnected_with_token(owner, 2, None, Some(token_b))
            .is_some());
        let _ = drain_events(&mut events);

        // ConnectionFailed: late A cannot close a fresh B.
        assert!(handle.shared.begin_connection_attempt());
        let token_b2 = azalea::join::AttemptToken::mint();
        bind_stamped_attempt(&handle, &mut app, owner, token_b2);
        let _ = drain_events(&mut events);
        assert!(handle
            .shared
            .admit_canonical_connection_failed_with_token(
                owner,
                3,
                "probe".to_owned(),
                Some(token_a)
            )
            .is_none());
        assert!(handle
            .shared
            .admit_canonical_connection_failed_with_token(
                owner,
                3,
                "probe".to_owned(),
                Some(token_b2),
            )
            .is_some());

        // Init/Client binding: A's token cannot consume B's reservation.
        assert!(handle.shared.begin_connection_attempt());
        let token_b3 = azalea::join::AttemptToken::mint();
        assert!(handle
            .shared
            .admit_canonical_join_started_with_token(4, Some(token_b3)));
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind_with_token(owner, Some(token_a)),
            None
        );
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind_with_token(owner, Some(token_b3)),
            Some(4)
        );

        // Client token gate: late A client rejected, matching B accepted.
        let empty_world = Arc::new(parking_lot::RwLock::new(bevy_ecs::world::World::new()));
        let client_a = azalea::Client::new_with_attempt_token(owner, empty_world.clone(), token_a);
        let client_b = azalea::Client::new_with_attempt_token(owner, empty_world, token_b3);
        assert!(!handle.shared.client_is_current_owner(&client_a));
        assert!(handle.shared.client_is_current_owner(&client_b));

        // Swarm disconnect: late A cannot claim a reconnect for B.
        assert!(!handle.shared.claim_reconnect_with_token(Some(token_a)));
        assert!(handle
            .shared
            .admit_canonical_disconnected_with_token(owner, 4, None, Some(token_b3))
            .is_some());
        assert!(handle.shared.claim_reconnect_with_token(Some(token_b3)));
    }

    #[test]
    fn source_token_bindings_are_one_to_one_and_idempotent() {
        let mut bindings = SourceTokenBindings::default();
        let a = azalea::join::AttemptToken::mint();
        let b = azalea::join::AttemptToken::mint();
        let c = azalea::join::AttemptToken::mint();

        assert!(bindings.bind(a, 1));
        assert!(
            !bindings.bind(a, 2),
            "A must never be re-registered on epoch 2"
        );
        assert!(bindings.bind(b, 2));
        assert!(!bindings.bind(c, 2), "epoch 2 cannot switch to C");
        assert!(bindings.bind(a, 1), "the same pair is idempotent");
        assert!(bindings.matches(a, 1));
        assert!(!bindings.matches(a, 2));
    }

    #[test]
    fn historical_token_cannot_bind_to_a_later_epoch() {
        let handle = RuntimeHandle::new(RunConfig::default());
        assert!(handle.shared.begin_connection_attempt());
        let token_a = azalea::join::AttemptToken::mint();
        assert!(handle
            .shared
            .admit_canonical_join_started_with_token(1, Some(token_a)));
        assert!(handle.shared.begin_connection_attempt());
        assert!(
            !handle
                .shared
                .admit_canonical_join_started_with_token(2, Some(token_a)),
            "a historical token must not be re-registered on a later epoch"
        );
        let token_b = azalea::join::AttemptToken::mint();
        assert!(handle
            .shared
            .admit_canonical_join_started_with_token(2, Some(token_b)));
    }

    #[test]
    fn reconnect_return_client_token_must_match_start_join_token() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut ecs_world = bevy_ecs::world::World::new();
        let entity = ecs_world.spawn_empty().id();

        assert!(handle.shared.begin_connection_attempt());
        let token_a = azalea::join::AttemptToken::mint();
        assert!(handle
            .shared
            .admit_canonical_join_started_with_token(1, Some(token_a)));
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind_with_token(entity, Some(token_a)),
            Some(1)
        );
        assert!(handle
            .shared
            .admit_canonical_disconnected_with_token(entity, 1, None, Some(token_a))
            .is_some());
        let _ = drain_events(&mut events);

        assert!(handle.shared.claim_reconnect_with_token(Some(token_a)));
        let reconnect_token = handle
            .shared
            .admit_reconnect_attempt()
            .expect("reconnect reservation");
        let token_b = azalea::join::AttemptToken::mint();
        assert!(handle
            .shared
            .admit_canonical_join_started_with_token(2, Some(token_b)));

        // A different client token than the StartJoin token is rejected.
        assert_eq!(
            handle
                .shared
                .bind_reconnect_return_with_token(reconnect_token, entity, Some(token_a)),
            None
        );
        // The matching token binds and installs the active client.
        assert_eq!(
            handle
                .shared
                .bind_reconnect_return_with_token(reconnect_token, entity, Some(token_b)),
            Some(2)
        );
        let empty_world = Arc::new(parking_lot::RwLock::new(bevy_ecs::world::World::new()));
        let client_b = azalea::Client::new_with_attempt_token(entity, empty_world, token_b);
        assert!(handle.shared.set_active_client_if_current(&client_b));
        handle.shared.finish_reconnect_attempt(reconnect_token);
    }

    #[test]
    fn tokenless_fallback_first_connect_works_but_same_entity_rebind_fails_closed() {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut ecs_world = bevy_ecs::world::World::new();
        let entity = ecs_world.spawn_empty().id();

        // First, never-reused connect keeps the legacy tokenless behavior.
        assert!(handle.shared.begin_connection_attempt());
        assert!(handle.shared.admit_canonical_join_started(1));
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(entity),
            Some(1)
        );
        let _ = drain_events(&mut events);
        assert!(handle.shared.admit_canonical_source(entity).is_some());

        // Same-entity rebind without a token must stay fail closed; the
        // current B token is never back-stamped onto A events.
        assert!(handle
            .shared
            .admit_canonical_disconnected(entity, 1, None)
            .is_some());
        let _ = drain_events(&mut events);
        assert!(handle.shared.begin_connection_attempt());
        assert!(!handle.shared.admit_canonical_join_started(2));
        assert!(handle.shared.admit_canonical_source(entity).is_none());
    }

    #[tokio::test]
    async fn stamped_admission_publication_race_rejects_a_after_b_rebind() {
        let (handle, mut app, owner, mut events) = stamped_block_app();
        assert!(handle.shared.begin_connection_attempt());
        let token_a = azalea::join::AttemptToken::mint();
        bind_stamped_attempt(&handle, &mut app, owner, token_a);
        let _ = drain_events(&mut events);
        let source = handle
            .shared
            .admit_canonical_source_with_token(owner, Some(token_a))
            .expect("A source must be admitted while A is current");

        let (checked_tx, checked_rx) = std_mpsc::channel();
        let (release_tx, release_rx) = std_mpsc::channel();
        let release_rx = Arc::new(StdMutex::new(Some(release_rx)));
        // The probe runs outside `command_admission`, so the main thread can
        // rebind B while A's publication is suspended between source
        // admission and the publication recheck.
        handle
            .shared
            .set_canonical_publication_probe(Some(Arc::new({
                let release_rx = release_rx.clone();
                move || {
                    checked_tx.send(()).expect("hook reached");
                    release_rx
                        .lock()
                        .expect("release gate lock")
                        .take()
                        .expect("one publication owns the gate")
                        .recv()
                        .expect("publication release");
                }
            })));

        let publish_shared = handle.shared.clone();
        let publisher = thread::spawn(move || {
            publish_shared.emit_canonical_observation_event(
                source,
                BackendEventPayload::Block(ContractProtocolBlockEvent::ChunkLoaded {
                    chunk_x: 0,
                    chunk_z: 0,
                }),
            )
        });
        checked_rx
            .recv_timeout(StdDuration::from_secs(1))
            .expect("A publication must reach the admission hook");

        // B rebinds the same entity while A's publication is suspended between
        // admission and publication.
        assert!(handle.shared.begin_connection_attempt());
        let token_b = azalea::join::AttemptToken::mint();
        bind_stamped_attempt(&handle, &mut app, owner, token_b);

        release_tx.send(()).expect("release A publication");
        assert!(
            !publisher.join().expect("publisher must finish"),
            "A publication must be rejected after B rebinds the same entity"
        );
    }
}
