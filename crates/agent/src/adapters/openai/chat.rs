//! OpenAI Chat Completions wire 协议。

use serde_json::{json, Map, Value};

use crate::adapters::http::{
    content_as_text, merge_request_fields, model_error, parse_arguments, WireCodec,
};
use crate::ports::{ModelRequest, ModelResponse};
use crate::types::{
    AgentError, ContentPart, JsonObject, ModelOutput, ModelUsage, ToolCall, TranscriptItem,
};

const RAW_MESSAGE_KEY: &str = "openai.chat.message";
const RESPONSE_ID_KEY: &str = "openai.chat.response_id";

/// Chat Completions 请求的协议级可选字段。
///
/// `additional_fields` 可承载兼容端点扩展，但不得覆盖 `model`、`messages`、`tools`、
/// `tool_choice` 或 `max_tokens`。本适配器不支持 SSE，`stream: true` 会被拒绝。
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct RequestOptions {
    pub max_tokens: Option<u64>,
    pub tool_choice: Option<Value>,
    pub additional_fields: JsonObject,
}

impl RequestOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_tokens(mut self, max_tokens: u64) -> Self {
        self.max_tokens = Some(max_tokens);
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

pub(crate) struct OpenAiChatCodec {
    options: RequestOptions,
}

impl OpenAiChatCodec {
    pub(crate) fn new(options: RequestOptions) -> Self {
        Self { options }
    }
}

impl WireCodec for OpenAiChatCodec {
    fn encode_request(&self, request: &ModelRequest, model: &str) -> Result<Value, AgentError> {
        if self.options.max_tokens == Some(0) {
            return Err(model_error("openai_chat_invalid_max_tokens"));
        }
        if request.function_tools.is_empty() && self.options.tool_choice.is_some() {
            return Err(model_error("openai_chat_tool_choice_without_tools"));
        }

        let mut body = Map::new();
        body.insert("model".to_owned(), Value::String(model.to_owned()));
        body.insert(
            "messages".to_owned(),
            Value::Array(encode_messages(&request.transcript)?),
        );

        if !request.function_tools.is_empty() {
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
                .collect();
            body.insert("tools".to_owned(), Value::Array(tools));
            if let Some(tool_choice) = &self.options.tool_choice {
                body.insert("tool_choice".to_owned(), tool_choice.clone());
            }
        }
        if let Some(max_tokens) = self.options.max_tokens {
            body.insert("max_tokens".to_owned(), Value::from(max_tokens));
        }
        merge_request_fields(
            &mut body,
            &self.options.additional_fields,
            &["model", "messages", "tools", "tool_choice", "max_tokens"],
            "openai_chat",
        )?;
        Ok(Value::Object(body))
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
        // 原始消息保留兼容端点添加的未知字段，并在续轮时完整回放。
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
