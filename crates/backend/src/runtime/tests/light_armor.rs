use super::production_support::*;
use super::raw_reducer::reducer_respawn_packet;
use super::*;

#[test]
fn production_armor_reducer_is_local_ordered_and_epoch_bound() {
    let (handle, mut app, owner, _shared_world, _source, _events) = producer_test_app();

    queue_producer_packet(
        &mut app,
        owner,
        attributes_packet(
            98,
            vec![armor_snapshot(
                azalea::registry::builtin::Attribute::Armor,
                19.0,
            )],
        ),
    );
    app.update();
    assert_eq!(handle.shared.observation.read().armor, None);

    queue_producer_packet(
        &mut app,
        owner,
        attributes_packet(
            99,
            vec![
                armor_snapshot(azalea::registry::builtin::Attribute::Armor, 4.0),
                armor_snapshot(azalea::registry::builtin::Attribute::MaxHealth, 100.0),
                armor_snapshot(azalea::registry::builtin::Attribute::Armor, 7.0),
            ],
        ),
    );
    app.update();
    assert_eq!(handle.shared.observation.read().armor, Some(7));

    queue_producer_packet(
        &mut app,
        owner,
        attributes_packet(
            99,
            vec![armor_snapshot(
                azalea::registry::builtin::Attribute::MaxHealth,
                100.0,
            )],
        ),
    );
    app.update();
    assert_eq!(handle.shared.observation.read().armor, Some(7));

    use azalea::core::attribute_modifier_operation::AttributeModifierOperation as Op;
    let duplicate_modifier_id = azalea::Identifier::from("test:duplicate");
    let grouped_packet = attributes_packet(
        99,
        vec![armor_snapshot_with_modifiers(
            10.0,
            vec![
                azalea::inventory::components::AttributeModifier {
                    id: duplicate_modifier_id.clone(),
                    amount: 100.0,
                    operation: Op::AddValue,
                },
                azalea::inventory::components::AttributeModifier {
                    id: duplicate_modifier_id,
                    amount: 1.0,
                    operation: Op::AddValue,
                },
                azalea::inventory::components::AttributeModifier {
                    id: azalea::Identifier::from("test:base"),
                    amount: 0.5,
                    operation: Op::AddMultipliedBase,
                },
                azalea::inventory::components::AttributeModifier {
                    id: azalea::Identifier::from("test:total"),
                    amount: 0.1,
                    operation: Op::AddMultipliedTotal,
                },
            ],
        )],
    );
    queue_producer_packet(&mut app, owner, grouped_packet);
    app.update();
    // The duplicate ID uses the last entry (1.0), then d1=11,
    // d3=16.5, and finally d3*=1.1 -> floor 18.
    assert_eq!(handle.shared.observation.read().armor, Some(18));

    queue_producer_packet(
        &mut app,
        owner,
        attributes_packet(
            99,
            vec![armor_snapshot_with_modifiers(
                10.0,
                vec![azalea::inventory::components::AttributeModifier {
                    id: azalea::Identifier::from("test:infinite"),
                    amount: f64::INFINITY,
                    operation: Op::AddMultipliedTotal,
                }],
            )],
        ),
    );
    app.update();
    assert_eq!(handle.shared.observation.read().armor, None);

    for (base, expected) in [(-2.0, Some(0)), (25.0, Some(20)), (0.0, Some(0))] {
        queue_producer_packet(
            &mut app,
            owner,
            attributes_packet(
                99,
                vec![armor_snapshot(
                    azalea::registry::builtin::Attribute::Armor,
                    base,
                )],
            ),
        );
        app.update();
        assert_eq!(handle.shared.observation.read().armor, expected);
    }

    queue_producer_packet(
        &mut app,
        owner,
        attributes_packet(
            99,
            vec![armor_snapshot(
                azalea::registry::builtin::Attribute::Armor,
                f64::NAN,
            )],
        ),
    );
    app.update();
    assert_eq!(handle.shared.observation.read().armor, None);

    assert!(handle
        .shared
        .reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
            owner,
            1,
            Some("minecraft:overworld".to_owned()),
            Some(true),
        ));
    assert_eq!(handle.shared.observation.read().armor, None);

    queue_producer_packet(
        &mut app,
        owner,
        attributes_packet(
            99,
            vec![armor_snapshot(
                azalea::registry::builtin::Attribute::Armor,
                6.0,
            )],
        ),
    );
    app.update();
    assert_eq!(handle.shared.observation.read().armor, Some(6));

    assert!(handle.shared.begin_connection_attempt());
    assert_eq!(handle.shared.observation.read().armor, None);
}

#[test]
fn production_light_survives_first_world_attach_and_same_epoch_respawn_scope() {
    let (handle, mut app, owner, shared_world, _source, _events) =
        producer_test_app_without_world();
    let index = (3 << 8) | (2 << 4) | 1;
    let first_light = light_data_with_masks(
        &[1],
        &[1],
        &[],
        &[],
        vec![packed_light_layer(&[(index, 11)])],
        vec![packed_light_layer(&[(index, 4)])],
    );

    // This is the Login boundary equivalent used by the raw reducer:
    // it establishes the packet scope before the first chunk light packet.
    assert!(handle
        .shared
        .reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
            owner,
            1,
            Some("minecraft:overworld".to_owned()),
            Some(true),
        ));
    let generation_after_scope_reset = handle.shared.observation.read().generation;
    let first_chunk = light_chunk_packet(0, 0, first_light);
    azalea::packet::game::process_packet(
        app.world_mut(),
        owner,
        &first_chunk,
        synthetic_attempt_token(),
    );
    queue_producer_packet(&mut app, owner, first_chunk);
    app.update();

    assert_eq!(
        handle
            .shared
            .observation
            .read()
            .light_cache
            .context
            .as_ref()
            .and_then(|context| context.has_skylight),
        Some(true)
    );
    assert_eq!(
        handle.shared.observation.read().generation,
        generation_after_scope_reset + 1,
        "scope reset and full chunk apply must each publish a generation"
    );

    // The first Event::Spawn attaches the WorldHolder after the raw Login
    // and chunk packet. The existing implementation cleared the cache here.
    assert!(handle.shared.set_world_if_running(shared_world.clone()));
    let mut first_snapshot = snapshot_at(1, 1.25, -61.0, 2.75);
    first_snapshot.world.dimension = "minecraft:overworld".to_owned();
    install_viewport_observation(
        &handle,
        first_snapshot,
        FactSource::ServerObserved,
        Vec::new(),
        shared_world.clone(),
    );
    handle.shared.observation.write().snapshot_scope_generation =
        handle.shared.entity_producer.lock().scope_generation;
    let first_facts = handle
        .capture_frame_facts()
        .expect("first attached world should have a snapshot");
    assert_eq!(first_facts.light, Some(11));
    assert_eq!(
        handle
            .shared
            .observation
            .read()
            .light_cache
            .context
            .as_ref()
            .and_then(|context| context.has_skylight),
        Some(true)
    );

    // Same-epoch Respawn resets the light scope but preserves armor. A new
    // chunk in the new scope must likewise survive its first world attach.
    {
        let mut observation = handle.shared.observation.write();
        observation.armor = Some(7);
        observation.armor_epoch = Some(1);
        observation.bump_generation();
    }
    assert!(handle
        .shared
        .reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
            owner,
            1,
            Some("minecraft:the_nether".to_owned()),
            Some(true),
        ));
    {
        let observation = handle.shared.observation.read();
        assert!(observation.snapshot.is_none());
        assert!(observation.light_cache.chunks.is_empty());
        assert_eq!(observation.armor, Some(7));
        assert_eq!(observation.armor_epoch, Some(1));
    }
    let stale_attach_world = empty_world();
    assert!(handle
        .shared
        .set_world_if_running(stale_attach_world.clone()));
    assert!(handle
        .shared
        .observation
        .read()
        .light_cache
        .chunks
        .is_empty());
    assert!(handle
        .shared
        .reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
            owner,
            1,
            Some("minecraft:the_nether".to_owned()),
            Some(true),
        ));

    let second_index = 0;
    let second_light = light_data_with_masks(
        &[1],
        &[1],
        &[],
        &[],
        vec![packed_light_layer(&[(second_index, 9)])],
        vec![packed_light_layer(&[(second_index, 3)])],
    );
    let second_chunk = light_chunk_packet(0, 0, second_light);
    assert_eq!(
        LightSectionGeometry::from_world(&shared_world.read()),
        Some(LightSectionGeometry {
            min_light_section: -5,
            light_section_count: 26,
        })
    );
    assert!(handle.shared.admit_canonical_source(owner).is_some());
    let generation_before_second_light = handle.shared.observation.read().generation;
    azalea::packet::game::process_packet(
        app.world_mut(),
        owner,
        &second_chunk,
        synthetic_attempt_token(),
    );
    queue_producer_packet(&mut app, owner, second_chunk);
    app.update();
    assert_eq!(
        handle.shared.observation.read().generation,
        generation_before_second_light + 1
    );

    {
        let observation = handle.shared.observation.read();
        assert_eq!(
            observation.light_cache.context.as_ref().map(|context| (
                &context.dimension,
                context.scope_generation,
                context.has_skylight
            )),
            Some((
                &"minecraft:the_nether".to_owned(),
                handle.shared.entity_producer.lock().scope_generation,
                Some(true)
            ))
        );
        assert_eq!(
            observation
                .light_cache
                .chunks
                .get(&(0, 0))
                .and_then(|chunk| chunk.sky.get(1))
                .and_then(Option::as_ref)
                .map(|layer| layer[0]),
            Some(9)
        );
        assert_eq!(
            observation.light_cache.value_at(
                &Vec3Value {
                    x: 0.25,
                    y: -64.0,
                    z: 0.25,
                },
                1,
                handle.shared.entity_producer.lock().scope_generation,
                "minecraft:the_nether",
            ),
            Some(9)
        );
    }

    let second_world = empty_world();
    assert!(handle.shared.set_world_if_running(second_world.clone()));
    let mut second_snapshot = snapshot_at(1, 0.25, -64.0, 0.25);
    second_snapshot.world.dimension = "minecraft:the_nether".to_owned();
    install_viewport_observation(
        &handle,
        second_snapshot,
        FactSource::ServerObserved,
        Vec::new(),
        second_world,
    );
    handle.shared.observation.write().snapshot_scope_generation =
        handle.shared.entity_producer.lock().scope_generation;
    let second_facts = handle
        .capture_frame_facts()
        .expect("respawned scope should have a snapshot");
    assert_eq!(second_facts.light, Some(9));
    assert_eq!(second_facts.armor, Some(7));
}

#[test]
fn production_raw_scope_order_resets_old_light_keeps_armor_and_forgets_chunks() {
    let (handle, mut app, owner, shared_world, _source, _events) = producer_test_app();
    assert!(handle
        .shared
        .reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
            owner,
            1,
            Some("minecraft:overworld".to_owned()),
            Some(true),
        ));

    let old_chunk = light_chunk_packet(
        0,
        0,
        light_data_with_masks(
            &[1],
            &[1],
            &[],
            &[],
            vec![packed_light_layer(&[(0, 5)])],
            vec![packed_light_layer(&[(0, 2)])],
        ),
    );
    let armor = attributes_packet(
        99,
        vec![armor_snapshot(
            azalea::registry::builtin::Attribute::Armor,
            8.0,
        )],
    );
    install_dimension_registry(&shared_world, "minecraft:the_nether", Some(false));
    let respawn = reducer_respawn_packet("minecraft:the_nether");
    let new_chunk = light_chunk_packet(
        1,
        0,
        light_data_with_masks(
            &[1],
            &[1],
            &[],
            &[],
            vec![packed_light_layer(&[(0, 7)])],
            vec![packed_light_layer(&[(0, 3)])],
        ),
    );
    let partial_update = light_update_packet(
        1,
        0,
        light_data_with_masks(
            &[1],
            &[1],
            &[],
            &[],
            vec![packed_light_layer(&[(0, 12)])],
            vec![packed_light_layer(&[(0, 12)])],
        ),
    );

    for packet in [old_chunk, armor, respawn, new_chunk, partial_update] {
        if let azalea::protocol::packets::game::ClientboundGamePacket::LevelChunkWithLight(_) =
            &packet
        {
            azalea::packet::game::process_packet(
                app.world_mut(),
                owner,
                &packet,
                synthetic_attempt_token(),
            );
        }
        queue_producer_packet(&mut app, owner, packet);
    }
    app.update();

    let observation = handle.shared.observation.read();
    let context = observation
        .light_cache
        .context
        .as_ref()
        .expect("post-respawn light context");
    assert_eq!(context.dimension, "minecraft:the_nether");
    assert_eq!(context.has_skylight, Some(false));
    assert!(!observation.light_cache.chunks.contains_key(&(0, 0)));
    assert_eq!(observation.armor, Some(8));
    drop(observation);

    let new_world = empty_world();
    assert!(handle.shared.set_world_if_running(new_world.clone()));
    let mut snapshot = snapshot_at(1, 16.25, -64.0, 0.25);
    snapshot.world.dimension = "minecraft:the_nether".to_owned();
    install_viewport_observation(
        &handle,
        snapshot,
        FactSource::ServerObserved,
        Vec::new(),
        new_world,
    );
    let facts = handle
        .capture_frame_facts()
        .expect("new scope snapshot should capture");
    assert_eq!(facts.light, Some(12));
    assert_eq!(facts.armor, Some(8));

    let forget = azalea::protocol::packets::game::ClientboundGamePacket::ForgetLevelChunk(
        azalea::protocol::packets::game::ClientboundForgetLevelChunk {
            pos: azalea::core::position::ChunkPos::new(1, 0),
        },
    );
    azalea::packet::game::process_packet(
        app.world_mut(),
        owner,
        &forget,
        synthetic_attempt_token(),
    );
    queue_producer_packet(&mut app, owner, forget);
    app.update();
    assert!(handle
        .shared
        .observation
        .read()
        .light_cache
        .chunks
        .is_empty());
    assert_eq!(
        handle
            .capture_frame_facts()
            .expect("snapshot remains")
            .light,
        None
    );

    // A new connection epoch physically clears the frame cache and cannot
    // expose either value from the old owner, even before a new snapshot.
    assert!(handle.shared.begin_connection_attempt());
    let observation = handle.shared.observation.read();
    assert!(observation.light_cache.chunks.is_empty());
    assert_eq!(observation.armor, None);
    assert!(observation.snapshot.is_none());
    drop(shared_world);
}

#[test]
fn light_cache_mutations_and_scope_resets_share_observation_generation() {
    let (handle, mut app, owner, shared_world, _source, _events) =
        producer_test_app_without_world();
    assert!(handle
        .shared
        .reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
            owner,
            1,
            Some("minecraft:overworld".to_owned()),
            Some(true),
        ));
    let generation_after_login = handle.shared.observation.read().generation;

    let full_chunk = light_chunk_packet(
        0,
        0,
        light_data_with_masks(
            &[1],
            &[1],
            &[],
            &[],
            vec![packed_light_layer(&[(0, 10)])],
            vec![packed_light_layer(&[(0, 2)])],
        ),
    );
    azalea::packet::game::process_packet(
        app.world_mut(),
        owner,
        &full_chunk,
        synthetic_attempt_token(),
    );
    queue_producer_packet(&mut app, owner, full_chunk);
    app.update();
    let generation_after_full_chunk = handle.shared.observation.read().generation;
    assert_eq!(generation_after_full_chunk, generation_after_login + 1);

    let partial = light_update_packet(
        0,
        0,
        light_data_with_masks(
            &[],
            &[1],
            &[],
            &[],
            Vec::new(),
            vec![packed_light_layer(&[(0, 6)])],
        ),
    );
    queue_producer_packet(&mut app, owner, partial);
    app.update();
    let generation_after_partial = handle.shared.observation.read().generation;
    assert_eq!(generation_after_partial, generation_after_full_chunk + 1);

    let forget = azalea::protocol::packets::game::ClientboundGamePacket::ForgetLevelChunk(
        azalea::protocol::packets::game::ClientboundForgetLevelChunk {
            pos: azalea::core::position::ChunkPos::new(0, 0),
        },
    );
    queue_producer_packet(&mut app, owner, forget);
    app.update();
    let generation_after_forget = handle.shared.observation.read().generation;
    assert_eq!(generation_after_forget, generation_after_partial + 1);

    assert!(handle
        .shared
        .reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
            owner,
            1,
            Some("minecraft:the_nether".to_owned()),
            Some(false),
        ));
    assert_eq!(
        handle.shared.observation.read().generation,
        generation_after_forget + 1
    );
    drop(shared_world);
}

#[test]
fn frame_facts_capture_cannot_mix_with_an_epoch_reset_at_the_read_boundary() {
    let handle = RuntimeHandle::new(RunConfig::default());
    assert!(handle.shared.begin_connection_attempt());
    let snapshot = snapshot_at(1, 0.25, -64.0, 0.25);
    handle.test_install_frame_facts(snapshot.clone(), Some(9), Some(12));
    let generation_before_capture = handle.shared.observation.read().generation;

    let (start_reset, start_reset_received) = std_mpsc::channel();
    let (about_to_reset, about_to_reset_received) = std_mpsc::channel();
    let (write_boundary, write_boundary_received) = std_mpsc::channel();
    let (reset_finished, reset_finished_received) = std_mpsc::channel();
    let reset_completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let write_gate = Arc::new((parking_lot::Mutex::new(false), parking_lot::Condvar::new()));
    let hook_write_gate = write_gate.clone();
    handle
        .shared
        .set_observation_write_boundary_hook(Some(Arc::new(move || {
            write_boundary
                .send(())
                .expect("reset should reach the observation write boundary");
            let (allowed, wake) = &*hook_write_gate;
            let mut allowed = allowed.lock();
            while !*allowed {
                wake.wait(&mut allowed);
            }
        })));

    let reset_completed_by_thread = reset_completed.clone();
    let shared = handle.shared.clone();
    let reset = thread::spawn(move || {
        start_reset_received
            .recv()
            .expect("reset must wait for an active capture read guard");
        about_to_reset
            .send(())
            .expect("reset should report before entering reset/write");
        let result = shared.begin_connection_attempt();
        reset_completed_by_thread.store(true, std::sync::atomic::Ordering::SeqCst);
        reset_finished
            .send(result)
            .expect("epoch reset result should remain observable");
    });

    let probe_shared = handle.shared.clone();
    let captured = handle
        .shared
        .capture_frame_facts_with_test_hooks(
            || {
                start_reset
                    .send(())
                    .expect("capture must start reset after taking its read guard");
                about_to_reset_received
                    .recv_timeout(StdDuration::from_secs(1))
                    .expect("reset should report before entering reset/write");
                write_boundary_received
                    .recv_timeout(StdDuration::from_secs(1))
                    .expect("reset should reach the observation write boundary");
                assert!(
                    !reset_completed.load(std::sync::atomic::Ordering::SeqCst),
                    "reset cannot complete while the capture read guard is held"
                );
                let (allowed, wake) = &*write_gate;
                *allowed.lock() = true;
                wake.notify_one();
            },
            move || {
                assert!(
                    probe_shared.observation.try_write().is_none(),
                    "capture must retain its observation read guard through fact reads"
                );
            },
        )
        .expect("the old frame should be captured coherently");

    assert_eq!(captured.snapshot, snapshot);
    assert_eq!(captured.armor, Some(9));
    assert_eq!(captured.light, Some(12));
    assert!(reset_finished_received
        .recv_timeout(StdDuration::from_secs(1))
        .expect("epoch reset should complete after capture"));
    reset.join().expect("epoch reset thread");
    assert!(handle.capture_frame_facts().is_none());
    assert!(handle.shared.observation.read().generation > generation_before_capture);
}

#[test]
fn light_cache_decodes_nibbles_masks_retain_bad_layers_and_full_replace() {
    let mut ecs_world = bevy_ecs::world::World::new();
    let entity = ecs_world.spawn_empty().id();
    let source = CanonicalSourceAdmission {
        entity,
        epoch: 1,
        scope_generation: 4,
        attempt_token: None,
    };
    let geometry = LightSectionGeometry {
        min_light_section: -5,
        light_section_count: 26,
    };
    let default_world = azalea::world::World::default();
    assert_eq!(
        LightSectionGeometry::from_world(&default_world),
        Some(geometry)
    );
    let mut cache = LightCache::default();

    let low_index = (3 << 8) | (2 << 4);
    let high_index = low_index | 1;
    let first = light_data_with_masks(
        &[1],
        &[1],
        &[],
        &[],
        vec![packed_light_layer(&[(low_index, 2), (high_index, 13)])],
        vec![packed_light_layer(&[(low_index, 7), (high_index, 4)])],
    );
    assert!(cache.apply_packet(
        source,
        "minecraft:overworld".to_owned(),
        Some(true),
        geometry,
        0,
        0,
        &first,
        true,
    ));
    assert_eq!(
        cache.value_at(
            &Vec3Value {
                x: 0.0,
                y: -61.0,
                z: 2.0,
            },
            1,
            4,
            "minecraft:overworld",
        ),
        Some(7)
    );
    assert_eq!(
        cache.value_at(
            &Vec3Value {
                x: 1.0,
                y: -61.0,
                z: 2.0,
            },
            1,
            4,
            "minecraft:overworld",
        ),
        Some(13)
    );

    let empty = light_data_with_masks(&[], &[], &[2], &[2], Vec::new(), Vec::new());
    assert!(cache.apply_packet(
        source,
        "minecraft:overworld".to_owned(),
        Some(true),
        geometry,
        0,
        0,
        &empty,
        false,
    ));
    assert_eq!(cache.layer(0, 0, 2, true).map(|layer| layer[0]), Some(0));
    assert_eq!(cache.layer(0, 0, 2, false).map(|layer| layer[0]), Some(0));

    // Empty masks do not erase an ordinary layer; a data mask on the same
    // bit wins over its empty counterpart.
    let partial = light_data_with_masks(
        &[2],
        &[2],
        &[2],
        &[2],
        vec![packed_light_layer(&[(0, 8)])],
        vec![packed_light_layer(&[(0, 3)])],
    );
    assert!(cache.apply_packet(
        source,
        "minecraft:overworld".to_owned(),
        Some(true),
        geometry,
        0,
        0,
        &partial,
        false,
    ));
    assert_eq!(cache.layer(0, 0, 2, true).map(|layer| layer[0]), Some(8));
    assert_eq!(cache.layer(0, 0, 2, false).map(|layer| layer[0]), Some(3));
    assert_eq!(cache.layer(0, 0, 1, true).map(|layer| layer[0]), Some(0));

    // A bad first array must not shift the valid second array onto the
    // first bit. Missing arrays similarly make only their own layer unknown.
    let bad_then_valid = light_data_with_masks(
        &[1, 2],
        &[],
        &[],
        &[],
        vec![
            vec![0; 2047].into_boxed_slice(),
            packed_light_layer(&[(0, 9)]),
        ],
        Vec::new(),
    );
    assert!(cache.apply_packet(
        source,
        "minecraft:overworld".to_owned(),
        Some(true),
        geometry,
        0,
        0,
        &bad_then_valid,
        false,
    ));
    assert!(cache.layer(0, 0, 1, true).is_none());
    assert_eq!(cache.layer(0, 0, 2, true).map(|layer| layer[0]), Some(9));

    let missing_second = light_data_with_masks(
        &[1, 2],
        &[],
        &[],
        &[],
        vec![packed_light_layer(&[(0, 6)])],
        Vec::new(),
    );
    assert!(cache.apply_packet(
        source,
        "minecraft:overworld".to_owned(),
        Some(true),
        geometry,
        0,
        0,
        &missing_second,
        false,
    ));
    assert_eq!(cache.layer(0, 0, 1, true).map(|layer| layer[0]), Some(6));
    assert!(cache.layer(0, 0, 2, true).is_none());

    let out_of_range = light_data_with_masks(
        &[30],
        &[],
        &[],
        &[],
        vec![packed_light_layer(&[(0, 15)])],
        Vec::new(),
    );
    assert!(cache.apply_packet(
        source,
        "minecraft:overworld".to_owned(),
        Some(true),
        geometry,
        0,
        0,
        &out_of_range,
        false,
    ));
    assert!(cache.layer(0, 0, 30, true).is_none());

    let retain =
        azalea::protocol::packets::game::c_light_update::ClientboundLightUpdatePacketData::default(
        );
    assert!(cache.apply_packet(
        source,
        "minecraft:overworld".to_owned(),
        Some(true),
        geometry,
        0,
        0,
        &retain,
        false,
    ));
    assert_eq!(cache.layer(0, 0, 1, true).map(|layer| layer[0]), Some(6));

    let replace = light_data_with_masks(&[], &[], &[], &[], Vec::new(), Vec::new());
    assert!(cache.apply_packet(
        source,
        "minecraft:overworld".to_owned(),
        Some(true),
        geometry,
        0,
        0,
        &replace,
        true,
    ));
    assert!(cache.layer(0, 0, 1, true).is_none());
    assert!(cache.remove_chunk(source, "minecraft:overworld", 0, 0));
    assert!(cache.chunks.is_empty());
}

#[test]
fn light_cache_unknown_max_skylight_false_and_floor_rules_are_fail_closed() {
    let mut ecs_world = bevy_ecs::world::World::new();
    let entity = ecs_world.spawn_empty().id();
    let source = CanonicalSourceAdmission {
        entity,
        epoch: 1,
        scope_generation: 1,
        attempt_token: None,
    };
    let geometry = LightSectionGeometry {
        min_light_section: -5,
        light_section_count: 26,
    };
    let mut cache = LightCache::default();

    let below_fifteen = light_data_with_masks(
        &[1],
        &[],
        &[],
        &[],
        vec![packed_light_layer(&[(0, 14)])],
        Vec::new(),
    );
    assert!(cache.apply_packet(
        source,
        "minecraft:overworld".to_owned(),
        Some(true),
        geometry,
        0,
        0,
        &below_fifteen,
        true,
    ));
    assert_eq!(
        cache.value_at(
            &Vec3Value {
                x: 0.0,
                y: -64.0,
                z: 0.0,
            },
            1,
            1,
            "minecraft:overworld",
        ),
        None
    );

    let fifteen = light_data_with_masks(
        &[1],
        &[],
        &[],
        &[],
        vec![packed_light_layer(&[(0, 15)])],
        Vec::new(),
    );
    assert!(cache.apply_packet(
        source,
        "minecraft:overworld".to_owned(),
        Some(true),
        geometry,
        0,
        0,
        &fifteen,
        true,
    ));
    assert_eq!(
        cache.value_at(
            &Vec3Value {
                x: 0.0,
                y: -64.0,
                z: 0.0,
            },
            1,
            1,
            "minecraft:overworld",
        ),
        Some(15)
    );

    cache.reset_scope(1, 2, Some("minecraft:nether".to_owned()), Some(false));
    let no_sky = light_data_with_masks(
        &[1],
        &[1],
        &[],
        &[],
        vec![vec![0; 2047].into_boxed_slice()],
        vec![packed_light_layer(&[(0, 3)])],
    );
    let source = CanonicalSourceAdmission {
        scope_generation: 2,
        ..source
    };
    assert!(cache.apply_packet(
        source,
        "minecraft:nether".to_owned(),
        Some(false),
        geometry,
        0,
        0,
        &no_sky,
        true,
    ));
    assert_eq!(cache.layer(0, 0, 1, true).map(|layer| layer[0]), Some(0));
    assert_eq!(
        cache.value_at(
            &Vec3Value {
                x: 0.0,
                y: -64.0,
                z: 0.0,
            },
            1,
            2,
            "minecraft:nether",
        ),
        Some(3)
    );
    assert_eq!(floor_block_coordinate(-0.1), Some(-1));
    assert_eq!(floor_block_coordinate(15.9), Some(15));
    assert_eq!(floor_block_coordinate(f64::NAN), None);
    assert_eq!(floor_block_coordinate(f64::INFINITY), None);
}
