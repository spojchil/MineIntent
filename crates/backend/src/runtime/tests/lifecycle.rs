use super::*;

#[test]
fn connection_request_preallocates_and_init_reuses_each_attempt_identity() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    let mut owner_world = bevy_ecs::world::World::new();
    let first_owner = owner_world.spawn_empty().id();
    let second_owner = owner_world.spawn_empty().id();
    assert!(events.try_recv().is_err());

    handle.shared.begin_connection_attempt();
    let first_request = events.try_recv().expect("首次连接请求事件");
    assert_eq!(first_request.connection_epoch, 1);
    assert_eq!(first_request.connection_attempt_id, "attempt-1");
    let first_payload = payload_json(&first_request);
    assert_eq!(first_payload["type"], "connection_requested");
    assert_eq!(first_payload["attempt"], 1);
    assert!(first_request.dimension.is_none());

    assert_eq!(
        handle
            .shared
            .consume_attempt_for_transport_init_and_bind(first_owner),
        Some(1)
    );
    let first_init = events.try_recv().expect("首次连接初始化事件");
    assert_eq!(
        first_init.process_session_id,
        first_request.process_session_id
    );
    assert_eq!(first_init.connection_epoch, 1);
    assert_eq!(first_init.connection_attempt_id, "attempt-1");

    handle.shared.begin_connection_attempt();
    let second_request = events.try_recv().expect("重连请求事件");
    assert_eq!(
        second_request.process_session_id,
        first_request.process_session_id
    );
    assert_eq!(second_request.connection_epoch, 2);
    assert_eq!(second_request.connection_attempt_id, "attempt-2");
    let second_payload = payload_json(&second_request);
    assert_eq!(second_payload["type"], "connection_requested");
    assert_eq!(second_payload["attempt"], 2);
    assert!(second_request.dimension.is_none());

    assert_eq!(
        handle
            .shared
            .consume_attempt_for_transport_init_and_bind(second_owner),
        Some(2)
    );
    let _second_transport = events.try_recv().expect("second transport connected");
    assert_eq!(handle.shared.context().1, 2);
    assert!(events.try_recv().is_err());
}

#[test]
fn event_dimension_is_captured_at_emit_time_and_cleared_for_a_new_attempt() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();

    handle.shared.begin_connection_attempt();
    let first_request = events.try_recv().expect("首次连接请求事件");
    assert!(first_request.dimension.is_none());

    assert_eq!(handle.shared.set_dimension("minecraft:overworld"), None);
    handle.shared.emit(
        FactSource::ServerObserved,
        BackendEventPayload::World(ContractProtocolWorldEvent::GameChanged {
            dimension: Some("minecraft:overworld".to_owned()),
            game_mode: Some("survival".to_owned()),
        }),
    );
    let world_event = events.try_recv().expect("世界事件");
    assert_eq!(
        world_event.dimension.as_deref(),
        Some("minecraft:overworld")
    );

    handle.shared.begin_connection_attempt();
    let second_request = events.try_recv().expect("重连请求事件");
    assert_eq!(second_request.connection_epoch, 2);
    assert!(second_request.dimension.is_none());
}

#[test]
fn dimension_changed_event_carries_the_new_dimension() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    handle.shared.begin_connection_attempt();
    let _request = events.try_recv().expect("连接请求事件");

    handle.shared.observe_dimension("minecraft:overworld");
    assert!(events.try_recv().is_err());
    handle.shared.observe_dimension("minecraft:the_nether");

    let changed = events.try_recv().expect("维度变化事件");
    let changed_payload = payload_json(&changed);
    assert_eq!(changed_payload["type"], "dimension_changed");
    assert_eq!(changed_payload["from"], "minecraft:overworld");
    assert_eq!(changed_payload["to"], "minecraft:the_nether");
    assert_eq!(changed.dimension.as_deref(), Some("minecraft:the_nether"));
}

#[test]
fn world_loaded_dimension_boundary_is_idempotent_for_the_bound_owner() {
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

    assert!(handle
        .shared
        .observe_dimension_from_world_boundary(owner, "minecraft:overworld"));
    assert!(events.try_recv().is_err());
    assert!(handle
        .shared
        .observe_dimension_from_world_boundary(owner, "minecraft:overworld"));
    assert!(events.try_recv().is_err());
    assert!(handle
        .shared
        .observe_dimension_from_world_boundary(owner, "minecraft:the_nether"));
    let changed = events.try_recv().expect("one dimension change");
    assert_eq!(payload_json(&changed)["type"], "dimension_changed");
    assert!(handle
        .shared
        .observe_dimension_from_world_boundary(owner, "minecraft:the_nether"));
    assert!(events.try_recv().is_err());
}

#[test]
fn runtime_ready_death_respawn_and_dimension_lifecycle_order_is_typed() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    handle.shared.begin_connection_attempt();
    let _request = events.try_recv().expect("connection request");

    handle.shared.emit_transport_connected();
    handle
        .shared
        .emit_logged_in("26.1.2", "minecraft:overworld".to_owned());
    handle.shared.emit_ready(7);
    handle.shared.emit_died();
    handle
        .shared
        .emit_respawn_transition_started("minecraft:overworld".to_owned());
    handle
        .shared
        .emit_respawned("minecraft:overworld".to_owned());
    handle.shared.observe_dimension("minecraft:the_nether");

    let emitted = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
    assert_eq!(
        emitted
            .iter()
            .map(|event| payload_json(event)["type"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        vec![
            "transport_connected".to_owned(),
            "logged_in".to_owned(),
            "ready".to_owned(),
            "died".to_owned(),
            "respawn_transition_started".to_owned(),
            "respawned".to_owned(),
            "dimension_changed".to_owned(),
        ]
    );
    assert_eq!(payload_json(&emitted[1])["version"], "26.1.2");
    assert_eq!(
        payload_json(&emitted[1])["dimension"],
        "minecraft:overworld"
    );
    assert_eq!(payload_json(&emitted[2])["snapshotRevision"], 7);
    assert_eq!(
        payload_json(&emitted[4])["fromDimension"],
        "minecraft:overworld"
    );
    assert_eq!(
        payload_json(&emitted[5])["dimension"],
        "minecraft:overworld"
    );
    assert_eq!(payload_json(&emitted[6])["from"], "minecraft:overworld");
    assert_eq!(payload_json(&emitted[6])["to"], "minecraft:the_nether");
    assert_eq!(emitted[2].dimension.as_deref(), Some("minecraft:overworld"));
    assert_eq!(
        emitted[6].dimension.as_deref(),
        Some("minecraft:the_nether")
    );
}

#[test]
fn lifecycle_state_timestamps_match_the_strict_v2_lifecycle_facts() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    handle.shared.begin_connection_attempt();
    let _request = events.try_recv().expect("connection request");

    handle.shared.emit_ready(7);
    let ready_event = events.try_recv().expect("ready event");
    let BackendState::Ready { ready_at, .. } = handle.state() else {
        panic!("ready state should be visible after ready admission");
    };
    assert_eq!(ready_at, ready_event.occurred_at);

    handle.shared.emit_died();
    let died_event = events.try_recv().expect("died event");
    let BackendState::Dead { died_at, .. } = handle.state() else {
        panic!("dead state should be visible after death admission");
    };
    assert_eq!(died_at, died_event.occurred_at);
}

#[test]
fn stopped_runtime_rejects_late_attempt_and_ready_without_resurrection() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    let mut owner_world = bevy_ecs::world::World::new();
    let owner = owner_world.spawn_empty().id();
    handle.stop("late_event_stop");
    let stopped = events.try_recv().expect("stopped event");
    assert_eq!(payload_json(&stopped)["type"], "stopped");
    assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));

    assert!(!handle.shared.begin_connection_attempt());
    assert!(handle
        .shared
        .consume_attempt_for_transport_init_and_bind(owner)
        .is_none());
    handle.shared.emit_transport_connected();
    handle
        .shared
        .emit_logged_in("26.1.2", "minecraft:overworld".to_owned());
    handle.shared.emit_ready(99);
    assert!(!handle.shared.ready.load(Ordering::Acquire));
    assert!(handle.shared.stopped_reported.load(Ordering::Acquire));
    assert!(events.try_recv().is_err());
}

#[test]
fn finalize_stop_ready_protocol_has_no_lost_wakeup_between_finalizers() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    handle.shared.stopping.store(true, Ordering::Release);
    *handle.shared.stop_reason.lock() = Some("lost_wakeup_regression".to_owned());

    let (hook_reached_tx, hook_reached_rx) = std_mpsc::channel();
    let (release_tx, release_rx) = std_mpsc::channel();
    let release_rx = Arc::new(StdMutex::new(Some(release_rx)));
    handle.shared.set_finalize_stop_hook(Some(Arc::new({
        let release_rx = release_rx.clone();
        move || {
            hook_reached_tx
                .send(())
                .expect("finalizer hook should be reached");
            release_rx
                .lock()
                .expect("release gate lock")
                .take()
                .expect("only the first finalizer owns the gate")
                .recv()
                .expect("release gate should open");
        }
    })));

    let first_shared = handle.shared.clone();
    let first = thread::spawn(move || first_shared.finalize_stop_if_ready());
    hook_reached_rx
        .recv_timeout(StdDuration::from_secs(1))
        .expect("first finalizer must reach the readiness gate");

    let (second_attempt_tx, second_attempt_rx) = std_mpsc::channel();
    let (second_done_tx, second_done_rx) = std_mpsc::channel();
    let second_shared = handle.shared.clone();
    let second = thread::spawn(move || {
        second_attempt_tx
            .send(())
            .expect("second finalizer attempt should be observable");
        second_shared.finalize_stop_if_ready();
        second_done_tx
            .send(())
            .expect("second finalizer should finish");
    });
    second_attempt_rx
        .recv_timeout(StdDuration::from_secs(1))
        .expect("second finalizer should attempt while first owns admission");
    assert!(
        second_done_rx.try_recv().is_err(),
        "second finalizer must wait instead of observing an empty reason"
    );

    release_tx.send(()).expect("release first finalizer");
    first.join().expect("first finalizer should not panic");
    second.join().expect("second finalizer should not panic");

    let stopped = events.try_recv().expect("stopped must not be lost");
    assert_eq!(payload_json(&stopped)["type"], "stopped");
    assert_eq!(payload_json(&stopped)["reason"], "lost_wakeup_regression");
    assert!(events.try_recv().is_err());
    assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
}

#[test]
fn running_event_admission_enqueue_precedes_stop_terminal_event() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    handle.shared.begin_connection_attempt();
    let _request = events.try_recv().expect("connection request");

    let (event_checked_tx, event_checked_rx) = std_mpsc::channel();
    let (release_tx, release_rx) = std_mpsc::channel();
    let release_rx = Arc::new(StdMutex::new(Some(release_rx)));
    handle.shared.set_event_admission_hook(Some(Arc::new({
        let release_rx = release_rx.clone();
        move || {
            event_checked_tx
                .send(())
                .expect("event admission hook should be reached");
            release_rx
                .lock()
                .expect("event release gate lock")
                .take()
                .expect("only one event admission owns the gate")
                .recv()
                .expect("event release gate should open");
        }
    })));

    let event_shared = handle.shared.clone();
    let emitter = thread::spawn(move || {
        event_shared.emit_if_running(
            FactSource::ServerObserved,
            BackendEventPayload::World(ContractProtocolWorldEvent::GameChanged {
                dimension: Some("minecraft:overworld".to_owned()),
                game_mode: Some("survival".to_owned()),
            }),
        )
    });
    event_checked_rx
        .recv_timeout(StdDuration::from_secs(1))
        .expect("event must hold admission between check and enqueue");

    let (stop_attempt_tx, stop_attempt_rx) = std_mpsc::channel();
    let stop_handle = handle.clone();
    let stopper = thread::spawn(move || {
        stop_attempt_tx
            .send(())
            .expect("stop attempt should be observable");
        stop_handle.stop("event_admission_stop");
    });
    stop_attempt_rx
        .recv_timeout(StdDuration::from_secs(1))
        .expect("stop must attempt while event owns admission");

    release_tx.send(()).expect("release event admission");
    assert!(emitter.join().expect("event emitter should not panic"));
    stopper.join().expect("stopper should not panic");

    let kinds = std::iter::from_fn(|| events.try_recv().ok())
        .map(|event| payload_json(&event)["type"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![
            "game_changed".to_owned(),
            "connection_closed".to_owned(),
            "stopped".to_owned()
        ]
    );
}

#[test]
fn stop_cannot_overtake_first_disconnect_cleanup() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    handle.shared.begin_connection_attempt();
    let _request = events.try_recv().expect("connection request");
    handle.shared.observation.write().world = Some(empty_world());
    handle
        .send_chat("queued before disconnect")
        .expect("pre-disconnect command should queue");

    let (cleanup_reached_tx, cleanup_reached_rx) = std_mpsc::channel();
    let (release_cleanup_tx, release_cleanup_rx) = std_mpsc::channel();
    let release_cleanup_rx = Arc::new(StdMutex::new(Some(release_cleanup_rx)));
    handle.shared.set_disconnect_cleanup_hook(Some(Arc::new({
        let release_cleanup_rx = release_cleanup_rx.clone();
        move || {
            cleanup_reached_tx
                .send(())
                .expect("disconnect cleanup hook should be reached");
            release_cleanup_rx
                .lock()
                .expect("disconnect cleanup gate lock")
                .take()
                .expect("only the first disconnect owns the gate")
                .recv()
                .expect("disconnect cleanup gate should open");
        }
    })));

    let disconnect_shared = handle.shared.clone();
    let disconnect = thread::spawn(move || {
        disconnect_shared.mark_disconnected(Some("Server closed".to_owned()))
    });
    cleanup_reached_rx
        .recv_timeout(StdDuration::from_secs(1))
        .expect("first disconnect must reach cleanup while holding admission");

    let (stop_attempt_tx, stop_attempt_rx) = std_mpsc::channel();
    let (stop_done_tx, stop_done_rx) = std_mpsc::channel();
    let stop_handle = handle.clone();
    let stopper = thread::spawn(move || {
        stop_attempt_tx
            .send(())
            .expect("stop attempt should be observable");
        stop_handle.stop("disconnect_cleanup_race");
        stop_done_tx.send(()).expect("stop completion signal");
    });
    stop_attempt_rx
        .recv_timeout(StdDuration::from_secs(1))
        .expect("stop must attempt while disconnect owns admission");
    assert!(
        stop_done_rx
            .recv_timeout(StdDuration::from_millis(50))
            .is_err(),
        "stopped must not overtake first-disconnect cleanup"
    );
    assert!(
        events.try_recv().is_err(),
        "close is not visible before cleanup"
    );

    release_cleanup_tx
        .send(())
        .expect("release disconnect cleanup");
    let close = disconnect
        .join()
        .expect("disconnect thread should not panic");
    assert_eq!(close.code, "server_shutdown");
    stopper.join().expect("stop thread should not panic");
    stop_done_rx
        .recv_timeout(StdDuration::from_secs(1))
        .expect("stop should finish after cleanup");

    let closed = events.try_recv().expect("close event after cleanup");
    let stopped = events.try_recv().expect("stopped event after cleanup");
    assert_eq!(payload_json(&closed)["type"], "connection_closed");
    assert_eq!(payload_json(&stopped)["type"], "stopped");
    assert!(handle.shared.observation.read().world.is_none());
    assert!(handle.shared.commands.lock().is_empty());
    assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
    assert!(events.try_recv().is_err());
}

#[test]
fn died_broadcast_reentrant_stop_has_no_post_stopped_actuator() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    handle.shared.begin_connection_attempt();
    let _request = events.try_recv().expect("connection request");

    let callback_handle = handle.clone();
    handle
        .shared
        .set_event_broadcast_hook(Some(Arc::new(move || {
            callback_handle.stop("died_callback_stop");
        })));
    let actuator_called = Arc::new(AtomicBool::new(false));
    let actuator_after_stop = Arc::new(AtomicBool::new(false));
    let called = actuator_called.clone();
    let after_stop = actuator_after_stop.clone();
    let result = handle.shared.admit_death_and_release(|| {
        if handle.shared.stopping.load(Ordering::Acquire) {
            after_stop.store(true, Ordering::Release);
        }
        called.store(true, Ordering::Release);
        true
    });

    assert_eq!(result, Some(true));
    assert!(actuator_called.load(Ordering::Acquire));
    assert!(
        !actuator_after_stop.load(Ordering::Acquire),
        "Death actuator must finish before a re-entrant died callback can stop"
    );
    let kinds = std::iter::from_fn(|| events.try_recv().ok())
        .map(|event| payload_json(&event)["type"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![
            "died".to_owned(),
            "connection_closed".to_owned(),
            "stopped".to_owned()
        ]
    );
}

#[test]
fn stop_wins_before_late_death_claim_without_actuator_or_event() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    handle.stop("late_death_stop");
    let _stopped = events.try_recv().expect("stop event");
    let actuator_called = Arc::new(AtomicBool::new(false));
    let called = actuator_called.clone();
    assert_eq!(
        handle.shared.admit_death_and_release(|| {
            called.store(true, Ordering::Release);
            true
        }),
        None
    );
    assert!(!actuator_called.load(Ordering::Acquire));
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn stop_cancels_admitted_reconnect_add_intent_and_preserves_terminal_state() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    handle
        .shared
        .reconnect_pending
        .store(true, Ordering::Release);
    let token = handle
        .shared
        .admit_reconnect_attempt()
        .expect("reconnect should be admitted before stop");
    let request = events.try_recv().expect("reconnect connection request");
    assert_eq!(payload_json(&request)["type"], "connection_requested");
    assert!(handle.shared.reconnect_add_is_allowed(token));

    handle.stop("cancel_reconnect_add");
    assert!(!handle.shared.reconnect_add_is_allowed(token));
    assert!(!handle.shared.reconnect_add_pending.load(Ordering::Acquire));
    assert!(!handle.shared.reconnect_pending.load(Ordering::Acquire));
    assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
    assert!(handle.shared.stopped_reported.load(Ordering::Acquire));
    tokio::time::timeout(
        StdDuration::from_secs(1),
        handle.shared.reconnect_cancel.notified(),
    )
    .await
    .expect("stop must wake a pending reconnect add task");

    let event_count_after_stop = std::iter::from_fn(|| events.try_recv().ok()).count();
    assert_eq!(
        event_count_after_stop, 2,
        "close then stopped after admitted attempt"
    );
    assert!(!handle.shared.begin_connection_attempt());
    assert!(events.try_recv().is_err());
}

#[test]
fn stop_wins_before_reconnect_attempt_admission() {
    let handle = RuntimeHandle::new(RunConfig::default());
    handle.stop("stop_before_reconnect");
    handle
        .shared
        .reconnect_pending
        .store(true, Ordering::Release);
    assert!(handle.shared.admit_reconnect_attempt().is_none());
    assert!(!handle.shared.reconnect_add_pending.load(Ordering::Acquire));
    assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
}

#[test]
fn runtime_retryable_close_then_reconnect_reuses_the_sealed_close_code() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    handle.shared.begin_connection_attempt();
    let _request = events.try_recv().expect("connection request");

    let close = handle
        .shared
        .mark_disconnected(Some("Server restarting".to_owned()));
    assert_eq!(close.code, "server_shutdown");
    assert!(close.retryable);
    assert!(!close.deliberate);
    handle.shared.emit_reconnect_scheduled(&close);

    let closed = events.try_recv().expect("connection closed event");
    let scheduled = events.try_recv().expect("reconnect scheduled event");
    assert_eq!(payload_json(&closed)["type"], "connection_closed");
    assert_eq!(payload_json(&closed)["close"]["code"], close.code);
    assert_eq!(payload_json(&scheduled)["type"], "reconnect_scheduled");
    assert_eq!(payload_json(&scheduled)["closeCode"], close.code);
    assert!(events.try_recv().is_err());
}

#[test]
fn runtime_fatal_close_emits_faulted_without_reconnect_disabled_rewrite() {
    let mut config = RunConfig::default();
    config.reconnect.enabled = false;
    let handle = RuntimeHandle::new(config);
    let mut events = handle.subscribe();
    handle.shared.begin_connection_attempt();
    let _request = events.try_recv().expect("connection request");

    let close = handle
        .shared
        .mark_disconnected(Some("You are banned from this server".to_owned()));
    let failure = handle.shared.failure_for_close(&close);
    assert_eq!(failure.code, BackendFailureCode::PermissionDenied);
    assert!(!failure.retryable);
    handle.shared.emit_faulted(failure);

    let closed = events.try_recv().expect("fatal close event");
    let faulted = events.try_recv().expect("faulted event");
    assert_eq!(payload_json(&closed)["type"], "connection_closed");
    assert_eq!(payload_json(&closed)["close"]["code"], "permission_denied");
    assert_eq!(payload_json(&faulted)["type"], "faulted");
    assert_eq!(
        payload_json(&faulted)["failure"]["code"],
        "permission_denied"
    );
    assert!(events.try_recv().is_err());
}

#[test]
fn runtime_invalid_session_close_uses_permission_denied_with_authentication_failure() {
    let mut config = RunConfig::default();
    config.reconnect.enabled = false;
    let handle = RuntimeHandle::new(config);
    let mut events = handle.subscribe();
    handle.shared.begin_connection_attempt();
    let _request = events.try_recv().expect("connection request");

    let close = handle
        .shared
        .mark_disconnected(Some("Invalid session".to_owned()));
    assert_eq!(close.code, "permission_denied");
    assert!(!close.retryable);
    assert!(close.kick.is_some());
    let failure = handle.shared.failure_for_close(&close);
    assert_eq!(failure.code, BackendFailureCode::AuthenticationFailed);
    assert!(!failure.retryable);
    handle.shared.emit_faulted(failure);

    let closed = events.try_recv().expect("invalid-session close event");
    let faulted = events.try_recv().expect("invalid-session fault event");
    assert_eq!(payload_json(&closed)["close"]["code"], "permission_denied");
    assert_eq!(
        payload_json(&faulted)["failure"]["code"],
        "authentication_failed"
    );
    assert!(events.try_recv().is_err());
}

#[test]
fn runtime_disconnect_kick_text_does_not_infer_forced_timeout_or_version_codes() {
    let cases = [
        ("server_shutdown", "server_shutdown", true, None),
        (
            "Unsupported version",
            "unclassified_kick",
            false,
            Some(BackendFailureCode::PermissionDenied),
        ),
        (
            "Timed out",
            "unclassified_kick",
            false,
            Some(BackendFailureCode::PermissionDenied),
        ),
    ];
    for (reason, expected_code, retryable, failure_code) in cases {
        let handle = RuntimeHandle::new(RunConfig::default());
        handle.shared.begin_connection_attempt();
        let close = handle.shared.mark_disconnected(Some(reason.to_owned()));
        assert_eq!(close.code, expected_code, "reason={reason}");
        assert_eq!(close.retryable, retryable, "reason={reason}");
        assert_eq!(
            handle.shared.failure_for_close(&close).code,
            failure_code.unwrap_or(BackendFailureCode::ProtocolError),
            "reason={reason}"
        );
    }
}

#[test]
fn runtime_unclassified_disconnect_component_is_fatal_not_reconnectable() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    handle.shared.begin_connection_attempt();
    let _request = events.try_recv().expect("connection request");

    let close = handle
        .shared
        .mark_disconnected(Some("Removed by an administrator".to_owned()));
    assert_eq!(close.code, "unclassified_kick");
    assert!(!close.retryable);
    assert!(close.kick.is_some());
    let failure = handle.shared.failure_for_close(&close);
    assert_eq!(failure.code, BackendFailureCode::PermissionDenied);
    assert!(!failure.retryable);
    assert!(!handle.shared.config.reconnect.enabled || !close.retryable);
    handle.shared.emit_faulted(failure);

    let _closed = events.try_recv().expect("unclassified kick close event");
    let faulted = events.try_recv().expect("unclassified kick fault event");
    assert_eq!(payload_json(&faulted)["type"], "faulted");
    assert!(
        events.try_recv().is_err(),
        "fatal kick must not schedule reconnect"
    );
}

#[test]
fn runtime_connection_failed_retains_error_and_disabled_retry_is_distinct() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    handle.shared.begin_connection_attempt();
    let _request = events.try_recv().expect("connection request");
    let close = handle
        .shared
        .mark_connection_failed("tcp reset by peer".to_owned());
    let closed = events.try_recv().expect("connection failed close event");
    assert_eq!(payload_json(&closed)["close"]["code"], "connection_failed");
    assert_eq!(
        payload_json(&closed)["close"]["error"]["message"],
        "tcp reset by peer"
    );
    assert_eq!(
        handle.shared.failure_for_close(&close).code,
        BackendFailureCode::ProtocolError
    );
}

#[test]
fn runtime_retryable_close_with_disabled_reconnect_emits_reconnect_disabled() {
    let mut config = RunConfig::default();
    config.reconnect.enabled = false;
    let handle = RuntimeHandle::new(config);
    let mut events = handle.subscribe();
    handle.shared.begin_connection_attempt();
    let _request = events.try_recv().expect("connection request");
    let close = handle
        .shared
        .mark_disconnected(Some("Server closed".to_owned()));
    assert!(close.retryable);
    assert_eq!(
        handle.shared.failure_for_close(&close).code,
        BackendFailureCode::ReconnectDisabled
    );
}

#[test]
fn runtime_expected_stop_closes_then_stops_after_local_cleanup_with_reason() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    handle.shared.begin_connection_attempt();
    let _request = events.try_recv().expect("connection request");
    handle.shared.set_dimension("minecraft:overworld");
    handle.shared.observation.write().world = Some(empty_world());

    handle.stop("operator_requested");
    let closed = events.try_recv().expect("expected close event");
    let stopped = events.try_recv().expect("stopped event");
    assert_eq!(payload_json(&closed)["type"], "connection_closed");
    assert_eq!(payload_json(&closed)["close"]["code"], "deliberate_stop");
    assert_eq!(payload_json(&closed)["close"]["deliberate"], true);
    assert_eq!(payload_json(&stopped)["type"], "stopped");
    assert_eq!(payload_json(&stopped)["reason"], "operator_requested");
    assert!(handle.shared.observation.read().world.is_none());
    assert!(handle.shared.pop_command().is_none());
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn runtime_active_stop_defers_stopped_until_release_seam_finishes() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    handle.shared.begin_connection_attempt();
    let _request = events.try_recv().expect("connection request");

    let (completion, state) = CommandCompletion::channel("command-stop".to_owned());
    state.begin_active_release(Arc::new(Notify::new()));
    *handle.shared.active_movement_completion.lock() = Some(state.clone());
    handle.shared.active_movement.store(true, Ordering::Release);
    *handle.shared.active_movement_id.lock() = Some("command-stop".to_owned());

    handle.stop("operator_requested");
    let closed = events.try_recv().expect("close must precede deferred stop");
    assert_eq!(payload_json(&closed)["type"], "connection_closed");
    assert!(events.try_recv().is_err(), "stopped must wait for release");
    assert!(!handle.shared.shutdown_requested.load(Ordering::Acquire));
    assert!(state.sender.lock().is_some());

    let completion_for_release = Some(state.clone());
    release_active_movement_and_finish(
        &handle.shared,
        "command-stop",
        0,
        &completion_for_release,
        || {
            assert!(handle.shared.active_movement.load(Ordering::Acquire));
            assert!(state.sender.lock().is_some());
            true
        },
        "stop move",
        Err(BackendError::Cancelled {
            operation: "command:command-stop".to_owned(),
        }),
    );
    state.wait_settled().await;
    assert!(matches!(
        completion.wait().await,
        Err(BackendError::Cancelled { .. })
    ));
    assert!(!handle.shared.active_movement.load(Ordering::Acquire));
    assert!(handle.shared.active_movement_id.lock().is_none());
    assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
    let stopped = events.try_recv().expect("stopped after release");
    assert_eq!(payload_json(&stopped)["type"], "stopped");
    assert_eq!(payload_json(&stopped)["reason"], "operator_requested");

    handle.stop("duplicate_reason");
    release_active_movement_and_finish(
        &handle.shared,
        "command-stop",
        0,
        &completion_for_release,
        || panic!("a settled active move must not release twice"),
        "stop move",
        Err(BackendError::Cancelled {
            operation: "command:command-stop".to_owned(),
        }),
    );
    assert!(events.try_recv().is_err());
}

#[test]
fn command_completion_settled_signal_waits_for_release_and_result_publication() {
    for _ in 0..64 {
        let (completion, state) = CommandCompletion::channel("settlement-race".to_owned());
        state.begin_active_release(Arc::new(Notify::new()));
        let barrier = Arc::new(Barrier::new(2));
        let waiter_state = state.clone();
        let observed_state = waiter_state.clone();
        let waiter_barrier = barrier.clone();
        let waiter = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("settlement waiter runtime");
            runtime.block_on(async move {
                waiter_barrier.wait();
                CommandCompletionCancellation(waiter_state.clone())
                    .wait_settled()
                    .await;
                // settled 是收尾事务里最后一次发布。观察到它的这个线程，
                // 必须已经能看到结果——零超时只给一次轮询机会，所以这里
                // 断言的是「已经在那儿」，不是「等一会儿会到」。
                assert_eq!(
                    tokio::time::timeout(Duration::ZERO, completion.wait()).await,
                    Ok(Ok(()))
                );
            });
            assert!(!observed_state.active_release.load(Ordering::Acquire));
        });
        barrier.wait();
        state.finish(Ok(()));
        waiter
            .join()
            .expect("settlement waiter should not race early");
    }
}

#[tokio::test]
async fn runtime_active_move_without_completion_stop_wakes_release_before_shutdown() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    handle.shared.begin_connection_attempt();
    let _request = events.try_recv().expect("connection request");

    let signal = Arc::new(Notify::new());
    *handle.shared.active_movement_cancel_signal.lock() = Some(signal.clone());
    handle.shared.active_movement.store(true, Ordering::Release);
    *handle.shared.active_movement_id.lock() = Some("command-no-completion".to_owned());
    let task_shared = handle.shared.clone();
    let released = Arc::new(AtomicBool::new(false));
    let released_by_task = released.clone();
    let release_task = tokio::spawn(async move {
        signal.notified().await;
        let no_completion = None;
        release_active_movement_and_finish(
            &task_shared,
            "command-no-completion",
            0,
            &no_completion,
            || {
                assert!(task_shared.active_movement.load(Ordering::Acquire));
                released_by_task.store(true, Ordering::Release);
                true
            },
            "stop move",
            Err(BackendError::Cancelled {
                operation: "command:command-no-completion".to_owned(),
            }),
        );
    });

    handle.stop("operator_requested");
    let closed = events.try_recv().expect("close must be emitted");
    assert_eq!(payload_json(&closed)["type"], "connection_closed");
    assert!(
        events.try_recv().is_err(),
        "stopped waits for no-completion release"
    );
    assert!(!handle.shared.shutdown_requested.load(Ordering::Acquire));
    assert!(handle.shared.active_movement.load(Ordering::Acquire));

    tokio::time::timeout(StdDuration::from_secs(1), release_task)
        .await
        .expect("no-completion move cancellation must wake promptly")
        .expect("release task should not panic");
    assert!(released.load(Ordering::Acquire));
    assert!(!handle.shared.active_movement.load(Ordering::Acquire));
    assert!(handle.shared.active_movement_id.lock().is_none());
    assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
    let stopped = events
        .try_recv()
        .expect("stopped after no-completion release");
    assert_eq!(payload_json(&stopped)["reason"], "operator_requested");
}

#[tokio::test]
async fn runtime_active_move_without_completion_disconnect_wakes_release() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    handle.shared.begin_connection_attempt();
    let _request = events.try_recv().expect("connection request");
    let signal = Arc::new(Notify::new());
    *handle.shared.active_movement_cancel_signal.lock() = Some(signal.clone());
    handle.shared.active_movement.store(true, Ordering::Release);
    *handle.shared.active_movement_id.lock() = Some("command-disconnect".to_owned());
    let task_shared = handle.shared.clone();
    let release_task = tokio::spawn(async move {
        signal.notified().await;
        let no_completion = None;
        release_active_movement_and_finish(
            &task_shared,
            "command-disconnect",
            0,
            &no_completion,
            || true,
            "disconnect move",
            Err(BackendError::Cancelled {
                operation: "command:command-disconnect".to_owned(),
            }),
        );
    });

    let close = handle
        .shared
        .mark_disconnected(Some("Server closed".to_owned()));
    assert_eq!(close.code, "server_shutdown");
    assert_eq!(
        payload_json(&events.try_recv().expect("close event"))["type"],
        "connection_closed"
    );
    tokio::time::timeout(StdDuration::from_secs(1), release_task)
        .await
        .expect("disconnect must wake no-completion move")
        .expect("release task should not panic");
    assert!(!handle.shared.active_movement.load(Ordering::Acquire));
    assert!(handle.shared.active_movement_id.lock().is_none());
    assert!(events.try_recv().is_err());
}
