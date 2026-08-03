use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use mineintent_contracts::minecraft::{
    BackendError, BackendEventEnvelope, BackendEventKind, BackendEventListener,
    BackendEventMetadata, BackendEventPayload, BackendState, FactSource, HeardSoundType,
    MinecraftBackendApi, MinecraftFrameFacts, MinecraftMotorDriverApi, MinecraftSnapshotV1,
    OperationControl, ProtocolChatEvent, ProtocolObservationSource, ProtocolSoundPayload,
    ProtocolSoundSource, Subscription, Vec3Value,
};

use mineintent_middle::{
    participant::{
        ParticipantFrameSource, ParticipantScope, ParticipantSourceError,
        ProductionParticipantFrameSource,
    },
    speech::{
        Addressing, AddressingEvidence, PlayerChatMessage, PlayerChatProtocol, PlayerChatSender,
        PlayerChatWorld,
    },
};

struct FrameBackend {
    facts: Mutex<MinecraftFrameFacts>,
    listeners: Arc<Mutex<Vec<(usize, Arc<dyn BackendEventListener>)>>>,
    next_listener: AtomicUsize,
    capture_calls: AtomicUsize,
    snapshot_calls: AtomicUsize,
}

impl FrameBackend {
    fn new() -> Arc<Self> {
        let mut snapshot = mineintent_contracts::minecraft::fixture_snapshot();
        snapshot.process_session_id = "production-process".to_owned();
        snapshot.connection_epoch = 7;
        snapshot.connection_attempt_id = "production-attempt".to_owned();
        snapshot.world.world_id = "production-world".to_owned();
        snapshot.world.dimension = "minecraft:overworld".to_owned();
        snapshot.self_snapshot.yaw = std::f64::consts::FRAC_PI_2;
        snapshot.self_snapshot.pitch = -std::f64::consts::FRAC_PI_4;
        snapshot.inventory.selected_hotbar_slot = 8;
        snapshot.inventory.slots = vec![
            mineintent_contracts::minecraft::InventorySlotSnapshot {
                slot: 36,
                item_name: "minecraft:stone".to_owned(),
                count: 3,
                metadata: None,
                durability_used: None,
            },
            mineintent_contracts::minecraft::InventorySlotSnapshot {
                slot: 44,
                item_name: "minecraft:torch".to_owned(),
                count: 1,
                metadata: None,
                durability_used: None,
            },
            mineintent_contracts::minecraft::InventorySlotSnapshot {
                slot: 45,
                item_name: "minecraft:shield".to_owned(),
                count: 1,
                metadata: None,
                durability_used: None,
            },
        ];
        Arc::new(Self {
            facts: Mutex::new(MinecraftFrameFacts {
                snapshot,
                armor: Some(20),
                light: Some(15),
            }),
            listeners: Arc::new(Mutex::new(Vec::new())),
            next_listener: AtomicUsize::new(1),
            capture_calls: AtomicUsize::new(0),
            snapshot_calls: AtomicUsize::new(0),
        })
    }

    fn scope(&self) -> ParticipantScope {
        let snapshot = self.facts.lock().unwrap().snapshot.clone();
        ParticipantScope::new(
            snapshot.process_session_id,
            snapshot.connection_epoch,
            snapshot.world.world_id,
            Some(snapshot.world.dimension),
        )
    }

    fn set_facts(&self, facts: MinecraftFrameFacts) {
        *self.facts.lock().unwrap() = facts;
    }

    fn update_scope(&self, scope: &ParticipantScope) {
        let mut facts = self.facts.lock().unwrap();
        facts.snapshot.process_session_id = scope.process_session_id.clone();
        facts.snapshot.connection_epoch = scope.connection_epoch;
        facts.snapshot.world.world_id = scope.world_id.clone();
        facts.snapshot.world.dimension = scope.dimension.clone().unwrap();
    }

    fn capture_call_count(&self) -> usize {
        self.capture_calls.load(Ordering::SeqCst)
    }

    fn snapshot_call_count(&self) -> usize {
        self.snapshot_calls.load(Ordering::SeqCst)
    }

    fn emit(&self, event: BackendEventEnvelope) {
        let listeners = self
            .listeners
            .lock()
            .unwrap()
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect::<Vec<_>>();
        for listener in listeners {
            listener.on_event(event.clone());
        }
    }

    fn listener_count(&self) -> usize {
        self.listeners.lock().unwrap().len()
    }
}

struct FrameSubscription {
    listeners: Arc<Mutex<Vec<(usize, Arc<dyn BackendEventListener>)>>>,
    id: usize,
    closed: bool,
}

impl Subscription for FrameSubscription {
    fn unsubscribe(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.listeners
            .lock()
            .unwrap()
            .retain(|(id, _)| *id != self.id);
    }

    fn is_closed(&self) -> bool {
        self.closed
    }
}

impl MinecraftBackendApi for FrameBackend {
    fn start(
        &self,
        _control: OperationControl,
    ) -> mineintent_contracts::minecraft::BoxFuture<
        '_,
        Result<mineintent_contracts::minecraft::BackendReady, BackendError>,
    > {
        Box::pin(async {
            Err(BackendError::NotReady {
                state: "test backend is externally ready".to_owned(),
            })
        })
    }

    fn stop(
        &self,
        _reason: String,
        _control: OperationControl,
    ) -> mineintent_contracts::minecraft::BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async { Ok(()) })
    }

    fn state(&self) -> BackendState {
        BackendState::Idle
    }

    fn snapshot(&self) -> Result<MinecraftSnapshotV1, BackendError> {
        self.snapshot_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.facts.lock().unwrap().snapshot.clone())
    }

    fn capture_frame_facts(&self) -> Result<MinecraftFrameFacts, BackendError> {
        self.capture_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.facts.lock().unwrap().clone())
    }

    fn subscribe(
        &self,
        listener: Arc<dyn BackendEventListener>,
    ) -> Result<Box<dyn Subscription>, BackendError> {
        let id = self.next_listener.fetch_add(1, Ordering::SeqCst);
        self.listeners.lock().unwrap().push((id, listener));
        Ok(Box::new(FrameSubscription {
            listeners: Arc::clone(&self.listeners),
            id,
            closed: false,
        }))
    }

    fn observation_source(&self) -> Result<Arc<dyn ProtocolObservationSource>, BackendError> {
        Err(BackendError::NotReady {
            state: "test observation source unused".to_owned(),
        })
    }

    fn motor(&self) -> Result<Arc<dyn MinecraftMotorDriverApi>, BackendError> {
        Err(BackendError::NotReady {
            state: "test motor unused".to_owned(),
        })
    }

    fn send_chat(&self, _message: String) -> Result<(), BackendError> {
        Ok(())
    }
}

fn make_scope(process: &str, epoch: u64, world: &str, dimension: &str) -> ParticipantScope {
    ParticipantScope::new(process, epoch, world, Some(dimension.to_owned()))
}

fn chat_event(
    id: &str,
    scope: &ParticipantScope,
    sender: &str,
    text: &str,
    at: &str,
) -> BackendEventEnvelope {
    BackendEventEnvelope::new(
        BackendEventMetadata {
            id: id.to_owned(),
            occurred_at: at.to_owned(),
            process_session_id: scope.process_session_id.clone(),
            connection_epoch: scope.connection_epoch,
            connection_attempt_id: "production-attempt".to_owned(),
            world_id: scope.world_id.clone(),
            dimension: scope.dimension.clone(),
        },
        BackendEventKind::Chat,
        FactSource::ServerObserved,
        BackendEventPayload::Chat(ProtocolChatEvent {
            sender_username: Some(sender.to_owned()),
            plain_text: text.to_owned(),
            position: Some(mineintent_contracts::minecraft::ChatPosition::Chat),
            verified: Some(true),
        }),
    )
}

fn player_message(
    id: &str,
    scope: &ParticipantScope,
    sender: &str,
    text: &str,
    at: &str,
) -> PlayerChatMessage {
    PlayerChatMessage {
        protocol: PlayerChatProtocol::V1,
        source_event_id: id.to_owned(),
        occurred_at: at.to_owned(),
        sender: PlayerChatSender {
            username: sender.to_owned(),
        },
        text: text.to_owned(),
        verified: Some(true),
        addressing: Addressing {
            addressed_to_participant: true,
            evidence: vec![AddressingEvidence::ExplicitName],
        },
        world: PlayerChatWorld {
            world_id: scope.world_id.clone(),
            dimension: scope.dimension.clone(),
            connection_epoch: scope.connection_epoch,
        },
    }
}

#[test]
fn production_source_maps_one_atomic_frame_and_reuses_sound_history() {
    let backend = FrameBackend::new();
    let scope = backend.scope();
    let source = ProductionParticipantFrameSource::new(backend.clone()).unwrap();
    let context = source.chat_context(&scope).unwrap();
    assert_eq!(context.participant_username, "MineFixture");
    assert_eq!(context.online_player_usernames, vec!["Observer"]);

    backend.emit(BackendEventEnvelope::new(
        BackendEventMetadata {
            id: "sound-1".to_owned(),
            occurred_at: "2026-08-03T00:00:04Z".to_owned(),
            process_session_id: scope.process_session_id.clone(),
            connection_epoch: scope.connection_epoch,
            connection_attempt_id: "production-attempt".to_owned(),
            world_id: scope.world_id.clone(),
            dimension: scope.dimension.clone(),
        },
        BackendEventKind::Sound,
        FactSource::ServerObserved,
        BackendEventPayload::Sound(ProtocolSoundPayload {
            event_type: HeardSoundType::Heard,
            sound_key: "minecraft:block.note_block.harp".to_owned(),
            sound_name: Some("note_block.harp".to_owned()),
            sound_id: None,
            category: Some("block".to_owned()),
            source_position: Vec3Value {
                x: 3.0,
                y: 64.0,
                z: -2.5,
            },
            volume: 1.0,
            pitch: 1.0,
            protocol_source: ProtocolSoundSource::NamedSoundEffect,
        }),
    ));

    let capture = source.capture(&scope).unwrap();
    assert_eq!(backend.capture_call_count(), 1);
    assert_eq!(capture.at, "2026-08-01T00:00:03Z");
    assert_eq!(capture.pose.yaw_degrees, 90.0);
    assert_eq!(capture.pose.pitch_degrees, -45.0);
    assert_eq!(capture.status.unwrap().armor, Some(20));
    assert_eq!(capture.hotbar.selected, 8);
    assert_eq!(capture.hotbar.slots[&0].0, "minecraft:stone");
    assert_eq!(capture.hotbar.slots[&8].1, 1);
    assert_eq!(capture.hotbar.off_hand.unwrap().0, "minecraft:shield");
    assert_eq!(capture.light, Some(15));
    assert_eq!(capture.sound.unwrap().recent_sounds.unwrap().len(), 1);
    let snapshots_before_second_capture = backend.snapshot_call_count();

    let mut facts = backend.facts.lock().unwrap().clone();
    facts.armor = Some(0);
    facts.light = Some(0);
    backend.set_facts(facts);
    let zero_capture = source.capture(&scope).unwrap();
    assert_eq!(
        backend.snapshot_call_count(),
        snapshots_before_second_capture,
        "frame capture must not call backend snapshot in addition to frame facts"
    );
    assert_eq!(zero_capture.status.unwrap().armor, None);
    assert_eq!(zero_capture.light, Some(0));
    assert_eq!(backend.capture_call_count(), 2);

    let mut facts = backend.facts.lock().unwrap().clone();
    facts.armor = None;
    facts.light = None;
    backend.set_facts(facts);
    let missing_capture = source.capture(&scope).unwrap();
    assert_eq!(missing_capture.status.unwrap().armor, None);
    assert_eq!(missing_capture.light, None);
}

#[test]
fn production_chat_sequence_preserves_duplicate_text_fifo_trigger_identity() {
    let backend = FrameBackend::new();
    let scope = backend.scope();
    let source = ProductionParticipantFrameSource::new(backend.clone()).unwrap();
    let first = player_message(
        "duplicate-1",
        &scope,
        "Alice",
        "@MineFixture same text",
        "2026-08-03T00:03:01Z",
    );
    let second = player_message(
        "duplicate-2",
        &scope,
        "Alice",
        "@MineFixture same text",
        "2026-08-03T00:03:02Z",
    );
    backend.emit(chat_event(
        "duplicate-1",
        &scope,
        "Alice",
        "@MineFixture same text",
        "2026-08-03T00:03:01Z",
    ));
    backend.emit(chat_event(
        "duplicate-2",
        &scope,
        "Alice",
        "@MineFixture same text",
        "2026-08-03T00:03:02Z",
    ));
    source.retain_trigger(&scope, &first).unwrap();
    source.retain_trigger(&scope, &second).unwrap();
    for sequence in 3..=12 {
        backend.emit(chat_event(
            &format!("ordinary-{sequence}"),
            &scope,
            "Alice",
            &format!("ordinary-{sequence}"),
            &format!("2026-08-03T00:03:{sequence:02}Z"),
        ));
    }

    let capture = source.capture(&scope).unwrap();
    let duplicate_messages = capture
        .unread_chat
        .iter()
        .filter(|chat| chat.message.text == "@MineFixture same text")
        .collect::<Vec<_>>();
    assert_eq!(duplicate_messages.len(), 2);
    assert_eq!(
        duplicate_messages
            .iter()
            .map(|chat| chat.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2],
        "queued wakes retain duplicate text by source identity and FIFO sequence"
    );
    assert_eq!(duplicate_messages[0].message.at, first.occurred_at);
    assert_eq!(duplicate_messages[1].message.at, second.occurred_at);
    assert_eq!(capture.unread_chat_omitted, 2);

    source.release_trigger(&scope, &first);
    source.release_trigger(&scope, &second);
    let after_release = source.capture(&scope).unwrap();
    assert_eq!(after_release.unread_chat.len(), 8);
    assert_eq!(after_release.unread_chat_omitted, 4);
}

#[test]
fn production_chat_pin_preserves_old_trigger_and_dispose_stops_delivery() {
    let backend = FrameBackend::new();
    let scope = backend.scope();
    let source = ProductionParticipantFrameSource::new(backend.clone()).unwrap();
    let trigger = player_message(
        "chat-1",
        &scope,
        "Alice",
        "@MineFixture old trigger",
        "2026-08-03T00:01:01Z",
    );
    backend.emit(chat_event(
        "chat-1",
        &scope,
        "Alice",
        "@MineFixture old trigger",
        "2026-08-03T00:01:01Z",
    ));
    source.retain_trigger(&scope, &trigger).unwrap();
    for sequence in 2..=10 {
        backend.emit(chat_event(
            &format!("chat-{sequence}"),
            &scope,
            "Alice",
            &format!("ordinary-{sequence}"),
            &format!("2026-08-03T00:01:{sequence:02}Z"),
        ));
    }
    let capture = source.capture(&scope).unwrap();
    assert_eq!(capture.unread_chat.len(), 9);
    assert_eq!(capture.unread_chat[0].sequence, 1);
    assert_eq!(
        capture.unread_chat[0].message.text,
        "@MineFixture old trigger"
    );
    assert_eq!(capture.unread_chat_omitted, 1);
    assert_eq!(
        capture
            .unread_chat
            .iter()
            .filter(|chat| chat.message.text == "@MineFixture old trigger")
            .count(),
        1
    );

    source.release_trigger(&scope, &trigger);
    let released = source.capture(&scope).unwrap();
    assert_eq!(released.unread_chat.len(), 8);
    assert_eq!(released.unread_chat_omitted, 2);
    assert!(released
        .unread_chat
        .iter()
        .all(|chat| chat.message.text != "@MineFixture old trigger"));

    let switched = make_scope(
        "production-process-2",
        8,
        "production-world-2",
        "minecraft:nether",
    );
    backend.update_scope(&switched);
    backend.emit(chat_event(
        "stale-old",
        &scope,
        "Alice",
        "must be dropped",
        "2026-08-03T00:02:01Z",
    ));
    backend.emit(chat_event(
        "new-scope",
        &switched,
        "Bob",
        "new scope",
        "2026-08-03T00:02:02Z",
    ));
    let switched_capture = source.capture(&switched).unwrap();
    assert_eq!(switched_capture.unread_chat.len(), 1);
    assert_eq!(switched_capture.unread_chat[0].message.text, "new scope");
    assert_eq!(switched_capture.unread_chat_omitted, 0);

    assert!(matches!(
        source.capture(&scope),
        Err(ParticipantSourceError::StaleScope(_))
    ));
    let after_stale_capture = source.capture(&switched).unwrap();
    assert_eq!(after_stale_capture.unread_chat.len(), 1);
    assert_eq!(after_stale_capture.unread_chat[0].message.text, "new scope");

    assert!(matches!(
        source.chat_context(&scope),
        Err(ParticipantSourceError::StaleScope(_))
    ));
    let after_stale_context = source.capture(&switched).unwrap();
    assert_eq!(after_stale_context.unread_chat.len(), 1);
    assert_eq!(after_stale_context.unread_chat[0].message.text, "new scope");

    source.dispose();
    assert!(source.is_disposed());
    assert_eq!(backend.listener_count(), 0);
    backend.emit(chat_event(
        "late",
        &switched,
        "Alice",
        "must not deliver",
        "2026-08-03T00:02:03Z",
    ));
    assert!(matches!(
        source.capture(&switched),
        Err(ParticipantSourceError::Failed(_))
    ));
}

#[test]
fn production_source_rejects_invalid_atomic_values_and_scope() {
    let backend = FrameBackend::new();
    let scope = backend.scope();
    let source = ProductionParticipantFrameSource::new(backend.clone()).unwrap();

    let mut facts = backend.facts.lock().unwrap().clone();
    facts.armor = Some(21);
    backend.set_facts(facts.clone());
    assert!(matches!(
        source.capture(&scope),
        Err(ParticipantSourceError::Invalid(_))
    ));

    facts.armor = Some(20);
    facts.light = Some(16);
    backend.set_facts(facts.clone());
    assert!(matches!(
        source.capture(&scope),
        Err(ParticipantSourceError::Invalid(_))
    ));

    facts.light = None;
    facts.snapshot.inventory.slots[0].slot = 46;
    backend.set_facts(facts.clone());
    assert!(matches!(
        source.capture(&scope),
        Err(ParticipantSourceError::Invalid(_))
    ));

    facts.snapshot.inventory.slots[0].slot = 36;
    facts
        .snapshot
        .inventory
        .slots
        .push(facts.snapshot.inventory.slots[0].clone());
    backend.set_facts(facts.clone());
    assert!(matches!(
        source.capture(&scope),
        Err(ParticipantSourceError::Invalid(_))
    ));

    facts.snapshot.inventory.slots.truncate(1);
    facts.snapshot.inventory.slots[0].count = 0;
    backend.set_facts(facts.clone());
    assert!(matches!(
        source.capture(&scope),
        Err(ParticipantSourceError::Invalid(_))
    ));

    facts.snapshot.inventory.slots[0].count = 1;
    facts.snapshot.inventory.slots[0].item_name.clear();
    backend.set_facts(facts.clone());
    assert!(matches!(
        source.capture(&scope),
        Err(ParticipantSourceError::Invalid(_))
    ));

    facts.snapshot.inventory.slots[0].item_name = "minecraft:stone".to_owned();
    facts.snapshot.inventory.selected_hotbar_slot = 9;
    backend.set_facts(facts.clone());
    assert!(matches!(
        source.capture(&scope),
        Err(ParticipantSourceError::Invalid(_))
    ));

    facts.snapshot.inventory.selected_hotbar_slot = 0;
    facts.snapshot.process_session_id = "different-process".to_owned();
    backend.set_facts(facts.clone());
    assert!(matches!(
        source.capture(&scope),
        Err(ParticipantSourceError::StaleScope(_))
    ));

    facts.snapshot.process_session_id = scope.process_session_id.clone();
    facts.snapshot.connection_epoch = scope.connection_epoch + 1;
    backend.set_facts(facts.clone());
    assert!(matches!(
        source.capture(&scope),
        Err(ParticipantSourceError::StaleScope(_))
    ));

    facts.snapshot.connection_epoch = scope.connection_epoch;
    facts.snapshot.world.world_id = "different-world".to_owned();
    backend.set_facts(facts.clone());
    assert!(matches!(
        source.capture(&scope),
        Err(ParticipantSourceError::StaleScope(_))
    ));

    facts.snapshot.world.world_id = scope.world_id.clone();
    facts.snapshot.world.dimension = "minecraft:the_end".to_owned();
    backend.set_facts(facts);
    assert!(matches!(
        source.capture(&scope),
        Err(ParticipantSourceError::StaleScope(_))
    ));
}
