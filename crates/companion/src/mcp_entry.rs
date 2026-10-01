//! 外接代理共用身体工具；事件随工具回执拉取，不要求宿主能被通知主动唤醒。
//!
//! 信箱保留最近 100 条带序号事件；序号只在本身体进程内有效。回执重复上一回执
//! 的新事件，重连接则重送保留历史，并说明可能重复及已丢失数量。这不是宿主
//! 确认协议，也不保证恰好一次。
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
    /// 上一回执的新事件起点；下一回执将其重送一次。
    replay_after: u64,
    attachments: u64,
    replaying_connection: bool,
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
        state.replay_after = 0;
    }

    /// 只在工具完成后的同步装配阶段推进；历史仍留在有界信箱里。
    fn reply_lines(&self) -> Vec<String> {
        let mut state = self.state.lock().expect("信箱锁中毒");
        let after = state.replay_after;
        let previous = state.reported;
        let mut lines = Vec::new();
        if state.replaying_connection {
            lines.push(
                "接入已重新建立；以下是本进程保留的最近事件，可能重送，请按事件序号识别。"
                    .to_owned(),
            );
        } else if state
            .wakes
            .iter()
            .any(|line| line.seq > after && line.seq <= previous)
        {
            lines.push("以下包含上一回执中的事件，可能重送，请按事件序号识别。".to_owned());
        }
        if let Some(first) = state.wakes.front() {
            let dropped = first.seq.saturating_sub(after.saturating_add(1));
            if dropped > 0 {
                lines.push(format!(
                    "更早的 {dropped} 条事件已超出最近 {WAKE_BACKLOG} 条的保留范围，无法重送。"
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
        state.replay_after = previous;
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

pub struct McpBody {
    dispatcher: Arc<Dispatcher>,
    inbox: Arc<Inbox>,
    frames: Mutex<FrameReplies>,
    snapshots: Arc<dyn SnapshotSource>,
    read_mark: Arc<ChatReadMark>,
    /// 动作依次执行；wait 不拿这把锁，等待期间仍能调身体工具。
    turn: tokio::sync::Mutex<()>,
}

struct FrameReplies {
    composer: FrameComposer,
    /// 同事件窗口一样，只重送上一回执新装配的行；不把重送再存一遍。
    previous: Vec<String>,
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
            frames: Mutex::new(FrameReplies {
                composer: FrameComposer::new(),
                previous: Vec::new(),
            }),
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
            let call_id = call.id.clone();
            let outcome = {
                let _turn = if call.name.as_str() == "wait" {
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
            // 构造回执仍不代表宿主收到；下一回执及重接有明确的有限重送。
            let mut during = self.inbox.reply_lines();
            let snapshot = self.snapshots.latest();
            {
                let mut frames = self.frames.lock().expect("帧锁中毒");
                let current = frames
                    .composer
                    .compose_on_pull(&snapshot, self.read_mark.position());
                let previous = std::mem::replace(&mut frames.previous, current.clone());
                if !previous.is_empty() {
                    during.push(
                        "上次回执附带的处境、拾取与进展（可能重送；以下是当时的记录）：".to_owned(),
                    );
                    during.extend(previous);
                }
                if !current.is_empty() {
                    during.push("本次拉取的处境与记录：".to_owned());
                    during.extend(current);
                }
            }
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
        self.frames.lock().expect("帧锁中毒").composer = FrameComposer::new();
        self.inbox.attached();
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
        assert!(text_of(&reconnected).contains("可能重送"));
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

    #[tokio::test]
    async fn the_previous_frame_repeats_pickups_even_after_the_snapshot_window_moves_on() {
        let (body, _, _) = fixture();
        let discarded = body.call(ToolCall::new("lost", "act", json!({}))).await;
        assert!(text_of(&discarded).contains("coal"));
        // 首份回执被调用方丢弃；模拟下一张快照已不再保留那次拾取。
        let mut body = match Arc::try_unwrap(body) {
            Ok(body) => body,
            Err(_) => panic!("没有在途调用，应可独占身体"),
        };
        body.snapshots = Arc::new(Snapshots(Arc::new(TickSnapshot::empty(
            Epoch(1),
            100,
            ConnectionPhase::Ready,
        ))));
        body.detached();
        body.attached();
        let recovered = body.call(ToolCall::new("after", "act", json!({}))).await;
        assert!(text_of(&recovered).contains("可能重送"));
        assert!(text_of(&recovered).contains("coal"));
        let next = body.call(ToolCall::new("next", "act", json!({}))).await;
        assert!(!text_of(&next).contains("coal"));
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
        assert!(replay.contains("可能重送"));
        assert!(replay.contains("更早的 3 条事件"));
    }

    #[test]
    fn the_previous_new_batch_is_repeated_once_without_draining_history() {
        let inbox = Inbox::new();
        inbox.push_wakes(vec!["甲".to_owned()]);
        assert!(inbox.reply_lines().join("\n").contains("[事件 #1] 甲"));
        inbox.push_wakes(vec!["乙".to_owned()]);
        let second = inbox.reply_lines().join("\n");
        assert!(second.contains("可能重送"));
        assert!(second.contains("[事件 #1] 甲"));
        assert!(second.contains("[事件 #2] 乙"));
        let third = inbox.reply_lines().join("\n");
        assert!(!third.contains("[事件 #1]"));
        assert!(third.contains("[事件 #2] 乙"));
        assert!(inbox.reply_lines().is_empty());
        assert_eq!(inbox.state.lock().unwrap().wakes.len(), 2);
    }
}
