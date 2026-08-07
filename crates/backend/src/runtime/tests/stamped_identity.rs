use super::production_support::*;
use super::*;

fn stamped_block_app() -> (
    RuntimeHandle,
    App,
    bevy_ecs::entity::Entity,
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
    app.world_mut().entity_mut(owner).insert((
        azalea::local_player::WorldHolder::new(owner, empty_world()),
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
    let events = handle.subscribe();
    (handle, app, owner, events)
}

fn bind_stamped_attempt(
    handle: &RuntimeHandle,
    app: &mut App,
    owner: bevy_ecs::entity::Entity,
    token: azalea::join::AttemptToken,
) {
    let epoch = handle.shared.connection_epoch();
    assert!(handle
        .shared
        .admit_canonical_join_started_with_token(epoch, Some(token)));
    assert_eq!(
        handle
            .shared
            .consume_attempt_for_transport_init_and_bind_with_token(owner, Some(token)),
        Some(epoch)
    );
    app.world_mut()
        .insert_resource(super::entity_events_owner_tests::TestAttemptToken(token));
}

fn drain_events(receiver: &mut RuntimeEventReceiver) -> Vec<BackendEventEnvelope> {
    std::iter::from_fn(|| receiver.try_recv().ok()).collect()
}

#[test]
fn stamped_rebind_restores_production_and_late_a_packets_are_rejected() {
    let (handle, mut app, owner, mut events) = stamped_block_app();
    assert!(handle.shared.begin_connection_attempt());
    let token_a = azalea::join::AttemptToken::mint();
    bind_stamped_attempt(&handle, &mut app, owner, token_a);
    let _ = drain_events(&mut events);

    // A packet is admitted on epoch 1 through the stamped source path.
    // 载体是声音包：方块生产者已删，本测试的主题是 token 绑定而不是方块。
    queue_production_sound_packet(&mut app, owner, token_a, 1.0);
    app.update();
    assert_eq!(sound_events(&mut events).len(), 1);

    assert!(handle
        .shared
        .admit_canonical_disconnected_with_token(owner, 1, None, Some(token_a))
        .is_some());
    let _ = drain_events(&mut events);

    // B reuses the same entity. The legacy fence becomes ambiguous, but
    // stamped B sources must still reach production.
    assert!(handle.shared.begin_connection_attempt());
    let token_b = azalea::join::AttemptToken::mint();
    bind_stamped_attempt(&handle, &mut app, owner, token_b);
    let _ = drain_events(&mut events);

    queue_production_sound_packet(&mut app, owner, token_b, 1.25);
    app.update();
    assert_eq!(
        sound_events(&mut events).len(),
        1,
        "stamped B must be accepted even after same-entity reuse made the fence ambiguous"
    );

    // A late A packet is rejected at the source admission; even if a
    // packet were queued, no envelope may be produced for it.
    assert!(
        handle
            .shared
            .admit_canonical_source_with_token(owner, Some(token_a))
            .is_none(),
        "a late A packet must be rejected at the stamped source admission"
    );
    queue_production_sound_packet(&mut app, owner, token_a, 1.5);
    app.update();
    assert!(
        sound_events(&mut events).is_empty(),
        "a late A packet must not publish into B's epoch"
    );
}

#[test]
fn late_a_lifecycle_sources_cannot_claim_b() {
    let (handle, mut app, owner, mut events) = stamped_block_app();
    assert!(handle.shared.begin_connection_attempt());
    let token_a = azalea::join::AttemptToken::mint();
    bind_stamped_attempt(&handle, &mut app, owner, token_a);
    let _ = drain_events(&mut events);
    assert!(handle
        .shared
        .admit_canonical_disconnected_with_token(owner, 1, None, Some(token_a))
        .is_some());
    let _ = drain_events(&mut events);

    assert!(handle.shared.begin_connection_attempt());
    let token_b = azalea::join::AttemptToken::mint();
    bind_stamped_attempt(&handle, &mut app, owner, token_b);
    let _ = drain_events(&mut events);
    let epoch = handle.shared.connection_epoch();
    assert_eq!(epoch, 2);

    // Stamped packet admission: A rejected, B accepted.
    assert!(handle
        .shared
        .admit_canonical_source_with_token(owner, Some(token_a))
        .is_none());
    let source_b = handle
        .shared
        .admit_canonical_source_with_token(owner, Some(token_b))
        .expect("stamped B source must be admitted");
    assert_eq!(source_b.attempt_token, Some(token_b));

    // WorldLoaded boundary: A rejected, B accepted.
    assert!(!handle
        .shared
        .observe_dimension_from_world_boundary_with_token(
            owner,
            "minecraft:the_nether",
            Some(token_a),
        ));
    assert!(handle
        .shared
        .observe_dimension_from_world_boundary_with_token(
            owner,
            "minecraft:the_nether",
            Some(token_b),
        ));

    // Disconnect: late A cannot close B; matching B closes normally.
    assert!(handle
        .shared
        .admit_canonical_disconnected_with_token(owner, 2, None, Some(token_a))
        .is_none());
    assert!(handle
        .shared
        .admit_canonical_disconnected_with_token(owner, 2, None, Some(token_b))
        .is_some());
    let _ = drain_events(&mut events);

    // ConnectionFailed: late A cannot close a fresh B.
    assert!(handle.shared.begin_connection_attempt());
    let token_b2 = azalea::join::AttemptToken::mint();
    bind_stamped_attempt(&handle, &mut app, owner, token_b2);
    let _ = drain_events(&mut events);
    assert!(handle
        .shared
        .admit_canonical_connection_failed_with_token(owner, 3, "probe".to_owned(), Some(token_a))
        .is_none());
    assert!(handle
        .shared
        .admit_canonical_connection_failed_with_token(owner, 3, "probe".to_owned(), Some(token_b2),)
        .is_some());

    // Init/Client binding: A's token cannot consume B's reservation.
    assert!(handle.shared.begin_connection_attempt());
    let token_b3 = azalea::join::AttemptToken::mint();
    assert!(handle
        .shared
        .admit_canonical_join_started_with_token(4, Some(token_b3)));
    assert_eq!(
        handle
            .shared
            .consume_attempt_for_transport_init_and_bind_with_token(owner, Some(token_a)),
        None
    );
    assert_eq!(
        handle
            .shared
            .consume_attempt_for_transport_init_and_bind_with_token(owner, Some(token_b3)),
        Some(4)
    );

    // Client token gate: late A client rejected, matching B accepted.
    let empty_world = Arc::new(parking_lot::RwLock::new(bevy_ecs::world::World::new()));
    let client_a = azalea::Client::new_with_attempt_token(owner, empty_world.clone(), token_a);
    let client_b = azalea::Client::new_with_attempt_token(owner, empty_world, token_b3);
    assert!(!handle.shared.client_is_current_owner(&client_a));
    assert!(handle.shared.client_is_current_owner(&client_b));

    // Swarm disconnect: late A cannot claim a reconnect for B.
    assert!(!handle.shared.claim_reconnect_with_token(Some(token_a)));
    assert!(handle
        .shared
        .admit_canonical_disconnected_with_token(owner, 4, None, Some(token_b3))
        .is_some());
    assert!(handle.shared.claim_reconnect_with_token(Some(token_b3)));
}

#[test]
fn source_token_bindings_are_one_to_one_and_idempotent() {
    let mut bindings = SourceTokenBindings::default();
    let a = azalea::join::AttemptToken::mint();
    let b = azalea::join::AttemptToken::mint();
    let c = azalea::join::AttemptToken::mint();

    assert!(bindings.bind(a, 1));
    assert!(
        !bindings.bind(a, 2),
        "A must never be re-registered on epoch 2"
    );
    assert!(bindings.bind(b, 2));
    assert!(!bindings.bind(c, 2), "epoch 2 cannot switch to C");
    assert!(bindings.bind(a, 1), "the same pair is idempotent");
    assert!(bindings.matches(a, 1));
    assert!(!bindings.matches(a, 2));
}

#[test]
fn stale_client_cannot_dequeue_current_owners_command() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut ecs_world = bevy_ecs::world::World::new();
    let owner = ecs_world.spawn_empty().id();

    assert!(handle.shared.begin_connection_attempt());
    let token_a = azalea::join::AttemptToken::mint();
    assert!(handle
        .shared
        .admit_canonical_join_started_with_token(1, Some(token_a)));
    assert_eq!(
        handle
            .shared
            .consume_attempt_for_transport_init_and_bind_with_token(owner, Some(token_a)),
        Some(1)
    );

    // Rebind the same entity as B, then enqueue work owned by the now-current
    // runtime. A late A Tick/Spawn copy must leave that queue untouched.
    assert!(handle.shared.begin_connection_attempt());
    let token_b = azalea::join::AttemptToken::mint();
    assert!(handle
        .shared
        .admit_canonical_join_started_with_token(2, Some(token_b)));
    assert_eq!(
        handle
            .shared
            .consume_attempt_for_transport_init_and_bind_with_token(owner, Some(token_b)),
        Some(2)
    );
    handle.shared.ready.store(true, Ordering::Release);
    handle.respawn().expect("queue current-owner command");

    let empty_world = Arc::new(parking_lot::RwLock::new(bevy_ecs::world::World::new()));
    let client_a = azalea::Client::new_with_attempt_token(owner, empty_world.clone(), token_a);
    let client_b = azalea::Client::new_with_attempt_token(owner, empty_world, token_b);
    assert!(handle.shared.next_command_for_client(&client_a).is_none());
    assert_eq!(handle.shared.commands.lock().len(), 1);
    assert!(handle.shared.next_command_for_client(&client_b).is_some());
    assert!(handle.shared.commands.lock().is_empty());
}

#[test]
fn historical_token_cannot_bind_to_a_later_epoch() {
    let handle = RuntimeHandle::new(RunConfig::default());
    assert!(handle.shared.begin_connection_attempt());
    let token_a = azalea::join::AttemptToken::mint();
    assert!(handle
        .shared
        .admit_canonical_join_started_with_token(1, Some(token_a)));
    assert!(handle.shared.begin_connection_attempt());
    assert!(
        !handle
            .shared
            .admit_canonical_join_started_with_token(2, Some(token_a)),
        "a historical token must not be re-registered on a later epoch"
    );
    let token_b = azalea::join::AttemptToken::mint();
    assert!(handle
        .shared
        .admit_canonical_join_started_with_token(2, Some(token_b)));
}

#[test]
fn rejected_start_join_cannot_poison_the_reconnect_reservation() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut ecs_world = bevy_ecs::world::World::new();
    let entity = ecs_world.spawn_empty().id();

    assert!(handle.shared.begin_connection_attempt());
    let first_token = azalea::join::AttemptToken::mint();
    assert!(handle
        .shared
        .admit_canonical_join_started_with_token(1, Some(first_token)));
    assert_eq!(
        handle
            .shared
            .consume_attempt_for_transport_init_and_bind_with_token(entity, Some(first_token)),
        Some(1)
    );
    assert!(handle
        .shared
        .admit_canonical_disconnected_with_token(entity, 1, None, Some(first_token))
        .is_some());
    assert!(handle.shared.claim_reconnect_with_token(Some(first_token)));
    let reconnect_token = handle
        .shared
        .admit_reconnect_attempt()
        .expect("reconnect reservation");
    assert_eq!(handle.shared.connection_epoch(), 2);

    // Suspend the exact reconnect add after reserving epoch 2. A StartJoin
    // observed in this window must fail without claiming either side of the
    // token↔epoch map or changing the reservation/writer identity.
    handle
        .shared
        .reconnect_add_pending
        .store(false, Ordering::Release);
    let rejected_token = azalea::join::AttemptToken::mint();
    let writer_before = handle.shared.writer.lock().context();
    let (attempt_before, token_count_before, epoch_count_before) = {
        let producer = handle.shared.entity_producer.lock();
        (
            producer.attempt,
            producer.source_token_bindings.token_to_epoch.len(),
            producer.source_token_bindings.epoch_to_token.len(),
        )
    };

    assert!(!handle
        .shared
        .admit_canonical_join_started_with_token(2, Some(rejected_token)));
    assert_eq!(handle.shared.writer.lock().context(), writer_before);
    {
        let producer = handle.shared.entity_producer.lock();
        assert_eq!(producer.attempt, attempt_before);
        assert_eq!(
            producer.source_token_bindings.token_to_epoch.len(),
            token_count_before
        );
        assert_eq!(
            producer.source_token_bindings.epoch_to_token.len(),
            epoch_count_before
        );
        assert!(!producer.source_token_bindings.matches(rejected_token, 2));
    }

    handle
        .shared
        .reconnect_add_pending
        .store(true, Ordering::Release);
    assert_eq!(
        handle.shared.bind_reconnect_return_with_token(
            reconnect_token,
            entity,
            Some(rejected_token),
        ),
        None,
        "a rejected, never-bound token cannot consume B's reservation"
    );

    let correct_token = azalea::join::AttemptToken::mint();
    assert!(handle
        .shared
        .admit_canonical_join_started_with_token(2, Some(correct_token)));
    assert_eq!(
        handle.shared.bind_reconnect_return_with_token(
            reconnect_token,
            entity,
            Some(correct_token),
        ),
        Some(2),
        "the correct B token remains admissible after the rejected candidate"
    );
}

#[test]
fn reconnect_return_client_token_must_match_start_join_token() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    let mut ecs_world = bevy_ecs::world::World::new();
    let entity = ecs_world.spawn_empty().id();

    assert!(handle.shared.begin_connection_attempt());
    let token_a = azalea::join::AttemptToken::mint();
    assert!(handle
        .shared
        .admit_canonical_join_started_with_token(1, Some(token_a)));
    assert_eq!(
        handle
            .shared
            .consume_attempt_for_transport_init_and_bind_with_token(entity, Some(token_a)),
        Some(1)
    );
    assert!(handle
        .shared
        .admit_canonical_disconnected_with_token(entity, 1, None, Some(token_a))
        .is_some());
    let _ = drain_events(&mut events);

    assert!(handle.shared.claim_reconnect_with_token(Some(token_a)));
    let reconnect_token = handle
        .shared
        .admit_reconnect_attempt()
        .expect("reconnect reservation");
    let token_b = azalea::join::AttemptToken::mint();
    assert!(handle
        .shared
        .admit_canonical_join_started_with_token(2, Some(token_b)));

    // A different client token than the StartJoin token is rejected.
    assert_eq!(
        handle
            .shared
            .bind_reconnect_return_with_token(reconnect_token, entity, Some(token_a)),
        None
    );
    // The matching token binds and installs the active client.
    assert_eq!(
        handle
            .shared
            .bind_reconnect_return_with_token(reconnect_token, entity, Some(token_b)),
        Some(2)
    );
    let empty_world = Arc::new(parking_lot::RwLock::new(bevy_ecs::world::World::new()));
    let client_b = azalea::Client::new_with_attempt_token(entity, empty_world, token_b);
    assert!(handle.shared.set_active_client_if_current(&client_b));
    handle.shared.finish_reconnect_attempt(reconnect_token);
}

#[test]
fn tokenless_fallback_first_connect_works_but_same_entity_rebind_fails_closed() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    let mut ecs_world = bevy_ecs::world::World::new();
    let entity = ecs_world.spawn_empty().id();

    // First, never-reused connect keeps the legacy tokenless behavior.
    assert!(handle.shared.begin_connection_attempt());
    assert!(handle.shared.admit_canonical_join_started(1));
    assert_eq!(
        handle
            .shared
            .consume_attempt_for_transport_init_and_bind(entity),
        Some(1)
    );
    let _ = drain_events(&mut events);
    assert!(handle.shared.admit_canonical_source(entity).is_some());

    // Same-entity rebind without a token must stay fail closed; the
    // current B token is never back-stamped onto A events.
    assert!(handle
        .shared
        .admit_canonical_disconnected(entity, 1, None)
        .is_some());
    let _ = drain_events(&mut events);
    assert!(handle.shared.begin_connection_attempt());
    assert!(!handle.shared.admit_canonical_join_started(2));
    assert!(handle.shared.admit_canonical_source(entity).is_none());
}

#[tokio::test]
async fn stamped_admission_publication_race_rejects_a_after_b_rebind() {
    let (handle, mut app, owner, mut events) = stamped_block_app();
    assert!(handle.shared.begin_connection_attempt());
    let token_a = azalea::join::AttemptToken::mint();
    bind_stamped_attempt(&handle, &mut app, owner, token_a);
    let _ = drain_events(&mut events);
    let source = handle
        .shared
        .admit_canonical_source_with_token(owner, Some(token_a))
        .expect("A source must be admitted while A is current");

    let (checked_tx, checked_rx) = std_mpsc::channel();
    let (release_tx, release_rx) = std_mpsc::channel();
    let release_rx = Arc::new(StdMutex::new(Some(release_rx)));
    // The probe runs outside `command_admission`, so the main thread can
    // rebind B while A's publication is suspended between source
    // admission and the publication recheck.
    handle
        .shared
        .set_canonical_publication_probe(Some(Arc::new({
            let release_rx = release_rx.clone();
            move || {
                checked_tx.send(()).expect("hook reached");
                release_rx
                    .lock()
                    .expect("release gate lock")
                    .take()
                    .expect("one publication owns the gate")
                    .recv()
                    .expect("publication release");
            }
        })));

    let publish_shared = handle.shared.clone();
    let publisher = thread::spawn(move || {
        publish_shared.emit_canonical_observation_event(
            source,
            BackendEventPayload::Block(ContractProtocolBlockEvent::ChunkLoaded {
                chunk_x: 0,
                chunk_z: 0,
            }),
        )
    });
    checked_rx
        .recv_timeout(StdDuration::from_secs(1))
        .expect("A publication must reach the admission hook");

    // B rebinds the same entity while A's publication is suspended between
    // admission and publication.
    assert!(handle.shared.begin_connection_attempt());
    let token_b = azalea::join::AttemptToken::mint();
    bind_stamped_attempt(&handle, &mut app, owner, token_b);

    release_tx.send(()).expect("release A publication");
    assert!(
        !publisher.join().expect("publisher must finish"),
        "A publication must be rejected after B rebinds the same entity"
    );
}
