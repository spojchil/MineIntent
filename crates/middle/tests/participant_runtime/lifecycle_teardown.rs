//! 生命周期终态、拆除次序与后端监听。
//!
//! fixture 与辅助函数在父文件，经 `use super::*` 复用。

use super::*;

#[tokio::test]
async fn request_stop_releases_before_bounded_worker_settle_and_speech_cancel() {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, speech, motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    source.set_chats(vec![chat_input(50, "Alice", "@Bot stop")]);
    runtime
        .ingest_backend_event(chat_event("71", 1, "Alice", "@Bot stop"))
        .unwrap();
    wait_for_request(&agent, 1).await;
    assert!(runtime.request_stop().unwrap());
    assert!(motor.releases.load(Ordering::SeqCst) >= 1);
    assert!(speech.cancelled.load(Ordering::SeqCst) >= 1);
    runtime.stop().await.unwrap();
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
}

/// worker 任务本身 panic 之后，停机必须出声。
///
/// 先说清楚这条**不**覆盖什么：工具与 provider 的 panic 发生在 process_wake
/// 里那个嵌套 `tokio::spawn` 中，它的 JoinError 已经在 mod.rs 的
/// `joined.map_err(..)` 处接住并变成 participant_handler_failed——那条路径
/// 一直是通的，正是 crates/toolloop/src/control.rs 开头所依据的接管点。
///
/// 缺口在 agent run **之外**的那半个 worker 循环：journal 落盘、帧捕获、
/// 队列记账、终态处理。这些直接在 worker 任务里 await，panic 会打到 worker
/// 自己的任务边界。而 `stop()` 原本写的是
///
/// ```ignore
/// if tokio::time::timeout(STOP_WORKER_SETTLE, &mut worker).await.is_err() { .. }
/// ```
///
/// 只判超时。worker 已 panic 时 `&mut worker` 立刻就绪，timeout 返回的是
/// `Ok(Err(JoinError))`——`.is_err()` 为 false，那个 JoinError 连绑定都没有，
/// 停机径直走到 Stopped 与 `Ok(())`。真实后果是参与者从 panic 那一刻起不再
/// 处理任何唤醒，运行期间无人发现，而停机报告成功。
///
/// 这里用 journal 的 append 作钩子，因为它是 worker 循环里最早、且必然被走到
/// 的那个 await（process_item 中，早于 process_wake）。
///
/// 测试输出里会出现一条 fixture 的 panic 回溯，那是被测现象本身，不是失败。
#[tokio::test]
async fn worker_panic_surfaces_as_failure_at_stop() {
    let agent = TestAgent::new(0);
    let (runtime, source, journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let mut failures = runtime.subscribe_failures();
    source.set_chats(vec![chat_input(50, "Alice", "@Bot boom")]);
    journal.arm_panic();
    runtime
        .ingest_backend_event(chat_event("91", 1, "Alice", "@Bot boom"))
        .unwrap();
    // fixture 先更新计数再 panic，所以这一句返回时 panic 已经发生在 worker
    // 任务里；随后让出几次，确保它完成解栈——不靠定时等待。
    journal.wait_for_entries(1).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }

    runtime.stop().await.unwrap();

    let mut codes = Vec::new();
    while let Ok(failure) = failures.try_recv() {
        codes.push(failure.code);
    }
    assert!(
        codes
            .iter()
            .any(|code| code == "participant_worker_panicked"),
        "停机必须报告 worker 的 panic，实际收到 {codes:?}"
    );
}

/// 工具与 provider 的 panic 止步于嵌套任务边界，不会带走 worker。
///
/// 这是上一条测试的另一半，也是 crates/toolloop/src/control.rs 删掉循环内
/// panic 隔离时所依据的那条契约：「让它照常传播，调用方的任务边界会把它接成
/// JoinError」。依据必须有测试守着，否则哪天 process_wake 不再 spawn，循环
/// 里没有捕获、外面也没有边界，panic 会一路打死 worker 而无人察觉。
///
/// 一并钉住两件事：worker 活下来（没有 participant_worker_panicked），以及
/// panic 被归到 participant_handler_failed。后者与普通 handler 失败同码——
/// 已知，暂不改：改它要先定「panic 在失败分类里算哪一类」。
///
/// 测试输出里的 panic 回溯同样是被测现象本身。
#[tokio::test]
async fn agent_run_panic_stops_at_the_nested_task_boundary() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let mut failures = runtime.subscribe_failures();
    agent.arm_panic();
    source.set_chats(vec![chat_input(50, "Alice", "@Bot boom")]);
    runtime
        .ingest_backend_event(chat_event("92", 1, "Alice", "@Bot boom"))
        .unwrap();
    wait_for_request(&agent, 1).await;

    let failure = tokio::time::timeout(Duration::from_secs(2), failures.recv())
        .await
        .expect("嵌套边界必须在超时内报出这次 panic")
        .expect("失败流仍在");
    assert_eq!(failure.code, "participant_handler_failed");

    runtime.stop().await.unwrap();
    let mut codes = Vec::new();
    while let Ok(failure) = failures.try_recv() {
        codes.push(failure.code);
    }
    assert!(
        !codes
            .iter()
            .any(|code| code == "participant_worker_panicked"),
        "worker 不该被工具的 panic 带走，实际收到 {codes:?}"
    );
}

#[tokio::test]
async fn concurrent_stop_uses_one_cleanup_and_completion_owner() {
    let agent = TestAgent::new(0);
    let (runtime, _source, _journal, speech, motor, backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let gate = speech.gate_cleanup();

    let first_runtime = Arc::clone(&runtime);
    let handle = tokio::runtime::Handle::current();
    let first = tokio::task::spawn_blocking(move || handle.block_on(first_runtime.stop()));
    gate.wait_started().await;

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (done_tx, mut done_rx) = tokio::sync::oneshot::channel();
    let second_runtime = Arc::clone(&runtime);
    let second = tokio::spawn(async move {
        let _ = entered_tx.send(());
        let result = second_runtime.stop().await;
        let _ = done_tx.send(());
        result
    });
    entered_rx.await.unwrap();
    tokio::task::yield_now().await;
    assert!(done_rx.try_recv().is_err());

    gate.release();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert!(backend.subscription_closed());
    assert_eq!(speech.cancelled.load(Ordering::SeqCst), 1);
    assert_eq!(motor.releases.load(Ordering::SeqCst), 1);
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
}

#[tokio::test]
async fn backend_listener_surfaces_source_error_and_uses_injected_debug_clock() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    source.fail_context.store(true, Ordering::SeqCst);
    let mut failures = runtime.subscribe_failures();
    backend.emit(chat_event("81", 1, "Alice", "@Bot secret"));
    let failure = tokio::time::timeout(Duration::from_secs(2), failures.recv())
        .await
        .unwrap()
        .unwrap();
    // 入队路径的失败带 ingest: 前缀，与 worker 路径可辨（两条路径的致命判据
    // 相同，但排障时必须知道是谁先报的）。
    assert_eq!(failure.code, "ingest:opening_frame_source_failed");
    assert!(!failure.summary.contains("secret"));
    let debug = runtime.debug_snapshot();
    assert_eq!(debug.recent_failures[0].at, "2026-08-03T00:00:00Z");
    assert!(!serde_json::to_string(&*debug).unwrap().contains("secret"));
    // 曾断言 Faulted，实测证伪：同伴在游戏里死一次，入队侧的 source 读取即
    // 失败，按旧行为整个同伴永久失聪。oracle 的 #recordFailure 只落盘不改
    // 生命周期（runtime.ts:552-557），worker 路径也把 Source 归为可恢复；
    // 本断言随致命判据统一而改为 Running。
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Running
    );
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn terminal_path_releases_queued_trigger_retention() {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, _speech, _motor, backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    let first = chat_input(501, "Alice", "@Bot terminal first");
    source.set_chats(vec![first.clone()]);
    runtime
        .ingest_backend_event(chat_event(
            "terminal-first",
            1,
            "Alice",
            "@Bot terminal first",
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;

    let second = chat_input(502, "Bob", "@Bot terminal queued");
    source.set_chats(vec![first, second]);
    runtime
        .ingest_backend_event(chat_event(
            "terminal-queued",
            1,
            "Bob",
            "@Bot terminal queued",
        ))
        .unwrap();
    assert_eq!(source.retained_count(), 1);

    backend.emit(lifecycle_event(
        "terminal-release",
        BackendLifecyclePayload::Stopped {
            reason: "release queued trigger".to_owned(),
        },
    ));
    wait_for_lifecycle(
        &runtime,
        mineintent_middle::participant::ParticipantLifecycle::Stopped,
    )
    .await;
    assert_eq!(source.retained_count(), 0);
    assert_eq!(source.retain_calls(), 2);
    assert_eq!(source.release_calls(), 2);
    assert!(source.release_all_calls() >= 1);
    runtime.stop().await.unwrap();
}

async fn assert_backend_terminal_event(
    payload: BackendLifecyclePayload,
    expected: mineintent_middle::participant::ParticipantLifecycle,
) {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, speech, motor, backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    source.set_chats(vec![chat_input(300, "Alice", "@Bot terminal")]);
    runtime
        .ingest_backend_event(chat_event("terminal-trigger", 1, "Alice", "@Bot terminal"))
        .unwrap();
    wait_for_request(&agent, 1).await;
    let releases_before = motor.releases.load(Ordering::SeqCst);
    let speech_cancels_before = speech.cancelled.load(Ordering::SeqCst);

    backend.emit(lifecycle_event("terminal-event", payload));
    assert!(motor.releases.load(Ordering::SeqCst) > releases_before);
    assert!(speech.cancelled.load(Ordering::SeqCst) > speech_cancels_before);
    assert_eq!(backend.subscription_unsubscribes(), 0);
    wait_for_lifecycle(&runtime, expected).await;
    assert_eq!(backend.subscription_unsubscribes(), 1);
    runtime.stop().await.unwrap();
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
    assert_eq!(backend.subscription_unsubscribes(), 1);
}

#[tokio::test]
async fn backend_terminal_events_teardown_after_journal_and_stop_is_bounded() {
    assert_backend_terminal_event(
        BackendLifecyclePayload::Stopped {
            reason: "backend stopped".to_owned(),
        },
        mineintent_middle::participant::ParticipantLifecycle::Stopped,
    )
    .await;
    assert_backend_terminal_event(
        BackendLifecyclePayload::Faulted {
            failure: BackendFailure {
                code: BackendFailureCode::ProtocolError,
                message: "protocol failure".to_owned(),
                retryable: false,
            },
        },
        mineintent_middle::participant::ParticipantLifecycle::Faulted,
    )
    .await;

    let agent = TestAgent::new(1);
    let (runtime, source, journal, speech, motor, backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    source.set_chats(vec![chat_input(301, "Alice", "@Bot gated terminal")]);
    runtime
        .ingest_backend_event(chat_event(
            "terminal-gated-trigger",
            1,
            "Alice",
            "@Bot gated terminal",
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;
    journal.set_gate(true);
    backend.emit(lifecycle_event(
        "terminal-gated",
        BackendLifecyclePayload::Stopped {
            reason: "journal gate".to_owned(),
        },
    ));
    assert!(motor.releases.load(Ordering::SeqCst) >= 1);
    assert!(speech.cancelled.load(Ordering::SeqCst) >= 1);
    tokio::time::timeout(Duration::from_secs(1), runtime.stop())
        .await
        .expect("stop must abort a terminal journal within the bounded fallback")
        .unwrap();
    assert_eq!(backend.subscription_unsubscribes(), 1);
    journal.set_gate(false);
}

#[tokio::test]
async fn backend_connection_closed_keeps_running_until_reconnect_or_terminal() {
    let agent = TestAgent::new(1);
    let (runtime, source, journal, speech, motor, backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    source.set_chats(vec![chat_input(400, "Alice", "@Bot before close")]);
    runtime
        .ingest_backend_event(chat_event("close-trigger", 1, "Alice", "@Bot before close"))
        .unwrap();
    wait_for_request(&agent, 1).await;
    let initial_journal_entries = journal.entries.lock().unwrap().len();
    let releases_before = motor.releases.load(Ordering::SeqCst);
    let speech_before = speech.cancelled.load(Ordering::SeqCst);

    backend.emit(scoped_lifecycle_event(
        "retryable-close",
        "process-test",
        1,
        "attempt-test",
        "world-test",
        Some("minecraft:overworld"),
        BackendLifecyclePayload::ConnectionClosed {
            close: BackendClose {
                epoch: 1,
                at: "2026-08-03T00:06:01Z".to_owned(),
                code: "transport_reset".to_owned(),
                retryable: true,
                deliberate: false,
                kick: None,
                error: None,
                end_reason: None,
            },
        },
    ));
    assert!(motor.releases.load(Ordering::SeqCst) > releases_before);
    assert!(speech.cancelled.load(Ordering::SeqCst) > speech_before);
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Running
    );
    assert_eq!(backend.subscription_unsubscribes(), 0);
    tokio::time::timeout(
        Duration::from_secs(2),
        journal.wait_for_entries(initial_journal_entries + 1),
    )
    .await
    .unwrap();
    let entries_after_close = journal.entries.lock().unwrap().len();

    assert!(matches!(
        runtime.ingest_backend_event(scoped_lifecycle_event(
            "wrong-reconnect-attempt",
            "process-test",
            1,
            "attempt-wrong",
            "world-test",
            Some("minecraft:overworld"),
            BackendLifecyclePayload::ReconnectScheduled {
                attempt: 2,
                retry_at: "2026-08-03T00:06:02Z".to_owned(),
                close_code: "transport_reset".to_owned(),
            },
        )),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert!(matches!(
        runtime.ingest_backend_event(scoped_lifecycle_event(
            "wrong-terminal-attempt",
            "process-test",
            1,
            "attempt-wrong",
            "world-test",
            Some("minecraft:overworld"),
            BackendLifecyclePayload::Faulted {
                failure: BackendFailure {
                    code: BackendFailureCode::ProtocolError,
                    message: "wrong attempt must not fault".to_owned(),
                    retryable: false,
                },
            },
        )),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert!(matches!(
        runtime.ingest_backend_event(scoped_lifecycle_event(
            "same-epoch-new-attempt",
            "process-test",
            1,
            "attempt-two",
            "world-test",
            Some("minecraft:overworld"),
            BackendLifecyclePayload::ConnectionRequested { attempt: 2 },
        )),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert!(matches!(
        runtime.ingest_backend_event(scoped_lifecycle_event(
            "higher-epoch-reused-attempt",
            "process-test",
            2,
            "attempt-test",
            "world-test",
            Some("minecraft:overworld"),
            BackendLifecyclePayload::ConnectionRequested { attempt: 2 },
        )),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert_eq!(runtime.current_scope(), None);
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Running
    );
    assert_eq!(journal.entries.lock().unwrap().len(), entries_after_close);

    source.set_chats(vec![chat_input(401, "Alice", "@Bot stale after close")]);
    assert!(matches!(
        runtime.ingest_backend_event(scoped_chat_event(
            "stale-after-close",
            "process-test",
            1,
            "world-test",
            "minecraft:overworld",
            "Alice",
            "@Bot stale after close",
        )),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert_eq!(agent.requests.lock().unwrap().len(), 1);
    assert_eq!(
        journal.entries.lock().unwrap().len(),
        initial_journal_entries + 1
    );

    backend.emit(scoped_lifecycle_event(
        "reconnect-scheduled",
        "process-test",
        1,
        "attempt-test",
        "world-test",
        Some("minecraft:overworld"),
        BackendLifecyclePayload::ReconnectScheduled {
            attempt: 2,
            retry_at: "2026-08-03T00:06:02Z".to_owned(),
            close_code: "transport_reset".to_owned(),
        },
    ));
    tokio::time::timeout(
        Duration::from_secs(2),
        journal.wait_for_entries(initial_journal_entries + 2),
    )
    .await
    .unwrap();
    assert_eq!(agent.requests.lock().unwrap().len(), 1);
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Running
    );
    assert_eq!(backend.subscription_unsubscribes(), 0);

    backend.emit(scoped_lifecycle_event(
        "connection-requested-2",
        "process-test",
        2,
        "attempt-two",
        "world-test",
        Some("minecraft:overworld"),
        BackendLifecyclePayload::ConnectionRequested { attempt: 2 },
    ));
    backend.emit(scoped_lifecycle_event(
        "logged-in-2",
        "process-test",
        2,
        "attempt-two",
        "world-test",
        Some("minecraft:overworld"),
        BackendLifecyclePayload::LoggedIn {
            version: "1.21".to_owned(),
            dimension: "minecraft:overworld".to_owned(),
        },
    ));
    backend.emit(scoped_lifecycle_event(
        "ready-2",
        "process-test",
        2,
        "attempt-two",
        "world-test",
        Some("minecraft:overworld"),
        BackendLifecyclePayload::Ready {
            snapshot_revision: 2,
        },
    ));
    source.set_chats(vec![chat_input(402, "Alice", "@Bot after reconnect")]);
    runtime
        .ingest_backend_event(scoped_chat_event_at_attempt(
            "chat-after-reconnect",
            "process-test",
            2,
            "attempt-two",
            "world-test",
            "minecraft:overworld",
            "Alice",
            "@Bot after reconnect",
            &chat_input(402, "Alice", "@Bot after reconnect").message.at,
        ))
        .unwrap();
    wait_for_request(&agent, 2).await;
    assert_eq!(
        runtime.current_scope(),
        Some(scope(2, "minecraft:overworld"))
    );
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Running
    );
    assert_eq!(backend.subscription_unsubscribes(), 0);

    backend.emit(scoped_lifecycle_event(
        "fatal-after-reconnect",
        "process-test",
        2,
        "attempt-two",
        "world-test",
        Some("minecraft:overworld"),
        BackendLifecyclePayload::Faulted {
            failure: BackendFailure {
                code: BackendFailureCode::ProtocolError,
                message: "fatal after reconnect".to_owned(),
                retryable: false,
            },
        },
    ));
    wait_for_lifecycle(
        &runtime,
        mineintent_middle::participant::ParticipantLifecycle::Faulted,
    )
    .await;
    assert_eq!(backend.subscription_unsubscribes(), 1);
    runtime.stop().await.unwrap();

    let second_agent = TestAgent::new(0);
    let (second_runtime, _source, second_journal, second_speech, second_motor, second_backend) =
        runtime_parts(Arc::clone(&second_agent));
    second_runtime.start_worker().unwrap();
    second_backend.emit(lifecycle_event(
        "deliberate-close-requested",
        BackendLifecyclePayload::ConnectionRequested { attempt: 1 },
    ));
    second_backend.emit(lifecycle_event(
        "deliberate-close-ready",
        BackendLifecyclePayload::Ready {
            snapshot_revision: 1,
        },
    ));
    tokio::time::timeout(Duration::from_secs(2), second_journal.wait_for_entries(2))
        .await
        .unwrap();
    assert_eq!(
        second_runtime.current_scope(),
        Some(scope(1, "minecraft:overworld"))
    );
    let second_releases_before = second_motor.releases.load(Ordering::SeqCst);
    let second_speech_before = second_speech.cancelled.load(Ordering::SeqCst);
    second_backend.emit(scoped_lifecycle_event(
        "deliberate-close",
        "process-test",
        1,
        "attempt-test",
        "world-test",
        Some("minecraft:overworld"),
        BackendLifecyclePayload::ConnectionClosed {
            close: BackendClose {
                epoch: 1,
                at: "2026-08-03T00:06:03Z".to_owned(),
                code: "requested_disconnect".to_owned(),
                retryable: false,
                deliberate: true,
                kick: None,
                error: None,
                end_reason: None,
            },
        },
    ));
    assert!(second_motor.releases.load(Ordering::SeqCst) > second_releases_before);
    assert!(second_speech.cancelled.load(Ordering::SeqCst) > second_speech_before);
    assert_eq!(second_runtime.current_scope(), None);
    assert_eq!(
        second_runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Running
    );
    assert_eq!(second_backend.subscription_unsubscribes(), 0);
    tokio::time::timeout(Duration::from_secs(2), second_journal.wait_for_entries(3))
        .await
        .unwrap();
    second_backend.emit(lifecycle_event(
        "stopped-after-deliberate-close",
        BackendLifecyclePayload::Stopped {
            reason: "backend stop confirmed".to_owned(),
        },
    ));
    wait_for_lifecycle(
        &second_runtime,
        mineintent_middle::participant::ParticipantLifecycle::Stopped,
    )
    .await;
    assert_eq!(second_backend.subscription_unsubscribes(), 1);
    second_runtime.stop().await.unwrap();
}
