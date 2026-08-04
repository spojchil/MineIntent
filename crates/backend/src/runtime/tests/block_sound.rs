use super::production_support::*;
use super::*;

#[test]
fn production_block_sound_updates_are_ordered_post_state_and_preserve_null_old() {
    let (handle, mut app, owner, shared_world, source, mut events) = producer_test_app();
    let position = azalea::BlockPos {
        x: -17,
        y: -46,
        z: 30,
    };
    let chunk_pos = azalea::core::position::ChunkPos::new(-2, 1);
    let chunk = install_shared_chunk(&shared_world, chunk_pos);
    expose_shared_chunk(&mut app, owner, chunk_pos, chunk);

    let state_a = test_block_state(1);
    let state_b = test_block_state(2);
    let state_c = test_block_state(3);
    shared_world
        .read()
        .chunks
        .set_block_state(position, state_a);

    let seen = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let _subscription = ProtocolObservationSource::subscribe(
        &source,
        Arc::new(ImmediateBlockObservationReader {
            source: source.clone(),
            seen: seen.clone(),
        }),
    )
    .expect("block callback subscription");

    queue_production_block_packet(&mut app, owner, position, state_b);
    queue_production_block_packet(&mut app, owner, position, state_c);
    app.update();
    assert_eq!(
        *seen.lock(),
        vec![
            (
                Some(u32::from(state_a.id())),
                u32::from(state_b.id()),
                Some(u32::from(state_b.id())),
            ),
            (
                Some(u32::from(state_b.id())),
                u32::from(state_c.id()),
                Some(u32::from(state_c.id())),
            ),
        ]
    );
    queue_production_block_packet(
        &mut app,
        owner,
        azalea::BlockPos {
            x: 145,
            y: -46,
            z: 145,
        },
        test_block_state(4),
    );
    app.update();

    assert_eq!(
        *seen.lock(),
        vec![
            (
                Some(u32::from(state_a.id())),
                u32::from(state_b.id()),
                Some(u32::from(state_b.id())),
            ),
            (
                Some(u32::from(state_b.id())),
                u32::from(state_c.id()),
                Some(u32::from(state_c.id())),
            ),
            (None, 4, None),
        ]
    );
    let updates = block_events(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            ContractProtocolBlockEvent::Updated {
                old_block,
                new_block,
            } => Some((old_block, new_block.expect("new block"))),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(updates.len(), 3);
    assert_eq!(
        updates[0].0.as_ref().expect("full old block").position,
        ContractBlockPosition {
            x: -17,
            y: -46,
            z: 30,
        }
    );
    assert_eq!(
        updates[0].1.position,
        updates[0].0.as_ref().unwrap().position
    );
    assert_eq!(updates[0].1.state_id, u32::from(state_b.id()));
    assert!(!updates[0].1.name.is_empty());
    assert!(
        updates[0].1.bounding_box == ContractBlockBoundingBox::Block
            || updates[0].1.bounding_box == ContractBlockBoundingBox::Empty
    );
    assert!(
        updates[2].0.is_none(),
        "unloaded oldBlock must be JSON null"
    );
    assert_eq!(handle.connection_epoch(), 1);
}

#[test]
fn production_block_sound_section_updates_flatten_in_wire_order_with_negative_section() {
    let (_handle, mut app, owner, shared_world, _source, mut events) = producer_test_app();
    let chunk_pos = azalea::core::position::ChunkPos::new(-2, 1);
    let chunk = install_shared_chunk(&shared_world, chunk_pos);
    expose_shared_chunk(&mut app, owner, chunk_pos, chunk);
    let packet = azalea::protocol::packets::game::ClientboundGamePacket::SectionBlocksUpdate(
        azalea::protocol::packets::game::ClientboundSectionBlocksUpdate {
            section_pos: azalea::core::position::ChunkSectionPos { x: -2, y: -3, z: 1 },
            states: vec![
                azalea::protocol::packets::game::c_section_blocks_update::BlockStateWithPosition {
                    pos: azalea::core::position::ChunkSectionBlockPos { x: 1, y: 2, z: 3 },
                    state: test_block_state(5),
                },
                azalea::protocol::packets::game::c_section_blocks_update::BlockStateWithPosition {
                    pos: azalea::core::position::ChunkSectionBlockPos { x: 15, y: 0, z: 0 },
                    state: test_block_state(6),
                },
            ],
        },
    );
    azalea::packet::game::process_packet(
        app.world_mut(),
        owner,
        &packet,
        synthetic_attempt_token(),
    );
    queue_producer_packet(&mut app, owner, packet);
    app.update();

    let updates = block_events(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            ContractProtocolBlockEvent::Updated { new_block, .. } => {
                Some(new_block.expect("section new block"))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(updates.len(), 2);
    assert_eq!(
        updates
            .iter()
            .map(|block| (
                block.position.x,
                block.position.y,
                block.position.z,
                block.state_id
            ))
            .collect::<Vec<_>>(),
        vec![(-31, -46, 19, 5), (-17, -48, 16, 6)]
    );
}
