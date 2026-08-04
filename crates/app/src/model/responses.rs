//! Responses API provider（OpenAI 系 `/responses` 形状）。
//!
//! 按协议命名而不是按供应商：DeepSeek 只是当前用这个形状的一家，
//! 端点、模型名与 key 都是配置。chat/completions 形状已随
//! `deepseek-chat` 退役一并移除，不再保留双路径。
//!
//! 适配边界：Responses 的请求是 `instructions + input[]`、响应是 `output[]`
//! 的 item 列表，而 Agent 状态机吃的是 OpenAI 形状的
//! `message { content, tool_calls }`。两者的互转全部收敛在这个文件里——
//! 换供应商就是再写一个这样的 provider，`AgentRun` 一行都不用动。

use std::time::{Duration, Instant};

use mineintent_contracts::agent::{
    AgentError, AgentErrorCode, ContractFuture, ExecutionControl, ModelProvider, ModelUsage,
};
use mineintent_middle::agent::{AgentModelRequest, ModelCompletion};
use serde_json::{json, Map, Value};

use crate::devlog;

/// 单次模型调用的墙钟上限。Responses 的思考轮可能较长，取值偏宽；
/// run 级的真正约束是 Participant 的 run deadline。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(110);

#[derive(Clone, Debug)]
pub struct ResponsesConfig {
    pub endpoint: String,
    pub model: String,
    /// 思考强度：`none` / `low` / `medium` / `high`（Responses 的 `reasoning.effort`）。
    pub reasoning_effort: String,
}

pub struct ResponsesModelProvider {
    config: ResponsesConfig,
    api_key: String,
    http: reqwest::Client,
}

fn provider_error(message: impl Into<String>) -> AgentError {
    let message = message.into();
    devlog::log("model", format!("provider 失败：{message}"));
    AgentError::new(AgentErrorCode::ProviderFailed, message)
}

impl ResponsesModelProvider {
    pub fn new(config: ResponsesConfig, api_key: String) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| format!("HTTP 客户端构建失败：{error}"))?;
        Ok(Self {
            config,
            api_key,
            http,
        })
    }

    fn build_payload(&self, request: &AgentModelRequest) -> Result<Value, AgentError> {
        let (instructions, input) = messages_to_responses_input(&request.messages)?;
        let tools = request
            .tools
            .iter()
            .map(tool_definition_to_responses)
            .collect::<Result<Vec<_>, _>>()?;

        let mut payload = json!({
            "model": self.config.model,
            "instructions": instructions,
            "input": input,
            "reasoning": { "effort": self.config.reasoning_effort },
            "stream": false,
        });
        if !tools.is_empty() {
            let map = payload.as_object_mut().expect("payload 是对象字面量");
            map.insert("tools".to_owned(), Value::Array(tools));
            map.insert("tool_choice".to_owned(), Value::String("auto".to_owned()));
        }
        Ok(payload)
    }
}

/// OpenAI 形状的消息序列 → Responses 的 `(instructions, input[])`。
///
/// 系统消息成为 `instructions`；其余按原顺序展开成 item：助手的每个
/// tool_call 各成一个 `function_call`，工具结果成 `function_call_output`。
fn messages_to_responses_input(
    messages: &[Map<String, Value>],
) -> Result<(String, Vec<Value>), AgentError> {
    let mut instructions = String::new();
    let mut input = Vec::new();

    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| provider_error("模型消息缺少 role"))?;
        match role {
            "system" => {
                let text = content_to_text(message.get("content"));
                if !instructions.is_empty() {
                    instructions.push_str("\n\n");
                }
                instructions.push_str(&text);
            }
            "tool" => {
                // 工具结果必须带回它应答的 call_id，否则 Responses 无法配对。
                let call_id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| provider_error("工具结果消息缺少 tool_call_id"))?;
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": content_to_text(message.get("content")),
                }));
            }
            "assistant" => {
                let text = content_to_text(message.get("content"));
                if !text.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": text,
                    }));
                }
                if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
                    for call in tool_calls {
                        input.push(json!({
                            "type": "function_call",
                            "call_id": call.get("id").and_then(Value::as_str).unwrap_or_default(),
                            "name": call
                                .pointer("/function/name")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                            "arguments": call
                                .pointer("/function/arguments")
                                .and_then(Value::as_str)
                                .unwrap_or("{}"),
                        }));
                    }
                }
            }
            other => input.push(json!({
                "type": "message",
                "role": other,
                "content": content_to_text(message.get("content")),
            })),
        }
    }

    Ok((instructions, input))
}

/// Responses 的工具是扁平的 `{type, name, description, parameters}`，
/// 没有 chat/completions 的嵌套 `function` 对象。
fn tool_definition_to_responses(
    definition: &mineintent_contracts::agent::WireToolDefinition,
) -> Result<Value, AgentError> {
    let value = serde_json::to_value(definition)
        .map_err(|error| provider_error(format!("工具定义序列化失败：{error}")))?;
    let function = value
        .get("function")
        .ok_or_else(|| provider_error("工具定义缺少 function"))?;
    Ok(json!({
        "type": "function",
        "name": function.get("name").cloned().unwrap_or(Value::Null),
        "description": function.get("description").cloned().unwrap_or(Value::Null),
        "parameters": function.get("parameters").cloned().unwrap_or(json!({})),
    }))
}

/// `output[]` → OpenAI 形状的 assistant 消息。
///
/// 纯工具调用轮也**始终带 content 键**（空串）：上层回放会原样带回下一轮，
/// 缺这个键的助手轮会被部分供应商拒绝。
fn responses_output_to_message(output: &[Value]) -> Result<Map<String, Value>, AgentError> {
    let mut text = String::new();
    let mut tool_calls = Vec::new();

    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                let piece = output_message_text(item);
                if !piece.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&piece);
                }
            }
            Some("function_call") => {
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| provider_error("function_call 缺少 call_id"))?;
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| provider_error("function_call 缺少 name"))?;
                tool_calls.push(json!({
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("{}"),
                    },
                }));
            }
            // reasoning 等 item 不进模型上下文回放：它们是本轮的过程，
            // 不是下一轮需要重放的事实。
            _ => {}
        }
    }

    let mut message = Map::new();
    message.insert("role".to_owned(), Value::String("assistant".to_owned()));
    message.insert("content".to_owned(), Value::String(text));
    if !tool_calls.is_empty() {
        message.insert("tool_calls".to_owned(), Value::Array(tool_calls));
    }
    Ok(message)
}

fn output_message_text(item: &Value) -> String {
    match item.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn content_to_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| match part {
                Value::String(text) => Some(text.clone()),
                other => other.get("text").and_then(Value::as_str).map(str::to_owned),
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Responses 的 `status`（completed/incomplete/…）与 chat/completions 的
/// finish_reason 是两套词汇；状态机只认后者，因此在 provider 内归一化，
/// 不把供应商词汇泄漏进上层。非正常终态如实上抛，由状态机按拒绝处理。
fn normalize_finish_reason(status: Option<&str>, message: &Map<String, Value>) -> String {
    match status {
        Some("completed") | None => {
            if message.contains_key("tool_calls") {
                "tool_calls".to_owned()
            } else {
                "stop".to_owned()
            }
        }
        Some(other) => other.to_owned(),
    }
}

fn map_usage(usage: &Value) -> ModelUsage {
    let read = |key: &str| usage.get(key).and_then(Value::as_u64);
    ModelUsage {
        input_tokens: read("input_tokens"),
        output_tokens: read("output_tokens"),
        cache_read_tokens: usage
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64),
        cache_write_tokens: None,
    }
}

impl ModelProvider for ResponsesModelProvider {
    type Request = AgentModelRequest;
    type Response = ModelCompletion;

    fn complete<'a>(
        &'a self,
        request: Self::Request,
        _control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<Self::Response, AgentError>> {
        Box::pin(async move {
            let payload = self.build_payload(&request)?;
            let stamp = format!(
                "{}-{}",
                request.run_id.as_str().replace(['/', '\\'], "_"),
                uuid::Uuid::new_v4().simple()
            );
            devlog::log(
                "model",
                format!(
                    "请求 run={} 输入项={} 工具数={}",
                    request.run_id.as_str(),
                    payload
                        .get("input")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len),
                    request.tools.len()
                ),
            );
            devlog::dump_model_io(&stamp, "req", &payload.to_string());

            let started = Instant::now();
            let response = self
                .http
                .post(&self.config.endpoint)
                .bearer_auth(&self.api_key)
                .json(&payload)
                .send()
                .await
                .map_err(|error| provider_error(format!("请求模型失败：{error}")))?;

            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(|error| provider_error(format!("读取响应失败：{error}")))?;
            devlog::dump_model_io(&stamp, "res", &body);

            if !status.is_success() {
                return Err(provider_error(format!(
                    "模型返回 HTTP {}：{}",
                    status.as_u16(),
                    body.chars().take(300).collect::<String>()
                )));
            }

            let parsed: Value = serde_json::from_str(&body).map_err(|error| {
                provider_error(format!(
                    "响应非 JSON：{error}；前 200 字节：{}",
                    body.chars().take(200).collect::<String>()
                ))
            })?;
            if let Some(error) = parsed.get("error").filter(|error| !error.is_null()) {
                return Err(provider_error(format!("模型报错：{error}")));
            }
            let output = parsed
                .get("output")
                .and_then(Value::as_array)
                .ok_or_else(|| provider_error("响应缺少 output 数组"))?;
            let message = responses_output_to_message(output)?;

            // Responses 的 `status`（completed/incomplete/…）与 chat/completions 的
            // finish_reason 是两套词汇；状态机只认后者，所以在这里归一化，
            // 不把供应商词汇泄漏进上层。有工具调用即 tool_calls，否则 stop。
            let finish_reason = Some(Value::String(normalize_finish_reason(
                parsed.get("status").and_then(Value::as_str),
                &message,
            )));
            let usage = parsed.get("usage").map(map_usage);
            let tool_names: Vec<String> = message
                .get("tool_calls")
                .and_then(Value::as_array)
                .map(|calls| {
                    calls
                        .iter()
                        .filter_map(|call| {
                            call.pointer("/function/name")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .collect()
                })
                .unwrap_or_default();
            devlog::log(
                "model",
                format!(
                    "响应 run={} status={} 用时={}ms 工具={:?} content={}",
                    request.run_id.as_str(),
                    finish_reason
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "?".to_owned()),
                    started.elapsed().as_millis(),
                    tool_names,
                    message
                        .get("content")
                        .and_then(Value::as_str)
                        .map(|text| text.chars().take(120).collect::<String>())
                        .unwrap_or_default()
                ),
            );

            Ok(ModelCompletion {
                message: Some(message),
                finish_reason,
                usage,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => unreachable!("测试字面量是对象"),
        }
    }

    #[test]
    fn system_message_becomes_instructions_and_rest_become_input_items() {
        let messages = vec![
            object(json!({"role": "system", "content": "你是同伴"})),
            object(json!({"role": "user", "content": "看看四周"})),
            object(json!({
                "role": "assistant",
                "content": "好",
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {"name": "view", "arguments": "{\"mode\":\"full\"}"}
                }]
            })),
            object(json!({"role": "tool", "tool_call_id": "call-1", "content": "{\"ok\":true}"})),
        ];
        let (instructions, input) = messages_to_responses_input(&messages).unwrap();
        assert_eq!(instructions, "你是同伴");
        let kinds: Vec<&str> = input
            .iter()
            .map(|item| item["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            vec![
                "message",
                "message",
                "function_call",
                "function_call_output"
            ]
        );
        assert_eq!(input[2]["call_id"], "call-1");
        assert_eq!(input[2]["name"], "view");
        assert_eq!(input[3]["call_id"], "call-1");
    }

    #[test]
    fn function_call_outputs_become_openai_shaped_tool_calls() {
        let output = vec![
            json!({"type": "reasoning", "summary": []}),
            json!({
                "type": "message",
                "content": [{"type": "output_text", "text": "我看看"}]
            }),
            json!({
                "type": "function_call",
                "call_id": "call-9",
                "name": "look_relative",
                "arguments": "{\"yaw_degrees\":30.0,\"pitch_degrees\":0.0}"
            }),
        ];
        let message = responses_output_to_message(&output).unwrap();
        assert_eq!(message["role"], "assistant");
        assert_eq!(message["content"], "我看看");
        let calls = message["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "call-9");
        assert_eq!(calls[0]["function"]["name"], "look_relative");
        assert!(!message.contains_key("reasoning"), "reasoning 不进回放");
    }

    #[test]
    fn tool_call_only_turn_still_carries_a_content_key() {
        let output = vec![json!({
            "type": "function_call",
            "call_id": "call-1",
            "name": "respawn",
            "arguments": "{}"
        })];
        let message = responses_output_to_message(&output).unwrap();
        assert_eq!(
            message.get("content"),
            Some(&Value::String(String::new())),
            "缺 content 键的助手轮会被部分供应商拒绝"
        );
    }

    #[test]
    fn responses_status_is_normalized_to_the_chat_finish_reason_vocabulary() {
        // Responses 的 status=completed 不是 finish_reason 的合法值；
        // 实盘曾因此每轮都被状态机拒绝（model_finish_reason_rejected:completed）。
        let with_tools = vec![json!({
            "type": "function_call", "call_id": "c", "name": "view", "arguments": "{}"
        })];
        let message = responses_output_to_message(&with_tools).unwrap();
        assert_eq!(
            normalize_finish_reason(Some("completed"), &message),
            "tool_calls"
        );

        let text_only = vec![json!({"type": "message", "content": "好的"})];
        let message = responses_output_to_message(&text_only).unwrap();
        assert_eq!(normalize_finish_reason(Some("completed"), &message), "stop");
        assert_eq!(normalize_finish_reason(None, &message), "stop");
        assert_eq!(
            normalize_finish_reason(Some("incomplete"), &message),
            "incomplete",
            "非正常终态如实上抛"
        );
    }

    #[test]
    fn tools_are_flattened_without_the_nested_function_object() {
        let definition: mineintent_contracts::agent::WireToolDefinition =
            serde_json::from_value(json!({
                "type": "function",
                "function": {
                    "name": "say",
                    "description": "说一句话",
                    "parameters": {"type": "object", "properties": {}}
                }
            }))
            .unwrap();
        let tool = tool_definition_to_responses(&definition).unwrap();
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["name"], "say");
        assert!(tool.get("function").is_none(), "Responses 的工具是扁平的");
    }
}
