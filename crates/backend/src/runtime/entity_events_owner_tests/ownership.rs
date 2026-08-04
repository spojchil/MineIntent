use super::*;

#[test]
fn owner_binding_rejects_late_a_and_preserves_b_epoch2_shadow() {
    let shared = Arc::new(SharedRuntime::new(RunConfig::default()));
    let mut world = bevy_ecs::world::World::new();
    let owner_a = world.spawn_empty().id();
    let owner_b = world.spawn_empty().id();

    assert!(shared.begin_connection_attempt());
    assert_eq!(
        shared.consume_attempt_for_transport_init_and_bind(owner_a),
        Some(1)
    );
    assert_eq!(shared.entity_producer_epoch_for(owner_a), Some(1));
    assert!(matches!(
        shared.apply_entity_input_for_owner(
            owner_a,
            1,
            EntityProducerInput::Spawn {
                token: token(1, 1),
                snapshot: snapshot(1, 7, 1.0),
            },
        ),
        Some(NormalizedEntityEvent::Spawned { .. })
    ));

    assert!(shared.begin_connection_attempt());
    assert_eq!(
        shared.consume_attempt_for_transport_init_and_bind(owner_b),
        Some(2)
    );
    assert_eq!(shared.entity_producer_epoch_for(owner_a), None);
    assert_eq!(shared.entity_producer_epoch_for(owner_b), Some(2));

    assert!(shared
        .apply_entity_input_for_owner(
            owner_a,
            1,
            EntityProducerInput::Spawn {
                token: token(1, 2),
                snapshot: snapshot(1, 8, 99.0),
            },
        )
        .is_none());

    let spawned = shared.apply_entity_input_for_owner(
        owner_b,
        2,
        EntityProducerInput::Spawn {
            token: token(2, 1),
            snapshot: snapshot(2, 7, 10.0),
        },
    );
    assert!(matches!(
        spawned,
        Some(NormalizedEntityEvent::Spawned { ref entity })
            if entity.entity_key() == "2:7" && entity.position[0] == 10.0
    ));

    // A late lifecycle message cannot reset or deactivate B's scope.
    assert!(!shared.reset_entity_scope_for_owner(owner_a));
    assert!(!shared.deactivate_entity_producer_owner(owner_a));
    assert_eq!(shared.entity_producer_epoch_for(owner_b), Some(2));

    let moved = shared.apply_entity_input_for_owner(
        owner_b,
        2,
        EntityProducerInput::Move {
            token: token(2, 2),
            patch: EntityMovePatch::relative(
                EntityIdentity::new(2, 7),
                Some([4096, 0, 0]),
                None,
                false,
            ),
        },
    );
    assert!(matches!(
        moved,
        Some(NormalizedEntityEvent::Moved { ref entity })
            if entity.entity_key() == "2:7"
                && entity.position[0] == 11.0
                && !entity.on_ground
    ));

    let removed = shared.apply_entity_input_for_owner(
        owner_b,
        2,
        EntityProducerInput::Remove {
            token: token(2, 3),
            entity: EntityIdentity::new(2, 7),
        },
    );
    assert!(matches!(
        removed,
        Some(NormalizedEntityEvent::Removed { entity, ref last })
            if entity.key() == "2:7" && last.position[0] == 11.0
    ));

    assert!(shared.deactivate_entity_producer_owner(owner_b));
    assert_eq!(shared.entity_producer_epoch_for(owner_b), None);
    assert!(shared
        .entity_producer
        .lock()
        .cache
        .apply(
            2,
            EntityProducerInput::Spawn {
                token: token(2, 4),
                snapshot: snapshot(2, 9, 12.0),
            },
        )
        .is_none());
    assert!(shared
        .apply_entity_input_for_owner(
            owner_b,
            2,
            EntityProducerInput::Spawn {
                token: token(2, 5),
                snapshot: snapshot(2, 9, 12.0),
            },
        )
        .is_none());
}

#[test]
fn accepted_a_payload_is_dropped_after_b_binds_instead_of_using_b_envelope() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    let mut world = bevy_ecs::world::World::new();
    let owner_a = world.spawn_empty().id();
    let owner_b = world.spawn_empty().id();

    assert!(handle.shared.begin_connection_attempt());
    let request_a = events.try_recv().expect("attempt 1 request");
    assert_eq!(request_a.connection_epoch, 1);
    assert_eq!(
        handle
            .shared
            .consume_attempt_for_transport_init_and_bind(owner_a),
        Some(1)
    );
    let _transport_a = events.try_recv().expect("A transport connected");

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
            owner_a,
            1,
            EntityProducerInput::Spawn {
                token: token(1, 1),
                snapshot: snapshot(1, 7, 1.0),
            },
        )
    });
    after_apply.wait();

    // The old implementation would resume A after this complete switch
    // and let EventWriter stamp A's payload with epoch 2.
    assert!(handle.shared.begin_connection_attempt());
    let request_b = events.try_recv().expect("attempt 2 request");
    assert_eq!(request_b.connection_epoch, 2);
    assert_eq!(
        handle
            .shared
            .consume_attempt_for_transport_init_and_bind(owner_b),
        Some(2)
    );
    let _transport_b = events.try_recv().expect("B transport connected");
    assert!(handle.shared.emit_entity_input(
        owner_b,
        2,
        EntityProducerInput::Spawn {
            token: token(2, 1),
            snapshot: snapshot(2, 7, 10.0),
        },
    ));
    let spawned_b = events.try_recv().expect("B spawn");
    assert_eq!(spawned_b.connection_epoch, 2);
    match spawned_b.payload {
        BackendEventPayload::Entity(ContractProtocolEntityEvent::Spawned { entity }) => {
            assert_eq!(entity.entity_key, "2:7");
            assert_eq!(entity.position.x, 10.0);
        }
        payload => panic!("expected B spawned payload, got {payload:?}"),
    }

    release_publish.wait();
    assert!(!emitter.join().expect("A emitter"));
    assert!(
        events.try_recv().is_err(),
        "accepted A payload must not appear under B metadata"
    );

    assert!(handle.shared.emit_entity_input(
        owner_b,
        2,
        EntityProducerInput::Move {
            token: token(2, 2),
            patch: EntityMovePatch::relative(
                EntityIdentity::new(2, 7),
                Some([4096, 0, 0]),
                None,
                false,
            ),
        },
    ));
    assert!(handle.shared.emit_entity_input(
        owner_b,
        2,
        EntityProducerInput::Remove {
            token: token(2, 3),
            entity: EntityIdentity::new(2, 7),
        },
    ));
    let moved_b = events.try_recv().expect("B move");
    let removed_b = events.try_recv().expect("B remove");
    assert_eq!(moved_b.connection_epoch, 2);
    assert_eq!(removed_b.connection_epoch, 2);
    match removed_b.payload {
        BackendEventPayload::Entity(ContractProtocolEntityEvent::Removed {
            entity_key,
            last,
            ..
        }) => {
            assert_eq!(entity_key, "2:7");
            assert_eq!(last.entity_key, "2:7");
            assert_eq!(last.position.x, 11.0);
        }
        payload => panic!("expected B removed payload, got {payload:?}"),
    }
    assert!(events.try_recv().is_err());
}

#[test]
fn init_owner_bind_and_attempt_transition_share_one_epoch_transaction() {
    let shared = Arc::new(SharedRuntime::new(RunConfig::default()));
    let mut world = bevy_ecs::world::World::new();
    let owner_a = world.spawn_empty().id();
    let owner_b = world.spawn_empty().id();

    assert!(shared.begin_connection_attempt());
    let bind_epoch_read = Arc::new(std::sync::Barrier::new(2));
    let release_bind = Arc::new(std::sync::Barrier::new(2));
    shared.set_entity_owner_bind_hook(Some(Arc::new({
        let bind_epoch_read = bind_epoch_read.clone();
        let release_bind = release_bind.clone();
        move || {
            bind_epoch_read.wait();
            release_bind.wait();
        }
    })));

    let init_shared = shared.clone();
    let init = std::thread::spawn(move || {
        init_shared.consume_attempt_for_transport_init_and_bind(owner_a)
    });
    bind_epoch_read.wait();
    assert!(
        shared.command_admission.try_lock().is_none(),
        "Init must retain lifecycle admission through owner installation"
    );
    assert_eq!(shared.writer.lock().connection_epoch, 1);

    let (transition_started_tx, transition_started_rx) = std::sync::mpsc::channel();
    let (transition_done_tx, transition_done_rx) = std::sync::mpsc::channel();
    let transition_shared = shared.clone();
    let transition = std::thread::spawn(move || {
        transition_started_tx
            .send(())
            .expect("attempt transition start signal");
        let result = transition_shared.begin_connection_attempt();
        transition_done_tx
            .send(result)
            .expect("attempt transition result");
    });
    transition_started_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("attempt 2 must start competing");
    assert!(
        transition_done_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "attempt 2 must not advance while Init owns admission"
    );
    assert_eq!(shared.writer.lock().connection_epoch, 1);

    release_bind.wait();
    assert_eq!(init.join().expect("attempt 1 Init"), Some(1));
    assert!(transition_done_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("attempt 2 completes after Init transaction"));
    transition.join().expect("attempt 2 transition");

    assert_eq!(shared.writer.lock().connection_epoch, 2);
    assert_eq!(shared.entity_producer_epoch_for(owner_a), None);
    assert_eq!(
        shared.consume_attempt_for_transport_init_and_bind(owner_b),
        Some(2)
    );
    assert_eq!(shared.entity_producer_epoch_for(owner_b), Some(2));
}

#[test]
fn swarm_disconnect_deactivates_current_owner_but_late_a_disconnect_cannot_clear_b() {
    let shared = Arc::new(SharedRuntime::new(RunConfig::default()));
    let mut world = bevy_ecs::world::World::new();
    let owner_a = world.spawn_empty().id();
    let owner_b = world.spawn_empty().id();

    assert!(shared.begin_connection_attempt());
    assert_eq!(
        shared.consume_attempt_for_transport_init_and_bind(owner_a),
        Some(1)
    );
    assert!(shared.begin_connection_attempt());
    assert_eq!(
        shared.consume_attempt_for_transport_init_and_bind(owner_b),
        Some(2)
    );
    assert_eq!(shared.entity_producer_epoch_for(owner_b), Some(2));

    // The entity-bearing canonical source closes B first.  The later
    // entity-less SwarmEvent is only the reconnect fallback.
    assert!(shared
        .admit_canonical_disconnected(owner_b, 2, Some("B canonical disconnect".to_owned()))
        .is_some());
    assert!(shared.claim_reconnect());
    assert_eq!(shared.entity_producer_epoch_for(owner_b), None);
    assert!(!shared.deactivate_entity_producer_owner(owner_a));
    assert!(shared
        .apply_entity_input_for_owner(
            owner_b,
            2,
            EntityProducerInput::Spawn {
                token: token(2, 1),
                snapshot: snapshot(2, 7, 10.0),
            },
        )
        .is_none());
}

#[test]
fn late_entity_lifecycle_cannot_close_b_but_current_b_disconnect_closes_once() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    let mut world = bevy_ecs::world::World::new();
    let owner_a = world.spawn_empty().id();
    let owner_b = world.spawn_empty().id();

    assert!(handle.shared.begin_connection_attempt());
    let _request_a = events.try_recv().expect("attempt 1 request");
    assert_eq!(
        handle
            .shared
            .consume_attempt_for_transport_init_and_bind(owner_a),
        Some(1)
    );
    let _transport_a = events.try_recv().expect("A transport connected");
    assert!(handle.shared.begin_connection_attempt());
    let _request_b = events.try_recv().expect("attempt 2 request");
    assert_eq!(
        handle
            .shared
            .consume_attempt_for_transport_init_and_bind(owner_b),
        Some(2)
    );
    let _transport_b = events.try_recv().expect("B transport connected");

    {
        let mut observation = handle.shared.observation.write();
        observation.generation = 77;
        observation.source = Some(FactSource::ServerObserved);
    }
    handle
        .send_chat("must survive stale lifecycle")
        .expect("pending command admission");

    assert!(handle
        .shared
        .admit_canonical_disconnected(owner_a, 1, Some("late A disconnect".to_owned()))
        .is_none());
    assert!(handle
        .shared
        .admit_canonical_connection_failed(owner_a, 1, "late A failure".to_owned())
        .is_none());

    assert_eq!(handle.shared.entity_producer_epoch_for(owner_b), Some(2));
    assert!(!handle.shared.disconnect_reported.load(Ordering::Acquire));
    assert!(handle.shared.last_close.lock().is_none());
    assert!(handle.shared.last_failure.lock().is_none());
    assert_eq!(handle.shared.observation.read().generation, 77);
    assert_eq!(
        handle.shared.observation.read().source,
        Some(FactSource::ServerObserved)
    );
    assert_eq!(handle.shared.commands.lock().len(), 1);
    assert!(events.try_recv().is_err());
    assert!(matches!(
        handle.state(),
        BackendState::LoggingIn { epoch: 2, .. }
    ));

    assert!(handle
        .shared
        .admit_canonical_disconnected(owner_b, 2, Some("current B disconnect".to_owned()))
        .is_some());
    let closed = events.try_recv().expect("current B close");
    assert_eq!(closed.connection_epoch, 2);
    assert_eq!(
        serde_json::to_value(&closed.payload).expect("close payload")["type"],
        "connection_closed"
    );
    assert!(events.try_recv().is_err(), "close must be emitted once");
    assert_eq!(handle.shared.entity_producer_epoch_for(owner_b), None);
    assert!(handle.shared.commands.lock().is_empty());
}

#[test]
fn pre_init_connection_failed_claims_only_the_unbound_current_attempt() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    let mut world = bevy_ecs::world::World::new();
    let owner = world.spawn_empty().id();

    assert!(handle.shared.begin_connection_attempt());
    let _request = events.try_recv().expect("pre-Init attempt request");
    assert!(handle.shared.admit_canonical_join_started(1));
    assert!(handle
        .shared
        .admit_canonical_connection_failed(owner, 1, "failed before Init".to_owned())
        .is_some());
    let closed = events.try_recv().expect("pre-Init failure close");
    assert_eq!(closed.connection_epoch, 1);
    assert_eq!(
        serde_json::to_value(&closed.payload).expect("close payload")["type"],
        "connection_closed"
    );
    assert!(handle
        .shared
        .admit_canonical_connection_failed(owner, 1, "duplicate pre-Init failure".to_owned())
        .is_none());
    assert!(handle
        .shared
        .take_canonical_connection_failure_followup(owner));
    assert!(!handle
        .shared
        .take_canonical_connection_failure_followup(owner));
    assert!(handle.shared.entity_producer_epoch_for(owner).is_none());
    assert!(matches!(
        handle.shared.entity_producer.lock().attempt,
        AttemptAdmissionState::Closed { epoch: 1 }
    ));
    assert!(events.try_recv().is_err());
}

#[test]
fn same_entity_reconnect_return_binds_without_init_and_publishes_spawn_move_remove() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    let mut world = bevy_ecs::world::World::new();
    let entity = world.spawn_empty().id();

    assert!(handle.shared.begin_connection_attempt());
    let _request_a = events.try_recv().expect("A request");
    assert_eq!(
        handle
            .shared
            .consume_attempt_for_transport_init_and_bind(entity),
        Some(1)
    );
    let _transport_a = events.try_recv().expect("A transport");
    assert!(handle
        .shared
        .admit_canonical_disconnected(entity, 1, Some("A ended".to_owned()))
        .is_some());
    let _close_a = events.try_recv().expect("A close");

    assert!(handle.shared.claim_reconnect());
    let reconnect_token = handle
        .shared
        .admit_reconnect_attempt()
        .expect("B reconnect admission");
    let _request_b = events.try_recv().expect("B request");

    // B has no Init.  The returned client consumes the reserved B
    // identity directly, before finish_reconnect_attempt can close it.
    assert_eq!(
        handle.shared.bind_reconnect_return(reconnect_token, entity),
        Some(2)
    );
    let transport_b = events.try_recv().expect("B transport");
    assert_eq!(transport_b.connection_epoch, 2);
    handle.shared.finish_reconnect_attempt(reconnect_token);
    assert_eq!(handle.shared.entity_producer_epoch_for(entity), Some(2));
    let source = handle.observation_source();

    // A late Init is an idempotent no-op after the return bind.
    assert_eq!(
        handle
            .shared
            .consume_attempt_for_transport_init_and_bind(entity),
        Some(2)
    );
    assert!(events.try_recv().is_err());

    assert!(handle.shared.emit_entity_input(
        entity,
        2,
        EntityProducerInput::Spawn {
            token: token(2, 1),
            snapshot: snapshot(2, 7, 10.0),
        },
    ));
    let after_spawn = source.list_tracked_entities().expect("spawn observation");
    assert_eq!(after_spawn.len(), 1);
    assert_eq!(after_spawn[0].entity_key, "2:7");
    assert_eq!(after_spawn[0].entity_type, "minecraft:pig");
    assert_eq!(after_spawn[0].position.x, 10.0);
    assert_eq!(after_spawn[0].head_yaw, Some(90.0));
    assert!(handle.shared.emit_entity_input(
        entity,
        2,
        EntityProducerInput::Move {
            token: token(2, 2),
            patch: EntityMovePatch::relative(
                EntityIdentity::new(2, 7),
                Some([4096, 0, 0]),
                None,
                false,
            ),
        },
    ));
    let after_move = source.list_tracked_entities().expect("move observation");
    assert_eq!(after_move[0].entity_key, "2:7");
    assert_eq!(after_move[0].position.x, 11.0);
    assert_eq!(after_move[0].entity_type, "minecraft:pig");
    assert!(handle.shared.emit_entity_input(
        entity,
        2,
        EntityProducerInput::Remove {
            token: token(2, 3),
            entity: EntityIdentity::new(2, 7),
        },
    ));
    assert!(source
        .list_tracked_entities()
        .expect("remove observation")
        .is_empty());

    let spawned = events.try_recv().expect("B spawn");
    let moved = events.try_recv().expect("B move");
    let removed = events.try_recv().expect("B remove");
    assert_eq!(spawned.connection_epoch, 2);
    assert_eq!(moved.connection_epoch, 2);
    assert_eq!(removed.connection_epoch, 2);
    match (spawned.payload, moved.payload, removed.payload) {
        (
            BackendEventPayload::Entity(ContractProtocolEntityEvent::Spawned { entity: spawn }),
            BackendEventPayload::Entity(ContractProtocolEntityEvent::Moved { entity: move_ }),
            BackendEventPayload::Entity(ContractProtocolEntityEvent::Removed {
                entity_key: remove_key,
                last,
                ..
            }),
        ) => {
            assert_eq!(spawn.entity_key, "2:7");
            assert_eq!(move_.entity_key, "2:7");
            assert_eq!(remove_key, "2:7");
            assert_eq!(last.entity_key, "2:7");
            assert_eq!(last.position.x, 11.0);
        }
        payloads => panic!("expected Spawn/Move/Remove, got {payloads:?}"),
    }
}

#[test]
fn same_entity_late_a_source_epoch_is_inert_before_and_after_b_bind() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    let mut world = bevy_ecs::world::World::new();
    let entity = world.spawn_empty().id();

    assert!(handle.shared.begin_connection_attempt());
    let _request_a = events.try_recv().expect("A request");
    assert_eq!(
        handle
            .shared
            .consume_attempt_for_transport_init_and_bind(entity),
        Some(1)
    );
    let _transport_a = events.try_recv().expect("A transport");
    assert!(handle
        .shared
        .admit_canonical_disconnected(entity, 1, Some("A ended".to_owned()))
        .is_some());
    let _close_a = events.try_recv().expect("A close");

    assert!(handle.shared.claim_reconnect());
    let reconnect_token = handle
        .shared
        .admit_reconnect_attempt()
        .expect("B reconnect admission");
    let _request_b = events.try_recv().expect("B request");

    // B is reserved but not bound.  Explicit source epoch 1 is rejected;
    // Entity alone would incorrectly identify this as the same client.
    assert!(handle
        .shared
        .admit_canonical_disconnected(entity, 1, Some("late A".to_owned()))
        .is_none());
    assert!(handle
        .shared
        .admit_canonical_connection_failed(entity, 1, "late A failure".to_owned())
        .is_none());
    assert!(!handle
        .shared
        .admit_canonical_packet_source_for_epoch(entity, 1));

    assert_eq!(
        handle.shared.bind_reconnect_return(reconnect_token, entity),
        Some(2)
    );
    let _transport_b = events.try_recv().expect("B transport");
    handle.shared.finish_reconnect_attempt(reconnect_token);
    assert!(handle.shared.emit_entity_input(
        entity,
        2,
        EntityProducerInput::Spawn {
            token: token(2, 10),
            snapshot: snapshot(2, 7, 20.0),
        },
    ));
    let _spawn = events.try_recv().expect("B spawn");

    // The same stale-A evidence remains inert after B owns the entity.
    assert!(handle
        .shared
        .admit_canonical_disconnected(entity, 1, Some("late A 2".to_owned()))
        .is_none());
    assert!(handle
        .shared
        .admit_canonical_connection_failed(entity, 1, "late A failure 2".to_owned())
        .is_none());
    assert!(!handle
        .shared
        .admit_canonical_packet_source_for_epoch(entity, 1));
    assert!(!handle
        .shared
        .reset_entity_scope_for_owner_at_epoch(entity, 1));
    assert!(handle
        .shared
        .apply_entity_input_for_owner(
            entity,
            1,
            EntityProducerInput::Spawn {
                token: token(1, 11),
                snapshot: snapshot(1, 7, 999.0),
            },
        )
        .is_none());

    assert!(handle.shared.emit_entity_input(
        entity,
        2,
        EntityProducerInput::Move {
            token: token(2, 12),
            patch: EntityMovePatch::relative(
                EntityIdentity::new(2, 7),
                Some([4096, 0, 0]),
                None,
                true,
            ),
        },
    ));
    let moved = events.try_recv().expect("B move after stale A");
    match moved.payload {
        BackendEventPayload::Entity(ContractProtocolEntityEvent::Moved { entity }) => {
            assert_eq!(entity.entity_key, "2:7");
            assert_eq!(entity.position.x, 21.0);
        }
        payload => panic!("expected B moved payload, got {payload:?}"),
    }
    assert!(events.try_recv().is_err());
}

#[test]
fn same_entity_current_preinit_failure_and_bound_closes_preserve_reason_once() {
    let pre_init = RuntimeHandle::new(RunConfig::default());
    let mut pre_events = pre_init.subscribe();
    let mut world = bevy_ecs::world::World::new();
    let entity = world.spawn_empty().id();

    assert!(pre_init.shared.begin_connection_attempt());
    let _request = pre_events.try_recv().expect("pre-Init request");
    assert!(pre_init.shared.admit_canonical_join_started(1));
    assert!(pre_init
        .shared
        .admit_canonical_connection_failed(entity, 1, "pre-init exact error".to_owned())
        .is_some());
    let close = pre_events.try_recv().expect("pre-Init close");
    match close.payload {
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionClosed { close }) => {
            assert_eq!(close.code, "connection_failed");
            assert_eq!(
                close.error.expect("failure error").message,
                "pre-init exact error"
            );
        }
        payload => panic!("expected pre-Init close, got {payload:?}"),
    }
    assert!(pre_init
        .shared
        .admit_canonical_connection_failed(entity, 1, "duplicate".to_owned())
        .is_none());
    assert!(pre_init
        .shared
        .take_canonical_connection_failure_followup(entity));
    assert!(!pre_init
        .shared
        .take_canonical_connection_failure_followup(entity));
    assert!(pre_events.try_recv().is_err());

    let bound = RuntimeHandle::new(RunConfig::default());
    let mut bound_events = bound.subscribe();
    let mut bound_world = bevy_ecs::world::World::new();
    let bound_entity = bound_world.spawn_empty().id();
    assert!(bound.shared.begin_connection_attempt());
    let _request = bound_events.try_recv().expect("bound request");
    assert_eq!(
        bound
            .shared
            .consume_attempt_for_transport_init_and_bind(bound_entity),
        Some(1)
    );
    let _transport = bound_events.try_recv().expect("bound transport");
    assert!(bound
        .shared
        .admit_canonical_disconnected(bound_entity, 1, Some("current B exact reason".to_owned()),)
        .is_some());
    let close = bound_events.try_recv().expect("bound close");
    match close.payload {
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionClosed { close }) => {
            assert_eq!(close.code, "unclassified_kick");
            assert_eq!(close.end_reason.as_deref(), Some("current B exact reason"));
        }
        payload => panic!("expected bound close, got {payload:?}"),
    }
    assert!(bound
        .shared
        .admit_canonical_disconnected(bound_entity, 1, Some("duplicate reason".to_owned()),)
        .is_none());
    assert!(bound_events.try_recv().is_err());
}

#[test]
fn same_reserved_attempt_init_is_rejected_and_return_is_idempotent() {
    fn exercise(init_first: bool) {
        let handle = RuntimeHandle::new(RunConfig::default());
        let mut events = handle.subscribe();
        let mut world = bevy_ecs::world::World::new();
        let entity = world.spawn_empty().id();

        assert!(handle.shared.begin_connection_attempt());
        let _request_a = events.try_recv().expect("A request");
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(entity),
            Some(1)
        );
        let _transport_a = events.try_recv().expect("A transport");
        assert!(handle
            .shared
            .admit_canonical_disconnected(entity, 1, Some("A ended".to_owned()))
            .is_some());
        let _close_a = events.try_recv().expect("A close");
        assert!(handle.shared.claim_reconnect());
        let reconnect_token = handle
            .shared
            .admit_reconnect_attempt()
            .expect("B admission");
        let _request_b = events.try_recv().expect("B request");

        if init_first {
            assert!(!handle.shared.admit_canonical_join_started(2));
            assert_eq!(
                handle
                    .shared
                    .consume_attempt_for_transport_init_and_bind(entity),
                None
            );
            assert_eq!(
                handle.shared.bind_reconnect_return(reconnect_token, entity),
                Some(2)
            );
            let _transport_b = events.try_recv().expect("B transport");
        } else {
            assert_eq!(
                handle.shared.bind_reconnect_return(reconnect_token, entity),
                Some(2)
            );
            let _transport_b = events.try_recv().expect("B transport");
            assert_eq!(
                handle
                    .shared
                    .consume_attempt_for_transport_init_and_bind(entity),
                Some(2)
            );
        }
        assert_eq!(
            handle.shared.bind_reconnect_return(reconnect_token, entity),
            Some(2)
        );
        assert_eq!(
            handle
                .shared
                .consume_attempt_for_transport_init_and_bind(entity),
            Some(2)
        );
        handle.shared.finish_reconnect_attempt(reconnect_token);
        assert_eq!(handle.shared.entity_producer_epoch_for(entity), Some(2));
        assert!(events.try_recv().is_err());
    }

    exercise(true);
    exercise(false);
}
