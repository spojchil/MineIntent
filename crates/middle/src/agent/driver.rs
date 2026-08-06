use std::time::Instant;

use mineintent_contracts::{
    agent::{
        AgentError, AgentErrorCode, ExecutionControl, JsonObject, ModelProvider, ModelUsage, RunId,
        ViewportFrameMessageV2, WireToolDefinition,
    },
    capability::{ExecutionResource, ToolDispatcher},
};
use serde::Serialize;
use serde_json::json;

use super::{
    AgentRun, AgentRunStep, AgentToolResult, ModelCompletion, NoRoundViewportSampler,
    PlannedToolCall, RoundViewportSampler,
};
use toolloop::{await_with_control, is_run_control_error, utc_timestamp_now};

/// 一轮 OpenAI-compatible completion 所需的 provider 无关输入。
#[derive(Clone, Debug, PartialEq)]
pub struct AgentModelRequest {
    pub run_id: RunId,
    pub messages: Vec<JsonObject>,
    pub tools: Vec<WireToolDefinition>,
}

/// 模型—工具循环结束后的 transcript-only closing 与累计 usage。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentLoopOutcome {
    pub closing: String,
    pub usage: Option<ModelUsage>,
}

/// 把纯 [`AgentRun`] 状态机接到进程内 model provider 与 tool dispatcher。
pub struct AgentLoopDriver<Model, Tools, Sampler = NoRoundViewportSampler> {
    model: Model,
    tools: Tools,
    viewport_sampler: Sampler,
}

impl<Model, Tools> AgentLoopDriver<Model, Tools, NoRoundViewportSampler> {
    pub fn new(model: Model, tools: Tools) -> Self {
        Self {
            model,
            tools,
            viewport_sampler: NoRoundViewportSampler,
        }
    }
}

impl<Model, Tools, Sampler> AgentLoopDriver<Model, Tools, Sampler> {
    pub fn new_with_viewport_sampler(
        model: Model,
        tools: Tools,
        viewport_sampler: Sampler,
    ) -> Self {
        Self {
            model,
            tools,
            viewport_sampler,
        }
    }

    pub fn model(&self) -> &Model {
        &self.model
    }

    pub fn tools(&self) -> &Tools {
        &self.tools
    }

    pub fn viewport_sampler(&self) -> &Sampler {
        &self.viewport_sampler
    }
}

impl<Model, Tools, Sampler> AgentLoopDriver<Model, Tools, Sampler>
where
    Model: ModelProvider<Request = AgentModelRequest, Response = ModelCompletion>,
    Tools: ToolDispatcher,
    Tools::Observation: Serialize,
    Sampler: RoundViewportSampler,
{
    /// 驱动已有的 run，所有 provider/dispatcher 调用共享调用者给定的 deadline。
    ///
    /// 调用者保留 `run` 的所有权，使后续 transcript 层能在成功和失败路径读取回放。
    pub async fn drive(
        &self,
        run: &mut AgentRun,
        definitions: &[WireToolDefinition],
        control: ExecutionControl<'_>,
    ) -> Result<AgentLoopOutcome, AgentError> {
        loop {
            control.check_at(Instant::now())?;
            match run.next_step()? {
                AgentRunStep::CallModel { messages } => {
                    let request = AgentModelRequest {
                        run_id: run.run_id().clone(),
                        messages,
                        tools: definitions.to_vec(),
                    };
                    let provider_future = self.model.complete(request, control);
                    let completion = await_with_control(provider_future, control).await?;
                    run.model_response(completion)?;
                }
                AgentRunStep::CallTools { calls } => {
                    let (results, body_dispatched) = self.dispatch_in_order(calls, control).await?;
                    control.check_at(Instant::now())?;
                    run.tool_results(results)?;
                    if body_dispatched {
                        let frame = self.sample_round_viewport(control).await?;
                        run.append_user_message(frame)?;
                    }
                }
                AgentRunStep::Done { closing, usage } => {
                    return Ok(AgentLoopOutcome { closing, usage });
                }
            }
        }
    }

    async fn dispatch_in_order(
        &self,
        calls: Vec<PlannedToolCall>,
        control: ExecutionControl<'_>,
    ) -> Result<(Vec<AgentToolResult>, bool), AgentError> {
        let mut results = Vec::with_capacity(calls.len());
        let mut body_dispatched = false;
        for call in calls {
            control.check_at(Instant::now())?;
            match call {
                PlannedToolCall::LocalResult(result) => results.push(result),
                PlannedToolCall::Dispatch(invocation) => {
                    let tool_call_id = invocation.tool_call_id.clone();
                    let resource = self.tools.resource(&invocation);
                    if resource == Some(ExecutionResource::Body) {
                        // Count before entering the dispatch future so ordinary
                        // failures and resource busy still trigger exactly one
                        // post-batch sample.
                        body_dispatched = true;
                    }
                    let dispatch_future = self.tools.dispatch(invocation, control);
                    let execution = await_with_control(dispatch_future, control).await;
                    match execution {
                        Ok(execution) => {
                            results.push(AgentToolResult::from_execution(tool_call_id, execution)?);
                        }
                        Err(error) if is_run_control_error(error.code) => return Err(error),
                        Err(error) => {
                            results.push(AgentToolResult::failed(tool_call_id, error.summary));
                        }
                    }
                }
            }
        }
        Ok((results, body_dispatched))
    }

    async fn sample_round_viewport(
        &self,
        control: ExecutionControl<'_>,
    ) -> Result<JsonObject, AgentError> {
        control.check_at(Instant::now())?;
        let at = self.viewport_sampler.timestamp();
        control.check_at(Instant::now())?;
        if ViewportFrameMessageV2::validate_at(&at).is_err() {
            return unavailable_frame_message(
                utc_timestamp_now(),
                "viewport_frame_timestamp_invalid",
            );
        }
        let sample_future = self.viewport_sampler.sample(control);
        let sampled = await_with_control(sample_future, control).await;

        match sampled {
            Ok(viewport) => {
                control.check_at(Instant::now())?;
                match serde_json::to_value(viewport) {
                    Ok(value) if !value.is_null() => {
                        let frame = match ViewportFrameMessageV2::success(at.clone(), value) {
                            Ok(frame) => frame,
                            Err(_) => {
                                return unavailable_frame_message(
                                    at,
                                    "viewport_frame_serialization_failed",
                                );
                            }
                        };
                        match encode_user_frame(frame) {
                            Ok(message) => Ok(message),
                            Err(_) => {
                                control.check_at(Instant::now())?;
                                unavailable_frame_message(at, "viewport_frame_serialization_failed")
                            }
                        }
                    }
                    Ok(_) => {
                        control.check_at(Instant::now())?;
                        unavailable_frame_message(at, "viewport_frame_null_payload")
                    }
                    Err(_) => {
                        control.check_at(Instant::now())?;
                        unavailable_frame_message(at, "viewport_frame_serialization_failed")
                    }
                }
            }
            Err(error) if is_run_control_error(error.code) => Err(error),
            Err(error) => {
                control.check_at(Instant::now())?;
                unavailable_frame_message(at, viewport_error_reason(&error))
            }
        }
    }
}

fn encode_user_frame(frame: ViewportFrameMessageV2) -> Result<JsonObject, AgentError> {
    let content = serde_json::to_string(&frame).map_err(|_| {
        AgentError::new(
            AgentErrorCode::ToolFailed,
            "viewport_frame_serialization_failed",
        )
    })?;
    Ok(json!({"role": "user", "content": content})
        .as_object()
        .cloned()
        .unwrap_or_default())
}

fn unavailable_frame_message(
    at: String,
    reason: impl Into<String>,
) -> Result<JsonObject, AgentError> {
    let frame = ViewportFrameMessageV2::unavailable(at, reason.into()).map_err(|_| {
        AgentError::new(
            AgentErrorCode::ToolFailed,
            "viewport_frame_serialization_failed",
        )
    })?;
    encode_user_frame(frame)
}

fn viewport_error_reason(error: &AgentError) -> String {
    let reason: String = error
        .summary
        .chars()
        .filter(|character| !character.is_control())
        .take(128)
        .collect();
    if reason.trim().is_empty() {
        error.code.to_string()
    } else {
        reason
    }
}
