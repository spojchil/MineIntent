use mineintent_contracts::agent::{AgentErrorCode, JsonObject, ModelUsage, RunId, ToolExecution};
use mineintent_middle::agent::{
    AgentRun, AgentRunStep, AgentToolResult, ModelCompletion, PlannedToolCall,
    MAX_MODEL_REQUESTS_PER_RUN, MAX_TOOL_CALLS_PER_RESPONSE, MAX_TOOL_CALLS_PER_RUN,
};
use serde_json::{json, Value};

fn object(value: Value) -> JsonObject {
    value
        .as_object()
        .cloned()
        .expect("fixture must be an object")
}

fn new_run() -> AgentRun {
    AgentRun::new(
        RunId::new("run-1").expect("valid run id"),
        vec![
            object(json!({"role": "system", "content": "stable"})),
            object(json!({"role": "user", "content": "opening-frame"})),
        ],
    )
}

fn call(id: &str, name: Value, arguments: Value) -> Value {
    json!({
        "id": id,
        "type": "function",
        "function": {"name": name, "arguments": arguments},
    })
}

fn completion(message: Value) -> ModelCompletion {
    ModelCompletion {
        message: Some(object(message)),
        finish_reason: None,
        usage: None,
    }
}

fn request_model(run: &mut AgentRun) -> Vec<JsonObject> {
    match run.next_step().expect("model step") {
        AgentRunStep::CallModel { messages } => messages,
        _ => panic!("expected model step"),
    }
}

fn request_tools(run: &mut AgentRun) -> Vec<PlannedToolCall> {
    match run.next_step().expect("tool step") {
        AgentRunStep::CallTools { calls } => calls,
        _ => panic!("expected tool step"),
    }
}

fn complete_plans(plans: Vec<PlannedToolCall>) -> Vec<AgentToolResult> {
    plans
        .into_iter()
        .map(|plan| match plan {
            PlannedToolCall::Dispatch(invocation) => AgentToolResult::from_execution(
                invocation.tool_call_id,
                ToolExecution::<Value>::new(json!({"status": "completed"}), None),
            )
            .expect("serializable result"),
            PlannedToolCall::LocalResult(result) => result,
        })
        .collect()
}

#[test]
fn replay_preserves_reasoning_tool_ids_order_and_summed_usage() {
    let mut run = new_run();
    let opening = request_model(&mut run);
    run.model_response(ModelCompletion {
        message: Some(object(json!({
            "role": "assistant",
            "content": "",
            "reasoning_content": "need turn",
            "tool_calls": [call("call-1", json!("look_relative"), json!("{\"yaw\":90}"))],
        }))),
        finish_reason: Some(json!("tool_calls")),
        usage: Some(ModelUsage {
            input_tokens: Some(10),
            output_tokens: Some(2),
            cache_read_tokens: Some(3),
            cache_write_tokens: None,
        }),
    })
    .expect("tool response is accepted");

    let plans = request_tools(&mut run);
    assert!(matches!(&plans[0], PlannedToolCall::Dispatch(_)));
    run.tool_results(complete_plans(plans))
        .expect("paired result");

    let replay = request_model(&mut run);
    assert_eq!(&replay[..2], opening.as_slice());
    assert_eq!(replay[2]["reasoning_content"], "need turn");
    assert_eq!(replay[3]["tool_call_id"], "call-1");
    assert_eq!(
        serde_json::from_str::<Value>(replay[3]["content"].as_str().expect("tool content"))
            .expect("JSON tool content")["result"]["status"],
        "completed"
    );

    run.model_response(ModelCompletion {
        message: Some(object(json!({"role": "assistant", "content": ""}))),
        finish_reason: None,
        usage: Some(ModelUsage {
            input_tokens: Some(20),
            output_tokens: Some(3),
            cache_read_tokens: Some(4),
            cache_write_tokens: Some(1),
        }),
    })
    .expect("final response is accepted");
    match run.next_step().expect("done") {
        AgentRunStep::Done { closing, usage } => {
            assert_eq!(closing, "");
            assert_eq!(
                usage,
                Some(ModelUsage {
                    input_tokens: Some(30),
                    output_tokens: Some(5),
                    cache_read_tokens: Some(7),
                    cache_write_tokens: Some(1),
                })
            );
        }
        _ => panic!("expected done"),
    }
}

#[test]
fn tool_call_ids_are_preflighted_atomically_and_unique_for_the_run() {
    for invalid_id in ["", &"x".repeat(129), &"😀".repeat(65)] {
        let mut run = new_run();
        request_model(&mut run);
        let error = run
            .model_response(completion(json!({
                "role": "assistant",
                "tool_calls": [call(invalid_id, json!("say"), json!("{}"))],
            })))
            .expect_err("invalid id must reject the whole batch");
        assert_eq!(error.code, AgentErrorCode::InvalidToolInvocation);
        assert_eq!(run.tool_call_count(), 0);
    }

    let mut run = new_run();
    request_model(&mut run);
    let error = run
        .model_response(completion(json!({
            "role": "assistant",
            "tool_calls": [
                call("same", json!("say"), json!("{}")),
                call("same", json!("say"), json!("{}")),
            ],
        })))
        .expect_err("duplicate batch id");
    assert_eq!(error.code, AgentErrorCode::ToolCallAlreadyHandled);
    assert_eq!(run.tool_call_count(), 0);

    let mut run = new_run();
    request_model(&mut run);
    run.model_response(completion(json!({
        "role": "assistant",
        "tool_calls": [call("old-call", json!("say"), json!("{}"))],
    })))
    .expect("first claim");
    let plans = request_tools(&mut run);
    run.tool_results(complete_plans(plans))
        .expect("first result");
    request_model(&mut run);
    let error = run
        .model_response(completion(json!({
            "role": "assistant",
            "tool_calls": [call("old-call", json!("say"), json!("{}"))],
        })))
        .expect_err("run-wide id reuse");
    assert_eq!(error.code, AgentErrorCode::ToolCallAlreadyHandled);
}

#[test]
fn invalid_model_tool_data_stays_local_and_keeps_the_pair() {
    let mut run = new_run();
    request_model(&mut run);
    run.model_response(completion(json!({
        "role": "assistant",
        "tool_calls": [call("bad-name", json!("x".repeat(65)), json!("{}"))],
    })))
    .expect("valid correlation id claims the call");

    let plans = request_tools(&mut run);
    let PlannedToolCall::LocalResult(result) = &plans[0] else {
        panic!("invalid model data must not reach the dispatcher");
    };
    assert_eq!(result.tool_call_id().as_str(), "bad-name");
    assert_eq!(
        result.output(),
        &object(json!({
            "result": {"status": "failed", "summary": "invalid tool call"},
            "observationAfter": null,
        }))
    );
    run.tool_results(complete_plans(plans))
        .expect("local failure still pairs");
    assert!(matches!(
        run.next_step().expect("loop continues"),
        AgentRunStep::CallModel { .. }
    ));
}

#[test]
fn unsafe_integer_arguments_stay_local() {
    let mut run = new_run();
    request_model(&mut run);
    run.model_response(completion(json!({
        "role": "assistant",
        "tool_calls": [call(
            "unsafe-number",
            json!("say"),
            json!("{\"count\":9007199254740992}"),
        )],
    })))
    .expect("ID/function preflight succeeds");
    assert!(matches!(
        &request_tools(&mut run)[0],
        PlannedToolCall::LocalResult(_)
    ));
}

#[test]
fn tool_results_are_strictly_n_in_n_out_in_input_order() {
    fn prepare_two_calls() -> AgentRun {
        let mut run = new_run();
        request_model(&mut run);
        run.model_response(completion(json!({
            "role": "assistant",
            "tool_calls": [
                call("one", json!("move_input"), json!("{}")),
                call("two", json!("look_relative"), json!("{}")),
            ],
        })))
        .expect("two calls");
        run
    }

    let mut ordered = prepare_two_calls();
    let plans = request_tools(&mut ordered);
    ordered
        .tool_results(complete_plans(plans))
        .expect("ordered results are accepted");
    let messages = request_model(&mut ordered);
    assert_eq!(messages[messages.len() - 2]["tool_call_id"], "one");
    assert_eq!(messages[messages.len() - 1]["tool_call_id"], "two");

    let mut reordered = prepare_two_calls();
    let plans = request_tools(&mut reordered);
    let mut results = complete_plans(plans);
    results.swap(0, 1);
    let error = reordered
        .tool_results(results)
        .expect_err("reordered results must not corrupt replay order");
    assert_eq!(error.code, AgentErrorCode::InvalidToolInvocation);
}

#[test]
fn loop_limits_match_the_python_oracle() {
    assert_eq!(MAX_MODEL_REQUESTS_PER_RUN, 16);
    assert_eq!(MAX_TOOL_CALLS_PER_RESPONSE, 8);
    assert_eq!(MAX_TOOL_CALLS_PER_RUN, 32);
}

#[test]
fn tool_calls_are_capped_per_response_and_per_run_before_dispatch() {
    let mut run = new_run();
    request_model(&mut run);
    let over_response = (0..9)
        .map(|index| call(&format!("over-{index}"), json!("say"), json!("{}")))
        .collect::<Vec<_>>();
    let error = run
        .model_response(completion(json!({
            "role": "assistant",
            "tool_calls": over_response,
        })))
        .expect_err("whole over-limit response is rejected");
    assert_eq!(error.code, AgentErrorCode::LimitExceeded);
    assert_eq!(run.tool_call_count(), 0);

    let mut run = new_run();
    for response in 0..(MAX_TOOL_CALLS_PER_RUN / MAX_TOOL_CALLS_PER_RESPONSE) {
        request_model(&mut run);
        let calls = (0..MAX_TOOL_CALLS_PER_RESPONSE)
            .map(|index| {
                call(
                    &format!("response-{response}-{index}"),
                    json!("say"),
                    json!("{}"),
                )
            })
            .collect::<Vec<_>>();
        run.model_response(completion(json!({
            "role": "assistant",
            "tool_calls": calls,
        })))
        .expect("response is within both limits");
        let plans = request_tools(&mut run);
        run.tool_results(complete_plans(plans))
            .expect("paired batch");
    }
    assert_eq!(run.tool_call_count(), MAX_TOOL_CALLS_PER_RUN);
    request_model(&mut run);
    let error = run
        .model_response(completion(json!({
            "role": "assistant",
            "tool_calls": [call("call-33", json!("say"), json!("{}"))],
        })))
        .expect_err("33rd call is rejected before dispatch");
    assert_eq!(error.code, AgentErrorCode::LimitExceeded);
    assert_eq!(run.tool_call_count(), MAX_TOOL_CALLS_PER_RUN);
}

#[test]
fn model_requests_stop_at_sixteen() {
    let mut run = new_run();
    for index in 0..MAX_MODEL_REQUESTS_PER_RUN {
        request_model(&mut run);
        run.model_response(completion(json!({
            "role": "assistant",
            "tool_calls": [call(&format!("call-{index}"), json!("say"), json!("{}"))],
        })))
        .expect("response within request limit");
        let plans = request_tools(&mut run);
        run.tool_results(complete_plans(plans))
            .expect("paired result");
    }
    let error = run
        .next_step()
        .expect_err("17th model request is not emitted");
    assert_eq!(error.code, AgentErrorCode::LimitExceeded);
    assert_eq!(run.model_request_count(), MAX_MODEL_REQUESTS_PER_RUN);
}

#[test]
fn finish_reason_is_allowlisted_and_missing_or_null_is_accepted() {
    for reason in [
        None,
        Some(Value::Null),
        Some(json!("stop")),
        Some(json!("tool_calls")),
        Some(json!("function_call")),
    ] {
        let mut run = new_run();
        request_model(&mut run);
        run.model_response(ModelCompletion {
            message: Some(object(json!({"role": "assistant", "content": "done"}))),
            finish_reason: reason,
            usage: None,
        })
        .expect("accepted finish reason");
    }

    for reason in [
        json!("length"),
        json!("content_filter"),
        json!(""),
        json!(7),
    ] {
        let mut run = new_run();
        request_model(&mut run);
        let error = run
            .model_response(ModelCompletion {
                message: Some(object(json!({
                    "role": "assistant",
                    "content": "half-written",
                }))),
                finish_reason: Some(reason.clone()),
                usage: None,
            })
            .expect_err("non-terminal finish reason must fail");
        assert_eq!(error.code, AgentErrorCode::ProviderFailed);
        if let Some(reason) = reason.as_str().filter(|reason| !reason.is_empty()) {
            assert!(error.summary.contains(reason));
        }
    }
}

#[test]
fn constructed_round_frame_is_appended_after_every_tool_message() {
    let mut run = new_run();
    request_model(&mut run);
    run.model_response(completion(json!({
        "role": "assistant",
        "tool_calls": [
            call("body-one", json!("move_input"), json!("{}")),
            call("body-two", json!("look_relative"), json!("{}")),
        ],
    })))
    .expect("body batch is accepted");

    let plans = request_tools(&mut run);
    run.tool_results(complete_plans(plans))
        .expect("tool messages are paired");
    run.append_user_message(object(json!({
        "role": "user",
        "content": "{\"protocol\":\"mineintent.viewport-frame.v1\"}",
    })))
    .expect("driver-constructed user frame is accepted");

    let messages = request_model(&mut run);
    assert_eq!(messages[messages.len() - 3]["role"], "tool");
    assert_eq!(messages[messages.len() - 2]["role"], "tool");
    assert_eq!(messages[messages.len() - 1]["role"], "user");
    assert!(messages[messages.len() - 1]["content"]
        .as_str()
        .expect("frame content")
        .contains("mineintent.viewport-frame.v1"));
}
