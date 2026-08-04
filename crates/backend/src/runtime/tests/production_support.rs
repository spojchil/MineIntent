use super::*;

pub(super) struct ImmediateBlockObservationReader {
    pub(super) source: RuntimeObservationSource,
    pub(super) seen: Arc<parking_lot::Mutex<Vec<(Option<u32>, u32, Option<u32>)>>>,
}

impl ObservationEventListener for ImmediateBlockObservationReader {
    fn on_event(&self, event: ObservationEvent) {
        let ObservationEvent::Block(envelope) = event else {
            return;
        };
        let ContractProtocolBlockEvent::Updated {
            old_block,
            new_block,
        } = envelope.payload
        else {
            return;
        };
        let new_block = new_block.expect("accepted block update has a new block");
        let position = ContractBlockPosition {
            x: new_block.position.x,
            y: new_block.position.y,
            z: new_block.position.z,
        };
        let read_state_id = match self
            .source
            .read_block(position)
            .expect("callback block read")
        {
            ContractBlockReadResult::Loaded { block } => Some(block.state_id),
            ContractBlockReadResult::Unloaded => None,
            other => panic!("callback block read left the world height, got {other:?}"),
        };
        self.seen.lock().push((
            old_block.map(|block| block.state_id),
            new_block.state_id,
            read_state_id,
        ));
    }
}

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
        CanonicalPacketSourceMetadata::default(),
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
    app.add_plugins(BlockSoundProducerPlugin);

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

pub(super) fn test_block_state(id: u32) -> azalea::block::BlockState {
    azalea::block::BlockState::try_from(id).expect("test block state id")
}

pub(super) fn synthetic_attempt_token() -> azalea::join::AttemptToken {
    azalea::join::AttemptToken::mint()
}

pub(super) fn install_shared_chunk(
    shared_world: &SharedWorld,
    pos: azalea::core::position::ChunkPos,
) -> Arc<parking_lot::RwLock<azalea::world::Chunk>> {
    shared_world
        .write()
        .chunks
        .upsert(pos, azalea::world::Chunk::default())
}

pub(super) fn expose_shared_chunk(
    app: &mut App,
    owner: bevy_ecs::entity::Entity,
    pos: azalea::core::position::ChunkPos,
    chunk: Arc<parking_lot::RwLock<azalea::world::Chunk>>,
) {
    let holder = app
        .world_mut()
        .get::<azalea::local_player::WorldHolder>(owner)
        .expect("test world holder")
        .clone();
    holder.partial.write().chunks.limited_set(&pos, Some(chunk));
}

pub(super) fn queue_production_block_packet(
    app: &mut App,
    owner: bevy_ecs::entity::Entity,
    position: azalea::BlockPos,
    state: azalea::block::BlockState,
) {
    let packet = azalea::protocol::packets::game::ClientboundGamePacket::BlockUpdate(
        azalea::protocol::packets::game::ClientboundBlockUpdate {
            pos: position,
            block_state: state,
        },
    );
    azalea::packet::game::process_packet(
        app.world_mut(),
        owner,
        &packet,
        synthetic_attempt_token(),
    );
    queue_producer_packet(app, owner, packet);
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

pub(super) fn block_events(events: &mut RuntimeEventReceiver) -> Vec<ContractProtocolBlockEvent> {
    std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event.payload {
            BackendEventPayload::Block(payload) => Some(payload),
            _ => None,
        })
        .collect()
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

pub(super) fn empty_chunk_packet(
    x: i32,
    z: i32,
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
            light_data: Default::default(),
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
