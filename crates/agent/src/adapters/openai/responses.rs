//! OpenAI Responses wire 协议。

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::adapters::http::{
    content_as_text, merge_request_fields, model_error, parse_arguments, SseEvent, StreamDecode,
    WireCodec, WireStreamDecoder,
};
use crate::ports::{ModelRequest, ModelResponse, ModelStreamEvent};
use crate::types::{
    AgentError, ContentPart, JsonObject, ModelOutput, ModelUsage, ToolCall, ToolCallSlot,
    TranscriptItem,
};

const RAW_OUTPUT_KEY: &str = "openai.responses.output";
const ITEM_ID_KEY: &str = "openai.responses.item_id";
const RESPONSE_ID_KEY: &str = "openai.responses.response_id";

/// Responses 请求的协议级可选字段。
///
/// `additional_fields` 不得覆盖 `model`、`input`、`tools`、`tool_choice` 或
/// `max_output_tokens`，也不得设置由模型方法管理的 `stream`。`role_mappings` 可将框架中的自定义 canonical role 映射到
/// 本协议接受的 wire role。
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct RequestOptions {
    pub max_output_tokens: Option<u64>,
    pub tool_choice: Option<Value>,
    pub additional_fields: JsonObject,
    pub role_mappings: BTreeMap<String, String>,
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

    /// 将一个 canonical role 映射成 Responses API 接受的 wire role。
    pub fn with_role_mapping(
        mut self,
        canonical_role: impl Into<String>,
        wire_role: impl Into<String>,
    ) -> Self {
        self.role_mappings
            .insert(canonical_role.into(), wire_role.into());
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
            Value::Array(encode_input(
                &request.transcript,
                &self.options.role_mappings,
            )?),
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
        decode_responses_response(value)
    }

    fn stream_decoder(&self) -> Box<dyn WireStreamDecoder> {
        Box::new(ResponsesStreamDecoder::default())
    }
}

fn decode_responses_response(value: Value) -> Result<ModelResponse, AgentError> {
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

#[derive(Default)]
struct ResponsesStreamDecoder {
    function_slots: BTreeMap<u64, ToolCallSlot>,
    ready_calls: BTreeMap<ToolCallSlot, ToolCall>,
    text_parts: BTreeMap<(u64, u64), u32>,
    next_tool_slot: u32,
    next_text_part: u32,
    terminal_seen: bool,
}

impl WireStreamDecoder for ResponsesStreamDecoder {
    fn push(&mut self, event: SseEvent) -> Result<StreamDecode, AgentError> {
        if self.terminal_seen {
            return Err(model_error("openai_responses_event_after_terminal"));
        }
        let value: Value = serde_json::from_str(&event.data)
            .map_err(|error| model_error(format!("openai_responses_stream_json_failed:{error}")))?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| model_error("openai_responses_stream_event_missing_type"))?;

        match kind {
            "response.output_item.added" => {
                if value.pointer("/item/type").and_then(Value::as_str) == Some("function_call") {
                    let output_index = required_u64(
                        &value,
                        "output_index",
                        "openai_responses_stream_item_missing_output_index",
                    )?;
                    self.ensure_function_slot(output_index)?;
                }
                Ok(StreamDecode::events(Vec::new()))
            }
            "response.output_text.delta" => {
                let delta = required_string(
                    &value,
                    "delta",
                    "openai_responses_stream_text_delta_missing_value",
                )?;
                let output_index = required_u64(
                    &value,
                    "output_index",
                    "openai_responses_stream_text_delta_missing_output_index",
                )?;
                let content_index = value
                    .get("content_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let key = (output_index, content_index);
                let part_index = match self.text_parts.get(&key).copied() {
                    Some(index) => index,
                    None => {
                        let index = self.next_text_part;
                        self.next_text_part =
                            self.next_text_part.checked_add(1).ok_or_else(|| {
                                model_error("openai_responses_stream_text_part_overflow")
                            })?;
                        self.text_parts.insert(key, index);
                        index
                    }
                };
                Ok(StreamDecode::events(vec![ModelStreamEvent::TextDelta {
                    part_index,
                    delta: delta.to_owned(),
                }]))
            }
            "response.output_item.done" => self.output_item_done(&value),
            "response.completed" => self.complete(value),
            "response.failed" | "response.incomplete" | "error" => {
                self.terminal_seen = true;
                Err(model_error(format!(
                    "openai_responses_stream_unsuccessful_terminal:{kind}"
                )))
            }
            // 生命周期、参数 delta、文本 done 以及未来扩展事件不改变本层的提交边界。
            _ => Ok(StreamDecode::events(Vec::new())),
        }
    }

    fn finish_eof(&mut self) -> Result<(), AgentError> {
        Err(model_error("openai_responses_stream_missing_terminal"))
    }
}

impl ResponsesStreamDecoder {
    fn ensure_function_slot(&mut self, output_index: u64) -> Result<ToolCallSlot, AgentError> {
        if let Some(slot) = self.function_slots.get(&output_index).copied() {
            return Ok(slot);
        }
        let slot = ToolCallSlot::new(self.next_tool_slot);
        self.next_tool_slot = self
            .next_tool_slot
            .checked_add(1)
            .ok_or_else(|| model_error("openai_responses_stream_tool_count_overflow"))?;
        self.function_slots.insert(output_index, slot);
        Ok(slot)
    }

    fn output_item_done(&mut self, value: &Value) -> Result<StreamDecode, AgentError> {
        let item = value
            .get("item")
            .ok_or_else(|| model_error("openai_responses_stream_done_missing_item"))?;
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            return Ok(StreamDecode::events(Vec::new()));
        }
        if item
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| status != "completed")
        {
            return Err(model_error(
                "openai_responses_stream_function_item_not_completed",
            ));
        }
        let output_index = required_u64(
            value,
            "output_index",
            "openai_responses_stream_done_missing_output_index",
        )?;
        let slot = self.ensure_function_slot(output_index)?;
        let call = decode_function_call(item)?;
        if self.ready_calls.insert(slot, call.clone()).is_some() {
            return Err(model_error(
                "openai_responses_stream_duplicate_function_item_done",
            ));
        }
        Ok(StreamDecode::events(vec![
            ModelStreamEvent::ToolCallReady { slot, call },
        ]))
    }

    fn complete(&mut self, value: Value) -> Result<StreamDecode, AgentError> {
        self.terminal_seen = true;
        let response_value = value
            .get("response")
            .cloned()
            .ok_or_else(|| model_error("openai_responses_stream_completed_missing_response"))?;
        let response = decode_responses_response(response_value)?;
        let call_count = u32::try_from(response.output.tool_calls.len())
            .map_err(|_| model_error("openai_responses_stream_tool_count_overflow"))?;

        let mut events = Vec::new();
        for (index, call) in response.output.tool_calls.iter().cloned().enumerate() {
            let slot = ToolCallSlot::new(
                u32::try_from(index)
                    .map_err(|_| model_error("openai_responses_stream_tool_count_overflow"))?,
            );
            match self.ready_calls.get(&slot) {
                Some(ready) if ready == &call => {}
                Some(_) => {
                    return Err(model_error("openai_responses_stream_ready_call_mismatch"));
                }
                None => {
                    // 兼容端点若省略 item.done，仍只能在完整 response 终态补发。
                    self.ready_calls.insert(slot, call.clone());
                    events.push(ModelStreamEvent::ToolCallReady { slot, call });
                }
            }
        }
        if self.ready_calls.len() != response.output.tool_calls.len() {
            return Err(model_error(
                "openai_responses_stream_ready_call_count_mismatch",
            ));
        }
        events.push(ModelStreamEvent::ToolCallsSealed { call_count });
        Ok(StreamDecode::completed(events, response))
    }
}

fn required_u64(value: &Value, field: &str, error: &'static str) -> Result<u64, AgentError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| model_error(error))
}

fn encode_input(
    transcript: &[TranscriptItem],
    role_mappings: &BTreeMap<String, String>,
) -> Result<Vec<Value>, AgentError> {
    let mut input = Vec::new();
    for item in transcript {
        match item {
            TranscriptItem::Input(message) => {
                let role = wire_role(&message.role, role_mappings)?;
                input.push(json!({
                    "role": role,
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
    // status 缺失时兼容只实现基础 wire 的端点；一旦显式提供，就必须确认单项已完成。
    if item
        .get("status")
        .is_some_and(|status| status.as_str() != Some("completed"))
    {
        return Err(model_error("openai_responses_function_call_not_completed"));
    }
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

fn wire_role<'a>(
    role: &'a str,
    role_mappings: &'a BTreeMap<String, String>,
) -> Result<&'a str, AgentError> {
    let lowered = role_mappings.get(role).map(String::as_str).unwrap_or(role);
    match lowered {
        "system" | "developer" | "user" | "assistant" => Ok(lowered),
        _ => Err(model_error(format!(
            "openai_responses_unsupported_input_role:{lowered}"
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
