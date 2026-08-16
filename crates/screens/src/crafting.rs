//! 工作台屏：3×3 合成容器（真相在服务端）。
//!
//! 与物品栏屏的差别只在**开屏路径**：工作台没有 open 动作——模型对着
//! 工作台方块使用（hand use_on），服务器发 OpenScreen，组合根随屏事实
//! 占域并投递格位清单。本工具只有 swap 与 close 两个动词，格号按工作台
//! 格空间解释（0 成品、1-9 摆料、10-36 主背包、37-45 快捷栏，无副手）。
//!
//! 配方知识在模型自己身上：机器不查配方表，摆什么出什么由服务器仲裁。

use std::sync::Arc;

use agent::{PortFuture, ToolCall, ToolDefinition, ToolResult};
use dispatch::{Domain, Occupancy, ToolClass};
use serde_json::{json, Value};
use world::SnapshotSource;

use crate::inventory::{kind_word, InventoryDoor, ScreenKind, ScreenState, DISCARD_SLOT};

const TOOL_NAME: &str = "crafting_table";

/// 开屏通知随附的用法说明。组合根在投递「工作台界面已打开」时引用，
/// 公开以便模型可见面导出评审。
pub const CRAFTING_USAGE: &str = "工作台用法：格号即协议号——0 成品（只出不进），\
1-9 摆料（3×3，行优先：1-3 上行、4-6 中行、7-9 下行），10-36 主背包，37-45 快捷栏；\
没有副手格，物品栏屏的格号在这里不适用。\
{action:\"swap\", a, b} 交换两格内容，一次一对；b 用 99 表示把 a 整格丢出去；\
两格恰有一格为空时可加 count 只挪这么多个过去（拆栈）——配方要同种材料占多格时\
就靠它，如把一组木板分放两格：{action:\"swap\", a:37, b:2, count:1} 再 {a:37, b:5, count:1}。\
取成品用 swap(0, 快捷栏或背包格)，会按配方消耗摆料。摆满配方后成品出现在 0，\
格位变化会另行通知。{action:\"close\"} 关闭工作台回到世界。\
开着工作台时无法移动或与世界交互。";

pub struct CraftingScreen {
    occupancy: Arc<Occupancy>,
    state: Arc<ScreenState>,
    door: Arc<dyn InventoryDoor>,
    snapshots: Arc<dyn SnapshotSource>,
}

impl CraftingScreen {
    pub fn new(
        occupancy: Arc<Occupancy>,
        state: Arc<ScreenState>,
        door: Arc<dyn InventoryDoor>,
        snapshots: Arc<dyn SnapshotSource>,
    ) -> Self {
        Self {
            occupancy,
            state,
            door,
            snapshots,
        }
    }

    async fn swap(
        &self,
        call_id: agent::ToolCallId,
        a: Option<&Value>,
        b: Option<&Value>,
        count: Option<&Value>,
    ) -> ToolResult {
        match self.state.current() {
            Some(ScreenKind::CraftingTable) => {}
            Some(other) => {
                return ToolResult::failure(
                    call_id,
                    format!("{}开着，不是工作台", kind_word(other)),
                );
            }
            None => {
                return ToolResult::failure(
                    call_id,
                    "工作台没有打开；先对着工作台方块使用（hand use_on），等界面打开的通知",
                );
            }
        }
        let slot = |value: Option<&Value>| value.and_then(Value::as_u64).map(|slot| slot as u16);
        let (Some(a), Some(b)) = (slot(a), slot(b)) else {
            return ToolResult::failure(call_id, "swap 需要整数参数 a 与 b；请改写调用");
        };
        let count = match count {
            None => None,
            Some(value) => match value.as_u64() {
                Some(count) => Some(count as u32),
                None => {
                    return ToolResult::failure(call_id, "count 必须是正整数；请改写调用");
                }
            },
        };
        if count.is_some() && b == DISCARD_SLOT {
            return ToolResult::failure(
                call_id,
                "count 只用于与空格之间挪个数，不能与 99 丢弃连用；请改写调用",
            );
        }
        let outcome = if b == DISCARD_SLOT {
            self.door.throw_slot(a).await
        } else {
            self.door.swap_slots(a, b, count).await
        };
        if let Err(reason) = outcome {
            return ToolResult::failure(call_id, reason);
        }
        // 点击本地预演即时生效，本 tick 的快照已按工作台格空间更新。
        let snapshot = self.snapshots.latest();
        let describe = |slot: u16| -> String {
            snapshot
                .self_state
                .inventory
                .slots
                .iter()
                .find(|entry| entry.slot == u32::from(slot))
                .map(|entry| format!("{} ×{}", entry.item_name, entry.count))
                .unwrap_or_else(|| "空".to_owned())
        };
        let summary = if b == DISCARD_SLOT {
            format!("已丢弃；格 {a} 现在：{}", describe(a))
        } else if let Some(count) = count {
            format!(
                "已挪 {count} 个；格 {a}：{}，格 {b}：{}",
                describe(a),
                describe(b)
            )
        } else {
            format!("已交换；格 {a}：{}，格 {b}：{}", describe(a), describe(b))
        };
        ToolResult::success_json(call_id, json!({ "done": summary }))
    }

    async fn close(&self, call_id: agent::ToolCallId) -> ToolResult {
        let outcome = self.door.close_container().await;
        // 无论门怎么说，本地屏状态与占域都收口：服务端容器不在了就该放行。
        self.state.close(ScreenKind::CraftingTable);
        self.occupancy.release(Domain::Screen);
        match outcome {
            Ok(()) => ToolResult::success_json(call_id, json!({ "state": "closed" })),
            Err(reason) => ToolResult::failure(call_id, reason),
        }
    }
}

impl dispatch::ToolProvider for CraftingScreen {
    fn tools(&self) -> Vec<(ToolDefinition, ToolClass)> {
        let mut definition = ToolDefinition::new(
            TOOL_NAME,
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["swap", "close"],
                        "description": "swap=交换两格（一次一对）；close=关闭工作台"
                    },
                    "a": { "type": "integer", "description": "swap 用：格号（0-45，工作台格空间）" },
                    "b": { "type": "integer", "description": "swap 用：格号（0-45），或 99=把 a 整格丢出去" },
                    "count": { "type": "integer", "description": "swap 可选：两格恰有一格为空时，从非空格挪这么多个到空格（拆栈，摆多格配方用）；不给则整组交换" }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        );
        definition.description = Some(
            "工作台（3×3 合成）。没有 open：对着工作台方块使用（hand use_on）后界面\
由服务器打开并通知你格位清单；开着期间用 swap 摆料/取成品，close 关闭。\
开着时无法移动或与世界交互。"
                .to_owned(),
        );
        vec![(
            definition,
            ToolClass::Body {
                domain: Domain::Screen,
            },
        )]
    }

    fn call<'a>(&'a self, call: ToolCall) -> PortFuture<'a, ToolResult> {
        Box::pin(async move {
            let call_id = call.id.clone();
            let Some(arguments) = call.arguments.as_object() else {
                return ToolResult::failure(call_id, "参数必须是 JSON 对象；请改写调用");
            };
            match arguments.get("action").and_then(Value::as_str) {
                Some("swap") => {
                    self.swap(
                        call_id,
                        arguments.get("a"),
                        arguments.get("b"),
                        arguments.get("count"),
                    )
                    .await
                }
                Some("close") => self.close(call_id).await,
                _ => ToolResult::failure(call_id, "action 必须是 swap/close 之一；请改写调用"),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use agent::{ContentPart, ToolResultStatus};

    use super::*;

    #[derive(Default)]
    struct RecordingDoor {
        calls: StdMutex<Vec<String>>,
        refuse_close: bool,
    }

    impl InventoryDoor for RecordingDoor {
        fn swap_slots<'a>(
            &'a self,
            a: u16,
            b: u16,
            count: Option<u32>,
        ) -> PortFuture<'a, Result<(), String>> {
            Box::pin(async move {
                let suffix = count.map(|n| format!(",{n}")).unwrap_or_default();
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("swap({a},{b}{suffix})"));
                Ok(())
            })
        }
        fn throw_slot<'a>(&'a self, slot: u16) -> PortFuture<'a, Result<(), String>> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(format!("throw({slot})"));
                Ok(())
            })
        }
        fn close_container<'a>(&'a self) -> PortFuture<'a, Result<(), String>> {
            Box::pin(async move {
                self.calls.lock().unwrap().push("close".to_owned());
                if self.refuse_close {
                    return Err("没有开着的容器界面".to_owned());
                }
                Ok(())
            })
        }
    }

    struct EmptySnapshots;

    impl SnapshotSource for EmptySnapshots {
        fn latest(&self) -> Arc<world::TickSnapshot> {
            Arc::new(world::TickSnapshot::empty(
                world::Epoch(1),
                1,
                world::ConnectionPhase::Ready,
            ))
        }
    }

    struct Fixture {
        screen: CraftingScreen,
        occupancy: Arc<Occupancy>,
        state: Arc<ScreenState>,
        door: Arc<RecordingDoor>,
    }

    fn fixture(refuse_close: bool) -> Fixture {
        let occupancy = Arc::new(Occupancy::new());
        let state = Arc::new(ScreenState::new());
        let door = Arc::new(RecordingDoor {
            refuse_close,
            ..RecordingDoor::default()
        });
        let screen = CraftingScreen::new(
            occupancy.clone(),
            state.clone(),
            door.clone(),
            Arc::new(EmptySnapshots),
        );
        Fixture {
            screen,
            occupancy,
            state,
            door,
        }
    }

    async fn invoke(fixture: &Fixture, arguments: serde_json::Value) -> ToolResult {
        dispatch::ToolProvider::call(&fixture.screen, ToolCall::new("call-1", TOOL_NAME, arguments))
            .await
    }

    fn text_of(result: &ToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                ContentPart::Json { value } => value.as_str(),
                _ => None,
            })
            .collect()
    }

    /// 模拟组合根对开屏事实的反应：登记状态并占域。
    fn server_opens(fixture: &Fixture) {
        fixture.state.server_open(ScreenKind::CraftingTable);
        fixture.occupancy.occupy(Domain::Screen);
    }

    #[tokio::test]
    async fn swap_requires_the_crafting_screen_to_be_open() {
        let fixture = fixture(false);
        let closed = invoke(&fixture, json!({"action": "swap", "a": 0, "b": 40})).await;
        assert_eq!(closed.status, ToolResultStatus::Error);
        assert!(text_of(&closed).contains("hand use_on"));
        assert!(fixture.door.calls.lock().unwrap().is_empty());

        server_opens(&fixture);
        let swapped = invoke(&fixture, json!({"action": "swap", "a": 0, "b": 40})).await;
        assert_eq!(swapped.status, ToolResultStatus::Success);
        let thrown = invoke(&fixture, json!({"action": "swap", "a": 5, "b": 99})).await;
        assert_eq!(thrown.status, ToolResultStatus::Success);
        assert_eq!(
            *fixture.door.calls.lock().unwrap(),
            vec!["swap(0,40)", "throw(5)"]
        );
    }

    #[tokio::test]
    async fn count_passes_through_to_the_door_but_not_with_discard() {
        let fixture = fixture(false);
        server_opens(&fixture);
        let moved = invoke(&fixture, json!({"action": "swap", "a": 37, "b": 2, "count": 1})).await;
        assert_eq!(moved.status, ToolResultStatus::Success);
        assert!(format!("{moved:?}").contains("已挪 1 个"), "{moved:?}");
        assert_eq!(*fixture.door.calls.lock().unwrap(), vec!["swap(37,2,1)"]);

        let bad = invoke(&fixture, json!({"action": "swap", "a": 37, "b": 99, "count": 2})).await;
        assert_eq!(bad.status, ToolResultStatus::Error);
        assert!(text_of(&bad).contains("不能与 99 丢弃连用"));
    }

    #[tokio::test]
    async fn swap_refuses_when_a_different_screen_is_open() {
        let fixture = fixture(false);
        fixture.state.server_open(ScreenKind::Inventory);
        let result = invoke(&fixture, json!({"action": "swap", "a": 1, "b": 2})).await;
        assert_eq!(result.status, ToolResultStatus::Error);
        assert!(text_of(&result).contains("物品栏"));
    }

    #[tokio::test]
    async fn close_reaches_the_door_and_releases_state_and_domain() {
        let fixture = fixture(false);
        server_opens(&fixture);
        let result = invoke(&fixture, json!({"action": "close"})).await;
        assert_eq!(result.status, ToolResultStatus::Success);
        assert_eq!(fixture.state.current(), None);
        assert!(!fixture.occupancy.is_occupied(Domain::Screen));
        assert_eq!(*fixture.door.calls.lock().unwrap(), vec!["close"]);
    }

    #[tokio::test]
    async fn close_clears_local_state_even_when_the_door_refuses() {
        // 门说没有容器 = 服务端早已关掉；本地状态必须跟上，不能把模型锁死。
        let fixture = fixture(true);
        server_opens(&fixture);
        let result = invoke(&fixture, json!({"action": "close"})).await;
        assert_eq!(result.status, ToolResultStatus::Error);
        assert_eq!(fixture.state.current(), None);
        assert!(!fixture.occupancy.is_occupied(Domain::Screen));
    }

    #[test]
    fn server_open_displaces_a_local_screen_and_reports_it() {
        let state = ScreenState::new();
        state.server_open(ScreenKind::CraftingTable);
        assert_eq!(state.current(), Some(ScreenKind::CraftingTable));
        // 已是同种类：不算顶替。
        assert_eq!(state.server_open(ScreenKind::CraftingTable), None);
        state.server_close(ScreenKind::CraftingTable);
        assert_eq!(state.current(), None);
        // 顶掉本地聊天屏要报出来（组合根据此提醒模型）。
        state.server_open(ScreenKind::Chat);
        assert_eq!(
            state.server_open(ScreenKind::CraftingTable),
            Some(ScreenKind::Chat)
        );
    }
}
