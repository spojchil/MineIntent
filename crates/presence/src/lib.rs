//! 生死去留：一件工具，管「还在不在这个世界里」。
//!
//! 为什么自成一件工具而不是并进 motion/hand：它是**死亡时唯一还放行的**
//! 那一类（`ToolClass::Vital`）。身体类工具在死亡时全部被生命闸门拦下，
//! 若复活也归身体类，死了就没有出路了。
//!
//! 当前只有 `respawn` 一个动作。`disconnect` / `connect`（下线与上线）
//! 按同一件工具的同一个 `action` 参数扩展，但要先把接入模块从一次性改成
//! 可重启——`Module::start` 起线程、`stop` 合流即终，`EPOCH` 还是硬常量，
//! 而离线期间没有 tick，唤醒循环整个停摆。那是另一轮的工作，此处不预留
//! 半截实现：工具表上不出现做不到的动作。
//!
//! ## 死亡期间还剩什么（26.1.2 客户端字节码考证）
//!
//! - **看得见世界、听得见声音**：死亡屏不暂停游戏。`DeathScreen.isPauseScreen()`
//!   恒为 false；而多人下 `Minecraft` 里 `pause` 的第一道闸就是
//!   `hasSingleplayerServer()`，本就为 false，`soundManager.pauseAllExcept`
//!   永远走不到。所以 `scan` 这类 `Free` 工具照常放行。
//! - **开不了口**：`handleKeybinds()`（聊天键在内）只在 `screen == null`
//!   时调用，死亡屏是非空 screen。所以 `chat_box` 连同其余身体类一起拦。
//! - **收得到别人说话**：聊天 HUD 归 `Gui` 渲染，不经 screen。所以唤醒判据
//!   与帧在死亡期间照常走——死人是听得见的，只是答不上话。

use std::sync::Arc;

use agent::{PortFuture, ToolCall, ToolDefinition, ToolResult};
use dispatch::{ToolClass, ToolProvider};
use serde_json::{json, Value};

/// 接入模块写口的窄化。Err 是机器的如实拒绝（未连接、还活着等），原文转达。
pub trait PresenceDoor: Send + Sync {
    fn respawn<'a>(&'a self) -> PortFuture<'a, Result<(), String>>;
}

const TOOL_NAME: &str = "presence";

pub struct PresenceTools {
    door: Arc<dyn PresenceDoor>,
}

impl PresenceTools {
    pub fn new(door: Arc<dyn PresenceDoor>) -> Self {
        Self { door }
    }

    async fn dispatch_presence(&self, call: ToolCall) -> ToolResult {
        let call_id = call.id.clone();
        let Some(arguments) = call.arguments.as_object() else {
            return ToolResult::failure(call_id, "参数必须是 JSON 对象；请改写调用");
        };
        match arguments.get("action").and_then(Value::as_str) {
            Some("respawn") => match self.door.respawn().await {
                // 只回执「请求已送达」。真的活过来是服务端的事，会由体征与
                // 伤害窗自己说话——工具面替它宣布复活成功就是替被测系统作证。
                Ok(()) => {
                    ToolResult::success_json(call_id, json!({ "state": "respawn_requested" }))
                }
                Err(reason) => ToolResult::failure(call_id, reason),
            },
            Some(other) => ToolResult::failure(
                call_id,
                format!("presence 没有 {other} 这个动作；当前只有 respawn"),
            ),
            None => ToolResult::failure(call_id, "presence 需要字符串参数 action；请改写调用"),
        }
    }
}

/// 注册进编排的身份：Vital 类——不占域、不受界面压制，也不受生命闸门压制。
impl ToolProvider for PresenceTools {
    fn tools(&self) -> Vec<(ToolDefinition, ToolClass)> {
        let mut definition = ToolDefinition::new(
            TOOL_NAME,
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["respawn"],
                        "description": "respawn=从死亡中复活"
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        );
        definition.description = Some(
            "决定要不要从死亡中起来。死亡不会自己恢复——不调用它你就一直躺着。\
躺着的时候你仍然看得见世界、听得见声音、收得到别人说的话，但动不了也开不了口。"
                .to_owned(),
        );
        vec![(definition, ToolClass::Vital)]
    }

    fn call<'a>(&'a self, call: ToolCall) -> PortFuture<'a, ToolResult> {
        Box::pin(async move { self.dispatch_presence(call).await })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use agent::{ContentPart, ToolResultStatus};

    use super::*;

    struct FakeDoor {
        outcome: Mutex<Result<(), String>>,
        calls: Mutex<u32>,
    }

    impl Default for FakeDoor {
        fn default() -> Self {
            Self {
                outcome: Mutex::new(Ok(())),
                calls: Mutex::new(0),
            }
        }
    }

    impl FakeDoor {
        fn rejecting(reason: &str) -> Self {
            Self {
                outcome: Mutex::new(Err(reason.to_owned())),
                calls: Mutex::new(0),
            }
        }
    }

    impl PresenceDoor for FakeDoor {
        fn respawn<'a>(&'a self) -> PortFuture<'a, Result<(), String>> {
            Box::pin(async move {
                *self.calls.lock().expect("计数锁") += 1;
                self.outcome.lock().expect("结局锁").clone()
            })
        }
    }

    fn call(arguments: Value) -> ToolCall {
        ToolCall::new("call-1", TOOL_NAME, arguments)
    }

    fn text_of(result: &ToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn respawn_reaches_the_door() {
        let door = Arc::new(FakeDoor::default());
        let tools = PresenceTools::new(door.clone());
        let result = tools.call(call(json!({"action": "respawn"}))).await;
        assert_eq!(result.status, ToolResultStatus::Success);
        assert_eq!(*door.calls.lock().expect("计数锁"), 1);
    }

    /// 门的拒绝原文必须原样转达，不改写成好听的说法。
    #[tokio::test]
    async fn door_rejection_is_relayed_verbatim() {
        let door = Arc::new(FakeDoor::rejecting("你还活着，没有可复活的"));
        let tools = PresenceTools::new(door);
        let result = tools.call(call(json!({"action": "respawn"}))).await;
        assert_eq!(result.status, ToolResultStatus::Error);
        assert!(text_of(&result).contains("你还活着"), "{result:?}");
    }

    /// 回执只说「请求已送达」。复活成没成由体征说话，工具面不替它宣布。
    #[tokio::test]
    async fn success_claims_only_that_the_request_was_sent() {
        let door = Arc::new(FakeDoor::default());
        let tools = PresenceTools::new(door);
        let result = tools.call(call(json!({"action": "respawn"}))).await;
        let ContentPart::Json { value } = &result.content[0] else {
            panic!("期望 JSON 结果，得到 {:?}", result.content[0]);
        };
        assert_eq!(value["state"], "respawn_requested");
    }

    #[tokio::test]
    async fn unknown_action_is_rejected_by_name() {
        let door = Arc::new(FakeDoor::default());
        let tools = PresenceTools::new(door.clone());
        let result = tools.call(call(json!({"action": "disconnect"}))).await;
        assert_eq!(result.status, ToolResultStatus::Error);
        assert!(text_of(&result).contains("disconnect"), "{result:?}");
        assert_eq!(*door.calls.lock().expect("计数锁"), 0, "不该碰门");
    }

    #[tokio::test]
    async fn missing_action_is_rejected() {
        let tools = PresenceTools::new(Arc::new(FakeDoor::default()));
        let result = tools.call(call(json!({}))).await;
        assert_eq!(result.status, ToolResultStatus::Error);
    }

    /// 注册身份必须是 Vital：归进 Body 就会被生命闸门拦掉，死了没有出路。
    #[test]
    fn registers_as_vital_so_the_life_gate_lets_it_through() {
        let tools = PresenceTools::new(Arc::new(FakeDoor::default()));
        let registered = tools.tools();
        assert_eq!(registered.len(), 1);
        assert_eq!(registered[0].0.name.as_str(), TOOL_NAME);
        assert_eq!(registered[0].1, ToolClass::Vital);
    }
}
