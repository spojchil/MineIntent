//! Production adapters from the frozen Minecraft backend API to Information.
//!
//! The adapter boundary deliberately has no `RuntimeHandle` or backend-specific
//! implementation dependency. The source-port traits predate the Rust backend
//! `Result` boundary, so failures are converted to stable panics and are
//! expected to be caught by the Information runtime/provider boundary.

use std::{
    collections::VecDeque,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{Arc, Mutex, MutexGuard, Weak},
};

use mineintent_contracts::{
    information::InformationConnectionState,
    minecraft::{
        BackendError, BackendEventEnvelope, BackendEventKind, BackendEventListener,
        BackendEventPayload, BackendState, BlockPosition, BlockReadResult, MinecraftBackendApi,
        MinecraftSnapshotV1, ProtocolObservationSource, SelfPose, Subscription, Vec3Value,
    },
};

use crate::information::{
    format_utc_millis,
    geometry::{distance_between, relative_bearing, Point3},
    scope::InformationScopeSource,
    source_ports::{
        InventoryPort, InventorySlotSnapshot, InventoryStateSnapshot, PerceptionBlock,
        PerceptionBlockAt, PerceptionEntityCandidate, PerceptionPort, PerceptionPose,
        PerceptionUnloaded, SelfEffectSnapshot, SelfExperienceSnapshot, SelfVitalsPort,
        SelfVitalsSnapshot, SoundHistoryPort, SoundObservation,
    },
    InformationClock, SystemInformationClock,
};

const SNAPSHOT_UNAVAILABLE_PANIC: &str = "information adapter backend snapshot unavailable";
const OBSERVATION_UNAVAILABLE_PANIC: &str = "information adapter backend observation unavailable";
const SOUND_HISTORY_CAPACITY: usize = 20;
const NON_FINITE_PANIC: &str = "information adapter numeric value is not finite";

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn snapshot_or_panic(backend: &dyn MinecraftBackendApi) -> MinecraftSnapshotV1 {
    backend
        .snapshot()
        .unwrap_or_else(|_| panic!("{SNAPSHOT_UNAVAILABLE_PANIC}"))
}

fn observation_source_or_panic(
    backend: &dyn MinecraftBackendApi,
) -> Arc<dyn ProtocolObservationSource> {
    backend
        .observation_source()
        .unwrap_or_else(|_| panic!("{OBSERVATION_UNAVAILABLE_PANIC}"))
}

fn point3(value: Vec3Value) -> Point3 {
    Point3 {
        x: finite(value.x),
        y: finite(value.y),
        z: finite(value.z),
    }
}

fn finite(value: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        panic!("{NON_FINITE_PANIC}");
    }
}

fn exact_u64(value: u64) -> f64 {
    let converted = value as f64;
    // u64::MAX as f64 rounds to 2^64, so check the upper bound before casting back.
    if !converted.is_finite()
        || converted >= 18_446_744_073_709_551_616.0
        || converted as u64 != value
    {
        panic!("information adapter integer cannot be represented without loss");
    }
    converted
}

fn self_pose(value: SelfPose) -> PerceptionPose {
    PerceptionPose {
        position: point3(value.position),
        yaw: finite(value.yaw),
        pitch: finite(value.pitch),
    }
}

/// Snapshot-backed self vitals adapter.
pub struct BackendSelfVitalsPort {
    backend: Arc<dyn MinecraftBackendApi>,
}

impl BackendSelfVitalsPort {
    pub fn new(backend: Arc<dyn MinecraftBackendApi>) -> Self {
        Self { backend }
    }
}

impl SelfVitalsPort for BackendSelfVitalsPort {
    fn current(&self) -> SelfVitalsSnapshot {
        let snapshot = snapshot_or_panic(self.backend.as_ref());
        SelfVitalsSnapshot {
            health: finite(snapshot.self_snapshot.health),
            food: finite(snapshot.self_snapshot.food),
            food_saturation: finite(snapshot.self_snapshot.food_saturation),
            oxygen: snapshot.self_snapshot.oxygen.map(finite),
            experience: snapshot.self_snapshot.experience.map(|experience| {
                SelfExperienceSnapshot {
                    level: experience.level as f64,
                    progress: finite(experience.progress),
                    total: exact_u64(experience.total),
                }
            }),
            effects: snapshot
                .self_snapshot
                .effects
                .into_iter()
                .map(|effect| SelfEffectSnapshot {
                    name: effect.name,
                    amplifier: effect.amplifier as f64,
                    duration_ticks: effect.duration_ticks.map(|duration| duration as f64),
                })
                .collect(),
        }
    }
}

/// Snapshot-backed inventory adapter.
pub struct BackendInventoryPort {
    backend: Arc<dyn MinecraftBackendApi>,
}

impl BackendInventoryPort {
    pub fn new(backend: Arc<dyn MinecraftBackendApi>) -> Self {
        Self { backend }
    }
}

impl InventoryPort for BackendInventoryPort {
    fn current(&self) -> InventoryStateSnapshot {
        let snapshot = snapshot_or_panic(self.backend.as_ref());
        InventoryStateSnapshot {
            selected_hotbar_slot: snapshot.inventory.selected_hotbar_slot as f64,
            slots: snapshot
                .inventory
                .slots
                .into_iter()
                .map(|slot| InventorySlotSnapshot {
                    slot: slot.slot as f64,
                    item_name: slot.item_name,
                    count: slot.count as f64,
                    metadata: slot.metadata.map(|metadata| metadata as f64),
                    durability_used: slot.durability_used.map(|durability| durability as f64),
                })
                .collect(),
        }
    }
}

/// Current-epoch observation adapter.
pub struct BackendPerceptionPort {
    backend: Arc<dyn MinecraftBackendApi>,
}

impl BackendPerceptionPort {
    pub fn new(backend: Arc<dyn MinecraftBackendApi>) -> Self {
        Self { backend }
    }
}

impl PerceptionPort for BackendPerceptionPort {
    fn self_pose(&self) -> PerceptionPose {
        let source = observation_source_or_panic(self.backend.as_ref());
        source
            .self_pose()
            .map(self_pose)
            .unwrap_or_else(|_| panic!("{OBSERVATION_UNAVAILABLE_PANIC}"))
    }

    fn revision(&self) -> f64 {
        exact_u64(snapshot_or_panic(self.backend.as_ref()).snapshot_revision)
    }

    fn block_at(&self, position: Point3) -> PerceptionBlockAt {
        let source = observation_source_or_panic(self.backend.as_ref());
        let position = BlockPosition {
            x: block_coordinate(position.x),
            y: block_coordinate(position.y),
            z: block_coordinate(position.z),
        };
        match source
            .read_block(position)
            .unwrap_or_else(|_| panic!("{OBSERVATION_UNAVAILABLE_PANIC}"))
        {
            BlockReadResult::Loaded { block } => {
                let visible = !matches!(block.name.as_str(), "air" | "cave_air" | "void_air");
                PerceptionBlockAt::Block(PerceptionBlock {
                    name: block.name,
                    visible,
                    occludes: visible && !block.transparent_hint,
                })
            }
            BlockReadResult::Unloaded | BlockReadResult::OutOfWorld => {
                PerceptionBlockAt::Unloaded(PerceptionUnloaded::Unloaded)
            }
        }
    }

    fn nearby_entities(&self) -> Vec<PerceptionEntityCandidate> {
        // Keep the TS adapter's try/catch behavior: a missing snapshot means
        // there is no key to exclude, but the current observation source is
        // still authoritative for the entity list.
        let self_entity_key = self
            .backend
            .snapshot()
            .ok()
            .map(|snapshot| snapshot.self_snapshot.entity_key);
        let source = observation_source_or_panic(self.backend.as_ref());
        source
            .list_tracked_entities()
            .unwrap_or_else(|_| panic!("{OBSERVATION_UNAVAILABLE_PANIC}"))
            .into_iter()
            .filter(|entity| Some(entity.entity_key.as_str()) != self_entity_key.as_deref())
            .map(|entity| PerceptionEntityCandidate {
                entity_type: entity.entity_type,
                name: entity.name,
                username: entity.username,
                position: point3(entity.position),
                width: Some(finite(entity.width)),
                height: Some(finite(entity.height)),
            })
            .collect()
    }
}

fn block_coordinate(value: f64) -> i32 {
    assert!(
        value.is_finite() && value.fract() == 0.0,
        "information adapter block coordinate must be a finite integer"
    );
    assert!(
        value >= f64::from(i32::MIN) && value <= f64::from(i32::MAX),
        "information adapter block coordinate is outside i32 range"
    );
    value as i32
}

struct SoundEntry {
    observation: SoundObservation,
    process_session_id: String,
    connection_epoch: u64,
    world_id: String,
    dimension: Option<String>,
}

struct SoundHistoryState {
    disposed: bool,
    revision: u64,
    entries: VecDeque<SoundEntry>,
}

struct SoundHistoryInner {
    backend: Arc<dyn MinecraftBackendApi>,
    state: Mutex<SoundHistoryState>,
}

struct SoundEventListener {
    inner: Weak<SoundHistoryInner>,
}

impl BackendEventListener for SoundEventListener {
    fn on_event(&self, event: BackendEventEnvelope) {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            if let Some(inner) = self.inner.upgrade() {
                inner.record(event);
            }
        }));
    }
}

impl SoundHistoryInner {
    fn record(&self, event: BackendEventEnvelope) {
        if event.protocol != mineintent_contracts::minecraft::BackendEventProtocol::V2
            || event.kind != BackendEventKind::Sound
        {
            return;
        }
        let BackendEventPayload::Sound(payload) = &event.payload else {
            return;
        };

        {
            let state = lock_recover(&self.state);
            if state.disposed {
                return;
            }
        }

        // Never hold the history lock across a backend call. A backend snapshot
        // may synchronously re-enter the event stream.
        let snapshot = match self.backend.snapshot() {
            Ok(snapshot) => snapshot,
            Err(_) => return,
        };
        let self_snapshot = &snapshot.self_snapshot;
        let observation = SoundObservation {
            sound_name: payload.sound_name.clone(),
            category: payload.category.clone(),
            distance: finite(distance_between(
                point3(self_snapshot.position),
                point3(payload.source_position),
            )),
            direction: relative_bearing(
                finite(self_snapshot.yaw),
                point3(self_snapshot.position),
                point3(payload.source_position),
            ),
            volume: finite(payload.volume),
            pitch: finite(payload.pitch),
            observed_at: event.occurred_at.clone(),
        };
        let entry = SoundEntry {
            observation,
            process_session_id: event.process_session_id,
            connection_epoch: event.connection_epoch,
            world_id: event.world_id,
            dimension: event.dimension,
        };

        let mut state = lock_recover(&self.state);
        if state.disposed {
            return;
        }
        state.entries.push_back(entry);
        if state.entries.len() > SOUND_HISTORY_CAPACITY {
            state.entries.pop_front();
        }
        state.revision += 1;
    }
}

/// Bounded history of strict v2 sound product events.
pub struct SoundHistory {
    inner: Arc<SoundHistoryInner>,
    subscription: Mutex<Option<Box<dyn Subscription>>>,
}

impl SoundHistory {
    pub fn new(backend: Arc<dyn MinecraftBackendApi>) -> Result<Self, BackendError> {
        let inner = Arc::new(SoundHistoryInner {
            backend: backend.clone(),
            state: Mutex::new(SoundHistoryState {
                disposed: false,
                revision: 0,
                entries: VecDeque::with_capacity(SOUND_HISTORY_CAPACITY),
            }),
        });
        let listener = Arc::new(SoundEventListener {
            inner: Arc::downgrade(&inner),
        });
        let subscription = backend.subscribe(listener)?;
        Ok(Self {
            inner,
            subscription: Mutex::new(Some(subscription)),
        })
    }

    /// Linearizes disposal before releasing the backend subscription.
    pub fn dispose(&self) {
        {
            let mut state = lock_recover(&self.inner.state);
            if state.disposed {
                return;
            }
            state.disposed = true;
        }

        let subscription = lock_recover(&self.subscription).take();
        if let Some(mut subscription) = subscription {
            let _ = catch_unwind(AssertUnwindSafe(|| subscription.unsubscribe()));
        }
    }

    /// Reads the already-recorded B5 sound bundle for an explicitly supplied
    /// backend scope.  Participant frame capture passes the scope from the
    /// same atomic `MinecraftFrameFacts` DTO, so this path never performs a
    /// second backend snapshot.
    pub fn recent_for_scope(
        &self,
        process_session_id: &str,
        connection_epoch: u64,
        world_id: &str,
        dimension: Option<&str>,
        limit: f64,
    ) -> Vec<SoundObservation> {
        let limit = recent_limit(limit);
        if limit == 0 {
            return Vec::new();
        }
        let state = lock_recover(&self.inner.state);
        state
            .entries
            .iter()
            .rev()
            .filter(|entry| {
                entry.process_session_id == process_session_id
                    && entry.connection_epoch == connection_epoch
                    && entry.world_id == world_id
                    && (entry.dimension.is_none() || entry.dimension.as_deref() == dimension)
            })
            .take(limit)
            .map(|entry| entry.observation.clone())
            .collect()
    }
}

impl Drop for SoundHistory {
    fn drop(&mut self) {
        self.dispose();
    }
}

impl SoundHistoryPort for SoundHistory {
    fn recent(&self, limit: f64) -> Vec<SoundObservation> {
        let snapshot = match self.inner.backend.snapshot() {
            Ok(snapshot) => snapshot,
            Err(_) => return Vec::new(),
        };
        self.recent_for_scope(
            &snapshot.process_session_id,
            snapshot.connection_epoch,
            &snapshot.world.world_id,
            Some(snapshot.world.dimension.as_str()),
            limit,
        )
    }

    fn revision(&self) -> f64 {
        exact_u64(lock_recover(&self.inner.state).revision)
    }
}

fn recent_limit(limit: f64) -> usize {
    if !limit.is_finite() || limit <= 0.0 {
        return 0;
    }
    if limit >= usize::MAX as f64 {
        usize::MAX
    } else {
        limit as usize
    }
}

/// Backend-backed Information scope capture.
pub struct BackendInformationScopeSource {
    backend: Arc<dyn MinecraftBackendApi>,
    process_session_id: String,
    clock: Arc<dyn InformationClock>,
}

impl BackendInformationScopeSource {
    pub fn new(
        backend: Arc<dyn MinecraftBackendApi>,
        process_session_id: impl Into<String>,
    ) -> Self {
        Self::with_clock(
            backend,
            process_session_id,
            Arc::new(SystemInformationClock),
        )
    }

    pub fn with_clock(
        backend: Arc<dyn MinecraftBackendApi>,
        process_session_id: impl Into<String>,
        clock: Arc<dyn InformationClock>,
    ) -> Self {
        Self {
            backend,
            process_session_id: process_session_id.into(),
            clock,
        }
    }
}

impl InformationScopeSource for BackendInformationScopeSource {
    fn capture(&self) -> mineintent_contracts::information::InformationScopeSnapshot {
        let state = self.backend.state();
        let (connection_state, connection_epoch) = match state {
            BackendState::Connecting { epoch, .. }
            | BackendState::LoggingIn { epoch, .. }
            | BackendState::Spawning { epoch, .. } => {
                (InformationConnectionState::Connecting, epoch)
            }
            BackendState::Ready { epoch, .. } | BackendState::Dead { epoch, .. } => {
                (InformationConnectionState::Play, epoch)
            }
            BackendState::Idle
            | BackendState::Reconnecting { .. }
            | BackendState::Stopping { .. }
            | BackendState::Stopped { .. }
            | BackendState::Faulted { .. } => (InformationConnectionState::Disconnected, 0),
        };
        let (world_id, dimension) = self
            .backend
            .snapshot()
            .ok()
            .map(|snapshot| {
                (
                    (!snapshot.world.world_id.is_empty()).then_some(snapshot.world.world_id),
                    (!snapshot.world.dimension.is_empty()).then_some(snapshot.world.dimension),
                )
            })
            .unwrap_or((None, None));
        let captured_at = format_utc_millis(self.clock.now_millis(), || ())
            .unwrap_or_else(|_| panic!("information scope clock returned invalid UTC millis"));
        mineintent_contracts::information::InformationScopeSnapshot {
            process_session_id: self.process_session_id.clone(),
            connection_state,
            connection_epoch,
            world_id,
            dimension,
            ui_revision: 0,
            screen_instance_id: None,
            screen_revision: None,
            captured_at,
        }
    }
}

/// Small composition bundle for later Participant assembly.
pub struct BackendInformationAdapterBundle {
    self_vitals: Arc<BackendSelfVitalsPort>,
    inventory: Arc<BackendInventoryPort>,
    perception: Arc<BackendPerceptionPort>,
    sound_history: Arc<SoundHistory>,
    scope: Arc<BackendInformationScopeSource>,
}

impl BackendInformationAdapterBundle {
    pub fn new(
        backend: Arc<dyn MinecraftBackendApi>,
        process_session_id: impl Into<String>,
    ) -> Result<Self, BackendError> {
        Self::with_clock(
            backend,
            process_session_id,
            Arc::new(SystemInformationClock),
        )
    }

    pub fn with_clock(
        backend: Arc<dyn MinecraftBackendApi>,
        process_session_id: impl Into<String>,
        clock: Arc<dyn InformationClock>,
    ) -> Result<Self, BackendError> {
        let process_session_id = process_session_id.into();
        Ok(Self {
            self_vitals: Arc::new(BackendSelfVitalsPort::new(backend.clone())),
            inventory: Arc::new(BackendInventoryPort::new(backend.clone())),
            perception: Arc::new(BackendPerceptionPort::new(backend.clone())),
            sound_history: Arc::new(SoundHistory::new(backend.clone())?),
            scope: Arc::new(BackendInformationScopeSource::with_clock(
                backend,
                process_session_id,
                clock,
            )),
        })
    }

    pub fn self_vitals(&self) -> Arc<BackendSelfVitalsPort> {
        self.self_vitals.clone()
    }

    pub fn inventory(&self) -> Arc<BackendInventoryPort> {
        self.inventory.clone()
    }

    pub fn perception(&self) -> Arc<BackendPerceptionPort> {
        self.perception.clone()
    }

    pub fn sound_history(&self) -> Arc<SoundHistory> {
        self.sound_history.clone()
    }

    pub fn scope(&self) -> Arc<BackendInformationScopeSource> {
        self.scope.clone()
    }
}
