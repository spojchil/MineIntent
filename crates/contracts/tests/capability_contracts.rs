use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context as TaskContext, Poll, Wake, Waker},
    time::{Duration, Instant},
};

use mineintent_contracts::{
    agent::{
        fixtures as agent_fixtures, AgentError, AgentErrorCode, CancellationSignal, ContractFuture,
        Deadline, ExecutionControl, FunctionToolDefinition, ToolDefinitionName, ToolDefinitionType,
        ToolExecution, ToolInvocation, ToolResponseProtocol, WireToolDefinition,
    },
    capability::{
        directed_view_result_schema, fixtures, move_input_parameters_schema,
        validate_directed_positions, view_parameters_schema, CapabilityExecutionContext,
        CapabilityInvocation, ExecutionResource, MoveDirection, MoveInputArguments, ScopeGuard,
        ToolCapability, ToolCapabilityRegistry, ToolDispatcher, ToolResultProtocol, ViewArguments,
        ViewMode, MAX_DIRECTED_VIEW_POSITIONS,
    },
    minecraft::{
        BlockInfo, DirectedOccluder, DirectedSeenBlock, DirectedUnseenBlock,
        DirectedViewportProjection, DirectedWhy,
    },
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

const CAPABILITY_INVOCATION: &str = include_str!("testdata/capability-invocation.valid.json");
const MOVE_INPUT_ARGUMENTS: &str = include_str!("testdata/move-input.arguments.valid.json");

#[test]
fn capability_invocation_fixture_is_strict_and_preserves_arguments() {
    let invocation: CapabilityInvocation = parse(CAPABILITY_INVOCATION);
    assert_eq!(invocation, fixtures::capability_invocation());
    assert_eq!(invocation.arguments["duration_ms"], 250);

    let mut unknown = fixture_value(CAPABILITY_INVOCATION);
    insert_unknown(&mut unknown, "roundId");
    assert_rejected::<CapabilityInvocation>(unknown);

    let mut wrong_arguments = fixture_value(CAPABILITY_INVOCATION);
    wrong_arguments["arguments"] = json!([]);
    assert_rejected::<CapabilityInvocation>(wrong_arguments);

    for (field, invalid) in [
        ("runId", String::new()),
        ("runId", "r".repeat(129)),
        ("toolCallId", "contains space".to_owned()),
        ("toolCallId", "c".repeat(129)),
    ] {
        let mut value = fixture_value(CAPABILITY_INVOCATION);
        value[field] = json!(invalid);
        assert_rejected::<CapabilityInvocation>(value);
    }
}

#[test]
fn registry_derives_ordered_definitions_and_dispatch_from_same_instances() {
    let first = stub_capability("first", Some(ExecutionResource::Body));
    let second = stub_capability("second", None);
    let registry = ToolCapabilityRegistry::new(vec![Arc::clone(&first), Arc::clone(&second)])
        .expect("unique registry is valid");

    let names: Vec<_> = registry
        .definitions()
        .into_iter()
        .map(|definition| definition.function.name.into_inner())
        .collect();
    assert_eq!(names, ["first", "second"]);
    assert_eq!(registry.len(), 2);
    assert!(!registry.is_empty());
    assert!(Arc::ptr_eq(
        &registry.resolve("first").expect("first resolves"),
        &first
    ));
    assert!(Arc::ptr_eq(
        &registry.resolve("second").expect("second resolves"),
        &second
    ));
    assert_eq!(first.resource(), Some(ExecutionResource::Body));
    assert_eq!(second.resource(), None);
    assert!(registry.resolve("absent").is_none());
}

#[test]
fn registry_rejects_duplicate_advertised_names_with_structured_error() {
    let error = ToolCapabilityRegistry::new(vec![
        stub_capability("same", Some(ExecutionResource::Body)),
        stub_capability("same", Some(ExecutionResource::Viewport)),
    ])
    .err()
    .expect("duplicate names fail at construction");

    assert_eq!(error.code, AgentErrorCode::DuplicateToolCapability);
    assert_eq!(error.summary, "duplicate_tool_capability:same");
}

#[test]
fn move_input_schema_matches_the_model_visible_oracle() {
    let schema = Value::Object(move_input_parameters_schema());
    assert_eq!(
        schema["properties"]["directions"],
        json!({
            "description": "同时按住的移动键，方向相对当前朝向；斜走时把两个键放在这里。",
            "minItems": 1,
            "maxItems": 4,
            "type": "array",
            "items": {"type": "string", "enum": ["forward", "back", "left", "right"]},
            "uniqueItems": true
        })
    );
    assert_eq!(schema["required"], json!(["directions", "duration_ms"]));
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"]["duration_ms"]["minimum"], 50);
    assert_eq!(schema["properties"]["duration_ms"]["maximum"], 1_500);

    let arguments: MoveInputArguments = parse(MOVE_INPUT_ARGUMENTS);
    assert_eq!(arguments, fixtures::move_input_arguments());
    assert_eq!(
        serde_json::to_value(arguments).expect("move arguments serialize"),
        fixture_value(MOVE_INPUT_ARGUMENTS)
    );

    let cancelling_axes: MoveInputArguments = serde_json::from_value(json!({
        "directions": ["forward", "back"],
        "duration_ms": 50
    }))
    .expect("opposing keys are distinct and remain valid");
    assert_eq!(
        cancelling_axes.directions,
        [MoveDirection::Forward, MoveDirection::Back]
    );
}

#[test]
fn move_input_arguments_reject_unknown_fields_versions_and_constraint_mutations() {
    for invalid in [
        json!({"directions": [], "duration_ms": 50}),
        json!({
            "directions": ["forward", "back", "left", "right", "forward"],
            "duration_ms": 50
        }),
        json!({"directions": ["forward", "forward"], "duration_ms": 50}),
        json!({"directions": ["up"], "duration_ms": 50}),
        json!({"directions": ["forward"], "duration_ms": 49}),
        json!({"directions": ["forward"], "duration_ms": 1501}),
        json!({"directions": ["forward"], "duration_ms": 50.5}),
        json!({"directions": ["forward"], "duration_ms": 50, "sprint": null}),
        json!({"directions": ["forward"], "duration_ms": 50, "jump": true}),
    ] {
        assert_rejected::<MoveInputArguments>(invalid);
    }

    assert_rejected::<MoveDirection>(json!("future_direction"));
}

#[test]
fn view_arguments_have_full_and_directed_positive_examples() {
    let full: ViewArguments = parse(r#"{"mode":"full"}"#);
    assert_eq!(full.mode, ViewMode::Full);
    assert_eq!(full.positions, None);
    assert_eq!(
        serde_json::to_value(&full).unwrap(),
        json!({"mode": "full"})
    );

    let directed: ViewArguments =
        parse(r#"{"mode":"directed","positions":[[0,64,0],[-12,65,37]]}"#);
    assert_eq!(directed.mode, ViewMode::Directed);
    assert_eq!(directed.positions, Some(vec![(0, 64, 0), (-12, 65, 37)]));
    assert_eq!(
        serde_json::to_value(&directed).unwrap(),
        json!({
            "mode": "directed",
            "positions": [[0, 64, 0], [-12, 65, 37]]
        })
    );

    let at_limit = json!({
        "mode": "directed",
        "positions": (0..MAX_DIRECTED_VIEW_POSITIONS)
            .map(|x| [x as i32, 64, 0])
            .collect::<Vec<_>>()
    });
    let _: ViewArguments = serde_json::from_value(at_limit).expect("initial batch limit is valid");
}

#[test]
fn view_schema_freezes_flat_modes_optional_positions_and_semantics() {
    let schema = Value::Object(view_parameters_schema());
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["required"], json!(["mode"]));
    assert_eq!(schema["additionalProperties"], false);
    assert!(schema.get("oneOf").is_none());
    assert_eq!(schema["properties"]["mode"]["type"], "string");
    assert_eq!(
        schema["properties"]["mode"]["enum"],
        json!(["full", "directed"])
    );
    assert_eq!(
        schema["properties"]["mode"]["description"],
        "full 只给出本次读取确认的正面可见证据，结果可能因预算而截断；未列出不表示不可见或不存在。想核对未列出的坐标时使用 directed。directed 对 positions 逐坐标给出可见事实或不可见原因；不可见时绝不返回目标方块的身份或状态。"
    );
    assert_eq!(
        schema["properties"]["positions"]["description"],
        "仅 directed 使用；每项是 Minecraft 方块体素世界绝对坐标的整数三元组 [x, y, z]，不是内部句柄。directed 必须至少给一个坐标；full 不得提供 positions。"
    );
    assert_eq!(schema["properties"]["positions"]["type"], "array");
    assert_eq!(schema["properties"]["positions"]["minItems"], 1);
    assert_eq!(
        schema["properties"]["positions"]["maxItems"],
        MAX_DIRECTED_VIEW_POSITIONS
    );
    let coordinate = &schema["properties"]["positions"]["items"];
    assert_eq!(coordinate["type"], "array");
    assert_eq!(coordinate["minItems"], 3);
    assert_eq!(coordinate["maxItems"], 3);
    assert_eq!(coordinate["items"]["type"], "integer");
    assert_eq!(coordinate["items"]["minimum"], i32::MIN);
    assert_eq!(coordinate["items"]["maximum"], i32::MAX);
}

#[test]
fn view_arguments_reject_missing_null_unknown_cross_field_and_bad_positions() {
    for invalid in [
        json!({}),
        json!({"mode": null}),
        json!({"mode": "future"}),
        json!({"mode": "full", "positions": []}),
        json!({"mode": "full", "positions": [[0, 64, 0]]}),
        json!({"mode": "directed"}),
        json!({"mode": "directed", "positions": null}),
        json!({"mode": "directed", "positions": []}),
        json!({
            "mode": "directed",
            "positions": vec![[0, 64, 0]; MAX_DIRECTED_VIEW_POSITIONS + 1]
        }),
        json!({"mode": "directed", "positions": [[0, 64]]}),
        json!({"mode": "directed", "positions": [[0, 64, 0, 1]]}),
        json!({"mode": "directed", "positions": [[0, 64, 0.5]]}),
        json!({"mode": "directed", "positions": [[0, 64, "0"]]}),
        json!({"mode": "directed", "positions": [[0, 64, null]]}),
        json!({"mode": "directed", "positions": [null]}),
        json!({"mode": "directed", "positions": [[0, 64, 0], [0, 64, 0]]}),
        json!({
            "mode": "directed",
            "positions": [[2147483648u64, 64u64, 0u64]]
        }),
        json!({"mode": "full", "unexpected": true}),
    ] {
        assert_rejected::<ViewArguments>(invalid);
    }

    for invalid_json in [
        r#"{"mode":"directed","positions":[[NaN,64,0]]}"#,
        r#"{"mode":"directed","positions":[[Infinity,64,0]]}"#,
        r#"{"mode":"directed","positions":[[-Infinity,64,0]]}"#,
        r#"{"mode":"directed","positions":[[1e400,64,0]]}"#,
    ] {
        assert!(
            serde_json::from_str::<ViewArguments>(invalid_json).is_err(),
            "non-finite coordinate was accepted: {invalid_json}"
        );
    }

    for (resource, encoded) in [
        (ExecutionResource::Body, "body"),
        (ExecutionResource::Chat, "chat"),
        (ExecutionResource::Memory, "memory"),
        (ExecutionResource::Viewport, "viewport"),
    ] {
        assert_eq!(serde_json::to_value(resource).unwrap(), encoded);
    }
    assert_rejected::<ExecutionResource>(json!("future_resource"));
    assert_eq!(
        serde_json::to_value(ToolResultProtocol::V1).unwrap(),
        "mineintent.tool-result.v1"
    );
    assert_rejected::<ToolResultProtocol>(json!("mineintent.tool-result.v0"));
}

#[test]
fn directed_wire_is_strict_reasoned_and_does_not_leak_unseen_target_block() {
    let projection = DirectedViewportProjection {
        seen: vec![DirectedSeenBlock {
            at: [0, 64, 0],
            block: BlockInfo::bare("air"),
        }],
        unseen: vec![
            DirectedUnseenBlock {
                at: [1, 64, 0],
                why: vec![DirectedWhy::OutsideFov, DirectedWhy::TooFar],
                distance: Some(47.2),
                max: Some(32.0),
                by: None,
            },
            DirectedUnseenBlock {
                at: [2, 64, 0],
                why: vec![DirectedWhy::Occluded],
                distance: None,
                max: None,
                by: Some(DirectedOccluder {
                    at: [2, 64, -1],
                    block: BlockInfo::bare("stone"),
                }),
            },
        ],
    };
    let wire = serde_json::to_value(&projection).unwrap();
    assert_eq!(wire["seen"][0]["block"], "air");
    assert_eq!(wire["unseen"][0]["why"], json!(["outside_fov", "too_far"]));
    assert_eq!(wire["unseen"][0]["distance"], 47.2);
    assert_eq!(wire["unseen"][0]["max"], 32.0);
    assert!(wire["unseen"][0].get("by").is_none());
    assert_eq!(wire["unseen"][1]["by"]["block"], "stone");
    assert!(wire["unseen"][1].get("block").is_none());

    let invalid_reason = json!({
        "seen": [],
        "unseen": [{"at":[0,64,0],"why":["secret_reason"]}]
    });
    assert_rejected::<DirectedViewportProjection>(invalid_reason);
    let invalid_distance = json!({
        "seen": [],
        "unseen": [{"at":[0,64,0],"why":["occluded"],"distance":2,"max":1}]
    });
    assert_rejected::<DirectedViewportProjection>(invalid_distance);
    let invalid_by = json!({
        "seen": [],
        "unseen": [{"at":[0,64,0],"why":["outside_fov"],"by":{"at":[0,64,-1],"block":"stone"}}]
    });
    assert_rejected::<DirectedViewportProjection>(invalid_by);
    let invalid_out_of_world_by = json!({
        "seen": [],
        "unseen": [{"at":[0,64,0],"why":["occluded", "out_of_world"],"by":{"at":[0,64,-1],"block":"stone"}}]
    });
    assert_rejected::<DirectedViewportProjection>(invalid_out_of_world_by);
    let five_reason_wire = json!({
        "seen": [],
        "unseen": [{
            "at":[0,64,0],
            "why":["outside_fov", "too_far", "occluded", "chunk_not_loaded", "out_of_world"],
            "distance": 47.2,
            "max": 32.0
        }]
    });
    let five_reason: DirectedViewportProjection =
        serde_json::from_value(five_reason_wire).expect("five directed reasons should deserialize");
    assert_eq!(
        five_reason.unseen[0].why,
        [
            DirectedWhy::OutsideFov,
            DirectedWhy::TooFar,
            DirectedWhy::Occluded,
            DirectedWhy::ChunkNotLoaded,
            DirectedWhy::OutOfWorld
        ]
    );
    let legacy_four_reason: DirectedViewportProjection = serde_json::from_value(json!({
        "seen": [],
        "unseen": [{"at":[0,64,0],"why":["outside_fov", "occluded"]}]
    }))
    .expect("old four-value reason wire must remain valid");
    assert_eq!(
        legacy_four_reason.unseen[0].why,
        [DirectedWhy::OutsideFov, DirectedWhy::Occluded]
    );
    let invalid_empty_block = json!({
        "seen": [{"at":[0,64,0],"block":{"name":"stone"}}],
        "unseen": []
    });
    assert_rejected::<DirectedViewportProjection>(invalid_empty_block);
}

#[test]
fn directed_input_validation_and_output_schema_share_closed_limits() {
    assert!(validate_directed_positions(&[(0, 64, 0); MAX_DIRECTED_VIEW_POSITIONS]).is_err());
    assert!(validate_directed_positions(&[]).is_err());
    assert!(validate_directed_positions(
        &(0..MAX_DIRECTED_VIEW_POSITIONS)
            .map(|x| (x as i32, 64, 0))
            .collect::<Vec<_>>()
    )
    .is_ok());

    let schema = Value::Object(directed_view_result_schema());
    assert_eq!(schema["required"], json!(["seen", "unseen"]));
    assert_eq!(
        schema["properties"]["unseen"]["items"]["properties"]["why"]["items"]["enum"],
        json!([
            "outside_fov",
            "too_far",
            "occluded",
            "chunk_not_loaded",
            "out_of_world"
        ])
    );
    let why_choices = schema["properties"]["unseen"]["items"]["properties"]["why"]["oneOf"]
        .as_array()
        .expect("directed why schema choices");
    assert_eq!(why_choices.len(), 31);
    assert!(why_choices
        .iter()
        .any(|choice| choice["const"] == json!(["outside_fov", "occluded"])));
    assert!(why_choices
        .iter()
        .any(|choice| choice["const"] == json!(["outside_fov", "out_of_world"])));
    assert!(why_choices.iter().any(|choice| {
        choice["const"]
            == json!([
                "outside_fov",
                "too_far",
                "occluded",
                "chunk_not_loaded",
                "out_of_world"
            ])
    }));
    assert!(!why_choices
        .iter()
        .any(|choice| choice["const"] == json!(["occluded", "outside_fov"])));
    assert_eq!(
        schema["properties"]["unseen"]["items"]["additionalProperties"],
        false
    );
    assert_eq!(
        schema["properties"]["seen"]["maxItems"],
        MAX_DIRECTED_VIEW_POSITIONS
    );
    assert_eq!(
        schema["properties"]["unseen"]["maxItems"],
        MAX_DIRECTED_VIEW_POSITIONS
    );
    let unseen_conditions = schema["properties"]["unseen"]["items"]["allOf"]
        .as_array()
        .expect("directed unseen schema conditions");
    assert!(unseen_conditions.iter().any(|condition| {
        condition["if"]["properties"]["why"]["contains"]["const"] == "out_of_world"
            && condition["then"]["not"]["required"] == json!(["by"])
    }));
    assert_eq!(
        schema["properties"]["seen"]["items"]["properties"]["block"]["oneOf"][1]["minProperties"],
        2
    );
    let too_many = json!({
        "seen": (0..=MAX_DIRECTED_VIEW_POSITIONS)
            .map(|x| json!({"at":[x as i32,64,0],"block":"air"}))
            .collect::<Vec<_>>(),
        "unseen": []
    });
    assert_rejected::<DirectedViewportProjection>(too_many);

    // Each side remains below its individual schema max; only the combined DTO budget is
    // exceeded. This prevents a per-array-only validation from replacing the frozen total 16.
    let split_too_many = json!({
        "seen": (0..8)
            .map(|x| json!({"at":[x,64,0],"block":"air"}))
            .collect::<Vec<_>>(),
        "unseen": (8..=16)
            .map(|x| json!({"at":[x,64,0],"why":["outside_fov"]}))
            .collect::<Vec<_>>()
    });
    assert_rejected::<DirectedViewportProjection>(split_too_many);
}

#[test]
fn capability_context_checks_cancellation_deadline_then_scope() {
    let now = Instant::now();
    let cancelled = FixedCancellation(Some(AgentError::run_cancelled()));
    let active = FixedCancellation(None);
    let invalid_scope = FixedScope(false);
    let current_scope = FixedScope(true);

    let context = CapabilityExecutionContext::new(
        "world-1",
        "chat-1",
        ExecutionControl::new(&cancelled, Deadline::at(now)),
        &invalid_scope,
    );
    assert_eq!(
        context.check_at(now).unwrap_err().code,
        AgentErrorCode::RunCancelled
    );
    let mut cancelled_notification = context.control().cancelled();
    assert!(matches!(
        poll_once(cancelled_notification.as_mut()),
        Poll::Ready(error) if error.code == AgentErrorCode::RunCancelled
    ));

    let context = CapabilityExecutionContext::new(
        "world-1",
        "chat-1",
        ExecutionControl::new(&active, Deadline::at(now)),
        &invalid_scope,
    );
    assert_eq!(
        context.check_at(now).unwrap_err().code,
        AgentErrorCode::DeadlineExceeded
    );
    let mut active_notification = context.control().cancelled();
    assert!(matches!(
        poll_once(active_notification.as_mut()),
        Poll::Pending
    ));

    let context = CapabilityExecutionContext::new(
        "world-1",
        "chat-1",
        ExecutionControl::new(
            &active,
            Deadline::after(now, Duration::from_secs(1))
                .expect("one-second deadline is representable"),
        ),
        &invalid_scope,
    );
    assert_eq!(
        context.check_at(now).unwrap_err().code,
        AgentErrorCode::ScopeInvalid
    );
    assert!(!context.is_current());

    let context = CapabilityExecutionContext::new(
        "world-1",
        "chat-1",
        ExecutionControl::new(
            &active,
            Deadline::after(now, Duration::from_secs(1))
                .expect("one-second deadline is representable"),
        ),
        &current_scope,
    );
    assert!(context.check_at(now).is_ok());
    assert!(context.is_current());
    assert_eq!(context.world_id(), "world-1");
    assert_eq!(context.chat_event_id(), "chat-1");
}

#[test]
fn in_process_capability_and_dispatch_traits_need_no_transport_fields() {
    let now = Instant::now();
    let active = FixedCancellation(None);
    let current_scope = FixedScope(true);
    let control = ExecutionControl::new(
        &active,
        Deadline::after(now, Duration::from_secs(1)).expect("one-second deadline is representable"),
    );
    let context = CapabilityExecutionContext::new("world", "chat", control, &current_scope);

    let capability = stub_capability("echo", None);
    let capability_result =
        poll_ready(capability.execute(fixtures::capability_invocation(), context))
            .expect("stub capability executes in process");
    assert_eq!(capability_result["actionId"], "action-1");

    let dispatcher_impl = StubDispatcher;
    let dispatcher: &dyn ToolDispatcher<Observation = Value> = &dispatcher_impl;
    let dispatch_result =
        poll_ready(dispatcher.dispatch(agent_fixtures::tool_invocation(), control))
            .expect("stub dispatcher executes in process");
    assert_eq!(dispatch_result.protocol, ToolResponseProtocol::V2);
    assert_eq!(dispatch_result.result["tool"], "look_relative");
    assert_eq!(dispatch_result.observation_after.as_ref(), None);
}

struct StubCapability {
    definition: WireToolDefinition,
    resource: Option<ExecutionResource>,
}

impl ToolCapability for StubCapability {
    fn definition(&self) -> &WireToolDefinition {
        &self.definition
    }

    fn resource(&self) -> Option<ExecutionResource> {
        self.resource
    }

    fn execute<'a>(
        &'a self,
        invocation: CapabilityInvocation,
        context: CapabilityExecutionContext<'a>,
    ) -> ContractFuture<'a, Result<Value, AgentError>> {
        Box::pin(async move {
            context.check_at(Instant::now())?;
            Ok(json!({"actionId": invocation.action_id}))
        })
    }
}

fn stub_capability(name: &str, resource: Option<ExecutionResource>) -> Arc<dyn ToolCapability> {
    Arc::new(StubCapability {
        definition: WireToolDefinition {
            r#type: ToolDefinitionType::Function,
            function: FunctionToolDefinition {
                name: ToolDefinitionName::new(name).expect("stub advertised name is valid"),
                description: format!("description:{name}"),
                parameters: view_parameters_schema(),
            },
        },
        resource,
    })
}

struct StubDispatcher;

impl ToolDispatcher for StubDispatcher {
    type Observation = Value;

    fn dispatch<'a>(
        &'a self,
        invocation: ToolInvocation,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<ToolExecution<Self::Observation>, AgentError>> {
        Box::pin(async move {
            control.check_at(Instant::now())?;
            Ok(ToolExecution::new(
                json!({"tool": invocation.name.as_str()}),
                None,
            ))
        })
    }
}

struct FixedCancellation(Option<AgentError>);

impl CancellationSignal for FixedCancellation {
    fn cancellation_error(&self) -> Option<AgentError> {
        self.0.clone()
    }

    fn cancelled(&self) -> Pin<Box<dyn Future<Output = AgentError> + Send + '_>> {
        match self.0.clone() {
            Some(error) => Box::pin(std::future::ready(error)),
            None => Box::pin(std::future::pending()),
        }
    }
}

struct FixedScope(bool);

impl ScopeGuard for FixedScope {
    fn check_current(&self) -> Result<(), AgentError> {
        if self.0 {
            Ok(())
        } else {
            Err(AgentError::new(
                AgentErrorCode::ScopeInvalid,
                "scope_invalid",
            ))
        }
    }

    fn is_current(&self) -> bool {
        self.0
    }
}

struct NoopWake;

impl Wake for NoopWake {
    fn wake(self: Arc<Self>) {}
}

fn poll_ready<T>(mut future: ContractFuture<'_, T>) -> T {
    match poll_once(future.as_mut()) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("contract test future unexpectedly yielded"),
    }
}

fn poll_once<F>(mut future: Pin<&mut F>) -> Poll<F::Output>
where
    F: Future + ?Sized,
{
    let waker = Waker::from(Arc::new(NoopWake));
    let mut context = TaskContext::from_waker(&waker);
    future.as_mut().poll(&mut context)
}

fn parse<T: DeserializeOwned>(source: &str) -> T {
    serde_json::from_str(source).expect("frozen fixture must decode")
}

fn fixture_value(source: &str) -> Value {
    parse(source)
}

fn insert_unknown(value: &mut Value, key: &str) {
    value
        .as_object_mut()
        .expect("test target is an object")
        .insert(key.to_owned(), json!(true));
}

fn assert_rejected<T: DeserializeOwned>(value: Value) {
    assert!(
        serde_json::from_value::<T>(value).is_err(),
        "invalid contract value was accepted"
    );
}
