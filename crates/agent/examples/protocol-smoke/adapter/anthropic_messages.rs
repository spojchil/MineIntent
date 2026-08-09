//! Anthropic Messages API wire codec。

use agent::{
    AgentError, ContentPart, JsonObject, ModelOutput, ModelRequest, ModelResponse, ModelUsage,
    ToolCall, ToolResultStatus, TranscriptItem,
};
use serde_json::{json, Value};

use super::transport::{content_as_text, model_error, AuthStyle, WireCodec};

const RAW_CONTENT_KEY: &str = "anthropic.messages.content";
const MESSAGE_ID_KEY: &str = "anthropic.messages.message_id";

pub(super) struct AnthropicMessagesCodec;

impl WireCodec for AnthropicMessagesCodec {
    fn auth_style(&self) -> AuthStyle {
        AuthStyle::Anthropic
    }

    fn encode_request(&self, request: &ModelRequest, model: &str) -> Result<Value, AgentError> {
        let (system, messages) = encode_conversation(&request.transcript)?;
        let tools = request
            .function_tools
            .iter()
            .map(|tool| {
                let mut definition = json!({
                    "name": tool.name.as_str(),
                    "input_schema": tool.input_schema,
                });
                if let Some(description) = &tool.description {
                    definition["description"] = Value::String(description.clone());
                }
                definition
            })
            .collect::<Vec<_>>();
        let mut body = json!({
            "model": model,
            "max_tokens": 256,
            "messages": messages,
            "tools": tools,
            "tool_choice": {"type": "auto"},
            "stream": false,
        });
        if !system.is_empty() {
            body["system"] = Value::Array(system);
        }
        Ok(body)
    }

    fn decode_response(&self, value: Value) -> Result<ModelResponse, AgentError> {
        let blocks = value
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| model_error("anthropic_messages_missing_content"))?;
        let mut content = Vec::new();
        let mut tool_calls = Vec::new();

        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let text = block
                        .get("text")
                        .and_then(Value::as_str)
                        .ok_or_else(|| model_error("anthropic_messages_text_missing_value"))?;
                    content.push(ContentPart::text(text));
                }
                Some("tool_use") => tool_calls.push(decode_tool_use(block)?),
                Some(kind) => content.push(ContentPart::Opaque {
                    kind: format!("anthropic.messages.{kind}"),
                    data: block.clone(),
                }),
                None => return Err(model_error("anthropic_messages_content_missing_type")),
            }
        }

        let mut provider_data = JsonObject::new();
        // 原始内容块保留签名、服务端扩展字段及其顺序，续轮时直接回放。
        provider_data.insert(RAW_CONTENT_KEY.to_owned(), Value::Array(blocks.clone()));
        if let Some(message_id) = value.get("id") {
            provider_data.insert(MESSAGE_ID_KEY.to_owned(), message_id.clone());
        }
        let usage = value.get("usage").map(|usage| {
            let input_tokens = usage.get("input_tokens").and_then(Value::as_u64);
            let output_tokens = usage.get("output_tokens").and_then(Value::as_u64);
            let cached_input_tokens = usage.get("cache_read_input_tokens").and_then(Value::as_u64);
            let cache_write_input_tokens = usage
                .get("cache_creation_input_tokens")
                .and_then(Value::as_u64);
            let total_tokens = [
                input_tokens,
                output_tokens,
                cached_input_tokens,
                cache_write_input_tokens,
            ]
            .into_iter()
            .flatten()
            .fold(None, |total, value| {
                Some(total.unwrap_or(0_u64).saturating_add(value))
            });
            ModelUsage {
                input_tokens,
                output_tokens,
                total_tokens,
                cached_input_tokens,
                cache_write_input_tokens,
                reasoning_output_tokens: None,
            }
        });

        Ok(ModelResponse {
            output: ModelOutput {
                content,
                tool_calls,
                provider_data,
            },
            finish_reason: value.get("stop_reason").cloned(),
            usage,
        })
    }
}

fn encode_conversation(
    transcript: &[TranscriptItem],
) -> Result<(Vec<Value>, Vec<Value>), AgentError> {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    for item in transcript {
        match item {
            TranscriptItem::Input(message) => match message.role.as_str() {
                "system" | "developer" if messages.is_empty() => {
                    system.extend(content_blocks(&message.content));
                }
                "system" | "developer" => {
                    return Err(model_error(
                        "anthropic_messages_mid_conversation_system_message",
                    ));
                }
                "user" | "assistant" => {
                    push_message(
                        &mut messages,
                        &message.role,
                        content_blocks(&message.content),
                    )?;
                }
                role => {
                    return Err(model_error(format!(
                        "anthropic_messages_unsupported_input_role:{role}"
                    )));
                }
            },
            TranscriptItem::ModelOutput(output) => {
                let blocks = output
                    .provider_data
                    .get(RAW_CONTENT_KEY)
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_else(|| canonical_assistant_blocks(output));
                push_message(&mut messages, "assistant", blocks)?;
            }
            TranscriptItem::ToolResults(batch) => {
                let blocks = batch
                    .results
                    .iter()
                    .map(|result| {
                        json!({
                            "type": "tool_result",
                            "tool_use_id": result.call_id.as_str(),
                            "content": content_as_text(&result.content),
                            "is_error": result.status == ToolResultStatus::Error,
                        })
                    })
                    .collect();
                // 完整批次必须位于同一条 user message；后续 steering 文本会追加在其后。
                push_message(&mut messages, "user", blocks)?;
            }
        }
    }
    Ok((system, messages))
}

fn content_blocks(parts: &[ContentPart]) -> Vec<Value> {
    parts
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => json!({"type": "text", "text": text}),
            ContentPart::Json { value } => {
                json!({"type": "text", "text": value.to_string()})
            }
            ContentPart::Opaque { kind, data } if kind.starts_with("anthropic.messages.") => {
                data.clone()
            }
            ContentPart::Opaque { data, .. } => {
                json!({"type": "text", "text": data.to_string()})
            }
        })
        .collect()
}

fn canonical_assistant_blocks(output: &ModelOutput) -> Vec<Value> {
    let mut blocks = content_blocks(&output.content);
    blocks.extend(output.tool_calls.iter().map(|call| {
        json!({
            "type": "tool_use",
            "id": call.id.as_str(),
            "name": call.name.as_str(),
            "input": call.arguments,
        })
    }));
    blocks
}

fn push_message(
    messages: &mut Vec<Value>,
    role: &str,
    mut blocks: Vec<Value>,
) -> Result<(), AgentError> {
    if blocks.is_empty() {
        return Ok(());
    }
    if let Some(last) = messages.last_mut() {
        let same_role = last.get("role").and_then(Value::as_str) == Some(role);
        if same_role {
            let content = last
                .get_mut("content")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| model_error("anthropic_messages_invalid_built_message"))?;
            content.append(&mut blocks);
            return Ok(());
        }
    }
    messages.push(json!({"role": role, "content": blocks}));
    Ok(())
}

fn decode_tool_use(block: &Value) -> Result<ToolCall, AgentError> {
    let id = required_string(block, "id", "anthropic_messages_tool_use_missing_id")?;
    let name = required_string(block, "name", "anthropic_messages_tool_use_missing_name")?;
    let input = block
        .get("input")
        .ok_or_else(|| model_error("anthropic_messages_tool_use_missing_input"))?
        .clone();
    Ok(ToolCall::new(id, name, input))
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
