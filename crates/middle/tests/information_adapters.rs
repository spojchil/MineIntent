use std::{
    collections::BTreeMap,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc, Mutex, MutexGuard, Weak,
    },
    thread,
    time::Duration,
};

use mineintent_contracts::{
    information::{InformationConnectionState, RelativeDirection},
    minecraft::{
        fixture_snapshot, BackendError, BackendEventEnvelope, BackendEventKind,
        BackendEventListener, BackendEventMetadata, BackendEventPayload, BackendFailure,
        BackendFailureCode, BackendOverflowPayload, BackendReady, BackendState, BlockBoundingBox,
        BlockPosition, BlockPropertyValue, BlockReadResult, BoxFuture, DirectedViewportError,
        DirectedViewportProjection, FactSource, MinecraftBackendApi, MinecraftMotorDriverApi,
        MinecraftSnapshotV1, ObservationEventListener, OperationControl, OverflowType,
        ProtocolBlockSnapshot, ProtocolEntitySnapshot, ProtocolObservationSource,
        ProtocolSoundPayload, ProtocolSoundSource, SelfPose, Subscription, Vec3Value, ViewportRead,
    },
};

use mineintent_middle::{
    information::{
        geometry::Point3,
        scope::InformationScopeSource,
        source_ports::{
            InventoryPort, PerceptionBlock, PerceptionBlockAt, PerceptionPort, SelfVitalsPort,
            SoundHistoryPort,
        },
        InformationClock,
    },
    participant::{
        BackendInformationAdapterBundle, BackendInformationScopeSource, BackendInventoryPort,
        BackendPerceptionPort, BackendSelfVitalsPort, SoundHistory,
    },
};

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn not_ready() -> BackendError {
    BackendError::NotReady {
        state: "idle".to_owned(),
    }
}

fn backend_failure(message: &str) -> BackendError {
    BackendError::BackendFailure {
        failure: BackendFailure {
            code: BackendFailureCode::ProtocolError,
            message: message.to_owned(),
            retryable: false,
        },
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
    pose: Mutex<SelfPose>,
    entities: Mutex<Vec<ProtocolEntitySnapshot>>,
    blocks: Mutex<BTreeMap<BlockPosition, BlockReadResult>>,
}

impl FakeObservationSource {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            epoch: 4,
            pose: Mutex::new(SelfPose {
                position: Vec3Value {
                    x: 1.25,
                    y: 64.5,
                    z: -2.75,
                },
                velocity: Vec3Value::default(),
                yaw: 0.75,
                pitch: -0.125,
            }),
            entities: Mutex::new(Vec::new()),
            blocks: Mutex::new(BTreeMap::new()),
        })
    }
}

impl ProtocolObservationSource for FakeObservationSource {
    fn epoch(&self) -> u64 {
        self.epoch
    }

    fn self_pose(&self) -> Result<SelfPose, BackendError> {
        Ok(*lock_recover(&self.pose))
    }

    fn list_tracked_entities(&self) -> Result<Vec<ProtocolEntitySnapshot>, BackendError> {
        Ok(lock_recover(&self.entities).clone())
    }

    fn read_block(&self, position: BlockPosition) -> Result<BlockReadResult, BackendError> {
        Ok(lock_recover(&self.blocks)
            .get(&position)
            .cloned()
            .unwrap_or(BlockReadResult::Unloaded))
    }

    fn subscribe(
        &self,
        _listener: Arc<dyn ObservationEventListener>,
    ) -> Result<Box<dyn Subscription>, BackendError> {
        Ok(Box::new(NoopSubscription::new()))
    }

    fn read_viewport(
        &self,
        _control: OperationControl,
    ) -> BoxFuture<'_, Result<ViewportRead, BackendError>> {
        Box::pin(async { Err(not_ready()) })
    }

    fn read_directed_viewport(
        &self,
        _positions: Vec<BlockPosition>,
        _control: OperationControl,
    ) -> BoxFuture<'_, Result<DirectedViewportProjection, DirectedViewportError>> {
        Box::pin(async { Err(DirectedViewportError::Backend(not_ready())) })
    }
}

struct ListenerRegistration {
    id: usize,
    listener: Arc<dyn BackendEventListener>,
}

struct ListenerStore {
    next_id: AtomicUsize,
    listeners: Mutex<Vec<ListenerRegistration>>,
}

struct FakeSubscription {
    store: Weak<ListenerStore>,
    id: usize,
    closed: AtomicBool,
    panic_once: Arc<AtomicBool>,
}

impl Subscription for FakeSubscription {
    fn unsubscribe(&mut self) {
        if self.panic_once.swap(false, Ordering::SeqCst) {
            panic!("fake unsubscribe panic");
        }
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

impl Drop for FakeSubscription {
    fn drop(&mut self) {
        self.unsubscribe();
    }
}

struct FakeBackend {
    state: Mutex<BackendState>,
    snapshot: Mutex<Result<MinecraftSnapshotV1, BackendError>>,
    source: Mutex<Result<Arc<dyn ProtocolObservationSource>, BackendError>>,
    listeners: Arc<ListenerStore>,
    snapshot_hook: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    panic_next_snapshot: AtomicBool,
    panic_next_unsubscribe: Arc<AtomicBool>,
}

impl FakeBackend {
    fn new(snapshot: MinecraftSnapshotV1, source: Arc<dyn ProtocolObservationSource>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(BackendState::Idle),
            snapshot: Mutex::new(Ok(snapshot)),
            source: Mutex::new(Ok(source)),
            listeners: Arc::new(ListenerStore {
                next_id: AtomicUsize::new(0),
                listeners: Mutex::new(Vec::new()),
            }),
            snapshot_hook: Mutex::new(None),
            panic_next_snapshot: AtomicBool::new(false),
            panic_next_unsubscribe: Arc::new(AtomicBool::new(false)),
        })
    }

    fn emit(&self, event: BackendEventEnvelope) {
        let listeners = lock_recover(&self.listeners.listeners)
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
        lock_recover(&self.listeners.listeners).len()
    }
}

impl MinecraftBackendApi for FakeBackend {
    fn start(
        &self,
        _control: OperationControl,
    ) -> BoxFuture<'_, Result<BackendReady, BackendError>> {
        Box::pin(async { Err(not_ready()) })
    }

    fn stop(
        &self,
        _reason: String,
        _control: OperationControl,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
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
        let id = self.listeners.next_id.fetch_add(1, Ordering::SeqCst);
        lock_recover(&self.listeners.listeners).push(ListenerRegistration { id, listener });
        Ok(Box::new(FakeSubscription {
            store: Arc::downgrade(&self.listeners),
            id,
            closed: AtomicBool::new(false),
            panic_once: self.panic_next_unsubscribe.clone(),
        }))
    }

    fn observation_source(&self) -> Result<Arc<dyn ProtocolObservationSource>, BackendError> {
        lock_recover(&self.source).clone()
    }

    fn motor(&self) -> Result<Arc<dyn MinecraftMotorDriverApi>, BackendError> {
        Err(backend_failure("motor is outside B5"))
    }

    fn send_chat(&self, _message: String) -> Result<(), BackendError> {
        Err(backend_failure("chat is outside B5"))
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
    snapshot
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

fn entity(entity_key: &str, username: Option<&str>) -> ProtocolEntitySnapshot {
    ProtocolEntitySnapshot {
        entity_key: entity_key.to_owned(),
        protocol_entity_id: 1,
        entity_type: "player".to_owned(),
        name: None,
        username: username.map(str::to_owned),
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
    }
}

/// 非有限坐标不再 panic：这条声音被跳过，历史不前进，其余事件照常记录。
///
/// 原先 `finite()` 会 panic，指望 Information runtime 接住；那个接手方从未接线，
/// 于是信号被 backend dispatcher 泛泛接成「订阅者回调 panic」。非有限坐标是一次
/// 读不出来的观察，不是缺陷。
#[test]
fn non_finite_sound_position_is_skipped_without_panicking() {
    let backend = FakeBackend::new(snapshot(), FakeObservationSource::new());
    let history = SoundHistory::new(backend.clone()).unwrap();

    backend.emit(sound_event(
        1,
        "process-fixture-0001",
        1,
        "world-fixture",
        Some("minecraft:overworld"),
        f64::NAN,
    ));
    assert_eq!(history.revision(), 0.0, "非有限坐标不得进入历史");
    assert!(history.recent(5.0).is_empty());

    backend.emit(sound_event(
        2,
        "process-fixture-0001",
        1,
        "world-fixture",
        Some("minecraft:overworld"),
        -5.0,
    ));
    assert_eq!(history.revision(), 1.0, "跳过一条之后其余事件照常记录");
    assert_eq!(
        history.recent(5.0)[0].sound_name.as_deref(),
        Some("sound-2")
    );
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
            occurred_at: format!("2026-08-03T00:00:{:02}.000Z", id % 60),
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
fn oracle_vitals_and_inventory_are_snapshot_lossless_and_not_ready_is_not_defaulted() {
    let source = FakeObservationSource::new();
    let mut initial_snapshot = snapshot();
    initial_snapshot.self_snapshot.experience =
        Some(mineintent_contracts::minecraft::ExperienceSnapshot {
            level: u32::MAX,
            progress: 0.75,
            total: 9_007_199_254_740_992,
        });
    initial_snapshot.inventory.slots =
        vec![mineintent_contracts::minecraft::InventorySlotSnapshot {
            slot: u32::MAX,
            item_name: "minecraft:oak_log".to_owned(),
            count: u32::MAX,
            metadata: Some(i32::MIN),
            durability_used: Some(u32::MAX),
        }];
    let backend = FakeBackend::new(initial_snapshot, source);
    let vitals = BackendSelfVitalsPort::new(backend.clone());
    let inventory = BackendInventoryPort::new(backend.clone());
    let current = vitals.current();
    assert_eq!(current.health, 18.0);
    assert_eq!(current.experience.unwrap().total, 9_007_199_254_740_992.0);
    let inventory = inventory.current();
    assert_eq!(inventory.slots[0].slot, u32::MAX as f64);
    assert_eq!(inventory.slots[0].count, u32::MAX as f64);
    assert_eq!(inventory.slots[0].metadata, Some(i32::MIN as f64));
    assert_eq!(inventory.slots[0].durability_used, Some(u32::MAX as f64));

    let mut exactly_representable = snapshot();
    exactly_representable.self_snapshot.experience =
        Some(mineintent_contracts::minecraft::ExperienceSnapshot {
            level: 0,
            progress: 0.0,
            total: 9_007_199_254_740_994,
        });
    backend.set_snapshot(Ok(exactly_representable));
    assert_eq!(
        vitals.current().experience.expect("experience").total,
        9_007_199_254_740_994.0
    );
    let mut lossy = snapshot();
    lossy.self_snapshot.experience = Some(mineintent_contracts::minecraft::ExperienceSnapshot {
        level: 0,
        progress: 0.0,
        total: 9_007_199_254_740_993,
    });
    backend.set_snapshot(Ok(lossy));
    assert!(catch_unwind(AssertUnwindSafe(|| vitals.current())).is_err());

    backend.set_snapshot(Err(not_ready()));
    let panic =
        catch_unwind(AssertUnwindSafe(|| vitals.current())).expect_err("not-ready must fail");
    let message = panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or_default();
    assert!(message.contains("snapshot unavailable"));
}

#[test]
fn oracle_perception_maps_current_pose_revision_blocks_and_self_entity_key() {
    let source = FakeObservationSource::new();
    *lock_recover(&source.entities) = vec![
        entity("self:1", Some("MineFixture")),
        entity("2:alex", Some("alex")),
    ];
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
        BlockPosition { x: 0, y: 63, z: 0 },
        BlockReadResult::Loaded {
            block: block("air", false),
        },
    );
    lock_recover(&source.blocks).insert(
        BlockPosition { x: 0, y: 64, z: 0 },
        BlockReadResult::Loaded {
            block: block("void_air", false),
        },
    );
    lock_recover(&source.blocks).insert(
        BlockPosition { x: 0, y: 70, z: 0 },
        BlockReadResult::OutOfWorld,
    );
    let backend = FakeBackend::new(snapshot(), source);
    let perception = BackendPerceptionPort::new(backend.clone());
    let pose = perception.self_pose();
    assert_eq!(pose.yaw, 0.75);
    assert_eq!(pose.pitch, -0.125);
    assert_eq!(perception.revision(), 42.0);
    let mut exact_revision = snapshot();
    exact_revision.snapshot_revision = 9_007_199_254_740_994;
    backend.set_snapshot(Ok(exact_revision));
    assert_eq!(perception.revision(), 9_007_199_254_740_994.0);
    let mut lossy_revision = snapshot();
    lossy_revision.snapshot_revision = 9_007_199_254_740_993;
    backend.set_snapshot(Ok(lossy_revision));
    assert!(catch_unwind(AssertUnwindSafe(|| perception.revision())).is_err());
    backend.set_snapshot(Ok(snapshot()));
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
        perception.block_at(Point3 { x: 0.0, y: 62.0, z: 0.0 }),
        PerceptionBlockAt::Block(PerceptionBlock { name, visible: false, occludes: false })
            if name == "cave_air"
    ));
    assert!(matches!(
        perception.block_at(Point3 { x: 0.0, y: 63.0, z: 0.0 }),
        PerceptionBlockAt::Block(PerceptionBlock { name, visible: false, occludes: false })
            if name == "air"
    ));
    assert!(matches!(
        perception.block_at(Point3 { x: 0.0, y: 64.0, z: 0.0 }),
        PerceptionBlockAt::Block(PerceptionBlock { name, visible: false, occludes: false })
            if name == "void_air"
    ));
    assert_eq!(
        perception.block_at(Point3 {
            x: 0.0,
            y: 70.0,
            z: 0.0
        }),
        PerceptionBlockAt::Unloaded(
            mineintent_middle::information::source_ports::PerceptionUnloaded::Unloaded
        )
    );
}

#[test]
fn perception_snapshot_unavailable_keeps_ts_nearby_fallback_and_rejects_nonfinite_inputs() {
    let source = FakeObservationSource::new();
    *lock_recover(&source.entities) = vec![
        entity("self:1", Some("MineFixture")),
        entity("2:alex", Some("alex")),
    ];
    lock_recover(&source.blocks).insert(
        BlockPosition { x: 0, y: 70, z: 0 },
        BlockReadResult::OutOfWorld,
    );
    let backend = FakeBackend::new(snapshot(), source.clone());
    let perception = BackendPerceptionPort::new(backend.clone());

    assert!(matches!(
        perception.block_at(Point3 {
            x: 0.0,
            y: 70.0,
            z: 0.0
        }),
        PerceptionBlockAt::Unloaded(_)
    ));
    assert!(catch_unwind(AssertUnwindSafe(|| {
        perception.block_at(Point3 {
            x: f64::NAN,
            y: 70.0,
            z: 0.0,
        });
    }))
    .is_err());
    assert!(catch_unwind(AssertUnwindSafe(|| {
        perception.block_at(Point3 {
            x: 1.5,
            y: 70.0,
            z: 0.0,
        });
    }))
    .is_err());
    assert!(catch_unwind(AssertUnwindSafe(|| {
        perception.block_at(Point3 {
            x: f64::from(i32::MAX) + 1.0,
            y: 70.0,
            z: 0.0,
        });
    }))
    .is_err());

    backend.set_snapshot(Err(not_ready()));
    let fallback_nearby = perception.nearby_entities();
    assert_eq!(fallback_nearby.len(), 2);
    assert_eq!(fallback_nearby[0].username.as_deref(), Some("MineFixture"));
    assert_eq!(fallback_nearby[1].username.as_deref(), Some("alex"));

    backend.set_snapshot(Ok(snapshot()));
    lock_recover(&source.pose).yaw = f64::NAN;
    assert!(catch_unwind(AssertUnwindSafe(|| {
        perception.self_pose();
    }))
    .is_err());
}

#[test]
fn oracle_scope_maps_status_epoch_world_and_deterministic_utc() {
    let backend = FakeBackend::new(snapshot(), FakeObservationSource::new());
    let scope = BackendInformationScopeSource::with_clock(
        backend.clone(),
        "process-test",
        Arc::new(FixedClock(1_783_900_800_123)),
    );
    let idle = scope.capture();
    assert_eq!(
        idle.connection_state,
        InformationConnectionState::Disconnected
    );
    assert_eq!(idle.connection_epoch, 0);
    assert_eq!(idle.world_id.as_deref(), Some("world-fixture"));
    assert_eq!(idle.dimension.as_deref(), Some("minecraft:overworld"));
    assert_eq!(idle.ui_revision, 0);
    assert_eq!(idle.captured_at, "2026-07-13T00:00:00.123Z");

    backend.set_state(BackendState::Ready {
        epoch: 4,
        attempt_id: "attempt".to_owned(),
        ready_at: "2026-08-03T00:00:00Z".to_owned(),
    });
    let ready = scope.capture();
    assert_eq!(ready.connection_state, InformationConnectionState::Play);
    assert_eq!(ready.connection_epoch, 4);
    backend.set_state(BackendState::Connecting {
        epoch: 5,
        attempt_id: "attempt-2".to_owned(),
        attempt: 1,
    });
    let connecting = scope.capture();
    assert_eq!(
        connecting.connection_state,
        InformationConnectionState::Connecting
    );
    assert_eq!(connecting.connection_epoch, 5);

    for (state, epoch) in [
        (
            BackendState::LoggingIn {
                epoch: 6,
                attempt_id: "attempt-3".to_owned(),
                attempt: 1,
            },
            6,
        ),
        (
            BackendState::Spawning {
                epoch: 7,
                attempt_id: "attempt-4".to_owned(),
                attempt: 1,
            },
            7,
        ),
    ] {
        backend.set_state(state);
        let connecting = scope.capture();
        assert_eq!(
            connecting.connection_state,
            InformationConnectionState::Connecting
        );
        assert_eq!(connecting.connection_epoch, epoch);
    }
    backend.set_state(BackendState::Dead {
        epoch: 8,
        attempt_id: "attempt-5".to_owned(),
        died_at: "2026-08-03T00:00:01Z".to_owned(),
    });
    let dead = scope.capture();
    assert_eq!(dead.connection_state, InformationConnectionState::Play);
    assert_eq!(dead.connection_epoch, 8);

    let invalid_clock = BackendInformationScopeSource::with_clock(
        backend.clone(),
        "process-test",
        Arc::new(FixedClock(i64::MAX)),
    );
    assert!(catch_unwind(AssertUnwindSafe(|| invalid_clock.capture())).is_err());

    backend.set_snapshot(Err(not_ready()));
    let unavailable = scope.capture();
    assert_eq!(unavailable.world_id, None);
    assert_eq!(unavailable.dimension, None);
    assert_eq!(
        unavailable.connection_state,
        InformationConnectionState::Play
    );
    assert_eq!(unavailable.connection_epoch, 8);
}

#[test]
fn oracle_sound_history_maps_pose_order_capacity_and_dimension_wildcard() {
    let backend = FakeBackend::new(snapshot(), FakeObservationSource::new());
    let history = SoundHistory::new(backend.clone()).expect("strict sound subscription");
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
    let recent = history.recent(3.0);
    assert_eq!(recent.len(), 3);
    assert_eq!(recent[0].sound_name.as_deref(), Some("sound-21"));
    assert_eq!(recent[1].sound_name.as_deref(), Some("sound-20"));
    assert_eq!(recent[2].sound_name.as_deref(), Some("sound-19"));
    assert_eq!(recent[0].distance, 5.0);
    assert_eq!(recent[0].direction, RelativeDirection::Ahead);
    assert_eq!(recent[0].category.as_deref(), Some("ambient"));
    assert_eq!(recent[0].volume, 21.25);
    assert_eq!(recent[0].pitch, 0.5);
    assert_eq!(recent[0].observed_at, "2026-08-03T00:00:21.000Z");
    assert!(!history
        .recent(20.0)
        .iter()
        .any(|sound| sound.sound_name.as_deref() == Some("sound-1")));

    for (id, process, epoch, world, dimension) in [
        (
            100,
            "other-process",
            1,
            "world-fixture",
            Some("minecraft:overworld"),
        ),
        (
            101,
            "process-fixture-0001",
            2,
            "world-fixture",
            Some("minecraft:overworld"),
        ),
        (
            102,
            "process-fixture-0001",
            1,
            "other-world",
            Some("minecraft:overworld"),
        ),
        (
            103,
            "process-fixture-0001",
            1,
            "world-fixture",
            Some("minecraft:the_nether"),
        ),
        (104, "process-fixture-0001", 1, "world-fixture", None),
    ] {
        backend.emit(sound_event(id, process, epoch, world, dimension, -5.0));
    }
    let filtered = history.recent(20.0);
    assert_eq!(filtered[0].sound_name.as_deref(), Some("sound-104"));
    assert!(!filtered.iter().any(|sound| {
        matches!(
            sound.sound_name.as_deref(),
            Some("sound-100" | "sound-101" | "sound-102" | "sound-103")
        )
    }));

    let mut switched_scope = snapshot();
    switched_scope.process_session_id = "process-fixture-0002".to_owned();
    switched_scope.connection_epoch = 9;
    switched_scope.world.world_id = "world-next".to_owned();
    switched_scope.world.dimension = "minecraft:the_end".to_owned();
    backend.set_snapshot(Ok(switched_scope));
    backend.emit(sound_event(
        106,
        "process-fixture-0002",
        9,
        "world-next",
        Some("minecraft:the_end"),
        -5.0,
    ));
    backend.emit(sound_event(
        107,
        "process-fixture-0001",
        1,
        "world-fixture",
        Some("minecraft:overworld"),
        -5.0,
    ));
    let switched = history.recent(20.0);
    assert_eq!(switched[0].sound_name.as_deref(), Some("sound-106"));
    assert!(!switched
        .iter()
        .any(|sound| sound.sound_name.as_deref() == Some("sound-107")));
    backend.set_snapshot(Ok(snapshot()));

    assert_eq!(history.recent(3.9).len(), 3);
    assert!(history.recent(-1.0).is_empty());
    assert!(history.recent(f64::NAN).is_empty());
    assert!(history.recent(f64::INFINITY).len() <= 20);

    let mut optional = sound_event(105, "process-fixture-0001", 1, "world-fixture", None, -5.0);
    if let BackendEventPayload::Sound(payload) = &mut optional.payload {
        payload.sound_name = None;
        payload.category = None;
        payload.source_position = Vec3Value {
            x: 0.0,
            y: 64.0,
            z: 0.0,
        };
        payload.volume = -0.0;
        payload.pitch = f64::MAX;
    }
    let mut moved_snapshot = snapshot();
    moved_snapshot.self_snapshot.position = Vec3Value {
        x: 0.0,
        y: 64.0,
        z: 0.0,
    };
    backend.set_snapshot(Ok(moved_snapshot));
    backend.emit(optional);
    let observation = history.recent(1.0).pop().expect("optional sound");
    assert_eq!(observation.sound_name, None);
    assert_eq!(observation.category, None);
    assert_eq!(observation.distance, 0.0);
    assert!(observation.volume.is_sign_negative());
    assert_eq!(observation.pitch, f64::MAX);
}

#[test]
fn sound_history_ignores_non_sound_and_snapshot_unavailable_without_revision() {
    let backend = FakeBackend::new(snapshot(), FakeObservationSource::new());
    let history = SoundHistory::new(backend.clone()).unwrap();
    let mut mismatched = sound_event(
        1,
        "process-fixture-0001",
        1,
        "world-fixture",
        Some("minecraft:overworld"),
        -5.0,
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
        -5.0,
    ));
    assert_eq!(history.revision(), 0.0);
    assert!(history.recent(20.0).is_empty());
}

#[ignore = "实验分支：dispose 中的退订 panic 捕获已移除"]
#[test]
fn sound_history_dispose_drop_and_callback_panic_are_safe() {
    let backend = FakeBackend::new(snapshot(), FakeObservationSource::new());
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
        -5.0,
    ));
    assert_eq!(history.revision(), 0.0);
    drop(history);
    assert_eq!(backend.listener_count(), 0);

    let dropped = SoundHistory::new(backend.clone()).unwrap();
    assert_eq!(backend.listener_count(), 1);
    drop(dropped);
    assert_eq!(backend.listener_count(), 0);

    let panic_on_unsubscribe = SoundHistory::new(backend.clone()).unwrap();
    backend.panic_next_unsubscribe.store(true, Ordering::SeqCst);
    panic_on_unsubscribe.dispose();
    assert_eq!(backend.listener_count(), 0);
    drop(panic_on_unsubscribe);

    let history = SoundHistory::new(backend.clone()).unwrap();
    backend.panic_next_snapshot.store(true, Ordering::SeqCst);
    let result = catch_unwind(AssertUnwindSafe(|| {
        backend.emit(sound_event(
            2,
            "process-fixture-0001",
            1,
            "world-fixture",
            Some("minecraft:overworld"),
            -5.0,
        ));
    }));
    // 适配器**不再**自己隔离 panic：隔离是 backend dispatcher 的职责，而且它会
    // 报告（facade 的 "listener panic isolated"）。适配器在里面再包一层的唯一
    // 效果，是让我们自己代码里的缺陷被吞掉、那句报告永远不触发。
    //
    // 这里的 FakeBackend 直接调用 listener，不模拟那层隔离，所以 panic 会一路
    // 传到本测试的 catch_unwind——这正是要断言的。
    assert!(
        result.is_err(),
        "adapter must not swallow a panic; isolation belongs to the backend dispatcher"
    );
    assert_eq!(history.revision(), 0.0);
}

#[test]
fn sound_history_reentrant_snapshot_and_dispose_race_have_completion_signals() {
    let backend = FakeBackend::new(snapshot(), FakeObservationSource::new());
    let history = Arc::new(SoundHistory::new(backend.clone()).unwrap());
    let nested_backend = backend.clone();
    backend.set_snapshot_hook(Some(Box::new(move || {
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
    done_rx
        .recv_timeout(Duration::from_millis(250))
        .expect("reentrant callback completion");
    emitter.join().expect("reentrant emitter termination");
    assert_eq!(history.revision(), 2.0);
    let reentrant_order = history.recent(2.0);
    assert_eq!(
        reentrant_order
            .iter()
            .map(|sound| sound.sound_name.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("sound-1"), Some("sound-2")]
    );

    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let release_rx = Arc::new(Mutex::new(release_rx));
    backend.set_snapshot_hook(Some(Box::new(move || {
        let _ = entered_tx.send(());
        let _ = lock_recover(&release_rx).recv_timeout(Duration::from_millis(250));
    })));
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let emitter_backend = backend.clone();
    let emitter = thread::spawn(move || {
        emitter_backend.emit(sound_event(
            3,
            "process-fixture-0001",
            1,
            "world-fixture",
            Some("minecraft:overworld"),
            -2.0,
        ));
        let _ = done_tx.send(());
    });
    entered_rx
        .recv_timeout(Duration::from_millis(250))
        .expect("callback entered snapshot");
    history.dispose();
    release_tx.send(()).expect("release in-flight callback");
    done_rx
        .recv_timeout(Duration::from_millis(250))
        .expect("dispose race completion");
    emitter.join().expect("dispose-race emitter termination");
    assert_eq!(history.revision(), 2.0);
}

#[test]
fn bundle_constructs_all_adapters_and_uses_injected_clock() {
    let backend = FakeBackend::new(snapshot(), FakeObservationSource::new());
    let bundle = BackendInformationAdapterBundle::with_clock(
        backend,
        "process-test",
        Arc::new(FixedClock(0)),
    )
    .unwrap();
    assert_eq!(bundle.self_vitals().current().health, 18.0);
    assert_eq!(bundle.inventory().current().selected_hotbar_slot, 2.0);
    assert_eq!(bundle.perception().revision(), 42.0);
    assert_eq!(
        bundle.scope().capture().captured_at,
        "1970-01-01T00:00:00.000Z"
    );
}
