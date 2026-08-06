//! 有界队列准入：journal 收录、FIFO 票序、饱和与溢出。
//!
//! fixture 与辅助函数在父文件，经 `use super::*` 复用。

use super::*;

#[tokio::test]
async fn active_run_body_drain_precedes_queued_opening_without_fact_replay() {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    let first = chat_input_at(120, "Alice", "@Bot seam first", "2026-08-03T00:11:00Z");
    source.set_chats(vec![first.clone()]);
    runtime
        .ingest_backend_event(scoped_chat_event_at(
            "seam-first",
            "process-test",
            1,
            "world-test",
            "minecraft:overworld",
            "Alice",
            "@Bot seam first",
            &first.message.at,
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;

    let run_scope = runtime.current_scope().expect("first run scope is active");
    let generation = runtime.current_generation();
    runtime
        .emit_internal(internal_fact("seam-damage", &run_scope, "self_hurt"))
        .unwrap();

    let second = chat_input_at(121, "Bob", "@Bot seam second", "2026-08-03T00:11:01Z");
    source.set_chats(vec![first, second]);
    let admission = runtime
        .ingest_backend_event(scoped_chat_event_at(
            "seam-second",
            "process-test",
            1,
            "world-test",
            "minecraft:overworld",
            "Bob",
            "@Bot seam second",
            "2026-08-03T00:11:01Z",
        ))
        .unwrap();
    assert!(matches!(admission, ParticipantAdmission::WakeQueued { .. }));
    assert_eq!(agent.requests.lock().unwrap().len(), 1);

    let frame_source: Arc<dyn ParticipantFrameSource> = source.clone();
    let observation = ParticipantObservationAfterSource::new(
        frame_source,
        runtime.fact_owner(),
        run_scope.clone(),
        generation,
        "seam-first",
    );
    let checks = Arc::new(AtomicUsize::new(0));
    let signal = NeverCancelled;
    let guard = RealScopeGuard {
        checks: Arc::clone(&checks),
    };
    let deadline = Deadline::after(std::time::Instant::now(), Duration::from_secs(1)).unwrap();
    let first_observation = observation
        .observe_after(
            observation_invocation("seam-body-failure"),
            serde_json::json!({"status": "failed"}),
            CapabilityExecutionContext::new(
                &run_scope.world_id,
                "seam-first",
                ExecutionControl::new(&signal, deadline),
                &guard,
            ),
        )
        .await
        .unwrap()
        .expect("body ordinary failure still has observation");
    assert!(first_observation["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| { event["type"] == "self_hurt" }));

    let signal = NeverCancelled;
    let guard = RealScopeGuard {
        checks: Arc::clone(&checks),
    };
    let deadline = Deadline::after(std::time::Instant::now(), Duration::from_secs(1)).unwrap();
    let second_observation = observation
        .observe_after(
            observation_invocation("seam-body-second"),
            serde_json::json!({"status": "failed"}),
            CapabilityExecutionContext::new(
                &run_scope.world_id,
                "seam-first",
                ExecutionControl::new(&signal, deadline),
                &guard,
            ),
        )
        .await
        .unwrap()
        .expect("second body sample keeps passive frame facts");
    assert!(second_observation.get("events").is_none());

    agent.release();
    wait_for_request(&agent, 2).await;
    let requests = agent.requests.lock().unwrap();
    let second_opening = requests[1]
        .context
        .frame
        .events
        .as_ref()
        .expect("second opening keeps its trigger event");
    assert!(!second_opening.iter().any(|event| {
        matches!(
            event,
            mineintent_contracts::agent::AgentEventV5::Summary { event_type, .. }
                if event_type == "self_hurt"
        )
    }));
    drop(requests);
    assert_eq!(agent.texts(), vec!["@Bot seam first", "@Bot seam second"]);
    assert_eq!(runtime.current_scope(), Some(run_scope));
    assert_eq!(runtime.current_generation(), generation);
    assert_eq!(source.retained_count(), 0);
    assert!(checks.load(Ordering::SeqCst) >= 6);
    runtime.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn body_observation_progresses_while_full_control_admission_waits() {
    let agent = TestAgent::new(1);
    let (runtime, source, journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    let first = chat_input_at(130, "Alice", "@Bot deadlock first", "2026-08-03T00:12:00Z");
    source.set_chats(vec![first.clone()]);
    runtime
        .ingest_backend_event(scoped_chat_event_at(
            "deadlock-first",
            "process-test",
            1,
            "world-test",
            "minecraft:overworld",
            "Alice",
            "@Bot deadlock first",
            &first.message.at,
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;

    let run_scope = runtime.current_scope().expect("first run scope is active");
    let generation = runtime.current_generation();
    let queued: Vec<_> = (0..TEST_CONTROL_CAPACITY)
        .map(|index| {
            chat_input_at(
                131 + index as u64,
                "Alice",
                &format!("@Bot deadlock queued {index}"),
                &format!("2026-08-03T00:12:{:02}Z", index + 1),
            )
        })
        .collect();
    let blocked_chat = chat_input_at(140, "Bob", "@Bot deadlock blocked", "2026-08-03T00:12:11Z");
    let mut chats = vec![first.clone()];
    chats.extend(queued.iter().cloned());
    chats.push(blocked_chat.clone());
    source.set_chats(chats);

    runtime
        .emit_internal(internal_fact("deadlock-body-fact", &run_scope, "self_hurt"))
        .unwrap();
    for (index, chat) in queued.iter().enumerate() {
        runtime
            .ingest_backend_event(scoped_chat_event_at(
                &format!("deadlock-queued-{index}"),
                "process-test",
                1,
                "world-test",
                "minecraft:overworld",
                &chat.message.username,
                &chat.message.text,
                &chat.message.at,
            ))
            .unwrap();
    }
    assert_eq!(runtime.queue_counts_for_test().1, TEST_CONTROL_CAPACITY);

    let blocked_runtime = Arc::clone(&runtime);
    let blocked_event = scoped_chat_event_at(
        "deadlock-blocked",
        "process-test",
        1,
        "world-test",
        "minecraft:overworld",
        &blocked_chat.message.username,
        &blocked_chat.message.text,
        &blocked_chat.message.at,
    );
    let blocked =
        tokio::task::spawn_blocking(move || blocked_runtime.ingest_backend_event(blocked_event));
    tokio::time::timeout(
        Duration::from_secs(2),
        runtime.wait_for_queue_waiters_for_test(1),
    )
    .await
    .expect("the next addressed admission must wait for control capacity");

    let frame_source: Arc<dyn ParticipantFrameSource> = source.clone();
    let observation = Arc::new(ParticipantObservationAfterSource::new(
        frame_source,
        runtime.fact_owner(),
        run_scope.clone(),
        generation,
        "deadlock-first",
    ));
    let observation_handle = tokio::runtime::Handle::current();
    let mut observation_task = tokio::task::spawn_blocking({
        let observation = Arc::clone(&observation);
        move || {
            let signal = NeverCancelled;
            let checks = Arc::new(AtomicUsize::new(0));
            let guard = RealScopeGuard { checks };
            let deadline =
                Deadline::after(std::time::Instant::now(), Duration::from_secs(1)).unwrap();
            observation_handle.block_on(observation.observe_after(
                observation_invocation("deadlock-body"),
                serde_json::json!({"status": "failed"}),
                CapabilityExecutionContext::new(
                    &run_scope.world_id,
                    "deadlock-first",
                    ExecutionControl::new(&signal, deadline),
                    &guard,
                ),
            ))
        }
    });
    let body_result = tokio::time::timeout(Duration::from_secs(2), &mut observation_task).await;
    assert!(
        !blocked.is_finished(),
        "the full control lane must remain blocked while the active run is held"
    );

    agent.release();
    let blocked_admission = tokio::time::timeout(Duration::from_secs(2), blocked)
        .await
        .expect("blocked admission must complete after the active run releases capacity")
        .expect("blocked admission task must not panic")
        .expect("blocked admission must retain its structured success boundary");
    assert!(matches!(
        blocked_admission,
        ParticipantAdmission::WakeQueued { .. }
    ));

    let body = body_result
        .expect("body observation must finish while the producer waits")
        .expect("body observation task must not panic")
        .expect("body observation must succeed")
        .expect("body observation must return a direct frame");
    assert!(body["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["type"] == "self_hurt"));

    wait_for_request(&agent, TEST_CONTROL_CAPACITY + 2).await;
    let expected_texts = std::iter::once(first.message.text.clone())
        .chain(queued.iter().map(|chat| chat.message.text.clone()))
        .chain(std::iter::once(blocked_chat.message.text.clone()))
        .collect::<Vec<_>>();
    assert_eq!(agent.texts(), expected_texts);

    let payloads = journal.payloads();
    let wake_ids = (0..TEST_CONTROL_CAPACITY)
        .map(|index| format!("deadlock-queued-{index}"))
        .chain(std::iter::once("deadlock-blocked".to_owned()))
        .collect::<Vec<_>>();
    let wake_tickets = wake_ids
        .iter()
        .map(|id| {
            payloads
                .iter()
                .find(|payload| payload.get("id").and_then(serde_json::Value::as_str) == Some(id))
                .map(admission_ticket)
                .expect("every queued wake must be journaled")
        })
        .collect::<Vec<_>>();
    assert!(wake_tickets
        .windows(2)
        .all(|tickets| tickets[0] < tickets[1]));

    runtime.stop().await.unwrap();
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
    assert_eq!(source.retained_count(), 0);
    assert_eq!(source.retain_calls(), TEST_CONTROL_CAPACITY + 2);
    assert_eq!(source.release_calls(), TEST_CONTROL_CAPACITY + 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn waiting_old_wake_is_ignored_after_scope_generation_changes() {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    let first = chat_input(150, "Alice", "@Bot stale wait first");
    source.set_chats(vec![first.clone()]);
    runtime
        .ingest_backend_event(scoped_chat_event_at(
            "stale-wait-first",
            "process-test",
            1,
            "world-test",
            "minecraft:overworld",
            "Alice",
            &first.message.text,
            &first.message.at,
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;

    let queued: Vec<_> = (0..TEST_CONTROL_CAPACITY)
        .map(|index| {
            chat_input(
                151 + index as u64,
                "Alice",
                &format!("@Bot stale wait {index}"),
            )
        })
        .collect();
    for (index, chat) in queued.iter().enumerate() {
        runtime
            .ingest_backend_event(scoped_chat_event_at(
                &format!("stale-wait-queued-{index}"),
                "process-test",
                1,
                "world-test",
                "minecraft:overworld",
                &chat.message.username,
                &chat.message.text,
                &chat.message.at,
            ))
            .unwrap();
    }
    assert_eq!(runtime.queue_counts_for_test().1, TEST_CONTROL_CAPACITY);

    let old_runtime = Arc::clone(&runtime);
    let old_chat = chat_input(160, "Bob", "@Bot stale wait blocked");
    let old_event = scoped_chat_event_at(
        "stale-wait-blocked",
        "process-test",
        1,
        "world-test",
        "minecraft:overworld",
        &old_chat.message.username,
        &old_chat.message.text,
        &old_chat.message.at,
    );
    let old_producer =
        tokio::task::spawn_blocking(move || old_runtime.ingest_backend_event(old_event));
    tokio::time::timeout(
        Duration::from_secs(2),
        runtime.wait_for_queue_waiters_for_test(1),
    )
    .await
    .expect("old wake producer must reach the bounded queue wait");

    let new_scope = scope(2, "minecraft:nether");
    let generation = runtime.current_generation();
    let scope_runtime = Arc::clone(&runtime);
    let scope_event = ParticipantInternalEvent::ScopeChanged {
        id: "stale-wait-scope-change".to_owned(),
        occurred_at: "2026-08-03T00:13:00Z".to_owned(),
        scope: new_scope.clone(),
        reason: "scope changes while an old wake waits for capacity".to_owned(),
    };
    let scope_producer =
        tokio::task::spawn_blocking(move || scope_runtime.emit_internal(scope_event));
    // Deterministic synchronization: wait until the scope-change producer has
    // published the invalidation (generation bump), which happens while it
    // still holds the admission serial. The old wake cannot resolve before
    // that serial is released, so this proves the scope/generation change was
    // applied while the old admission was still pending. The previous
    // `wait_for_queue_waiters_for_test(2)` assertion was a scheduling
    // assumption: the worker may pop a stale control item and wake the old
    // producer before the scope producer registers as the second queue
    // waiter, so the transient "two waiters" count is not a protocol fact.
    tokio::time::timeout(
        Duration::from_secs(2),
        runtime.wait_for_generation_for_test(generation + 1),
    )
    .await
    .expect("scope change must be published while the old wake waits for capacity");

    let old_admission = tokio::time::timeout(Duration::from_secs(2), old_producer)
        .await
        .expect("old waiting producer must complete after scope cancellation")
        .expect("old producer task must not panic")
        .expect("old producer must keep the structured admission boundary");
    assert!(matches!(old_admission, ParticipantAdmission::Ignored));

    let scope_admission = tokio::time::timeout(Duration::from_secs(2), scope_producer)
        .await
        .expect("scope change producer must complete after the stale ticket is skipped")
        .expect("scope producer task must not panic")
        .expect("scope change admission must succeed");
    assert!(matches!(scope_admission, ParticipantAdmission::Recorded));
    assert_eq!(runtime.current_scope(), Some(new_scope));

    runtime.stop().await.unwrap();
    assert_eq!(source.retained_count(), 0);
    assert_eq!(source.retain_calls(), TEST_CONTROL_CAPACITY + 2);
}

/// journal 只收产品事实：可重建的普通事实只计数不落盘，被指名叫醒照记。
///
/// 实测背景：收窄前 100 秒实跑写了 36,764 条 `participant.event` 信封，
/// 对应的产品事实只有 4 条，且信封 payload 不含事实内容。oracle
/// （TS runtime.ts 的 12 个写入点）从来没有「每条摄入事件记一笔」。
#[tokio::test]
async fn ordinary_facts_are_counted_not_journalled_while_wake_is_recorded() {
    let agent = TestAgent::new(0);
    let (runtime, source, journal, _speech, _motor, backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let current = scope(1, "minecraft:overworld");

    // 保持在 ordinary lane 容量内：超出部分会按 NEW-11 被丢弃并留下遗漏
    // 标记，那是另一条已裁定的语义，不该混进本用例。
    for index in 0..8 {
        runtime
            .emit_internal(internal_fact(
                &format!("ordinary-{index}"),
                &current,
                "entity",
            ))
            .unwrap();
    }
    for index in 0..3 {
        runtime
            .emit_internal(internal_fact(&format!("block-{index}"), &current, "block"))
            .unwrap();
    }

    source.set_chats(vec![chat_input(20, "Alice", "@Bot 你好")]);
    backend.emit(chat_event("wake-1", 1, "Alice", "@Bot 你好"));
    wait_for_request(&agent, 1).await;

    let types = journal.entries.lock().unwrap().clone();
    // participant.event 不为零是对的：scope 迁移这类结构事实仍然要落盘。
    // 关键是它不再与摄入量成正比——27 条普通事实没有换来 27 条记录。
    let structural = types
        .iter()
        .filter(|entry| *entry == "participant.event")
        .count();
    assert!(
        structural <= 2,
        "结构事实应稀疏；27 条普通事实后 participant.event 写了 {structural} 条：{types:?}"
    );
    assert!(
        types.iter().any(|entry| entry == "player.chat.received"),
        "被指名叫醒是产品事实，必须进 journal，实际写入：{types:?}"
    );
    assert!(
        types
            .iter()
            .any(|entry| entry == "model.decision.completed"),
        "做完决定是产品事实（oracle runtime.ts:303），必须进 journal：{types:?}"
    );

    let counts = runtime.ingest_counters().snapshot();
    assert_eq!(counts.get("entity").copied(), Some(8));
    assert_eq!(counts.get("block").copied(), Some(3));
    assert!(
        !counts.contains_key("player_chat"),
        "进了 journal 的事实不该再计入未落盘计数：{counts:?}"
    );

    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn journal_gate_serializes_later_chat_before_model() {
    let agent = TestAgent::new(0);
    let (runtime, source, journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    journal.set_gate(false);
    let first = chat_input(20, "Alice", "@Bot journal one");
    source.set_chats(vec![first]);
    journal.set_gate(true);
    runtime
        .ingest_backend_event(chat_event("41", 1, "Alice", "@Bot journal one"))
        .unwrap();
    journal.wait_for_entries(1).await;
    let second = chat_input(21, "Bob", "@Bot journal two");
    source.set_chats(vec![chat_input(20, "Alice", "@Bot journal one"), second]);
    runtime
        .ingest_backend_event(chat_event("42", 1, "Bob", "@Bot journal two"))
        .unwrap();
    assert!(agent.requests.lock().unwrap().is_empty());
    journal.set_gate(false);
    wait_for_request(&agent, 2).await;
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn request_stop_wakes_full_control_and_overflow_producer() {
    let agent = TestAgent::new(0);
    let (runtime, source, journal, _speech, _motor, backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let mut failures = runtime.subscribe_failures();
    let current = scope(1, "minecraft:overworld");
    hold_worker_on_second_journal(&runtime, &journal, &current).await;

    fill_ordinary_lane(&runtime, &current, "stop-saturation");
    for marker_index in 0..TEST_OVERFLOW_CAPACITY {
        while runtime.queue_counts_for_test().0 < TEST_ORDINARY_CAPACITY {
            runtime
                .emit_internal(internal_fact(
                    &format!("stop-fill-{marker_index}"),
                    &current,
                    "ordinary_fact",
                ))
                .unwrap();
        }
        runtime
            .emit_internal(internal_fact(
                &format!("stop-loss-{marker_index}"),
                &current,
                "ordinary_loss_candidate",
            ))
            .unwrap();
        let (_, _, overflow, _, _) = runtime.queue_counts_for_test();
        assert_eq!(overflow, marker_index + 1);
        if marker_index + 1 < TEST_OVERFLOW_CAPACITY {
            // 放行一条，并等 worker 真的消化掉它、重新停在闸门上：
            // hold 后闸门计数为 2，每放行一条就再 +1。
            runtime.worker_gate().allow(1);
            runtime
                .worker_gate()
                .wait_entered(3 + marker_index as u64)
                .await;
        }
    }

    for chat_index in 0..TEST_CONTROL_CAPACITY {
        let text = format!("@Bot control-{chat_index}");
        backend.emit(chat_event(
            &format!("control-{chat_index}"),
            1,
            "Alice",
            &text,
        ));
    }
    let (ordinary, control, overflow, terminal, waiting) = runtime.queue_counts_for_test();
    assert_eq!(ordinary, TEST_ORDINARY_CAPACITY);
    assert_eq!(control, TEST_CONTROL_CAPACITY);
    assert_eq!(overflow, TEST_OVERFLOW_CAPACITY);
    assert_eq!(terminal, 0);
    assert_eq!(waiting, 0);

    let producer_backend = Arc::clone(&backend);
    let producer = std::thread::spawn(move || {
        producer_backend.emit(chat_event(
            "control-blocked",
            1,
            "Bob",
            "@Bot control-blocked",
        ));
    });
    tokio::time::timeout(
        Duration::from_secs(2),
        runtime.wait_for_queue_waiters_for_test(1),
    )
    .await
    .expect("the extra control producer must reach the bounded queue wait");

    let callback = tokio::task::spawn_blocking(move || producer.join());
    let stop_runtime = Arc::clone(&runtime);
    let stop = tokio::task::spawn_blocking(move || stop_runtime.request_stop());
    let stopped = tokio::time::timeout(Duration::from_secs(2), stop)
        .await
        .expect("request_stop must not wait indefinitely for a full lane")
        .expect("request_stop task must not panic")
        .expect("request_stop must succeed");
    assert!(stopped);
    tokio::time::timeout(Duration::from_secs(2), callback)
        .await
        .expect("backend listener callback must return after queue cancellation")
        .expect("backend listener callback task must not panic")
        .expect("backend listener thread must not panic");
    assert!(matches!(
        failures.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(source.retain_calls(), TEST_CONTROL_CAPACITY + 1);
    assert_eq!(source.retained_count(), 0);
    assert_eq!(source.release_calls(), TEST_CONTROL_CAPACITY + 1);
    assert_ne!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Faulted
    );

    runtime.worker_gate().release_all();
    journal.set_gate(false);
    runtime.stop().await.unwrap();
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
}

#[tokio::test]
async fn old_scope_omission_marker_keeps_ticket_and_cannot_cross_generation() {
    let agent = TestAgent::new(0);
    let (runtime, source, journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let old_scope = scope(1, "minecraft:overworld");
    hold_worker_on_second_journal(&runtime, &journal, &old_scope).await;
    fill_ordinary_lane(&runtime, &old_scope, "marker");
    runtime
        .emit_internal(internal_fact(
            "first-loss",
            &old_scope,
            "old_scope_loss_candidate",
        ))
        .unwrap();
    assert_eq!(runtime.queue_counts_for_test().2, 1);

    let new_scope = scope(2, "minecraft:nether");
    runtime
        .emit_internal(ParticipantInternalEvent::ScopeChanged {
            id: "scope-to-nether".to_owned(),
            occurred_at: "2026-08-03T00:04:00Z".to_owned(),
            scope: new_scope.clone(),
            reason: "dimension transition after ordinary loss".to_owned(),
        })
        .unwrap();
    assert_eq!(runtime.queue_counts_for_test().2, 1);
    runtime.worker_gate().release_all();
    tokio::time::timeout(
        Duration::from_secs(2),
        journal.wait_for_payload_ids(&["participant-overflow-19", "scope-to-nether"]),
    )
    .await
    .expect("marker and transition must both reach the journal");

    let payloads = journal.payloads();
    let marker = payloads
        .iter()
        .find(|payload| {
            payload.get("eventType").and_then(serde_json::Value::as_str)
                == Some("participant_events_omitted")
        })
        .expect("the first loss marker remains journal-visible");
    let transition = payloads
        .iter()
        .find(|payload| {
            payload.get("id").and_then(serde_json::Value::as_str) == Some("scope-to-nether")
        })
        .expect("the scope transition remains journal-visible");
    assert!(admission_ticket(marker) < admission_ticket(transition));

    runtime.worker_gate().release_all();
    journal.set_gate(false);
    let current_chat = chat_input(700, "Alice", "@Bot after marker");
    source.set_chats(vec![current_chat.clone()]);
    runtime
        .ingest_backend_event(scoped_chat_event_at(
            "chat-after-marker",
            "process-test",
            2,
            "world-test",
            "minecraft:nether",
            "Alice",
            "@Bot after marker",
            &current_chat.message.at,
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;
    let request = agent.requests.lock().unwrap()[0].clone();
    assert!(
        !request.context.frame.events.as_ref().is_some_and(|events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    mineintent_contracts::agent::AgentEventV5::Summary { event_type, .. }
                        if event_type == "participant_events_omitted"
                )
            })
        })
    );
    assert_eq!(runtime.current_scope(), Some(new_scope));
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn lifecycle_controls_keep_ticket_fifo_when_ordinary_lane_is_full() {
    let agent = TestAgent::new(0);
    let (runtime, _source, journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let overworld = scope(1, "minecraft:overworld");
    hold_worker_on_second_journal(&runtime, &journal, &overworld).await;
    fill_ordinary_lane(&runtime, &overworld, "lifecycle");

    for event in [
        scoped_lifecycle_event(
            "connection-requested-lane",
            "process-test",
            1,
            "attempt-test",
            "world-test",
            Some("minecraft:overworld"),
            BackendLifecyclePayload::ConnectionRequested { attempt: 1 },
        ),
        scoped_lifecycle_event(
            "logged-in-lane",
            "process-test",
            1,
            "attempt-test",
            "world-test",
            Some("minecraft:overworld"),
            BackendLifecyclePayload::LoggedIn {
                version: "1.21".to_owned(),
                dimension: "minecraft:overworld".to_owned(),
            },
        ),
        scoped_lifecycle_event(
            "ready-lane",
            "process-test",
            1,
            "attempt-test",
            "world-test",
            Some("minecraft:overworld"),
            BackendLifecyclePayload::Ready {
                snapshot_revision: 1,
            },
        ),
        scoped_lifecycle_event(
            "dimension-changed-lane",
            "process-test",
            1,
            "attempt-test",
            "world-test",
            Some("minecraft:nether"),
            BackendLifecyclePayload::DimensionChanged {
                from: "minecraft:overworld".to_owned(),
                to: "minecraft:nether".to_owned(),
            },
        ),
    ] {
        runtime.ingest_backend_event(event).unwrap();
    }

    let (ordinary, control, overflow, terminal, waiting) = runtime.queue_counts_for_test();
    assert_eq!(ordinary, TEST_ORDINARY_CAPACITY);
    assert_eq!(control, 4);
    assert_eq!(overflow, 0);
    assert_eq!(terminal, 0);
    assert_eq!(waiting, 0);

    runtime.worker_gate().release_all();
    journal.set_gate(false);
    tokio::time::timeout(
        Duration::from_secs(2),
        journal.wait_for_payload_ids(&[
            "connection-requested-lane",
            "logged-in-lane",
            "ready-lane",
            "dimension-changed-lane",
        ]),
    )
    .await
    .expect("the complete lifecycle control batch must reach the journal");
    let lifecycle_ids: Vec<String> = journal
        .payloads()
        .iter()
        .filter_map(|payload| {
            let id = payload.get("id").and_then(serde_json::Value::as_str)?;
            id.ends_with("-lane").then(|| id.to_owned())
        })
        .collect();
    assert_eq!(
        lifecycle_ids,
        vec![
            "connection-requested-lane",
            "logged-in-lane",
            "ready-lane",
            "dimension-changed-lane",
        ]
    );
    let lifecycle_tickets: Vec<u64> = journal
        .payloads()
        .iter()
        .filter_map(|payload| {
            let id = payload.get("id").and_then(serde_json::Value::as_str)?;
            id.ends_with("-lane").then(|| admission_ticket(payload))
        })
        .collect();
    assert!(lifecycle_tickets
        .windows(2)
        .all(|tickets| tickets[0] < tickets[1]));
    let all_tickets: Vec<u64> = journal.payloads().iter().map(admission_ticket).collect();
    assert!(all_tickets
        .windows(2)
        .all(|tickets| tickets[0] < tickets[1]));
    assert!(!journal.payloads().iter().any(|payload| {
        payload.get("eventType").and_then(serde_json::Value::as_str)
            == Some("participant_events_omitted")
    }));
    assert_eq!(runtime.current_scope(), Some(scope(1, "minecraft:nether")));
    runtime.stop().await.unwrap();
}

/// 标记位耗尽之后，一条**可丢**的普通事实曾经会把生产者钉住——它落到有界队列
/// 的等待路径上，而它本来是允许被丢掉的。2026-08-05 实盘的四节点死锁里被钉住的
/// 正是后端派发线程；2026-08-06 那次「停机走完了进程不退出」也是同一条路。
///
/// 现在的语义：并进最新的那条标记。丢的是这一段丢失的位置精度，不是丢失本身。
#[tokio::test]
async fn a_droppable_fact_never_blocks_the_producer_once_markers_are_exhausted() {
    let agent = TestAgent::new(0);
    let (runtime, _source, journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let current = scope(1, "minecraft:overworld");
    hold_worker_on_second_journal(&runtime, &journal, &current).await;

    // 把 ordinary lane 与全部标记位都占满：每开一个新标记，都要先让 worker
    // 放行一条、使得下一次丢弃不能并进上一个标记。
    fill_ordinary_lane(&runtime, &current, "exhaust");
    for marker_index in 0..TEST_OVERFLOW_CAPACITY {
        while runtime.queue_counts_for_test().0 < TEST_ORDINARY_CAPACITY {
            runtime
                .emit_internal(internal_fact(
                    &format!("exhaust-fill-{marker_index}"),
                    &current,
                    "ordinary_fact",
                ))
                .unwrap();
        }
        runtime
            .emit_internal(internal_fact(
                &format!("exhaust-loss-{marker_index}"),
                &current,
                "ordinary_loss_candidate",
            ))
            .unwrap();
        assert_eq!(runtime.queue_counts_for_test().2, marker_index + 1);
        if marker_index + 1 < TEST_OVERFLOW_CAPACITY {
            runtime.worker_gate().allow(1);
            runtime
                .worker_gate()
                .wait_entered(3 + marker_index as u64)
                .await;
        }
    }

    while runtime.queue_counts_for_test().0 < TEST_ORDINARY_CAPACITY {
        runtime
            .emit_internal(internal_fact("exhaust-tail", &current, "ordinary_fact"))
            .unwrap();
    }
    let (_, _, overflow, _, _) = runtime.queue_counts_for_test();
    assert_eq!(overflow, TEST_OVERFLOW_CAPACITY, "标记位应当已经耗尽");

    // 再来一条可丢事实。它必须立刻返回；钉住生产者就是缺陷。
    let emitter = Arc::clone(&runtime);
    let emit_scope = current.clone();
    let admitted = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::task::spawn_blocking(move || {
            emitter.emit_internal(internal_fact(
                "beyond-markers",
                &emit_scope,
                "ordinary_loss_candidate",
            ))
        }),
    )
    .await
    .expect("标记位耗尽后，可丢事实把生产者钉住了")
    .expect("emit 任务应当正常结束");
    assert!(admitted.is_ok());

    let (_, _, overflow, _, waiting) = runtime.queue_counts_for_test();
    assert_eq!(overflow, TEST_OVERFLOW_CAPACITY, "不该凭空多出标记");
    assert_eq!(waiting, 0, "不该有生产者卡在准入上");

    runtime.worker_gate().release_all();
    runtime.stop().await.unwrap();
}
