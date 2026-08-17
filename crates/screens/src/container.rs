//! 容器屏：所有服务端容器（工作台、箱子、熔炉……）共用的一件工具。
//!
//! 与物品栏屏的本质差异在**开/关路径**：容器真相在服务端——没有 open
//! 动作，模型对容器方块使用（hand use_on），服务器发 OpenScreen，组合根
//! 随屏事实占域并投递格位清单与用法；关闭要通知服务器（ContainerClose），
//! 服务器也可以强关。格子层面各容器只是"几个格子的差别"：move（移动/
//! 合堆/对调）与丢弃动词一律通用（机器按活动菜单现状仲裁），每种容器的
//! 差异降为数据（清单段表在 render，用法补充在 [`container_usage`]）。
//!
//! 配方知识在模型自己身上：机器不查配方表，摆什么出什么由服务器仲裁。

use std::sync::Arc;

use agent::{PortFuture, ToolCall, ToolDefinition, ToolResult};
use dispatch::{Domain, Occupancy, ToolClass};
use serde_json::{json, Value};
use world::SnapshotSource;

use crate::inventory::{kind_word, InventoryDoor, ScreenKind, ScreenState, DISCARD_SLOT};

const TOOL_NAME: &str = "container";

/// 所有容器共用的动词说明。开屏通知 = 种类名 + 格位清单 + 本文 +
/// 种类补充（[`container_usage`] 汇总）。公开以便模型可见面导出评审。
pub const CONTAINER_USAGE: &str =
    "容器界面用法：格号即协议号，属于当前界面（物品栏屏的格号在这里不适用）。\
{action:\"move\", from, to} 把 from 格的东西弄到 to 格，语义随 to 现状：to 为空=移过去\
（可加 count 只挪几个，拆栈）；to 是同种物品=倒入合堆（可加 count 只倒几个，装不下的留在原格）；\
to 是不同物品=整组对调（count 不适用）；to 用 99=把 from 整格丢出去。\
{action:\"close\"} 关闭容器回到世界。开着容器时无法移动或与世界交互。";

/// 种类专属的用法补充（数据，不是代码）：只写通用动词说明覆盖不到的语义。
fn kind_supplement(kind: &str) -> Option<&'static str> {
    match kind {
        "crafting" => Some(
            "这是工作台（3×3 合成）：0 成品（只出不进、只能整组取走），1-9 摆料\
（行优先：1-3 上行、4-6 中行、7-9 下行），10-36 主背包，37-45 快捷栏，无副手格。\
摆满配方后成品出现在 0，取成品用 move(0, 快捷栏或背包格)，会按配方消耗摆料；\
配方要同种材料占多格时用 count 拆栈，如 {action:\"move\", from:37, to:2, count:1}。",
        ),
        "furnace" => Some(
            "这是熔炉：0 原料，1 燃料（只收燃料，如煤炭、木制品；放别的会被服务器退回），\
2 成品（只出不进、只能整组取走），3-29 主背包，30-38 快捷栏，无副手格。\
原料与燃料就位后自动开始烧，每件约 10 秒；不必守着界面——close 之后熔炉照样烧，\
估摸烧完再回来开取成品。",
        ),
        "blast_furnace" => Some(
            "这是高炉（只炼矿石与金属类，速度是熔炉两倍、每件约 5 秒）：\
0 原料，1 燃料（只收燃料），2 成品（只出不进、只能整组取走），\
3-29 主背包，30-38 快捷栏。close 之后照样烧，烧完再来取。",
        ),
        "smoker" => Some(
            "这是烟熏炉（只烤食物，速度是熔炉两倍、每件约 5 秒）：\
0 原料，1 燃料（只收燃料），2 成品（只出不进、只能整组取走），\
3-29 主背包，30-38 快捷栏。close 之后照样烧，烧完再来取。",
        ),
        _ => None,
    }
}

/// 某种容器的完整用法文本：通用动词说明 + 种类补充（若有）。
pub fn container_usage(kind: &str) -> String {
    match kind_supplement(kind) {
        Some(supplement) => format!("{CONTAINER_USAGE}\n{supplement}"),
        None => CONTAINER_USAGE.to_owned(),
    }
}

pub struct ContainerScreen {
    occupancy: Arc<Occupancy>,
    state: Arc<ScreenState>,
    door: Arc<dyn InventoryDoor>,
    snapshots: Arc<dyn SnapshotSource>,
}

impl ContainerScreen {
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

    async fn move_items(
        &self,
        call_id: agent::ToolCallId,
        from: Option<&Value>,
        to: Option<&Value>,
        count: Option<&Value>,
    ) -> ToolResult {
        match self.state.current() {
            Some(ScreenKind::Container) => {}
            Some(other) => {
                return ToolResult::failure(
                    call_id,
                    format!("{}开着，不是容器界面", kind_word(other)),
                );
            }
            None => {
                return ToolResult::failure(
                    call_id,
                    "没有开着的容器界面；先对容器方块使用（hand use_on），等界面打开的通知",
                );
            }
        }
        let slot = |value: Option<&Value>| value.and_then(Value::as_u64).map(|slot| slot as u16);
        let (Some(from), Some(to)) = (slot(from), slot(to)) else {
            return ToolResult::failure(call_id, "move 需要整数参数 from 与 to；请改写调用");
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
        if count.is_some() && to == DISCARD_SLOT {
            return ToolResult::failure(
                call_id,
                "count 不能与 99 丢弃连用（丢弃是整格）；请改写调用",
            );
        }
        let outcome = if to == DISCARD_SLOT {
            self.door.throw_slot(from).await
        } else {
            self.door.move_slots(from, to, count).await
        };
        if let Err(reason) = outcome {
            return ToolResult::failure(call_id, reason);
        }
        // 点击本地预演即时生效，本 tick 的快照已按容器格空间更新。
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
        let summary = if to == DISCARD_SLOT {
            format!("已丢弃；格 {from} 现在：{}", describe(from))
        } else {
            format!(
                "已完成；格 {from}：{}，格 {to}：{}",
                describe(from),
                describe(to)
            )
        };
        ToolResult::success_json(call_id, json!({ "done": summary }))
    }

    async fn close(&self, call_id: agent::ToolCallId) -> ToolResult {
        let outcome = self.door.close_container().await;
        // 无论门怎么说，本地屏状态与占域都收口：服务端容器不在了就该放行。
        self.state.close(ScreenKind::Container);
        self.occupancy.release(Domain::Screen);
        match outcome {
            Ok(()) => ToolResult::success_json(call_id, json!({ "state": "closed" })),
            Err(reason) => ToolResult::failure(call_id, reason),
        }
    }
}

impl dispatch::ToolProvider for ContainerScreen {
    fn tools(&self) -> Vec<(ToolDefinition, ToolClass)> {
        let mut definition = ToolDefinition::new(
            TOOL_NAME,
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["move", "close"],
                        "description": "move=把 from 格的东西弄到 to 格（移动/合堆/对调）；close=关闭容器"
                    },
                    "from": { "type": "integer", "description": "move 用：来源格号（当前容器的格空间）" },
                    "to": { "type": "integer", "description": "move 用：目标格号——空=移过去、同种物品=倒入合堆、不同物品=整组对调；99=把 from 整格丢出去" },
                    "count": { "type": "integer", "description": "move 可选：只挪/只倒这么多个（to 为空或同种物品时）；不给则整组" }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        );
        definition.description = Some(
            "当前开着的容器界面（工作台、箱子、熔炉等共用）。没有 open：对容器方块\
使用（hand use_on）后界面由服务器打开，你会收到格位清单与用法；开着期间用 move \
搬动/合堆/摆料/取物，close 关闭。开着时无法移动或与世界交互。"
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
                Some("move") => {
                    self.move_items(
                        call_id,
                        arguments.get("from"),
                        arguments.get("to"),
                        arguments.get("count"),
                    )
                    .await
                }
                Some("close") => self.close(call_id).await,
                _ => ToolResult::failure(call_id, "action 必须是 move/close 之一；请改写调用"),
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
        fn move_slots<'a>(
            &'a self,
            from: u16,
            to: u16,
            count: Option<u32>,
        ) -> PortFuture<'a, Result<(), String>> {
            Box::pin(async move {
                let suffix = count.map(|n| format!(",{n}")).unwrap_or_default();
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("move({from},{to}{suffix})"));
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
        screen: ContainerScreen,
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
        let screen = ContainerScreen::new(
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
        dispatch::ToolProvider::call(
            &fixture.screen,
            ToolCall::new("call-1", TOOL_NAME, arguments),
        )
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
        fixture.state.server_open(ScreenKind::Container);
        fixture.occupancy.occupy(Domain::Screen);
    }

    #[tokio::test]
    async fn move_requires_a_container_to_be_open() {
        let fixture = fixture(false);
        let closed = invoke(&fixture, json!({"action": "move", "from": 0, "to": 40})).await;
        assert_eq!(closed.status, ToolResultStatus::Error);
        assert!(text_of(&closed).contains("hand use_on"));
        assert!(fixture.door.calls.lock().unwrap().is_empty());

        server_opens(&fixture);
        let moved = invoke(&fixture, json!({"action": "move", "from": 0, "to": 40})).await;
        assert_eq!(moved.status, ToolResultStatus::Success);
        let thrown = invoke(&fixture, json!({"action": "move", "from": 5, "to": 99})).await;
        assert_eq!(thrown.status, ToolResultStatus::Success);
        assert_eq!(
            *fixture.door.calls.lock().unwrap(),
            vec!["move(0,40)", "throw(5)"]
        );
    }

    #[tokio::test]
    async fn count_passes_through_to_the_door_but_not_with_discard() {
        let fixture = fixture(false);
        server_opens(&fixture);
        let moved = invoke(
            &fixture,
            json!({"action": "move", "from": 37, "to": 2, "count": 1}),
        )
        .await;
        assert_eq!(moved.status, ToolResultStatus::Success);
        assert_eq!(*fixture.door.calls.lock().unwrap(), vec!["move(37,2,1)"]);

        let bad = invoke(
            &fixture,
            json!({"action": "move", "from": 37, "to": 99, "count": 2}),
        )
        .await;
        assert_eq!(bad.status, ToolResultStatus::Error);
        assert!(text_of(&bad).contains("不能与 99 丢弃连用"));
    }

    #[tokio::test]
    async fn move_refuses_when_a_different_screen_is_open() {
        let fixture = fixture(false);
        fixture.state.server_open(ScreenKind::Inventory);
        let result = invoke(&fixture, json!({"action": "move", "from": 1, "to": 2})).await;
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
        state.server_open(ScreenKind::Container);
        assert_eq!(state.current(), Some(ScreenKind::Container));
        // 已是同种类：不算顶替。
        assert_eq!(state.server_open(ScreenKind::Container), None);
        state.server_close(ScreenKind::Container);
        assert_eq!(state.current(), None);
        // 顶掉本地聊天屏要报出来（组合根据此提醒模型）。
        state.server_open(ScreenKind::Chat);
        assert_eq!(
            state.server_open(ScreenKind::Container),
            Some(ScreenKind::Chat)
        );
    }

    #[test]
    fn crafting_usage_carries_the_semantic_supplement_and_unknown_kinds_stay_generic() {
        let crafting = container_usage("crafting");
        assert!(crafting.contains("容器界面用法"));
        assert!(crafting.contains("成品"));
        let chest = container_usage("generic_9x3");
        assert!(chest.contains("容器界面用法"));
        assert!(!chest.contains("成品"));
    }

    #[test]
    fn furnace_family_supplements_teach_slots_and_fire_and_forget() {
        // 三种炉子各有补充：格位语义 + 「close 之后照样烧」（不守界面）。
        for kind in ["furnace", "blast_furnace", "smoker"] {
            let usage = container_usage(kind);
            assert!(usage.contains("容器界面用法"), "{kind}");
            assert!(usage.contains("燃料"), "{kind}");
            assert!(usage.contains("只出不进"), "{kind}");
            assert!(usage.contains("照样烧"), "{kind}");
        }
        // 专属差异各自点名。
        assert!(container_usage("blast_furnace").contains("矿石"));
        assert!(container_usage("smoker").contains("食物"));
    }
}
