use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    sync::Arc,
    time::Instant,
};

use mineintent_contracts::{
    agent::{
        AgentError, AgentErrorCode, AgentRunProtocol, AgentRunRequest,
        AgentRunner as ContractRunner, ContractFuture, ExecutionControl,
        JsonAgentDecisionContextV4, ModelName, ModelProvider, ModelRunResult, RunId,
        WireToolDefinition,
    },
    capability::ToolDispatcher,
};
use serde::Serialize;

use super::{
    initial_messages, summarize_error, AgentLoopDriver, AgentModelRequest, AgentRun,
    AgentTranscriptRecord, FileTranscriptStore, ModelCompletion, TranscriptSink,
};

/// The concrete in-process runner assembled from one model provider and one
/// ordered tool dispatcher.
///
/// The model name is explicit assembly data.  It is not inferred from a
/// template, context, or an implicit default, and is copied verbatim to the
/// contract result after the loop completes.
pub struct ConcreteAgentRunner<Model, Tools> {
    driver: AgentLoopDriver<Model, Tools>,
    model_name: ModelName,
    transcript_sink: Option<Arc<dyn TranscriptSink>>,
}

impl<Model, Tools> ConcreteAgentRunner<Model, Tools> {
    pub fn new(model: Model, tools: Tools, model_name: ModelName) -> Self {
        Self {
            driver: AgentLoopDriver::new(model, tools),
            model_name,
            transcript_sink: None,
        }
    }

    /// Constructs a runner with an explicitly selected file store. The
    /// ordinary `new` constructor remains completely side-effect free.
    pub fn with_transcript_store(
        model: Model,
        tools: Tools,
        model_name: ModelName,
        store: FileTranscriptStore,
    ) -> Self {
        Self::with_shared_transcript_sink(model, tools, model_name, Arc::new(store))
    }

    /// Constructs a runner with an arbitrary diagnostic sink.
    pub fn with_transcript_sink<S>(
        model: Model,
        tools: Tools,
        model_name: ModelName,
        sink: S,
    ) -> Self
    where
        S: TranscriptSink + 'static,
    {
        Self::with_shared_transcript_sink(model, tools, model_name, Arc::new(sink))
    }

    pub fn with_shared_transcript_sink(
        model: Model,
        tools: Tools,
        model_name: ModelName,
        sink: Arc<dyn TranscriptSink>,
    ) -> Self {
        Self {
            driver: AgentLoopDriver::new(model, tools),
            model_name,
            transcript_sink: Some(sink),
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
            let run_id = request.run_id.clone();
            let mut run = AgentRun::new(request.run_id, messages);
            let drive_result = self.driver.drive(&mut run, &request.tools, control).await;
            self.append_transcript(&run, run_id, &request.tools, &drive_result);
            let outcome = drive_result?;

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

impl<Model, Tools> ConcreteAgentRunner<Model, Tools> {
    fn append_transcript(
        &self,
        run: &AgentRun,
        run_id: RunId,
        tools: &[WireToolDefinition],
        drive_result: &Result<super::AgentLoopOutcome, AgentError>,
    ) {
        let Some(sink) = self.transcript_sink.as_ref() else {
            return;
        };

        let (closing, error) = match drive_result {
            Ok(outcome) => (Some(outcome.closing.clone()), None),
            Err(error) => (
                run.closing().map(str::to_owned),
                Some(summarize_error(error)),
            ),
        };
        let record = AgentTranscriptRecord::new(
            run_id,
            self.model_name.clone(),
            tools,
            run.messages().to_vec(),
            closing,
            run.usage().cloned(),
            error,
        );

        // Transcript diagnostics are deliberately fail-open. This catches
        // both an ordinary sink error (ignored below) and a custom sink panic.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _ = sink.append_record(&record);
        }));
    }
}

fn prompt_error(error: super::PromptError) -> AgentError {
    AgentError::new(AgentErrorCode::InvalidRequest, error.to_string())
}
