use super::observation::valid_observation_payload;
use super::*;

#[test]
fn runtime_subscriber_queue_stays_bounded_under_unconsumed_high_frequency() {
    let mut config = RunConfig::default();
    config.emit_stdout = false;
    let handle = RuntimeHandle::new(config);
    let mut events = handle.subscribe();
    for _ in 0..10_000 {
        handle.test_drive_event(
            FactSource::ServerObserved,
            valid_observation_payload(BackendEventKind::Entity),
        );
        assert!(
            events.queued_count()
                <= RUNTIME_BROKER_ORDINARY_CAPACITY
                    + RUNTIME_BROKER_CONTROL_CAPACITY
                    + RUNTIME_BROKER_OVERFLOW_CAPACITY
                    + 1
        );
    }
    handle.test_drive_event(
        FactSource::ServerObserved,
        BackendEventPayload::Chat(ContractProtocolChatEvent {
            sender_username: Some("Alex".to_owned()),
            plain_text: "upstream chat".to_owned(),
            position: Some(ChatPosition::Chat),
            verified: None,
        }),
    );
    let mut saw_chat = false;
    while let Ok(event) = events.try_recv() {
        if matches!(
            event.payload,
            BackendEventPayload::Chat(ContractProtocolChatEvent { ref plain_text, .. })
                if plain_text == "upstream chat"
        ) {
            saw_chat = true;
        }
    }
    assert!(saw_chat, "bounded upstream must retain admitted chat");
}

#[test]
fn runtime_broker_old_marker_pop_does_not_close_new_loss_segment() {
    let queue = RuntimeEventQueue::new(None);
    let cancel = AtomicBool::new(false);
    let mut writer = EventWriter::new("world");

    for _ in 0..RUNTIME_BROKER_ORDINARY_CAPACITY {
        assert!(queue.publish(
            writer.emit(
                FactSource::ServerObserved,
                valid_observation_payload(BackendEventKind::Entity)
            ),
            &cancel,
        ));
    }
    assert!(queue.publish(
        writer.emit(
            FactSource::ServerObserved,
            valid_observation_payload(BackendEventKind::Entity)
        ),
        &cancel,
    ));
    assert!(queue.publish(
        writer.emit(
            FactSource::ServerObserved,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected),
        ),
        &cancel,
    ));
    for _ in 0..RUNTIME_BROKER_ORDINARY_CAPACITY {
        queue.pop().expect("ordinary broker entry");
    }
    for _ in 0..RUNTIME_BROKER_ORDINARY_CAPACITY {
        assert!(queue.publish(
            writer.emit(
                FactSource::ServerObserved,
                valid_observation_payload(BackendEventKind::Entity)
            ),
            &cancel,
        ));
    }
    assert!(queue.publish(
        writer.emit(
            FactSource::ServerObserved,
            valid_observation_payload(BackendEventKind::Entity)
        ),
        &cancel,
    ));
    let first = queue.pop().expect("first broker marker");
    assert!(matches!(
        first.payload,
        BackendEventPayload::Overflow(payload) if payload.dropped_count == 1
    ));

    assert!(queue.publish(
        writer.emit(
            FactSource::ServerObserved,
            valid_observation_payload(BackendEventKind::Entity)
        ),
        &cancel,
    ));
    let control = queue.pop().expect("broker control after first marker");
    assert!(matches!(
        control.payload,
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected)
    ));
    for _ in 0..RUNTIME_BROKER_ORDINARY_CAPACITY {
        queue.pop().expect("refilled ordinary broker entry");
    }
    let second = queue.pop().expect("second broker marker");
    assert!(matches!(
        second.payload,
        BackendEventPayload::Overflow(payload) if payload.dropped_count == 2
    ));
}

#[test]
fn runtime_dispatch_old_marker_pop_does_not_close_new_loss_segment() {
    let mut state = EventDispatchState::default();
    let cancel = AtomicBool::new(false);
    let mut writer = EventWriter::new("world");

    for _ in 0..RUNTIME_DISPATCH_ORDINARY_CAPACITY {
        let mut event = Some(writer.emit(
            FactSource::ServerObserved,
            valid_observation_payload(BackendEventKind::Entity),
        ));
        let mut pending = None;
        assert!(matches!(
            state.enqueue(&mut event, &mut pending, &cancel),
            RuntimeDispatchAdmission::Accepted(_)
        ));
    }
    for payload in [
        valid_observation_payload(BackendEventKind::Entity),
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected),
    ] {
        let mut event = Some(writer.emit(FactSource::ServerObserved, payload));
        let mut pending = None;
        assert!(matches!(
            state.enqueue(&mut event, &mut pending, &cancel),
            RuntimeDispatchAdmission::Accepted(_)
        ));
    }

    for _ in 0..RUNTIME_DISPATCH_ORDINARY_CAPACITY {
        state.pop_next().expect("ordinary dispatch entry");
    }
    for _ in 0..RUNTIME_DISPATCH_ORDINARY_CAPACITY {
        let mut event = Some(writer.emit(
            FactSource::ServerObserved,
            valid_observation_payload(BackendEventKind::Entity),
        ));
        let mut pending = None;
        assert!(matches!(
            state.enqueue(&mut event, &mut pending, &cancel),
            RuntimeDispatchAdmission::Accepted(_)
        ));
    }
    let mut event = Some(writer.emit(
        FactSource::ServerObserved,
        valid_observation_payload(BackendEventKind::Entity),
    ));
    let mut pending = None;
    assert!(matches!(
        state.enqueue(&mut event, &mut pending, &cancel),
        RuntimeDispatchAdmission::Accepted(_)
    ));
    let first = state.pop_next().expect("first dispatch marker");
    assert!(matches!(
        first.payload,
        BackendEventPayload::Overflow(payload) if payload.dropped_count == 1
    ));

    let mut event = Some(writer.emit(
        FactSource::ServerObserved,
        valid_observation_payload(BackendEventKind::Entity),
    ));
    let mut pending = None;
    assert!(matches!(
        state.enqueue(&mut event, &mut pending, &cancel),
        RuntimeDispatchAdmission::Accepted(_)
    ));
    let control = state
        .pop_next()
        .expect("dispatch control after first marker");
    assert!(matches!(
        control.payload,
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected)
    ));
    for _ in 0..RUNTIME_DISPATCH_ORDINARY_CAPACITY {
        state.pop_next().expect("refilled ordinary dispatch entry");
    }
    let second = state.pop_next().expect("second dispatch marker");
    assert!(matches!(
        second.payload,
        BackendEventPayload::Overflow(payload) if payload.dropped_count == 2
    ));
}

#[test]
fn runtime_dispatch_waiting_ticket_cannot_be_overtaken_by_later_ordinary() {
    let state = Arc::new(parking_lot::Mutex::new(EventDispatchState::default()));
    let cancel = Arc::new(AtomicBool::new(false));
    let mut writer = EventWriter::new("world");
    {
        let mut guard = state.lock();
        for _ in 0..RUNTIME_DISPATCH_CONTROL_CAPACITY {
            let mut event = Some(writer.emit(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected),
            ));
            let mut pending = None;
            assert!(matches!(
                guard.enqueue(&mut event, &mut pending, &cancel),
                RuntimeDispatchAdmission::Accepted(_)
            ));
        }
    }

    let first_event = writer.emit(
        FactSource::ServerObserved,
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::LoggedIn {
            version: "test".to_owned(),
            dimension: "minecraft:overworld".to_owned(),
        }),
    );
    let later_event = writer.emit(
        FactSource::ServerObserved,
        valid_observation_payload(BackendEventKind::Entity),
    );
    let (first_waited, first_waited_received) = std_mpsc::channel();
    let (later_waited, later_waited_received) = std_mpsc::channel();
    let waiting_barrier = Arc::new(Barrier::new(3));
    let (first_release, first_release_received) = std_mpsc::channel();
    let (later_release, later_release_received) = std_mpsc::channel();
    let (admitted, admitted_received) = std_mpsc::channel();

    let first_state = state.clone();
    let first_cancel = cancel.clone();
    let first_waiting_barrier = waiting_barrier.clone();
    let first_admitted = admitted.clone();
    let first = thread::spawn(move || {
        let mut event = Some(first_event);
        let mut pending = None;
        let mut guard = first_state.lock();
        assert!(matches!(
            guard.enqueue(&mut event, &mut pending, &first_cancel),
            RuntimeDispatchAdmission::Wait
        ));
        first_waited.send(()).expect("first wait gate");
        drop(guard);
        first_waiting_barrier.wait();
        first_release_received
            .recv_timeout(StdDuration::from_secs(1))
            .expect("first admission release");
        let mut guard = first_state.lock();
        assert!(matches!(
            guard.enqueue(&mut event, &mut pending, &first_cancel),
            RuntimeDispatchAdmission::Accepted(_)
        ));
        first_admitted
            .send("control")
            .expect("first admission result");
    });

    first_waited_received
        .recv_timeout(StdDuration::from_secs(1))
        .expect("first control must wait at full control capacity");

    let later_state = state.clone();
    let later_cancel = cancel.clone();
    let later_waiting_barrier = waiting_barrier.clone();
    let later_admitted = admitted.clone();
    let later = thread::spawn(move || {
        let mut event = Some(later_event);
        let mut pending = None;
        let mut guard = later_state.lock();
        assert!(matches!(
            guard.enqueue(&mut event, &mut pending, &later_cancel),
            RuntimeDispatchAdmission::Wait
        ));
        later_waited.send(()).expect("later wait gate");
        drop(guard);
        later_waiting_barrier.wait();
        later_release_received
            .recv_timeout(StdDuration::from_secs(1))
            .expect("later admission release");
        let mut guard = later_state.lock();
        assert!(matches!(
            guard.enqueue(&mut event, &mut pending, &later_cancel),
            RuntimeDispatchAdmission::Accepted(_)
        ));
        later_admitted
            .send("ordinary")
            .expect("later admission result");
    });

    later_waited_received
        .recv_timeout(StdDuration::from_secs(1))
        .expect("later ordinary must wait behind the earlier control ticket");
    assert!(admitted_received.try_recv().is_err());

    {
        let mut guard = state.lock();
        guard.control.pop_front();
    }
    waiting_barrier.wait();
    first_release
        .send(())
        .expect("first admission release sender");
    assert_eq!(
        admitted_received
            .recv_timeout(StdDuration::from_secs(1))
            .expect("first admission after one control slot is released"),
        "control"
    );
    later_release
        .send(())
        .expect("later admission release sender");
    assert_eq!(
        admitted_received
            .recv_timeout(StdDuration::from_secs(1))
            .expect("later admission after the first ticket advances"),
        "ordinary"
    );
    first.join().expect("first waiter");
    later.join().expect("later waiter");
}

#[test]
fn runtime_dispatch_stop_cancels_waiting_ticket_and_advances_turn() {
    let state = Arc::new(parking_lot::Mutex::new(EventDispatchState::default()));
    let wake = Arc::new(parking_lot::Condvar::new());
    let cancel = Arc::new(AtomicBool::new(false));
    let mut writer = EventWriter::new("world");
    {
        let mut guard = state.lock();
        for _ in 0..RUNTIME_DISPATCH_CONTROL_CAPACITY {
            let mut event = Some(writer.emit(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected),
            ));
            let mut pending = None;
            assert!(matches!(
                guard.enqueue(&mut event, &mut pending, &cancel),
                RuntimeDispatchAdmission::Accepted(_)
            ));
        }
    }
    let first_event = writer.emit(
        FactSource::ServerObserved,
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::LoggedIn {
            version: "test".to_owned(),
            dimension: "minecraft:overworld".to_owned(),
        }),
    );
    let later_event = writer.emit(
        FactSource::ServerObserved,
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::Ready {
            snapshot_revision: 1,
        }),
    );
    let (first_waited, first_waited_received) = std_mpsc::channel();
    let (later_waited, later_waited_received) = std_mpsc::channel();
    let (cancelled, cancelled_received) = std_mpsc::channel();
    let cancellation_barrier = Arc::new(Barrier::new(3));
    let first_state = state.clone();
    let first_cancel = cancel.clone();
    let first_cancelled = cancelled.clone();
    let first_cancellation_barrier = cancellation_barrier.clone();
    let first = thread::spawn(move || {
        let mut event = Some(first_event);
        let mut pending = None;
        loop {
            let mut guard = first_state.lock();
            match guard.enqueue(&mut event, &mut pending, &first_cancel) {
                RuntimeDispatchAdmission::Wait => {
                    let _ = first_waited.send(());
                    drop(guard);
                    first_cancellation_barrier.wait();
                    continue;
                }
                RuntimeDispatchAdmission::Cancelled => {
                    first_cancelled.send("first").expect("first cancellation");
                    return;
                }
                RuntimeDispatchAdmission::Accepted(_) => {
                    panic!("stop-cancelled first waiter must not admit");
                }
            }
        }
    });
    first_waited_received
        .recv_timeout(StdDuration::from_secs(1))
        .expect("first stop-cancelled waiter");

    let later_state = state.clone();
    let later_cancel = cancel.clone();
    let later_cancelled = cancelled.clone();
    let later_cancellation_barrier = cancellation_barrier.clone();
    let later = thread::spawn(move || {
        let mut event = Some(later_event);
        let mut pending = None;
        loop {
            let mut guard = later_state.lock();
            match guard.enqueue(&mut event, &mut pending, &later_cancel) {
                RuntimeDispatchAdmission::Wait => {
                    let _ = later_waited.send(());
                    drop(guard);
                    later_cancellation_barrier.wait();
                    continue;
                }
                RuntimeDispatchAdmission::Cancelled => {
                    later_cancelled.send("later").expect("later cancellation");
                    return;
                }
                RuntimeDispatchAdmission::Accepted(_) => {
                    panic!("stop-cancelled later waiter must not admit");
                }
            }
        }
    });
    later_waited_received
        .recv_timeout(StdDuration::from_secs(1))
        .expect("later stop-cancelled waiter");

    cancel.store(true, Ordering::Release);
    cancellation_barrier.wait();
    wake.notify_all();
    let mut cancelled_names = vec![
        cancelled_received
            .recv_timeout(StdDuration::from_secs(1))
            .expect("first waiter cancellation"),
        cancelled_received
            .recv_timeout(StdDuration::from_secs(1))
            .expect("later waiter cancellation"),
    ];
    cancelled_names.sort_unstable();
    assert_eq!(cancelled_names, vec!["first", "later"]);
    first.join().expect("first cancelled waiter");
    later.join().expect("later cancelled waiter");
    let state = state.lock();
    assert_eq!(state.next_admission, state.next_sequence);
}

#[test]
fn runtime_stop_cancellation_is_not_reset_while_connection_admission_waits() {
    let mut config = RunConfig::default();
    config.emit_stdout = false;
    let handle = RuntimeHandle::new(config);

    let (broker_waiting, broker_waiting_received) = std_mpsc::channel();
    handle.test_set_runtime_broker_backpressure_hook(Some(Arc::new(move || {
        broker_waiting
            .send(())
            .expect("runtime broker wait gate should remain live");
    })));
    let mut events = handle.subscribe();
    let cancel = AtomicBool::new(false);
    let mut broker_writer = EventWriter::new("world");
    for _ in 0..RUNTIME_BROKER_CONTROL_CAPACITY {
        assert!(events.queue.publish(
            broker_writer.emit(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected,),
            ),
            &cancel,
        ));
    }

    for index in 0..RUNTIME_DISPATCH_CONTROL_CAPACITY {
        let should_drain = handle.shared.enqueue_event(
            FactSource::ServerObserved,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionRequested {
                attempt: 7,
            }),
        );
        assert_eq!(should_drain, index == 0);
    }
    assert_eq!(
        handle.test_event_dispatch_counts(),
        (0, RUNTIME_DISPATCH_CONTROL_CAPACITY, 0, 0)
    );

    let shared = handle.shared.clone();
    let drainer = thread::spawn(move || {
        shared.drain_events();
    });
    broker_waiting_received
        .recv_timeout(StdDuration::from_secs(1))
        .expect("runtime worker drainer must block at the full broker");
    assert!(
        !handle.shared.enqueue_event(
            FactSource::ServerObserved,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionRequested {
                attempt: 8
            },),
        )
    );
    assert_eq!(
        handle.test_event_dispatch_counts(),
        (0, RUNTIME_DISPATCH_CONTROL_CAPACITY, 0, 0)
    );

    let (dispatch_waiting, dispatch_waiting_received) = std_mpsc::channel();
    handle.test_set_event_dispatch_backpressure_hook(Some(Arc::new(move || {
        dispatch_waiting
            .send(())
            .expect("connection admission wait gate should remain live");
    })));
    let connection_shared = handle.shared.clone();
    let (connection_result, connection_result_received) = std_mpsc::channel();
    let (release_command, release_command_received) = std_mpsc::channel();
    let connection = thread::spawn(move || {
        let admission = connection_shared.command_admission.lock();
        let result = connection_shared.begin_connection_attempt_locked(None);
        connection_result
            .send(result)
            .expect("connection admission result should be observable");
        release_command_received
            .recv_timeout(StdDuration::from_secs(1))
            .expect("test must release the command admission");
        drop(admission);
    });
    dispatch_waiting_received
        .recv_timeout(StdDuration::from_secs(1))
        .expect("connection attempt must wait in internal dispatch admission");

    let (stop_signaled, stop_signaled_received) = std_mpsc::channel();
    handle.test_set_stop_signal_hook(Some(Arc::new(move || {
        stop_signaled
            .send(())
            .expect("stop signal gate should remain live");
    })));
    let stop_handle = handle.clone();
    let (stop_finished, stop_finished_received) = std_mpsc::channel();
    let stopper = thread::spawn(move || {
        stop_handle.stop("connection-admission-stop");
        stop_finished
            .send(())
            .expect("stop completion should be observable");
    });
    stop_signaled_received
        .recv_timeout(StdDuration::from_secs(1))
        .expect("stop must publish its monotonic cancellation before command lock");
    assert!(handle.shared.stop_requested.load(Ordering::Acquire));
    assert!(handle.shared.dispatch_cancelled.load(Ordering::Acquire));
    assert!(!handle.shared.shutdown_requested.load(Ordering::Acquire));

    assert_eq!(
        connection_result_received
            .recv_timeout(StdDuration::from_secs(1))
            .expect("connection admission must cancel while stop waits for its lock"),
        Some(false)
    );
    release_command
        .send(())
        .expect("connection command admission release");
    connection.join().expect("connection waiter");
    stop_finished_received
        .recv_timeout(StdDuration::from_secs(1))
        .expect("stop must finish after the cancelled admission releases");
    stopper.join().expect("stopper");
    drainer.join().expect("runtime drainer");

    let mut stopped_count = 0;
    while let Ok(event) = events.try_recv() {
        if matches!(
            event.payload,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Stopped { .. })
        ) {
            stopped_count += 1;
        }
    }
    assert_eq!(
        stopped_count, 1,
        "stop must settle exactly one terminal event"
    );
    assert!(matches!(handle.state(), BackendState::Stopped { .. }));
}

#[test]
fn runtime_dropped_receiver_is_pruned_before_control_backpressure() {
    let mut config = RunConfig::default();
    config.emit_stdout = false;
    let handle = RuntimeHandle::new(config);
    let dropped = handle.subscribe();
    drop(dropped);

    let producer_handle = handle.clone();
    let (finished, finished_received) = std_mpsc::channel();
    let producer = thread::spawn(move || {
        for attempt in 0..(RUNTIME_BROKER_CONTROL_CAPACITY * 2) {
            producer_handle.test_drive_event(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionRequested {
                    attempt: attempt as u32,
                }),
            );
        }
        finished.send(()).expect("dropped receiver producer result");
    });
    finished_received
        .recv_timeout(StdDuration::from_secs(1))
        .expect("dropped receiver must not create control backpressure");
    producer.join().expect("dropped receiver producer");
    assert_eq!(handle.shared.subscribers.lock().len(), 0);
}
