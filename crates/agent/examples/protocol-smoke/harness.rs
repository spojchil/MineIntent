//! 三种协议共享的 agent 场景、工具运行时和观测输出。

use std::error::Error;
use std::io;
use std::sync::Arc;

use agent::{
    AgentError, AgentErrorKind, AgentEvent, AgentSession, Compaction, FilteredObserver,
    InputMessage, MailboxInput, Observer, PortFuture, PromptSource, SessionConfig, ToolCallBatch,
    ToolCallId, ToolDefinition, ToolResult, ToolResultBatch, ToolRuntime, TranscriptItem,
    TurnOutcome,
};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Semaphore};
use tokio::time::timeout;

use crate::adapter::{HttpModel, Protocol};
use crate::config::SmokeConfig;

type SmokeError = Box<dyn Error + Send + Sync>;

pub(crate) async fn run_smoke(protocol: Protocol, config: &SmokeConfig) -> Result<(), SmokeError> {
    println!("\n== {} ==", protocol.label());
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let gate = Arc::new(Semaphore::new(0));
    let observer = Arc::new(FilteredObserver::new(
        Arc::new(PrintingObserver { protocol }),
        config.level,
    ));
    let model = HttpModel::new(
        config.api_key.clone(),
        config.model.clone(),
        config.endpoint(protocol).to_owned(),
        protocol,
        config.timeout,
        config.wire_log,
    )?;
    let session = Arc::new(
        AgentSession::new(
            Arc::new(SmokePrompt),
            Arc::new(SmokeTools {
                started: started_tx,
                gate: gate.clone(),
            }),
            Arc::new(NoCompaction),
            Arc::new(model),
            SessionConfig::default(),
        )
        .with_observer(observer),
    );

    let running = session.clone();
    let mut task = tokio::spawn(async move {
        running
            .start_if_idle(vec![InputMessage::text(
                "user",
                "请在同一条回复中调用 add 两次，分别计算 2+3 与 5+7；现在不要自己计算。",
            )
            .into()])
            .await
    });

    let call_count = match timeout(config.timeout, started_rx.recv()).await {
        Ok(Some(count)) => count,
        Ok(None) => {
            task.abort();
            return Err(io::Error::other("工具 runtime 通知通道意外关闭").into());
        }
        Err(_) => {
            task.abort();
            return Err(io::Error::other(format!("等待 {} 工具调用超时", protocol.label())).into());
        }
    };
    println!("runtime 收到一个完整工具批，调用数：{call_count}");

    session
        .enqueue_if_running(MailboxInput::next_model_request(vec![InputMessage::text(
            "user",
            "这是工具执行期间插入的消息：结果齐全后只简短报告两个和。",
        )
        .into()]))
        .await?;
    gate.add_permits(1);

    let outcome = match timeout(config.timeout, &mut task).await {
        Ok(result) => result??,
        Err(_) => {
            task.abort();
            return Err(
                io::Error::other(format!("等待 {} agent 完成超时", protocol.label())).into(),
            );
        }
    };
    if call_count != 2 {
        return Err(io::Error::other(format!(
            "期望一个含两个调用的批次，实际调用数：{call_count}"
        ))
        .into());
    }

    match outcome {
        TurnOutcome::Completed { output, usage } => {
            println!("最终文本：{}", output.text_content());
            println!("usage：{usage:?}");
            Ok(())
        }
        other => Err(io::Error::other(format!("冒烟测试未正常完成：{other:?}")).into()),
    }
}

struct SmokePrompt;

impl PromptSource for SmokePrompt {
    fn base_context(&self) -> Vec<TranscriptItem> {
        vec![InputMessage::text(
            "system",
            "你在执行 agent 协议冒烟测试。必须严格按用户要求调用工具。",
        )
        .into()]
    }

    fn run_context(&self) -> Vec<TranscriptItem> {
        Vec::new()
    }
}

struct SmokeTools {
    started: mpsc::UnboundedSender<usize>,
    gate: Arc<Semaphore>,
}

impl ToolRuntime for SmokeTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definition = ToolDefinition::new(
            "add",
            json!({
                "type": "object",
                "properties": {
                    "left": {"type": "integer"},
                    "right": {"type": "integer"}
                },
                "required": ["left", "right"],
                "additionalProperties": false
            }),
        );
        definition.description = Some("返回两个整数的和。".to_owned());
        vec![definition]
    }

    fn dispatch<'a>(
        &'a self,
        batch: ToolCallBatch,
    ) -> PortFuture<'a, Result<ToolResultBatch, AgentError>> {
        Box::pin(async move {
            self.started.send(batch.calls.len()).map_err(|_| {
                AgentError::new(AgentErrorKind::ToolDispatch, "smoke_receiver_closed")
            })?;
            let _permit =
                self.gate.acquire().await.map_err(|_| {
                    AgentError::new(AgentErrorKind::ToolDispatch, "smoke_gate_closed")
                })?;

            let results = batch
                .calls
                .iter()
                .map(|call| add_result(call.id.clone(), &call.arguments))
                .collect();
            Ok(ToolResultBatch { results })
        })
    }
}

fn add_result(call_id: ToolCallId, arguments: &Value) -> ToolResult {
    let values = arguments
        .get("left")
        .and_then(Value::as_i64)
        .zip(arguments.get("right").and_then(Value::as_i64));
    match values {
        Some((left, right)) => ToolResult::success_json(call_id, json!({"sum": left + right})),
        None => ToolResult::failure(call_id, "left/right 必须是整数"),
    }
}

struct NoCompaction;

impl Compaction for NoCompaction {
    fn compact<'a>(
        &'a self,
        conversation: &'a [TranscriptItem],
    ) -> PortFuture<'a, Vec<TranscriptItem>> {
        Box::pin(async move { conversation.to_vec() })
    }
}

struct PrintingObserver {
    protocol: Protocol,
}

impl Observer for PrintingObserver {
    fn observe(&self, event: &AgentEvent) {
        eprintln!(
            "[{}][{:?}] {event:?}",
            self.protocol.label(),
            event.metadata().level
        );
    }
}
