//! 手：攻击、挖掘、使用与物品操作。
//!
//! 客户端考证（26.1.2）的三条互斥事实由模块一的状态机如实仲裁，本层不复刻：
//! 用着物品时攻/挖/用的点击被吞、挖着时不能用、攻与用都过 isHandsBusy——
//! 门拒绝什么，工具就把原因原文转达给模型。手与移动完全正交（边走边挖合法）。
//!
//! 挖掘与持续使用是状态：开挖/放置那刻机器代看一眼目标（挖什么看什么的
//! 原版机制，命中面因此真实），`release` 是唯一的松手动词（停止挖掘或
//! 松开使用中的物品——手是单槽，正在做的只有一件事，不需要两个松法）。
//! 瞬时动词（attack/place/drop/swap_offhand/select_slot）发出即完，无持续占用；
//! 挖穿/放上与否由世界变化通知证实，回执不替服务端作证。

use std::sync::Arc;

use agent::{PortFuture, ToolCall, ToolDefinition, ToolResult};
use dispatch::{Domain, ToolClass, ToolProvider};
use serde_json::{json, Value};

/// 模块一写口的窄化。use_on 不带手别参数：原版按主手→副手自动轮询
/// （Minecraft.startUseItem 遍历 InteractionHand），副手内容经 swap_offhand 调换。
pub trait HandDoor: Send + Sync {
    fn attack<'a>(&'a self, entity_key: &'a str) -> PortFuture<'a, Result<(), String>>;
    /// 按顺序挖一串方块。**队列**：机器逐块挖完，新队列顶替旧队列。
    /// 单块也走这里（长度 1 的数组）。
    fn mine<'a>(&'a self, blocks: Vec<[i32; 3]>) -> PortFuture<'a, Result<(), String>>;
    /// 把手持方块放到目标空位（目标须紧挨已有方块；依附面由机器代选）。
    fn place<'a>(&'a self, block: [i32; 3]) -> PortFuture<'a, Result<(), String>>;
    fn use_on_block<'a>(&'a self, block: [i32; 3]) -> PortFuture<'a, Result<(), String>>;
    fn use_on_entity<'a>(&'a self, entity_key: &'a str) -> PortFuture<'a, Result<(), String>>;
    fn use_item<'a>(&'a self) -> PortFuture<'a, Result<(), String>>;
    /// 松手：停止挖掘或松开使用中的物品。没在做事时松手不是错误。
    fn release<'a>(&'a self) -> PortFuture<'a, Result<(), String>>;
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
            Some("attack") => match arguments.get("entity").and_then(Value::as_str) {
                Some(entity_key) => self.door.attack(entity_key).await,
                None => {
                    return ToolResult::failure(call_id, "attack 需要字符串参数 entity；请改写调用")
                }
            },
            Some("mine") => match read_blocks(arguments.get("blocks").or(arguments.get("block"))) {
                Ok(blocks) => self.door.mine(blocks).await,
                Err(reason) => return ToolResult::failure(call_id, reason),
            },
            Some("place") => match read_block(arguments.get("block")) {
                Ok(block) => self.door.place(block).await,
                Err(reason) => return ToolResult::failure(call_id, reason),
            },
            Some("use_on") => {
                match (
                    arguments.get("entity").and_then(Value::as_str),
                    arguments.get("block"),
                ) {
                    (Some(entity_key), None) => self.door.use_on_entity(entity_key).await,
                    (None, Some(block)) => match read_block(Some(block)) {
                        Ok(block) => self.door.use_on_block(block).await,
                        Err(reason) => return ToolResult::failure(call_id, reason),
                    },
                    _ => {
                        return ToolResult::failure(
                            call_id,
                            "use_on 需要 entity 或 block 恰好其一；请改写调用",
                        )
                    }
                }
            }
            Some("use_item") => self.door.use_item().await,
            Some("release") => self.door.release().await,
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
                    "action 必须是 attack/mine/place/use_on/use_item/release/drop/swap_offhand/select_slot 之一；请改写调用",
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

/// 一串坐标。兼容单块写法（`[x,y,z]`）——模型给一块时不必包成数组的数组。
fn read_blocks(value: Option<&Value>) -> Result<Vec<[i32; 3]>, String> {
    let Some(array) = value.and_then(Value::as_array) else {
        return Err("mine 需要 blocks：坐标数组，如 [[37,63,-10]]；请改写调用".to_owned());
    };
    if array.is_empty() {
        return Err("blocks 是空的，没有要挖的".to_owned());
    }
    // 单块写法 [x,y,z]：三个整数。
    if array.iter().all(Value::is_number) {
        return read_block(value).map(|block| vec![block]);
    }
    array
        .iter()
        .map(|item| read_block(Some(item)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "blocks 里每一项都要是 [x, y, z] 三个整数；请改写调用".to_owned())
}

fn read_block(value: Option<&Value>) -> Result<[i32; 3], String> {
    let coords: Option<Vec<i64>> = value
        .and_then(Value::as_array)
        .map(|array| array.iter().filter_map(Value::as_i64).collect());
    match coords {
        Some(coords)
            if coords.len() == 3 && coords.iter().all(|axis| i32::try_from(*axis).is_ok()) =>
        {
            Ok([coords[0] as i32, coords[1] as i32, coords[2] as i32])
        }
        _ => Err("block 需要 [x, y, z] 三个整数；请改写调用".to_owned()),
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
                        "enum": ["attack", "mine", "place", "use_on", "use_item", "release", "drop", "swap_offhand", "select_slot"],
                        "description": "attack=攻击实体；mine=按顺序挖掉一串方块（blocks 给坐标数组，机器逐块挖完；挖穿要时间，别急着发下一个——再发一次 mine 会放弃当前这串。想看进度用 jobs 工具；可用 release 停手）；place=把手持方块放到目标空位（目标须紧挨已有方块）；use_on=对方块/实体使用（右键）；use_item=使用手持物品（吃/喝/举盾，持续到用完或 release）；release=松手；drop=丢手持物；swap_offhand=主副手对调；select_slot=选快捷栏格"
                    },
                    "entity": { "type": "string", "description": "attack/use_on 用：目标实体的 entity_key" },
                    "block": { "type": "array", "items": {"type": "integer"}, "description": "place/use_on 用：方块坐标 [x, y, z]" },
                    "blocks": { "type": "array", "items": {"type": "array", "items": {"type": "integer"}}, "description": "mine 用：要按顺序挖的坐标数组，如 [[37,63,-10],[37,64,-10]]；不必先确认那里有什么，空气会被如实拒绝" },
                    "stack": { "type": "boolean", "description": "drop 用：true 丢整组，默认丢一个" },
                    "slot": { "type": "integer", "minimum": 0, "maximum": 8, "description": "select_slot 用：快捷栏格号" }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        );
        definition.description = Some(
            "手上动作。攻击、挖掘、使用三者同一时刻只能做一件（正在使用物品时攻击和挖掘\
无效，挖掘中无法使用物品）；与走动互不影响。开挖和放置时会自动看向目标。\
回执只代表动作已发出：挖穿了没有、放上了没有，以世界变化通知为准，等通知即可，别急着重做。\
放置放的是当前手持物；拿的不是可放置的方块时服务端不会理会。"
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
        Box::pin(async move { self.dispatch(call).await })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use agent::ToolResultStatus;
    use serde_json::json;

    use super::*;

    #[derive(Default)]
    struct RecordingDoor {
        calls: StdMutex<Vec<String>>,
        refuse: Option<&'static str>,
    }

    impl RecordingDoor {
        fn log<'a>(&'a self, entry: String) -> PortFuture<'a, Result<(), String>> {
            Box::pin(async move {
                if let Some(reason) = self.refuse {
                    return Err(reason.to_owned());
                }
                self.calls.lock().unwrap().push(entry);
                Ok(())
            })
        }
    }

    impl HandDoor for RecordingDoor {
        fn attack<'a>(&'a self, entity_key: &'a str) -> PortFuture<'a, Result<(), String>> {
            self.log(format!("attack({entity_key})"))
        }
        fn mine<'a>(&'a self, blocks: Vec<[i32; 3]>) -> PortFuture<'a, Result<(), String>> {
            self.log(format!("mine{blocks:?}"))
        }
        fn place<'a>(&'a self, block: [i32; 3]) -> PortFuture<'a, Result<(), String>> {
            self.log(format!("place{block:?}"))
        }
        fn use_on_block<'a>(&'a self, block: [i32; 3]) -> PortFuture<'a, Result<(), String>> {
            self.log(format!("use_on_block{block:?}"))
        }
        fn use_on_entity<'a>(&'a self, entity_key: &'a str) -> PortFuture<'a, Result<(), String>> {
            self.log(format!("use_on_entity({entity_key})"))
        }
        fn use_item<'a>(&'a self) -> PortFuture<'a, Result<(), String>> {
            self.log("use_item".to_owned())
        }
        fn release<'a>(&'a self) -> PortFuture<'a, Result<(), String>> {
            self.log("release".to_owned())
        }
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

    fn tools(refuse: Option<&'static str>) -> (HandTools, Arc<RecordingDoor>) {
        let door = Arc::new(RecordingDoor {
            refuse,
            ..RecordingDoor::default()
        });
        (HandTools::new(door.clone()), door)
    }

    fn call(arguments: Value) -> ToolCall {
        ToolCall::new("call-1", TOOL_NAME, arguments)
    }

    #[tokio::test]
    async fn every_action_reaches_the_door_with_its_arguments() {
        let (tools, door) = tools(None);
        for arguments in [
            json!({"action": "attack", "entity": "zombie-7"}),
            json!({"action": "mine", "block": [10, 64, -3]}),
            json!({"action": "place", "block": [11, 64, -3]}),
            json!({"action": "use_on", "block": [10, 65, -3]}),
            json!({"action": "use_on", "entity": "villager-2"}),
            json!({"action": "use_item"}),
            json!({"action": "release"}),
            json!({"action": "drop"}),
            json!({"action": "drop", "stack": true}),
            json!({"action": "swap_offhand"}),
            json!({"action": "select_slot", "slot": 8}),
        ] {
            let result = tools.call(call(arguments)).await;
            assert_eq!(result.status, ToolResultStatus::Success);
        }
        assert_eq!(
            *door.calls.lock().unwrap(),
            vec![
                "attack(zombie-7)",
                "mine[[10, 64, -3]]",
                "place[11, 64, -3]",
                "use_on_block[10, 65, -3]",
                "use_on_entity(villager-2)",
                "use_item",
                "release",
                "drop(false)",
                "drop(true)",
                "swap_offhand",
                "select_slot(8)",
            ]
        );
    }

    #[tokio::test]
    async fn bad_arguments_are_rejected_before_the_door() {
        let (tools, door) = tools(None);
        for arguments in [
            json!({"action": "attack"}),
            json!({"action": "mine", "blocks": []}),
            json!({"action": "mine", "block": [1, 2]}),
            json!({"action": "mine", "block": [1.5, 2.0, 3.0]}),
            json!({"action": "place"}),
            json!({"action": "place", "block": [1, 2]}),
            json!({"action": "use_on"}),
            json!({"action": "use_on", "entity": "a", "block": [1, 2, 3]}),
            json!({"action": "select_slot", "slot": 9}),
            json!({"action": "wave"}),
        ] {
            let result = tools.call(call(arguments.clone())).await;
            assert_eq!(
                result.status,
                ToolResultStatus::Error,
                "该被拒绝：{arguments}"
            );
        }
        assert!(door.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn machine_refusal_reaches_the_model_verbatim() {
        // 模块一状态机的如实拒绝（如"正在挖掘，无法使用物品"）原文转达。
        let (tools, _) = tools(Some("正在挖掘，无法使用物品"));
        let result = tools.call(call(json!({"action": "use_item"}))).await;
        assert_eq!(result.status, ToolResultStatus::Error);
        match &result.content[0] {
            agent::ContentPart::Text { text } => {
                assert!(text.contains("正在挖掘"));
            }
            other => panic!("期望文本失败原因，得到 {other:?}"),
        }
    }

    #[test]
    fn registers_one_hand_domain_tool() {
        let (tools, _) = tools(None);
        let registered = ToolProvider::tools(&tools);
        assert_eq!(registered.len(), 1);
        assert_eq!(registered[0].0.name.as_str(), "hand");
        assert_eq!(
            registered[0].1,
            ToolClass::Body {
                domain: Domain::Hand
            }
        );
    }

    /// 描述里讲了的动作，schema 必须也放行。
    ///
    /// mining_status 曾经实现了、描述里也写了，却漏在 enum 之外——严格模式的
    /// 服务商会照 enum 挡掉，模型看得见却调不出来。这正是本工具自己在修的
    /// 「工具说了谎」，用测试钉住。
    #[test]
    fn every_action_the_description_mentions_is_allowed_by_the_schema() {
        let (tools, _) = tools(None);
        let registered = ToolProvider::tools(&tools);
        let definition = &registered[0].0;
        let allowed: Vec<&str> = definition.input_schema["properties"]["action"]["enum"]
            .as_array()
            .expect("action 应有 enum")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let described = definition.input_schema["properties"]["action"]["description"]
            .as_str()
            .expect("action 应有描述");
        // 动作名写成 `名字=解释`，名字是 ASCII 标识符；解释里的中文分号
        // （括号内的补充说明）不构成新动作，靠这条形状过滤掉。
        let mentioned: Vec<&str> = described
            .split('；')
            .filter_map(|clause| clause.split('=').next())
            .map(str::trim)
            .filter(|name| {
                !name.is_empty()
                    && name
                        .chars()
                        .all(|character| character.is_ascii_lowercase() || character == '_')
            })
            .collect();
        assert!(
            mentioned.len() >= allowed.len(),
            "描述漏讲了动作：讲了 {mentioned:?}，schema 放行 {allowed:?}"
        );
        for action in mentioned {
            assert!(
                allowed.contains(&action),
                "描述里讲了 {action}，schema 的 enum 却没放行：{allowed:?}"
            );
        }
    }
}
