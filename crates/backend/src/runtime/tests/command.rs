use super::*;

#[test]
fn command_validation_matches_motor_boundary() {
    assert!(validate_command(&BackendCommand::Move {
        directions: vec![MotorDirection::Forward, MotorDirection::Left],
        duration_ms: 1_500,
        sprint: Some(true),
        jump: Some(false),
        crouch: Some(false),
    })
    .is_ok());
    assert!(validate_command(&BackendCommand::Move {
        directions: vec![MotorDirection::Forward, MotorDirection::Forward],
        duration_ms: 100,
        sprint: None,
        jump: None,
        crouch: None,
    })
    .is_err());
    assert!(validate_command(&BackendCommand::Move {
        directions: vec![MotorDirection::Forward],
        duration_ms: 49,
        sprint: None,
        jump: None,
        crouch: None,
    })
    .is_err());
    assert!(validate_command(&BackendCommand::SendChat {
        message: "hello\nworld".to_owned(),
    })
    .is_err());
}

#[test]
fn relative_look_validation_rejects_non_finite_angles() {
    assert!(validate_command(&BackendCommand::LookRelative {
        yaw_degrees: 90.0,
        pitch_degrees: -90.0,
    })
    .is_ok());
    assert!(validate_command(&BackendCommand::LookRelative {
        yaw_degrees: 90.1,
        pitch_degrees: 0.0,
    })
    .is_err());
    assert!(validate_command(&BackendCommand::LookRelative {
        yaw_degrees: f32::NAN,
        pitch_degrees: 0.0,
    })
    .is_err());
}

#[test]
fn run_config_rejects_invalid_offline_username() {
    let mut config = RunConfig::default();
    config.username = "MineIntentUsernameTooLong".to_owned();
    assert!(validate_run_config(&config).is_err());
    config.username = "bad-name".to_owned();
    assert!(validate_run_config(&config).is_err());
    config.username = "MineM130Fresh".to_owned();
    assert!(validate_run_config(&config).is_ok());
}

#[tokio::test]
async fn command_completion_seam_reports_fifo_success_failure_and_shutdown_cancel() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let first = handle
        .look_relative(10.0, -5.0)
        .expect("look command should enqueue");
    let second = handle
        .move_input(vec![MotorDirection::Forward], 100, None, None, None)
        .expect("move command should enqueue");
    let third = handle
        .release_all()
        .expect("release command should enqueue");
    assert_eq!(first.command_id(), "command-1");
    assert_eq!(second.command_id(), "command-2");
    assert_eq!(third.command_id(), "command-3");

    let first_queued = handle.shared.pop_command().expect("first queue item");
    let second_queued = handle.shared.pop_command().expect("second queue item");
    let third_queued = handle.shared.pop_command().expect("third queue item");
    assert_eq!(first_queued.envelope.id, "command-1");
    assert_eq!(second_queued.envelope.id, "command-2");
    assert_eq!(third_queued.envelope.id, "command-3");

    finish_command(&first_queued.completion, Ok(()));
    finish_command(
        &second_queued.completion,
        Err(BackendError::BackendFailure {
            failure: BackendFailure {
                code: BackendFailureCode::ProtocolError,
                message: "synthetic movement failure".to_owned(),
                retryable: true,
            },
        }),
    );
    handle.shared.requeue_front(third_queued);
    handle.shared.cancel_pending_commands();

    assert_eq!(first.wait().await, Ok(()));
    assert!(matches!(
        second.wait().await,
        Err(BackendError::BackendFailure {
            failure: BackendFailure {
                code: BackendFailureCode::ProtocolError,
                ..
            }
        })
    ));
    assert!(matches!(
        third.wait().await,
        Err(BackendError::Cancelled { .. })
    ));
}

#[tokio::test]
async fn runtime_stop_admission_linearizes_enqueue_and_settles_completion() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let producer_handle = handle.clone();
    let (result_tx, result_rx) = std_mpsc::channel();
    let producer = thread::spawn(move || {
        let result = producer_handle.look_relative(1.0, 2.0);
        result_tx
            .send(result)
            .expect("command producer result should be delivered");
    });

    for _ in 0..100_000 {
        if !handle.shared.commands.lock().is_empty() {
            break;
        }
        thread::yield_now();
    }
    assert!(
        !handle.shared.commands.lock().is_empty(),
        "producer must enqueue before stop wins the admission lock"
    );
    handle.stop("enqueue_race_stop");
    producer.join().expect("command producer should not panic");

    let completion = result_rx
        .recv_timeout(StdDuration::from_secs(1))
        .expect("completion enqueue result");
    let completion = completion.expect("enqueue that won must return a completion");
    assert!(matches!(
        completion.wait().await,
        Err(BackendError::Cancelled { .. })
    ));
    assert!(handle.shared.commands.lock().is_empty());

    let stopped_first = RuntimeHandle::new(RunConfig::default());
    stopped_first.stop("already_stopped");
    assert!(stopped_first.send_chat("not queued").is_err());
    assert!(stopped_first.look_relative(0.0, 0.0).is_err());
    assert!(stopped_first.shared.commands.lock().is_empty());
}

#[test]
fn global_stream_excludes_keep_alive_motor_error_and_commanded_chat() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();

    handle
        .send_chat("commanded chat stays local")
        .expect("commanded chat should enqueue locally");
    let _look = handle
        .look_relative(10.0, 5.0)
        .expect("look completion should enqueue locally");
    let _move = handle
        .move_input(vec![MotorDirection::Forward], 100, None, None, None)
        .expect("move completion should enqueue locally");
    let _release = handle
        .release_all()
        .expect("release completion should enqueue locally");

    assert!(events.try_recv().is_err());
    assert_eq!(handle.shared.commands.lock().len(), 4);
    assert_eq!(
        mineintent_contracts::minecraft::BackendEventKind::PRODUCT_KINDS.len(),
        9
    );
}

#[tokio::test]
async fn runtime_stop_during_move_registration_cancels_before_any_actuator() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    handle.shared.begin_connection_attempt();
    let _request = events.try_recv().expect("connection request");

    let (completion, state) = CommandCompletion::channel("command-registration".to_owned());
    let stop_handle = handle.clone();
    handle
        .shared
        .set_active_movement_registration_hook(Some(Arc::new(move || {
            stop_handle.stop("registration_stop")
        })));

    let generation = handle
        .shared
        .movement_generation
        .fetch_add(1, Ordering::AcqRel)
        + 1;
    let registration = handle.shared.register_active_movement(
        "command-registration",
        generation,
        250,
        &Some(state.clone()),
    );

    assert!(matches!(
        registration,
        ActiveMovementRegistration::Cancelled
    ));
    assert!(!handle.shared.active_movement.load(Ordering::Acquire));
    assert!(handle.shared.active_movement_id.lock().is_none());
    assert!(handle.shared.active_movement_cancel_signal.lock().is_none());
    assert!(handle.shared.active_movement_completion.lock().is_none());
    assert!(!handle
        .shared
        .active_movement_registration
        .load(Ordering::Acquire));

    let closed = events.try_recv().expect("close precedes stopped");
    assert_eq!(payload_json(&closed)["type"], "connection_closed");
    let stopped = events
        .try_recv()
        .expect("stopped waits for registration cleanup");
    assert_eq!(payload_json(&stopped)["type"], "stopped");
    assert_eq!(payload_json(&stopped)["reason"], "registration_stop");
    assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
    assert!(matches!(
        completion.wait().await,
        Err(BackendError::Cancelled { .. })
    ));
}

#[test]
fn runtime_stop_during_move_registration_without_completion_cleans_signal() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let mut events = handle.subscribe();
    handle.shared.begin_connection_attempt();
    let _request = events.try_recv().expect("connection request");

    let stop_handle = handle.clone();
    handle
        .shared
        .set_active_movement_registration_hook(Some(Arc::new(move || {
            stop_handle.stop("registration_without_completion")
        })));
    let generation = handle
        .shared
        .movement_generation
        .fetch_add(1, Ordering::AcqRel)
        + 1;
    let registration = handle.shared.register_active_movement(
        "command-registration-no-completion",
        generation,
        250,
        &None,
    );

    assert!(matches!(
        registration,
        ActiveMovementRegistration::Cancelled
    ));
    assert!(!handle.shared.active_movement.load(Ordering::Acquire));
    assert!(handle.shared.active_movement_id.lock().is_none());
    assert!(handle.shared.active_movement_cancel_signal.lock().is_none());
    assert!(handle.shared.active_movement_completion.lock().is_none());
    assert_eq!(
        payload_json(&events.try_recv().expect("close event"))["type"],
        "connection_closed"
    );
    assert_eq!(
        payload_json(&events.try_recv().expect("stopped event"))["type"],
        "stopped"
    );
    assert!(handle.shared.shutdown_requested.load(Ordering::Acquire));
}

#[tokio::test]
async fn active_move_completion_cancel_notifies_release_and_finishes_once() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let (completion, state) = CommandCompletion::channel("command-active".to_owned());
    let signal = Arc::new(Notify::new());
    state.begin_active_release(signal.clone());
    *handle.shared.active_movement_completion.lock() = Some(state.clone());
    handle.shared.active_movement.store(true, Ordering::Release);
    *handle.shared.active_movement_id.lock() = Some("command-active".to_owned());

    let release_attempted = Arc::new(AtomicBool::new(false));
    let release_saw_active = Arc::new(AtomicBool::new(false));
    let release_saw_pending_completion = Arc::new(AtomicBool::new(false));
    let release_attempted_by_task = release_attempted.clone();
    let release_saw_active_by_task = release_saw_active.clone();
    let release_saw_pending_by_task = release_saw_pending_completion.clone();
    let task_shared = handle.shared.clone();
    let task_state = state.clone();
    let release_waiter = tokio::spawn(async move {
        signal.notified().await;
        let completion_for_task = Some(task_state.clone());
        release_active_movement_and_finish(
            &task_shared,
            "command-active",
            0,
            &completion_for_task,
            || {
                release_attempted_by_task.store(true, Ordering::Release);
                release_saw_active_by_task.store(
                    task_shared.active_movement.load(Ordering::Acquire)
                        && task_shared.active_movement_id.lock().as_deref()
                            == Some("command-active"),
                    Ordering::Release,
                );
                release_saw_pending_by_task
                    .store(task_state.sender.lock().is_some(), Ordering::Release);
                true
            },
            "cancel move",
            Err(BackendError::Cancelled {
                operation: "command:command-active".to_owned(),
            }),
        );
    });

    completion.cancel();
    completion.cancel();
    assert!(
        state.sender.lock().is_some(),
        "active cancellation must defer completion until release"
    );
    tokio::time::timeout(StdDuration::from_secs(1), release_waiter)
        .await
        .expect("active movement cancellation must run the release seam")
        .expect("release waiter should not panic");

    assert!(release_attempted.load(Ordering::Acquire));
    assert!(release_saw_active.load(Ordering::Acquire));
    assert!(release_saw_pending_completion.load(Ordering::Acquire));
    assert!(!handle.shared.active_movement.load(Ordering::Acquire));
    assert!(handle.shared.active_movement_id.lock().is_none());
    assert!(state.sender.lock().is_none());
    assert!(matches!(
        completion.wait().await,
        Err(BackendError::Cancelled { .. })
    ));
}

#[test]
fn active_move_cancellation_wins_first_actuator_admission() {
    let handle = RuntimeHandle::new(RunConfig::default());
    handle.shared.begin_connection_attempt();
    let (completion, state) = CommandCompletion::channel("command-first-actuator".to_owned());
    let generation = handle
        .shared
        .movement_generation
        .fetch_add(1, Ordering::AcqRel)
        + 1;
    let registration = handle.shared.register_active_movement(
        "command-first-actuator",
        generation,
        250,
        &Some(state.clone()),
    );
    assert!(matches!(
        registration,
        ActiveMovementRegistration::Started { .. }
    ));

    completion.cancel();
    let actuator_called = Arc::new(AtomicBool::new(false));
    let called = actuator_called.clone();
    let result = handle.shared.with_active_movement_admission(
        "command-first-actuator",
        generation,
        &Some(state.clone()),
        || {
            called.store(true, Ordering::Release);
        },
    );
    assert!(result.is_err());
    assert!(!actuator_called.load(Ordering::Acquire));
    let cancel_signal = handle.shared.active_movement_cancel_signal.lock().clone();
    handle.shared.cancel_registered_active_movement(
        "command-first-actuator",
        generation,
        &cancel_signal,
        &Some(state.clone()),
    );
    assert!(state.sender.lock().is_none());
}

#[test]
fn not_ready_command_head_cannot_be_overtaken_by_a_later_enqueue() {
    let handle = RuntimeHandle::new(RunConfig::default());
    let first = handle
        .look_relative(1.0, 2.0)
        .expect("first command should enqueue");
    let barrier = Arc::new(Barrier::new(2));
    let producer_started = Arc::new(AtomicBool::new(false));
    let producer_done = Arc::new(AtomicBool::new(false));
    let (producer_id_tx, producer_id_rx) = std_mpsc::channel();
    let producer_handle = handle.clone();
    let producer_barrier = barrier.clone();
    let producer_started_flag = producer_started.clone();
    let producer_done_flag = producer_done.clone();
    let producer = thread::spawn(move || {
        producer_barrier.wait();
        producer_started_flag.store(true, Ordering::Release);
        let second = producer_handle
            .release_all()
            .expect("concurrent later command should enqueue");
        producer_id_tx
            .send(second.command_id().to_owned())
            .expect("producer id should be delivered");
        producer_done_flag.store(true, Ordering::Release);
    });

    assert!(handle
        .shared
        .next_command_for_processing_with_hook(|| {
            barrier.wait();
            while !producer_started.load(Ordering::Acquire) {
                thread::yield_now();
            }
            assert!(
                !producer_done.load(Ordering::Acquire),
                "producer must still be blocked by the queue lock"
            );
        })
        .is_none());
    producer
        .join()
        .expect("concurrent producer should not panic");
    let second_id = producer_id_rx
        .recv_timeout(StdDuration::from_secs(1))
        .expect("later command id");

    let first_queued = handle.shared.pop_command().expect("deferred head");
    let second_queued = handle.shared.pop_command().expect("later queue item");
    assert_eq!(first_queued.envelope.id, first.command_id());
    assert_eq!(second_queued.envelope.id, second_id);
    handle.shared.requeue_front(second_queued);
    handle.shared.requeue_front(first_queued);
    handle.shared.cancel_pending_commands();
}

/// epoch 切片执行点复检：同代放行，重连推进一代后旧戳被拒且错误携带双方世代。
/// 所有权半边需要真实 Client，由出队门既有回归与 Paper 纵向覆盖。
#[test]
fn execution_point_epoch_recheck_rejects_stale_stamp_and_passes_current() {
    let handle = RuntimeHandle::new(RunConfig::default());
    handle.shared.begin_connection_attempt();
    let bound_epoch = handle.shared.writer.lock().connection_epoch;
    assert!(
        handle
            .shared
            .stale_epoch_error_locked(bound_epoch)
            .is_none(),
        "同代戳必须放行"
    );
    handle.shared.begin_connection_attempt();
    match handle.shared.stale_epoch_error_locked(bound_epoch) {
        Some(BackendError::StaleEpoch {
            bound_epoch: bound,
            current_epoch,
        }) => {
            assert_eq!(bound, bound_epoch);
            assert_eq!(current_epoch, bound_epoch + 1);
        }
        other => panic!("旧戳必须被拒，实得 {other:?}"),
    }
}

/// epoch 切片入队门：期望世代不匹配的命令拒于入队，不进队列；
/// 匹配的入队项携带入队时刻的 epoch 戳供执行点复检。
#[test]
fn enqueue_rejects_mismatched_expected_epoch_and_stamps_queue_items() {
    let handle = RuntimeHandle::new(RunConfig::default());
    handle.shared.begin_connection_attempt();
    let epoch = handle.shared.writer.lock().connection_epoch;
    let envelope = |id: &str| BackendCommandEnvelope {
        protocol: "mineintent.backend-command.v1".to_owned(),
        id: id.to_owned(),
        issued_at: chrono::Utc::now(),
        command: BackendCommand::SendChat {
            message: "hi".to_owned(),
        },
    };
    let error = handle
        .shared
        .enqueue_command_if_running(envelope("stale"), Some(epoch + 1))
        .expect_err("期望世代不匹配必须拒于入队");
    assert!(
        error.contains("stale command epoch"),
        "拒绝理由必须点名世代陈旧，实得 {error}"
    );
    assert!(handle.shared.pop_command().is_none(), "被拒命令不得入队");
    handle
        .shared
        .enqueue_command_if_running(envelope("current"), Some(epoch))
        .expect("同代命令正常入队");
    let queued = handle.shared.pop_command().expect("入队项存在");
    assert_eq!(queued.connection_epoch, epoch, "队列项携带入队时刻的世代戳");
}
