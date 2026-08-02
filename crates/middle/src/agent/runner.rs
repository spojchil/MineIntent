use std::time::Instant;

use mineintent_contracts::{
    agent::{
        AgentError, AgentErrorCode, AgentRunProtocol, AgentRunRequest,
        AgentRunner as ContractRunner, ContractFuture, ExecutionControl,
        JsonAgentDecisionContextV4, ModelName, ModelProvider, ModelRunResult,
    },
    capability::ToolDispatcher,
};
use serde::Serialize;

use super::{initial_messages, AgentLoopDriver, AgentModelRequest, AgentRun, ModelCompletion};

/// The concrete in-process runner assembled from one model provider and one
/// ordered tool dispatcher.
///
/// The model name is explicit assembly data.  It is not inferred from a
/// template, context, or an implicit default, and is copied verbatim to the
/// contract result after the loop completes.
pub struct ConcreteAgentRunner<Model, Tools> {
    driver: AgentLoopDriver<Model, Tools>,
    model_name: ModelName,
}

impl<Model, Tools> ConcreteAgentRunner<Model, Tools> {
    pub fn new(model: Model, tools: Tools, model_name: ModelName) -> Self {
        Self {
            driver: AgentLoopDriver::new(model, tools),
            model_name,
        }
    }

    pub fn driver(&self) -> &AgentLoopDriver<Model, Tools> {
        &self.driver
    }

    pub fn model_name(&self) -> &ModelName {
        &self.model_name
    }
}

/// Name retained as an implementation-oriented alias for assembly code that
/// calls the component an implementation rather than a concrete runner.
pub type AgentRunnerImpl<Model, Tools> = ConcreteAgentRunner<Model, Tools>;

impl<Model, Tools> ContractRunner for ConcreteAgentRunner<Model, Tools>
where
    Model: ModelProvider<Request = AgentModelRequest, Response = ModelCompletion>,
    Tools: ToolDispatcher,
    Tools::Observation: Serialize,
{
    type Context = JsonAgentDecisionContextV4;

    fn run<'a>(
        &'a self,
        request: AgentRunRequest<Self::Context>,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<ModelRunResult, AgentError>> {
        Box::pin(async move {
            request.validate()?;
            control.check_at(Instant::now())?;

            // Prompt composition is intentionally outside the loop.  AgentRun
            // owns the resulting prefix and only appends assistant/tool turns.
            let messages = initial_messages(&request.context, &request.prompt_template)
                .map_err(prompt_error)?;
            let mut run = AgentRun::new(request.run_id, messages);
            let outcome = self.driver.drive(&mut run, &request.tools, control).await?;

            // `closing` is deliberately not copied: it is transcript-only and
            // never becomes speech, a tool result, or ModelRunResult content.
            let _transcript_candidate_closing = outcome.closing;
            Ok(ModelRunResult {
                protocol: AgentRunProtocol::V1,
                model: self.model_name.clone(),
                usage: outcome.usage,
            })
        })
    }
}

fn prompt_error(error: super::PromptError) -> AgentError {
    AgentError::new(AgentErrorCode::InvalidRequest, error.to_string())
}
