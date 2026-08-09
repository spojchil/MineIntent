//! Anthropic Messages wire 协议。

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::adapters::http::{content_as_text, merge_request_fields, model_error, WireCodec};
use crate::adapters::http::{SseEvent, StreamDecode, WireStreamDecoder};
use crate::ports::{ModelRequest, ModelResponse, ModelStreamEvent};
use crate::types::{
    AgentError, ContentPart, JsonObject, ModelOutput, ModelUsage, ToolCall, ToolCallSlot,
    ToolResultStatus, TranscriptItem,
};

const RAW_CONTENT_KEY: &str = "anthropic.messages.content";
const MESSAGE_ID_KEY: &str = "anthropic.messages.message_id";

/// Anthropic Messages 请求的默认输出 token 上限。
pub const DEFAULT_MAX_TOKENS: u64 = 1024;

/// Messages 请求的协议级可选字段。
///
/// Anthropic 协议要求 `max_tokens`，其值必须大于零。`additional_fields` 不得覆盖
/// `model`、`system`、`messages`、`tools`、`tool_choice`、`max_tokens`，也不得设置由模型方法管理的 `stream`。
/// `role_mappings` 可将框架中的自定义 canonical role 映射到本协议接受的 wire role。
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct RequestOptions {
    pub max_tokens: u64,
    pub tool_choice: Option<Value>,
    pub additional_fields: JsonObject,
    pub role_mappings: BTreeMap<String, String>,
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            max_tokens: DEFAULT_MAX_TOKENS,
            tool_choice: None,
            additional_fields: JsonObject::new(),
            role_mappings: BTreeMap::new(),
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

    /// 将一个 canonical role 映射成 Messages API 接受的 wire role。
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

        let (system, messages) =
            encode_conversation(&request.transcript, &self.options.role_mappings)?;
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
        decode_messages_response(value)
    }

    fn stream_decoder(&self) -> Box<dyn WireStreamDecoder> {
        Box::new(MessagesStreamDecoder::default())
    }
}

fn decode_messages_response(value: Value) -> Result<ModelResponse, AgentError> {
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

    let stop_reason = value.get("stop_reason").cloned();
    let reason = stop_reason
        .as_ref()
        .and_then(Value::as_str)
        .ok_or_else(|| model_error("anthropic_messages_missing_stop_reason"))?;
    match reason {
        "end_turn" | "tool_use" | "stop_sequence" | "refusal" => {}
        "max_tokens" | "model_context_window_exceeded" => {
            return Err(model_error(format!(
                "anthropic_messages_unsuccessful_stop_reason:{reason}"
            )));
        }
        _ => {
            return Err(model_error("anthropic_messages_unsupported_stop_reason"));
        }
    }
    // 本地工具调用与终止原因必须双向一致，不能凭缺失或意外的 tool_use 推断完整工具批。
    match (tool_calls.is_empty(), reason) {
        (false, "tool_use") => {}
        (true, "end_turn" | "stop_sequence" | "refusal") => {}
        (false, _) => {
            return Err(model_error(
                "anthropic_messages_tool_calls_without_tool_use_stop",
            ));
        }
        (true, _) => {
            return Err(model_error(
                "anthropic_messages_tool_use_stop_without_tool_calls",
            ));
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
        finish_reason: stop_reason,
        usage,
    })
}

struct MessageBlockDraft {
    block: Value,
    input_json: String,
    tool_slot: Option<ToolCallSlot>,
    stopped: bool,
}

#[derive(Default)]
struct MessagesStreamDecoder {
    message: Option<Value>,
    blocks: BTreeMap<u32, MessageBlockDraft>,
    ready_calls: BTreeMap<ToolCallSlot, ToolCall>,
    next_tool_slot: u32,
    terminal_seen: bool,
}

impl WireStreamDecoder for MessagesStreamDecoder {
    fn push(&mut self, event: SseEvent) -> Result<StreamDecode, AgentError> {
        if self.terminal_seen {
            return Err(model_error("anthropic_messages_event_after_terminal"));
        }
        let value: Value = serde_json::from_str(&event.data).map_err(|error| {
            model_error(format!("anthropic_messages_stream_json_failed:{error}"))
        })?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| model_error("anthropic_messages_stream_event_missing_type"))?;

        match kind {
            "message_start" => self.message_start(&value),
            "content_block_start" => self.content_block_start(&value),
            "content_block_delta" => self.content_block_delta(&value),
            "content_block_stop" => self.content_block_stop(&value),
            "message_delta" => self.message_delta(&value),
            "message_stop" => self.message_stop(),
            "error" => {
                self.terminal_seen = true;
                Err(model_error("anthropic_messages_stream_error"))
            }
            // ping 和未来事件不会改变已声明的内容块边界。
            _ => Ok(StreamDecode::events(Vec::new())),
        }
    }

    fn finish_eof(&mut self) -> Result<(), AgentError> {
        Err(model_error(
            "anthropic_messages_stream_missing_message_stop",
        ))
    }
}

impl MessagesStreamDecoder {
    fn message_start(&mut self, value: &Value) -> Result<StreamDecode, AgentError> {
        if self.message.is_some() {
            return Err(model_error(
                "anthropic_messages_stream_duplicate_message_start",
            ));
        }
        let message = value
            .get("message")
            .cloned()
            .ok_or_else(|| model_error("anthropic_messages_stream_start_missing_message"))?;
        if !message.is_object() {
            return Err(model_error(
                "anthropic_messages_stream_start_message_not_object",
            ));
        }
        if message
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|content| !content.is_empty())
        {
            return Err(model_error(
                "anthropic_messages_stream_start_content_not_empty",
            ));
        }
        self.message = Some(message);
        Ok(StreamDecode::events(Vec::new()))
    }

    fn content_block_start(&mut self, value: &Value) -> Result<StreamDecode, AgentError> {
        self.require_message_start()?;
        let index = event_index(value, "anthropic_messages_stream_block_start_missing_index")?;
        if self.blocks.contains_key(&index) {
            return Err(model_error(
                "anthropic_messages_stream_duplicate_block_start",
            ));
        }
        let block = value
            .get("content_block")
            .cloned()
            .ok_or_else(|| model_error("anthropic_messages_stream_block_start_missing_block"))?;
        let tool_slot = if block.get("type").and_then(Value::as_str) == Some("tool_use") {
            let slot = ToolCallSlot::new(self.next_tool_slot);
            self.next_tool_slot = self
                .next_tool_slot
                .checked_add(1)
                .ok_or_else(|| model_error("anthropic_messages_stream_tool_count_overflow"))?;
            Some(slot)
        } else {
            None
        };
        self.blocks.insert(
            index,
            MessageBlockDraft {
                block,
                input_json: String::new(),
                tool_slot,
                stopped: false,
            },
        );
        Ok(StreamDecode::events(Vec::new()))
    }

    fn content_block_delta(&mut self, value: &Value) -> Result<StreamDecode, AgentError> {
        let index = event_index(value, "anthropic_messages_stream_block_delta_missing_index")?;
        let draft = self
            .blocks
            .get_mut(&index)
            .ok_or_else(|| model_error("anthropic_messages_stream_delta_before_block_start"))?;
        if draft.stopped {
            return Err(model_error(
                "anthropic_messages_stream_delta_after_block_stop",
            ));
        }
        let delta = value
            .get("delta")
            .ok_or_else(|| model_error("anthropic_messages_stream_delta_missing_value"))?;
        let kind = delta
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| model_error("anthropic_messages_stream_delta_missing_type"))?;
        match kind {
            "text_delta" => {
                let text = string_allow_empty(
                    delta,
                    "text",
                    "anthropic_messages_stream_text_delta_missing_text",
                )?;
                append_string_field(&mut draft.block, "text", text)?;
                let events = if text.is_empty() {
                    Vec::new()
                } else {
                    vec![ModelStreamEvent::TextDelta {
                        part_index: index,
                        delta: text.to_owned(),
                    }]
                };
                Ok(StreamDecode::events(events))
            }
            "input_json_delta" => {
                if draft.tool_slot.is_none() {
                    return Err(model_error(
                        "anthropic_messages_stream_input_delta_for_non_tool_block",
                    ));
                }
                let partial = string_allow_empty(
                    delta,
                    "partial_json",
                    "anthropic_messages_stream_input_delta_missing_json",
                )?;
                draft.input_json.push_str(partial);
                Ok(StreamDecode::events(Vec::new()))
            }
            "thinking_delta" => {
                let thinking = string_allow_empty(
                    delta,
                    "thinking",
                    "anthropic_messages_stream_thinking_delta_missing_value",
                )?;
                append_string_field(&mut draft.block, "thinking", thinking)?;
                Ok(StreamDecode::events(Vec::new()))
            }
            "signature_delta" => {
                let signature = string_allow_empty(
                    delta,
                    "signature",
                    "anthropic_messages_stream_signature_delta_missing_value",
                )?;
                append_string_field(&mut draft.block, "signature", signature)?;
                Ok(StreamDecode::events(Vec::new()))
            }
            _ => Ok(StreamDecode::events(Vec::new())),
        }
    }

    fn content_block_stop(&mut self, value: &Value) -> Result<StreamDecode, AgentError> {
        let index = event_index(value, "anthropic_messages_stream_block_stop_missing_index")?;
        let draft = self
            .blocks
            .get_mut(&index)
            .ok_or_else(|| model_error("anthropic_messages_stream_stop_before_block_start"))?;
        if draft.stopped {
            return Err(model_error(
                "anthropic_messages_stream_duplicate_block_stop",
            ));
        }
        draft.stopped = true;
        let Some(slot) = draft.tool_slot else {
            return Ok(StreamDecode::events(Vec::new()));
        };

        if !draft.input_json.is_empty() {
            let input = serde_json::from_str(&draft.input_json).map_err(|error| {
                model_error(format!(
                    "anthropic_messages_stream_tool_input_invalid:{error}"
                ))
            })?;
            draft.block["input"] = input;
        }
        let call = decode_tool_use(&draft.block)?;
        if self.ready_calls.insert(slot, call.clone()).is_some() {
            return Err(model_error(
                "anthropic_messages_stream_duplicate_ready_call",
            ));
        }
        Ok(StreamDecode::events(vec![
            ModelStreamEvent::ToolCallReady { slot, call },
        ]))
    }

    fn message_delta(&mut self, value: &Value) -> Result<StreamDecode, AgentError> {
        self.require_message_start()?;
        let message = self.message.as_mut().expect("message_start checked");
        if let Some(delta) = value.get("delta").and_then(Value::as_object) {
            for (key, value) in delta {
                message[key] = value.clone();
            }
        }
        if let Some(usage) = value.get("usage").and_then(Value::as_object) {
            let target = message
                .as_object_mut()
                .expect("message_start object checked")
                .entry("usage")
                .or_insert_with(|| Value::Object(Map::new()));
            let target = target.as_object_mut().ok_or_else(|| {
                model_error("anthropic_messages_stream_accumulated_usage_not_object")
            })?;
            for (key, value) in usage {
                target.insert(key.clone(), value.clone());
            }
        }
        Ok(StreamDecode::events(Vec::new()))
    }

    fn message_stop(&mut self) -> Result<StreamDecode, AgentError> {
        self.terminal_seen = true;
        self.require_message_start()?;
        if self.blocks.values().any(|block| !block.stopped) {
            return Err(model_error(
                "anthropic_messages_stream_unclosed_content_block",
            ));
        }
        let mut content = Vec::with_capacity(self.blocks.len());
        for expected in 0..self.blocks.len() {
            let index = u32::try_from(expected)
                .map_err(|_| model_error("anthropic_messages_stream_block_count_overflow"))?;
            let block = self
                .blocks
                .get(&index)
                .ok_or_else(|| model_error("anthropic_messages_stream_non_contiguous_blocks"))?;
            content.push(block.block.clone());
        }

        let mut message = self.message.clone().expect("message_start checked");
        message["content"] = Value::Array(content);
        let stop_reason = message
            .get("stop_reason")
            .and_then(Value::as_str)
            .ok_or_else(|| model_error("anthropic_messages_stream_missing_stop_reason"))?
            .to_owned();
        if matches!(
            stop_reason.as_str(),
            "max_tokens" | "model_context_window_exceeded"
        ) {
            return Err(model_error(format!(
                "anthropic_messages_stream_unsuccessful_stop_reason:{stop_reason}"
            )));
        }
        if !matches!(
            stop_reason.as_str(),
            "end_turn" | "tool_use" | "stop_sequence" | "refusal"
        ) {
            return Err(model_error(
                "anthropic_messages_stream_unsupported_stop_reason",
            ));
        }
        match (self.ready_calls.is_empty(), stop_reason.as_str()) {
            (false, "tool_use") => {}
            (true, "end_turn" | "stop_sequence" | "refusal") => {}
            (false, _) => {
                return Err(model_error(
                    "anthropic_messages_stream_tool_calls_without_tool_use_stop",
                ));
            }
            (true, _) => {
                return Err(model_error(
                    "anthropic_messages_stream_tool_use_stop_without_tool_calls",
                ));
            }
        }
        let response = decode_messages_response(message)?;
        let call_count = u32::try_from(response.output.tool_calls.len())
            .map_err(|_| model_error("anthropic_messages_stream_tool_count_overflow"))?;
        if self.ready_calls.len() != response.output.tool_calls.len()
            || response
                .output
                .tool_calls
                .iter()
                .enumerate()
                .any(|(index, call)| {
                    u32::try_from(index)
                        .ok()
                        .map(ToolCallSlot::new)
                        .and_then(|slot| self.ready_calls.get(&slot))
                        != Some(call)
                })
        {
            return Err(model_error("anthropic_messages_stream_ready_call_mismatch"));
        }
        Ok(StreamDecode::completed(
            vec![ModelStreamEvent::ToolCallsSealed { call_count }],
            response,
        ))
    }

    fn require_message_start(&self) -> Result<(), AgentError> {
        if self.message.is_some() {
            Ok(())
        } else {
            Err(model_error(
                "anthropic_messages_stream_event_before_message_start",
            ))
        }
    }
}

fn event_index(value: &Value, error: &'static str) -> Result<u32, AgentError> {
    let index = value
        .get("index")
        .and_then(Value::as_u64)
        .ok_or_else(|| model_error(error))?;
    u32::try_from(index).map_err(|_| model_error("anthropic_messages_stream_index_overflow"))
}

fn string_allow_empty<'a>(
    value: &'a Value,
    field: &str,
    error: &'static str,
) -> Result<&'a str, AgentError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| model_error(error))
}

fn append_string_field(value: &mut Value, field: &str, delta: &str) -> Result<(), AgentError> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| model_error("anthropic_messages_stream_block_not_object"))?;
    let target = object
        .entry(field)
        .or_insert_with(|| Value::String(String::new()));
    match target {
        Value::String(target) => {
            target.push_str(delta);
            Ok(())
        }
        _ => Err(model_error(format!(
            "anthropic_messages_stream_block_{field}_not_string"
        ))),
    }
}

fn encode_conversation(
    transcript: &[TranscriptItem],
    role_mappings: &BTreeMap<String, String>,
) -> Result<(Vec<Value>, Vec<Value>), AgentError> {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    for item in transcript {
        match item {
            TranscriptItem::Input(message) => match wire_role(&message.role, role_mappings)? {
                "system" | "developer" if messages.is_empty() => {
                    system.extend(content_blocks(&message.content));
                }
                "system" | "developer" => {
                    return Err(model_error(
                        "anthropic_messages_mid_conversation_system_message",
                    ));
                }
                role @ ("user" | "assistant") => {
                    push_message(&mut messages, role, content_blocks(&message.content))?;
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

fn wire_role<'a>(
    role: &'a str,
    role_mappings: &'a BTreeMap<String, String>,
) -> Result<&'a str, AgentError> {
    let lowered = role_mappings.get(role).map(String::as_str).unwrap_or(role);
    match lowered {
        "system" | "developer" | "user" | "assistant" => Ok(lowered),
        _ => Err(model_error(format!(
            "anthropic_messages_unsupported_input_role:{lowered}"
        ))),
    }
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
