//! OpenAI Chat Completions wire codec。

use agent::{
    AgentError, ContentPart, JsonObject, ModelOutput, ModelRequest, ModelResponse, ModelUsage,
    ToolCall, TranscriptItem,
};
use serde_json::{json, Value};

use super::transport::{content_as_text, model_error, parse_arguments, AuthStyle, WireCodec};

const RAW_MESSAGE_KEY: &str = "openai.chat.message";
const RESPONSE_ID_KEY: &str = "openai.chat.response_id";

pub(super) struct OpenAiChatCodec;

impl WireCodec for OpenAiChatCodec {
    fn auth_style(&self) -> AuthStyle {
        AuthStyle::Bearer
    }

    fn encode_request(&self, request: &ModelRequest, model: &str) -> Result<Value, AgentError> {
        let messages = encode_messages(&request.transcript)?;
        let tools = request
            .function_tools
            .iter()
            .map(|tool| {
                let mut function = json!({
                    "name": tool.name.as_str(),
                    "parameters": tool.input_schema,
                });
                if let Some(description) = &tool.description {
                    function["description"] = Value::String(description.clone());
                }
                json!({"type": "function", "function": function})
            })
            .collect::<Vec<_>>();

        Ok(json!({
            "model": model,
            "messages": messages,
            "tools": tools,
            "tool_choice": "auto",
            "max_tokens": 256,
            "stream": false,
        }))
    }

    fn decode_response(&self, value: Value) -> Result<ModelResponse, AgentError> {
        let choice = value
            .pointer("/choices/0")
            .ok_or_else(|| model_error("openai_chat_response_missing_choice"))?;
        let message = choice
            .get("message")
            .ok_or_else(|| model_error("openai_chat_response_missing_message"))?;

        let content = message
            .get("content")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(|text| vec![ContentPart::text(text)])
            .unwrap_or_default();
        let tool_calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(|calls| {
                calls
                    .iter()
                    .map(|call| {
                        let id = required_string(call, "id", "openai_chat_tool_call_missing_id")?;
                        let function = call
                            .get("function")
                            .ok_or_else(|| model_error("openai_chat_tool_call_missing_function"))?;
                        let name = required_string(
                            function,
                            "name",
                            "openai_chat_tool_call_missing_name",
                        )?;
                        let arguments = required_string(
                            function,
                            "arguments",
                            "openai_chat_tool_call_missing_arguments",
                        )?;
                        Ok(ToolCall::new(id, name, parse_arguments(arguments)))
                    })
                    .collect::<Result<Vec<_>, AgentError>>()
            })
            .transpose()?
            .unwrap_or_default();

        let mut provider_data = JsonObject::new();
        // 原始消息可无损保留兼容端点添加的未知字段，并在续轮时完整回放。
        provider_data.insert(RAW_MESSAGE_KEY.to_owned(), message.clone());
        if let Some(response_id) = value.get("id") {
            provider_data.insert(RESPONSE_ID_KEY.to_owned(), response_id.clone());
        }

        let usage = value.get("usage").map(|usage| ModelUsage {
            input_tokens: usage.get("prompt_tokens").and_then(Value::as_u64),
            output_tokens: usage.get("completion_tokens").and_then(Value::as_u64),
            total_tokens: usage.get("total_tokens").and_then(Value::as_u64),
            cached_input_tokens: usage
                .pointer("/prompt_tokens_details/cached_tokens")
                .and_then(Value::as_u64),
            cache_write_input_tokens: None,
            reasoning_output_tokens: usage
                .pointer("/completion_tokens_details/reasoning_tokens")
                .and_then(Value::as_u64),
        });

        Ok(ModelResponse {
            output: ModelOutput {
                content,
                tool_calls,
                provider_data,
            },
            finish_reason: choice.get("finish_reason").cloned(),
            usage,
        })
    }
}

fn encode_messages(transcript: &[TranscriptItem]) -> Result<Vec<Value>, AgentError> {
    let mut messages = Vec::new();
    for item in transcript {
        match item {
            TranscriptItem::Input(message) => {
                let role = wire_role(&message.role)?;
                messages.push(json!({
                    "role": role,
                    "content": content_as_text(&message.content),
                }));
            }
            TranscriptItem::ModelOutput(output) => {
                if let Some(message) = output.provider_data.get(RAW_MESSAGE_KEY) {
                    // 服务端签发的调用结构和未知扩展字段必须与原响应一起回放。
                    messages.push(message.clone());
                    continue;
                }
                let mut message = json!({
                    "role": "assistant",
                    "content": if output.content.is_empty() {
                        Value::Null
                    } else {
                        Value::String(content_as_text(&output.content))
                    },
                });
                if !output.tool_calls.is_empty() {
                    message["tool_calls"] = Value::Array(
                        output
                            .tool_calls
                            .iter()
                            .map(|call| {
                                json!({
                                    "id": call.id.as_str(),
                                    "type": "function",
                                    "function": {
                                        "name": call.name.as_str(),
                                        "arguments": match &call.arguments {
                                            Value::String(raw) => raw.clone(),
                                            value => value.to_string(),
                                        },
                                    }
                                })
                            })
                            .collect(),
                    );
                }
                messages.push(message);
            }
            TranscriptItem::ToolResults(batch) => {
                messages.extend(batch.results.iter().map(|result| {
                    json!({
                        "role": "tool",
                        "tool_call_id": result.call_id.as_str(),
                        "content": content_as_text(&result.content),
                    })
                }));
            }
        }
    }
    Ok(messages)
}

fn wire_role(role: &str) -> Result<&str, AgentError> {
    match role {
        "developer" | "system" | "user" | "assistant" => Ok(role),
        _ => Err(model_error(format!(
            "openai_chat_unsupported_input_role:{role}"
        ))),
    }
}

fn required_string<'a>(
    value: &'a Value,
    field: &str,
    error: &'static str,
) -> Result<&'a str, AgentError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| model_error(error))
}
