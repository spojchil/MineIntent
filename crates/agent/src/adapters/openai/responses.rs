//! OpenAI Responses wire 协议。

use serde_json::{json, Map, Value};

use crate::adapters::http::{
    content_as_text, merge_request_fields, model_error, parse_arguments, WireCodec,
};
use crate::ports::{ModelRequest, ModelResponse};
use crate::types::{
    AgentError, ContentPart, JsonObject, ModelOutput, ModelUsage, ToolCall, TranscriptItem,
};

const RAW_OUTPUT_KEY: &str = "openai.responses.output";
const ITEM_ID_KEY: &str = "openai.responses.item_id";
const RESPONSE_ID_KEY: &str = "openai.responses.response_id";

/// Responses 请求的协议级可选字段。
///
/// `additional_fields` 不得覆盖 `model`、`input`、`tools`、`tool_choice` 或
/// `max_output_tokens`。本适配器不支持 SSE，`stream: true` 会被拒绝。
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct RequestOptions {
    pub max_output_tokens: Option<u64>,
    pub tool_choice: Option<Value>,
    pub additional_fields: JsonObject,
}

impl RequestOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_output_tokens(mut self, max_output_tokens: u64) -> Self {
        self.max_output_tokens = Some(max_output_tokens);
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

pub(crate) struct OpenAiResponsesCodec {
    options: RequestOptions,
}

impl OpenAiResponsesCodec {
    pub(crate) fn new(options: RequestOptions) -> Self {
        Self { options }
    }
}

impl WireCodec for OpenAiResponsesCodec {
    fn encode_request(&self, request: &ModelRequest, model: &str) -> Result<Value, AgentError> {
        if self.options.max_output_tokens == Some(0) {
            return Err(model_error("openai_responses_invalid_max_output_tokens"));
        }
        if request.function_tools.is_empty() && self.options.tool_choice.is_some() {
            return Err(model_error("openai_responses_tool_choice_without_tools"));
        }

        let mut body = Map::new();
        body.insert("model".to_owned(), Value::String(model.to_owned()));
        body.insert(
            "input".to_owned(),
            Value::Array(encode_input(&request.transcript)?),
        );
        if !request.function_tools.is_empty() {
            let tools = request
                .function_tools
                .iter()
                .map(|tool| {
                    let mut definition = json!({
                        "type": "function",
                        "name": tool.name.as_str(),
                        "parameters": tool.input_schema,
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
        if let Some(max_output_tokens) = self.options.max_output_tokens {
            body.insert(
                "max_output_tokens".to_owned(),
                Value::from(max_output_tokens),
            );
        }
        merge_request_fields(
            &mut body,
            &self.options.additional_fields,
            &[
                "model",
                "input",
                "tools",
                "tool_choice",
                "max_output_tokens",
            ],
            "openai_responses",
        )?;
        Ok(Value::Object(body))
    }

    fn decode_response(&self, value: Value) -> Result<ModelResponse, AgentError> {
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| model_error("openai_responses_missing_status"))?;
        if status != "completed" {
            return Err(model_error(format!(
                "openai_responses_not_completed:{status}"
            )));
        }
        let output = value
            .get("output")
            .and_then(Value::as_array)
            .ok_or_else(|| model_error("openai_responses_missing_output"))?;
        let mut content = Vec::new();
        let mut tool_calls = Vec::new();

        for item in output {
            match item.get("type").and_then(Value::as_str) {
                Some("message") => decode_message_content(item, &mut content)?,
                Some("function_call") => tool_calls.push(decode_function_call(item)?),
                Some(kind) => content.push(ContentPart::Opaque {
                    kind: format!("openai.responses.{kind}"),
                    data: item.clone(),
                }),
                None => return Err(model_error("openai_responses_output_item_missing_type")),
            }
        }

        let mut provider_data = JsonObject::new();
        // 原始输出数组保留 item 顺序、服务端 ID 以及兼容端点的未知扩展字段。
        provider_data.insert(RAW_OUTPUT_KEY.to_owned(), Value::Array(output.clone()));
        if let Some(response_id) = value.get("id") {
            provider_data.insert(RESPONSE_ID_KEY.to_owned(), response_id.clone());
        }
        let usage = value.get("usage").map(|usage| ModelUsage {
            input_tokens: usage.get("input_tokens").and_then(Value::as_u64),
            output_tokens: usage.get("output_tokens").and_then(Value::as_u64),
            total_tokens: usage.get("total_tokens").and_then(Value::as_u64),
            cached_input_tokens: usage
                .pointer("/input_tokens_details/cached_tokens")
                .and_then(Value::as_u64),
            cache_write_input_tokens: None,
            reasoning_output_tokens: usage
                .pointer("/output_tokens_details/reasoning_tokens")
                .and_then(Value::as_u64),
        });

        Ok(ModelResponse {
            output: ModelOutput {
                content,
                tool_calls,
                provider_data,
            },
            finish_reason: value.get("status").cloned(),
            usage,
        })
    }
}

fn encode_input(transcript: &[TranscriptItem]) -> Result<Vec<Value>, AgentError> {
    let mut input = Vec::new();
    for item in transcript {
        match item {
            TranscriptItem::Input(message) => {
                ensure_message_role(&message.role)?;
                input.push(json!({
                    "role": message.role,
                    "content": content_as_text(&message.content),
                }));
            }
            TranscriptItem::ModelOutput(output) => {
                if let Some(raw) = output
                    .provider_data
                    .get(RAW_OUTPUT_KEY)
                    .and_then(Value::as_array)
                {
                    // reasoning、item ID、未知扩展字段与输出顺序需要精确回放。
                    input.extend(raw.iter().cloned());
                } else {
                    encode_canonical_output(output, &mut input);
                }
            }
            TranscriptItem::ToolResults(batch) => {
                input.extend(batch.results.iter().map(|result| {
                    json!({
                        "type": "function_call_output",
                        "call_id": result.call_id.as_str(),
                        "output": content_as_text(&result.content),
                    })
                }));
            }
        }
    }
    Ok(input)
}

fn encode_canonical_output(output: &ModelOutput, input: &mut Vec<Value>) {
    if !output.content.is_empty() {
        input.push(json!({
            "role": "assistant",
            "content": content_as_text(&output.content),
        }));
    }
    input.extend(output.tool_calls.iter().map(|call| {
        let mut item = json!({
            "type": "function_call",
            "call_id": call.id.as_str(),
            "name": call.name.as_str(),
            "arguments": match &call.arguments {
                Value::String(raw) => raw.clone(),
                value => value.to_string(),
            },
        });
        if let Some(item_id) = call.provider_data.get(ITEM_ID_KEY) {
            item["id"] = item_id.clone();
        }
        item
    }));
}

fn decode_message_content(item: &Value, content: &mut Vec<ContentPart>) -> Result<(), AgentError> {
    let blocks = item
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| model_error("openai_responses_message_missing_content"))?;
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("output_text") | Some("text") => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| model_error("openai_responses_text_missing_value"))?;
                content.push(ContentPart::text(text));
            }
            Some(kind) => content.push(ContentPart::Opaque {
                kind: format!("openai.responses.content.{kind}"),
                data: block.clone(),
            }),
            None => return Err(model_error("openai_responses_content_missing_type")),
        }
    }
    Ok(())
}

fn decode_function_call(item: &Value) -> Result<ToolCall, AgentError> {
    // `call_id` 才是结果回传关联键；`id` 只是 Responses output item ID。
    let call_id = required_string(
        item,
        "call_id",
        "openai_responses_function_call_missing_call_id",
    )?;
    let name = required_string(item, "name", "openai_responses_function_call_missing_name")?;
    let arguments = required_string(
        item,
        "arguments",
        "openai_responses_function_call_missing_arguments",
    )?;
    let mut call = ToolCall::new(call_id, name, parse_arguments(arguments));
    if let Some(item_id) = item.get("id") {
        call.provider_data
            .insert(ITEM_ID_KEY.to_owned(), item_id.clone());
    }
    Ok(call)
}

fn ensure_message_role(role: &str) -> Result<(), AgentError> {
    match role {
        "system" | "developer" | "user" | "assistant" => Ok(()),
        _ => Err(model_error(format!(
            "openai_responses_unsupported_input_role:{role}"
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
