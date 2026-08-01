use std::{
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
        fixtures, move_input_parameters_schema, view_parameters_schema, CapabilityExecutionContext,
        CapabilityInvocation, ExecutionResource, MoveDirection, MoveInputArguments, ScopeGuard,
        ToolCapability, ToolCapabilityRegistry, ToolDispatcher, ToolResultProtocol, ViewArguments,
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
fn view_arguments_and_execution_enums_are_closed() {
    let view: ViewArguments = serde_json::from_value(json!({})).expect("view takes no arguments");
    assert_eq!(view, ViewArguments::default());
    assert_rejected::<ViewArguments>(json!({"direction": "north"}));

    assert_eq!(
        Value::Object(view_parameters_schema()),
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    );

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

    let context = CapabilityExecutionContext::new(
        "world-1",
        "chat-1",
        ExecutionControl::new(&active, Deadline::after(now, Duration::from_secs(1))),
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
        ExecutionControl::new(&active, Deadline::after(now, Duration::from_secs(1))),
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
    let control = ExecutionControl::new(&active, Deadline::after(now, Duration::from_secs(1)));
    let context = CapabilityExecutionContext::new("world", "chat", control, &current_scope);

    let capability = stub_capability("echo", None);
    let capability_result =
        poll_ready(capability.execute(fixtures::capability_invocation(), context))
            .expect("stub capability executes in process");
    assert_eq!(capability_result["actionId"], "action-1");

    let dispatcher = StubDispatcher;
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
    let waker = Waker::from(Arc::new(NoopWake));
    let mut context = TaskContext::from_waker(&waker);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("contract test future unexpectedly yielded"),
    }
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
