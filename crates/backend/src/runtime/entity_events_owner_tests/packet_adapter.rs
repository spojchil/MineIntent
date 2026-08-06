use super::*;

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
            movement: azalea::core::delta::LpVec3::from_vec3(azalea::Vec3::new(0.25, 0.0, -0.5)),
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

fn production_position_packets() -> [azalea::protocol::packets::game::ClientboundGamePacket; 6] {
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
                (value - 90.0).abs() < 1e-6
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
        (spawned[0].head_yaw.expect("spawn head yaw") - 45.0).abs() < 1e-6
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
        Some(90.0)
    );
    assert!(matches!(
        events.try_recv().expect("RotateHead envelope").payload,
        BackendEventPayload::Entity(ContractProtocolEntityEvent::Moved { entity })
            if entity
                .head_yaw
                .is_some_and(|value| (value - 90.0).abs() < 1e-6)
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
                Some(90.0)
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
        Some(90.0)
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
        Some(90.0)
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
