//! Anthropic Messages wire 协议。

use serde_json::{json, Map, Value};

use crate::adapters::http::{content_as_text, merge_request_fields, model_error, WireCodec};
use crate::ports::{ModelRequest, ModelResponse};
use crate::types::{
    AgentError, ContentPart, JsonObject, ModelOutput, ModelUsage, ToolCall, ToolResultStatus,
    TranscriptItem,
};

const RAW_CONTENT_KEY: &str = "anthropic.messages.content";
const MESSAGE_ID_KEY: &str = "anthropic.messages.message_id";

/// Anthropic Messages 请求的默认输出 token 上限。
pub const DEFAULT_MAX_TOKENS: u64 = 1024;

/// Messages 请求的协议级可选字段。
///
/// Anthropic 协议要求 `max_tokens`，其值必须大于零。`additional_fields` 不得覆盖
/// `model`、`system`、`messages`、`tools`、`tool_choice` 或 `max_tokens`。本适配器不支持
/// SSE，`stream: true` 会被拒绝。
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct RequestOptions {
    pub max_tokens: u64,
    pub tool_choice: Option<Value>,
    pub additional_fields: JsonObject,
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            max_tokens: DEFAULT_MAX_TOKENS,
            tool_choice: None,
            additional_fields: JsonObject::new(),
        }
    }
}

impl RequestOptions {
    pub fn new(max_tokens: u64) -> Self {
        Self {
            max_tokens,
            ..Self::default()
        }
    }

    pub fn with_max_tokens(mut self, max_tokens: u64) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn with_tool_choice(mut self, tool_choice: impl Into<Value>) -> Self {
        self.tool_choice = Some(tool_choice.into());
        self
    }

    pub fn with_additional_field(
        mut self,
        key: impl Into<String>,
        value: impl Into<Value>,
    ) -> Self {
        self.additional_fields.insert(key.into(), value.into());
        self
    }
}

pub(crate) struct AnthropicMessagesCodec {
    options: RequestOptions,
}

impl AnthropicMessagesCodec {
    pub(crate) fn new(options: RequestOptions) -> Self {
        Self { options }
    }
}

impl WireCodec for AnthropicMessagesCodec {
    fn encode_request(&self, request: &ModelRequest, model: &str) -> Result<Value, AgentError> {
        if self.options.max_tokens == 0 {
            return Err(model_error("anthropic_messages_invalid_max_tokens"));
        }
        if request.function_tools.is_empty() && self.options.tool_choice.is_some() {
            return Err(model_error("anthropic_messages_tool_choice_without_tools"));
        }

        let (system, messages) = encode_conversation(&request.transcript)?;
        let mut body = Map::new();
        body.insert("model".to_owned(), Value::String(model.to_owned()));
        body.insert(
            "max_tokens".to_owned(),
            Value::from(self.options.max_tokens),
        );
        body.insert("messages".to_owned(), Value::Array(messages));
        if !system.is_empty() {
            body.insert("system".to_owned(), Value::Array(system));
        }
        if !request.function_tools.is_empty() {
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
                .collect();
            body.insert("tools".to_owned(), Value::Array(tools));
            if let Some(tool_choice) = &self.options.tool_choice {
                body.insert("tool_choice".to_owned(), tool_choice.clone());
            }
        }
        merge_request_fields(
            &mut body,
            &self.options.additional_fields,
            &[
                "model",
                "max_tokens",
                "system",
                "messages",
                "tools",
                "tool_choice",
            ],
            "anthropic_messages",
        )?;
        Ok(Value::Object(body))
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
