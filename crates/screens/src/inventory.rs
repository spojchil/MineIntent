//! 物品栏屏：玩家自己的 46 格菜单（容器 0）。
//!
//! 原版同构：打开不发包（E 键是纯客户端动作），格位编号即协议号
//! （0 合成结果、1-4 随身合成、5-8 盔甲、9-35 主背包、36-44 快捷栏、
//! 45 副手）；操作单动词 **swap 一次一对**——指针持物状态对模型不存在
//! （交换开始空、结束空），伪格 99 表示丢弃（只进：swap(a,99)=把 a 整格
//! 丢出去）。非法交换（盔甲格类型不符、结果格放入等）由服务器仲裁，
//! 门如实转达。
//!
//! 开屏期间预期之外的格位变化（拾取、合成结果出现等）由己投递通知；
//! 本屏只负责「开着没有」这个事实（[`ScreenState`]）。

use std::sync::Arc;

use agent::{ContentPart, PortFuture, ToolCall, ToolDefinition, ToolResult};
use dispatch::{Domain, Occupancy, ToolClass};
use parking_lot::Mutex;
use serde_json::{json, Value};
use world::SnapshotSource;

/// 丢弃伪格：swap(a, 99) = 把 a 整格丢出去（屏内 Ctrl+Q 语义）。只进不出。
pub const DISCARD_SLOT: u16 = 99;

/// 当前开着的屏的种类。一次只能开一个屏（原版事实）；
/// 谁开谁关，跨屏互斥在这里裁，占用账本只管域。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScreenKind {
    Chat,
    Inventory,
    /// 工作台：真相在服务端（use_on 触发开、可被强关），
    /// 本地状态由组合根随屏事实翻转。
    CraftingTable,
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
        ScreenKind::CraftingTable => "工作台",
    }
}

/// 接入模块写口的窄化：交换与丢弃都在 tick 回调内原子执行，
/// 格空间随当前开着的界面（机器按活动菜单解释格号）。
pub trait InventoryDoor: Send + Sync {
    /// `count`：仅当两格恰有一格为空时有效——从非空格挪这么多个到空格
    /// （拆栈）；None = 整组交换。合法性由机器按两格现状如实仲裁。
    fn swap_slots<'a>(
        &'a self,
        a: u16,
        b: u16,
        count: Option<u32>,
    ) -> PortFuture<'a, Result<(), String>>;
    fn throw_slot<'a>(&'a self, slot: u16) -> PortFuture<'a, Result<(), String>>;
    /// 关闭当前开着的服务端容器（发 ContainerClose）。物品栏屏用不到它。
    fn close_container<'a>(&'a self) -> PortFuture<'a, Result<(), String>>;
}

const TOOL_NAME: &str = "inventory";

const USAGE: &str = "物品栏用法：格号即协议号——0 合成结果（只出不进），1-4 随身合成格（2×2 摆料，\
成品出现在 0），5-8 盔甲（头/胸/腿/脚），9-35 主背包，36-44 快捷栏，45 副手。\
{action:\"swap\", a, b} 交换两格内容，一次一对；b 用 99 表示把 a 整格丢出去；\
两格恰有一格为空时可加 count 只挪这么多个过去（拆栈），如 {action:\"swap\", a:9, b:2, count:1}。\
非法放置（如盔甲格放非装备）会被世界拒绝。开着物品栏时无法移动或与世界交互。";

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
        ToolResult::success(
            call_id,
            vec![ContentPart::text(format!("{listing}\n\n{USAGE}"))],
        )
    }

    async fn swap(
        &self,
        call_id: agent::ToolCallId,
        a: Option<&Value>,
        b: Option<&Value>,
        count: Option<&Value>,
    ) -> ToolResult {
        if self.state.current() != Some(ScreenKind::Inventory) {
            return ToolResult::failure(call_id, "物品栏没有打开；先 open");
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
        // 点击本地预演即时生效，本 tick 的快照已含交换后内容。
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
                        "enum": ["open", "swap", "close"],
                        "description": "open=打开并列出全部格位与用法；swap=交换两格（一次一对）；close=关闭"
                    },
                    "a": { "type": "integer", "description": "swap 用：格号（0-45）" },
                    "b": { "type": "integer", "description": "swap 用：格号（0-45），或 99=把 a 整格丢出去" },
                    "count": { "type": "integer", "description": "swap 可选：两格恰有一格为空时，从非空格挪这么多个到空格（拆栈）；不给则整组交换" }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        );
        definition.description = Some(
            "物品栏。打开才能看到格位并整理（交换/穿装备/摆随身合成/丢弃）；\
打开期间无法移动或与世界交互，看完记得关。"
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
                Some("swap") => {
                    self.swap(
                        call_id,
                        arguments.get("a"),
                        arguments.get("b"),
                        arguments.get("count"),
                    )
                    .await
                }
                Some("close") => self.close(call_id),
                _ => ToolResult::failure(
                    call_id,
                    "action 必须是 open/swap/close 之一；请改写调用",
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
        fn swap_slots<'a>(
            &'a self,
            a: u16,
            b: u16,
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
            Box::pin(async move { Err("物品栏屏用不到容器关闭".to_owned()) })
        }
    }

    struct FixedSnapshots;

    impl SnapshotSource for FixedSnapshots {
        fn latest(&self) -> std::sync::Arc<world::TickSnapshot> {
            let mut snap = world::TickSnapshot::empty(
                world::Epoch(1),
                100,
                world::ConnectionPhase::Ready,
            );
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

    #[tokio::test]
    async fn open_lists_slots_with_usage_and_occupies_screen() {
        let fixture = fixture(None);
        let result = invoke(&fixture, json!({"action": "open"})).await;
        assert_eq!(result.status, ToolResultStatus::Success);
        let text = text_of(&result);
        assert!(text.contains("10=diamond ×3"), "{text}");
        assert!(text.contains("物品栏用法"), "{text}");
        assert!(fixture.occupancy.is_occupied(Domain::Screen));
        assert_eq!(fixture.state.current(), Some(ScreenKind::Inventory));
    }

    #[tokio::test]
    async fn swap_requires_open_then_reaches_the_door() {
        let fixture = fixture(None);
        let closed = invoke(&fixture, json!({"action": "swap", "a": 10, "b": 38})).await;
        assert_eq!(closed.status, ToolResultStatus::Error);
        assert!(fixture.door.calls.lock().unwrap().is_empty());

        invoke(&fixture, json!({"action": "open"})).await;
        let swapped = invoke(&fixture, json!({"action": "swap", "a": 10, "b": 38})).await;
        assert_eq!(swapped.status, ToolResultStatus::Success);
        let thrown = invoke(&fixture, json!({"action": "swap", "a": 10, "b": 99})).await;
        assert_eq!(thrown.status, ToolResultStatus::Success);
        assert_eq!(
            *fixture.door.calls.lock().unwrap(),
            vec!["swap(10,38)", "throw(10)"]
        );
    }

    #[tokio::test]
    async fn door_refusal_comes_back_verbatim() {
        let fixture = fixture(Some("盔甲格只收对应装备"));
        invoke(&fixture, json!({"action": "open"})).await;
        let result = invoke(&fixture, json!({"action": "swap", "a": 10, "b": 5})).await;
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
