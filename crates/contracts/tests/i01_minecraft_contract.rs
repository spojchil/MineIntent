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
    assert_eq!(first.payload["type"], "connection_requested");
    assert_eq!(first.payload["attempt"], 1);
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
        .map(|event| event.payload["type"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        death_types,
        ["died", "respawn_transition_started", "respawned"]
    );
    assert_eq!(fixtures.dimension[0].payload["type"], "dimension_changed");
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
            .filter(|event| event.payload["type"] == "connection_closed")
            .count(),
        1
    );
    assert_eq!(fixtures.close[0].payload["close"]["retryable"], false);
    assert_eq!(fixtures.close[1].payload["type"], "faulted");
}

#[test]
fn structured_shutdown_is_flattened_and_retryable() {
    let event = &json_sequences().reconnect[0];
    assert_eq!(event.payload["close"]["code"], "server_shutdown");
    assert_eq!(event.payload["close"]["retryable"], true);
    assert_eq!(event.payload["close"]["kick"]["text"], "Server closed");
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
        .all(|event| event.payload["type"] != "reconnect_scheduled"));
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
