use std::{
    future::Future,
    panic::{catch_unwind, AssertUnwindSafe},
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};

use mineintent_contracts::{
    agent::{
        AgentError, AgentErrorCode, ExecutionControl, JsonObject, ModelProvider, ModelUsage,
        ToolCallId, WireToolDefinition,
    },
    capability::ToolDispatcher,
};
use serde::Serialize;

use super::{AgentRun, AgentRunStep, AgentToolResult, ModelCompletion, PlannedToolCall};

/// 一轮 OpenAI-compatible completion 所需的 provider 无关输入。
#[derive(Clone, Debug, PartialEq)]
pub struct AgentModelRequest {
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
pub struct AgentLoopDriver<Model, Tools> {
    model: Model,
    tools: Tools,
}

impl<Model, Tools> AgentLoopDriver<Model, Tools> {
    pub fn new(model: Model, tools: Tools) -> Self {
        Self { model, tools }
    }

    pub fn model(&self) -> &Model {
        &self.model
    }

    pub fn tools(&self) -> &Tools {
        &self.tools
    }
}

impl<Model, Tools> AgentLoopDriver<Model, Tools>
where
    Model: ModelProvider<Request = AgentModelRequest, Response = ModelCompletion>,
    Tools: ToolDispatcher,
    Tools::Observation: Serialize,
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
                        messages,
                        tools: definitions.to_vec(),
                    };
                    let provider_future =
                        catch_unwind(AssertUnwindSafe(|| self.model.complete(request, control)))
                            .map_err(|_| {
                                AgentError::new(
                                    AgentErrorCode::ProviderFailed,
                                    "model_provider_panicked",
                                )
                            })?;
                    let completion = await_with_control(
                        catch_future_panic(
                            provider_future,
                            AgentErrorCode::ProviderFailed,
                            "model_provider_panicked",
                        ),
                        control,
                    )
                    .await?;
                    run.model_response(completion)?;
                }
                AgentRunStep::CallTools { calls } => {
                    let results = self.dispatch_in_order(calls, control).await?;
                    control.check_at(Instant::now())?;
                    run.tool_results(results)?;
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
    ) -> Result<Vec<AgentToolResult>, AgentError> {
        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            control.check_at(Instant::now())?;
            match call {
                PlannedToolCall::LocalResult(result) => results.push(result),
                PlannedToolCall::Dispatch(invocation) => {
                    let tool_call_id = invocation.tool_call_id.clone();
                    let dispatch_future = match catch_unwind(AssertUnwindSafe(|| {
                        self.tools.dispatch(invocation, control)
                    })) {
                        Ok(future) => future,
                        Err(_) => {
                            results.push(tool_panic_result(tool_call_id));
                            continue;
                        }
                    };
                    let execution = await_with_control(
                        catch_future_panic(
                            dispatch_future,
                            AgentErrorCode::ToolFailed,
                            "tool_dispatch_panicked",
                        ),
                        control,
                    )
                    .await;
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
        Ok(results)
    }
}

fn tool_panic_result(tool_call_id: ToolCallId) -> AgentToolResult {
    AgentToolResult::failed(tool_call_id, "tool_dispatch_panicked")
}

fn is_run_control_error(code: AgentErrorCode) -> bool {
    matches!(
        code,
        AgentErrorCode::RunCancelled | AgentErrorCode::DeadlineExceeded
    )
}

async fn await_with_control<F, Output>(
    future: F,
    control: ExecutionControl<'_>,
) -> Result<Output, AgentError>
where
    F: Future<Output = Result<Output, AgentError>> + Send,
{
    control.check_at(Instant::now())?;
    let cancellation = control.cancelled();
    let timer = tokio::time::sleep_until(tokio::time::Instant::from_std(
        control.deadline().expires_at(),
    ));
    tokio::pin!(future);
    tokio::pin!(cancellation);
    tokio::pin!(timer);

    tokio::select! {
        biased;
        cancellation_error = &mut cancellation => {
            match control.check_at(Instant::now()) {
                Err(error) => Err(error),
                Ok(()) => Err(cancellation_error),
            }
        }
        _ = &mut timer => {
            match control.check_at(Instant::now()) {
                Err(error) => Err(error),
                Ok(()) => Err(AgentError::deadline_exceeded()),
            }
        }
        result = &mut future => {
            control.check_at(Instant::now())?;
            result
        }
    }
}

async fn catch_future_panic<Output, F>(
    future: F,
    code: AgentErrorCode,
    summary: &'static str,
) -> Result<Output, AgentError>
where
    F: Future<Output = Result<Output, AgentError>> + Send,
{
    CatchUnwindFuture::new(future)
        .await
        .map_err(|()| AgentError::new(code, summary))?
}

/// `std` 没有 async `catch_unwind`；把每次 poll 单独围住即可隔离同步与异步 panic。
struct CatchUnwindFuture<F> {
    future: Pin<Box<F>>,
}

impl<F> CatchUnwindFuture<F> {
    fn new(future: F) -> Self {
        Self {
            future: Box::pin(future),
        }
    }
}

impl<F> Future for CatchUnwindFuture<F>
where
    F: Future,
{
    type Output = Result<F::Output, ()>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        catch_unwind(AssertUnwindSafe(|| this.future.as_mut().poll(context)))
            .map_or(Poll::Ready(Err(())), |poll| poll.map(Ok))
    }
}
