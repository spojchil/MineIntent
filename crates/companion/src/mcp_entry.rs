//! 外接代理共用身体工具；事件随工具回执拉取，空闲时另经转接器敲门。
//!
//! 信箱保留最近 100 条带序号事件；序号只在本身体进程内有效。每次回执只带上次
//! 回执以来的新事件，处境也只说变了的行。新接入重送保留的事件与全量处境：新来
//! 的代理没见过它们，上一个接入可能见过一部分，回执里如实说明。
//!
//! 实测（Claude Code 2.1.287）否决过「每次回执重送上一回执」：stdio 不丢消息，
//! 它防的只是「回执已构造、恰在此时被取消」这一窄窗；代价是每次回执翻倍，代理
//! 自己反馈重复内容把新信息埋掉了，危急时尤甚。
//! 取消发生在工具等待期间时不推进阅读位置；已经交给世界的输入不会因此回滚。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent::{
    ContentPart, PortFuture, RunId, ToolBatchId, ToolCall, ToolCallBatch, ToolDefinition,
    ToolResult, ToolRuntime,
};
use dispatch::{Dispatcher, Occupancy};
use screens::{ChatReadMark, ScreenState};
use tokio::sync::Notify;
use wait::{Interruptions, Woke};
use world::{Module, SnapshotSource};

use crate::frame::FrameComposer;
use crate::wake::{SelfIdentity, WakeCursors};

const WAKE_BACKLOG: usize = 100;
const DURING: &str = "——期间——";

struct WakeLine {
    seq: u64,
    text: String,
}

#[derive(Default)]
struct InboxState {
    wakes: VecDeque<WakeLine>,
    sequence: u64,
    /// 已装入回执的末条，不表示宿主已经收到或读到。
    reported: u64,
    attachments: u64,
    replaying_connection: bool,
    /// 在途调用数：有调用在途就不必敲门，那次回执会把事件带回去。
    in_flight: usize,
    /// 已为哪一条之前的事件敲过门。
    nudged_upto: u64,
}

#[derive(Default)]
pub struct Inbox {
    state: Mutex<InboxState>,
    bell: Notify,
}

impl Inbox {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn push_wakes(&self, lines: Vec<String>) {
        if lines.is_empty() {
            return;
        }
        {
            let mut state = self.state.lock().expect("信箱锁中毒");
            for text in lines {
                state.sequence += 1;
                let seq = state.sequence;
                state.wakes.push_back(WakeLine { seq, text });
                if state.wakes.len() > WAKE_BACKLOG {
                    state.wakes.pop_front();
                }
            }
        }
        self.bell.notify_waiters();
    }

    fn attached(&self) {
        let mut state = self.state.lock().expect("信箱锁中毒");
        state.attachments += 1;
        state.replaying_connection = state.attachments > 1;
        state.reported = 0;
        state.nudged_upto = 0;
        drop(state);
        self.bell.notify_waiters();
    }

    fn begin_call(&self) -> CallInFlight<'_> {
        let mut state = self.state.lock().expect("信箱锁中毒");
        state.in_flight += 1;
        CallInFlight {
            inbox: self,
            since: state.reported,
        }
    }

    /// 等到有未取走的事件、且没有调用在途；同一批只敲一次。取消安全：只在返回时记账。
    async fn next_nudge(&self) -> u64 {
        loop {
            let mut ringing = std::pin::pin!(self.bell.notified());
            ringing.as_mut().enable();
            {
                let mut state = self.state.lock().expect("信箱锁中毒");
                let seen = state.reported.max(state.nudged_upto);
                if state.in_flight == 0 && state.sequence > seen {
                    state.nudged_upto = state.sequence;
                    return state.sequence - state.reported;
                }
            }
            ringing.await;
        }
    }

    /// 只在工具完成后的同步装配阶段推进；历史仍留在有界信箱里。
    #[cfg(test)]
    fn reply_lines(&self) -> Vec<String> {
        self.reply_lines_since(u64::MAX)
    }

    /// 带回 `since`（调用开始时的读取位置）与上次回执两者中较早那条之后的事件。
    ///
    /// 顺序调用时两者相同。调用重叠时（`wait` 挂着、代理又调了动作），重叠期间的
    /// 事件两边回执都带：否则先返回的那次把事件带走，`wait` 被它叫醒却说「就在
    /// 下面」而下面什么都没有。
    fn reply_lines_since(&self, since: u64) -> Vec<String> {
        let mut state = self.state.lock().expect("信箱锁中毒");
        let after = state.reported.min(since);
        let mut lines = Vec::new();
        if state.replaying_connection {
            lines.push(
                "接入已重新建立；以下是本进程保留的最近事件，上一个接入可能见过其中一些，请按事件序号识别。"
                    .to_owned(),
            );
        }
        if let Some(first) = state.wakes.front() {
            let dropped = first.seq.saturating_sub(after.saturating_add(1));
            if dropped > 0 {
                lines.push(format!(
                    "更早的 {dropped} 条事件已超出最近 {WAKE_BACKLOG} 条的保留范围，没攒下。"
                ));
            }
        }
        lines.extend(
            state
                .wakes
                .iter()
                .filter(|line| line.seq > after)
                .map(|line| format!("[事件 #{}] {}", line.seq, line.text)),
        );
        state.reported = state.sequence;
        state.replaying_connection = false;
        lines
    }
}

impl Interruptions for Inbox {
    fn until_woken<'a>(&'a self, at_most: Duration) -> PortFuture<'a, Woke> {
        Box::pin(async move {
            // 固定本次等待的基准：并发回执不能把已经叫醒它的事件藏掉。
            let reported = self.state.lock().expect("信箱锁中毒").reported;
            let deadline = tokio::time::Instant::now() + at_most;
            let mut ringing = std::pin::pin!(self.bell.notified());
            loop {
                // notify_waiters 不保存 permit，先注册再查序号。
                ringing.as_mut().enable();
                if self.state.lock().expect("信箱锁中毒").sequence > reported {
                    return Woke::Interrupted;
                }
                tokio::select! {
                    _ = &mut ringing => ringing.set(self.bell.notified()),
                    _ = tokio::time::sleep_until(deadline) => return Woke::Timeout,
                }
            }
        })
    }
}

/// 在途调用的记账；调用被取消（future 丢弃）时同样归还。
struct CallInFlight<'a> {
    inbox: &'a Inbox,
    /// 调用开始时信箱已装入回执的末条。
    since: u64,
}

impl Drop for CallInFlight<'_> {
    fn drop(&mut self) {
        self.inbox.state.lock().expect("信箱锁中毒").in_flight -= 1;
        // 在途数变了：等着敲门的一侧要重新判断。
        self.inbox.bell.notify_waiters();
    }
}

pub struct McpBody {
    dispatcher: Arc<Dispatcher>,
    inbox: Arc<Inbox>,
    frames: Mutex<FrameComposer>,
    snapshots: Arc<dyn SnapshotSource>,
    read_mark: Arc<ChatReadMark>,
    /// 动作依次执行；wait 不拿这把锁，等待期间仍能调身体工具。
    turn: tokio::sync::Mutex<()>,
}

impl McpBody {
    pub fn new(
        dispatcher: Arc<Dispatcher>,
        inbox: Arc<Inbox>,
        snapshots: Arc<dyn SnapshotSource>,
        read_mark: Arc<ChatReadMark>,
    ) -> Self {
        Self {
            dispatcher,
            inbox,
            frames: Mutex::new(FrameComposer::new()),
            snapshots,
            read_mark,
            turn: tokio::sync::Mutex::new(()),
        }
    }
}

impl bridge::body::Body for McpBody {
    fn tools(&self) -> Vec<ToolDefinition> {
        self.dispatcher.definitions()
    }

    fn call(&self, call: ToolCall) -> PortFuture<'_, ToolResult> {
        Box::pin(async move {
            let in_flight = self.inbox.begin_call();
            let waiting = call.name.as_str() == "wait";
            let call_id = call.id.clone();
            let outcome = {
                let _turn = if waiting {
                    None
                } else {
                    Some(self.turn.lock().await)
                };
                self.dispatcher
                    .dispatch(ToolCallBatch {
                        run_id: RunId::new("mcp"),
                        batch_id: ToolBatchId::new(call_id.as_str()),
                        calls: vec![call],
                    })
                    .await
            };
            let mut result = match outcome {
                Ok(mut batch) if batch.results.len() == 1 => batch.results.remove(0),
                Ok(batch) => ToolResult::failure(
                    call_id,
                    format!("编排回了 {} 条结果，预期 1 条", batch.results.len()),
                ),
                Err(error) => ToolResult::failure(call_id, format!("编排失败：{error:?}")),
            };

            // 此后没有 await：取消不会落在推进游标与构造回执之间。
            // 构造回执仍不代表宿主收到；重新接入时重送保留的事件与全量处境。
            // 只有 wait 按自己开始时的位置取：它可能被重叠调用的回执带走的事件叫醒，
            // 回执里得有那件事。排队的动作按此刻的位置取——并行发的两个动作若也按
            // 开始位置，后一个会把前一个回执已带走的事件再带一遍。
            let since = if waiting { in_flight.since } else { u64::MAX };
            let mut during = self.inbox.reply_lines_since(since);
            let snapshot = self.snapshots.latest();
            during.extend(
                self.frames
                    .lock()
                    .expect("帧锁中毒")
                    .compose_on_pull(&snapshot, self.read_mark.position()),
            );
            if !during.is_empty() {
                result.content.push(ContentPart::text(format!(
                    "{DURING}\n{}",
                    during.join("\n")
                )));
            }
            result
        })
    }

    fn attached(&self) {
        // 新接入重建处境与快照内事件的读取位置，并重送信箱保留历史。
        *self.frames.lock().expect("帧锁中毒") = FrameComposer::new();
        self.inbox.attached();
    }

    fn next_nudge(&self) -> PortFuture<'_, u64> {
        Box::pin(self.inbox.next_nudge())
    }
}

pub struct Parts {
    pub listener: tokio::net::TcpListener,
    pub module: Arc<Module>,
    pub dispatcher: Arc<Dispatcher>,
    pub inbox: Arc<Inbox>,
    pub snapshots: Arc<dyn SnapshotSource>,
    pub read_mark: Arc<ChatReadMark>,
    pub screen_state: Arc<ScreenState>,
    pub occupancy: Arc<Occupancy>,
    pub username: String,
}

/// 监听错误与外部停机都先收束接入/调用，再关闭世界连接。
pub async fn run(parts: Parts) -> Result<(), String> {
    let Parts {
        listener,
        module,
        dispatcher,
        inbox,
        snapshots,
        read_mark,
        screen_state,
        occupancy,
        username,
    } = parts;
    let body = Arc::new(McpBody::new(
        dispatcher,
        inbox.clone(),
        snapshots.clone(),
        read_mark,
    ));
    let shutdown = bridge::CancellationToken::new();
    let serving = bridge::body::serve(listener, body, shutdown.clone());
    tokio::pin!(serving);
    let own_key = snapshots.latest().self_state.entity_key.clone();
    let mut cursors = WakeCursors::resume_from(&snapshots.latest());
    let mut listener_ended = false;
    println!("[组合根] 身体已就绪，等 MCP 代理接入（Ctrl+C 停机）");
    let mut outcome = loop {
        tokio::select! {
            result = &mut serving => {
                listener_ended = true;
                break result.map_err(|error| format!("外接入口监听中止：{error}"));
            }
            _ = module.ticked() => {
                let lines = crate::wake_lines(
                    &mut cursors,
                    &snapshots.latest(),
                    SelfIdentity { entity_key: &own_key, username: &username },
                    &screen_state,
                    &occupancy,
                );
                inbox.push_wakes(lines);
            }
            result = tokio::signal::ctrl_c() => {
                break result.map_err(|error| format!("信号监听失败：{error}"));
            }
        }
    };
    shutdown.cancel();
    if !listener_ended {
        outcome = serving
            .await
            .map_err(|error| format!("外接入口收尾失败：{error}"))
            .and(outcome);
    }
    let stopped = crate::stop_world(&module).await;
    outcome.and(stopped)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use agent::ToolResultStatus;
    use bridge::body::Body;
    use dispatch::{AlwaysAlive, Domain, ToolClass, ToolProvider};
    use serde_json::json;
    use world::{ConnectionPhase, Epoch, TickSnapshot};

    use super::*;

    struct Snapshots(Arc<TickSnapshot>);

    impl SnapshotSource for Snapshots {
        fn latest(&self) -> Arc<TickSnapshot> {
            self.0.clone()
        }
    }

    struct Actions {
        inbox: Arc<Inbox>,
        release: Notify,
        calls: AtomicUsize,
    }

    impl ToolProvider for Actions {
        fn tools(&self) -> Vec<(ToolDefinition, ToolClass)> {
            vec![(
                ToolDefinition::new("act", json!({"type": "object"})),
                ToolClass::Body {
                    domain: Domain::Movement,
                },
            )]
        }

        fn call<'a>(&'a self, call: ToolCall) -> PortFuture<'a, ToolResult> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if call.arguments["publish"].as_bool() == Some(true) {
                    self.inbox.push_wakes(vec!["身体动作产生了事件".to_owned()]);
                }
                if call.arguments["block"].as_bool() == Some(true) {
                    self.release.notified().await;
                }
                ToolResult::success(call.id, vec![ContentPart::text("动作回执")])
            })
        }
    }

    fn fixture() -> (Arc<McpBody>, Arc<Inbox>, Arc<Actions>) {
        let inbox = Inbox::new();
        let actions = Arc::new(Actions {
            inbox: inbox.clone(),
            release: Notify::new(),
            calls: AtomicUsize::new(0),
        });
        let dispatcher = Arc::new(
            Dispatcher::new(
                vec![
                    actions.clone(),
                    Arc::new(wait::WaitTools::new(inbox.clone())),
                ],
                Arc::new(Occupancy::new()),
                Arc::new(AlwaysAlive),
            )
            .unwrap(),
        );
        let mut snapshot = TickSnapshot::empty(Epoch(1), 1, ConnectionPhase::Ready);
        snapshot.pickups.entries.push(world::PickupEntry {
            seq: 1,
            tick: 1,
            occurred_at: std::time::SystemTime::UNIX_EPOCH,
            by_self: true,
            by: None,
            item_name: Some("minecraft:coal".to_owned()),
            count: 1,
        });
        let body = Arc::new(McpBody::new(
            dispatcher,
            inbox.clone(),
            Arc::new(Snapshots(Arc::new(snapshot))),
            Arc::new(ChatReadMark::new()),
        ));
        body.attached();
        (body, inbox, actions)
    }

    fn text_of(result: &ToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 观察真实调用已经挂起，无需为测试在生产代码保留在途计数。
    async fn start_pending_call(
        body: Arc<McpBody>,
        call: ToolCall,
    ) -> tokio::task::JoinHandle<ToolResult> {
        let (started, pending) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut future = body.call(call);
            std::future::poll_fn(|cx| {
                assert!(std::future::Future::poll(future.as_mut(), cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            let _ = started.send(());
            future.await
        });
        tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .expect("调用应进入挂起状态")
            .expect("调用任务不应提前结束");
        task
    }

    #[tokio::test]
    async fn wait_leaves_actions_free_and_keeps_its_wake_after_another_reply() {
        let (body, _, _) = fixture();
        let waiting = start_pending_call(
            body.clone(),
            ToolCall::new("wait-1", "wait", json!({"seconds": 30})),
        )
        .await;
        let action = tokio::time::timeout(
            Duration::from_secs(1),
            body.call(ToolCall::new("act-1", "act", json!({"publish": true}))),
        )
        .await
        .expect("wait 不得挡住动作");
        assert_eq!(action.status, ToolResultStatus::Success);
        assert!(text_of(&action).contains("[事件 #1]"));
        let waited = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("另一回执不能吞掉此次等待的唤醒")
            .unwrap();
        assert!(text_of(&waited).contains("提前醒"));
        assert!(text_of(&waited).contains("[事件 #1]"));
    }

    #[tokio::test]
    async fn cancelling_wait_keeps_later_events_and_reconnecting_replays_history() {
        let (body, inbox, _) = fixture();
        let waiting = start_pending_call(
            body.clone(),
            ToolCall::new("wait-1", "wait", json!({"seconds": 30})),
        )
        .await;
        waiting.abort();
        assert!(waiting.await.unwrap_err().is_cancelled());

        inbox.push_wakes(vec!["取消之后的新事件".to_owned()]);
        let first = body.call(ToolCall::new("act-1", "act", json!({}))).await;
        assert!(text_of(&first).contains("取消之后的新事件"));
        body.detached();
        body.attached();
        let reconnected = body.call(ToolCall::new("act-2", "act", json!({}))).await;
        assert!(text_of(&reconnected).contains("[事件 #1]"));
        assert!(text_of(&reconnected).contains("接入已重新建立"));
        assert_eq!(
            inbox.until_woken(Duration::ZERO).await,
            Woke::Timeout,
            "历史重送不应让 wait 一直立刻返回"
        );
    }

    #[tokio::test]
    async fn cancelling_an_action_keeps_events_and_releases_the_action_lock() {
        let (body, inbox, _) = fixture();
        let acting = start_pending_call(
            body.clone(),
            ToolCall::new("blocked", "act", json!({"publish": true, "block": true})),
        )
        .await;
        acting.abort();
        assert!(acting.await.unwrap_err().is_cancelled());
        assert_eq!(inbox.state.lock().unwrap().reported, 0);
        let next = tokio::time::timeout(
            Duration::from_secs(1),
            body.call(ToolCall::new("next", "act", json!({}))),
        )
        .await
        .expect("取消后应释放动作锁");
        assert!(text_of(&next).contains("[事件 #1]"));
    }

    /// 处境只说一次；新接入从头再说一遍（新来的代理没见过）。
    #[tokio::test]
    async fn the_situation_is_said_once_and_again_for_a_new_attachment() {
        let (body, _, _) = fixture();
        let first = body.call(ToolCall::new("first", "act", json!({}))).await;
        assert!(text_of(&first).contains("coal"));
        let second = body.call(ToolCall::new("second", "act", json!({}))).await;
        assert!(
            !text_of(&second).contains("——期间——"),
            "没变就不该重复：{}",
            text_of(&second)
        );
        body.detached();
        body.attached();
        let reattached = body.call(ToolCall::new("third", "act", json!({}))).await;
        assert!(text_of(&reattached).contains("——期间——"));
    }

    /// 代理在一条消息里并行发两个动作：事件只随先返回的那个回执带回一次。
    #[tokio::test]
    async fn parallel_actions_do_not_repeat_each_others_events() {
        let (body, _, actions) = fixture();
        let first = start_pending_call(
            body.clone(),
            ToolCall::new("first", "act", json!({"publish": true, "block": true})),
        )
        .await;
        let second =
            start_pending_call(body.clone(), ToolCall::new("second", "act", json!({}))).await;
        actions.release.notify_one();
        let (first, second) = tokio::time::timeout(Duration::from_secs(1), async {
            (first.await.unwrap(), second.await.unwrap())
        })
        .await
        .unwrap();
        let carried = [&first, &second]
            .iter()
            .filter(|reply| text_of(reply).contains("[事件 #1]"))
            .count();
        assert_eq!(carried, 1, "{}\n---\n{}", text_of(&first), text_of(&second));
    }

    #[tokio::test]
    async fn actions_are_serial_even_though_wait_is_not() {
        let (body, _, actions) = fixture();
        let first = start_pending_call(
            body.clone(),
            ToolCall::new("first", "act", json!({"block": true})),
        )
        .await;
        let second =
            start_pending_call(body.clone(), ToolCall::new("second", "act", json!({}))).await;
        assert_eq!(actions.calls.load(Ordering::SeqCst), 1);
        actions.release.notify_one();
        let (first, second) = tokio::time::timeout(Duration::from_secs(1), async {
            (first.await.unwrap(), second.await.unwrap())
        })
        .await
        .unwrap();
        assert_eq!(first.status, ToolResultStatus::Success);
        assert_eq!(second.status, ToolResultStatus::Success);
        assert_eq!(actions.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cancelling_a_queued_action_never_dispatches_it() {
        let (body, _, actions) = fixture();
        let first = start_pending_call(
            body.clone(),
            ToolCall::new("first", "act", json!({"block": true})),
        )
        .await;
        let queued =
            start_pending_call(body.clone(), ToolCall::new("cancelled", "act", json!({}))).await;
        queued.abort();
        assert!(queued.await.unwrap_err().is_cancelled());
        actions.release.notify_one();
        tokio::time::timeout(Duration::from_secs(1), first)
            .await
            .unwrap()
            .unwrap();
        let next = body.call(ToolCall::new("next", "act", json!({}))).await;
        assert_eq!(next.status, ToolResultStatus::Success);
        assert_eq!(actions.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn history_is_bounded_and_reconnect_reports_the_gap() {
        let inbox = Inbox::new();
        inbox.attached();
        inbox.push_wakes((1..=103).map(|n| format!("事件内容 {n}")).collect());
        assert_eq!(inbox.state.lock().unwrap().wakes.len(), WAKE_BACKLOG);
        let first = inbox.reply_lines().join("\n");
        assert!(first.contains("更早的 3 条事件"));
        assert!(first.contains("[事件 #4]"));
        assert!(first.contains("[事件 #103]"));
        assert!(!first.contains("[事件 #3]"));
        inbox.attached();
        let replay = inbox.reply_lines().join("\n");
        assert!(replay.contains("接入已重新建立"));
        assert!(replay.contains("更早的 3 条事件"));
    }

    #[test]
    fn each_event_reaches_one_reply_without_draining_history() {
        let inbox = Inbox::new();
        inbox.push_wakes(vec!["甲".to_owned()]);
        assert!(inbox.reply_lines().join("\n").contains("[事件 #1] 甲"));
        inbox.push_wakes(vec!["乙".to_owned()]);
        let second = inbox.reply_lines().join("\n");
        assert!(!second.contains("[事件 #1]"), "{second}");
        assert!(second.contains("[事件 #2] 乙"));
        assert!(inbox.reply_lines().is_empty());
        assert_eq!(inbox.state.lock().unwrap().wakes.len(), 2);
    }

    #[tokio::test]
    async fn an_idle_event_is_nudged_once_per_batch() {
        let inbox = Inbox::new();
        inbox.push_wakes(vec!["甲".to_owned()]);
        assert_eq!(inbox.next_nudge().await, 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), inbox.next_nudge())
                .await
                .is_err(),
            "同一批不该敲第二次"
        );
        inbox.push_wakes(vec!["乙".to_owned()]);
        assert_eq!(inbox.next_nudge().await, 2);
    }

    /// 有调用在途就不敲：那次回执会把事件带回去。调用结束（含被取消）后才敲。
    #[tokio::test]
    async fn no_nudge_while_a_call_is_in_flight() {
        let (body, inbox, actions) = fixture();
        let acting = start_pending_call(
            body.clone(),
            ToolCall::new("blocked", "act", json!({"publish": true, "block": true})),
        )
        .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(50), inbox.next_nudge())
                .await
                .is_err(),
            "调用在途时不该敲门"
        );
        acting.abort();
        assert!(acting.await.unwrap_err().is_cancelled());
        let pending = tokio::time::timeout(Duration::from_secs(1), inbox.next_nudge())
            .await
            .expect("取消的调用没带回事件，空闲后应敲门");
        assert_eq!(pending, 1);
        drop(actions);
    }

    /// 回执带走事件后，空闲也不再敲。
    #[tokio::test]
    async fn a_reply_that_carried_the_event_leaves_nothing_to_nudge() {
        let (body, inbox, _) = fixture();
        let reply = body
            .call(ToolCall::new("act", "act", json!({"publish": true})))
            .await;
        assert!(text_of(&reply).contains("[事件 #1]"));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), inbox.next_nudge())
                .await
                .is_err()
        );
    }
}
