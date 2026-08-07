use super::*;

pub(super) fn producer_test_app() -> (
    RuntimeHandle,
    App,
    bevy_ecs::entity::Entity,
    SharedWorld,
    RuntimeObservationSource,
    RuntimeEventReceiver,
) {
    let (handle, app, owner, shared_world, source, events) = producer_test_app_without_world();
    assert!(handle.shared.set_world_if_running(shared_world.clone()));
    (handle, app, owner, shared_world, source, events)
}

pub(super) fn producer_test_app_without_world() -> (
    RuntimeHandle,
    App,
    bevy_ecs::entity::Entity,
    SharedWorld,
    RuntimeObservationSource,
    RuntimeEventReceiver,
) {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut app = App::new();
    app.add_message::<azalea::packet::game::ReceiveGamePacketEvent>();
    app.add_message::<azalea::chunks::ReceiveChunkEvent>();
    let owner = app
        .world_mut()
        .spawn((LocalEntity, azalea::core::entity_id::MinecraftEntityId(99)))
        .id();
    let shared_world = empty_world();
    app.world_mut().entity_mut(owner).insert((
        azalea::local_player::WorldHolder::new(owner, shared_world.clone()),
        azalea::block_update::QueuedServerBlockUpdates::default(),
        azalea::interact::BlockStatePredictionHandler::default(),
    ));
    app.insert_resource(SwarmState {
        shared: handle.shared.clone(),
    });
    app.add_systems(azalea::app::PreUpdate, produce_entity_packet_events);
    app.add_systems(
        Update,
        (
            azalea::chunks::handle_receive_chunk_event,
            azalea::block_update::handle_block_update_event,
        ),
    );

    assert!(handle.shared.begin_connection_attempt());
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
        .insert_resource(super::entity_events_owner_tests::TestAttemptToken(
            test_token,
        ));
    let source = handle.observation_source();
    let events = handle.subscribe();
    (handle, app, owner, shared_world, source, events)
}

pub(super) fn synthetic_attempt_token() -> azalea::join::AttemptToken {
    azalea::join::AttemptToken::mint()
}

pub(super) fn queue_producer_packet(
    app: &mut App,
    owner: bevy_ecs::entity::Entity,
    packet: azalea::protocol::packets::game::ClientboundGamePacket,
) {
    let attempt_token = app
        .world()
        .resource::<super::entity_events_owner_tests::TestAttemptToken>()
        .0;
    app.world_mut()
        .write_message(azalea::packet::game::ReceiveGamePacketEvent {
            entity: owner,
            packet: Arc::new(packet),
            attempt_token,
        });
}

/// 取出声音载荷。
///
/// 曾经这里是 `block_events`：方块更新有专门的生产者，测试拿方块包当「一条被
/// 发布的观察事实」的载体。那个生产者已删（方块变化经视口到达，不再发事件），
/// 载体换成声音——它是归约器仍然发布的那一类。
pub(super) fn sound_events(events: &mut RuntimeEventReceiver) -> Vec<ContractProtocolSoundPayload> {
    std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event.payload {
            BackendEventPayload::Sound(payload) => Some(payload),
            _ => None,
        })
        .collect()
}

/// 把一条声音包投进归约器，作为「发布一条观察事实」的最小驱动。
pub(super) fn queue_production_sound_packet(
    app: &mut App,
    owner: bevy_ecs::entity::Entity,
    attempt_token: azalea::join::AttemptToken,
    pitch: f32,
) {
    app.world_mut()
        .write_message(azalea::packet::game::ReceiveGamePacketEvent {
            entity: owner,
            packet: Arc::new(sound_packet(
                azalea::registry::Holder::Reference(
                    azalea::registry::builtin::SoundEvent::BlockAnvilLand,
                ),
                1.0,
                pitch,
            )),
            attempt_token,
        });
}

pub(super) fn sound_packet(
    sound: azalea::registry::Holder<
        azalea::registry::builtin::SoundEvent,
        azalea::core::sound::CustomSound,
    >,
    volume: f32,
    pitch: f32,
) -> azalea::protocol::packets::game::ClientboundGamePacket {
    azalea::protocol::packets::game::ClientboundGamePacket::Sound(
        azalea::protocol::packets::game::ClientboundSound {
            sound,
            source: azalea::protocol::packets::game::c_sound::SoundSource::Master,
            x: 9,
            y: -8,
            z: 16,
            volume,
            pitch,
            seed: 42,
        },
    )
}

pub(super) fn packed_light_layer(values: &[(usize, u8)]) -> Box<[u8]> {
    let mut bytes = vec![0; 2048];
    for &(index, value) in values {
        assert!(index < 4096);
        assert!(value <= 15);
        let byte = &mut bytes[index >> 1];
        if index & 1 == 0 {
            *byte = (*byte & 0xf0) | value;
        } else {
            *byte = (*byte & 0x0f) | (value << 4);
        }
    }
    bytes.into_boxed_slice()
}

pub(super) fn light_data_with_masks(
    sky_bits: &[usize],
    block_bits: &[usize],
    empty_sky_bits: &[usize],
    empty_block_bits: &[usize],
    sky_updates: Vec<Box<[u8]>>,
    block_updates: Vec<Box<[u8]>>,
) -> azalea::protocol::packets::game::c_light_update::ClientboundLightUpdatePacketData {
    let mut sky_y_mask = azalea::core::bitset::BitSet::new(64);
    for &bit in sky_bits {
        sky_y_mask.set(bit);
    }
    let mut block_y_mask = azalea::core::bitset::BitSet::new(64);
    for &bit in block_bits {
        block_y_mask.set(bit);
    }
    let mut empty_sky_y_mask = azalea::core::bitset::BitSet::new(64);
    for &bit in empty_sky_bits {
        empty_sky_y_mask.set(bit);
    }
    let mut empty_block_y_mask = azalea::core::bitset::BitSet::new(64);
    for &bit in empty_block_bits {
        empty_block_y_mask.set(bit);
    }
    azalea::protocol::packets::game::c_light_update::ClientboundLightUpdatePacketData {
        sky_y_mask,
        block_y_mask,
        empty_sky_y_mask,
        empty_block_y_mask,
        sky_updates: Arc::new(sky_updates.into_boxed_slice()),
        block_updates: Arc::new(block_updates.into_boxed_slice()),
    }
}

pub(super) fn light_chunk_packet(
    x: i32,
    z: i32,
    light_data: azalea::protocol::packets::game::c_light_update::ClientboundLightUpdatePacketData,
) -> azalea::protocol::packets::game::ClientboundGamePacket {
    azalea::protocol::packets::game::ClientboundGamePacket::LevelChunkWithLight(
        azalea::protocol::packets::game::ClientboundLevelChunkWithLight {
            x,
            z,
            chunk_data: azalea::protocol::packets::game::c_level_chunk_with_light::ClientboundLevelChunkPacketData {
                heightmaps: Vec::new(),
                data: Arc::new(Vec::<u8>::new().into_boxed_slice()),
                block_entities: Vec::new(),
            },
            light_data,
        },
    )
}

pub(super) fn light_update_packet(
    x: i32,
    z: i32,
    light_data: azalea::protocol::packets::game::c_light_update::ClientboundLightUpdatePacketData,
) -> azalea::protocol::packets::game::ClientboundGamePacket {
    azalea::protocol::packets::game::ClientboundGamePacket::LightUpdate(
        azalea::protocol::packets::game::ClientboundLightUpdate { x, z, light_data },
    )
}

pub(super) fn attributes_packet(
    entity_id: i32,
    values: Vec<azalea::protocol::packets::game::c_update_attributes::AttributeSnapshot>,
) -> azalea::protocol::packets::game::ClientboundGamePacket {
    azalea::protocol::packets::game::ClientboundGamePacket::UpdateAttributes(
        azalea::protocol::packets::game::ClientboundUpdateAttributes {
            entity_id: azalea::core::entity_id::MinecraftEntityId(entity_id),
            values,
        },
    )
}

pub(super) fn armor_snapshot(
    attribute: azalea::registry::builtin::Attribute,
    base: f64,
) -> azalea::protocol::packets::game::c_update_attributes::AttributeSnapshot {
    azalea::protocol::packets::game::c_update_attributes::AttributeSnapshot {
        attribute,
        base,
        modifiers: Vec::new(),
    }
}

pub(super) fn armor_snapshot_with_modifiers(
    base: f64,
    modifiers: Vec<azalea::inventory::components::AttributeModifier>,
) -> azalea::protocol::packets::game::c_update_attributes::AttributeSnapshot {
    azalea::protocol::packets::game::c_update_attributes::AttributeSnapshot {
        attribute: azalea::registry::builtin::Attribute::Armor,
        base,
        modifiers,
    }
}
