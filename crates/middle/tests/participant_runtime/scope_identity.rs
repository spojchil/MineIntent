//! scope 与进程会话身份：陈旧事件不得污染当前 scope。
//!
//! fixture 与辅助函数在父文件，经 `use super::*` 复用。

use super::*;

#[tokio::test]
async fn stale_scope_chat_cannot_drain_current_pending_fact() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let current = scope(2, "minecraft:overworld");
    runtime
        .emit_internal(ParticipantInternalEvent::Fact {
            id: "health-2".to_owned(),
            occurred_at: "2026-08-03T00:02:00Z".to_owned(),
            scope: current,
            event_type: "health_baseline".to_owned(),
            summary: "health baseline scope two".to_owned(),
        })
        .unwrap();
    source.set_chats(vec![chat_input(30, "Alice", "@Bot stale")]);
    assert!(matches!(
        runtime.ingest_backend_event(chat_event("51", 1, "Alice", "@Bot stale")),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert!(agent.requests.lock().unwrap().is_empty());
    let current_chat = chat_input(31, "Alice", "@Bot current");
    source.set_chats(vec![current_chat]);
    runtime
        .ingest_backend_event(chat_event("52", 2, "Alice", "@Bot current"))
        .unwrap();
    wait_for_request(&agent, 1).await;
    let request = agent.requests.lock().unwrap().remove(0);
    assert!(request.context.frame.events.as_ref().unwrap().iter().any(|event| {
        matches!(event, mineintent_contracts::agent::AgentEventV5::Summary { event_type, .. } if event_type == "health_baseline")
    }));
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn retired_process_sessions_cannot_reactivate_old_scope() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    let process_a = ParticipantScope::new(
        "process-A",
        5,
        "world-A",
        Some("minecraft:overworld".to_owned()),
    );
    source.set_chats(Vec::new());
    runtime
        .ingest_backend_event(scoped_chat_event(
            "a-fact",
            &process_a.process_session_id,
            process_a.connection_epoch,
            &process_a.world_id,
            process_a.dimension.as_deref().unwrap(),
            "Alice",
            "A-only fact",
        ))
        .unwrap();

    let process_b = ParticipantScope::new(
        "process-B",
        1,
        "world-B",
        Some("minecraft:overworld".to_owned()),
    );
    let first_b = chat_input(101, "Alice", "@Bot B first");
    source.set_chats(vec![first_b]);
    runtime
        .ingest_backend_event(scoped_chat_event(
            "b-first",
            &process_b.process_session_id,
            process_b.connection_epoch,
            &process_b.world_id,
            process_b.dimension.as_deref().unwrap(),
            "Alice",
            "@Bot B first",
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;
    let first_request = agent.requests.lock().unwrap()[0].clone();
    assert!(!first_request
        .context
        .frame
        .events
        .as_ref()
        .is_some_and(|events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    mineintent_contracts::agent::AgentEventV5::Summary { event_type, .. }
                        if event_type == "player_chat_not_addressed"
                )
            })
        }));

    source.set_chats(Vec::new());
    assert!(matches!(
        runtime.ingest_backend_event(scoped_chat_event(
            "a-late",
            "process-A",
            6,
            "world-A",
            "minecraft:overworld",
            "Alice",
            "A late fact",
        )),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert!(matches!(
        runtime.ingest_backend_event(dimension_changed_event(
            "a-transition-late",
            "process-A",
            6,
            "world-A",
            "minecraft:nether",
            "minecraft:overworld",
            "minecraft:nether",
        )),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert!(matches!(
        runtime.ingest_backend_event(dimension_changed_event(
            "b-transition-low",
            "process-B",
            0,
            "world-B",
            "minecraft:nether",
            "minecraft:overworld",
            "minecraft:nether",
        )),
        Ok(ParticipantAdmission::Ignored)
    ));

    assert!(matches!(
        runtime.ingest_backend_event(dimension_changed_event(
            "b-transition-valid",
            "process-B",
            1,
            "world-B",
            "minecraft:nether",
            "minecraft:overworld",
            "minecraft:nether",
        )),
        Ok(ParticipantAdmission::Recorded)
    ));
    let b_scope = ParticipantScope::new(
        "process-B",
        1,
        "world-B",
        Some("minecraft:nether".to_owned()),
    );
    runtime
        .emit_internal(ParticipantInternalEvent::Fact {
            id: "b-health".to_owned(),
            occurred_at: "2026-08-03T00:03:00Z".to_owned(),
            scope: b_scope.clone(),
            event_type: "health_baseline".to_owned(),
            summary: "B health baseline".to_owned(),
        })
        .unwrap();
    let second_b = chat_input(102, "Alice", "@Bot B second");
    source.set_chats(vec![second_b]);
    runtime
        .ingest_backend_event(scoped_chat_event(
            "b-second",
            "process-B",
            1,
            "world-B",
            "minecraft:nether",
            "Alice",
            "@Bot B second",
        ))
        .unwrap();
    wait_for_request(&agent, 2).await;
    let second_request = agent.requests.lock().unwrap()[1].clone();
    assert!(second_request
        .context
        .frame
        .events
        .as_ref()
        .is_some_and(|events| {
            events.iter().any(|event| {
                matches!(
                    event,
                    mineintent_contracts::agent::AgentEventV5::Summary { event_type, .. }
                        if event_type == "health_baseline"
                )
            })
        }));
    assert_eq!(runtime.current_scope(), Some(b_scope));
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn stale_internal_scope_events_cannot_pollute_current_scope() {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, speech, motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    runtime
        .emit_internal(ParticipantInternalEvent::Fact {
            id: "a-seed".to_owned(),
            occurred_at: "2026-08-03T00:04:00Z".to_owned(),
            scope: ParticipantScope::new(
                "process-A",
                5,
                "world-A",
                Some("minecraft:overworld".to_owned()),
            ),
            event_type: "old_fact".to_owned(),
            summary: "old process fact".to_owned(),
        })
        .unwrap();

    let b_scope = ParticipantScope::new(
        "process-B",
        1,
        "world-B",
        Some("minecraft:overworld".to_owned()),
    );
    source.set_chats(vec![chat_input(110, "Alice", "@Bot B active")]);
    runtime
        .ingest_backend_event(scoped_chat_event(
            "b-active",
            "process-B",
            1,
            "world-B",
            "minecraft:overworld",
            "Alice",
            "@Bot B active",
        ))
        .unwrap();
    wait_for_request(&agent, 1).await;
    let releases_before_stale = motor.releases.load(Ordering::SeqCst);
    let speech_cancels_before_stale = speech.cancelled.load(Ordering::SeqCst);

    runtime
        .emit_internal(ParticipantInternalEvent::Fact {
            id: "b-health".to_owned(),
            occurred_at: "2026-08-03T00:04:01Z".to_owned(),
            scope: b_scope.clone(),
            event_type: "health_baseline".to_owned(),
            summary: "B health baseline".to_owned(),
        })
        .unwrap();
    let old_scope = ParticipantScope::new(
        "process-A",
        6,
        "world-A",
        Some("minecraft:nether".to_owned()),
    );
    assert!(matches!(
        runtime.emit_internal(ParticipantInternalEvent::Fact {
            id: "a-late-fact".to_owned(),
            occurred_at: "2026-08-03T00:04:02Z".to_owned(),
            scope: old_scope.clone(),
            event_type: "old_health".to_owned(),
            summary: "late A fact".to_owned(),
        }),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert!(matches!(
        runtime.emit_internal(ParticipantInternalEvent::ScopeChanged {
            id: "a-late-transition".to_owned(),
            occurred_at: "2026-08-03T00:04:03Z".to_owned(),
            scope: old_scope,
            reason: "late A transition".to_owned(),
        }),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert_eq!(runtime.current_scope(), Some(b_scope.clone()));
    assert_eq!(motor.releases.load(Ordering::SeqCst), releases_before_stale);
    assert_eq!(
        speech.cancelled.load(Ordering::SeqCst),
        speech_cancels_before_stale
    );
    assert_eq!(agent.requests.lock().unwrap().len(), 1);

    source.set_chats(vec![chat_input(111, "Alice", "@Bot B second")]);
    runtime
        .ingest_backend_event(scoped_chat_event(
            "b-second-internal-regression",
            "process-B",
            1,
            "world-B",
            "minecraft:overworld",
            "Alice",
            "@Bot B second",
        ))
        .unwrap();
    agent.release();
    wait_for_request(&agent, 2).await;
    let second_request = agent.requests.lock().unwrap()[1].clone();
    let events = second_request.context.frame.events.as_ref().unwrap();
    assert!(events.iter().any(|event| {
        matches!(
            event,
            mineintent_contracts::agent::AgentEventV5::Summary { event_type, .. }
                if event_type == "health_baseline"
        )
    }));
    assert!(!events.iter().any(|event| {
        matches!(
            event,
            mineintent_contracts::agent::AgentEventV5::Summary { event_type, .. }
                if event_type == "old_health"
        )
    }));
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn retired_process_identity_is_not_evicted_after_many_reconnects() {
    let agent = TestAgent::new(0);
    let (runtime, _source, _journal, speech, motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    for index in 0..10_u64 {
        let process = format!("process-{index}");
        let admission = runtime.ingest_backend_event(scoped_chat_event(
            &format!("ordinary-{index}"),
            &process,
            0,
            &format!("world-{index}"),
            "minecraft:overworld",
            "Alice",
            "ordinary message",
        ));
        assert!(matches!(admission, Ok(ParticipantAdmission::Recorded)));
    }
    assert_eq!(
        runtime.current_scope(),
        Some(ParticipantScope::new(
            "process-9",
            0,
            "world-9",
            Some("minecraft:overworld".to_owned()),
        ))
    );
    let releases_before_stale = motor.releases.load(Ordering::SeqCst);
    let speech_cancels_before_stale = speech.cancelled.load(Ordering::SeqCst);
    let old_scope = ParticipantScope::new(
        "process-0",
        99,
        "world-0",
        Some("minecraft:nether".to_owned()),
    );
    assert!(matches!(
        runtime.ingest_backend_event(scoped_chat_event(
            "late-ordinary-0",
            "process-0",
            99,
            "world-0",
            "minecraft:nether",
            "Alice",
            "ordinary late message",
        )),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert!(matches!(
        runtime.emit_internal(ParticipantInternalEvent::ScopeChanged {
            id: "late-transition-0".to_owned(),
            occurred_at: "2026-08-03T00:05:00Z".to_owned(),
            scope: old_scope,
            reason: "late old process transition".to_owned(),
        }),
        Ok(ParticipantAdmission::Ignored)
    ));
    assert_eq!(
        runtime.current_scope(),
        Some(ParticipantScope::new(
            "process-9",
            0,
            "world-9",
            Some("minecraft:overworld".to_owned()),
        ))
    );
    assert_eq!(motor.releases.load(Ordering::SeqCst), releases_before_stale);
    assert_eq!(
        speech.cancelled.load(Ordering::SeqCst),
        speech_cancels_before_stale
    );
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn scope_change_cancels_active_run_and_drops_old_queue() {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, speech, motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let old = chat_input(40, "Alice", "@Bot old");
    source.set_chats(vec![old.clone()]);
    runtime
        .ingest_backend_event(chat_event("61", 1, "Alice", "@Bot old"))
        .unwrap();
    wait_for_request(&agent, 1).await;
    source.set_chats(vec![old.clone(), chat_input(41, "Bob", "@Bot queued old")]);
    runtime
        .ingest_backend_event(chat_event("62", 1, "Bob", "@Bot queued old"))
        .unwrap();

    runtime
        .emit_internal(ParticipantInternalEvent::ScopeChanged {
            id: "scope-2".to_owned(),
            occurred_at: "2026-08-03T00:03:00Z".to_owned(),
            scope: scope(2, "minecraft:nether"),
            reason: "dimension changed".to_owned(),
        })
        .unwrap();
    assert!(motor.releases.load(Ordering::SeqCst) >= 1);
    assert!(speech.cancelled.load(Ordering::SeqCst) >= 1);
    let new_chat = chat_input(42, "Alice", "@Bot new scope");
    source.set_chats(vec![new_chat]);
    runtime
        .ingest_backend_event(scoped_chat_event_at(
            "63",
            "process-test",
            2,
            "world-test",
            "minecraft:nether",
            "Alice",
            "@Bot new scope",
            &chat_input(42, "Alice", "@Bot new scope").message.at,
        ))
        .unwrap();
    wait_for_request(&agent, 2).await;
    assert_eq!(agent.texts(), vec!["@Bot old", "@Bot new scope"]);
    assert_eq!(source.retain_calls(), 3);
    assert_eq!(source.retained_count(), 0);
    assert_eq!(source.release_calls(), 3);
    runtime.stop().await.unwrap();
}
