//! 物品栏屏：玩家自己的 46 格菜单（容器 0）。
//!
//! 原版同构：打开不发包（E 键是纯客户端动作），格位编号即协议号
//! （0 合成结果、1-4 随身合成、5-8 盔甲、9-35 主背包、36-44 快捷栏、
//! 45 副手）；操作单动词 **move 一次一对**（移动/合堆/对调三合一，语义
//! 随 to 格现状）——指针持物状态对模型不存在（动词开始空、结束空），
//! 伪格 99 表示丢弃（只进：move(from,99)=把 from 整格丢出去）。
//! 非法放置（盔甲格类型不符、成品格倒入等）由机器与服务器如实仲裁。
//!
//! 开屏期间预期之外的格位变化（拾取、合成结果出现等）由己投递通知；
//! 本屏只负责「开着没有」这个事实（[`ScreenState`]）。

use std::sync::Arc;

use agent::{ContentPart, PortFuture, ToolCall, ToolDefinition, ToolResult};
use dispatch::{Domain, Occupancy, ToolClass};
use parking_lot::Mutex;
use serde_json::{json, Value};
use world::SnapshotSource;

/// 丢弃伪格：move(from, 99) = 把 from 整格丢出去（屏内 Ctrl+Q 语义）。只进不出。
pub const DISCARD_SLOT: u16 = 99;

/// 当前开着的屏的种类。一次只能开一个屏（原版事实）；
/// 谁开谁关，跨屏互斥在这里裁，占用账本只管域。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScreenKind {
    Chat,
    Inventory,
    /// 服务端容器（工作台、箱子、熔炉等）：真相在服务端（use_on 触发开、
    /// 可被强关），本地状态由组合根随屏事实翻转。具体种类在快照
    /// `open_screen` 里，这里只管互斥。
    Container,
}

/// 屏种类状态：屏模块内共享，己也读它（开屏期间才投递库存变化通知）。
#[derive(Default)]
pub struct ScreenState {
    current: Mutex<Option<ScreenKind>>,
}

impl ScreenState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn current(&self) -> Option<ScreenKind> {
        *self.current.lock()
    }

    /// 尝试记录开屏。已有别的屏开着时如实拒绝。
    pub(crate) fn open(&self, kind: ScreenKind) -> Result<(), ScreenKind> {
        let mut current = self.current.lock();
        match *current {
            Some(existing) if existing != kind => Err(existing),
            _ => {
                *current = Some(kind);
                Ok(())
            }
        }
    }

    /// 关屏：只有本种类开着时才清（幂等）。
    pub(crate) fn close(&self, kind: ScreenKind) {
        let mut current = self.current.lock();
        if *current == Some(kind) {
            *current = None;
        }
    }

    /// 服务端主导的开屏（容器屏真相在服务端）：无条件登记，
    /// 返回被顶掉的本地屏种类（若有）——服务器说开就是开了。
    pub fn server_open(&self, kind: ScreenKind) -> Option<ScreenKind> {
        let mut current = self.current.lock();
        let displaced = current.filter(|existing| *existing != kind);
        *current = Some(kind);
        displaced
    }

    /// 服务端主导的关屏（幂等；只清本种类）。
    pub fn server_close(&self, kind: ScreenKind) {
        self.close(kind);
    }
}

/// 屏种类的人话名，拒绝话术用。
pub(crate) fn kind_word(kind: ScreenKind) -> &'static str {
    match kind {
        ScreenKind::Chat => "聊天框",
        ScreenKind::Inventory => "物品栏",
        ScreenKind::Container => "容器界面",
    }
}

/// 接入模块写口的窄化：搬动与丢弃都在 tick 回调内原子执行，
/// 格空间随当前开着的界面（机器按活动菜单解释格号）。
pub trait InventoryDoor: Send + Sync {
    /// 把 from 格的东西弄到 to 格。语义按 to 现状分派（空=移动、同种=
    /// 合堆、不同=对调），count 拆栈/限量；合法性由机器按两格现状仲裁。
    fn move_slots<'a>(
        &'a self,
        from: u16,
        to: u16,
        count: Option<u32>,
    ) -> PortFuture<'a, Result<(), String>>;
    fn throw_slot<'a>(&'a self, slot: u16) -> PortFuture<'a, Result<(), String>>;
    /// 关闭当前开着的服务端容器（发 ContainerClose）。物品栏屏用不到它。
    fn close_container<'a>(&'a self) -> PortFuture<'a, Result<(), String>>;
}

const TOOL_NAME: &str = "inventory";

const USAGE: &str = "物品栏用法：格号即协议号——0 合成结果（只出不进），1-4 随身合成格（2×2 摆料，\
成品出现在 0），5-8 盔甲（头/胸/腿/脚），9-35 主背包，36-44 快捷栏，45 副手。\
{action:\"move\", from, to} 把 from 格的东西弄到 to 格，语义随 to 现状：to 为空=移过去\
（可加 count 只挪几个，拆栈）；to 是同种物品=倒入合堆（可加 count 只倒几个，装不下的留在原格）；\
to 是不同物品=整组对调（count 不适用）；to 用 99=把 from 整格丢出去。\
成品格（0）只能整组取走。非法放置（如盔甲格放非装备）会被世界拒绝。\
开着物品栏时无法移动或与世界交互。";

/// 开屏时代替用法全文的一行指路。
const USAGE_POINTER: &str = "（格号语义与 move 的用法：{\"action\":\"describe\"}）";

pub struct InventoryScreen {
    occupancy: Arc<Occupancy>,
    state: Arc<ScreenState>,
    door: Arc<dyn InventoryDoor>,
    snapshots: Arc<dyn SnapshotSource>,
}

impl InventoryScreen {
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

    fn open(&self, call_id: agent::ToolCallId) -> ToolResult {
        if let Err(existing) = self.state.open(ScreenKind::Inventory) {
            return ToolResult::failure(
                call_id,
                format!("{}开着，先关闭它再打开物品栏", kind_word(existing)),
            );
        }
        self.occupancy.occupy(Domain::Screen);
        let listing = render::render_player_menu(&self.snapshots.latest());
        // 只给清单，不带用法全文。用法是**静态文本**：随开屏无条件重发，等于每开
        // 一次就往会话区里塞一份同样的 700 字节，而它一个字都不会变。查用法自己
        // 是一个动作（describe），与 chat_box 的 open{describe} 同一立场。
        ToolResult::success(
            call_id,
            vec![ContentPart::text(format!("{listing}\n\n{USAGE_POINTER}"))],
        )
    }

    /// 用法全文按需取。
    fn describe(&self, call_id: agent::ToolCallId) -> ToolResult {
        ToolResult::success(call_id, vec![ContentPart::text(USAGE.to_owned())])
    }

    async fn move_items(
        &self,
        call_id: agent::ToolCallId,
        from: Option<&Value>,
        to: Option<&Value>,
        count: Option<&Value>,
    ) -> ToolResult {
        if self.state.current() != Some(ScreenKind::Inventory) {
            return ToolResult::failure(call_id, "物品栏没有打开；先 open");
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
        // 回执只说动作结论，不报格位现状（理由见 container.rs 同处注释：
        // 读的是上一 tick 的快照，且格位现状是事实、归快照与格位变化窗）。
        let summary = if to == DISCARD_SLOT {
            format!("已丢弃格 {from}")
        } else {
            "已完成".to_owned()
        };
        ToolResult::success_json(call_id, json!({ "done": summary }))
    }

    fn close(&self, call_id: agent::ToolCallId) -> ToolResult {
        self.state.close(ScreenKind::Inventory);
        self.occupancy.release(Domain::Screen);
        ToolResult::success_json(call_id, json!({ "state": "closed" }))
    }
}

impl dispatch::ToolProvider for InventoryScreen {
    fn tools(&self) -> Vec<(ToolDefinition, ToolClass)> {
        let mut definition = ToolDefinition::new(
            TOOL_NAME,
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["open", "describe", "move", "close"],
                        "description": "open=打开并列出全部格位；describe=取格号语义与 move 的完整用法（不随开屏自动给，要看自己取）；move=把 from 格的东西弄到 to 格（移动/合堆/对调）；close=关闭"
                    },
                    "from": { "type": "integer", "description": "move 用：来源格号（0-45）" },
                    "to": { "type": "integer", "description": "move 用：目标格号（0-45）——空=移过去、同种物品=倒入合堆、不同物品=整组对调；99=把 from 整格丢出去" },
                    "count": { "type": "integer", "description": "move 可选：只挪/只倒这么多个（to 为空或同种物品时）；不给则整组" }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        );
        definition.description = Some(
            "物品栏。打开才能看到格位并整理（搬动/合堆/穿装备/摆随身合成/丢弃）；\
打开期间无法移动或与世界交互，看完记得关。\
与服务端交互有延迟：回执只说明点击已发出并被本地菜单接受，世界的反应要过一小会儿（实测约 2~3 游戏刻，一百多毫秒，且会浮动）才回来，并作为格位变化通知送到你这里。刚做完就去读格位，读到的多半还是旧的——**以通知为准，不要因为「立刻没看到变化」就判断动作失败而重做**。"
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
                Some("open") => self.open(call_id),
                Some("describe") => self.describe(call_id),
                Some("move") => {
                    self.move_items(
                        call_id,
                        arguments.get("from"),
                        arguments.get("to"),
                        arguments.get("count"),
                    )
                    .await
                }
                Some("close") => self.close(call_id),
                _ => ToolResult::failure(
                    call_id,
                    "action 必须是 open/describe/move/close 之一；请改写调用",
                ),
            }
        })
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
        refuse: Option<&'static str>,
    }

    impl InventoryDoor for RecordingDoor {
        fn move_slots<'a>(
            &'a self,
            from: u16,
            to: u16,
            count: Option<u32>,
        ) -> PortFuture<'a, Result<(), String>> {
            Box::pin(async move {
                if let Some(reason) = self.refuse {
                    return Err(reason.to_owned());
                }
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
            Box::pin(async move { Err("物品栏屏用不到容器关闭".to_owned()) })
        }
    }

    struct FixedSnapshots;

    impl SnapshotSource for FixedSnapshots {
        fn latest(&self) -> std::sync::Arc<world::TickSnapshot> {
            let mut snap =
                world::TickSnapshot::empty(world::Epoch(1), 100, world::ConnectionPhase::Ready);
            snap.self_state.inventory.slots.push(world::InventorySlot {
                slot: 10,
                item_name: "diamond".to_owned(),
                count: 3,
                metadata: None,
                durability_used: None,
            });
            std::sync::Arc::new(snap)
        }
    }

    struct Fixture {
        screen: InventoryScreen,
        occupancy: Arc<Occupancy>,
        state: Arc<ScreenState>,
        door: Arc<RecordingDoor>,
    }

    fn fixture(refuse: Option<&'static str>) -> Fixture {
        let occupancy = Arc::new(Occupancy::new());
        let state = Arc::new(ScreenState::new());
        let door = Arc::new(RecordingDoor {
            refuse,
            ..RecordingDoor::default()
        });
        let screen = InventoryScreen::new(
            occupancy.clone(),
            state.clone(),
            door.clone(),
            Arc::new(FixedSnapshots),
        );
        Fixture {
            screen,
            occupancy,
            state,
            door,
        }
    }

    fn call(arguments: Value) -> ToolCall {
        ToolCall::new("call-1", TOOL_NAME, arguments)
    }

    async fn invoke(fixture: &Fixture, arguments: Value) -> ToolResult {
        dispatch::ToolProvider::call(&fixture.screen, call(arguments)).await
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

    /// 开屏只给清单与一行指路，**不带用法全文**——用法是静态文本，随开屏
    /// 无条件重发等于每开一次就往会话区塞一份同样的几百字节。
    #[tokio::test]
    async fn open_lists_slots_without_the_usage_text_and_occupies_screen() {
        let fixture = fixture(None);
        let result = invoke(&fixture, json!({"action": "open"})).await;
        assert_eq!(result.status, ToolResultStatus::Success);
        let text = text_of(&result);
        assert!(text.contains("10=diamond ×3"), "{text}");
        assert!(!text.contains("物品栏用法"), "开屏不该带用法全文：{text}");
        assert!(text.contains("describe"), "但要指得出路：{text}");
        assert!(fixture.occupancy.is_occupied(Domain::Screen));
        assert_eq!(fixture.state.current(), Some(ScreenKind::Inventory));
    }

    /// 用法按需取。指路里写的动作必须真的存在——否则指了个空。
    #[tokio::test]
    async fn describe_returns_the_full_usage() {
        let fixture = fixture(None);
        let opened = text_of(&invoke(&fixture, json!({"action": "open"})).await);
        let described = invoke(&fixture, json!({"action": "describe"})).await;
        assert_eq!(described.status, ToolResultStatus::Success);
        let text = text_of(&described);
        assert!(text.contains("物品栏用法"), "{text}");
        assert!(text.contains("成品格（0）只能整组取走"), "{text}");
        // 指路那行提到的动作名与真实动作对得上。
        assert!(opened.contains("\"action\":\"describe\""), "{opened}");
    }

    #[tokio::test]
    async fn move_requires_open_then_reaches_the_door() {
        let fixture = fixture(None);
        let closed = invoke(&fixture, json!({"action": "move", "from": 10, "to": 38})).await;
        assert_eq!(closed.status, ToolResultStatus::Error);
        assert!(fixture.door.calls.lock().unwrap().is_empty());

        invoke(&fixture, json!({"action": "open"})).await;
        let moved = invoke(&fixture, json!({"action": "move", "from": 10, "to": 38})).await;
        assert_eq!(moved.status, ToolResultStatus::Success);
        let counted = invoke(
            &fixture,
            json!({"action": "move", "from": 10, "to": 20, "count": 3}),
        )
        .await;
        assert_eq!(counted.status, ToolResultStatus::Success);
        let thrown = invoke(&fixture, json!({"action": "move", "from": 10, "to": 99})).await;
        assert_eq!(thrown.status, ToolResultStatus::Success);
        assert_eq!(
            *fixture.door.calls.lock().unwrap(),
            vec!["move(10,38)", "move(10,20,3)", "throw(10)"]
        );
    }

    #[tokio::test]
    async fn door_refusal_comes_back_verbatim() {
        let fixture = fixture(Some("盔甲格只收对应装备"));
        invoke(&fixture, json!({"action": "open"})).await;
        let result = invoke(&fixture, json!({"action": "move", "from": 10, "to": 5})).await;
        assert_eq!(result.status, ToolResultStatus::Error);
        assert!(text_of(&result).contains("盔甲格只收对应装备"));
    }

    #[tokio::test]
    async fn close_releases_screen_and_state_idempotently() {
        let fixture = fixture(None);
        invoke(&fixture, json!({"action": "open"})).await;
        invoke(&fixture, json!({"action": "close"})).await;
        assert!(!fixture.occupancy.is_occupied(Domain::Screen));
        assert_eq!(fixture.state.current(), None);
        let again = invoke(&fixture, json!({"action": "close"})).await;
        assert_eq!(again.status, ToolResultStatus::Success);
    }

    #[test]
    fn screen_state_rejects_cross_kind_open() {
        let state = ScreenState::new();
        state.open(ScreenKind::Chat).unwrap();
        assert_eq!(state.open(ScreenKind::Inventory), Err(ScreenKind::Chat));
        state.close(ScreenKind::Chat);
        assert!(state.open(ScreenKind::Inventory).is_ok());
        // 关别的种类不影响当前屏。
        state.close(ScreenKind::Chat);
        assert_eq!(state.current(), Some(ScreenKind::Inventory));
    }
}
