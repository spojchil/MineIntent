//! agent 装配、注册表绑定与 run id。
//!
//! fixture 与辅助函数在父文件，经 `use super::*` 复用。

use super::*;

#[tokio::test]
async fn agent_assembly_rejects_tool_definition_drift() {
    let registry = Arc::new(ToolCapabilityRegistry::new(Vec::new()).unwrap());
    let factory = Arc::new(AssemblyFactory {
        registry: Arc::clone(&registry),
        runner_registry: Arc::clone(&registry),
        bindings: Arc::new(Mutex::new(Vec::new())),
    });
    let assembly = ParticipantAgentAssembly::new(factory);
    assert!(assembly.definitions().is_empty());
    let mut request = AgentRunRequest {
        run_id: mineintent_contracts::agent::RunId::new("assembly-test").unwrap(),
        context: fixtures::agent_context_v5(),
        tools: Vec::new(),
        prompt_template: fixtures::prompt_template(),
    };
    request.tools.push(
        serde_json::from_value(serde_json::json!({
            "type": "function",
            "function": {
                "name": "drift",
                "description": "drift",
                "parameters": {"type": "object"}
            }
        }))
        .unwrap(),
    );
    let signal = NeverCancelled;
    let deadline = mineintent_contracts::agent::Deadline::after(
        std::time::Instant::now(),
        Duration::from_secs(1),
    )
    .unwrap();
    let result = assembly
        .run(
            scope(1, "minecraft:overworld"),
            0,
            "assembly-event".to_owned(),
            request,
            ExecutionControl::new(&signal, deadline),
        )
        .await;
    assert_eq!(result.unwrap_err().code, AgentErrorCode::InvalidRequest);
}

#[tokio::test]
async fn agent_factory_binds_each_wake_scope_and_trigger_identity() {
    let registry = Arc::new(ToolCapabilityRegistry::new(Vec::new()).unwrap());
    let bindings = Arc::new(Mutex::new(Vec::new()));
    let factory = Arc::new(AssemblyFactory {
        registry: Arc::clone(&registry),
        runner_registry: Arc::clone(&registry),
        bindings: Arc::clone(&bindings),
    });
    let assembly = ParticipantAgentAssembly::new(factory);
    let signal = NeverCancelled;
    let scope_one = ParticipantScope::new(
        "process-one",
        1,
        "world-one",
        Some("minecraft:overworld".to_owned()),
    );
    let scope_two = ParticipantScope::new(
        "process-two",
        2,
        "world-two",
        Some("minecraft:nether".to_owned()),
    );
    let deadline_one = mineintent_contracts::agent::Deadline::after(
        std::time::Instant::now(),
        Duration::from_secs(1),
    )
    .unwrap();
    assembly
        .run(
            scope_one.clone(),
            1,
            "chat-one".to_owned(),
            AgentRunRequest {
                run_id: mineintent_contracts::agent::RunId::new("factory-one").unwrap(),
                context: fixtures::agent_context_v5(),
                tools: Vec::new(),
                prompt_template: fixtures::prompt_template(),
            },
            ExecutionControl::new(&signal, deadline_one),
        )
        .await
        .unwrap();
    let deadline_two = mineintent_contracts::agent::Deadline::after(
        std::time::Instant::now(),
        Duration::from_secs(1),
    )
    .unwrap();
    assembly
        .run(
            scope_two.clone(),
            2,
            "chat-two".to_owned(),
            AgentRunRequest {
                run_id: mineintent_contracts::agent::RunId::new("factory-two").unwrap(),
                context: fixtures::agent_context_v5(),
                tools: Vec::new(),
                prompt_template: fixtures::prompt_template(),
            },
            ExecutionControl::new(&signal, deadline_two),
        )
        .await
        .unwrap();
    assert_eq!(
        *bindings.lock().unwrap(),
        vec![
            (scope_one, "chat-one".to_owned()),
            (scope_two, "chat-two".to_owned()),
        ]
    );
}

#[tokio::test]
async fn agent_assembly_rejects_runner_bound_to_foreign_registry() {
    let registry = Arc::new(ToolCapabilityRegistry::new(Vec::new()).unwrap());
    let foreign_registry = Arc::new(ToolCapabilityRegistry::new(Vec::new()).unwrap());
    let factory = Arc::new(AssemblyFactory {
        registry,
        runner_registry: foreign_registry,
        bindings: Arc::new(Mutex::new(Vec::new())),
    });
    let assembly = ParticipantAgentAssembly::new(factory);
    let signal = NeverCancelled;
    let deadline = mineintent_contracts::agent::Deadline::after(
        std::time::Instant::now(),
        Duration::from_secs(1),
    )
    .unwrap();
    let result = assembly
        .run(
            scope(1, "minecraft:overworld"),
            1,
            "foreign-registry".to_owned(),
            AgentRunRequest {
                run_id: mineintent_contracts::agent::RunId::new("foreign-registry").unwrap(),
                context: fixtures::agent_context_v5(),
                tools: Vec::new(),
                prompt_template: fixtures::prompt_template(),
            },
            ExecutionControl::new(&signal, deadline),
        )
        .await;
    assert_eq!(result.unwrap_err().code, AgentErrorCode::InvalidRequest);
}

#[tokio::test]
async fn real_concrete_runner_uses_one_registry_and_rebinds_scope_per_wake() {
    let motor = TestMotor::new();
    let backend = TestBackend::new(Arc::clone(&motor));
    let backend_api: Arc<dyn MinecraftBackendApi> = backend.clone();
    let journal = TestJournal::new();
    let speech = TestSpeech::new();
    let memory = Arc::new(RealMemoryPort::default());
    let services = ProductionCapabilityServices::new(
        Arc::clone(&backend_api),
        Arc::new(ViewportReader::new(backend_api)),
        journal.clone(),
        speech,
        memory.clone(),
    );
    let registry = build_production_capability_registry(services).unwrap();
    let definitions = registry.definitions();
    let names: Vec<_> = definitions
        .iter()
        .map(|definition| definition.function.name.as_str().to_owned())
        .collect();
    assert_eq!(
        names,
        vec![
            "look_relative",
            "move_input",
            "respawn",
            "view",
            "say",
            "remember"
        ]
    );

    let bindings = Arc::new(Mutex::new(Vec::new()));
    let model_requests = Arc::new(Mutex::new(Vec::new()));
    let scope_checks = Arc::new(AtomicUsize::new(0));
    let factory = Arc::new(ConcreteRegistryFactory {
        registry: Arc::clone(&registry),
        bindings: Arc::clone(&bindings),
        model_requests: Arc::clone(&model_requests),
        scope_checks: Arc::clone(&scope_checks),
    });
    let assembly = ParticipantAgentAssembly::new(factory.clone());
    assert!(Arc::ptr_eq(&assembly.registry(), &registry));
    assert_eq!(assembly.definitions(), definitions);

    let scope_one = ParticipantScope::new(
        "real-process",
        7,
        "world-one",
        Some("minecraft:overworld".to_owned()),
    );
    let scope_two = ParticipantScope::new(
        "real-process",
        8,
        "world-two",
        Some("minecraft:nether".to_owned()),
    );
    let signal = NeverCancelled;
    for (scope, trigger, run_id) in [
        (scope_one.clone(), "real-chat-one", "real-run-one"),
        (scope_two.clone(), "real-chat-two", "real-run-two"),
    ] {
        let deadline = mineintent_contracts::agent::Deadline::after(
            std::time::Instant::now(),
            Duration::from_secs(1),
        )
        .unwrap();
        assembly
            .run(
                scope,
                1,
                trigger.to_owned(),
                AgentRunRequest {
                    run_id: mineintent_contracts::agent::RunId::new(run_id).unwrap(),
                    context: fixtures::agent_context_v5(),
                    tools: definitions.clone(),
                    prompt_template: fixtures::prompt_template(),
                },
                ExecutionControl::new(&signal, deadline),
            )
            .await
            .unwrap();
    }

    assert_eq!(
        *bindings.lock().unwrap(),
        vec![
            (scope_one, "real-chat-one".to_owned()),
            (scope_two, "real-chat-two".to_owned()),
        ]
    );
    assert_eq!(memory.appends.load(Ordering::SeqCst), 2);
    assert!(scope_checks.load(Ordering::SeqCst) >= 2);
    assert_eq!(
        model_requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.tools == definitions)
            .count(),
        4
    );
    assert_eq!(
        journal.entries.lock().unwrap().as_slice(),
        &[
            "memory.remembered".to_owned(),
            "memory.remembered".to_owned(),
        ]
    );
}

#[tokio::test]
async fn reconstructed_runtimes_have_distinct_bounded_run_ids() {
    let first_agent = TestAgent::new(0);
    let (first_runtime, first_source, _journal, _speech, _motor, _backend) =
        runtime_parts_with_namespace(Arc::clone(&first_agent), "same-session");
    first_runtime.start_worker().unwrap();
    first_source.set_chats(vec![chat_input(90, "Alice", "@Bot first id")]);
    first_runtime
        .ingest_backend_event(chat_event("90", 1, "Alice", "@Bot first id"))
        .unwrap();
    wait_for_request(&first_agent, 1).await;
    let first_id = first_agent.requests.lock().unwrap()[0].run_id.to_string();
    first_runtime.stop().await.unwrap();

    let second_agent = TestAgent::new(0);
    let (second_runtime, second_source, _journal, _speech, _motor, _backend) =
        runtime_parts_with_namespace(Arc::clone(&second_agent), "same-session");
    second_runtime.start_worker().unwrap();
    second_source.set_chats(vec![chat_input(91, "Alice", "@Bot second id")]);
    second_runtime
        .ingest_backend_event(chat_event("91", 1, "Alice", "@Bot second id"))
        .unwrap();
    wait_for_request(&second_agent, 1).await;
    let second_id = second_agent.requests.lock().unwrap()[0].run_id.to_string();
    second_runtime.stop().await.unwrap();

    assert_ne!(first_id, second_id);
    assert!(first_id.chars().count() <= 128);
    assert!(second_id.chars().count() <= 128);
    assert!(!first_id.contains("same-session"));
    assert!(!first_id.contains("process-test"));
    assert!(first_id.starts_with("p-"));
    assert!(first_id.split('-').skip(1).all(|part| part
        .chars()
        .all(|character: char| character.is_ascii_hexdigit()
            || character.is_ascii_digit()
            || character.is_ascii_lowercase())));

    let max_namespace = "n".repeat(128);
    let max_agent = TestAgent::new(0);
    let (max_runtime, max_source, _journal, _speech, _motor, _backend) =
        runtime_parts_with_namespace(Arc::clone(&max_agent), &max_namespace);
    max_runtime.start_worker().unwrap();
    max_source.set_chats(vec![chat_input(92, "Alice", "@Bot max id")]);
    max_runtime
        .ingest_backend_event(chat_event("92", 1, "Alice", "@Bot max id"))
        .unwrap();
    wait_for_request(&max_agent, 1).await;
    assert!(
        max_agent.requests.lock().unwrap()[0]
            .run_id
            .to_string()
            .chars()
            .count()
            <= 128
    );
    max_runtime.stop().await.unwrap();

    let invalid_agent = TestAgent::new(0);
    let invalid_motor = TestMotor::new();
    let invalid_config = ParticipantRuntimeConfig {
        backend: TestBackend::new(Arc::clone(&invalid_motor)),
        agent: invalid_agent,
        frame_source: TestFrameSource::new(),
        memory: Arc::new(TestMemory),
        journal: TestJournal::new(),
        speech: TestSpeech::new(),
        debug: Arc::new(DebugStateStore::new()),
        clock: Arc::new(TestClock),
        prompt_template: fixtures::prompt_template(),
        run_deadline: Duration::from_secs(30),
        wake_registry: WakeRegistry::default(),
        run_id_namespace: "x".repeat(129),
    };
    assert!(ParticipantRuntime::try_new(invalid_config).is_err());
}

#[derive(Default)]
struct RealMemoryPort {
    appends: AtomicUsize,
}

impl MemoryStorePort for RealMemoryPort {
    fn append<'a>(&'a self, _text: String) -> ContractFuture<'a, Result<(), MemoryError>> {
        self.appends.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }

    fn replace<'a>(
        &'a self,
        _old_text: String,
        _new_text: String,
    ) -> ContractFuture<'a, Result<(), MemoryError>> {
        Box::pin(async { Ok(()) })
    }

    fn rewrite<'a>(&'a self, _text: String) -> ContractFuture<'a, Result<(), MemoryError>> {
        Box::pin(async { Ok(()) })
    }
}

struct RealActionIds;

impl CapabilityActionIdSource for RealActionIds {
    fn next_action_id(&self, invocation: &ToolInvocation) -> Result<String, AgentError> {
        Ok(format!("real-action-{}", invocation.tool_call_id))
    }
}

struct RealUtc;

impl CapabilityUtcTimestampSource for RealUtc {
    fn now_utc(&self) -> Result<String, AgentError> {
        Ok("2026-08-03T00:00:00Z".to_owned())
    }
}

struct RealRegistryModel {
    requests: Arc<Mutex<Vec<AgentModelRequest>>>,
    calls: AtomicUsize,
}

impl ModelProvider for RealRegistryModel {
    type Request = AgentModelRequest;
    type Response = ModelCompletion;

    fn complete<'a>(
        &'a self,
        request: Self::Request,
        _control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<Self::Response, AgentError>> {
        self.requests.lock().unwrap().push(request);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let message = if call == 0 {
            serde_json::json!({
                "role": "assistant",
                "tool_calls": [{
                    "id": "real-remember-call",
                    "function": {
                        "name": "remember",
                        "arguments": "{\"operation\":\"append\",\"text\":\"real scope fact\"}"
                    }
                }]
            })
        } else {
            serde_json::json!({"role": "assistant", "content": "done"})
        };
        Box::pin(async move {
            Ok(ModelCompletion {
                message: message.as_object().cloned(),
                finish_reason: None,
                usage: Some(ModelUsage::default()),
            })
        })
    }
}

struct ConcreteRegistryFactory {
    registry: Arc<ToolCapabilityRegistry>,
    bindings: Arc<Mutex<Vec<(ParticipantScope, String)>>>,
    model_requests: Arc<Mutex<Vec<AgentModelRequest>>>,
    scope_checks: Arc<AtomicUsize>,
}

impl ParticipantAgentFactory for ConcreteRegistryFactory {
    fn registry(&self) -> Arc<ToolCapabilityRegistry> {
        Arc::clone(&self.registry)
    }

    fn build(
        &self,
        scope: &ParticipantScope,
        _generation: u64,
        trigger_event_id: &str,
    ) -> Result<Arc<dyn ParticipantScopedAgentRunner>, AgentError> {
        self.bindings
            .lock()
            .unwrap()
            .push((scope.clone(), trigger_event_id.to_owned()));
        let scope_guard: Arc<dyn ScopeGuard> = Arc::new(RealScopeGuard {
            checks: Arc::clone(&self.scope_checks),
        });
        let dispatcher = RegistryToolDispatcher::new(
            Arc::clone(&self.registry),
            Default::default(),
            Arc::new(ExplicitCapabilityInvocationAssembler::new(
                Arc::new(RealActionIds),
                Arc::new(RealUtc),
            )),
            Arc::new(CapabilityScopeAssembly::new(
                scope.world_id.clone(),
                trigger_event_id.to_owned(),
                scope_guard,
            )),
        );
        Ok(Arc::new(ConcreteAgentRunner::new(
            RealRegistryModel {
                requests: Arc::clone(&self.model_requests),
                calls: AtomicUsize::new(0),
            },
            dispatcher,
            mineintent_contracts::agent::ModelName::new("real-participant-model").unwrap(),
        )))
    }
}

struct AssemblyFactory {
    registry: Arc<ToolCapabilityRegistry>,
    runner_registry: Arc<ToolCapabilityRegistry>,
    bindings: Arc<Mutex<Vec<(ParticipantScope, String)>>>,
}

impl ParticipantAgentFactory for AssemblyFactory {
    fn registry(&self) -> Arc<ToolCapabilityRegistry> {
        Arc::clone(&self.registry)
    }

    fn build(
        &self,
        scope: &ParticipantScope,
        _generation: u64,
        trigger_event_id: &str,
    ) -> Result<Arc<dyn ParticipantScopedAgentRunner>, AgentError> {
        self.bindings
            .lock()
            .unwrap()
            .push((scope.clone(), trigger_event_id.to_owned()));
        Ok(Arc::new(AssemblyRunner {
            registry: Arc::clone(&self.runner_registry),
        }))
    }
}

struct AssemblyRunner {
    registry: Arc<ToolCapabilityRegistry>,
}

impl ParticipantRegistryBound for AssemblyRunner {
    fn tool_registry(&self) -> Arc<ToolCapabilityRegistry> {
        Arc::clone(&self.registry)
    }
}

impl AgentRunner for AssemblyRunner {
    type Context = JsonAgentDecisionContextV5;

    fn run<'a>(
        &'a self,
        _request: AgentRunRequest<Self::Context>,
        _control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<ModelRunResult, AgentError>> {
        Box::pin(async { Ok(fixtures::model_run_result()) })
    }
}

/// 实测抓到的移植偏差回归：模型 provider 一次失败曾把整个 runtime 打成
/// Faulted，此后任何指名聊天都不再唤醒（同伴永久失聪）。oracle
/// runtime.ts:311-314 只 catch 住记 model.decision_failed 并继续。
/// 本回归钉住：失败后 runtime 仍 Running，且下一次唤醒照常进模型。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_provider_failure_ends_the_run_not_the_participant() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    agent.fail.store(true, Ordering::SeqCst);
    let first = chat_input(10, "Alice", "@Bot first");
    source.set_chats(vec![first.clone()]);
    runtime
        .ingest_backend_event(chat_event("41", 1, "Alice", "@Bot first"))
        .unwrap();
    wait_for_request(&agent, 1).await;
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Running,
        "模型失败不得让同伴进入 Faulted"
    );

    agent.fail.store(false, Ordering::SeqCst);
    let second = chat_input(11, "Bob", "@Bot second");
    source.set_chats(vec![first, second]);
    runtime
        .ingest_backend_event(chat_event("42", 1, "Bob", "@Bot second"))
        .unwrap();
    wait_for_request(&agent, 2).await;
    assert_eq!(
        agent.texts(),
        vec!["@Bot first", "@Bot second"],
        "失败之后的唤醒必须照常进入模型"
    );
    runtime.stop().await.unwrap();
}

/// 实测抓到的第二处移植偏差回归：入队路径把瞬时 source 错误当致命，
/// 同伴在游戏里死一次就永久 Faulted。致命判据必须与 worker 路径同一条规则。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transient_source_failure_during_ingest_does_not_fault_the_participant() {
    let agent = TestAgent::new(0);
    let (runtime, source, _journal, _speech, _motor, _backend) = runtime_parts(Arc::clone(&agent));
    runtime.start_worker().unwrap();

    source.fail_context.store(true, Ordering::SeqCst);
    // 走监听器路径：致命判定发生在 on_event，不在公开的 ingest 返回值上。
    mineintent_contracts::minecraft::BackendEventListener::on_event(
        runtime.as_ref(),
        chat_event("51", 1, "Alice", "@Bot while dead"),
    );
    assert_eq!(
        runtime.lifecycle(),
        mineintent_middle::participant::ParticipantLifecycle::Running,
        "瞬时 source 失败不得打死同伴"
    );

    source.fail_context.store(false, Ordering::SeqCst);
    let recovered = chat_input(12, "Bob", "@Bot after recovery");
    source.set_chats(vec![recovered]);
    runtime
        .ingest_backend_event(chat_event("52", 1, "Bob", "@Bot after recovery"))
        .unwrap();
    wait_for_request(&agent, 1).await;
    assert_eq!(
        agent.texts(),
        vec!["@Bot after recovery"],
        "恢复后必须照常唤醒"
    );
    runtime.stop().await.unwrap();
}
