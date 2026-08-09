//! 轮状态机：一次触发 → 若干次（模型请求 → 工具批）→ 终文。
//!
//! 从旧 `toolloop::AgentRun` 重写吸收。保留的是正确性纪律：
//! tool-call ID 一次性 claim、结果批与调用批严格配对、
//! 消息追加只在请求边界（`NeedModel`）合法。
//! 不保留的是预算脚手架（每轮 16 次请求那类）——
//! 真实约束是上下文窗，由会话层的压缩触发持有。

use std::collections::HashSet;

use mineintent_contracts::agent::{
    AgentError, AgentErrorCode, JsonObject, ModelUsage, RunId, ToolCallId, ToolInvocation,
    ToolName,
};
use serde_json::Value;

use crate::ports::ModelCompletion;

/// 一次已与 tool-call ID 配对、可回放进上下文的工具结果。
#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    tool_call_id: ToolCallId,
    output: JsonObject,
}

impl ToolResult {
    pub fn new(tool_call_id: ToolCallId, output: JsonObject) -> Self {
        Self {
            tool_call_id,
            output,
        }
    }

    /// 参数无效一类的失败：不产生世界副作用，但必须回放。
    /// 失败文案是给模型看的一句话，不是系统错误（opencode 的教训）。
    pub fn failed(tool_call_id: ToolCallId, summary: impl AsRef<str>) -> Self {
        let mut output = JsonObject::new();
        output.insert("status".to_owned(), Value::String("failed".to_owned()));
        output.insert(
            "summary".to_owned(),
            Value::String(summary.as_ref().to_owned()),
        );
        Self {
            tool_call_id,
            output,
        }
    }

    pub fn tool_call_id(&self) -> &ToolCallId {
        &self.tool_call_id
    }
}

/// 单个 tool-call 的执行计划。
#[derive(Clone, Debug, PartialEq)]
pub enum PlannedToolCall {
    /// 参数结构有效，交给②号端口执行。
    Dispatch(ToolInvocation),
    /// 模型给的 name/arguments 无效；不执行，但失败结果必须回放。
    LocalResult(ToolResult),
}

impl PlannedToolCall {
    pub fn tool_call_id(&self) -> &ToolCallId {
        match self {
            Self::Dispatch(invocation) => &invocation.tool_call_id,
            Self::LocalResult(result) => result.tool_call_id(),
        }
    }
}

/// 状态机要求驱动方完成的下一个动作。
#[derive(Clone, Debug, PartialEq)]
pub enum TurnStep {
    CallModel { messages: Vec<JsonObject> },
    CallTools { calls: Vec<PlannedToolCall> },
    Done { closing: String, usage: Option<ModelUsage> },
}

enum TurnState {
    NeedModel,
    WaitingModel,
    NeedTools(Vec<PlannedToolCall>),
    WaitingTools(Vec<ToolCallId>),
    Done(String),
    Failed,
}

/// 一轮：provider 无关、无 I/O、可步进。
pub struct Turn {
    run_id: RunId,
    messages: Vec<JsonObject>,
    seen_tool_call_ids: HashSet<ToolCallId>,
    usage: Option<ModelUsage>,
    state: TurnState,
}

impl Turn {
    /// 以已排好的前缀（persona + situation + 既有对话）开始一轮。
    pub fn new(run_id: RunId, messages: Vec<JsonObject>) -> Self {
        Self {
            run_id,
            messages,
            seen_tool_call_ids: HashSet::new(),
            usage: None,
            state: TurnState::NeedModel,
        }
    }

    pub fn messages(&self) -> &[JsonObject] {
        &self.messages
    }

    /// 请求边界：信箱在此排空才合法。
    pub fn at_request_boundary(&self) -> bool {
        matches!(self.state, TurnState::NeedModel)
    }

    /// 追加一条模型可见的 user 消息。只在请求边界合法——
    /// 整批工具结果之后、下一次模型请求之前。
    pub fn append_user_message(&mut self, message: JsonObject) -> Result<(), AgentError> {
        if !matches!(self.state, TurnState::NeedModel) {
            return Err(AgentError::new(
                AgentErrorCode::InvalidRequest,
                "turn_not_at_request_boundary",
            ));
        }
        if message.get("role").and_then(Value::as_str) != Some("user") {
            return Err(AgentError::new(
                AgentErrorCode::InvalidRequest,
                "injected_message_requires_user_role",
            ));
        }
        self.messages.push(message);
        Ok(())
    }

    pub fn next_step(&mut self) -> Result<TurnStep, AgentError> {
        let state = std::mem::replace(&mut self.state, TurnState::Failed);
        match state {
            TurnState::NeedModel => {
                self.state = TurnState::WaitingModel;
                Ok(TurnStep::CallModel {
                    messages: self.messages.clone(),
                })
            }
            TurnState::NeedTools(calls) => {
                let pending = calls
                    .iter()
                    .map(|call| call.tool_call_id().clone())
                    .collect();
                self.state = TurnState::WaitingTools(pending);
                Ok(TurnStep::CallTools { calls })
            }
            TurnState::Done(closing) => {
                self.state = TurnState::Done(closing.clone());
                Ok(TurnStep::Done {
                    closing,
                    usage: self.usage.clone(),
                })
            }
            TurnState::WaitingModel => {
                self.state = TurnState::WaitingModel;
                Err(AgentError::new(
                    AgentErrorCode::InvalidRequest,
                    "turn_waiting_for_model",
                ))
            }
            TurnState::WaitingTools(pending) => {
                self.state = TurnState::WaitingTools(pending);
                Err(AgentError::new(
                    AgentErrorCode::InvalidRequest,
                    "turn_waiting_for_tools",
                ))
            }
            TurnState::Failed => Err(AgentError::new(
                AgentErrorCode::InvalidRequest,
                "turn_failed",
            )),
        }
    }

    /// 交回一次 provider 响应；在任何世界副作用前 claim 整批 tool-call ID。
    pub fn model_response(&mut self, completion: ModelCompletion) -> Result<(), AgentError> {
        if !matches!(self.state, TurnState::WaitingModel) {
            return Err(AgentError::new(
                AgentErrorCode::InvalidRequest,
                "turn_not_waiting_for_model",
            ));
        }
        let Some(message) = completion.message else {
            return self.fail(AgentError::new(
                AgentErrorCode::ProviderFailed,
                "model_response_missing_assistant_message",
            ));
        };
        if completion.usage.is_some() {
            self.usage = completion.usage;
        }

        let tool_calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .filter(|calls| !calls.is_empty())
            .cloned();
        if let Some(tool_calls) = tool_calls {
            let plans = match self.claim_and_plan(&tool_calls) {
                Ok(plans) => plans,
                Err(error) => return self.fail(error),
            };
            // assistant 回放保持无损：整条消息原样入上下文。
            self.messages.push(message);
            self.state = TurnState::NeedTools(plans);
            return Ok(());
        }

        let Some(content) = message.get("content").and_then(Value::as_str) else {
            return self.fail(AgentError::new(
                AgentErrorCode::ProviderFailed,
                "model_final_content_missing",
            ));
        };
        let closing = content.trim().to_owned();
        self.messages.push(message);
        self.state = TurnState::Done(closing);
        Ok(())
    }

    /// 交回与上一批调用一一对应的结果；数量、ID、顺序必须完全一致。
    pub fn tool_results(&mut self, results: Vec<ToolResult>) -> Result<(), AgentError> {
        let TurnState::WaitingTools(pending) = &self.state else {
            return Err(AgentError::new(
                AgentErrorCode::InvalidRequest,
                "turn_not_waiting_for_tools",
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
            self.messages.push(message);
        }
        self.state = TurnState::NeedModel;
        Ok(())
    }

    /// claim 整批 ID（重复即整批失败），逐个把 name/arguments 定成执行计划；
    /// 单个调用的参数问题降级为 LocalResult（给模型的一句话），不失败整批。
    fn claim_and_plan(&mut self, tool_calls: &[Value]) -> Result<Vec<PlannedToolCall>, AgentError> {
        let mut claimed_in_batch = HashSet::new();
        let mut plans = Vec::with_capacity(tool_calls.len());
        for call in tool_calls {
            let id = call
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| {
                    AgentError::new(
                        AgentErrorCode::InvalidToolInvocation,
                        "tool_call_missing_id",
                    )
                })?;
            let Ok(tool_call_id) = ToolCallId::new(id.to_owned()) else {
                return Err(AgentError::new(
                    AgentErrorCode::InvalidToolInvocation,
                    "tool_call_id_invalid",
                ));
            };
            if self.seen_tool_call_ids.contains(&tool_call_id)
                || !claimed_in_batch.insert(tool_call_id.clone())
            {
                return Err(AgentError::new(
                    AgentErrorCode::InvalidToolInvocation,
                    "tool_call_id_reused",
                ));
            }

            let function = call.get("function");
            let name = function
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = function
                .and_then(|f| f.get("arguments"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            // name/arguments 的问题降级为给模型的一句话，不失败整批。
            let plan = match (ToolName::new(name.to_owned()), serde_json::from_str(arguments)) {
                (Ok(name), Ok(Value::Object(arguments))) => {
                    PlannedToolCall::Dispatch(ToolInvocation {
                        run_id: self.run_id.clone(),
                        tool_call_id: tool_call_id.clone(),
                        name,
                        arguments,
                    })
                }
                (Err(_), _) => PlannedToolCall::LocalResult(ToolResult::failed(
                    tool_call_id.clone(),
                    "tool call has a missing or invalid function name; rewrite the call",
                )),
                (Ok(_), _) => PlannedToolCall::LocalResult(ToolResult::failed(
                    tool_call_id.clone(),
                    "tool arguments must be a JSON object; rewrite the input",
                )),
            };
            plans.push(plan);
            self.seen_tool_call_ids.insert(tool_call_id);
        }
        Ok(plans)
    }

    fn fail(&mut self, error: AgentError) -> Result<(), AgentError> {
        self.state = TurnState::Failed;
        Err(error)
    }
}
