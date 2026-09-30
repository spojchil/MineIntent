//! 手：快捷栏、丢弃、主副手对调——原版里按一下就完成的几个键（1–9、Q、F）。
//!
//! 攻击、挖掘、使用都是鼠标键，在 `input` 工具里按住若干时长、作用于准星所指；
//! 这里只剩没有持续、不看准星的瞬时键。发出即完，结果由物品栏变化证实。

use std::sync::Arc;

use agent::{PortFuture, ToolCall, ToolDefinition, ToolResult};
use dispatch::{Domain, ToolClass, ToolProvider};
use serde_json::{json, Value};

/// 模块一写口的窄化。
pub trait HandDoor: Send + Sync {
    fn drop_item<'a>(&'a self, whole_stack: bool) -> PortFuture<'a, Result<(), String>>;
    fn swap_offhand<'a>(&'a self) -> PortFuture<'a, Result<(), String>>;
    fn select_slot<'a>(&'a self, slot: u8) -> PortFuture<'a, Result<(), String>>;
}

const TOOL_NAME: &str = "hand";

pub struct HandTools {
    door: Arc<dyn HandDoor>,
}

impl HandTools {
    pub fn new(door: Arc<dyn HandDoor>) -> Self {
        Self { door }
    }

    async fn dispatch(&self, call: ToolCall) -> ToolResult {
        let call_id = call.id.clone();
        let Some(arguments) = call.arguments.as_object() else {
            return ToolResult::failure(call_id, "参数必须是 JSON 对象；请改写调用");
        };
        let action = arguments.get("action").and_then(Value::as_str);
        let outcome = match action {
            Some("drop") => {
                let whole_stack = arguments
                    .get("stack")
                    .map(Value::as_bool)
                    .unwrap_or(Some(false));
                match whole_stack {
                    Some(whole_stack) => self.door.drop_item(whole_stack).await,
                    None => {
                        return ToolResult::failure(call_id, "drop 的 stack 需要是布尔；请改写调用")
                    }
                }
            }
            Some("swap_offhand") => self.door.swap_offhand().await,
            Some("select_slot") => {
                let Some(slot) = arguments
                    .get("slot")
                    .and_then(Value::as_u64)
                    .filter(|slot| *slot <= 8)
                else {
                    return ToolResult::failure(
                        call_id,
                        "select_slot 需要 0..=8 的整数参数 slot；请改写调用",
                    );
                };
                self.door.select_slot(slot as u8).await
            }
            _ => {
                return ToolResult::failure(
                    call_id,
                    "action 必须是 select_slot/drop/swap_offhand 之一；攻击、挖掘、使用请用 input 的鼠标键",
                )
            }
        };
        match outcome {
            Ok(()) => {
                ToolResult::success_json(call_id, json!({ "accepted": action.unwrap_or_default() }))
            }
            Err(reason) => ToolResult::failure(call_id, reason),
        }
    }
}

impl ToolProvider for HandTools {
    fn tools(&self) -> Vec<(ToolDefinition, ToolClass)> {
        let mut definition = ToolDefinition::new(
            TOOL_NAME,
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["select_slot", "drop", "swap_offhand"],
                        "description": "select_slot=选快捷栏格（数字键）；drop=丢手持物（Q）；swap_offhand=主副手对调（F）"
                    },
                    "stack": { "type": "boolean", "description": "drop 用：true 丢整组，默认丢一个" },
                    "slot": { "type": "integer", "minimum": 0, "maximum": 8, "description": "select_slot 用：快捷栏格号" }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        );
        definition.description = Some(
            "按一下就完成的手上动作：换快捷栏格、丢东西、主副手对调。\
攻击、挖掘、放置、吃东西都是鼠标键，用 input。"
                .to_owned(),
        );
        vec![(
            definition,
            ToolClass::Body {
                domain: Domain::Hand,
            },
        )]
    }

    fn call<'a>(&'a self, call: ToolCall) -> PortFuture<'a, ToolResult> {
        Box::pin(self.dispatch(call))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use agent::ToolResultStatus;

    use super::*;

    #[derive(Default)]
    struct RecordingDoor {
        calls: StdMutex<Vec<String>>,
    }

    impl RecordingDoor {
        fn log<'a>(&'a self, entry: String) -> PortFuture<'a, Result<(), String>> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(entry);
                Ok(())
            })
        }
    }

    impl HandDoor for RecordingDoor {
        fn drop_item<'a>(&'a self, whole_stack: bool) -> PortFuture<'a, Result<(), String>> {
            self.log(format!("drop({whole_stack})"))
        }
        fn swap_offhand<'a>(&'a self) -> PortFuture<'a, Result<(), String>> {
            self.log("swap_offhand".to_owned())
        }
        fn select_slot<'a>(&'a self, slot: u8) -> PortFuture<'a, Result<(), String>> {
            self.log(format!("select_slot({slot})"))
        }
    }

    fn call(arguments: Value) -> ToolCall {
        ToolCall::new("call-1", TOOL_NAME, arguments)
    }

    #[tokio::test]
    async fn instant_keys_reach_the_door() {
        let door = Arc::new(RecordingDoor::default());
        let tools = HandTools::new(door.clone());
        for arguments in [
            json!({"action": "select_slot", "slot": 3}),
            json!({"action": "drop"}),
            json!({"action": "drop", "stack": true}),
            json!({"action": "swap_offhand"}),
        ] {
            let result = tools.call(call(arguments)).await;
            assert_eq!(result.status, ToolResultStatus::Success);
        }
        assert_eq!(
            *door.calls.lock().unwrap(),
            vec![
                "select_slot(3)",
                "drop(false)",
                "drop(true)",
                "swap_offhand"
            ]
        );
    }

    #[tokio::test]
    async fn mouse_actions_moved_to_input_are_refused() {
        let door = Arc::new(RecordingDoor::default());
        let tools = HandTools::new(door.clone());
        for arguments in [
            json!({"action": "mine", "blocks": [[0, 64, 0]]}),
            json!({"action": "attack", "entity": "1:5"}),
            json!({"action": "select_slot", "slot": 9}),
            json!({"action": "drop", "stack": "yes"}),
        ] {
            let result = tools.call(call(arguments.clone())).await;
            assert_eq!(result.status, ToolResultStatus::Error, "{arguments}");
        }
        assert!(door.calls.lock().unwrap().is_empty());
    }
}
