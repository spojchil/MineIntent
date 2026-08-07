use super::*;

// SelfState 现在没有生产者（`record_server_position_corrections` 已删），但契约
// 变体还在，下面覆盖全部 `BackendEventKind` 的夹具仍需要构造它。生产侧不再导入
// 这两个类型，所以在这里单独引。契约变体本身的去留见后续清理。
use mineintent_contracts::minecraft::{
    ProtocolSelfEvent as ContractProtocolSelfEvent, RelativeMovementFlags,
};

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
        // A 会被调用两次（测试先发 Entity 再发 Block），第二次 B 已经被退订。
        // 原先这里 `.expect("B subscription should be present during A callback")`
        // 在第二次必然 panic——测试之所以通过，是因为外层的 catch_unwind 把它
        // 吞掉并记成一句「listener panic isolated」。捕获藏起来的是一个 fixture
        // 缺陷，不是被保护的不变量。
        let subscription = self
            .b_subscription
            .lock()
            .expect("B subscription mutex should not be poisoned")
            .take();
        if let Some(mut subscription) = subscription {
            subscription.unsubscribe();
        }
    }
}

pub(super) fn valid_observation_payload(kind: BackendEventKind) -> BackendEventPayload {
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
            protocol_source: mineintent_contracts::minecraft::ProtocolSoundSource::NamedSoundEffect,
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
        BackendEventKind::SelfState => {
            BackendEventPayload::SelfState(ContractProtocolSelfEvent::ServerPositionCorrection {
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
            })
        }
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
        BackendEventKind::Overflow => {
            BackendEventPayload::Overflow(mineintent_contracts::minecraft::BackendOverflowPayload {
                event_type: mineintent_contracts::minecraft::OverflowType::Overflow,
                dropped_count: 1,
                dropped_kinds: vec![BackendEventKind::Entity],
            })
        }
    }
}

fn emit_test_fact(handle: &RuntimeHandle, kind: BackendEventKind) {
    handle
        .shared
        .emit(FactSource::ServerObserved, valid_observation_payload(kind));
}

fn emit_and_capture(handle: &RuntimeHandle, payload: BackendEventPayload) -> BackendEventEnvelope {
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

    let result = source.read_block_with_post_read_hook(BlockPosition { x: 0, y: 64, z: 0 }, || {
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
    let subscription = ProtocolObservationSource::subscribe(&new_source, Arc::new(NoopListener))
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
    let _subscription = ProtocolObservationSource::subscribe(&source, listener).expect("subscribe");

    let expected_entity = contract_entity_event_fixture();
    let expected_block = contract_block_event_fixture();
    let expected_sound = contract_sound_fixture();
    let raw_entity = emit_and_capture(
        &handle,
        BackendEventPayload::Entity(expected_entity.clone()),
    );
    let raw_block = emit_and_capture(&handle, BackendEventPayload::Block(expected_block.clone()));
    let raw_sound = emit_and_capture(&handle, BackendEventPayload::Sound(expected_sound.clone()));

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
    let _subscription = ProtocolObservationSource::subscribe(&source, listener).expect("subscribe");

    assert!(events.lock().is_empty());
    emit_test_fact(&handle, BackendEventKind::Entity);
    assert_eq!(events.lock().len(), 1);
}

/// 订阅者 panic 不再被隔离。
///
/// 这条记的是删掉捕获的**实际代价**：一个订阅者的缺陷会中断整趟投递，排在它后面
/// 的订阅者收不到已经发生的事实，panic 一路传到发事件的调用点。
///
/// 原测试名叫 `callback_panic_isolated_from_later_listeners_and_events`，断言的
/// 正是被移除的那条性质。留着它只会让人以为隔离还在。
#[test]
#[should_panic(expected = "observation listener test panic")]
fn callback_panic_propagates_and_stops_the_dispatch_pass() {
    let handle = RuntimeHandle::new(RunConfig::default());
    handle.shared.begin_connection_attempt();
    let source = handle.observation_source();
    let _panic_subscription =
        ProtocolObservationSource::subscribe(&source, Arc::new(PanicListener))
            .expect("panic listener subscription should succeed");
    let (listener, _events) = recording_listener();
    let _recording_subscription =
        ProtocolObservationSource::subscribe(&source, listener).expect("subscribe");

    emit_test_fact(&handle, BackendEventKind::Entity);
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
