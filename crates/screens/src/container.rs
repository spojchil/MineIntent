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

use agent::{ContentPart, PortFuture, ToolCall, ToolDefinition, ToolResult};
use dispatch::{Domain, Occupancy, ToolClass};
use serde_json::{json, Value};
use world::SnapshotSource;

use crate::inventory::{kind_word, InventoryDoor, ScreenKind, ScreenState, DISCARD};

const TOOL_NAME: &str = "container";

/// 所有容器共用的动词说明。开屏通知 = 种类名 + 格位清单 + 本文 +
/// 种类补充（[`container_usage`] 汇总）。公开以便模型可见面导出评审。
pub const CONTAINER_USAGE: &str =
    "容器界面用法：格位用地址，不是数字——具体认哪些写法，随开屏清单一起给。\
**同一个位置在任何界面下都是同一个地址**——开着容器不改变 pack 与 hotbar 的写法。\
{action:\"move\", from, to} 把 from 格的东西弄到 to 格，语义随 to 现状：to 为空=移过去\
（可加 count 只挪几个，拆栈）；to 是同种物品=倒入合堆（可加 count 只倒几个，装不下的留在原格）；\
to 是不同物品=整组对调（count 不适用）；to 写 drop=把 from 整格丢出去。\
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
        let slot = |value: Option<&Value>| value.and_then(Value::as_str).map(str::to_owned);
        let (Some(from), Some(to)) = (slot(from), slot(to)) else {
            return ToolResult::failure(
                call_id,
                "move 需要 from 与 to 两个格位地址（如 \"pack 3\"、\"hotbar 0\"）；请改写调用",
            );
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
        if count.is_some() && to == DISCARD {
            return ToolResult::failure(
                call_id,
                "count 不能与 drop 连用（丢弃是整格）；请改写调用",
            );
        }
        let outcome = if to == DISCARD {
            self.door.throw_slot(from.clone()).await
        } else {
            self.door.move_slots(from.clone(), to.clone(), count).await
        };
        if let Err(reason) = outcome {
            return ToolResult::failure(call_id, reason);
        }
        // 回执只说动作结论，不报格位现状。两个理由，后一个才是根本的：
        //
        // 1. 回执发出时真相还没到。机器级探针三跑一致（craft_timing_probe）：
        //    写口回执当下成品格必然还是旧值，服务端确认要 **2~3 游戏刻**才回来，
        //    而且会浮动。这不是本层的 bug——点击包要往返一趟，服务端每 tick 报的
        //    是它已经处理完的。实盘后果：目标格显示为空，模型合理地判断没生效，
        //    于是每一步都做了两遍（一次 GUI 实验里 13 次 move 近一半是这么来的）。
        //    预判会说谎、空等没有上限，所以改为**把延迟告诉模型**
        //    （见本工具 description）。
        // 2. 就算读对了也没意义——格位现状是**事实**，归快照与格位变化窗；
        //    工具只表达意图、回执只说结论（与 motion/hand 的 `accepted` 同款）。
        //    真正要紧的那件事——摆料之后成品格冒出什么——本来就走信箱：
        //    自己点的两格是 Commanded 回声（不吵），成品格是 ServerObserved
        //    （预期之外，投递）。回执再报一遍格位既重复又落后。
        let summary = if to == DISCARD {
            format!("已丢弃格 {from}")
        } else {
            "已完成".to_owned()
        };
        ToolResult::success_json(call_id, json!({ "done": summary }))
    }

    /// 用法全文按需取——种类随**当前开着的那个屏**，不用模型自己报。
    ///
    /// 用法是静态文本：随开屏无条件投递等于每开一次就往会话区塞一份同样的
    /// 900 字节，而它一个字都不会变。查用法自己是一个动作。
    fn describe(&self, call_id: agent::ToolCallId) -> ToolResult {
        let snapshot = self.snapshots.latest();
        let Some(open) = snapshot.open_screen.as_ref() else {
            return ToolResult::failure(call_id, "现在没有开着的容器界面");
        };
        ToolResult::success(
            call_id,
            vec![ContentPart::text(container_usage(&open.kind))],
        )
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
                        "enum": ["move", "describe", "close"],
                        "description": "move=把 from 格的东西弄到 to 格（移动/合堆/对调）；describe=取当前这种容器的完整用法（不随开屏自动给，要看自己取）；close=关闭容器"
                    },
                    "from": { "type": "string", "description": "move 用：来源格位地址，照清单上写的抄（容器自有区按容器名，如 chest 0-26；熔炉族是 smelt/fuel/result；工作台是 result 与 craft 0-8；另有 pack 0-26 与 hotbar 0-8）" },
                    "to": { "type": "string", "description": "move 用：目标格位地址——空=移过去、同种物品=倒入合堆、不同物品=整组对调；写 drop=把 from 整格丢出去" },
                    "count": { "type": "integer", "description": "move 可选：只挪/只倒这么多个（to 为空或同种物品时）；不给则整组" }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        );
        definition.description = Some(
            "当前开着的容器界面（工作台、箱子、熔炉等共用）。没有 open：对容器方块\
使用（hand use_on）后界面由服务器打开，你会收到格位清单；不确定这种容器怎么用就 \
describe。开着期间用 move 搬动/合堆/摆料/取物，close 关闭。开着时无法移动或与世界交互。\
容器在服务端，动作的效果不会立刻反映出来；变化会主动通知你，等通知即可，别急着重做。\
**一条消息里可以连发多个动作**——想好整套摆法就一次发全，比一次一格来回等快得多，\
也不会看到摆到一半的中间产物。"
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
                Some("describe") => self.describe(call_id),
                Some("close") => self.close(call_id).await,
                _ => ToolResult::failure(
                    call_id,
                    "action 必须是 move/describe/close 之一；请改写调用",
                ),
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
            from: String,
            to: String,
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
        fn throw_slot<'a>(&'a self, slot: String) -> PortFuture<'a, Result<(), String>> {
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

    /// 开着某种容器的快照：describe 的种类从这里读，不用模型自报。
    struct OpenSnapshots(&'static str);

    impl SnapshotSource for OpenSnapshots {
        fn latest(&self) -> Arc<world::TickSnapshot> {
            let mut snapshot =
                world::TickSnapshot::empty(world::Epoch(1), 1, world::ConnectionPhase::Ready);
            snapshot.open_screen = Some(world::OpenScreenState {
                kind: self.0.to_owned(),
                container_id: 1,
                title: None,
            });
            Arc::new(snapshot)
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
        let closed = invoke(
            &fixture,
            json!({"action": "move", "from": "result", "to": "hotbar 4"}),
        )
        .await;
        assert_eq!(closed.status, ToolResultStatus::Error);
        assert!(text_of(&closed).contains("hand use_on"));
        assert!(fixture.door.calls.lock().unwrap().is_empty());

        server_opens(&fixture);
        let moved = invoke(
            &fixture,
            json!({"action": "move", "from": "result", "to": "hotbar 4"}),
        )
        .await;
        assert_eq!(moved.status, ToolResultStatus::Success);
        let thrown = invoke(
            &fixture,
            json!({"action": "move", "from": "armor head", "to": "drop"}),
        )
        .await;
        assert_eq!(thrown.status, ToolResultStatus::Success);
        assert_eq!(
            *fixture.door.calls.lock().unwrap(),
            vec!["move(result,hotbar 4)", "throw(armor head)"]
        );
    }

    #[tokio::test]
    async fn count_passes_through_to_the_door_but_not_with_discard() {
        let fixture = fixture(false);
        server_opens(&fixture);
        let moved = invoke(
            &fixture,
            json!({"action": "move", "from": "pack 37", "to": "pack 2", "count": 1}),
        )
        .await;
        assert_eq!(moved.status, ToolResultStatus::Success);
        assert_eq!(
            *fixture.door.calls.lock().unwrap(),
            vec!["move(pack 37,pack 2,1)"]
        );

        let bad = invoke(
            &fixture,
            json!({"action": "move", "from": "pack 37", "to": "drop", "count": 2}),
        )
        .await;
        assert_eq!(bad.status, ToolResultStatus::Error);
        assert!(text_of(&bad).contains("不能与 drop 连用"));
    }

    #[tokio::test]
    async fn move_refuses_when_a_different_screen_is_open() {
        let fixture = fixture(false);
        fixture.state.server_open(ScreenKind::Inventory);
        let result = invoke(
            &fixture,
            json!({"action": "move", "from": "pack 1", "to": "pack 2"}),
        )
        .await;
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

    /// 用法按需取，种类从当前开着的屏读——模型不用也不该自己报种类。
    #[tokio::test]
    async fn describe_returns_the_usage_of_the_currently_open_kind() {
        let occupancy = Arc::new(Occupancy::new());
        let state = Arc::new(ScreenState::new());
        let screen = ContainerScreen::new(
            occupancy,
            state,
            Arc::new(RecordingDoor::default()),
            Arc::new(OpenSnapshots("furnace")),
        );
        let result = dispatch::ToolProvider::call(
            &screen,
            ToolCall::new("call-1", TOOL_NAME, json!({"action": "describe"})),
        )
        .await;
        assert_eq!(result.status, ToolResultStatus::Success);
        let text = text_of(&result);
        assert!(text.contains("容器界面用法"), "{text}");
        // 熔炉的种类补充要在，否则 describe 等于只给了通用段。
        assert!(text.contains("燃料"), "{text}");
    }

    /// 没有开着的屏就如实拒绝，不编一份通用用法糊弄过去。
    #[tokio::test]
    async fn describe_without_an_open_screen_is_rejected() {
        let fixture = fixture(false);
        let result = invoke(&fixture, json!({"action": "describe"})).await;
        assert_eq!(result.status, ToolResultStatus::Error);
        assert!(text_of(&result).contains("没有开着的容器界面"));
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
