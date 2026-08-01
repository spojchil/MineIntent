use std::time::{Duration, Instant};

use mineintent_contracts::agent::{
    fixtures, AgentContextProtocol, AgentError, AgentErrorCode, AgentRunRequest,
    CancellationSignal, Deadline, ExecutionControl, JsonAgentDecisionContext, ModelRunResult,
    ModelUsage, RequiredNullable, ToolCallKey, ToolDefinitionName, ToolExecution, ToolInvocation,
    ToolName, WireToolDefinition,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

const AGENT_CONTEXT: &str = include_str!("testdata/agent-context.v3.json");
const TOOL_DEFINITION: &str = include_str!("testdata/tool-definition.function.json");
const TOOL_INVOCATION: &str = include_str!("testdata/tool-invocation.valid.json");
const TOOL_EXECUTION: &str = include_str!("testdata/tool-execution.v2.json");
const AGENT_RUN: &str = include_str!("testdata/agent-run.v1.json");

#[test]
fn deterministic_fixtures_match_the_frozen_wire_examples() {
    let context: JsonAgentDecisionContext = parse(AGENT_CONTEXT);
    let definition: WireToolDefinition = parse(TOOL_DEFINITION);
    let invocation: ToolInvocation = parse(TOOL_INVOCATION);

    assert_eq!(context, fixtures::agent_context());
    assert_eq!(definition, fixtures::tool_definition());
    assert_eq!(invocation, fixtures::tool_invocation());
    assert_eq!(fixtures::agent_frame(), fixtures::agent_frame());
    assert_eq!(fixtures::agent_run_request().run_id.as_str(), "run-1");
    assert_eq!(fixtures::model_run_result(), fixtures::model_run_result());
}

#[test]
fn context_v3_round_trips_with_strict_outer_shapes() {
    let context: JsonAgentDecisionContext = parse(AGENT_CONTEXT);
    let encoded = serde_json::to_value(&context).expect("context serializes");
    assert_eq!(encoded, fixture_value(AGENT_CONTEXT));
    assert_eq!(context.protocol, AgentContextProtocol::V3);

    let mut wrong_protocol = fixture_value(AGENT_CONTEXT);
    wrong_protocol["protocol"] = json!("mineintent.agent-context.v2");
    assert_rejected::<JsonAgentDecisionContext>(wrong_protocol);

    let mut unknown_top_level = fixture_value(AGENT_CONTEXT);
    insert_unknown(&mut unknown_top_level, "profile");
    assert_rejected::<JsonAgentDecisionContext>(unknown_top_level);

    let mut unknown_frame = fixture_value(AGENT_CONTEXT);
    insert_unknown(&mut unknown_frame["frame"], "viewport");
    assert_rejected::<JsonAgentDecisionContext>(unknown_frame);

    let mut unknown_world = fixture_value(AGENT_CONTEXT);
    insert_unknown(&mut unknown_world["frame"]["world"], "weather");
    assert_rejected::<JsonAgentDecisionContext>(unknown_world);

    let mut unknown_event = fixture_value(AGENT_CONTEXT);
    unknown_event["frame"]["events"] = json!([{
        "type": "damage",
        "summary": "受到伤害",
        "amount": 1
    }]);
    assert_rejected::<JsonAgentDecisionContext>(unknown_event);
}

#[test]
fn optional_frame_fields_reject_explicit_null_and_non_finite_output() {
    for key in [
        "player",
        "self",
        "status",
        "inventory",
        "sound",
        "omittedEvents",
    ] {
        let mut value = fixture_value(AGENT_CONTEXT);
        value["frame"][key] = Value::Null;
        assert_rejected::<JsonAgentDecisionContext>(value);
    }

    let mut null_time = fixture_value(AGENT_CONTEXT);
    null_time["frame"]["world"]["timeOfDay"] = Value::Null;
    assert_rejected::<JsonAgentDecisionContext>(null_time);

    let mut frame = fixtures::agent_frame();
    frame
        .self_state
        .as_mut()
        .expect("fixture has self observation")
        .yaw_degrees = f64::NAN;
    assert!(serde_json::to_value(frame).is_err());

    let mut frame = fixtures::agent_frame();
    frame.world.time_of_day = Some(f64::INFINITY);
    assert!(serde_json::to_value(frame).is_err());
}

#[test]
fn information_owned_payloads_remain_opaque_inside_strict_context_envelopes() {
    let mut value = fixture_value(AGENT_CONTEXT);
    value["stable"]["memories"][0]["futureMemoryField"] = json!({"from": "I01"});
    value["frame"]["status"] = json!({"futureStatusField": [1, 2, 3]});

    let decoded: JsonAgentDecisionContext =
        serde_json::from_value(value.clone()).expect("A-owned payloads are opaque here");
    assert_eq!(
        decoded.stable.memories[0]["futureMemoryField"],
        json!({"from": "I01"})
    );
    assert_eq!(
        decoded.frame.status,
        Some(json!({"futureStatusField": [1, 2, 3]}))
    );
}

#[test]
fn advertised_tool_definition_is_strict_and_provider_safe() {
    let definition: WireToolDefinition = parse(TOOL_DEFINITION);
    assert_eq!(
        serde_json::to_value(definition).expect("definition serializes"),
        fixture_value(TOOL_DEFINITION)
    );

    for name in ["", "has space", "看", &"a".repeat(65)] {
        let mut value = fixture_value(TOOL_DEFINITION);
        value["function"]["name"] = json!(name);
        assert_rejected::<WireToolDefinition>(value);
    }
    assert!(ToolDefinitionName::new("say-2").is_ok());

    let mut wrong_type = fixture_value(TOOL_DEFINITION);
    wrong_type["type"] = json!("client_tool");
    assert_rejected::<WireToolDefinition>(wrong_type);

    let mut unknown_function_field = fixture_value(TOOL_DEFINITION);
    insert_unknown(&mut unknown_function_field["function"], "strict");
    assert_rejected::<WireToolDefinition>(unknown_function_field);

    let mut non_object_parameters = fixture_value(TOOL_DEFINITION);
    non_object_parameters["function"]["parameters"] = json!([]);
    assert_rejected::<WireToolDefinition>(non_object_parameters);
}

#[test]
fn invocation_preserves_open_tool_name_and_arguments_but_validates_keys() {
    let invocation: ToolInvocation = parse(TOOL_INVOCATION);
    assert_eq!(ToolCallKey::from(&invocation).run_id.as_str(), "run-1");

    let complex_arguments = json!({
        "nested": {"future": true},
        "array": [null, 1, "two"],
        "unicode": "羊"
    });
    let mut value = fixture_value(TOOL_INVOCATION);
    value["name"] = json!("未来工具");
    value["arguments"] = complex_arguments.clone();
    let decoded: ToolInvocation = serde_json::from_value(value).expect("open name is accepted");
    assert_eq!(decoded.name, ToolName::new("未来工具").unwrap());
    assert_eq!(
        decoded.arguments,
        complex_arguments.as_object().unwrap().clone()
    );

    for (field, invalid) in [
        ("runId", String::new()),
        ("runId", "r".repeat(129)),
        ("toolCallId", String::new()),
        ("toolCallId", "c".repeat(129)),
        ("toolCallId", "包含空格".to_owned()),
        ("name", String::new()),
        ("name", "n".repeat(65)),
    ] {
        let mut value = fixture_value(TOOL_INVOCATION);
        value[field] = json!(invalid);
        assert_rejected::<ToolInvocation>(value);
    }

    let mut unknown_round = fixture_value(TOOL_INVOCATION);
    insert_unknown(&mut unknown_round, "round");
    assert_rejected::<ToolInvocation>(unknown_round);

    let mut non_object_arguments = fixture_value(TOOL_INVOCATION);
    non_object_arguments["arguments"] = json!([]);
    assert_rejected::<ToolInvocation>(non_object_arguments);

    let invalid_number = TOOL_INVOCATION.replace("10", "NaN");
    assert!(serde_json::from_str::<ToolInvocation>(&invalid_number).is_err());
}

#[test]
fn tool_execution_v2_requires_nullable_observation_and_rejects_transport_legacy() {
    let execution: ToolExecution = parse(TOOL_EXECUTION);
    assert_eq!(execution.observation_after.as_ref(), None);

    let mut with_observation = fixture_value(TOOL_EXECUTION);
    with_observation["observationAfter"] =
        serde_json::to_value(fixtures::agent_frame()).expect("fixture frame serializes");
    let decoded: ToolExecution =
        serde_json::from_value(with_observation).expect("frame observation is accepted");
    assert!(decoded.observation_after.as_ref().is_some());

    let mut missing_observation = fixture_value(TOOL_EXECUTION);
    missing_observation
        .as_object_mut()
        .unwrap()
        .remove("observationAfter");
    assert_rejected::<ToolExecution>(missing_observation);

    let mut wrong_protocol = fixture_value(TOOL_EXECUTION);
    wrong_protocol["protocol"] = json!("mineintent.tool-response.v1");
    assert_rejected::<ToolExecution>(wrong_protocol);

    let mut legacy_round = fixture_value(TOOL_EXECUTION);
    insert_unknown(&mut legacy_round, "roundId");
    assert_rejected::<ToolExecution>(legacy_round);

    let mut wrong_observation = fixture_value(TOOL_EXECUTION);
    wrong_observation["observationAfter"] = json!([]);
    assert_rejected::<ToolExecution>(wrong_observation);

    let constructed: ToolExecution<Value> = ToolExecution::new(json!(null), None);
    assert_eq!(constructed.observation_after, RequiredNullable::new(None));
}

#[test]
fn run_request_uses_external_prompt_reference_and_excludes_transport_configuration() {
    let request: AgentRunRequest<JsonAgentDecisionContext> = parse(AGENT_RUN);
    let encoded = serde_json::to_value(&request).expect("run request serializes");
    assert_eq!(
        encoded["promptTemplate"],
        json!({
            "key": "participant-system",
            "version": "v1"
        })
    );
    assert!(encoded.get("prompt").is_none());

    for legacy in [
        "callbackUrl",
        "callbackToken",
        "toolCallbackUrl",
        "serviceToken",
    ] {
        let mut value = fixture_value(AGENT_RUN);
        insert_unknown(&mut value, legacy);
        assert_rejected::<AgentRunRequest<JsonAgentDecisionContext>>(value);
    }

    let mut embedded_prompt = fixture_value(AGENT_RUN);
    embedded_prompt["promptTemplate"]["text"] = json!("hard-coded prompt");
    assert_rejected::<AgentRunRequest<JsonAgentDecisionContext>>(embedded_prompt);

    let mut too_many_tools = fixture_value(AGENT_RUN);
    too_many_tools["tools"] = Value::Array(vec![fixture_value(TOOL_DEFINITION); 33]);
    assert_rejected::<AgentRunRequest<JsonAgentDecisionContext>>(too_many_tools);
}

#[test]
fn model_result_usage_and_structured_errors_are_versioned_and_strict() {
    let result = fixtures::model_run_result();
    let value = serde_json::to_value(&result).expect("model result serializes");
    let decoded: ModelRunResult =
        serde_json::from_value(value.clone()).expect("model result round trips");
    assert_eq!(decoded, result);

    let mut wrong_protocol = value.clone();
    wrong_protocol["protocol"] = json!("mineintent.agent-run.v0");
    assert_rejected::<ModelRunResult>(wrong_protocol);

    let mut negative_usage = value.clone();
    negative_usage["usage"]["inputTokens"] = json!(-1);
    assert_rejected::<ModelRunResult>(negative_usage);

    let mut null_usage_field = value.clone();
    null_usage_field["usage"]["outputTokens"] = Value::Null;
    assert_rejected::<ModelRunResult>(null_usage_field);

    let mut unknown_usage = value;
    insert_unknown(&mut unknown_usage["usage"], "reasoningTokens");
    assert_rejected::<ModelRunResult>(unknown_usage);

    let error = AgentError::new(AgentErrorCode::UnknownTool, "unknown_tool:future");
    let encoded = serde_json::to_value(&error).expect("error serializes");
    assert_eq!(encoded["code"], "unknown_tool");
    assert_eq!(error.to_string(), "unknown_tool: unknown_tool:future");

    assert_rejected::<AgentError>(json!({"code": "future_error", "summary": "future"}));
    assert_rejected::<AgentError>(json!({
        "code": "unknown_tool",
        "summary": "future",
        "detail": "not frozen"
    }));
}

#[test]
fn cancellation_precedes_deadline_and_deadline_is_deterministic() {
    let now = Instant::now();
    let deadline = Deadline::after(now, Duration::from_millis(10));
    let active = FixedCancellation(None);
    let active_control = ExecutionControl::new(&active, deadline);
    assert!(active_control.check_at(now).is_ok());
    assert_eq!(
        active_control
            .check_at(now + Duration::from_millis(10))
            .unwrap_err()
            .code,
        AgentErrorCode::DeadlineExceeded
    );

    let cancelled = FixedCancellation(Some(AgentError::run_cancelled()));
    let cancelled_control = ExecutionControl::new(&cancelled, Deadline::at(now));
    assert_eq!(
        cancelled_control.check_at(now).unwrap_err().code,
        AgentErrorCode::RunCancelled
    );
}

#[test]
fn model_usage_defaults_to_absent_counters_without_accepting_null() {
    let usage: ModelUsage = serde_json::from_value(json!({})).expect("empty usage is valid");
    assert_eq!(usage, ModelUsage::default());
    assert_rejected::<ModelUsage>(json!({"cacheReadTokens": null}));
}

struct FixedCancellation(Option<AgentError>);

impl CancellationSignal for FixedCancellation {
    fn cancellation_error(&self) -> Option<AgentError> {
        self.0.clone()
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
