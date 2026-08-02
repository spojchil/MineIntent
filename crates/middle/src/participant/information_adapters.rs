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
        let self_entity_key = snapshot_or_panic(self.backend.as_ref())
            .self_snapshot
            .entity_key;
        let source = observation_source_or_panic(self.backend.as_ref());
        source
            .list_tracked_entities()
            .unwrap_or_else(|_| panic!("{OBSERVATION_UNAVAILABLE_PANIC}"))
            .into_iter()
            .filter(|entity| entity.entity_key != self_entity_key)
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
                entry.process_session_id == snapshot.process_session_id
                    && entry.connection_epoch == snapshot.connection_epoch
                    && entry.world_id == snapshot.world.world_id
                    && (entry.dimension.is_none()
                        || entry.dimension.as_deref() == Some(snapshot.world.dimension.as_str()))
            })
            .take(limit)
            .map(|entry| entry.observation.clone())
            .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc::{self, Receiver, SyncSender},
        },
        thread,
        time::Duration,
    };

    use mineintent_contracts::minecraft::{
        fixture_snapshot, BackendEventMetadata, BackendFailure, BackendFailureCode,
        BackendOverflowPayload, BackendReady, BlockBoundingBox, BlockPropertyValue,
        DirectedViewportError, DirectedViewportProjection, FactSource, MinecraftMotorDriverApi,
        OperationControl, OverflowType, ProtocolBlockSnapshot, ProtocolEntitySnapshot,
        ProtocolSoundPayload, ProtocolSoundSource, ViewportRead,
    };

    fn backend_failure(message: &str) -> BackendError {
        BackendError::BackendFailure {
            failure: BackendFailure {
                code: BackendFailureCode::ProtocolError,
                message: message.to_owned(),
                retryable: false,
            },
        }
    }

    fn not_ready() -> BackendError {
        BackendError::NotReady {
            state: "idle".to_owned(),
        }
    }

    struct FixedClock(i64);

    impl InformationClock for FixedClock {
        fn now_millis(&self) -> i64 {
            self.0
        }
    }

    struct NoopSubscription {
        closed: AtomicBool,
    }

    impl NoopSubscription {
        fn new() -> Self {
            Self {
                closed: AtomicBool::new(false),
            }
        }
    }

    impl Subscription for NoopSubscription {
        fn unsubscribe(&mut self) {
            self.closed.store(true, Ordering::SeqCst);
        }

        fn is_closed(&self) -> bool {
            self.closed.load(Ordering::SeqCst)
        }
    }

    struct FakeObservationSource {
        epoch: u64,
        pose: Mutex<Result<SelfPose, BackendError>>,
        entities: Mutex<Result<Vec<ProtocolEntitySnapshot>, BackendError>>,
        blocks: Mutex<BTreeMap<BlockPosition, BlockReadResult>>,
    }

    impl FakeObservationSource {
        fn new() -> Self {
            Self {
                epoch: 4,
                pose: Mutex::new(Ok(SelfPose {
                    position: Vec3Value {
                        x: 1.25,
                        y: 64.5,
                        z: -2.75,
                    },
                    velocity: Vec3Value::default(),
                    yaw: 0.75,
                    pitch: -0.125,
                })),
                entities: Mutex::new(Ok(Vec::new())),
                blocks: Mutex::new(BTreeMap::new()),
            }
        }
    }

    impl ProtocolObservationSource for FakeObservationSource {
        fn epoch(&self) -> u64 {
            self.epoch
        }

        fn self_pose(&self) -> Result<SelfPose, BackendError> {
            lock_recover(&self.pose).clone()
        }

        fn list_tracked_entities(&self) -> Result<Vec<ProtocolEntitySnapshot>, BackendError> {
            lock_recover(&self.entities).clone()
        }

        fn read_block(&self, position: BlockPosition) -> Result<BlockReadResult, BackendError> {
            Ok(lock_recover(&self.blocks)
                .get(&position)
                .cloned()
                .unwrap_or(BlockReadResult::Unloaded))
        }

        fn subscribe(
            &self,
            _listener: Arc<dyn mineintent_contracts::minecraft::ObservationEventListener>,
        ) -> Result<Box<dyn Subscription>, BackendError> {
            Ok(Box::new(NoopSubscription::new()))
        }

        fn read_viewport(
            &self,
            _control: OperationControl,
        ) -> mineintent_contracts::minecraft::BoxFuture<'_, Result<ViewportRead, BackendError>>
        {
            Box::pin(async { Err(not_ready()) })
        }

        fn read_directed_viewport(
            &self,
            _positions: Vec<BlockPosition>,
            _control: OperationControl,
        ) -> mineintent_contracts::minecraft::BoxFuture<
            '_,
            Result<DirectedViewportProjection, DirectedViewportError>,
        > {
            Box::pin(async { Err(DirectedViewportError::Backend(not_ready())) })
        }
    }

    struct ListenerRegistration {
        id: usize,
        listener: Arc<dyn BackendEventListener>,
    }

    struct ListenerStore {
        listeners: Mutex<Vec<ListenerRegistration>>,
    }

    struct TestSubscription {
        store: Weak<ListenerStore>,
        id: usize,
        closed: AtomicBool,
    }

    impl Subscription for TestSubscription {
        fn unsubscribe(&mut self) {
            if !self.closed.swap(true, Ordering::SeqCst) {
                if let Some(store) = self.store.upgrade() {
                    lock_recover(&store.listeners).retain(|entry| entry.id != self.id);
                }
            }
        }

        fn is_closed(&self) -> bool {
            self.closed.load(Ordering::SeqCst)
        }
    }

    struct FakeBackend {
        state: Mutex<BackendState>,
        snapshot: Mutex<Result<MinecraftSnapshotV1, BackendError>>,
        source: Mutex<Result<Arc<dyn ProtocolObservationSource>, BackendError>>,
        store: Arc<ListenerStore>,
        next_listener_id: AtomicUsize,
        snapshot_hook: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
        panic_next_snapshot: AtomicBool,
    }

    impl FakeBackend {
        fn new(
            snapshot: MinecraftSnapshotV1,
            source: Arc<dyn ProtocolObservationSource>,
        ) -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::new(BackendState::Idle),
                snapshot: Mutex::new(Ok(snapshot)),
                source: Mutex::new(Ok(source)),
                store: Arc::new(ListenerStore {
                    listeners: Mutex::new(Vec::new()),
                }),
                next_listener_id: AtomicUsize::new(0),
                snapshot_hook: Mutex::new(None),
                panic_next_snapshot: AtomicBool::new(false),
            })
        }

        fn emit(&self, event: BackendEventEnvelope) {
            let listeners = lock_recover(&self.store.listeners)
                .iter()
                .map(|entry| entry.listener.clone())
                .collect::<Vec<_>>();
            for listener in listeners {
                listener.on_event(event.clone());
            }
        }

        fn set_state(&self, state: BackendState) {
            *lock_recover(&self.state) = state;
        }

        fn set_snapshot(&self, snapshot: Result<MinecraftSnapshotV1, BackendError>) {
            *lock_recover(&self.snapshot) = snapshot;
        }

        fn set_snapshot_hook(&self, hook: Option<Box<dyn Fn() + Send + Sync>>) {
            *lock_recover(&self.snapshot_hook) = hook;
        }

        fn listener_count(&self) -> usize {
            lock_recover(&self.store.listeners).len()
        }
    }

    impl MinecraftBackendApi for FakeBackend {
        fn start(
            &self,
            _control: OperationControl,
        ) -> mineintent_contracts::minecraft::BoxFuture<'_, Result<BackendReady, BackendError>>
        {
            Box::pin(async { Err(not_ready()) })
        }

        fn stop(
            &self,
            _reason: String,
            _control: OperationControl,
        ) -> mineintent_contracts::minecraft::BoxFuture<'_, Result<(), BackendError>> {
            Box::pin(async { Err(not_ready()) })
        }

        fn state(&self) -> BackendState {
            lock_recover(&self.state).clone()
        }

        fn snapshot(&self) -> Result<MinecraftSnapshotV1, BackendError> {
            if self.panic_next_snapshot.swap(false, Ordering::SeqCst) {
                panic!("fake snapshot panic")
            }
            let hook = lock_recover(&self.snapshot_hook).take();
            if let Some(hook) = hook {
                hook();
            }
            lock_recover(&self.snapshot).clone()
        }

        fn subscribe(
            &self,
            listener: Arc<dyn BackendEventListener>,
        ) -> Result<Box<dyn Subscription>, BackendError> {
            let id = self.next_listener_id.fetch_add(1, Ordering::SeqCst);
            lock_recover(&self.store.listeners).push(ListenerRegistration { id, listener });
            Ok(Box::new(TestSubscription {
                store: Arc::downgrade(&self.store),
                id,
                closed: AtomicBool::new(false),
            }))
        }

        fn observation_source(&self) -> Result<Arc<dyn ProtocolObservationSource>, BackendError> {
            lock_recover(&self.source).clone()
        }

        fn motor(&self) -> Result<Arc<dyn MinecraftMotorDriverApi>, BackendError> {
            Err(backend_failure("motor not used by information adapters"))
        }

        fn send_chat(&self, _message: String) -> Result<(), BackendError> {
            Err(backend_failure("chat not used by information adapters"))
        }
    }

    fn snapshot() -> MinecraftSnapshotV1 {
        let mut snapshot = fixture_snapshot();
        snapshot.snapshot_revision = 42;
        snapshot.self_snapshot.entity_key = "self:1".to_owned();
        snapshot.self_snapshot.position = Vec3Value {
            x: 0.0,
            y: 64.0,
            z: 0.0,
        };
        snapshot.self_snapshot.yaw = 0.0;
        snapshot.self_snapshot.pitch = 0.0;
        snapshot.self_snapshot.health = 15.0;
        snapshot.self_snapshot.food = 17.0;
        snapshot.self_snapshot.food_saturation = 3.5;
        snapshot
    }

    fn sound_event(
        id: usize,
        process_session_id: &str,
        epoch: u64,
        world_id: &str,
        dimension: Option<&str>,
        source_z: f64,
    ) -> BackendEventEnvelope {
        BackendEventEnvelope::new(
            BackendEventMetadata {
                id: format!("sound-{id}"),
                occurred_at: format!("2026-08-03T00:00:{id:02}.000Z"),
                process_session_id: process_session_id.to_owned(),
                connection_epoch: epoch,
                connection_attempt_id: "attempt".to_owned(),
                world_id: world_id.to_owned(),
                dimension: dimension.map(str::to_owned),
            },
            BackendEventKind::Sound,
            FactSource::ServerObserved,
            BackendEventPayload::Sound(ProtocolSoundPayload {
                event_type: mineintent_contracts::minecraft::HeardSoundType::Heard,
                sound_key: format!("minecraft:sound_{id}"),
                sound_name: Some(format!("sound-{id}")),
                sound_id: None,
                category: Some("ambient".to_owned()),
                source_position: Vec3Value {
                    x: 0.0,
                    y: 64.0,
                    z: source_z,
                },
                volume: 0.25 + id as f64,
                pitch: 0.5,
                protocol_source: ProtocolSoundSource::NamedSoundEffect,
            }),
        )
    }

    #[test]
    fn mechanical_vitals_and_inventory_read_current_snapshot_without_defaults() {
        let source = Arc::new(FakeObservationSource::new());
        let mut snapshot = snapshot();
        snapshot.self_snapshot.oxygen = None;
        snapshot.self_snapshot.experience = None;
        snapshot.self_snapshot.effects = Vec::new();
        snapshot.inventory.selected_hotbar_slot = 2;
        snapshot.inventory.slots = vec![mineintent_contracts::minecraft::InventorySlotSnapshot {
            slot: u32::MAX,
            item_name: "minecraft:oak_log".to_owned(),
            count: u32::MAX,
            metadata: Some(i32::MIN),
            durability_used: Some(u32::MAX),
        }];
        let backend = FakeBackend::new(snapshot, source);
        let vitals = BackendSelfVitalsPort::new(backend.clone());
        let inventory = BackendInventoryPort::new(backend.clone());

        let current = vitals.current();
        assert_eq!(current.health, 15.0);
        assert_eq!(current.oxygen, None);
        assert_eq!(current.experience, None);
        assert!(current.effects.is_empty());
        let current_inventory = inventory.current();
        assert_eq!(current_inventory.selected_hotbar_slot, 2.0);
        assert_eq!(current_inventory.slots[0].slot, u32::MAX as f64);
        assert_eq!(current_inventory.slots[0].count, u32::MAX as f64);
        assert_eq!(current_inventory.slots[0].metadata, Some(i32::MIN as f64));
        assert_eq!(
            current_inventory.slots[0].durability_used,
            Some(u32::MAX as f64)
        );
        backend.set_snapshot(Err(not_ready()));
        assert!(catch_unwind(AssertUnwindSafe(|| vitals.current())).is_err());
        assert!(catch_unwind(AssertUnwindSafe(|| inventory.current())).is_err());
    }

    #[test]
    fn perception_maps_pose_revision_blocks_and_excludes_self_by_entity_key() {
        let source = Arc::new(FakeObservationSource::new());
        let self_entity = ProtocolEntitySnapshot {
            entity_key: "self:1".to_owned(),
            protocol_entity_id: 1,
            entity_type: "player".to_owned(),
            name: Some("MineFixture".to_owned()),
            username: Some("MineFixture".to_owned()),
            uuid: None,
            position: Vec3Value::default(),
            velocity: Vec3Value::default(),
            yaw: 0.0,
            pitch: 0.0,
            head_yaw: None,
            width: 0.6,
            height: 1.8,
            on_ground: true,
            pose: None,
            held_item_name: None,
            equipment: Vec::new(),
            valid: true,
        };
        let other = ProtocolEntitySnapshot {
            entity_key: "2:alex".to_owned(),
            protocol_entity_id: 2,
            entity_type: "player".to_owned(),
            name: None,
            username: Some("alex".to_owned()),
            uuid: None,
            position: Vec3Value {
                x: 3.0,
                y: 64.0,
                z: 4.0,
            },
            velocity: Vec3Value::default(),
            yaw: 0.0,
            pitch: 0.0,
            head_yaw: None,
            width: 0.6,
            height: 1.8,
            on_ground: true,
            pose: None,
            held_item_name: None,
            equipment: Vec::new(),
            valid: true,
        };
        *lock_recover(&source.entities) = Ok(vec![self_entity, other]);
        lock_recover(&source.blocks).insert(
            BlockPosition { x: 0, y: 60, z: 0 },
            BlockReadResult::Loaded {
                block: block("stone", false),
            },
        );
        lock_recover(&source.blocks).insert(
            BlockPosition { x: 0, y: 61, z: 0 },
            BlockReadResult::Loaded {
                block: block("glass", true),
            },
        );
        lock_recover(&source.blocks).insert(
            BlockPosition { x: 0, y: 62, z: 0 },
            BlockReadResult::Loaded {
                block: block("cave_air", false),
            },
        );
        lock_recover(&source.blocks).insert(
            BlockPosition { x: 0, y: 70, z: 0 },
            BlockReadResult::OutOfWorld,
        );
        let backend = FakeBackend::new(snapshot(), source);
        let perception = BackendPerceptionPort::new(backend);

        assert_eq!(perception.self_pose().yaw, 0.75);
        assert_eq!(perception.self_pose().pitch, -0.125);
        assert_eq!(perception.revision(), 42.0);
        let nearby = perception.nearby_entities();
        assert_eq!(nearby.len(), 1);
        assert_eq!(nearby[0].username.as_deref(), Some("alex"));
        assert_eq!(nearby[0].width, Some(0.6));
        assert!(matches!(
            perception.block_at(Point3 {
                x: 0.0,
                y: 60.0,
                z: 0.0
            }),
            PerceptionBlockAt::Block(PerceptionBlock {
                visible: true,
                occludes: true,
                ..
            })
        ));
        assert!(matches!(
            perception.block_at(Point3 {
                x: 0.0,
                y: 61.0,
                z: 0.0
            }),
            PerceptionBlockAt::Block(PerceptionBlock {
                visible: true,
                occludes: false,
                ..
            })
        ));
        assert!(matches!(
            perception.block_at(Point3 {
                x: 0.0,
                y: 62.0,
                z: 0.0
            }),
            PerceptionBlockAt::Block(PerceptionBlock {
                name,
                visible: false,
                occludes: false,
            }) if name == "cave_air"
        ));
        assert_eq!(
            perception.block_at(Point3 {
                x: 0.0,
                y: 70.0,
                z: 0.0
            }),
            PerceptionBlockAt::Unloaded(PerceptionUnloaded::Unloaded)
        );
    }

    fn block(name: &str, transparent_hint: bool) -> ProtocolBlockSnapshot {
        ProtocolBlockSnapshot {
            position: BlockPosition { x: 0, y: 0, z: 0 },
            name: name.to_owned(),
            state_id: 1,
            properties: BTreeMap::<String, BlockPropertyValue>::new(),
            collision_shapes: Vec::new(),
            transparent_hint,
            bounding_box: BlockBoundingBox::Block,
        }
    }

    #[test]
    fn scope_maps_connection_states_world_and_fixed_utc_capture() {
        let backend = FakeBackend::new(snapshot(), Arc::new(FakeObservationSource::new()));
        let source = BackendInformationScopeSource::with_clock(
            backend.clone(),
            "process-test",
            Arc::new(FixedClock(1_783_900_800_123)),
        );
        let idle = source.capture();
        assert_eq!(
            idle.connection_state,
            InformationConnectionState::Disconnected
        );
        assert_eq!(idle.connection_epoch, 0);
        assert_eq!(idle.world_id.as_deref(), Some("world-fixture"));
        assert_eq!(idle.captured_at, "2026-07-13T00:00:00.123Z");
        backend.set_state(BackendState::Ready {
            epoch: 4,
            attempt_id: "attempt".to_owned(),
            ready_at: "2026-08-03T00:00:00Z".to_owned(),
        });
        let ready = source.capture();
        assert_eq!(ready.connection_state, InformationConnectionState::Play);
        assert_eq!(ready.connection_epoch, 4);
        backend.set_state(BackendState::Connecting {
            epoch: 5,
            attempt_id: "attempt-2".to_owned(),
            attempt: 1,
        });
        let connecting = source.capture();
        assert_eq!(
            connecting.connection_state,
            InformationConnectionState::Connecting
        );
        assert_eq!(connecting.connection_epoch, 5);
        backend.set_snapshot(Err(not_ready()));
        let unavailable = source.capture();
        assert_eq!(unavailable.world_id, None);
        assert_eq!(unavailable.dimension, None);
    }

    #[test]
    fn sound_history_maps_event_pose_revision_capacity_order_and_scope_filters() {
        let backend = FakeBackend::new(snapshot(), Arc::new(FakeObservationSource::new()));
        let history = SoundHistory::new(backend.clone()).expect("subscription should succeed");
        assert_eq!(history.revision(), 0.0);
        for id in 1..=21 {
            backend.emit(sound_event(
                id,
                "process-fixture-0001",
                1,
                "world-fixture",
                Some("minecraft:overworld"),
                -5.0,
            ));
        }
        assert_eq!(history.revision(), 21.0);
        assert_eq!(history.recent(20.0).len(), 20);
        let recent = history.recent(3.0);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].sound_name.as_deref(), Some("sound-21"));
        assert_eq!(recent[1].sound_name.as_deref(), Some("sound-20"));
        assert_eq!(recent[2].sound_name.as_deref(), Some("sound-19"));
        assert_eq!(recent[0].distance, 5.0);
        assert_eq!(
            recent[0].direction,
            mineintent_contracts::information::RelativeDirection::Ahead
        );
        assert_eq!(recent[0].observed_at, "2026-08-03T00:00:21.000Z");
        assert!(!history
            .recent(20.0)
            .iter()
            .any(|observation| observation.sound_name.as_deref() == Some("sound-1")));

        backend.emit(sound_event(
            100,
            "other-process",
            1,
            "world-fixture",
            Some("minecraft:overworld"),
            -2.0,
        ));
        backend.emit(sound_event(
            101,
            "process-fixture-0001",
            2,
            "world-fixture",
            Some("minecraft:overworld"),
            -2.0,
        ));
        backend.emit(sound_event(
            102,
            "process-fixture-0001",
            1,
            "other-world",
            Some("minecraft:overworld"),
            -2.0,
        ));
        backend.emit(sound_event(
            103,
            "process-fixture-0001",
            1,
            "world-fixture",
            Some("minecraft:the_nether"),
            -2.0,
        ));
        backend.emit(sound_event(
            104,
            "process-fixture-0001",
            1,
            "world-fixture",
            None,
            -2.0,
        ));
        let filtered = history.recent(20.0);
        assert_eq!(filtered[0].sound_name.as_deref(), Some("sound-104"));
        assert!(!filtered.iter().any(|sound| {
            matches!(
                sound.sound_name.as_deref(),
                Some("sound-100" | "sound-101" | "sound-102" | "sound-103")
            )
        }));
    }

    #[test]
    fn sound_history_ignores_non_sound_or_mismatched_events_and_snapshot_unavailable() {
        let backend = FakeBackend::new(snapshot(), Arc::new(FakeObservationSource::new()));
        let history = SoundHistory::new(backend.clone()).unwrap();
        let mut mismatched = sound_event(
            1,
            "process-fixture-0001",
            1,
            "world-fixture",
            Some("minecraft:overworld"),
            -2.0,
        );
        mismatched.kind = BackendEventKind::Chat;
        backend.emit(mismatched);
        backend.emit(BackendEventEnvelope::new(
            BackendEventMetadata {
                id: "overflow".to_owned(),
                occurred_at: "2026-08-03T00:00:00.000Z".to_owned(),
                process_session_id: "process-fixture-0001".to_owned(),
                connection_epoch: 1,
                connection_attempt_id: "attempt".to_owned(),
                world_id: "world-fixture".to_owned(),
                dimension: Some("minecraft:overworld".to_owned()),
            },
            BackendEventKind::Overflow,
            FactSource::ServerObserved,
            BackendEventPayload::Overflow(BackendOverflowPayload {
                event_type: OverflowType::Overflow,
                dropped_count: 1,
                dropped_kinds: vec![BackendEventKind::Sound],
            }),
        ));
        assert_eq!(history.revision(), 0.0);
        backend.set_snapshot(Err(not_ready()));
        backend.emit(sound_event(
            2,
            "process-fixture-0001",
            1,
            "world-fixture",
            Some("minecraft:overworld"),
            -2.0,
        ));
        assert_eq!(history.revision(), 0.0);
        assert!(history.recent(20.0).is_empty());
    }

    #[test]
    fn sound_history_dispose_and_drop_stop_delivery() {
        let backend = FakeBackend::new(snapshot(), Arc::new(FakeObservationSource::new()));
        let history = SoundHistory::new(backend.clone()).unwrap();
        assert_eq!(backend.listener_count(), 1);
        history.dispose();
        history.dispose();
        backend.emit(sound_event(
            1,
            "process-fixture-0001",
            1,
            "world-fixture",
            Some("minecraft:overworld"),
            -2.0,
        ));
        assert_eq!(history.revision(), 0.0);
        drop(history);
        assert_eq!(backend.listener_count(), 0);
    }

    #[test]
    fn sound_history_reentrant_snapshot_does_not_hold_history_lock() {
        let backend = FakeBackend::new(snapshot(), Arc::new(FakeObservationSource::new()));
        let history = Arc::new(SoundHistory::new(backend.clone()).unwrap());
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let nested_backend = backend.clone();
        backend.set_snapshot_hook(Some(Box::new(move || {
            let _ = started_tx.send(());
            nested_backend.emit(sound_event(
                2,
                "process-fixture-0001",
                1,
                "world-fixture",
                Some("minecraft:overworld"),
                -3.0,
            ));
        })));
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let emitter_backend = backend.clone();
        let emitter = thread::spawn(move || {
            emitter_backend.emit(sound_event(
                1,
                "process-fixture-0001",
                1,
                "world-fixture",
                Some("minecraft:overworld"),
                -2.0,
            ));
            let _ = done_tx.send(());
        });
        started_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("reentrant snapshot hook should complete");
        done_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("reentrant event should finish");
        emitter.join().expect("emitter should terminate");
        assert_eq!(history.revision(), 2.0);
    }

    #[test]
    fn sound_history_catches_callback_panic_and_dispose_race_is_linearized() {
        let backend = FakeBackend::new(snapshot(), Arc::new(FakeObservationSource::new()));
        let history = Arc::new(SoundHistory::new(backend.clone()).unwrap());
        backend.panic_next_snapshot.store(true, Ordering::SeqCst);
        backend.emit(sound_event(
            1,
            "process-fixture-0001",
            1,
            "world-fixture",
            Some("minecraft:overworld"),
            -2.0,
        ));
        assert_eq!(history.revision(), 0.0);

        let (started_tx, started_rx): (SyncSender<()>, Receiver<()>) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let release_rx = Arc::new(Mutex::new(release_rx));
        let hook_release_rx = release_rx.clone();
        backend.set_snapshot_hook(Some(Box::new(move || {
            let _ = started_tx.send(());
            let _ = lock_recover(&hook_release_rx).recv_timeout(Duration::from_millis(250));
        })));
        let emitter_backend = backend.clone();
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let emitter = thread::spawn(move || {
            emitter_backend.emit(sound_event(
                2,
                "process-fixture-0001",
                1,
                "world-fixture",
                Some("minecraft:overworld"),
                -2.0,
            ));
            let _ = done_tx.send(());
        });
        started_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("in-flight callback should reach snapshot");
        history.dispose();
        release_tx.send(()).expect("release callback");
        done_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("in-flight callback should finish");
        emitter.join().expect("emitter should terminate");
        assert_eq!(history.revision(), 0.0);
    }

    #[test]
    fn bundle_constructs_all_adapters_with_injected_clock() {
        let backend = FakeBackend::new(snapshot(), Arc::new(FakeObservationSource::new()));
        let bundle = BackendInformationAdapterBundle::with_clock(
            backend,
            "process-test",
            Arc::new(FixedClock(0)),
        )
        .unwrap();
        assert_eq!(bundle.self_vitals().current().health, 15.0);
        assert_eq!(bundle.inventory().current().selected_hotbar_slot, 2.0);
        assert_eq!(bundle.perception().revision(), 42.0);
        assert_eq!(
            bundle.scope().capture().captured_at,
            "1970-01-01T00:00:00.000Z"
        );
    }
}
