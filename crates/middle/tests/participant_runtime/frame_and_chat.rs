//! v5 帧装配、观察漏出与聊天身份/排队。
//!
//! fixture 与辅助函数在父文件，经 `use super::*` 复用。

use super::*;

#[tokio::test]
async fn v5_frame_requires_light_deduplicates_trigger_and_preserves_armor() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let trigger = chat_input(5, "Alice", "@Bot frame");
    source.set_chats(vec![trigger.clone()]);
    source.capture.lock().unwrap().events = vec![AgentContextV5EventInput::PlayerChat {
        sequence: 5,
        message: trigger.message.clone(),
    }];
    runtime
        .ingest_backend_event(chat_event("21", 1, "Alice", "@Bot frame"))
        .unwrap();
    wait_for_request(&agent, 1).await;
    let request = agent.requests.lock().unwrap().remove(0);
    let wire = serde_json::to_value(&request.context).unwrap();
    assert_eq!(wire["frame"]["light"], 7);
    assert!(wire["frame"].get("viewport").is_none());
    assert!(wire["frame"]["status"].get("armor").is_none());
    assert_eq!(wire.to_string().matches("@Bot frame").count(), 1);
    assert!(wire["frame"]["chat"]["items"][0].get("text").is_none());
    assert!(wire.to_string().find("sequence").is_none());

    source.set_armor(Some(6));
    source.capture.lock().unwrap().events.clear();
    let next = chat_input(6, "Bob", "@Bot armor");
    source.set_chats(vec![trigger, next]);
    runtime
        .ingest_backend_event(chat_event("22", 1, "Bob", "@Bot armor"))
        .unwrap();
    wait_for_request(&agent, 2).await;
    let request = agent.requests.lock().unwrap().remove(0);
    assert_eq!(request.context.frame.status.unwrap().armor, Some(6));

    source.missing_light.store(true, Ordering::SeqCst);
    let missing = chat_input(7, "Alice", "@Bot no light");
    source.set_chats(vec![missing]);
    let mut failures = runtime.subscribe_failures();
    runtime
        .ingest_backend_event(chat_event("23", 1, "Alice", "@Bot no light"))
        .unwrap();
    let failure = tokio::time::timeout(Duration::from_secs(2), failures.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failure.code, "opening_frame_light_missing");
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Running
    );
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn production_observation_after_is_body_only_and_drains_facts_once() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    let opening = chat_input(401, "Alice", "@Bot observation");
    source.set_chats(vec![opening]);
    runtime
        .ingest_backend_event(chat_event(
            "observation-trigger",
            1,
            "Alice",
            "@Bot observation",
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;

    let run_scope = runtime
        .current_scope()
        .expect("startup scope remains active");
    let generation = runtime.current_generation();
    runtime
        .emit_internal(internal_fact("body-fact", &run_scope, "self_hurt"))
        .unwrap();

    let frame_source: Arc<dyn ParticipantFrameSource> = source.clone();
    let observation = ParticipantObservationAfterSource::new(
        frame_source,
        runtime.fact_owner(),
        run_scope.clone(),
        generation,
        "body-trigger",
    );
    let checks = Arc::new(AtomicUsize::new(0));
    let signal = NeverCancelled;
    let guard = RealScopeGuard {
        checks: Arc::clone(&checks),
    };
    let deadline = Deadline::after(std::time::Instant::now(), Duration::from_secs(1)).unwrap();
    let first = observation
        .observe_after(
            observation_invocation("first"),
            ExecutionResource::Body,
            serde_json::json!({"status": "failed"}),
            CapabilityExecutionContext::new(
                &run_scope.world_id,
                "body-trigger",
                ExecutionControl::new(&signal, deadline),
                &guard,
            ),
        )
        .await
        .unwrap()
        .expect("body observation returns direct frame");
    assert!(first.get("world").is_some());
    assert!(first.get("viewport").is_none());
    assert!(first.get("stable").is_none());
    assert!(first["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| { event["type"] == "self_hurt" }));
    assert!(first.get("triggerPlayer").is_none());

    let signal = NeverCancelled;
    let guard = RealScopeGuard {
        checks: Arc::clone(&checks),
    };
    let deadline = Deadline::after(std::time::Instant::now(), Duration::from_secs(1)).unwrap();
    let second = observation
        .observe_after(
            observation_invocation("second"),
            ExecutionResource::Body,
            serde_json::json!({"status": "completed"}),
            CapabilityExecutionContext::new(
                &run_scope.world_id,
                "body-trigger",
                ExecutionControl::new(&signal, deadline),
                &guard,
            ),
        )
        .await
        .unwrap()
        .expect("passive body facts remain sampleable");
    assert!(second.get("events").is_none());
    assert!(second.get("status").is_some());
    assert_eq!(source.capture_calls(), 3, "opening plus two body samples");

    for (resource, id) in [
        (ExecutionResource::Viewport, "viewport"),
        (ExecutionResource::Chat, "chat"),
        (ExecutionResource::Memory, "memory"),
    ] {
        let signal = NeverCancelled;
        let guard = RealScopeGuard {
            checks: Arc::clone(&checks),
        };
        let deadline = Deadline::after(std::time::Instant::now(), Duration::from_secs(1)).unwrap();
        let result = observation
            .observe_after(
                observation_invocation(id),
                resource,
                serde_json::json!({"status": "completed"}),
                CapabilityExecutionContext::new(
                    &run_scope.world_id,
                    "body-trigger",
                    ExecutionControl::new(&signal, deadline),
                    &guard,
                ),
            )
            .await
            .unwrap();
        assert!(result.is_none(), "{resource:?} must not sample body facts");
    }
    assert_eq!(source.capture_calls(), 3);
    assert!(checks.load(Ordering::SeqCst) >= 7);
    runtime.stop().await.unwrap();
    drop(runtime);

    let signal = NeverCancelled;
    let guard = RealScopeGuard { checks };
    let deadline = Deadline::after(std::time::Instant::now(), Duration::from_secs(1)).unwrap();
    let error = observation
        .observe_after(
            observation_invocation("after-runtime-drop"),
            ExecutionResource::Body,
            serde_json::json!({"status": "completed"}),
            CapabilityExecutionContext::new(
                &run_scope.world_id,
                "body-trigger",
                ExecutionControl::new(&signal, deadline),
                &guard,
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, AgentErrorCode::ScopeInvalid);
}

#[tokio::test]
async fn repeated_chat_text_uses_timestamp_and_sequence_identity() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    let first = chat_input_at(200, "Alice", "@Bot same text", "2026-08-03T00:10:00Z");
    let second = chat_input_at(201, "Alice", "@Bot same text", "2026-08-03T00:10:01Z");
    source.set_chats(vec![first.clone(), second.clone()]);
    runtime
        .ingest_backend_event(scoped_chat_event_at(
            "repeat-1",
            "process-test",
            1,
            "world-test",
            "minecraft:overworld",
            "Alice",
            "@Bot same text",
            &first.message.at,
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;
    let first_request = agent.requests.lock().unwrap()[0].clone();
    let first_events = first_request.context.frame.events.as_ref().unwrap();
    assert_eq!(
        first_events
            .iter()
            .filter_map(|event| match event {
                mineintent_contracts::agent::AgentEventV5::PlayerChat(message) => {
                    Some(message.at.as_str())
                }
                mineintent_contracts::agent::AgentEventV5::Summary { .. } => None,
            })
            .collect::<Vec<_>>(),
        vec![first.message.at.as_str()]
    );
    assert!(first_request
        .context
        .frame
        .chat
        .as_ref()
        .unwrap()
        .items
        .iter()
        .any(|item| matches!(
            item,
            mineintent_contracts::agent::AgentChatItemV5::Message(message)
                if message.at == second.message.at
        )));

    source.set_chats(vec![first.clone(), second.clone()]);
    runtime
        .ingest_backend_event(scoped_chat_event_at(
            "repeat-2",
            "process-test",
            1,
            "world-test",
            "minecraft:overworld",
            "Alice",
            "@Bot same text",
            &second.message.at,
        ))
        .unwrap();
    wait_for_request(&agent, 2).await;
    let second_request = agent.requests.lock().unwrap()[1].clone();
    let second_events = second_request.context.frame.events.as_ref().unwrap();
    let trigger_events = second_events
        .iter()
        .filter_map(|event| match event {
            mineintent_contracts::agent::AgentEventV5::PlayerChat(message) => Some(message),
            mineintent_contracts::agent::AgentEventV5::Summary { .. } => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(trigger_events.len(), 1);
    assert_eq!(trigger_events[0].at, second.message.at);
    assert!(second_request
        .context
        .frame
        .chat
        .as_ref()
        .unwrap()
        .items
        .iter()
        .any(|item| matches!(
            item,
            mineintent_contracts::agent::AgentChatItemV5::Message(message)
                if message.at == first.message.at
        )));
    assert!(second_request
        .context
        .frame
        .chat
        .as_ref()
        .unwrap()
        .items
        .iter()
        .any(|item| matches!(
            item,
            mineintent_contracts::agent::AgentChatItemV5::Moved(moved)
                if moved.at == second.message.at
        )));
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn second_chat_queues_fifo_without_preempting_first_run() {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, speech, motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let first = chat_input(10, "Alice", "@Bot first");
    source.set_chats(vec![first.clone()]);
    runtime
        .ingest_backend_event(chat_event("31", 1, "Alice", "@Bot first"))
        .unwrap();
    wait_for_request(&agent, 1).await;
    let second = chat_input(11, "Bob", "@Bot second");
    source.set_chats(vec![first, second]);
    runtime
        .ingest_backend_event(chat_event("32", 1, "Bob", "@Bot second"))
        .unwrap();
    assert_eq!(agent.requests.lock().unwrap().len(), 1);
    assert_eq!(motor.releases.load(Ordering::SeqCst), 0);
    assert_eq!(speech.cancelled.load(Ordering::SeqCst), 0);
    agent.release();
    wait_for_request(&agent, 2).await;
    assert_eq!(agent.texts(), vec!["@Bot first", "@Bot second"]);
    assert_eq!(source.retain_calls(), 2);
    assert_eq!(source.retained_count(), 0);
    assert_eq!(source.release_calls(), 2);
    runtime.stop().await.unwrap();
    assert!(source.release_all_calls() >= 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fact_recorded_after_wake_admission_is_drained_at_opening_processing_boundary() {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(agent.clone());
    runtime.start_worker().unwrap();

    let first = chat_input(100, "Alice", "@Bot first boundary");
    source.set_chats(vec![first.clone()]);
    runtime
        .ingest_backend_event(chat_event(
            "boundary-first",
            1,
            "Alice",
            "@Bot first boundary",
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;

    let second = chat_input(101, "Bob", "@Bot second boundary");
    source.set_chats(vec![first, second]);
    let capture_gate = source.gate_capture();
    let admission = runtime
        .ingest_backend_event(chat_event(
            "boundary-second",
            1,
            "Bob",
            "@Bot second boundary",
        ))
        .unwrap();
    assert!(matches!(admission, ParticipantAdmission::WakeQueued { .. }));

    agent.release();
    tokio::time::timeout(Duration::from_secs(2), capture_gate.wait_started())
        .await
        .expect("second opening capture should reach the controlled gate");

    let admission_gate = CleanupGate::new();
    runtime.install_admission_observer_for_test(Arc::new(TestAdmissionObserver {
        gate: Arc::clone(&admission_gate),
    }));
    let fact_runtime = Arc::clone(&runtime);
    let fact_scope = scope(1, "minecraft:overworld");
    let fact_producer = tokio::task::spawn_blocking(move || {
        fact_runtime.emit_internal(internal_fact(
            "damage-after-admission",
            &fact_scope,
            "self_hurt",
        ))
    });
    tokio::time::timeout(Duration::from_secs(2), admission_gate.wait_started())
        .await
        .expect("producer should stop after queue admission before record_fact");
    capture_gate.release();
    admission_gate.release();
    fact_producer
        .await
        .expect("fact producer task should join")
        .expect("fact admission should succeed");
    wait_for_request(&agent, 2).await;

    let requests = agent.requests.lock().unwrap();
    let second_events = requests[1]
        .context
        .frame
        .events
        .as_ref()
        .expect("second opening has trigger and fact");
    assert!(second_events.iter().any(|event| {
        matches!(
            event,
            mineintent_contracts::agent::AgentEventV5::Summary { event_type, .. }
                if event_type == "self_hurt"
        )
    }));

    runtime.stop().await.unwrap();
}
