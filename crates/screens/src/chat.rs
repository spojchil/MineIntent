//! 聊天框：界面域的第一个屏。
//!
//! 单工具 `chat_box`，动词四个：说（`/` 开头的行按原版语义作为命令路由）、
//! 历史、开、关。说与历史是完整闭环（结束必关屏）；只有显式"开"保持打开。
//! 发送无客户端限速——频率约束由服务端仲裁，与玩家同规。

use std::sync::Arc;

use agent::{ContentPart, PortFuture, ToolCall, ToolDefinition, ToolResult};
use dispatch::{Domain, Occupancy, ToolClass};
use serde_json::{json, Value};

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

const TOOL_NAME: &str = "chat_box";

const USAGE: &str = "聊天框用法：{action:\"say\", text} 按行发送，以 / 开头的行作为命令执行，\
每行至多 256 字符，聊天全服可见；{action:\"history\", count} 查看最近的聊天记录；\
{action:\"open\", describe?} 打开并保持聊天框；{action:\"close\"} 关闭。";

pub struct ChatBox {
    occupancy: Arc<Occupancy>,
    door: Arc<dyn ChatDoor>,
    history: Arc<dyn ChatHistory>,
}

impl ChatBox {
    pub fn new(
        occupancy: Arc<Occupancy>,
        door: Arc<dyn ChatDoor>,
        history: Arc<dyn ChatHistory>,
    ) -> Self {
        Self {
            occupancy,
            door,
            history,
        }
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definition = ToolDefinition::new(
            TOOL_NAME,
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["say", "history", "open", "close"],
                        "description": "say=说话或执行命令；history=翻聊天记录；open=打开并保持；close=关闭"
                    },
                    "text": {
                        "type": "string",
                        "description": "say 用：要说的话，按行发送；以 / 开头的行作为命令执行"
                    },
                    "count": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "history 用：要看最近多少条"
                    },
                    "describe": {
                        "type": "boolean",
                        "description": "open 用：是否附带用法介绍"
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        );
        definition.description = Some(
            "聊天框。说话、执行命令、翻聊天记录都在这里；打开期间无法移动或与世界交互。"
                .to_owned(),
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
                Some("open") => self.open(call_id, arguments.get("describe")),
                Some("close") => self.close(call_id),
                _ => ToolResult::failure(
                    call_id,
                    "action 必须是 say/history/open/close 之一；请改写调用",
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

        self.occupancy.occupy(Domain::Screen);
        for (index, line) in lines.iter().enumerate() {
            if let Err(reason) = self.door.send_chat(line).await {
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
        self.occupancy.release(Domain::Screen);
        ToolResult::success_json(call_id, json!({ "sent_lines": lines.len() }))
    }

    fn browse_history(&self, call_id: agent::ToolCallId, count: Option<&Value>) -> ToolResult {
        let Some(count) = count.and_then(Value::as_u64).filter(|count| *count > 0) else {
            return ToolResult::failure(call_id, "history 需要正整数参数 count；请改写调用");
        };
        self.occupancy.occupy(Domain::Screen);
        let lines = self.history.recent(count as usize);
        self.occupancy.release(Domain::Screen);
        ToolResult::success_json(call_id, json!({ "lines": lines }))
    }

    fn open(&self, call_id: agent::ToolCallId, describe: Option<&Value>) -> ToolResult {
        self.occupancy.occupy(Domain::Screen);
        let mut payload = json!({ "state": "open" });
        if describe.and_then(Value::as_bool) == Some(true) {
            payload["usage"] = Value::String(USAGE.to_owned());
        }
        ToolResult::success_json(call_id, payload)
    }

    fn close(&self, call_id: agent::ToolCallId) -> ToolResult {
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

    struct Fixture {
        chat: ChatBox,
        occupancy: Arc<Occupancy>,
        door: Arc<RecordingDoor>,
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
        let chat = ChatBox::new(occupancy.clone(), door.clone(), history);
        Fixture {
            chat,
            occupancy,
            door,
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
        assert_eq!(*fixture.door.occupied_at_send.lock().unwrap(), vec![true, true]);
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
        let opened = fixture
            .chat
            .call(call(json!({"action": "open", "describe": true})))
            .await;
        assert_eq!(json_payload(&opened)["state"], "open");
        assert!(json_payload(&opened)["usage"].as_str().unwrap().contains("聊天框用法"));
        assert!(screen_occupied(&fixture));

        fixture
            .chat
            .call(call(json!({"action": "say", "text": "嗯"})))
            .await;
        assert!(!screen_occupied(&fixture));
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
