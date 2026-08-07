use super::production_support::*;
use super::*;

fn reducer_common_spawn_info(
    dimension: &str,
) -> azalea::protocol::packets::common::CommonPlayerSpawnInfo {
    use azalea::core::game_type::{GameMode, OptionalGameType};
    use azalea::protocol::packets::common::CommonPlayerSpawnInfo;
    use azalea::registry::data::DimensionKind;

    CommonPlayerSpawnInfo {
        dimension_type: <DimensionKind as azalea::registry::DataRegistry>::new_raw(0),
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

pub(super) fn reducer_respawn_packet(
    dimension: &str,
) -> azalea::protocol::packets::game::ClientboundGamePacket {
    let common = reducer_common_spawn_info(dimension);
    azalea::protocol::packets::game::ClientboundGamePacket::Respawn(
        azalea::protocol::packets::game::ClientboundRespawn {
            common,
            data_to_keep: 0,
        },
    )
}

#[test]
fn registry_dimension_extra_proves_login_respawn_skylight_semantics() {
    let mut ecs_world = bevy_ecs::world::World::new();
    let owner = ecs_world.spawn_empty().id();
    let common = reducer_common_spawn_info("minecraft:overworld");

    for (proof, expected) in [
        (Some(true), Some(true)),
        (Some(false), Some(false)),
        (None, None),
    ] {
        let shared_world = empty_world();
        install_dimension_registry(&shared_world, "minecraft:overworld", proof);
        let holder = azalea::local_player::WorldHolder::new(owner, shared_world);
        assert_eq!(prove_has_skylight(&common, &holder), expected);
    }
}

fn sound_event_metadata(events: &mut RuntimeEventReceiver) -> Vec<(Option<String>, String)> {
    std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| {
            let dimension = event.dimension.clone();
            match event.payload {
                BackendEventPayload::Sound(payload) => Some((dimension, payload.sound_name?)),
                _ => None,
            }
        })
        .collect()
}

#[test]
fn production_block_sound_raw_reducer_preserves_sound_scope_at_packet_position() {
    let run = |packets: Vec<azalea::protocol::packets::game::ClientboundGamePacket>| {
        let (_handle, mut app, owner, _shared_world, _source, mut events) = producer_test_app();
        for packet in packets {
            queue_producer_packet(&mut app, owner, packet);
        }
        app.update();
        sound_event_metadata(&mut events)
    };

    assert_eq!(
        run(vec![
            sound_packet(
                azalea::registry::Holder::Direct(azalea::core::sound::CustomSound {
                    sound_id: azalea::Identifier::from("custom:old"),
                    range: None,
                }),
                1.0,
                1.0,
            ),
            reducer_respawn_packet("minecraft:the_nether"),
            sound_packet(
                azalea::registry::Holder::Direct(azalea::core::sound::CustomSound {
                    sound_id: azalea::Identifier::from("custom:new"),
                    range: None,
                }),
                1.0,
                1.0,
            ),
        ]),
        vec![
            (None, "custom:old".to_owned()),
            (
                Some("minecraft:the_nether".to_owned()),
                "custom:new".to_owned()
            ),
        ]
    );
    assert_eq!(
        run(vec![
            reducer_respawn_packet("minecraft:the_nether"),
            sound_packet(
                azalea::registry::Holder::Direct(azalea::core::sound::CustomSound {
                    sound_id: azalea::Identifier::from("custom:new-only"),
                    range: None,
                }),
                1.0,
                1.0,
            ),
        ]),
        vec![(
            Some("minecraft:the_nether".to_owned()),
            "custom:new-only".to_owned()
        )]
    );
}

#[test]
fn production_block_sound_named_direct_and_reference_are_canonical_and_invalid_is_dropped() {
    let (_handle, mut app, owner, _shared_world, _source, mut events) = producer_test_app();
    queue_producer_packet(
        &mut app,
        owner,
        sound_packet(
            azalea::registry::Holder::Direct(azalea::core::sound::CustomSound {
                sound_id: azalea::Identifier::from("custom:bell"),
                range: None,
            }),
            0.75,
            1.25,
        ),
    );
    queue_producer_packet(
        &mut app,
        owner,
        sound_packet(
            azalea::registry::Holder::Reference(azalea::registry::builtin::SoundEvent::AmbientCave),
            1.0,
            0.5,
        ),
    );
    for (volume, pitch) in [
        (f32::NAN, 1.0),
        (f32::INFINITY, 1.0),
        (-1.0, 1.0),
        (1.0, f32::NAN),
        (1.0, f32::INFINITY),
    ] {
        queue_producer_packet(
            &mut app,
            owner,
            sound_packet(
                azalea::registry::Holder::Reference(
                    azalea::registry::builtin::SoundEvent::AmbientCave,
                ),
                volume,
                pitch,
            ),
        );
    }
    app.update();

    let sounds = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event.payload {
            BackendEventPayload::Sound(payload) => Some(payload),
            _ => None,
        })
        .collect::<Vec<ContractProtocolSoundPayload>>();
    assert_eq!(sounds.len(), 2);
    assert_eq!(sounds[0].event_type, ContractHeardSoundType::Heard);
    assert_eq!(sounds[0].sound_name.as_deref(), Some("custom:bell"));
    assert_eq!(
        sounds[1].sound_name.as_deref(),
        Some("minecraft:ambient.cave")
    );
    for sound in &sounds {
        assert!(sound.sound_id.is_none());
        assert!(sound.category.is_none());
        assert_eq!(
            sound.protocol_source,
            ContractProtocolSoundSource::NamedSoundEffect
        );
        assert!(!sound.sound_key.is_empty());
        assert_eq!(
            (
                sound.source_position.x,
                sound.source_position.y,
                sound.source_position.z
            ),
            (1.125, -1.0, 2.0)
        );
    }
    assert_ne!(sounds[0].sound_key, sounds[1].sound_key);
    assert_eq!((sounds[0].volume, sounds[0].pitch), (0.75, 1.25));
    assert_eq!((sounds[1].volume, sounds[1].pitch), (1.0, 0.5));
}

#[test]
fn canonical_observation_late_scope_publication_is_fail_closed() {
    let (handle, _app, owner, _shared_world, _source, mut events) = producer_test_app();
    let source = handle
        .shared
        .admit_canonical_source(owner)
        .expect("current owner admission");
    assert!(handle
        .shared
        .reset_entity_scope_for_owner_at_epoch(owner, source.epoch));
    assert!(!handle.shared.emit_canonical_observation_event(
        source,
        BackendEventPayload::Block(ContractProtocolBlockEvent::ChunkLoaded {
            chunk_x: -1,
            chunk_z: -2,
        }),
    ));
    assert!(events.try_recv().is_err());
}
