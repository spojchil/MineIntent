use super::*;

#[test]
fn transport_retry_schedule_matches_ts_oracle_boundaries() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-08-03T00:00:00Z")
        .expect("fixed clock")
        .with_timezone(&chrono::Utc);
    let policy = ReconnectPolicy {
        enabled: true,
        initial_delay_ms: 100,
        multiplier: 2.0,
        max_delay_ms: 1_000,
        jitter_ratio: 0.0,
        stable_reset_ms: 60_000,
    };
    assert_eq!(
        reconnect_schedule_at(&policy, 1, 0.5, now).delay,
        Duration::from_millis(100)
    );
    assert_eq!(
        reconnect_schedule_at(&policy, 2, 0.5, now).delay,
        Duration::from_millis(200)
    );
    assert_eq!(
        reconnect_schedule_at(&policy, 4, 0.5, now).delay,
        Duration::from_millis(800)
    );
    let clamped = reconnect_schedule_at(&policy, 5, 0.5, now);
    assert_eq!(clamped.delay, Duration::from_millis(1_000));
    assert_eq!(
        clamped
            .retry_at
            .signed_duration_since(now)
            .num_milliseconds(),
        1_000
    );

    let initial_larger_than_max = ReconnectPolicy {
        initial_delay_ms: 5_000,
        max_delay_ms: 1_000,
        ..policy.clone()
    };
    assert_eq!(
        reconnect_schedule_at(&initial_larger_than_max, 1, 0.5, now).delay,
        Duration::from_millis(1_000)
    );

    let jitter = ReconnectPolicy {
        initial_delay_ms: 100,
        max_delay_ms: 1_000,
        jitter_ratio: 0.2,
        ..policy.clone()
    };
    assert_eq!(
        reconnect_schedule_at(&jitter, 1, 0.0, now).delay,
        Duration::from_millis(80)
    );
    assert_eq!(
        reconnect_schedule_at(&jitter, 1, 0.5, now).delay,
        Duration::from_millis(100)
    );
    assert_eq!(
        reconnect_schedule_at(&jitter, 1, 0.999_999, now).delay,
        Duration::from_millis(120)
    );

    let full_jitter = ReconnectPolicy {
        initial_delay_ms: 1,
        max_delay_ms: 10,
        jitter_ratio: 1.0,
        ..policy.clone()
    };
    assert_eq!(
        reconnect_schedule_at(&full_jitter, 1, 0.0, now).delay,
        Duration::ZERO
    );
    assert_eq!(
        reconnect_schedule_at(&full_jitter, 1, 0.5, now).delay,
        Duration::from_millis(1)
    );
    assert_eq!(
        reconnect_schedule_at(&full_jitter, 1, 0.75, now).delay,
        Duration::from_millis(2)
    );

    let huge = ReconnectPolicy {
        initial_delay_ms: u64::MAX,
        max_delay_ms: u64::MAX,
        multiplier: f64::MAX,
        jitter_ratio: 1.0,
        ..policy
    };
    assert_eq!(
        reconnect_schedule_at(&huge, u64::MAX, 1.0, now).delay,
        Duration::from_millis(u64::MAX)
    );
    assert_eq!(
        reconnect_schedule_at(&huge, u64::MAX, 1.0, now).retry_at,
        chrono::DateTime::<chrono::Utc>::MAX_UTC
    );
}

#[test]
fn transport_retry_ordinal_matches_scheduled_and_next_request_while_epoch_stays_monotonic() {
    let mut config = RunConfig::default();
    config.reconnect.jitter_ratio = 0.0;
    let handle = RuntimeHandle::new(config);
    let mut events = handle.subscribe();
    assert!(handle.shared.begin_connection_attempt());
    let first_request = events.try_recv().expect("first request");
    assert_eq!(first_request.connection_epoch, 1);
    assert_eq!(payload_json(&first_request)["attempt"], 1);

    let close = handle.shared.mark_disconnected(None);
    let _closed = events.try_recv().expect("first close");
    assert_eq!(
        handle.shared.emit_reconnect_scheduled(&close),
        Some(Duration::from_millis(1_000))
    );
    let scheduled = events.try_recv().expect("reconnect schedule");
    assert_eq!(scheduled.connection_epoch, 1);
    assert_eq!(payload_json(&scheduled)["attempt"], 2);

    assert!(handle.shared.begin_connection_attempt());
    let second_request = events.try_recv().expect("second request");
    assert_eq!(second_request.connection_epoch, 2);
    assert_eq!(payload_json(&second_request)["attempt"], 2);
    assert_eq!(handle.shared.retry_ordinal.load(Ordering::Acquire), 2);
}

#[test]
fn stable_reset_requires_exact_ready_epoch_attempt_and_generation() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut world = bevy_ecs::world::World::new();
    let owner = world.spawn_empty().id();
    let owner_b = world.spawn_empty().id();
    let mut events = handle.subscribe();

    assert!(handle.shared.begin_connection_attempt());
    let _request_a = events.try_recv().expect("request A");
    assert_eq!(
        handle
            .shared
            .consume_attempt_for_transport_init_and_bind(owner),
        Some(1)
    );
    let _transport_a = events.try_recv().expect("transport A");
    handle
        .shared
        .emit_logged_in("26.1.2", "minecraft:overworld".to_owned());
    let _logged_in_a = events.try_recv().expect("login A");
    handle.shared.emit_ready(1);
    let _ready_a = events.try_recv().expect("ready A");
    let stable_a = handle.shared.test_current_stable_token();

    let _close_a = handle.shared.mark_disconnected(None);
    let _closed_a = events.try_recv().expect("close A");
    assert!(handle.shared.begin_connection_attempt());
    let _request_b = events.try_recv().expect("request B");
    assert_eq!(handle.shared.retry_ordinal.load(Ordering::Acquire), 2);
    handle.shared.fire_stable_reset(stable_a);
    assert_eq!(handle.shared.retry_ordinal.load(Ordering::Acquire), 2);

    assert!(handle
        .shared
        .consume_attempt_for_transport_init_and_bind(owner_b)
        .is_some());
    let _transport_b = events.try_recv().expect("transport B");
    handle
        .shared
        .emit_logged_in("26.1.2", "minecraft:overworld".to_owned());
    let _logged_in_b = events.try_recv().expect("login B");
    handle.shared.emit_ready(2);
    let _ready_b = events.try_recv().expect("ready B");
    let stable_b = handle.shared.test_current_stable_token();
    handle.shared.fire_stable_reset(stable_b);
    assert_eq!(handle.shared.retry_ordinal.load(Ordering::Acquire), 0);

    handle
        .shared
        .emit_logged_in("26.1.2", "minecraft:overworld".to_owned());
    let _logged_in_again = events.try_recv().expect("non-ready transition");
    handle.shared.fire_stable_reset(stable_b);
    assert_eq!(handle.shared.retry_ordinal.load(Ordering::Acquire), 0);

    let stable_non_ready = handle.shared.test_current_stable_token();
    handle.stop("stable-stop");
    handle.shared.fire_stable_reset(stable_non_ready);
    assert_eq!(handle.shared.retry_ordinal.load(Ordering::Acquire), 0);
}

#[test]
fn phase_deadlines_have_distinct_codes_unique_close_and_disabled_fault_priority() {
    for (phase, expected_code) in [
        (TransportPhase::Connecting, "connection_timeout"),
        (TransportPhase::LoggingIn, "login_timeout"),
        (TransportPhase::Spawning, "spawn_timeout"),
    ] {
        let mut config = RunConfig::default();
        config.reconnect.enabled = false;
        let handle = RuntimeHandle::new(config);
        let mut events = handle.subscribe();
        let mut world = bevy_ecs::world::World::new();
        let owner = world.spawn_empty().id();
        assert!(handle.shared.begin_connection_attempt());
        let _request = events.try_recv().expect("request");
        if phase != TransportPhase::Connecting {
            assert!(handle
                .shared
                .consume_attempt_for_transport_init_and_bind(owner)
                .is_some());
            let _transport = events.try_recv().expect("transport");
        }
        if phase == TransportPhase::Spawning {
            handle
                .shared
                .emit_logged_in("26.1.2", "minecraft:overworld".to_owned());
            let _login = events.try_recv().expect("login");
        }
        let token = handle.shared.test_current_phase_token(phase);
        handle.shared.fire_phase_deadline(token);
        let closed = events.try_recv().expect("timeout close");
        assert_eq!(payload_json(&closed)["close"]["code"], expected_code);
        let faulted = events.try_recv().expect("disabled fault");
        assert_eq!(payload_json(&faulted)["type"], "faulted");
        assert_eq!(
            payload_json(&faulted)["failure"]["code"],
            "reconnect_disabled"
        );
        handle.shared.fire_phase_deadline(token);
        assert!(events.try_recv().is_err(), "late timeout must be inert");
    }
}

#[test]
fn late_phase_timeout_and_disconnect_stop_races_are_admission_gated() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    let mut world = bevy_ecs::world::World::new();
    let owner_a = world.spawn_empty().id();
    let owner_b = world.spawn_empty().id();

    assert!(handle.shared.begin_connection_attempt());
    let _request_a = events.try_recv().expect("request A");
    assert!(handle
        .shared
        .consume_attempt_for_transport_init_and_bind(owner_a)
        .is_some());
    let _transport_a = events.try_recv().expect("transport A");
    let timeout_a = handle
        .shared
        .test_current_phase_token(TransportPhase::LoggingIn);
    assert!(handle.shared.begin_connection_attempt());
    let _request_b = events.try_recv().expect("request B");
    assert_eq!(handle.shared.connection_epoch(), 2);
    handle.shared.fire_phase_deadline(timeout_a);
    assert!(
        events.try_recv().is_err(),
        "late A timeout must not close B"
    );

    assert!(handle
        .shared
        .consume_attempt_for_transport_init_and_bind(owner_b)
        .is_some());
    let _transport_b = events.try_recv().expect("transport B");
    let timeout_b = handle
        .shared
        .test_current_phase_token(TransportPhase::LoggingIn);
    let close_b = handle.shared.mark_disconnected(None);
    let closed_b = events.try_recv().expect("disconnect close B");
    assert_eq!(payload_json(&closed_b)["close"]["code"], close_b.code);
    handle.shared.fire_phase_deadline(timeout_b);
    assert!(
        events.try_recv().is_err(),
        "late timeout after disconnect is inert"
    );

    let stop_handle = RuntimeHandle::new(RunConfig::default());
    let mut stop_events = stop_handle.subscribe();
    assert!(stop_handle.shared.begin_connection_attempt());
    let _stop_request = stop_events.try_recv().expect("stop request");
    assert!(stop_handle
        .shared
        .consume_attempt_for_transport_init_and_bind(owner_a)
        .is_some());
    let _stop_transport = stop_events.try_recv().expect("stop transport");
    let timeout_before_stop = stop_handle
        .shared
        .test_current_phase_token(TransportPhase::LoggingIn);
    stop_handle.stop("timeout-stop-race");
    let _stop_close = stop_events.try_recv().expect("stop close");
    let _stopped = stop_events.try_recv().expect("stopped");
    stop_handle.shared.fire_phase_deadline(timeout_before_stop);
    assert!(
        stop_events.try_recv().is_err(),
        "late timeout after stop is inert"
    );
}

#[tokio::test]
async fn runtime_shaped_timer_tasks_install_progress_and_cancel_on_phase_and_stop() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut config = RunConfig::default();
            config.timeouts.login_ms = 1;
            config.timeouts.spawn_ms = 1;
            config.timeouts.stop_ms = 1;
            config.reconnect.stable_reset_ms = 1_000;
            let handle = RuntimeHandle::new(config);
            handle.shared.timers_enabled.store(true, Ordering::Release);
            let mut world = bevy_ecs::world::World::new();
            let owner = world.spawn_empty().id();
            let mut events = handle.subscribe();

            assert!(handle.shared.begin_connection_attempt());
            let _request = events.try_recv().expect("request");
            assert!(handle
                .shared
                .consume_attempt_for_transport_init_and_bind(owner)
                .is_some());
            let _transport = events.try_recv().expect("transport");
            // The real login timer was installed; advancing to the next
            // phase invalidates it before it can close the attempt.
            handle
                .shared
                .emit_logged_in("26.1.2", "minecraft:overworld".to_owned());
            let _logged_in = events.try_recv().expect("logged in");
            handle.shared.emit_ready(1);
            let _ready = events.try_recv().expect("ready");
            tokio::time::sleep(Duration::from_millis(5)).await;
            assert!(
                events.try_recv().is_err(),
                "cancelled login/spawn timers stay inert"
            );

            let watchdog_handle = RuntimeHandle::new(RunConfig {
                timeouts: BackendTimeouts {
                    connect_ms: 1,
                    login_ms: 1,
                    spawn_ms: 1,
                    stop_ms: 1,
                },
                ..RunConfig::default()
            });
            watchdog_handle
                .shared
                .timers_enabled
                .store(true, Ordering::Release);
            let mut watchdog_events = watchdog_handle.subscribe();
            assert!(watchdog_handle.shared.begin_connection_attempt());
            let _watchdog_request = watchdog_events.try_recv().expect("watchdog request");
            let (completion, state) = CommandCompletion::channel("runtime-watchdog".to_owned());
            state.begin_active_release(Arc::new(Notify::new()));
            *watchdog_handle.shared.active_movement_completion.lock() = Some(state);
            *watchdog_handle.shared.active_movement_id.lock() = Some("runtime-watchdog".to_owned());
            watchdog_handle
                .shared
                .active_movement
                .store(true, Ordering::Release);
            watchdog_handle.stop("runtime-watchdog");
            let _watchdog_close = watchdog_events.try_recv().expect("watchdog close");
            tokio::time::sleep(Duration::from_millis(5)).await;
            assert!(completion.wait().await.is_err());
            let stopped = watchdog_events.try_recv().expect("watchdog stopped");
            assert_eq!(payload_json(&stopped)["type"], "stopped");
            assert!(watchdog_events.try_recv().is_err());
        })
        .await;
}

#[test]
fn normal_stop_invalidates_watchdog_and_publishes_stopped_once() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    assert!(handle.shared.begin_connection_attempt());
    let _request = events.try_recv().expect("request");
    handle.stop("normal-stop");
    let _closed = events.try_recv().expect("close");
    let stopped = events.try_recv().expect("stopped");
    assert_eq!(payload_json(&stopped)["type"], "stopped");
    let token = handle.shared.test_current_stop_watchdog_token();
    handle.shared.fire_stop_watchdog(token);
    assert!(
        events.try_recv().is_err(),
        "cancelled watchdog must not duplicate stopped"
    );
}

#[tokio::test]
async fn normal_stop_cancels_watchdog_task_before_large_stop_deadline() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut config = RunConfig::default();
            config.timeouts.stop_ms = 60_000;
            let handle = RuntimeHandle::new(config);
            handle.shared.timers_enabled.store(true, Ordering::Release);
            let (probe_sender, probe_receiver) = oneshot::channel();
            *handle.shared.stop_watchdog_completion_probe.lock() = Some(probe_sender);
            let weak_shared = Arc::downgrade(&handle.shared);
            let mut events = handle.subscribe();

            assert!(handle.shared.begin_connection_attempt());
            let _request = events.try_recv().expect("request");
            // initiate_stop spawns the watchdog and immediately completes
            // normal cleanup in this case.  No yield occurs between those
            // operations, so this also exercises Notify's pre-first-poll
            // permit retention.
            handle.stop("normal-stop-task");
            let _closed = events.try_recv().expect("close");
            let stopped = events.try_recv().expect("stopped");
            assert_eq!(payload_json(&stopped)["type"], "stopped");
            drop(events);
            drop(handle);

            tokio::time::timeout(Duration::from_secs(1), probe_receiver)
                .await
                .expect("normal cleanup must cancel the large watchdog without sleeping")
                .expect("watchdog completion probe");
            assert!(
                weak_shared.upgrade().is_none(),
                "completed watchdog task must not retain RuntimeShared"
            );
        })
        .await;
}

#[tokio::test]
async fn stop_watchdog_forces_pending_movement_and_has_no_fault_or_reconnect() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    assert!(handle.shared.begin_connection_attempt());
    let _request = events.try_recv().expect("request");
    let (completion, state) = CommandCompletion::channel("watchdog-move".to_owned());
    state.begin_active_release(Arc::new(Notify::new()));
    *handle.shared.active_movement_completion.lock() = Some(state);
    *handle.shared.active_movement_id.lock() = Some("watchdog-move".to_owned());
    handle.shared.active_movement.store(true, Ordering::Release);

    handle.stop("watchdog-stop");
    let _closed = events.try_recv().expect("close");
    let token = handle.shared.test_current_stop_watchdog_token();
    assert!(
        events.try_recv().is_err(),
        "stopped waits for forced cleanup"
    );
    handle.shared.fire_stop_watchdog(token);
    assert!(completion.wait().await.is_err());
    let stopped = events.try_recv().expect("forced stopped");
    assert_eq!(payload_json(&stopped)["type"], "stopped");
    assert!(events.try_recv().is_err());
    assert!(matches!(handle.state(), BackendState::Stopped { .. }));
}

// ================= NEW-13 V2: AttemptToken ↔ epoch binding =================
