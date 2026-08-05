//! 启动、订阅回滚与停机路径。
//!
//! fixture 与辅助函数在父文件，经 `use super::*` 复用。

use super::*;

#[tokio::test]
async fn startup_registry_and_addressing_are_explicit_and_symmetric() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    assert_eq!(runtime.wake_registry().len(), 1);
    assert_eq!(
        runtime.wake_registry().entries(),
        vec![WakeRule {
            kind: WakeKind::PlayerChat,
            condition: WakeRuleCondition::AddressedToParticipant,
        }]
    );

    source.set_chats(Vec::new());
    assert!(matches!(
        runtime.ingest_backend_event(chat_event("01", 1, "Alice", "hello everyone")),
        Ok(ParticipantAdmission::Recorded)
    ));
    assert!(agent.requests.lock().unwrap().is_empty());

    let alice = chat_input(1, "Alice", "@Bot help");
    source.set_chats(vec![alice.clone()]);
    runtime
        .ingest_event(ParticipantEvent::Backend(chat_event(
            "02",
            1,
            "Alice",
            "@Bot help",
        )))
        .unwrap();
    wait_for_request(&agent, 1).await;

    let bob = chat_input(2, "Bob", "@Bot look");
    source.set_chats(vec![alice, bob.clone()]);
    runtime
        .ingest_backend_event(chat_event("03", 1, "Bob", "@Bot look"))
        .unwrap();
    wait_for_request(&agent, 2).await;
    assert_eq!(agent.texts(), vec!["@Bot help", "@Bot look"]);
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn startup_seeds_one_participant_started_without_calling_model() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    assert!(agent.requests.lock().unwrap().is_empty());
    assert!(matches!(
        runtime.start_worker(),
        Err(ParticipantRuntimeError::AlreadyStarted)
    ));
    assert!(agent.requests.lock().unwrap().is_empty());

    let trigger = chat_input(301, "Alice", "@Bot startup seed");
    source.set_chats(vec![trigger]);
    runtime
        .ingest_backend_event(chat_event("startup-chat", 1, "Alice", "@Bot startup seed"))
        .unwrap();
    wait_for_request(&agent, 1).await;

    let request = agent.requests.lock().unwrap()[0].clone();
    let seed_events = request
        .context
        .frame
        .events
        .as_ref()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            mineintent_contracts::agent::AgentEventV5::Summary {
                event_type,
                summary,
            } if event_type == "participant.started" => Some(summary.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(seed_events, vec!["AI 参与者已进入世界"]);
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn startup_snapshot_failure_rolls_back_without_worker_or_model_call() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, backend) = runtime_parts(Arc::clone(&agent));
    backend.fail_snapshot();

    let error = runtime.start_worker().unwrap_err();
    assert!(matches!(error, ParticipantRuntimeError::Backend(_)));
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Faulted
    );
    assert!(runtime.current_scope().is_none());
    assert!(agent.requests.lock().unwrap().is_empty());
    assert_eq!(backend.subscription_unsubscribes(), 0);
    assert_eq!(source.retained_count(), 0);
    assert!(source.release_all_calls() >= 1);

    runtime.stop().await.unwrap();
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
}

#[tokio::test]
async fn stop_finishes_while_subscribe_is_blocked_and_closes_late_handle() {
    let agent = TestAgent::new(0);
    let (runtime, _source, _journal, _speech, _motor, backend) = runtime_parts(Arc::clone(&agent));
    let subscribe_gate = backend.gate_subscribe();
    let start_runtime = Arc::clone(&runtime);
    let start = tokio::task::spawn_blocking(move || start_runtime.start_worker());
    subscribe_gate.wait_started().await;

    let stop_runtime = Arc::clone(&runtime);
    tokio::time::timeout(Duration::from_secs(1), stop_runtime.stop())
        .await
        .expect("stop must not wait for a blocked backend subscribe")
        .unwrap();
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
    assert!(!backend.subscription_closed());

    subscribe_gate.release();
    let start_result = tokio::time::timeout(Duration::from_secs(1), start)
        .await
        .expect("blocked start must return after subscribe is released")
        .unwrap();
    assert!(matches!(
        start_result,
        Err(ParticipantRuntimeError::Stopped)
    ));
    assert!(backend.subscription_closed());
    assert_eq!(backend.subscription_unsubscribes(), 1);
    runtime.stop().await.unwrap();
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
}

#[tokio::test]
async fn stop_finishes_before_blocked_subscribe_error_without_late_attach() {
    let agent = TestAgent::new(0);
    let (runtime, _source, _journal, _speech, _motor, backend) = runtime_parts(Arc::clone(&agent));
    let subscribe_gate = backend.gate_subscribe();
    backend.fail_subscribe();

    let start_runtime = Arc::clone(&runtime);
    let start = tokio::task::spawn_blocking(move || start_runtime.start_worker());
    subscribe_gate.wait_started().await;

    tokio::time::timeout(Duration::from_secs(1), runtime.stop())
        .await
        .expect("stop must not wait for a blocked subscribe that will fail")
        .unwrap();
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
    assert_eq!(backend.subscription_unsubscribes(), 0);
    assert!(!backend.subscription_closed());

    subscribe_gate.release();
    let start_result = tokio::time::timeout(Duration::from_secs(1), start)
        .await
        .expect("blocked start must return after the backend reports subscribe failure")
        .unwrap();
    assert!(matches!(
        start_result,
        Err(ParticipantRuntimeError::Backend(_))
    ));
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
    assert_eq!(backend.subscription_unsubscribes(), 0);
    assert!(!backend.subscription_closed());
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn subscribe_failure_rolls_back_worker_and_lifecycle() {
    let agent = TestAgent::new(0);
    let (runtime, _source, _journal, _speech, _motor, backend) = runtime_parts(Arc::clone(&agent));
    backend.fail_subscribe();
    let error = runtime.start_worker().unwrap_err();
    assert!(matches!(error, ParticipantRuntimeError::Backend(_)));
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Faulted
    );
    assert_eq!(backend.subscription_unsubscribes(), 0);
    runtime.stop().await.unwrap();
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Stopped
    );
}

#[tokio::test]
async fn ordinary_events_do_not_release_body_or_cancel_speech() {
    let agent = TestAgent::new(1);
    let (runtime, source, _journal, speech, motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();
    let first = chat_input(1, "Alice", "@Bot first");
    source.set_chats(vec![first]);
    runtime
        .ingest_backend_event(chat_event("11", 1, "Alice", "@Bot first"))
        .unwrap();
    wait_for_request(&agent, 1).await;

    let second = chat_input(2, "Bob", "@Bot second");
    source.set_chats(vec![chat_input(1, "Alice", "@Bot first"), second]);
    runtime
        .ingest_backend_event(chat_event("12", 1, "Bob", "@Bot second"))
        .unwrap();
    assert_eq!(motor.releases.load(Ordering::SeqCst), 0);
    assert_eq!(speech.cancelled.load(Ordering::SeqCst), 0);
    runtime.request_stop().unwrap();
    assert!(motor.releases.load(Ordering::SeqCst) >= 1);
    assert!(speech.cancelled.load(Ordering::SeqCst) >= 1);
    runtime.stop().await.unwrap();
}
