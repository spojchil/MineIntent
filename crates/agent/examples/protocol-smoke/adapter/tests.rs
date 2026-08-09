//! 只验证 wire 投影，不访问环境变量或网络。

use agent::{
    InputMessage, ModelRequest, ToolCallId, ToolDefinition, ToolResult, ToolResultBatch,
    TranscriptItem,
};
use serde_json::json;

use super::anthropic_messages::AnthropicMessagesCodec;
use super::openai_chat::OpenAiChatCodec;
use super::openai_responses::OpenAiResponsesCodec;
use super::transport::WireCodec;

#[test]
fn chat_uses_tool_call_id_as_the_canonical_id() {
    let codec = OpenAiChatCodec;
    let response = codec
        .decode_response(json!({
            "id": "chat-response",
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_chat_1",
                        "type": "function",
                        "function": {"name": "add", "arguments": "{\"left\":2,\"right\":3}"}
                    }],
                    "compatible_endpoint_extension": {"opaque": true}
                }
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 4, "total_tokens": 14}
        }))
        .unwrap();

    assert_eq!(response.output.tool_calls[0].id.as_str(), "call_chat_1");
    assert_eq!(response.output.tool_calls[0].arguments["left"], 2);

    let request = ModelRequest {
        transcript: vec![
            InputMessage::text("developer", "只按协议执行").into(),
            TranscriptItem::ModelOutput(response.output),
        ],
        function_tools: Vec::new(),
    };
    let body = codec.encode_request(&request, "test-model").unwrap();
    assert_eq!(body["model"], "test-model");
    assert!(body.get("thinking").is_none());
    assert_eq!(body["messages"][0]["role"], "developer");
    assert_eq!(body["messages"][1]["tool_calls"][0]["id"], "call_chat_1");
    assert_eq!(
        body["messages"][1]["compatible_endpoint_extension"]["opaque"],
        true
    );
}

#[test]
fn responses_preserves_output_items_and_correlates_with_call_id() {
    let codec = OpenAiResponsesCodec;
    let response = codec
        .decode_response(json!({
            "id": "resp_1",
            "status": "completed",
            "output": [
                {"id": "rs_1", "type": "reasoning", "content": []},
                {
                    "id": "fc_1",
                    "type": "function_call",
                    "status": "completed",
                    "call_id": "call_responses_1",
                    "name": "add",
                    "arguments": "{\"left\":2,\"right\":3}"
                },
                {
                    "id": "fc_2",
                    "type": "function_call",
                    "status": "completed",
                    "call_id": "call_responses_2",
                    "name": "add",
                    "arguments": "{\"left\":5,\"right\":7}"
                }
            ],
            "usage": {"input_tokens": 20, "output_tokens": 8, "total_tokens": 28}
        }))
        .unwrap();
    assert_eq!(
        response.output.tool_calls[0].id.as_str(),
        "call_responses_1"
    );
    assert_eq!(
        response.output.tool_calls[0].provider_data["openai.responses.item_id"],
        "fc_1"
    );

    let request = continuation_request(response.output);
    let body = codec.encode_request(&request, "test-model").unwrap();
    assert_eq!(body["model"], "test-model");
    let input = body["input"].as_array().unwrap();
    assert_eq!(input[1]["id"], "rs_1");
    assert_eq!(input[2]["id"], "fc_1");
    assert_eq!(input[2]["call_id"], "call_responses_1");

    let outputs = input
        .iter()
        .filter(|item| item["type"] == "function_call_output")
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0]["call_id"], "call_responses_1");
    assert_eq!(outputs[1]["call_id"], "call_responses_2");
    assert_eq!(input.last().unwrap()["content"], "结果齐全后简短回答");
}

#[test]
fn anthropic_groups_the_complete_result_batch_before_steering_text() {
    let codec = AnthropicMessagesCodec;
    let response = codec
        .decode_response(json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "stop_reason": "tool_use",
            "content": [
                {"type": "thinking", "thinking": "opaque", "signature": "sig"},
                {"type": "tool_use", "id": "toolu_1", "name": "add", "input": {"left": 2, "right": 3}},
                {"type": "tool_use", "id": "toolu_2", "name": "add", "input": {"left": 5, "right": 7}}
            ],
            "usage": {"input_tokens": 30, "output_tokens": 10}
        }))
        .unwrap();
    assert_eq!(response.output.tool_calls[0].id.as_str(), "toolu_1");
    assert_eq!(response.usage.unwrap().total_tokens, Some(40));

    let request = continuation_request(response.output);
    let body = codec.encode_request(&request, "test-model").unwrap();
    assert_eq!(body["model"], "test-model");
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"][0]["signature"], "sig");

    let final_user = messages[2]["content"].as_array().unwrap();
    assert_eq!(
        final_user
            .iter()
            .map(|block| block["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["tool_result", "tool_result", "text"]
    );
    assert_eq!(final_user[0]["tool_use_id"], "toolu_1");
    assert_eq!(final_user[1]["tool_use_id"], "toolu_2");
    assert_eq!(final_user[2]["text"], "结果齐全后简短回答");
}

#[test]
fn anthropic_extracts_initial_system_context_and_rejects_late_system_input() {
    let codec = AnthropicMessagesCodec;
    let request = ModelRequest {
        transcript: vec![
            InputMessage::text("system", "基础约束").into(),
            InputMessage::text("developer", "本轮约束").into(),
            InputMessage::text("user", "开始").into(),
        ],
        function_tools: Vec::new(),
    };
    let body = codec.encode_request(&request, "test-model").unwrap();
    assert_eq!(body["system"].as_array().unwrap().len(), 2);
    assert_eq!(body["messages"][0]["role"], "user");

    let late = ModelRequest {
        transcript: vec![
            InputMessage::text("user", "开始").into(),
            InputMessage::text("system", "过晚约束").into(),
        ],
        function_tools: Vec::new(),
    };
    assert!(codec.encode_request(&late, "test-model").is_err());
}

fn continuation_request(output: agent::ModelOutput) -> ModelRequest {
    let calls = output.tool_calls.clone();
    ModelRequest {
        transcript: vec![
            InputMessage::text("user", "调用两次 add").into(),
            TranscriptItem::ModelOutput(output),
            TranscriptItem::ToolResults(ToolResultBatch {
                results: calls
                    .iter()
                    .enumerate()
                    .map(|(index, call)| {
                        ToolResult::success_json(
                            ToolCallId::new(call.id.as_str()),
                            json!({"sum": if index == 0 { 5 } else { 12 }}),
                        )
                    })
                    .collect(),
            }),
            InputMessage::text("user", "结果齐全后简短回答").into(),
        ],
        function_tools: vec![ToolDefinition::new(
            "add",
            json!({"type": "object", "properties": {}}),
        )],
    }
}

#[test]
fn responses_rejects_an_item_id_without_a_call_id() {
    let error = OpenAiResponsesCodec
        .decode_response(json!({
            "status": "completed",
            "output": [{
                "id": "fc_only",
                "type": "function_call",
                "name": "add",
                "arguments": "{}"
            }]
        }))
        .unwrap_err();
    assert_eq!(error.kind, agent::AgentErrorKind::Model);
}

#[test]
fn responses_rejects_an_incomplete_response() {
    let error = OpenAiResponsesCodec
        .decode_response(json!({
            "status": "incomplete",
            "output": []
        }))
        .unwrap_err();
    assert_eq!(error.summary, "openai_responses_not_completed:incomplete");
}
