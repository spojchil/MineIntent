//! 运动：位移与朝向的意图工具。
//!
//! 工具只表达意图并立刻返回；保持状态、机械终止（距离/碰撞/超时）与
//! 合法性检查都在模块一的写口后面——能不能跑由原版物理自我仲裁
//! （饱食度、前进冲量、被物品减速），我们不复刻这些规则，门拒绝就如实转达。
//!
//! go_to/forward 的可执行图只使用同伴最后观察到的三态地图；未知格不会被当成空气。
//! 路段走到知识边界后，连接机器主动环视，再在新冻结图上直达或选择观察 frontier。
//!
//! 单意图槽：新意图顶替旧意图（门的语义）；屏开着时被编排压制（Body 类）。
//! 屏压制掐掉的移动意图作废，关屏后不续走——模型想走再说一次。

use std::sync::Arc;

use agent::{PortFuture, ToolCall, ToolDefinition, ToolResult};
use dispatch::{Domain, ToolClass, ToolProvider};
use serde_json::{json, Value};

/// 模块一写口的窄化：意图写入即返回，Err 是机器的如实拒绝（未连接、已死亡等）。
pub trait MotionDoor: Send + Sync {
    fn go_to<'a>(&'a self, target: [f64; 3]) -> PortFuture<'a, Result<(), String>>;
    fn forward<'a>(&'a self, blocks: f64) -> PortFuture<'a, Result<(), String>>;
    fn stop<'a>(&'a self) -> PortFuture<'a, Result<(), String>>;
    fn jump<'a>(&'a self) -> PortFuture<'a, Result<(), String>>;
    fn sneak<'a>(&'a self, on: bool) -> PortFuture<'a, Result<(), String>>;
    fn sprint<'a>(&'a self, on: bool) -> PortFuture<'a, Result<(), String>>;
    fn look_at<'a>(&'a self, target: [f64; 3]) -> PortFuture<'a, Result<(), String>>;
    fn face<'a>(&'a self, yaw: f64, pitch: f64) -> PortFuture<'a, Result<(), String>>;
}

const MOTION_TOOL: &str = "motion";
const LOOK_TOOL: &str = "look";

pub struct MotionTools {
    door: Arc<dyn MotionDoor>,
}

impl MotionTools {
    pub fn new(door: Arc<dyn MotionDoor>) -> Self {
        Self { door }
    }

    async fn dispatch_motion(&self, call: ToolCall) -> ToolResult {
        let call_id = call.id.clone();
        let Some(arguments) = call.arguments.as_object() else {
            return ToolResult::failure(call_id, "参数必须是 JSON 对象；请改写调用");
        };
        let outcome = match arguments.get("action").and_then(Value::as_str) {
            Some("go_to") => match read_vec3(arguments.get("target")) {
                Ok(target) => self.door.go_to(target).await,
                Err(reason) => return ToolResult::failure(call_id, reason),
            },
            Some("forward") => {
                let Some(blocks) = arguments
                    .get("blocks")
                    .and_then(Value::as_f64)
                    .filter(|blocks| blocks.is_finite() && *blocks > 0.0)
                else {
                    return ToolResult::failure(call_id, "forward 需要正数参数 blocks；请改写调用");
                };
                self.door.forward(blocks).await
            }
            Some("stop") => self.door.stop().await,
            Some("jump") => self.door.jump().await,
            Some("sneak") => match arguments.get("on").and_then(Value::as_bool) {
                Some(on) => self.door.sneak(on).await,
                None => return ToolResult::failure(call_id, "sneak 需要布尔参数 on；请改写调用"),
            },
            Some("sprint") => match arguments.get("on").and_then(Value::as_bool) {
                Some(on) => self.door.sprint(on).await,
                None => return ToolResult::failure(call_id, "sprint 需要布尔参数 on；请改写调用"),
            },
            _ => {
                return ToolResult::failure(
                    call_id,
                    "action 必须是 go_to/forward/stop/jump/sneak/sprint 之一；请改写调用",
                )
            }
        };
        settle(call_id, arguments, outcome)
    }

    async fn dispatch_look(&self, call: ToolCall) -> ToolResult {
        let call_id = call.id.clone();
        let Some(arguments) = call.arguments.as_object() else {
            return ToolResult::failure(call_id, "参数必须是 JSON 对象；请改写调用");
        };
        let outcome = match arguments.get("action").and_then(Value::as_str) {
            Some("look_at") => match read_vec3(arguments.get("target")) {
                Ok(target) => self.door.look_at(target).await,
                Err(reason) => return ToolResult::failure(call_id, reason),
            },
            Some("face") => {
                let yaw = arguments.get("yaw").and_then(Value::as_f64);
                let pitch = arguments.get("pitch").and_then(Value::as_f64);
                match (yaw, pitch) {
                    (Some(yaw), Some(pitch))
                        if yaw.is_finite() && pitch.is_finite() && pitch.abs() <= 90.0 =>
                    {
                        self.door.face(yaw, pitch).await
                    }
                    _ => {
                        return ToolResult::failure(
                            call_id,
                            "face 需要数字参数 yaw 与 pitch（pitch 在 ±90 内）；请改写调用",
                        )
                    }
                }
            }
            _ => {
                return ToolResult::failure(call_id, "action 必须是 look_at/face 之一；请改写调用")
            }
        };
        settle(call_id, arguments, outcome)
    }
}

fn read_vec3(value: Option<&Value>) -> Result<[f64; 3], String> {
    let coords: Option<Vec<f64>> = value.and_then(Value::as_array).and_then(|array| {
        (array.len() == 3)
            .then(|| array.iter().map(Value::as_f64).collect::<Option<Vec<_>>>())
            .flatten()
    });
    match coords {
        Some(coords) if coords.len() == 3 && coords.iter().all(|axis| axis.is_finite()) => {
            Ok([coords[0], coords[1], coords[2]])
        }
        _ => Err("target 需要 [x, y, z] 三个数字；请改写调用".to_owned()),
    }
}

/// 意图被机器接受=成功（不代表已到达）；机器拒绝原文转达给模型。
fn settle(
    call_id: agent::ToolCallId,
    arguments: &serde_json::Map<String, Value>,
    outcome: Result<(), String>,
) -> ToolResult {
    let action = arguments
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("");
    match outcome {
        Ok(()) => ToolResult::success_json(call_id, json!({ "accepted": action })),
        Err(reason) => ToolResult::failure(call_id, reason),
    }
}

impl ToolProvider for MotionTools {
    fn tools(&self) -> Vec<(ToolDefinition, ToolClass)> {
        let mut motion = ToolDefinition::new(
            MOTION_TOOL,
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["go_to", "forward", "stop", "jump", "sneak", "sprint"],
                        "description": "go_to=寻路前往；forward=朝面前直走；stop=停下；jump=跳一下；sneak/sprint=开关潜行/疾跑"
                    },
                    "target": {
                        "type": "array", "items": {"type": "number"},
                        "description": "go_to 用：[x, y, z]——你要**站进去**的那一格，不是脚下踩的那块方块"
                    },
                    "blocks": { "type": "number", "description": "forward 用：走几格" },
                    "on": { "type": "boolean", "description": "sneak/sprint 用：开或关" }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        );
        motion.description = Some(
            "移动。发出意图立刻返回，行走在后台继续；新意图顶替旧意图。\
能不能跑动由世界决定（饱食度、被阻挡等）；到达、走不到或途中卡住会收到通知。\
\n\n**go_to 的目标是你身体要站进去的那一格，不是你看到的地面方块。**\
你看见 (12,164,10) 是 grass_block、想走过去，目标要写 (12,165,10)——\
人站在方块**上面**，不是站在方块里面。写成方块本身那一格，身体挤不进去，\
到不了。"
                .to_owned(),
        );

        let mut look = ToolDefinition::new(
            LOOK_TOOL,
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["look_at", "face"],
                        "description": "look_at=看向某个位置；face=转到指定朝向"
                    },
                    "target": {
                        "type": "array", "items": {"type": "number"},
                        "description": "look_at 用：[x, y, z]"
                    },
                    "yaw": { "type": "number", "description": "face 用：水平朝向角（度）" },
                    "pitch": { "type": "number", "description": "face 用：俯仰角（度，±90）" }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        );
        look.description =
            Some("转头。寻路与挖掘期间朝向由任务牵引，此时转头会顶掉当前任务的视线。".to_owned());

        vec![
            (
                motion,
                ToolClass::Body {
                    domain: Domain::Movement,
                },
            ),
            (
                look,
                ToolClass::Body {
                    domain: Domain::Facing,
                },
            ),
        ]
    }

    fn call<'a>(&'a self, call: ToolCall) -> PortFuture<'a, ToolResult> {
        Box::pin(async move {
            if call.name.as_str() == LOOK_TOOL {
                self.dispatch_look(call).await
            } else {
                self.dispatch_motion(call).await
            }
        })
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
        refuse: bool,
    }

    impl RecordingDoor {
        fn log<'a>(&'a self, entry: String) -> PortFuture<'a, Result<(), String>> {
            Box::pin(async move {
                if self.refuse {
                    return Err("尚未连接到世界".to_owned());
                }
                self.calls.lock().unwrap().push(entry);
                Ok(())
            })
        }
    }

    impl MotionDoor for RecordingDoor {
        fn go_to<'a>(&'a self, target: [f64; 3]) -> PortFuture<'a, Result<(), String>> {
            self.log(format!("go_to{target:?}"))
        }
        fn forward<'a>(&'a self, blocks: f64) -> PortFuture<'a, Result<(), String>> {
            self.log(format!("forward({blocks})"))
        }
        fn stop<'a>(&'a self) -> PortFuture<'a, Result<(), String>> {
            self.log("stop".to_owned())
        }
        fn jump<'a>(&'a self) -> PortFuture<'a, Result<(), String>> {
            self.log("jump".to_owned())
        }
        fn sneak<'a>(&'a self, on: bool) -> PortFuture<'a, Result<(), String>> {
            self.log(format!("sneak({on})"))
        }
        fn sprint<'a>(&'a self, on: bool) -> PortFuture<'a, Result<(), String>> {
            self.log(format!("sprint({on})"))
        }
        fn look_at<'a>(&'a self, target: [f64; 3]) -> PortFuture<'a, Result<(), String>> {
            self.log(format!("look_at{target:?}"))
        }
        fn face<'a>(&'a self, yaw: f64, pitch: f64) -> PortFuture<'a, Result<(), String>> {
            self.log(format!("face({yaw},{pitch})"))
        }
    }

    fn tools(refuse: bool) -> (MotionTools, Arc<RecordingDoor>) {
        let door = Arc::new(RecordingDoor {
            refuse,
            ..RecordingDoor::default()
        });
        (MotionTools::new(door.clone()), door)
    }

    fn call(tool: &str, arguments: Value) -> ToolCall {
        ToolCall::new("call-1", tool, arguments)
    }

    #[tokio::test]
    async fn every_action_reaches_the_door_with_its_arguments() {
        let (tools, door) = tools(false);
        for (tool, arguments) in [
            (
                MOTION_TOOL,
                json!({"action": "go_to", "target": [1.0, 64.0, -3.5]}),
            ),
            (MOTION_TOOL, json!({"action": "forward", "blocks": 3})),
            (MOTION_TOOL, json!({"action": "stop"})),
            (MOTION_TOOL, json!({"action": "jump"})),
            (MOTION_TOOL, json!({"action": "sneak", "on": true})),
            (MOTION_TOOL, json!({"action": "sprint", "on": false})),
            (
                LOOK_TOOL,
                json!({"action": "look_at", "target": [0, 70, 0]}),
            ),
            (
                LOOK_TOOL,
                json!({"action": "face", "yaw": -90.0, "pitch": 45.0}),
            ),
        ] {
            let result = tools.call(call(tool, arguments)).await;
            assert_eq!(result.status, ToolResultStatus::Success);
        }
        assert_eq!(
            *door.calls.lock().unwrap(),
            vec![
                "go_to[1.0, 64.0, -3.5]",
                "forward(3)",
                "stop",
                "jump",
                "sneak(true)",
                "sprint(false)",
                "look_at[0.0, 70.0, 0.0]",
                "face(-90,45)",
            ]
        );
    }

    #[tokio::test]
    async fn bad_arguments_are_rejected_before_the_door() {
        let (tools, door) = tools(false);
        for (tool, arguments) in [
            (MOTION_TOOL, json!({"action": "go_to", "target": [1, 2]})),
            (
                MOTION_TOOL,
                json!({"action": "go_to", "target": [1, "bad", 64, -3]}),
            ),
            (
                LOOK_TOOL,
                json!({"action": "look_at", "target": [1, "bad", -3]}),
            ),
            (MOTION_TOOL, json!({"action": "forward", "blocks": -1})),
            (MOTION_TOOL, json!({"action": "sneak"})),
            (MOTION_TOOL, json!({"action": "dance"})),
            (
                LOOK_TOOL,
                json!({"action": "face", "yaw": 0.0, "pitch": 120.0}),
            ),
            (LOOK_TOOL, json!({"action": "look_at"})),
        ] {
            let result = tools.call(call(tool, arguments.clone())).await;
            assert_eq!(
                result.status,
                ToolResultStatus::Error,
                "该被拒绝：{arguments}"
            );
        }
        assert!(door.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn door_refusal_comes_back_verbatim_as_a_model_face_failure() {
        let (tools, _) = tools(true);
        let result = tools
            .call(call(MOTION_TOOL, json!({"action": "stop"})))
            .await;
        assert_eq!(result.status, ToolResultStatus::Error);
        match &result.content[0] {
            agent::ContentPart::Text { text } => assert!(text.contains("尚未连接到世界")),
            other => panic!("期望文本失败原因，得到 {other:?}"),
        }
    }

    #[test]
    fn registers_motion_and_look_with_their_domains() {
        let (tools, _) = tools(false);
        let registered = ToolProvider::tools(&tools);
        let names: Vec<(&str, ToolClass)> = registered
            .iter()
            .map(|(definition, class)| (definition.name.as_str(), *class))
            .collect();
        assert_eq!(
            names,
            vec![
                (
                    "motion",
                    ToolClass::Body {
                        domain: Domain::Movement
                    }
                ),
                (
                    "look",
                    ToolClass::Body {
                        domain: Domain::Facing
                    }
                ),
            ]
        );
    }
}
