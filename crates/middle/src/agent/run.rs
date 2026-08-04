use std::collections::HashSet;

use mineintent_contracts::agent::{
    AgentError, AgentErrorCode, JsonObject, ModelUsage, RunId, ToolCallId, ToolDefinitionName,
    ToolExecution, ToolInvocation, ToolName,
};
use serde::Serialize;
use serde_json::{json, Value};

pub const MAX_MODEL_REQUESTS_PER_RUN: usize = 16;
pub const MAX_TOOL_CALLS_PER_RESPONSE: usize = 8;
pub const MAX_TOOL_CALLS_PER_RUN: usize = 32;

const MAX_SAFE_JSON_INTEGER: u64 = 9_007_199_254_740_991;
const INVALID_TOOL_CALL_SUMMARY: &str = "invalid tool call";

/// Provider 已归一化的一次 assistant completion。
///
/// `message` 保持 JSON object，是为了无损回放 provider 的 `reasoning_content` 与
/// `tool_calls`；本状态机不会把某一家 provider 的私有 wire 提升为公共 contract。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelCompletion {
    pub message: Option<JsonObject>,
    /// 缺字段用 `None`，显式 null 用 `Some(Value::Null)`；两者都表示 provider 未报告。
    pub finish_reason: Option<Value>,
    pub usage: Option<ModelUsage>,
}

/// 一次已经与 tool-call ID 配对、可加入下一轮模型上下文的结果。
#[derive(Clone, Debug, PartialEq)]
pub struct AgentToolResult {
    tool_call_id: ToolCallId,
    output: JsonObject,
}

impl AgentToolResult {
    /// 把严格的进程内 tool-response 转成模型可见的结果。
    ///
    /// `protocol` 已由 Rust 类型保证为 v2；模型只看 `result` 与
    /// `observationAfter`，与被移除的 Python 边界一致。
    pub fn from_execution<Observation>(
        tool_call_id: ToolCallId,
        execution: ToolExecution<Observation>,
    ) -> Result<Self, AgentError>
    where
        Observation: Serialize,
    {
        let observation_after = execution
            .observation_after
            .into_inner()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| {
                AgentError::new(
                    AgentErrorCode::ToolFailed,
                    "tool_result_serialization_failed",
                )
            })?
            .unwrap_or(Value::Null);
        let mut output = JsonObject::new();
        output.insert("result".to_owned(), execution.result);
        output.insert("observationAfter".to_owned(), observation_after);
        Ok(Self {
            tool_call_id,
            output,
        })
    }

    /// 为无效模型调用或具体工具失败创建仍然可回放的配对结果。
    pub fn failed(tool_call_id: ToolCallId, summary: impl AsRef<str>) -> Self {
        let summary = truncate_summary(summary.as_ref());
        let output = json!({
            "result": {
                "status": "failed",
                "summary": summary,
            },
            "observationAfter": null,
        })
        .as_object()
        .cloned()
        .unwrap_or_default();
        Self {
            tool_call_id,
            output,
        }
    }

    pub fn tool_call_id(&self) -> &ToolCallId {
        &self.tool_call_id
    }

    pub fn output(&self) -> &JsonObject {
        &self.output
    }
}

/// 驱动器按数组顺序处理的单个 tool-call 计划。
#[derive(Clone, Debug, PartialEq)]
pub enum PlannedToolCall {
    /// 参数结构有效，交给进程内 [`ToolDispatcher`](mineintent_contracts::capability::ToolDispatcher)。
    Dispatch(ToolInvocation),
    /// 模型给出的 name/arguments 无效；不产生世界副作用，但必须回放失败结果。
    LocalResult(AgentToolResult),
}

impl PlannedToolCall {
    pub fn tool_call_id(&self) -> &ToolCallId {
        match self {
            Self::Dispatch(invocation) => &invocation.tool_call_id,
            Self::LocalResult(result) => result.tool_call_id(),
        }
    }
}

/// `AgentRun` 下一步要求驱动器完成的动作。
#[derive(Clone, Debug, PartialEq)]
pub enum AgentRunStep {
    CallModel {
        messages: Vec<JsonObject>,
    },
    CallTools {
        calls: Vec<PlannedToolCall>,
    },
    Done {
        /// Speech 已经通过工具发生；closing 仅供 transcript 使用。
        closing: String,
        usage: Option<ModelUsage>,
    },
}

enum RunState {
    NeedModel,
    WaitingModel,
    NeedTools(Vec<PlannedToolCall>),
    WaitingTools(Vec<ToolCallId>),
    Done(String),
    Failed,
}

/// 一次 provider 无关、无 I/O 的可步进 Agent 运行。
pub struct AgentRun {
    run_id: RunId,
    messages: Vec<JsonObject>,
    model_requests: usize,
    tool_calls: usize,
    seen_tool_call_ids: HashSet<ToolCallId>,
    usage: Option<ModelUsage>,
    state: RunState,
}

impl AgentRun {
    /// 以已经排好稳定前缀与 opening frame 的消息开始一次运行。
    ///
    /// context v4 的具体渲染留给 composer；循环只追加 assistant replay 与 tool results。
    pub fn new(run_id: RunId, messages: Vec<JsonObject>) -> Self {
        Self {
            run_id,
            messages,
            model_requests: 0,
            tool_calls: 0,
            seen_tool_call_ids: HashSet::new(),
            usage: None,
            state: RunState::NeedModel,
        }
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    /// The complete model-visible replay accumulated so far.
    pub fn messages(&self) -> &[JsonObject] {
        &self.messages
    }

    /// Usage accumulated from provider responses so far, including a
    /// partially completed run.
    pub fn usage(&self) -> Option<&ModelUsage> {
        self.usage.as_ref()
    }

    /// A closing is available only after an assistant final message has been
    /// accepted. It remains readable if a later control check fails before
    /// the driver emits `Done`.
    pub fn closing(&self) -> Option<&str> {
        match &self.state {
            RunState::Done(closing) => Some(closing),
            _ => None,
        }
    }

    pub fn next_step(&mut self) -> Result<AgentRunStep, AgentError> {
        let state = std::mem::replace(&mut self.state, RunState::Failed);
        match state {
            RunState::NeedModel => {
                if self.model_requests >= MAX_MODEL_REQUESTS_PER_RUN {
                    return Err(AgentError::new(
                        AgentErrorCode::LimitExceeded,
                        "model_request_limit_exceeded",
                    ));
                }
                self.model_requests += 1;
                self.state = RunState::WaitingModel;
                Ok(AgentRunStep::CallModel {
                    messages: self.messages.clone(),
                })
            }
            RunState::NeedTools(calls) => {
                let pending = calls
                    .iter()
                    .map(|call| call.tool_call_id().clone())
                    .collect();
                self.state = RunState::WaitingTools(pending);
                Ok(AgentRunStep::CallTools { calls })
            }
            RunState::Done(closing) => {
                self.state = RunState::Done(closing.clone());
                Ok(AgentRunStep::Done {
                    closing,
                    usage: self.usage.clone(),
                })
            }
            RunState::WaitingModel => {
                self.state = RunState::WaitingModel;
                Err(AgentError::new(
                    AgentErrorCode::InvalidRequest,
                    "agent_run_waiting_for_model",
                ))
            }
            RunState::WaitingTools(pending) => {
                self.state = RunState::WaitingTools(pending);
                Err(AgentError::new(
                    AgentErrorCode::InvalidRequest,
                    "agent_run_waiting_for_tools",
                ))
            }
            RunState::Failed => Err(AgentError::new(
                AgentErrorCode::InvalidRequest,
                "agent_run_failed",
            )),
        }
    }

    /// 交回一轮 provider 响应，并在任何世界副作用前预检和 claim 整批 tool-call ID。
    pub fn model_response(&mut self, completion: ModelCompletion) -> Result<(), AgentError> {
        if !matches!(self.state, RunState::WaitingModel) {
            return Err(AgentError::new(
                AgentErrorCode::InvalidRequest,
                "agent_run_not_waiting_for_model",
            ));
        }

        let Some(message) = completion.message else {
            return self.fail(AgentError::new(
                AgentErrorCode::ProviderFailed,
                "model_response_missing_assistant_message",
            ));
        };
        if let Err(error) = self.add_usage(completion.usage) {
            return self.fail(error);
        }
        if let Some(error) = finish_reason_error(completion.finish_reason.as_ref()) {
            return self.fail(error);
        }

        let tool_calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .filter(|calls| !calls.is_empty());
        if let Some(tool_calls) = tool_calls {
            if tool_calls.len() > MAX_TOOL_CALLS_PER_RESPONSE {
                return self.fail(AgentError::new(
                    AgentErrorCode::LimitExceeded,
                    "tool_calls_per_response_limit_exceeded",
                ));
            }
            if self.tool_calls + tool_calls.len() > MAX_TOOL_CALLS_PER_RUN {
                return self.fail(AgentError::new(
                    AgentErrorCode::LimitExceeded,
                    "tool_calls_per_run_limit_exceeded",
                ));
            }

            let claimed = match claim_tool_call_batch(tool_calls, &self.seen_tool_call_ids) {
                Ok(claimed) => claimed,
                Err(error) => return self.fail(error),
            };
            let replay = assistant_replay(&message);
            let plans = claimed
                .iter()
                .map(|(tool_call_id, function)| {
                    prepare_tool_call(&self.run_id, tool_call_id.clone(), function)
                })
                .collect();

            self.seen_tool_call_ids
                .extend(claimed.iter().map(|(tool_call_id, _)| tool_call_id.clone()));
            self.tool_calls += claimed.len();
            self.messages.push(replay);
            self.state = RunState::NeedTools(plans);
            return Ok(());
        }

        let Some(content) = message.get("content").and_then(Value::as_str) else {
            return self.fail(AgentError::new(
                AgentErrorCode::ProviderFailed,
                "model_final_content_missing",
            ));
        };
        self.state = RunState::Done(content.trim().to_owned());
        Ok(())
    }

    /// 交回与上一批调用一一对应的结果；数量、ID 与数组顺序必须完全一致。
    pub fn tool_results(&mut self, results: Vec<AgentToolResult>) -> Result<(), AgentError> {
        let RunState::WaitingTools(pending) = &self.state else {
            return Err(AgentError::new(
                AgentErrorCode::InvalidRequest,
                "agent_run_not_waiting_for_tools",
            ));
        };
        if results.len() != pending.len()
            || results
                .iter()
                .zip(pending)
                .any(|(result, expected)| result.tool_call_id() != expected)
        {
            return self.fail(AgentError::new(
                AgentErrorCode::InvalidToolInvocation,
                "tool_result_batch_mismatch",
            ));
        }

        let mut messages = Vec::with_capacity(results.len());
        for result in results {
            let content = serde_json::to_string(&Value::Object(result.output)).map_err(|_| {
                AgentError::new(
                    AgentErrorCode::ToolFailed,
                    "tool_result_serialization_failed",
                )
            })?;
            let mut message = JsonObject::new();
            message.insert("role".to_owned(), Value::String("tool".to_owned()));
            message.insert(
                "tool_call_id".to_owned(),
                Value::String(result.tool_call_id.into_inner()),
            );
            message.insert("content".to_owned(), Value::String(content));
            messages.push(message);
        }
        self.messages.extend(messages);
        self.state = RunState::NeedModel;
        Ok(())
    }

    /// Appends a model-visible message that was already constructed by the
    /// driver/assembly layer.  Sampling and serialization stay outside this
    /// provider-independent state machine; the state guard keeps the message
    /// after the complete tool batch and before the next model request.
    pub fn append_user_message(&mut self, message: JsonObject) -> Result<(), AgentError> {
        if !matches!(self.state, RunState::NeedModel) {
            return Err(AgentError::new(
                AgentErrorCode::InvalidRequest,
                "agent_run_not_ready_for_user_message",
            ));
        }
        if message.get("role").and_then(Value::as_str) != Some("user") {
            return Err(AgentError::new(
                AgentErrorCode::InvalidRequest,
                "agent_user_message_requires_user_role",
            ));
        }
        self.messages.push(message);
        Ok(())
    }

    pub fn model_request_count(&self) -> usize {
        self.model_requests
    }

    pub fn tool_call_count(&self) -> usize {
        self.tool_calls
    }

    fn add_usage(&mut self, usage: Option<ModelUsage>) -> Result<(), AgentError> {
        let Some(usage) = usage else {
            return Ok(());
        };
        let total = self.usage.get_or_insert_with(ModelUsage::default);
        add_counter(&mut total.input_tokens, usage.input_tokens)?;
        add_counter(&mut total.output_tokens, usage.output_tokens)?;
        add_counter(&mut total.cache_read_tokens, usage.cache_read_tokens)?;
        add_counter(&mut total.cache_write_tokens, usage.cache_write_tokens)?;
        Ok(())
    }

    fn fail(&mut self, error: AgentError) -> Result<(), AgentError> {
        self.state = RunState::Failed;
        Err(error)
    }
}

fn add_counter(total: &mut Option<u64>, value: Option<u64>) -> Result<(), AgentError> {
    let Some(value) = value else {
        return Ok(());
    };
    *total = Some(
        total
            .unwrap_or_default()
            .checked_add(value)
            .ok_or_else(|| {
                AgentError::new(
                    AgentErrorCode::LimitExceeded,
                    "model_usage_counter_overflow",
                )
            })?,
    );
    Ok(())
}

fn finish_reason_error(reason: Option<&Value>) -> Option<AgentError> {
    match reason {
        None | Some(Value::Null) => None,
        Some(Value::String(reason))
            if matches!(reason.as_str(), "stop" | "tool_calls" | "function_call") =>
        {
            None
        }
        Some(Value::String(reason)) if !reason.is_empty() => Some(AgentError::new(
            AgentErrorCode::ProviderFailed,
            format!(
                "model_finish_reason_rejected:{}",
                stable_fragment(reason, 64)
            ),
        )),
        Some(_) => Some(AgentError::new(
            AgentErrorCode::ProviderFailed,
            "model_finish_reason_invalid",
        )),
    }
}

fn claim_tool_call_batch(
    tool_calls: &[Value],
    seen: &HashSet<ToolCallId>,
) -> Result<Vec<(ToolCallId, JsonObject)>, AgentError> {
    let mut claimed = Vec::with_capacity(tool_calls.len());
    let mut batch_ids = HashSet::with_capacity(tool_calls.len());
    for call in tool_calls {
        let Some(call) = call.as_object() else {
            return Err(invalid_model_tool_call());
        };
        let Some(tool_call_id) = call
            .get("id")
            .and_then(Value::as_str)
            .and_then(|value| ToolCallId::new(value).ok())
        else {
            return Err(invalid_model_tool_call());
        };
        let Some(function) = call.get("function").and_then(Value::as_object) else {
            return Err(invalid_model_tool_call());
        };
        if seen.contains(&tool_call_id) || !batch_ids.insert(tool_call_id.clone()) {
            return Err(AgentError::new(
                AgentErrorCode::ToolCallAlreadyHandled,
                "tool_call_id_reused",
            ));
        }
        claimed.push((tool_call_id, function.clone()));
    }
    Ok(claimed)
}

fn prepare_tool_call(
    run_id: &RunId,
    tool_call_id: ToolCallId,
    function: &JsonObject,
) -> PlannedToolCall {
    let invocation = function
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| ToolDefinitionName::new(*name).is_ok())
        .zip(function.get("arguments").and_then(Value::as_str))
        .and_then(|(name, arguments)| parse_arguments(arguments).map(|arguments| (name, arguments)))
        .and_then(|(name, arguments)| {
            ToolName::new(name).ok().map(|name| ToolInvocation {
                run_id: run_id.clone(),
                tool_call_id: tool_call_id.clone(),
                name,
                arguments,
            })
        });

    invocation.map_or_else(
        || {
            PlannedToolCall::LocalResult(AgentToolResult::failed(
                tool_call_id,
                INVALID_TOOL_CALL_SUMMARY,
            ))
        },
        PlannedToolCall::Dispatch,
    )
}

fn parse_arguments(raw: &str) -> Option<JsonObject> {
    let value: Value = serde_json::from_str(raw).ok()?;
    if !json_numbers_are_safe(&value) {
        return None;
    }
    value.as_object().cloned()
}

fn json_numbers_are_safe(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().all(json_numbers_are_safe),
        Value::Object(values) => values.values().all(json_numbers_are_safe),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                value.unsigned_abs() <= MAX_SAFE_JSON_INTEGER
            } else if let Some(value) = number.as_u64() {
                value <= MAX_SAFE_JSON_INTEGER
            } else {
                true
            }
        }
        _ => true,
    }
}

fn assistant_replay(message: &JsonObject) -> JsonObject {
    let mut replay = JsonObject::new();
    for key in ["role", "content", "reasoning_content", "tool_calls"] {
        if let Some(value) = message.get(key) {
            replay.insert(key.to_owned(), value.clone());
        }
    }
    replay
        .entry("role".to_owned())
        .or_insert_with(|| Value::String("assistant".to_owned()));
    replay
}

fn invalid_model_tool_call() -> AgentError {
    AgentError::new(
        AgentErrorCode::InvalidToolInvocation,
        "model_tool_call_invalid",
    )
}

fn truncate_summary(summary: &str) -> String {
    summary
        .chars()
        .filter(|character| !character.is_control())
        .take(300)
        .collect()
}

fn stable_fragment(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
        .collect()
}
