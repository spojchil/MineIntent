use std::{
    future::pending,
    sync::{atomic::AtomicUsize, mpsc as std_mpsc, Barrier, Condvar, Mutex as StdMutex},
    thread,
    time::Duration as StdDuration,
};

use super::*;
use crate::snapshot::{ExperienceSnapshot, InventorySnapshot, SelfSnapshot, WorldSnapshot};
// Block / SelfState 的生产者已删（方块变化经视口到达，位置回拉 pose 就是当前值），
// 但契约变体还在，覆盖全部 BackendEventKind 的夹具仍需构造它们。生产侧不再导入，
// 所以在测试根这里引一次，子模块经 `use super::*` 共用。契约变体的去留见后续清理。
use super::dto::contract_block_snapshot;
use mineintent_contracts::minecraft::{
    BackendEventProtocol as ContractBackendEventProtocol,
    BlockPropertyValue as ContractBlockPropertyValue, CancellationSignal, Deadline,
    HeardSoundType as ContractHeardSoundType, ProtocolBlockEvent as ContractProtocolBlockEvent,
    ProtocolEntityEvent as ContractProtocolEntityEvent,
    ProtocolSelfEvent as ContractProtocolSelfEvent,
    ProtocolSoundPayload as ContractProtocolSoundPayload,
    ProtocolSoundSource as ContractProtocolSoundSource,
    ProtocolWorldEvent as ContractProtocolWorldEvent, RelativeMovementFlags,
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

fn payload_json(event: &BackendEventEnvelope) -> serde_json::Value {
    serde_json::to_value(&event.payload).expect("strict v2 payload is serializable")
}

mod armor_formula;
mod command;
mod dispatch;
mod lifecycle;
mod light_armor;
mod observation;
mod production_support;
mod raw_reducer;
mod stamped_identity;
mod transport;
mod viewport;
