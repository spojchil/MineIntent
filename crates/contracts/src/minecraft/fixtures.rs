use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    AuthKind, BackendClose, BackendEventEnvelope, BackendEventKind, BackendEventMetadata,
    BackendEventPayload, BackendFailure, BackendFailureCode, BackendLifecyclePayload,
    BackendOverflowPayload, BackendReady, BackendState, BackendTimeouts, BlockBoundingBox,
    Difficulty, FactSource, GameMode, InventorySlotSnapshot, InventorySnapshot,
    MinecraftBackendConfig, MinecraftIdentityConfig, MinecraftServerConfig, MinecraftSnapshotV1,
    MotorMoveDirection, MoveInputRequest, OverflowType, ReconnectPolicy, SelfSnapshot,
    SnapshotProtocol, StatusEffectSnapshot, TrackedPlayerSnapshot, Vec3Value, WorldSnapshot,
    TARGET_MINECRAFT_VERSION, TARGET_PROTOCOL_VERSION,
};

pub const FIXTURE_PROCESS_SESSION_ID: &str = "process-fixture-0001";
pub const FIXTURE_WORLD_ID: &str = "world-fixture";
pub const FIXTURE_ATTEMPT_ID: &str = "attempt-fixture-0001";
pub const FIXTURE_DIMENSION: &str = "minecraft:overworld";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackendSequenceFixtures {
    pub ready: Vec<BackendEventEnvelope>,
    pub death: Vec<BackendEventEnvelope>,
    pub dimension: Vec<BackendEventEnvelope>,
    pub reconnect: Vec<BackendEventEnvelope>,
    pub close: Vec<BackendEventEnvelope>,
    pub overflow: Vec<BackendEventEnvelope>,
}

/// Fake backend 所需的纯脚本数据；它不执行状态机、定时器、订阅或 IO。
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptedBackendData {
    pub initial_state: BackendState,
    pub start_result: Result<BackendReady, super::BackendError>,
    pub events: Vec<BackendEventEnvelope>,
}

#[derive(Clone, Debug)]
pub struct ScriptedBackendDataBuilder {
    initial_state: BackendState,
    start_result: Result<BackendReady, super::BackendError>,
    events: Vec<BackendEventEnvelope>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixtureControlKey {
    Direction(MotorMoveDirection),
    Sprint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixtureControlAction {
    Press(FixtureControlKey),
    Release(FixtureControlKey),
}

/// Expected control ordering used by a fake motor. It describes the oracle and performs no input.
pub fn fixture_move_control_script(
    request: &MoveInputRequest,
) -> Result<Vec<FixtureControlAction>, super::BackendError> {
    request.validate()?;
    let mut pressed = request
        .directions
        .iter()
        .copied()
        .map(FixtureControlKey::Direction)
        .collect::<Vec<_>>();
    if request.sprint == Some(true) {
        pressed.push(FixtureControlKey::Sprint);
    }
    let mut actions = pressed
        .iter()
        .copied()
        .map(FixtureControlAction::Press)
        .collect::<Vec<_>>();
    actions.extend(pressed.into_iter().rev().map(FixtureControlAction::Release));
    Ok(actions)
}

impl ScriptedBackendDataBuilder {
    pub fn ready() -> Self {
        Self {
            initial_state: BackendState::Idle,
            start_result: Ok(fixture_ready()),
            events: fixture_sequences().ready,
        }
    }

    pub fn initial_state(mut self, state: BackendState) -> Self {
        self.initial_state = state;
        self
    }

    pub fn start_result(mut self, result: Result<BackendReady, super::BackendError>) -> Self {
        self.start_result = result;
        self
    }

    pub fn events(mut self, events: Vec<BackendEventEnvelope>) -> Self {
        self.events = events;
        self
    }

    pub fn build(self) -> ScriptedBackendData {
        ScriptedBackendData {
            initial_state: self.initial_state,
            start_result: self.start_result,
            events: self.events,
        }
    }
}

pub fn fixture_config() -> MinecraftBackendConfig {
    MinecraftBackendConfig {
        world_id: FIXTURE_WORLD_ID.to_owned(),
        server: MinecraftServerConfig {
            host: "127.0.0.1".to_owned(),
            port: 25_565,
            version: TARGET_MINECRAFT_VERSION.to_owned(),
        },
        identity: MinecraftIdentityConfig {
            username: "MineFixture".to_owned(),
            auth: AuthKind::Offline,
            profiles_folder: None,
        },
        timeouts: BackendTimeouts {
            connect_ms: 10_000,
            login_ms: 10_000,
            spawn_ms: 15_000,
            stop_ms: 5_000,
        },
        reconnect: ReconnectPolicy {
            enabled: true,
            initial_delay_ms: 500,
            multiplier: 2.0,
            max_delay_ms: 30_000,
            jitter_ratio: 0.2,
            stable_reset_ms: 60_000,
        },
    }
}

pub fn fixture_snapshot() -> MinecraftSnapshotV1 {
    MinecraftSnapshotV1 {
        protocol: SnapshotProtocol::V1,
        snapshot_revision: 7,
        lifecycle_revision: 4,
        captured_at: "2026-08-01T00:00:03Z".to_owned(),
        process_session_id: FIXTURE_PROCESS_SESSION_ID.to_owned(),
        connection_epoch: 1,
        connection_attempt_id: FIXTURE_ATTEMPT_ID.to_owned(),
        world: WorldSnapshot {
            world_id: FIXTURE_WORLD_ID.to_owned(),
            dimension: FIXTURE_DIMENSION.to_owned(),
            minecraft_version: TARGET_MINECRAFT_VERSION.to_owned(),
            protocol_version: TARGET_PROTOCOL_VERSION,
            game_mode: GameMode::Survival,
            difficulty: Some(Difficulty::Normal),
            min_y: -64,
            height: 384,
            server_view_distance: Some(10),
            time_of_day: Some(6_000),
            is_raining: Some(false),
        },
        self_snapshot: SelfSnapshot {
            entity_key: "1:42".to_owned(),
            username: "MineFixture".to_owned(),
            position: Vec3Value {
                x: 1.5,
                y: 64.0,
                z: -2.5,
            },
            velocity: Vec3Value::default(),
            yaw: 0.5,
            pitch: -0.25,
            on_ground: true,
            alive: true,
            health: 18.0,
            food: 17.0,
            food_saturation: 3.5,
            oxygen: Some(20.0),
            experience: Some(super::ExperienceSnapshot {
                level: 3,
                progress: 0.25,
                total: 42,
            }),
            effects: vec![StatusEffectSnapshot {
                name: "speed".to_owned(),
                amplifier: 1,
                duration_ticks: Some(120),
            }],
        },
        inventory: InventorySnapshot {
            selected_hotbar_slot: 2,
            slots: vec![InventorySlotSnapshot {
                slot: 38,
                item_name: "diamond_pickaxe".to_owned(),
                count: 1,
                metadata: Some(0),
                durability_used: Some(7),
            }],
        },
        tracked_players: vec![TrackedPlayerSnapshot {
            player_key: "00000000-0000-0000-0000-000000000002".to_owned(),
            uuid: Some("00000000-0000-0000-0000-000000000002".to_owned()),
            username: "Observer".to_owned(),
            listed: true,
            entity_tracked: true,
            position: Some(Vec3Value {
                x: 4.0,
                y: 64.0,
                z: -2.0,
            }),
            yaw: Some(1.0),
            pitch: Some(0.0),
            held_item_name: Some("torch".to_owned()),
        }],
    }
}

pub fn fixture_ready() -> BackendReady {
    BackendReady {
        process_session_id: FIXTURE_PROCESS_SESSION_ID.to_owned(),
        connection_epoch: 1,
        connection_attempt_id: FIXTURE_ATTEMPT_ID.to_owned(),
        snapshot: fixture_snapshot(),
    }
}

pub fn fixture_sequences() -> BackendSequenceFixtures {
    let close = fixture_retryable_close();
    let ready = vec![
        lifecycle_event(
            1,
            1,
            FIXTURE_ATTEMPT_ID,
            None,
            FactSource::Commanded,
            BackendLifecyclePayload::ConnectionRequested { attempt: 1 },
        ),
        lifecycle_event(
            2,
            1,
            FIXTURE_ATTEMPT_ID,
            None,
            FactSource::ServerObserved,
            BackendLifecyclePayload::TransportConnected,
        ),
        lifecycle_event(
            3,
            1,
            FIXTURE_ATTEMPT_ID,
            Some(FIXTURE_DIMENSION),
            FactSource::ServerObserved,
            BackendLifecyclePayload::LoggedIn {
                version: TARGET_MINECRAFT_VERSION.to_owned(),
                dimension: FIXTURE_DIMENSION.to_owned(),
            },
        ),
        lifecycle_event(
            4,
            1,
            FIXTURE_ATTEMPT_ID,
            Some(FIXTURE_DIMENSION),
            FactSource::ServerObserved,
            BackendLifecyclePayload::Ready {
                snapshot_revision: 7,
            },
        ),
    ];
    let death = vec![
        lifecycle_event(
            5,
            1,
            FIXTURE_ATTEMPT_ID,
            Some(FIXTURE_DIMENSION),
            FactSource::ServerObserved,
            BackendLifecyclePayload::Died,
        ),
        lifecycle_event(
            6,
            1,
            FIXTURE_ATTEMPT_ID,
            Some(FIXTURE_DIMENSION),
            FactSource::ServerObserved,
            BackendLifecyclePayload::RespawnTransitionStarted {
                from_dimension: FIXTURE_DIMENSION.to_owned(),
            },
        ),
        lifecycle_event(
            7,
            1,
            FIXTURE_ATTEMPT_ID,
            Some(FIXTURE_DIMENSION),
            FactSource::ServerObserved,
            BackendLifecyclePayload::Respawned {
                dimension: FIXTURE_DIMENSION.to_owned(),
            },
        ),
    ];
    let dimension = vec![lifecycle_event(
        8,
        1,
        FIXTURE_ATTEMPT_ID,
        Some("minecraft:the_nether"),
        FactSource::ServerObserved,
        BackendLifecyclePayload::DimensionChanged {
            from: FIXTURE_DIMENSION.to_owned(),
            to: "minecraft:the_nether".to_owned(),
        },
    )];
    let reconnect = vec![
        lifecycle_event(
            9,
            1,
            FIXTURE_ATTEMPT_ID,
            Some(FIXTURE_DIMENSION),
            FactSource::ServerObserved,
            BackendLifecyclePayload::ConnectionClosed {
                close: close.clone(),
            },
        ),
        lifecycle_event(
            10,
            1,
            FIXTURE_ATTEMPT_ID,
            Some(FIXTURE_DIMENSION),
            FactSource::ClientPredicted,
            BackendLifecyclePayload::ReconnectScheduled {
                attempt: 2,
                retry_at: "2026-08-01T00:00:10Z".to_owned(),
                close_code: close.code.clone(),
            },
        ),
        lifecycle_event(
            11,
            2,
            "attempt-fixture-0002",
            None,
            FactSource::Commanded,
            BackendLifecyclePayload::ConnectionRequested { attempt: 2 },
        ),
    ];
    let close_sequence = vec![
        lifecycle_event(
            12,
            1,
            FIXTURE_ATTEMPT_ID,
            Some(FIXTURE_DIMENSION),
            FactSource::ServerObserved,
            BackendLifecyclePayload::ConnectionClosed {
                close: fixture_fatal_close(),
            },
        ),
        lifecycle_event(
            13,
            1,
            FIXTURE_ATTEMPT_ID,
            Some(FIXTURE_DIMENSION),
            FactSource::ServerObserved,
            BackendLifecyclePayload::Faulted {
                failure: BackendFailure {
                    code: BackendFailureCode::PermissionDenied,
                    message: "You are banned".to_owned(),
                    retryable: false,
                },
            },
        ),
    ];
    let overflow = vec![BackendEventEnvelope::from_payload(
        metadata(14, 1, FIXTURE_ATTEMPT_ID, Some(FIXTURE_DIMENSION)),
        FactSource::ClientPredicted,
        BackendEventPayload::Overflow(BackendOverflowPayload {
            event_type: OverflowType::Overflow,
            dropped_count: 3,
            dropped_kinds: vec![BackendEventKind::Entity, BackendEventKind::Block],
        }),
    )];
    BackendSequenceFixtures {
        ready,
        death,
        dimension,
        reconnect,
        close: close_sequence,
        overflow,
    }
}

pub fn fixture_block_properties() -> BTreeMap<String, super::BlockPropertyValue> {
    BTreeMap::from([
        (
            "axis".to_owned(),
            super::BlockPropertyValue::String("y".to_owned()),
        ),
        (
            "waterlogged".to_owned(),
            super::BlockPropertyValue::Boolean(false),
        ),
    ])
}

pub fn fixture_block() -> super::ProtocolBlockSnapshot {
    super::ProtocolBlockSnapshot {
        position: super::BlockPosition { x: 1, y: 63, z: -2 },
        name: "stone".to_owned(),
        state_id: 1,
        properties: fixture_block_properties(),
        collision_shapes: vec![[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]],
        transparent_hint: false,
        bounding_box: BlockBoundingBox::Block,
    }
}

fn lifecycle_event(
    sequence: u32,
    epoch: u64,
    attempt_id: &str,
    dimension: Option<&str>,
    source: FactSource,
    payload: BackendLifecyclePayload,
) -> BackendEventEnvelope {
    BackendEventEnvelope::from_payload(
        metadata(sequence, epoch, attempt_id, dimension),
        source,
        BackendEventPayload::Lifecycle(payload),
    )
}

fn metadata(
    sequence: u32,
    epoch: u64,
    attempt_id: &str,
    dimension: Option<&str>,
) -> BackendEventMetadata {
    BackendEventMetadata {
        id: format!("event-fixture-{sequence:04}"),
        occurred_at: format!("2026-08-01T00:00:{sequence:02}Z"),
        process_session_id: FIXTURE_PROCESS_SESSION_ID.to_owned(),
        connection_epoch: epoch,
        connection_attempt_id: attempt_id.to_owned(),
        world_id: FIXTURE_WORLD_ID.to_owned(),
        dimension: dimension.map(str::to_owned),
    }
}

fn fixture_retryable_close() -> BackendClose {
    BackendClose {
        epoch: 1,
        at: "2026-08-01T00:00:09Z".to_owned(),
        code: "server_shutdown".to_owned(),
        retryable: true,
        deliberate: false,
        kick: Some(super::BackendKick {
            text: "Server closed".to_owned(),
            during_login: false,
        }),
        error: None,
        end_reason: Some("socket_closed".to_owned()),
    }
}

fn fixture_fatal_close() -> BackendClose {
    BackendClose {
        epoch: 1,
        at: "2026-08-01T00:00:12Z".to_owned(),
        code: "permission_denied".to_owned(),
        retryable: false,
        deliberate: false,
        kick: Some(super::BackendKick {
            text: "You are banned".to_owned(),
            during_login: false,
        }),
        error: None,
        end_reason: Some("kicked".to_owned()),
    }
}
