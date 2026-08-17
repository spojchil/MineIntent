//! 聊天框：界面域的第一个屏。
//!
//! 单工具 `chat_box`，动词五个：说（`/` 开头的行按原版语义作为命令路由）、
//! 历史、开、取用法、关。说与历史是完整闭环（结束必关屏）；只有显式"开"保持打开。
//! 发送无客户端限速——频率约束由服务端仲裁，与玩家同规。

use std::sync::{Arc, Mutex as StdMutex};

use agent::{ContentPart, PortFuture, ToolCall, ToolDefinition, ToolResult};
use dispatch::{Domain, Occupancy, ToolClass};
use serde_json::{json, Value};
use world::SnapshotSource;

use crate::inventory::{ScreenKind, ScreenState};
use crate::segment::{plan_lines, MAX_CHAT_UTF16};

/// 模块一写表面的窄化：一行 = 一次原版输入循环。
/// 以 `/` 开头的行由实现方路由为命令包（ChatScreen 同语义），其余走聊天包。
pub trait ChatDoor: Send + Sync {
    fn send_chat<'a>(&'a self, line: &'a str) -> PortFuture<'a, Result<(), String>>;
}

/// 聊天窗读取的窄化：最近 `count` 条，旧在前新在后，不足则全部。
pub trait ChatHistory: Send + Sync {
    fn recent(&self, count: usize) -> Vec<String>;
}

/// 聊天已读水位。未读数 = 聊天窗里 (epoch, tick) 晚于水位的条数（读数在渲染层）；
/// 本类型只记"上次看到哪"。松语义（维护者裁定）：history 一看整体清零，
/// 不追每条是否真的读过；重连换 epoch 后整窗算新。
#[derive(Default)]
pub struct ChatReadMark {
    position: StdMutex<(u64, u64)>,
}

impl ChatReadMark {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mark_read(&self, epoch: u64, tick: u64) {
        *self.position.lock().expect("已读水位锁中毒") = (epoch, tick);
    }

    /// (epoch, tick)。初始 (0, 0)：还什么都没看过。
    pub fn position(&self) -> (u64, u64) {
        *self.position.lock().expect("已读水位锁中毒")
    }
}

const TOOL_NAME: &str = "chat_box";

const USAGE: &str = "聊天框用法：{action:\"say\", text} 按行发送，以 / 开头的行作为命令执行，\
每行至多 256 字符，聊天全服可见；{action:\"history\", count} 查看最近的聊天记录；\
{action:\"open\"} 打开并保持聊天框；{action:\"close\"} 关闭。";

pub struct ChatBox {
    occupancy: Arc<Occupancy>,
    state: Arc<ScreenState>,
    door: Arc<dyn ChatDoor>,
    history: Arc<dyn ChatHistory>,
    read_mark: Arc<ChatReadMark>,
    snapshots: Arc<dyn SnapshotSource>,
}

impl ChatBox {
    pub fn new(
        occupancy: Arc<Occupancy>,
        state: Arc<ScreenState>,
        door: Arc<dyn ChatDoor>,
        history: Arc<dyn ChatHistory>,
        read_mark: Arc<ChatReadMark>,
        snapshots: Arc<dyn SnapshotSource>,
    ) -> Self {
        Self {
            occupancy,
            state,
            door,
            history,
            read_mark,
            snapshots,
        }
    }

    /// 跨屏互斥：别的屏开着时聊天动词如实拒绝（原版一次只有一个屏）。
    fn claim_chat_screen(&self) -> Result<(), String> {
        self.state.open(ScreenKind::Chat).map_err(|existing| {
            format!(
                "{}开着，先关闭它再用聊天框",
                super::inventory::kind_word(existing)
            )
        })
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definition = ToolDefinition::new(
            TOOL_NAME,
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["say", "history", "open", "describe", "close"],
                        "description": "say=说话或执行命令；history=翻聊天记录；open=打开并保持；describe=取聊天框的完整用法（不随开屏自动给，要看自己取）；close=关闭"
                    },
                    "text": {
                        "type": "string",
                        "description": "say 用：要说的话，按行发送；以 / 开头的行作为命令执行"
                    },
                    "count": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "history 用：要看最近多少条"
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        );
        definition.description = Some(
            "聊天框。说话、执行命令、翻聊天记录都在这里；打开期间无法移动或与世界交互。".to_owned(),
        );
        vec![definition]
    }

    pub fn call<'a>(&'a self, call: ToolCall) -> PortFuture<'a, ToolResult> {
        Box::pin(async move {
            let call_id = call.id.clone();
            let Some(arguments) = call.arguments.as_object() else {
                return ToolResult::failure(call_id, "参数必须是 JSON 对象；请改写调用");
            };
            match arguments.get("action").and_then(Value::as_str) {
                Some("say") => self.say(call_id, arguments.get("text")).await,
                Some("history") => self.browse_history(call_id, arguments.get("count")),
                Some("open") => self.open(call_id),
                Some("describe") => self.describe(call_id),
                Some("close") => self.close(call_id),
                _ => ToolResult::failure(
                    call_id,
                    "action 必须是 say/history/open/describe/close 之一；请改写调用",
                ),
            }
        })
    }

    async fn say(&self, call_id: agent::ToolCallId, text: Option<&Value>) -> ToolResult {
        let Some(text) = text.and_then(Value::as_str) else {
            return ToolResult::failure(call_id, "say 需要字符串参数 text；请改写调用");
        };
        let lines = match plan_lines(text) {
            Ok(lines) => lines,
            Err(reason) => return ToolResult::failure(call_id, reason),
        };
        if let Err(reason) = self.claim_chat_screen() {
            return ToolResult::failure(call_id, reason);
        }

        self.occupancy.occupy(Domain::Screen);
        for (index, line) in lines.iter().enumerate() {
            if let Err(reason) = self.door.send_chat(line).await {
                self.state.close(ScreenKind::Chat);
                self.occupancy.release(Domain::Screen);
                return ToolResult {
                    call_id,
                    status: agent::ToolResultStatus::Error,
                    content: vec![ContentPart::json(json!({
                        "sent_lines": index,
                        "summary": format!("发送第 {} 行时失败：{reason}", index + 1),
                    }))],
                    metadata: agent::JsonObject::new(),
                };
            }
        }
        self.state.close(ScreenKind::Chat);
        self.occupancy.release(Domain::Screen);
        ToolResult::success_json(call_id, json!({ "sent_lines": lines.len() }))
    }

    fn browse_history(&self, call_id: agent::ToolCallId, count: Option<&Value>) -> ToolResult {
        let Some(count) = count.and_then(Value::as_u64).filter(|count| *count > 0) else {
            return ToolResult::failure(call_id, "history 需要正整数参数 count；请改写调用");
        };
        if let Err(reason) = self.claim_chat_screen() {
            return ToolResult::failure(call_id, reason);
        }
        self.occupancy.occupy(Domain::Screen);
        let lines = self.history.recent(count as usize);
        // 看了就清零：把已读水位推到当前时刻，不追每条是否真的读过。
        let now = self.snapshots.latest();
        self.read_mark.mark_read(now.epoch.0, now.tick);
        self.state.close(ScreenKind::Chat);
        self.occupancy.release(Domain::Screen);
        ToolResult::success_json(call_id, json!({ "lines": lines }))
    }

    fn open(&self, call_id: agent::ToolCallId) -> ToolResult {
        if let Err(reason) = self.claim_chat_screen() {
            return ToolResult::failure(call_id, reason);
        }
        self.occupancy.occupy(Domain::Screen);
        ToolResult::success_json(call_id, json!({ "state": "open" }))
    }

    /// 用法全文按需取。
    ///
    /// 2026-08-17 由 `open{describe}` 参数改成独立动作，与 inventory/container
    /// 收口成同一套：查用法是界面的一个动作，不是开屏的一个选项。开着的时候
    /// 想再看一眼用法，不必为此重开一次屏。
    fn describe(&self, call_id: agent::ToolCallId) -> ToolResult {
        ToolResult::success(call_id, vec![agent::ContentPart::text(USAGE.to_owned())])
    }

    fn close(&self, call_id: agent::ToolCallId) -> ToolResult {
        self.state.close(ScreenKind::Chat);
        self.occupancy.release(Domain::Screen);
        ToolResult::success_json(call_id, json!({ "state": "closed" }))
    }
}

// MAX_CHAT_UTF16 出现在工具描述与用法文本里；编译期钉住两处一致。
const _: () = assert!(MAX_CHAT_UTF16 == 256);

/// 注册进编排的身份：身体类、界面域。
impl dispatch::ToolProvider for ChatBox {
    fn tools(&self) -> Vec<(ToolDefinition, ToolClass)> {
        self.definitions()
            .into_iter()
            .map(|definition| {
                (
                    definition,
                    ToolClass::Body {
                        domain: Domain::Screen,
                    },
                )
            })
            .collect()
    }

    fn call<'a>(&'a self, call: ToolCall) -> PortFuture<'a, ToolResult> {
        ChatBox::call(self, call)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use serde_json::json;

    use super::*;

    struct RecordingDoor {
        sent: StdMutex<Vec<String>>,
        occupied_at_send: StdMutex<Vec<bool>>,
        occupancy: Arc<Occupancy>,
        fail_on_line: Option<usize>,
    }

    impl ChatDoor for RecordingDoor {
        fn send_chat<'a>(&'a self, line: &'a str) -> PortFuture<'a, Result<(), String>> {
            Box::pin(async move {
                let index = self.sent.lock().unwrap().len();
                if self.fail_on_line == Some(index) {
                    return Err("连接已断开".to_owned());
                }
                self.occupied_at_send
                    .lock()
                    .unwrap()
                    .push(self.occupancy.is_occupied(Domain::Screen));
                self.sent.lock().unwrap().push(line.to_owned());
                Ok(())
            })
        }
    }

    struct FixedHistory(Vec<String>);

    impl ChatHistory for FixedHistory {
        fn recent(&self, count: usize) -> Vec<String> {
            let skip = self.0.len().saturating_sub(count);
            self.0[skip..].to_vec()
        }
    }

    /// 固定在 (epoch 3, tick 400) 的快照源。
    struct FixedSnapshots;

    impl SnapshotSource for FixedSnapshots {
        fn latest(&self) -> Arc<world::TickSnapshot> {
            Arc::new(world::TickSnapshot::empty(
                world::Epoch(3),
                400,
                world::ConnectionPhase::Ready,
            ))
        }
    }

    struct Fixture {
        chat: ChatBox,
        occupancy: Arc<Occupancy>,
        door: Arc<RecordingDoor>,
        read_mark: Arc<ChatReadMark>,
    }

    fn fixture(fail_on_line: Option<usize>) -> Fixture {
        let occupancy = Arc::new(Occupancy::new());
        let door = Arc::new(RecordingDoor {
            sent: StdMutex::new(Vec::new()),
            occupied_at_send: StdMutex::new(Vec::new()),
            occupancy: occupancy.clone(),
            fail_on_line,
        });
        let history = Arc::new(FixedHistory(vec![
            "甲：你好".to_owned(),
            "乙：在吗".to_owned(),
            "丙：走了".to_owned(),
        ]));
        let read_mark = Arc::new(ChatReadMark::new());
        let chat = ChatBox::new(
            occupancy.clone(),
            Arc::new(ScreenState::new()),
            door.clone(),
            history,
            read_mark.clone(),
            Arc::new(FixedSnapshots),
        );
        Fixture {
            chat,
            occupancy,
            door,
            read_mark,
        }
    }

    fn call(arguments: Value) -> ToolCall {
        ToolCall::new("call-1", TOOL_NAME, arguments)
    }

    fn json_payload(result: &ToolResult) -> &Value {
        match &result.content[0] {
            ContentPart::Json { value } => value,
            other => panic!("期望 JSON 结果，得到 {other:?}"),
        }
    }

    fn screen_occupied(fixture: &Fixture) -> bool {
        fixture.occupancy.is_occupied(Domain::Screen)
    }

    #[tokio::test]
    async fn say_sends_lines_in_order_holds_screen_and_releases_after() {
        let fixture = fixture(None);
        let result = fixture
            .chat
            .call(call(json!({"action": "say", "text": "到了\n/help"})))
            .await;

        assert_eq!(result.status, agent::ToolResultStatus::Success);
        assert_eq!(json_payload(&result)["sent_lines"], 2);
        assert_eq!(*fixture.door.sent.lock().unwrap(), vec!["到了", "/help"]);
        // 发送期间界面域占用，结束后必释放。
        assert_eq!(
            *fixture.door.occupied_at_send.lock().unwrap(),
            vec![true, true]
        );
        assert!(!screen_occupied(&fixture));
    }

    #[tokio::test]
    async fn say_failure_reports_progress_and_releases_screen() {
        let fixture = fixture(Some(1));
        let result = fixture
            .chat
            .call(call(json!({"action": "say", "text": "第一句\n第二句"})))
            .await;

        assert_eq!(result.status, agent::ToolResultStatus::Error);
        assert_eq!(json_payload(&result)["sent_lines"], 1);
        assert!(!screen_occupied(&fixture));
    }

    #[tokio::test]
    async fn say_rejects_blank_text_without_touching_occupancy() {
        let fixture = fixture(None);
        let result = fixture
            .chat
            .call(call(json!({"action": "say", "text": "  \n "})))
            .await;

        assert_eq!(result.status, agent::ToolResultStatus::Error);
        assert!(fixture.door.sent.lock().unwrap().is_empty());
        assert!(!screen_occupied(&fixture));
    }

    #[tokio::test]
    async fn open_keeps_the_screen_and_say_still_releases_at_the_end() {
        let fixture = fixture(None);
        let opened = fixture.chat.call(call(json!({"action": "open"}))).await;
        assert_eq!(json_payload(&opened)["state"], "open");
        // 开屏回执里不带用法：它是静态文本，要看自己调 describe。
        assert!(json_payload(&opened).get("usage").is_none());
        assert!(screen_occupied(&fixture));

        fixture
            .chat
            .call(call(json!({"action": "say", "text": "嗯"})))
            .await;
        assert!(!screen_occupied(&fixture));
    }

    /// 用法是独立动作，不是开屏的选项——开着的时候想再看一眼，不必重开屏。
    #[tokio::test]
    async fn describe_is_an_action_and_works_while_already_open() {
        let fixture = fixture(None);
        fixture.chat.call(call(json!({"action": "open"}))).await;
        let described = fixture.chat.call(call(json!({"action": "describe"}))).await;
        assert_eq!(described.status, agent::ToolResultStatus::Success);
        let text: String = described
            .content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(text.contains("聊天框用法"), "{text}");
        // 取用法不该动屏：仍然开着。
        assert!(screen_occupied(&fixture));
    }

    #[tokio::test]
    async fn history_returns_recent_lines_and_ends_released() {
        let fixture = fixture(None);
        fixture.chat.call(call(json!({"action": "open"}))).await;
        let result = fixture
            .chat
            .call(call(json!({"action": "history", "count": 2})))
            .await;

        assert_eq!(
            json_payload(&result)["lines"],
            json!(["乙：在吗", "丙：走了"])
        );
        assert!(!screen_occupied(&fixture));
    }

    #[tokio::test]
    async fn history_advances_the_read_mark_to_now_and_other_verbs_do_not() {
        let fixture = fixture(None);
        assert_eq!(fixture.read_mark.position(), (0, 0));

        fixture
            .chat
            .call(call(json!({"action": "say", "text": "先说话"})))
            .await;
        fixture.chat.call(call(json!({"action": "open"}))).await;
        fixture.chat.call(call(json!({"action": "close"}))).await;
        assert_eq!(fixture.read_mark.position(), (0, 0));

        fixture
            .chat
            .call(call(json!({"action": "history", "count": 1})))
            .await;
        assert_eq!(fixture.read_mark.position(), (3, 400));
    }

    #[tokio::test]
    async fn close_is_idempotent() {
        let fixture = fixture(None);
        let closed = fixture.chat.call(call(json!({"action": "close"}))).await;
        assert_eq!(json_payload(&closed)["state"], "closed");

        fixture.chat.call(call(json!({"action": "open"}))).await;
        fixture.chat.call(call(json!({"action": "close"}))).await;
        assert!(!screen_occupied(&fixture));
    }

    #[tokio::test]
    async fn unknown_action_and_bad_arguments_come_back_as_rewrite_hints() {
        let fixture = fixture(None);
        let unknown = fixture.chat.call(call(json!({"action": "dance"}))).await;
        assert_eq!(unknown.status, agent::ToolResultStatus::Error);

        let missing_text = fixture.chat.call(call(json!({"action": "say"}))).await;
        assert_eq!(missing_text.status, agent::ToolResultStatus::Error);

        let bad_count = fixture
            .chat
            .call(call(json!({"action": "history", "count": 0})))
            .await;
        assert_eq!(bad_count.status, agent::ToolResultStatus::Error);
    }

    #[test]
    fn definition_exposes_one_ascii_named_tool() {
        let fixture = fixture(None);
        let definitions = fixture.chat.definitions();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].name.as_str(), "chat_box");
        assert!(definitions[0]
            .name
            .as_str()
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_'));
    }
}
