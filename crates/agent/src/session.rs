//! 会话：持有一段持续的对话，驱动一轮轮的状态机。
//!
//! 忙/闲原子对采 codex 形状（`session/inject.rs`）：检查与置位在同一把锁下，
//! 拒绝时原物奉还并带稳定原因。对话跨轮延续；轮末信箱有剩件立即成为
//! 下一轮触发（忙→闲切换点零遗漏），景蒸发。

use std::sync::Arc;

use mineintent_contracts::agent::{AgentError, ModelUsage, RunId};
use serde_json::Value;
use tokio::sync::{Mutex, Notify};

use crate::mailbox::Mailbox;
use crate::ports::{Compaction, Message, Model, ModelRequest, PromptSource, Tools};
use crate::run::{PlannedToolCall, Turn, TurnStep};

/// 会话配置。唯一的预算就是上下文窗：对话段序列化字节数超过阈值即触发压缩。
/// 声明式数据，归会话持有；策略只答"怎么压"。
#[derive(Clone, Copy, Debug)]
pub struct SessionConfig {
    pub compaction_trigger_bytes: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            // 粗阈值占位；真实取值随④的供应商上下文窗定（规格 ⑤-2）。
            compaction_trigger_bytes: 400_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartRejectedReason {
    /// 已有活动轮（竞态窗口里被抢了）。调用方可转投 `inject_if_running`。
    Busy,
    /// 会话正在停止或已停止。
    Stopping,
}

/// 拒绝时原物奉还，调用方决定重试还是丢弃。
#[derive(Debug)]
pub struct StartRejected {
    pub reason: StartRejectedReason,
    pub messages: Vec<Message>,
}

#[derive(Debug)]
pub enum TurnOutcome {
    /// 正常走到终文。closing 是最后一轮的收尾文本。
    Completed {
        closing: String,
        usage: Option<ModelUsage>,
    },
    /// 被 `stop` 截停；已跑的部分如实留在对话里。
    Stopped,
    /// 循环失败（模型/工具/状态机错误）。已跑的部分如实留在对话里。
    Failed { error: AgentError },
}

struct SessionState {
    /// 跨轮延续的对话段（不含 persona/situation——那两段每轮由①重导出）。
    conversation: Vec<Message>,
    mailbox: Mailbox,
    turn_active: bool,
    stopping: bool,
    turn_seq: u64,
}

pub struct AgentSession {
    prompt: Arc<dyn PromptSource>,
    tools: Arc<dyn Tools>,
    compaction: Arc<dyn Compaction>,
    model: Arc<dyn Model>,
    config: SessionConfig,
    state: Mutex<SessionState>,
    turn_ended: Notify,
}

impl AgentSession {
    pub fn new(
        prompt: Arc<dyn PromptSource>,
        tools: Arc<dyn Tools>,
        compaction: Arc<dyn Compaction>,
        model: Arc<dyn Model>,
        config: SessionConfig,
    ) -> Self {
        Self {
            prompt,
            tools,
            compaction,
            model,
            config,
            state: Mutex::new(SessionState {
                conversation: Vec::new(),
                mailbox: Mailbox::default(),
                turn_active: false,
                stopping: false,
                turn_seq: 0,
            }),
            turn_ended: Notify::new(),
        }
    }

    /// 忙：件进信箱，下一次模型请求前可见。闲：原物奉还，调用方转投
    /// [`Self::start_if_idle`]。
    pub async fn inject_if_running(&self, pieces: Vec<Message>) -> Result<(), Vec<Message>> {
        let mut state = self.state.lock().await;
        if state.turn_active && !state.stopping {
            state.mailbox.post_pieces(pieces);
            Ok(())
        } else {
            Err(pieces)
        }
    }

    /// 景（处境更新）：在箱内顶替。闲时原物奉还——景是可重导出的状态，
    /// 无轮可插时无处可去也无需去（下一轮 situation 由①现拉）。
    pub async fn inject_scene_if_running(&self, scene: Message) -> Result<(), Message> {
        let mut state = self.state.lock().await;
        if state.turn_active && !state.stopping {
            state.mailbox.post_scene(scene);
            Ok(())
        } else {
            Err(scene)
        }
    }

    /// 闲时起轮。检查与置位原子；拒绝带稳定原因 + 原物奉还。
    /// 本函数驱动到本次唤醒的所有轮结束（信箱剩件会连轮）；
    /// 调用方通常 `spawn` 它。
    pub async fn start_if_idle(&self, trigger: Vec<Message>) -> Result<TurnOutcome, StartRejected> {
        {
            let mut state = self.state.lock().await;
            if state.stopping {
                return Err(StartRejected {
                    reason: StartRejectedReason::Stopping,
                    messages: trigger,
                });
            }
            if state.turn_active {
                return Err(StartRejected {
                    reason: StartRejectedReason::Busy,
                    messages: trigger,
                });
            }
            state.turn_active = true;
            state.mailbox.post_pieces(trigger);
        }
        let outcome = self.drive_until_quiet().await;
        {
            let mut state = self.state.lock().await;
            state.turn_active = false;
        }
        self.turn_ended.notify_waiters();
        Ok(outcome)
    }

    /// 停止：置位后等活动轮在下一个请求边界让出。幂等。
    pub async fn stop(&self) {
        loop {
            let notified = self.turn_ended.notified();
            {
                let mut state = self.state.lock().await;
                state.stopping = true;
                if !state.turn_active {
                    return;
                }
            }
            notified.await;
        }
    }

    /// 驱动轮，直到信箱无剩件或被停止。件晋升为下一轮触发在这儿发生。
    async fn drive_until_quiet(&self) -> TurnOutcome {
        loop {
            let outcome = self.drive_one_turn().await;
            let mut state = self.state.lock().await;
            state.mailbox.end_of_turn();
            let continue_with_leftover = matches!(outcome, TurnOutcome::Completed { .. })
                && state.mailbox.has_pieces()
                && !state.stopping;
            if continue_with_leftover {
                continue;
            }
            return outcome;
        }
    }

    async fn drive_one_turn(&self) -> TurnOutcome {
        // persona/situation 每轮重导出——压缩的保护区，摘要永不背负它们。
        let persona = self.prompt.persona();
        let situation = self.prompt.situation();
        let (run_id, conversation) = {
            let mut state = self.state.lock().await;
            state.turn_seq += 1;
            (
                RunId::new(format!("turn-{}", state.turn_seq))
                    .unwrap_or_else(|_| unreachable!("固定格式的 run id 恒合法")),
                state.conversation.clone(),
            )
        };
        let mut initial = persona;
        initial.extend(situation);
        let prefix_len = initial.len();
        initial.extend(conversation);
        let mut turn = Turn::new(run_id, initial);

        let outcome = self.drive_turn_steps(&mut turn).await;

        // 无论成败截停，跑到哪儿算哪儿——对话如实延续（W07a 的方向）。
        let new_conversation: Vec<Message> = turn.messages()[prefix_len..].to_vec();
        let new_conversation = self.maybe_compact(new_conversation).await;
        let mut state = self.state.lock().await;
        state.conversation = new_conversation;
        outcome
    }

    async fn drive_turn_steps(&self, turn: &mut Turn) -> TurnOutcome {
        loop {
            // 请求边界：查停 + 全量排空信箱（件序在前，景最后）。
            if turn.at_request_boundary() {
                let drained = {
                    let mut state = self.state.lock().await;
                    if state.stopping {
                        return TurnOutcome::Stopped;
                    }
                    state.mailbox.drain()
                };
                for message in drained {
                    if let Err(error) = turn.append_user_message(message) {
                        return TurnOutcome::Failed { error };
                    }
                }
            }
            let step = match turn.next_step() {
                Ok(step) => step,
                Err(error) => return TurnOutcome::Failed { error },
            };
            match step {
                TurnStep::CallModel { messages } => {
                    let request = ModelRequest {
                        messages,
                        tools: self.tools.definitions(),
                    };
                    let completion = match self.model.complete(request).await {
                        Ok(completion) => completion,
                        Err(error) => return TurnOutcome::Failed { error },
                    };
                    if let Err(error) = turn.model_response(completion) {
                        return TurnOutcome::Failed { error };
                    }
                }
                TurnStep::CallTools { calls } => {
                    let mut results = Vec::with_capacity(calls.len());
                    for call in calls {
                        match call {
                            PlannedToolCall::LocalResult(result) => results.push(result),
                            PlannedToolCall::Dispatch(invocation) => {
                                results.push(self.tools.call(invocation).await);
                            }
                        }
                    }
                    if let Err(error) = turn.tool_results(results) {
                        return TurnOutcome::Failed { error };
                    }
                }
                TurnStep::Done { closing, usage } => {
                    return TurnOutcome::Completed { closing, usage };
                }
            }
        }
    }

    /// 触发判定：对话段序列化字节数。粗但机械——这是循环的数据，不是策略的判断。
    async fn maybe_compact(&self, conversation: Vec<Message>) -> Vec<Message> {
        let bytes: usize = conversation
            .iter()
            .map(|message| {
                serde_json::to_string(&Value::Object(message.clone()))
                    .map(|s| s.len())
                    .unwrap_or(0)
            })
            .sum();
        if bytes <= self.config.compaction_trigger_bytes {
            return conversation;
        }
        self.compaction.compact(&conversation).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{ModelCompletion, PortFuture};
    use crate::run::ToolResult;
    use mineintent_contracts::agent::{JsonObject, ToolInvocation, WireToolDefinition};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    fn user_msg(text: &str) -> Message {
        let mut m = Message::new();
        m.insert("role".to_owned(), Value::String("user".to_owned()));
        m.insert("content".to_owned(), Value::String(text.to_owned()));
        m
    }

    fn final_completion(text: &str) -> ModelCompletion {
        let mut message = JsonObject::new();
        message.insert("role".to_owned(), Value::String("assistant".to_owned()));
        message.insert("content".to_owned(), Value::String(text.to_owned()));
        ModelCompletion {
            message: Some(message),
            ..Default::default()
        }
    }

    fn tool_completion(call_id: &str, name: &str) -> ModelCompletion {
        let mut message = JsonObject::new();
        message.insert("role".to_owned(), Value::String("assistant".to_owned()));
        message.insert(
            "tool_calls".to_owned(),
            serde_json::json!([{
                "id": call_id,
                "type": "function",
                "function": {"name": name, "arguments": "{}"}
            }]),
        );
        ModelCompletion {
            message: Some(message),
            ..Default::default()
        }
    }

    fn message_texts(messages: &[Message]) -> Vec<String> {
        messages
            .iter()
            .filter_map(|m| m.get("content").and_then(Value::as_str))
            .map(str::to_owned)
            .collect()
    }

    /// ①：persona 静态；situation 带计数器，证明"每轮重导出"。
    struct CountingPrompt {
        situations: AtomicUsize,
    }

    impl PromptSource for CountingPrompt {
        fn persona(&self) -> Vec<Message> {
            let mut m = Message::new();
            m.insert("role".to_owned(), Value::String("system".to_owned()));
            m.insert("content".to_owned(), Value::String("persona".to_owned()));
            vec![m]
        }

        fn situation(&self) -> Vec<Message> {
            let n = self.situations.fetch_add(1, Ordering::SeqCst) + 1;
            vec![user_msg(&format!("situation-{n}"))]
        }
    }

    /// ④：请求经通道交给测试检视，completion 由测试按需投喂——时序全受控。
    struct ChannelModel {
        requests: mpsc::UnboundedSender<Vec<Message>>,
        completions: Mutex<mpsc::UnboundedReceiver<Result<ModelCompletion, AgentError>>>,
    }

    impl Model for ChannelModel {
        fn complete<'a>(
            &'a self,
            request: ModelRequest,
        ) -> PortFuture<'a, Result<ModelCompletion, AgentError>> {
            Box::pin(async move {
                self.requests
                    .send(request.messages)
                    .expect("test holds receiver");
                self.completions
                    .lock()
                    .await
                    .recv()
                    .await
                    .expect("test sends completion")
            })
        }
    }

    /// ②：执行前先通知测试、再等测试放行——制造"工具执行期间"这个注入窗口。
    struct GatedTools {
        started: mpsc::UnboundedSender<()>,
        gate: Arc<tokio::sync::Semaphore>,
    }

    impl Tools for GatedTools {
        fn definitions(&self) -> Vec<WireToolDefinition> {
            Vec::new()
        }

        fn call<'a>(&'a self, invocation: ToolInvocation) -> PortFuture<'a, ToolResult> {
            Box::pin(async move {
                self.started.send(()).expect("test holds receiver");
                let _permit = self.gate.acquire().await.expect("gate open");
                let mut output = JsonObject::new();
                output.insert("status".to_owned(), Value::String("ok".to_owned()));
                ToolResult::new(invocation.tool_call_id, output)
            })
        }
    }

    struct MarkerCompaction;

    impl Compaction for MarkerCompaction {
        fn compact<'a>(&'a self, _conversation: &'a [Message]) -> PortFuture<'a, Vec<Message>> {
            Box::pin(async move { vec![user_msg("[compacted]")] })
        }
    }

    struct Fixture {
        session: Arc<AgentSession>,
        requests: mpsc::UnboundedReceiver<Vec<Message>>,
        completions: mpsc::UnboundedSender<Result<ModelCompletion, AgentError>>,
        tool_started: mpsc::UnboundedReceiver<()>,
        tool_gate: Arc<tokio::sync::Semaphore>,
        situations: Arc<CountingPrompt>,
    }

    fn fixture(config: SessionConfig) -> Fixture {
        let (req_tx, req_rx) = mpsc::unbounded_channel();
        let (comp_tx, comp_rx) = mpsc::unbounded_channel();
        let (started_tx, started_rx) = mpsc::unbounded_channel();
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let prompt = Arc::new(CountingPrompt {
            situations: AtomicUsize::new(0),
        });
        let session = Arc::new(AgentSession::new(
            prompt.clone(),
            Arc::new(GatedTools {
                started: started_tx,
                gate: gate.clone(),
            }),
            Arc::new(MarkerCompaction),
            Arc::new(ChannelModel {
                requests: req_tx,
                completions: Mutex::new(comp_rx),
            }),
            config,
        ));
        Fixture {
            session,
            requests: req_rx,
            completions: comp_tx,
            tool_started: started_rx,
            tool_gate: gate,
            situations: prompt,
        }
    }

    #[tokio::test]
    async fn idle_injection_returns_the_messages_untouched() {
        let f = fixture(SessionConfig::default());
        let rejected = f
            .session
            .inject_if_running(vec![user_msg("hello")])
            .await
            .expect_err("no active turn");
        assert_eq!(message_texts(&rejected), vec!["hello"]);
    }

    #[tokio::test]
    async fn a_turn_completes_and_the_conversation_persists_across_turns() {
        let mut f = fixture(SessionConfig::default());
        let session = f.session.clone();
        let handle =
            tokio::spawn(async move { session.start_if_idle(vec![user_msg("hi")]).await });

        let first = f.requests.recv().await.expect("first request");
        // 前缀:persona + situation-1;触发件经信箱排空跟在其后。
        assert_eq!(message_texts(&first), vec!["persona", "situation-1", "hi"]);
        f.completions.send(Ok(final_completion("done"))).unwrap();

        let outcome = handle.await.unwrap().expect("not rejected");
        assert!(matches!(outcome, TurnOutcome::Completed { ref closing, .. } if closing == "done"));

        // 第二轮:situation 重导出(situation-2),对话段(hi + 终文)延续。
        let session = f.session.clone();
        let handle =
            tokio::spawn(async move { session.start_if_idle(vec![user_msg("again")]).await });
        let second = f.requests.recv().await.expect("second request");
        assert_eq!(
            message_texts(&second),
            vec!["persona", "situation-2", "hi", "done", "again"]
        );
        f.completions.send(Ok(final_completion("bye"))).unwrap();
        handle.await.unwrap().expect("not rejected");
        assert_eq!(f.situations.situations.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn pieces_injected_while_tools_run_arrive_at_the_next_request_boundary() {
        let mut f = fixture(SessionConfig::default());
        let session = f.session.clone();
        let handle =
            tokio::spawn(async move { session.start_if_idle(vec![user_msg("dig")]).await });

        let _first = f.requests.recv().await.expect("first request");
        f.completions
            .send(Ok(tool_completion("call-1", "mine")))
            .unwrap();

        // 工具执行中(被门拦住)——这是玩家说话的窗口。
        f.tool_started.recv().await.expect("tool started");
        f.session
            .inject_if_running(vec![user_msg("stop please")])
            .await
            .expect("turn is active");
        f.tool_gate.add_permits(1);

        // 下一次请求:工具结果之后、紧跟注入的件。
        let second = f.requests.recv().await.expect("second request");
        let texts = message_texts(&second);
        assert_eq!(texts.last().map(String::as_str), Some("stop please"));
        assert!(texts.iter().any(|t| t.contains("\"status\":\"ok\"")));

        f.completions.send(Ok(final_completion("ok"))).unwrap();
        handle.await.unwrap().expect("not rejected");
    }

    #[tokio::test]
    async fn leftover_pieces_start_the_next_turn_and_scenes_evaporate() {
        let mut f = fixture(SessionConfig::default());
        let session = f.session.clone();
        let handle =
            tokio::spawn(async move { session.start_if_idle(vec![user_msg("hi")]).await });

        let _first = f.requests.recv().await.expect("first request");
        // 模型答复在途时,玩家又说了一句、处境也更新了一版。
        f.session
            .inject_if_running(vec![user_msg("one more")])
            .await
            .expect("active");
        f.session
            .inject_scene_if_running(user_msg("scene-stale"))
            .await
            .expect("active");
        // 终文直达轮末:件晋升为下一轮触发,景蒸发。
        f.completions.send(Ok(final_completion("done"))).unwrap();

        let second = f.requests.recv().await.expect("auto second turn");
        let texts = message_texts(&second);
        assert!(texts.contains(&"one more".to_owned()));
        assert!(!texts.contains(&"scene-stale".to_owned()));
        assert!(texts.contains(&"situation-2".to_owned()));

        f.completions.send(Ok(final_completion("bye"))).unwrap();
        let outcome = handle.await.unwrap().expect("not rejected");
        assert!(matches!(outcome, TurnOutcome::Completed { ref closing, .. } if closing == "bye"));
    }

    #[tokio::test]
    async fn busy_start_is_rejected_with_the_messages_returned() {
        let mut f = fixture(SessionConfig::default());
        let session = f.session.clone();
        let handle =
            tokio::spawn(async move { session.start_if_idle(vec![user_msg("hi")]).await });
        let _first = f.requests.recv().await.expect("first request");

        let rejected = f
            .session
            .start_if_idle(vec![user_msg("late")])
            .await
            .expect_err("busy");
        assert_eq!(rejected.reason, StartRejectedReason::Busy);
        assert_eq!(message_texts(&rejected.messages), vec!["late"]);

        f.completions.send(Ok(final_completion("done"))).unwrap();
        handle.await.unwrap().expect("not rejected");
    }

    #[tokio::test]
    async fn compaction_replaces_the_conversation_when_the_window_overflows() {
        let mut f = fixture(SessionConfig {
            compaction_trigger_bytes: 1,
        });
        let session = f.session.clone();
        let handle =
            tokio::spawn(async move { session.start_if_idle(vec![user_msg("hi")]).await });
        let _first = f.requests.recv().await.expect("first request");
        f.completions.send(Ok(final_completion("done"))).unwrap();
        handle.await.unwrap().expect("not rejected");

        // 第二轮的对话段只剩压缩标记;persona/situation 在保护区外照常。
        let session = f.session.clone();
        let handle =
            tokio::spawn(async move { session.start_if_idle(vec![user_msg("again")]).await });
        let second = f.requests.recv().await.expect("second request");
        assert_eq!(
            message_texts(&second),
            vec!["persona", "situation-2", "[compacted]", "again"]
        );
        f.completions.send(Ok(final_completion("bye"))).unwrap();
        handle.await.unwrap().expect("not rejected");
    }

    #[tokio::test]
    async fn stop_ends_the_turn_at_the_next_request_boundary() {
        let mut f = fixture(SessionConfig::default());
        let session = f.session.clone();
        let handle =
            tokio::spawn(async move { session.start_if_idle(vec![user_msg("dig")]).await });
        let _first = f.requests.recv().await.expect("first request");
        f.completions
            .send(Ok(tool_completion("call-1", "mine")))
            .unwrap();
        f.tool_started.recv().await.expect("tool started");

        // 工具还被门拦着时请求停止;放行后轮应在下一个边界让出。
        let session = f.session.clone();
        let stop = tokio::spawn(async move { session.stop().await });
        f.tool_gate.add_permits(1);

        let outcome = handle.await.unwrap().expect("not rejected");
        assert!(matches!(outcome, TurnOutcome::Stopped));
        stop.await.unwrap();

        // 停止后不再接受新轮。
        let rejected = f
            .session
            .start_if_idle(vec![user_msg("late")])
            .await
            .expect_err("stopping");
        assert_eq!(rejected.reason, StartRejectedReason::Stopping);
    }
}
