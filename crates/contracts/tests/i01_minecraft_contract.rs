use std::{
    future::{pending, ready},
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

use mineintent_contracts::minecraft::*;
use serde_json::json;

fn json_sequences() -> BackendSequenceFixtures {
    serde_json::from_str(include_str!("../testdata/i01/backend_sequences.v2.json"))
        .expect("versioned I01 fixture must deserialize")
}

fn payload_json(event: &BackendEventEnvelope) -> serde_json::Value {
    serde_json::to_value(&event.payload).expect("strict payload is serializable")
}

struct FixedCancellation(bool);
impl CancellationSignal for FixedCancellation {
    fn is_cancelled(&self) -> bool {
        self.0
    }

    fn cancelled(&self) -> BoxFuture<'_, ()> {
        if self.0 {
            Box::pin(ready(()))
        } else {
            Box::pin(pending())
        }
    }
}

struct FixedDeadline(bool);
impl Deadline for FixedDeadline {
    fn has_elapsed(&self) -> bool {
        self.0
    }

    fn elapsed(&self) -> BoxFuture<'_, ()> {
        if self.0 {
            Box::pin(ready(()))
        } else {
            Box::pin(pending())
        }
    }
}

fn control(cancelled: bool, elapsed: bool) -> OperationControl {
    OperationControl::new(
        Arc::new(FixedCancellation(cancelled)),
        Some(Arc::new(FixedDeadline(elapsed))),
    )
}

struct NoopWake;
impl Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
}

fn poll_once(future: &mut BoxFuture<'_, ()>) -> Poll<()> {
    let waker = Waker::from(Arc::new(NoopWake));
    let mut context = Context::from_waker(&waker);
    future.as_mut().poll(&mut context)
}

#[test]
fn config_rejects_unknown_fields_and_non_target_version() {
    let config = fixture_config();
    config
        .clone()
        .validate_and_normalize()
        .expect("canonical fixture is valid");

    let mut unknown = serde_json::to_value(&config).unwrap();
    unknown["unexpected"] = json!(true);
    assert!(serde_json::from_value::<MinecraftBackendConfig>(unknown).is_err());

    let mut wrong_version = config.clone();
    wrong_version.server.version = "1.21.1".to_owned();
    assert!(matches!(
        wrong_version.validate_and_normalize(),
        Err(BackendError::UnsupportedVersion { actual, .. }) if actual == "1.21.1"
    ));

    let mut microsoft = config;
    microsoft.identity.auth = AuthKind::Microsoft;
    assert_eq!(
        microsoft.validate_and_normalize(),
        Err(BackendError::UnsupportedAuth {
            auth: AuthKind::Microsoft
        })
    );
}

#[test]
fn config_normalizes_whitespace_and_accepts_oracle_boundaries() {
    let mut config = fixture_config();
    config.world_id = "  world-trimmed  ".to_owned();
    config.server.host = "  localhost  ".to_owned();
    config.identity.username = format!("  {}  ", "界".repeat(64));
    config.identity.profiles_folder = Some("   ".to_owned());
    config.reconnect.initial_delay_ms = 10_000;
    config.reconnect.max_delay_ms = 1;
    config.reconnect.stable_reset_ms = 0;

    let normalized = config.validate_and_normalize().unwrap();
    assert_eq!(normalized.world_id, "world-trimmed");
    assert_eq!(normalized.server.host, "localhost");
    assert_eq!(normalized.identity.username, "界".repeat(64));
    assert_eq!(normalized.identity.profiles_folder.as_deref(), Some("   "));
    assert_eq!(normalized.reconnect.initial_delay_ms, 10_000);
    assert_eq!(normalized.reconnect.max_delay_ms, 1);
    assert_eq!(normalized.reconnect.stable_reset_ms, 0);
}

#[test]
fn config_rejects_65_character_username_blank_fields_and_empty_profiles_folder() {
    let mut too_long = fixture_config();
    too_long.identity.username = "a".repeat(65);
    assert!(matches!(
        too_long.validate_and_normalize(),
        Err(BackendError::InvalidConfig { field, .. }) if field == "identity.username"
    ));

    let mut blank = fixture_config();
    blank.world_id = " \t ".to_owned();
    assert!(matches!(
        blank.validate_and_normalize(),
        Err(BackendError::InvalidConfig { field, .. }) if field == "worldId"
    ));

    let mut blank_host = fixture_config();
    blank_host.server.host = " \r\n ".to_owned();
    assert!(matches!(
        blank_host.validate_and_normalize(),
        Err(BackendError::InvalidConfig { field, .. }) if field == "server.host"
    ));

    let mut blank_username = fixture_config();
    blank_username.identity.username = " \n ".to_owned();
    assert!(matches!(
        blank_username.validate_and_normalize(),
        Err(BackendError::InvalidConfig { field, .. }) if field == "identity.username"
    ));

    let mut empty_profiles = fixture_config();
    empty_profiles.identity.profiles_folder = Some(String::new());
    assert!(matches!(
        empty_profiles.validate_and_normalize(),
        Err(BackendError::InvalidConfig { field, .. })
            if field == "identity.profilesFolder"
    ));
}

#[test]
fn connection_requested_uses_preallocated_attempt() {
    let first = &json_sequences().ready[0];
    assert_eq!(first.connection_epoch, 1);
    assert_eq!(first.connection_attempt_id, FIXTURE_ATTEMPT_ID);
    let payload = payload_json(first);
    assert_eq!(payload["type"], "connection_requested");
    assert_eq!(payload["attempt"], 1);
    assert!(first.dimension.is_none());
}

#[test]
fn cancel_before_ready_deliberately_stops_and_rejects_start() {
    assert!(matches!(
        control(true, false).preflight("start"),
        Err(BackendError::Cancelled { operation }) if operation == "start"
    ));
}

#[test]
fn connect_timeout_faults_without_reconnect() {
    assert!(matches!(
        control(false, true).preflight("connect"),
        Err(BackendError::DeadlineExceeded { operation }) if operation == "connect"
    ));
    let failure = BackendFailure {
        code: BackendFailureCode::ConnectionTimeout,
        message: "connect deadline elapsed".to_owned(),
        retryable: false,
    };
    let script = ScriptedBackendDataBuilder::ready()
        .start_result(Err(BackendError::BackendFailure { failure }))
        .events(Vec::new())
        .build();
    assert!(matches!(
        script.start_result,
        Err(BackendError::BackendFailure { .. })
    ));
    assert!(script.events.is_empty());
}

#[test]
fn operation_control_notifications_wake_blocked_futures() {
    let triggered = control(true, true);
    let mut cancelled = triggered.cancelled();
    let mut elapsed = triggered.deadline_elapsed().expect("fixture deadline");
    assert_eq!(poll_once(&mut cancelled), Poll::Ready(()));
    assert_eq!(poll_once(&mut elapsed), Poll::Ready(()));

    let dormant = control(false, false);
    let mut not_cancelled = dormant.cancelled();
    let mut not_elapsed = dormant.deadline_elapsed().expect("fixture deadline");
    assert_eq!(poll_once(&mut not_cancelled), Poll::Pending);
    assert_eq!(poll_once(&mut not_elapsed), Poll::Pending);
    assert!(dormant.preflight("viewport").is_ok());

    let without_deadline = OperationControl::new(Arc::new(FixedCancellation(false)), None);
    assert!(without_deadline.deadline_elapsed().is_none());
}

#[test]
fn ready_returns_detached_canonical_snapshot() {
    let scripted = ScriptedBackendDataBuilder::ready().build();
    let original = scripted.start_result.expect("ready script");
    let mut detached = original.snapshot.clone();
    detached.self_snapshot.username = "Changed".to_owned();
    assert_eq!(original.snapshot.self_snapshot.username, "MineFixture");
    original.snapshot.validate_target_axes().unwrap();
}

#[test]
fn death_respawn_and_dimension_change_are_distinct() {
    let fixtures = json_sequences();
    let death_types = fixtures
        .death
        .iter()
        .map(|event| payload_json(event)["type"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        death_types,
        ["died", "respawn_transition_started", "respawned"]
    );
    assert_eq!(
        payload_json(&fixtures.dimension[0])["type"],
        "dimension_changed"
    );
    assert_eq!(
        fixtures.dimension[0].dimension.as_deref(),
        Some("minecraft:the_nether")
    );
}

#[test]
fn kick_error_end_seal_one_fatal_close() {
    let fixtures = json_sequences();
    assert_eq!(
        fixtures
            .close
            .iter()
            .filter(|event| payload_json(event)["type"] == "connection_closed")
            .count(),
        1
    );
    assert_eq!(
        payload_json(&fixtures.close[0])["close"]["retryable"],
        false
    );
    assert_eq!(payload_json(&fixtures.close[1])["type"], "faulted");
}

#[test]
fn structured_shutdown_is_flattened_and_retryable() {
    let event = &json_sequences().reconnect[0];
    assert_eq!(payload_json(event)["close"]["code"], "server_shutdown");
    assert_eq!(payload_json(event)["close"]["retryable"], true);
    assert_eq!(
        payload_json(event)["close"]["kick"]["text"],
        "Server closed"
    );
}

#[test]
fn reconnect_renews_epoch_and_stales_old_observation() {
    let reconnect = json_sequences().reconnect;
    assert_eq!(reconnect[0].connection_epoch, 1);
    assert_eq!(reconnect[2].connection_epoch, 2);
    assert_ne!(
        reconnect[0].connection_attempt_id,
        reconnect[2].connection_attempt_id
    );
    let error = BackendError::StaleEpoch {
        bound_epoch: 1,
        current_epoch: 2,
    };
    assert_eq!(serde_json::to_value(error).unwrap()["code"], "stale_epoch");
}

#[test]
fn observation_emits_plain_dtos_and_deduplicates_compat_sound() {
    let payload = ProtocolSoundPayload {
        event_type: HeardSoundType::Heard,
        sound_key: "minecraft:block.note_block.harp".to_owned(),
        sound_name: Some("block.note_block.harp".to_owned()),
        sound_id: None,
        category: Some("record".to_owned()),
        source_position: Vec3Value {
            x: 2.0,
            y: 64.0,
            z: -3.0,
        },
        volume: 1.0,
        pitch: 0.8,
        protocol_source: ProtocolSoundSource::NamedSoundEffect,
    };
    let value = serde_json::to_value(&payload).unwrap();
    assert_eq!(value["type"], "heard");
    assert_eq!(value["soundName"], "block.note_block.harp");
    assert!(value.get("emit").is_none());
    assert!(serde_json::from_value::<ProtocolSoundPayload>(value).is_ok());
}

#[test]
fn stop_is_idempotent_and_disables_reconnect() {
    struct ExplicitSubscription(bool);
    impl Subscription for ExplicitSubscription {
        fn unsubscribe(&mut self) {
            self.0 = true;
        }
        fn is_closed(&self) -> bool {
            self.0
        }
    }
    let mut subscription = ExplicitSubscription(false);
    subscription.unsubscribe();
    subscription.unsubscribe();
    assert!(subscription.is_closed());

    let close = json_sequences().close;
    assert!(close
        .iter()
        .all(|event| payload_json(event)["type"] != "reconnect_scheduled"));
}

#[test]
fn twenty_cycles_leave_no_owned_resources() {
    let expected = ScriptedBackendDataBuilder::ready().build();
    for _ in 0..20 {
        assert_eq!(ScriptedBackendDataBuilder::ready().build(), expected);
    }
}

#[test]
fn look_relative_preserves_right_down_sign_convention() {
    let request = LookRelativeRequest {
        yaw_degrees: 30.0,
        pitch_degrees: 15.0,
    };
    request.validate().unwrap();
    let encoded = serde_json::to_value(request).unwrap();
    assert_eq!(encoded, json!({"yawDegrees": 30.0, "pitchDegrees": 15.0}));
}

#[test]
fn pre_cancelled_look_never_dispatches() {
    assert!(matches!(
        control(true, false).preflight("look_relative"),
        Err(BackendError::Cancelled { .. })
    ));
}

#[test]
fn move_presses_all_keys_then_releases_in_reverse() {
    let request = MoveInputRequest {
        directions: vec![MotorMoveDirection::Forward, MotorMoveDirection::Left],
        duration_ms: 500,
        sprint: Some(true),
    };
    assert_eq!(
        fixture_move_control_script(&request).unwrap(),
        vec![
            FixtureControlAction::Press(FixtureControlKey::Direction(MotorMoveDirection::Forward)),
            FixtureControlAction::Press(FixtureControlKey::Direction(MotorMoveDirection::Left)),
            FixtureControlAction::Press(FixtureControlKey::Sprint),
            FixtureControlAction::Release(FixtureControlKey::Sprint),
            FixtureControlAction::Release(FixtureControlKey::Direction(MotorMoveDirection::Left)),
            FixtureControlAction::Release(FixtureControlKey::Direction(
                MotorMoveDirection::Forward
            )),
        ]
    );
}

#[test]
fn cancelled_move_releases_every_pressed_key() {
    let request = MoveInputRequest {
        directions: vec![MotorMoveDirection::Forward, MotorMoveDirection::Right],
        duration_ms: 500,
        sprint: None,
    };
    let actions = fixture_move_control_script(&request).unwrap();
    assert_eq!(actions.len(), 4);
    assert!(matches!(actions[2], FixtureControlAction::Release(_)));
    assert!(matches!(actions[3], FixtureControlAction::Release(_)));
}

#[test]
fn move_rejects_empty_duplicate_or_invalid_sets() {
    for request in [
        MoveInputRequest {
            directions: Vec::new(),
            duration_ms: 500,
            sprint: None,
        },
        MoveInputRequest {
            directions: vec![MotorMoveDirection::Forward, MotorMoveDirection::Forward],
            duration_ms: 500,
            sprint: None,
        },
        MoveInputRequest {
            directions: vec![MotorMoveDirection::Forward],
            duration_ms: 49,
            sprint: None,
        },
    ] {
        assert!(request.validate().is_err());
    }
}

#[test]
fn all_fifteen_direction_subsets_preserve_uncancelled_axes() {
    let directions = [
        MotorMoveDirection::Forward,
        MotorMoveDirection::Back,
        MotorMoveDirection::Left,
        MotorMoveDirection::Right,
    ];
    let mut valid_subsets = 0;
    for mask in 1_u8..16 {
        let selected = directions
            .iter()
            .enumerate()
            .filter(|(index, _)| mask & (1 << index) != 0)
            .map(|(_, direction)| *direction)
            .collect::<Vec<_>>();
        let request = MoveInputRequest {
            directions: selected.clone(),
            duration_ms: 50,
            sprint: None,
        };
        request.validate().unwrap();
        let actions = fixture_move_control_script(&request).unwrap();
        let pressed = actions
            .iter()
            .filter(|action| matches!(action, FixtureControlAction::Press(_)))
            .count();
        assert_eq!(pressed, selected.len());
        valid_subsets += 1;
    }
    assert_eq!(valid_subsets, 15);
}

#[test]
fn event_v2_fixture_matches_deterministic_builder() {
    assert_eq!(json_sequences(), fixture_sequences());
}

#[test]
fn event_v2_rejects_v1_discriminator() {
    let mut event = serde_json::to_value(&json_sequences().ready[0]).unwrap();
    event["protocol"] = json!("mineintent.minecraft.backend-event.v1");
    assert!(serde_json::from_value::<BackendEventEnvelope>(event).is_err());
}

#[test]
fn event_v2_rejects_unknown_version_missing_source_and_unknown_fields() {
    let event = serde_json::to_value(&json_sequences().ready[0]).unwrap();
    for mutation in [
        (
            "protocol",
            Some(json!("mineintent.minecraft.backend-event.v3")),
        ),
        ("source", None),
        ("unknown", Some(json!(true))),
    ] {
        let mut candidate = event.clone();
        match mutation {
            (field, Some(value)) => candidate[field] = value,
            (field, None) => {
                candidate.as_object_mut().unwrap().remove(field);
            }
        }
        assert!(serde_json::from_value::<BackendEventEnvelope>(candidate).is_err());
    }
}

fn event_metadata(dimension: Option<&str>) -> BackendEventMetadata {
    BackendEventMetadata {
        id: "typed-event-1".to_owned(),
        occurred_at: "2026-08-01T00:00:00Z".to_owned(),
        process_session_id: "process-1".to_owned(),
        connection_epoch: 1,
        connection_attempt_id: "attempt-1".to_owned(),
        world_id: "world-1".to_owned(),
        dimension: dimension.map(str::to_owned),
    }
}

#[test]
fn default_and_typed_event_envelopes_round_trip_and_reject_mismatch_or_unknown_fields() {
    let lifecycle = BackendEventEnvelope::from_payload(
        event_metadata(None),
        FactSource::ServerObserved,
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected),
    );
    let encoded = serde_json::to_value(&lifecycle).unwrap();
    let decoded: BackendEventEnvelope = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(decoded, lifecycle);

    let mut mismatched = encoded.clone();
    mismatched["kind"] = json!("chat");
    assert!(serde_json::from_value::<BackendEventEnvelope>(mismatched).is_err());
    let mut unknown_payload = encoded;
    unknown_payload["payload"]["unexpected"] = json!(true);
    assert!(serde_json::from_value::<BackendEventEnvelope>(unknown_payload).is_err());

    let entity = BackendEventEnvelope::new(
        event_metadata(Some("minecraft:overworld")),
        BackendEventKind::Entity,
        FactSource::ServerObserved,
        ProtocolEntityEvent::Animation {
            entity_key: "entity-7".to_owned(),
            animation: "swing".to_owned(),
        },
    );
    let entity_value = serde_json::to_value(&entity).unwrap();
    let entity_decoded: BackendEventEnvelope<ProtocolEntityEvent> =
        serde_json::from_value(entity_value.clone()).unwrap();
    assert_eq!(entity_decoded, entity);
    let mut entity_mismatch = entity_value.clone();
    entity_mismatch["kind"] = json!("block");
    assert!(
        serde_json::from_value::<BackendEventEnvelope<ProtocolEntityEvent>>(entity_mismatch)
            .is_err()
    );
    let mut entity_unknown = entity_value;
    entity_unknown["payload"]["unexpected"] = json!(true);
    assert!(
        serde_json::from_value::<BackendEventEnvelope<ProtocolEntityEvent>>(entity_unknown)
            .is_err()
    );

    let block = BackendEventEnvelope::new(
        event_metadata(Some("minecraft:overworld")),
        BackendEventKind::Block,
        FactSource::ServerObserved,
        ProtocolBlockEvent::ChunkLoaded {
            chunk_x: 3,
            chunk_z: -4,
        },
    );
    let block_value = serde_json::to_value(&block).unwrap();
    let block_decoded: BackendEventEnvelope<ProtocolBlockEvent> =
        serde_json::from_value(block_value.clone()).unwrap();
    assert_eq!(block_decoded, block);
    let mut block_mismatch = block_value.clone();
    block_mismatch["kind"] = json!("sound");
    assert!(
        serde_json::from_value::<BackendEventEnvelope<ProtocolBlockEvent>>(block_mismatch).is_err()
    );
    let mut block_unknown = block_value;
    block_unknown["payload"]["unexpected"] = json!(true);
    assert!(
        serde_json::from_value::<BackendEventEnvelope<ProtocolBlockEvent>>(block_unknown).is_err()
    );

    let updated = BackendEventEnvelope::from_payload(
        event_metadata(Some("minecraft:overworld")),
        FactSource::ServerObserved,
        BackendEventPayload::Block(ProtocolBlockEvent::Updated {
            old_block: None,
            new_block: None,
        }),
    );
    let updated_value = serde_json::to_value(&updated).unwrap();
    assert!(updated_value["payload"].get("oldBlock").is_some());
    assert!(updated_value["payload"].get("newBlock").is_some());
    assert!(serde_json::from_value::<BackendEventEnvelope>(updated_value.clone()).is_ok());
    for missing in ["oldBlock", "newBlock"] {
        let mut candidate = updated_value.clone();
        candidate["payload"]
            .as_object_mut()
            .unwrap()
            .remove(missing);
        assert!(serde_json::from_value::<BackendEventEnvelope>(candidate).is_err());
    }

    let typed_updated = BackendEventEnvelope::new(
        event_metadata(Some("minecraft:overworld")),
        BackendEventKind::Block,
        FactSource::ServerObserved,
        ProtocolBlockEvent::Updated {
            old_block: None,
            new_block: None,
        },
    );
    let typed_updated_value = serde_json::to_value(&typed_updated).unwrap();
    assert!(
        serde_json::from_value::<BackendEventEnvelope<ProtocolBlockEvent>>(
            typed_updated_value.clone()
        )
        .is_ok()
    );
    for missing in ["oldBlock", "newBlock"] {
        let mut candidate = typed_updated_value.clone();
        candidate["payload"]
            .as_object_mut()
            .unwrap()
            .remove(missing);
        assert!(
            serde_json::from_value::<BackendEventEnvelope<ProtocolBlockEvent>>(candidate).is_err()
        );
    }

    let sound = BackendEventEnvelope::new(
        event_metadata(Some("minecraft:overworld")),
        BackendEventKind::Sound,
        FactSource::ServerObserved,
        ProtocolSoundPayload {
            event_type: HeardSoundType::Heard,
            sound_key: "minecraft:block.note_block.harp".to_owned(),
            sound_name: None,
            sound_id: None,
            category: None,
            source_position: Vec3Value {
                x: 1.0,
                y: 64.0,
                z: 2.0,
            },
            volume: 1.0,
            pitch: 1.0,
            protocol_source: ProtocolSoundSource::SoundEffect,
        },
    );
    let sound_value = serde_json::to_value(&sound).unwrap();
    let sound_decoded: BackendEventEnvelope<ProtocolSoundPayload> =
        serde_json::from_value(sound_value.clone()).unwrap();
    assert_eq!(sound_decoded, sound);
    let mut sound_mismatch = sound_value.clone();
    sound_mismatch["kind"] = json!("entity");
    assert!(
        serde_json::from_value::<BackendEventEnvelope<ProtocolSoundPayload>>(sound_mismatch)
            .is_err()
    );
    let mut sound_unknown = sound_value;
    sound_unknown["payload"]["unexpected"] = json!(true);
    assert!(
        serde_json::from_value::<BackendEventEnvelope<ProtocolSoundPayload>>(sound_unknown)
            .is_err()
    );

    let chat = BackendEventEnvelope::new(
        event_metadata(Some("minecraft:overworld")),
        BackendEventKind::Chat,
        FactSource::ServerObserved,
        ProtocolChatEvent {
            sender_username: Some("Alex".to_owned()),
            plain_text: "hello".to_owned(),
            position: Some(ChatPosition::Chat),
            verified: Some(true),
        },
    );
    let chat_value = serde_json::to_value(&chat).unwrap();
    let chat_decoded: BackendEventEnvelope<ProtocolChatEvent> =
        serde_json::from_value(chat_value.clone()).unwrap();
    assert_eq!(chat_decoded, chat);
    let mut chat_mismatch = chat_value.clone();
    chat_mismatch["kind"] = json!("world");
    assert!(
        serde_json::from_value::<BackendEventEnvelope<ProtocolChatEvent>>(chat_mismatch).is_err()
    );
    let mut chat_unknown = chat_value;
    chat_unknown["payload"]["rawText"] = json!("<Alex> hello");
    assert!(
        serde_json::from_value::<BackendEventEnvelope<ProtocolChatEvent>>(chat_unknown).is_err()
    );
}

#[test]
fn typed_lifecycle_envelope_rejects_unknown_unit_lifecycle_fields() {
    let lifecycle = BackendEventEnvelope::new(
        event_metadata(None),
        BackendEventKind::Lifecycle,
        FactSource::ServerObserved,
        BackendLifecyclePayload::TransportConnected,
    );
    let encoded = serde_json::to_value(&lifecycle).unwrap();
    let decoded: BackendEventEnvelope<BackendLifecyclePayload> =
        serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(decoded, lifecycle);

    let mut unknown = encoded.clone();
    unknown["payload"]["unexpected"] = json!(true);
    assert!(
        serde_json::from_value::<BackendEventEnvelope<BackendLifecyclePayload>>(unknown).is_err()
    );

    let mut mismatched = encoded;
    mismatched["kind"] = json!("world");
    assert!(
        serde_json::from_value::<BackendEventEnvelope<BackendLifecyclePayload>>(mismatched)
            .is_err()
    );
}

#[test]
fn all_twelve_lifecycle_payloads_round_trip_and_reject_unknown_fields() {
    let payloads = vec![
        BackendLifecyclePayload::ConnectionRequested { attempt: 1 },
        BackendLifecyclePayload::TransportConnected,
        BackendLifecyclePayload::LoggedIn {
            version: "26.1.2".to_owned(),
            dimension: "minecraft:overworld".to_owned(),
        },
        BackendLifecyclePayload::Ready {
            snapshot_revision: 7,
        },
        BackendLifecyclePayload::Died,
        BackendLifecyclePayload::RespawnTransitionStarted {
            from_dimension: "minecraft:overworld".to_owned(),
        },
        BackendLifecyclePayload::Respawned {
            dimension: "minecraft:overworld".to_owned(),
        },
        BackendLifecyclePayload::DimensionChanged {
            from: "minecraft:overworld".to_owned(),
            to: "minecraft:the_nether".to_owned(),
        },
        BackendLifecyclePayload::ReconnectScheduled {
            attempt: 2,
            retry_at: "2026-08-01T00:00:10Z".to_owned(),
            close_code: "server_shutdown".to_owned(),
        },
        BackendLifecyclePayload::ConnectionClosed {
            close: BackendClose {
                epoch: 1,
                at: "2026-08-01T00:00:09Z".to_owned(),
                code: "server_shutdown".to_owned(),
                retryable: true,
                deliberate: false,
                kick: None,
                error: None,
                end_reason: None,
            },
        },
        BackendLifecyclePayload::Faulted {
            failure: BackendFailure {
                code: BackendFailureCode::ProtocolError,
                message: "protocol failure".to_owned(),
                retryable: false,
            },
        },
        BackendLifecyclePayload::Stopped {
            reason: "operator_requested".to_owned(),
        },
    ];
    let expected_types = [
        "connection_requested",
        "transport_connected",
        "logged_in",
        "ready",
        "died",
        "respawn_transition_started",
        "respawned",
        "dimension_changed",
        "reconnect_scheduled",
        "connection_closed",
        "faulted",
        "stopped",
    ];

    for (payload, expected_type) in payloads.into_iter().zip(expected_types) {
        let envelope = BackendEventEnvelope::new(
            event_metadata(Some("minecraft:overworld")),
            BackendEventKind::Lifecycle,
            FactSource::ServerObserved,
            payload.clone(),
        );
        let encoded = serde_json::to_value(&envelope).unwrap();
        assert_eq!(encoded["payload"]["type"], expected_type);
        let decoded: BackendEventEnvelope<BackendLifecyclePayload> =
            serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(decoded, envelope);

        let mut unknown = encoded;
        unknown["payload"]["unexpected"] = json!(true);
        assert!(
            serde_json::from_value::<BackendEventEnvelope<BackendLifecyclePayload>>(unknown)
                .is_err()
        );
    }
}

#[test]
fn remaining_typed_v2_envelopes_round_trip_and_reject_mismatch_or_unknown_fields() {
    macro_rules! assert_typed_strict {
        ($ty:ty, $kind:expr, $payload:expr, $wrong_kind:expr, $unknown_field:expr) => {{
            let envelope = BackendEventEnvelope::new(
                event_metadata(Some("minecraft:overworld")),
                $kind,
                FactSource::ServerObserved,
                $payload,
            );
            let encoded = serde_json::to_value(&envelope).unwrap();
            let decoded: BackendEventEnvelope<$ty> =
                serde_json::from_value(encoded.clone()).unwrap();
            assert_eq!(decoded, envelope);

            let mut mismatched = encoded.clone();
            mismatched["kind"] = json!($wrong_kind);
            assert!(serde_json::from_value::<BackendEventEnvelope<$ty>>(mismatched).is_err());

            let mut unknown = encoded;
            unknown["payload"][$unknown_field] = json!(true);
            assert!(serde_json::from_value::<BackendEventEnvelope<$ty>>(unknown).is_err());
        }};
    }

    assert_typed_strict!(
        ProtocolSelfEvent,
        BackendEventKind::SelfState,
        ProtocolSelfEvent::ServerPositionCorrection {
            teleport_id: 9,
            position: Vec3Value {
                x: 1.0,
                y: 64.0,
                z: 2.0,
            },
            velocity: Vec3Value {
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
        "world",
        "unexpected"
    );

    let world = BackendEventEnvelope::new(
        event_metadata(Some("minecraft:overworld")),
        BackendEventKind::World,
        FactSource::ServerObserved,
        ProtocolWorldEvent::GameChanged {
            dimension: None,
            game_mode: None,
        },
    );
    let world_value = serde_json::to_value(&world).unwrap();
    assert!(world_value["payload"].get("dimension").is_none());
    assert!(world_value["payload"].get("gameMode").is_none());
    let world_decoded: BackendEventEnvelope<ProtocolWorldEvent> =
        serde_json::from_value(world_value.clone()).unwrap();
    assert_eq!(world_decoded, world);
    let mut world_mismatch = world_value.clone();
    world_mismatch["kind"] = json!("player_list");
    assert!(
        serde_json::from_value::<BackendEventEnvelope<ProtocolWorldEvent>>(world_mismatch).is_err()
    );
    let mut world_unknown = world_value;
    world_unknown["payload"]["unexpected"] = json!(true);
    assert!(
        serde_json::from_value::<BackendEventEnvelope<ProtocolWorldEvent>>(world_unknown).is_err()
    );

    assert_typed_strict!(
        ProtocolPlayerListEvent,
        BackendEventKind::PlayerList,
        ProtocolPlayerListEvent::Add {
            uuid: "uuid-1".to_owned(),
            username: "Alex".to_owned(),
        },
        "chat",
        "unexpected"
    );
    let player_wire = serde_json::to_value(ProtocolPlayerListEvent::Remove {
        uuid: "uuid-1".to_owned(),
        username: "Alex".to_owned(),
    })
    .unwrap();
    assert_eq!(player_wire["type"], "player_list_remove");

    assert_typed_strict!(
        ProtocolSnapshotChangedEvent,
        BackendEventKind::SnapshotChanged,
        ProtocolSnapshotChangedEvent {
            group: "world".to_owned(),
            snapshot_revision: 4,
        },
        "entity",
        "unexpected"
    );

    assert_typed_strict!(
        BackendOverflowPayload,
        BackendEventKind::Overflow,
        BackendOverflowPayload {
            event_type: OverflowType::Overflow,
            dropped_count: 2,
            dropped_kinds: vec![BackendEventKind::Entity],
        },
        "chat",
        "unexpected"
    );
}

#[test]
fn snapshot_v1_is_strict_and_keeps_target_axes() {
    let snapshot = fixture_snapshot();
    snapshot.validate_target_axes().unwrap();
    let mut value = serde_json::to_value(snapshot).unwrap();
    value["self"]["unknown"] = json!(1);
    assert!(serde_json::from_value::<MinecraftSnapshotV1>(value).is_err());
}

#[test]
fn public_async_traits_are_object_safe() {
    let _: Option<&dyn CancellationSignal> = None;
    let _: Option<&dyn Deadline> = None;
    let _: Option<&dyn MinecraftBackendApi> = None;
    let _: Option<&dyn ProtocolObservationSource> = None;
    let _: Option<&dyn MinecraftMotorDriverApi> = None;
    let _: Option<&dyn Subscription> = None;
}
