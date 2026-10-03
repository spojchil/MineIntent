//! 键鼠输入：模型对世界动手的唯一身体通道。
//!
//! 一次调用 = 一组按键与至多一个鼠标键按住若干秒后全部松开，起手可以先挪一下鼠标
//! （相对转动）。左右键作用于准星所指，没有坐标参数——看哪里、按什么，都跟着画面走。
//! 调用在松开之后才返回，回执说清结果（位移、准星下是什么、挖碎了什么）。
//!
//! 实时性暂不追求：按住期间模型看不见世界，所以单次按住有上限
//! （[`world::MAX_INPUT_TICKS`]）。快捷栏、丢弃、换手这些瞬时键在 `hand` 工具里。

use std::sync::Arc;

use agent::{ContentPart, PortFuture, ToolCall, ToolDefinition, ToolResult};
use dispatch::{Domain, ToolClass, ToolProvider};
use serde_json::{json, Map, Value};
use world::{HeldKeys, InputOutcome, InputSpec, MouseButton, Turn, MAX_INPUT_TICKS};

/// 模块一写口的窄化：按下、等松开、回执结果。Err 是机器的如实拒绝（未连接、已死亡等）。
pub trait InputDoor: Send + Sync {
    fn input<'a>(&'a self, spec: InputSpec) -> PortFuture<'a, Result<InputOutcome, String>>;
}

const TOOL_NAME: &str = "input";
const TICKS_PER_SECOND: f64 = 20.0;

pub struct InputTools {
    door: Arc<dyn InputDoor>,
}

impl InputTools {
    pub fn new(door: Arc<dyn InputDoor>) -> Self {
        Self { door }
    }

    async fn dispatch(&self, call: ToolCall) -> ToolResult {
        let call_id = call.id.clone();
        let Some(arguments) = call.arguments.as_object() else {
            return ToolResult::failure(call_id, "参数必须是 JSON 对象；请改写调用");
        };
        let spec = match read_spec(arguments) {
            Ok(spec) => spec,
            Err(reason) => return ToolResult::failure(call_id, reason),
        };
        match self.door.input(spec).await {
            Ok(outcome) => ToolResult::success(
                call_id,
                vec![ContentPart::text(render::render_input_outcome(&outcome))],
            ),
            Err(reason) => ToolResult::failure(call_id, reason),
        }
    }
}

fn read_spec(arguments: &Map<String, Value>) -> Result<InputSpec, String> {
    let keys = read_keys(arguments.get("keys"))?;
    let mouse = match arguments.get("mouse") {
        None | Some(Value::Null) => None,
        Some(Value::String(button)) if button == "left" => Some(MouseButton::Left),
        Some(Value::String(button)) if button == "right" => Some(MouseButton::Right),
        Some(_) => return Err("mouse 只能是 \"left\" 或 \"right\"；请改写调用".to_owned()),
    };
    let turn = read_turn(arguments.get("turn"))?;
    let ticks = read_ticks(arguments.get("seconds"))?;
    if !keys.any() && mouse.is_none() && turn.is_none() {
        return Err("什么都没按：keys、mouse、turn 至少给一个；想等一会儿用 wait".to_owned());
    }
    Ok(InputSpec {
        keys,
        mouse,
        turn,
        ticks,
    })
}

fn read_keys(value: Option<&Value>) -> Result<HeldKeys, String> {
    let mut keys = HeldKeys::default();
    let Some(value) = value else {
        return Ok(keys);
    };
    let Some(names) = value.as_array() else {
        return Err("keys 要是键名数组，如 [\"w\", \"space\"]；请改写调用".to_owned());
    };
    for name in names {
        let slot = match name.as_str() {
            Some("w") => &mut keys.forward,
            Some("s") => &mut keys.back,
            Some("a") => &mut keys.left,
            Some("d") => &mut keys.right,
            Some("space") => &mut keys.jump,
            Some("shift") => &mut keys.sneak,
            Some("ctrl") => &mut keys.sprint,
            _ => {
                return Err(format!(
                    "不认识的键 {name}；keys 只能用 w/a/s/d/space/shift/ctrl"
                ))
            }
        };
        *slot = true;
    }
    Ok(keys)
}

fn read_turn(value: Option<&Value>) -> Result<Option<Turn>, String> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let angle = |axis: &str| -> Result<f32, String> {
        match value.get(axis) {
            None => Ok(0.0),
            Some(angle) => angle
                .as_f64()
                .filter(|angle| angle.is_finite() && angle.abs() <= 360.0)
                .map(|angle| angle as f32)
                .ok_or_else(|| format!("turn.{axis} 要是 ±360 以内的数字（度）；请改写调用")),
        }
    };
    if !value.is_object() {
        return Err("turn 要是 {\"yaw\": 度, \"pitch\": 度}；请改写调用".to_owned());
    }
    Ok(Some(Turn {
        yaw: angle("yaw")?,
        pitch: angle("pitch")?,
    }))
}

/// 秒换成 tick。不给就是点按（1 tick）。
fn read_ticks(value: Option<&Value>) -> Result<u32, String> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(1);
    };
    let max_seconds = f64::from(MAX_INPUT_TICKS) / TICKS_PER_SECOND;
    let Some(seconds) = value
        .as_f64()
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0 && *seconds <= max_seconds)
    else {
        return Err(format!(
            "seconds 要在 0 到 {max_seconds} 之间；按更久请分几次按，中间看一眼"
        ));
    };
    Ok(((seconds * TICKS_PER_SECOND).round() as u32).clamp(1, MAX_INPUT_TICKS))
}

impl ToolProvider for InputTools {
    fn tools(&self) -> Vec<(ToolDefinition, ToolClass)> {
        let max_seconds = f64::from(MAX_INPUT_TICKS) / TICKS_PER_SECOND;
        let mut definition = ToolDefinition::new(
            TOOL_NAME,
            json!({
                "type": "object",
                "properties": {
                    "keys": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["w", "a", "s", "d", "space", "shift", "ctrl"] },
                        "description": "按住的键：w/a/s/d=前/左/后/右走，space=跳，shift=潜行，ctrl=疾跑（要同时按 w 才生效）"
                    },
                    "mouse": {
                        "type": "string",
                        "enum": ["left", "right"],
                        "description": "按住的鼠标键，作用于准星所指。left=攻击实体/挖方块（按住才挖得动）；right=对方块使用或放置、与实体交互，都不是就用手里的物品（吃、拉弓、举盾要按住）"
                    },
                    "turn": {
                        "type": "object",
                        "properties": {
                            "yaw": { "type": "number", "description": "向右转多少度（负数向左）" },
                            "pitch": { "type": "number", "description": "向下看多少度（负数向上）" }
                        },
                        "additionalProperties": false,
                        "description": "按键之前先挪鼠标：相对当前朝向转动"
                    },
                    "seconds": {
                        "type": "number",
                        "description": format!("按住几秒，最多 {max_seconds}；不给就是点一下")
                    }
                },
                "additionalProperties": false
            }),
        );
        definition.description = Some(
            "键盘鼠标。按住给定的键和鼠标键若干秒后全部松开，松开后才返回结果。\
turn 在按键之前生效，准星以转完的朝向为准。按着左键挖方块时，准星下的方块一碎就提前松手；\
打开界面或死亡也会提前松开。按住期间看不见世界，走远一点或挖之前先看一眼画面。"
                .to_owned(),
        );
        vec![(
            definition,
            ToolClass::Body {
                domain: Domain::Movement,
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
    use world::InputEnd;

    use super::*;

    #[derive(Default)]
    struct RecordingDoor {
        specs: StdMutex<Vec<InputSpec>>,
    }

    impl InputDoor for RecordingDoor {
        fn input<'a>(&'a self, spec: InputSpec) -> PortFuture<'a, Result<InputOutcome, String>> {
            Box::pin(async move {
                self.specs.lock().unwrap().push(spec);
                Ok(InputOutcome {
                    ticks: spec.ticks,
                    ended: InputEnd::Elapsed,
                    from: [0.5, 64.0, 0.5],
                    to: [0.5, 64.0, 3.5],
                    yaw: 0.0,
                    pitch: 0.0,
                    mouse: spec.mouse,
                    pressed_on: None,
                    broken: Vec::new(),
                    placed: Vec::new(),
                    used: None,
                    unconfirmed: None,
                })
            })
        }
    }

    fn call(arguments: Value) -> ToolCall {
        ToolCall::new("call-1", TOOL_NAME, arguments)
    }

    async fn run(arguments: Value) -> (ToolResult, Vec<InputSpec>) {
        let door = Arc::new(RecordingDoor::default());
        let tools = InputTools::new(door.clone());
        let result = tools.call(call(arguments)).await;
        let specs = door.specs.lock().unwrap().clone();
        (result, specs)
    }

    #[tokio::test]
    async fn keys_mouse_turn_and_seconds_become_one_spec() {
        let (result, specs) = run(json!({
            "keys": ["w", "ctrl", "space"],
            "mouse": "left",
            "turn": {"yaw": 30, "pitch": -10},
            "seconds": 1.5
        }))
        .await;
        assert_eq!(result.status, ToolResultStatus::Success);
        assert_eq!(
            specs,
            vec![InputSpec {
                keys: HeldKeys {
                    forward: true,
                    jump: true,
                    sprint: true,
                    ..HeldKeys::default()
                },
                mouse: Some(MouseButton::Left),
                turn: Some(Turn {
                    yaw: 30.0,
                    pitch: -10.0
                }),
                ticks: 30,
            }]
        );
    }

    #[tokio::test]
    async fn no_seconds_is_a_single_tick_tap() {
        let (_, specs) = run(json!({"mouse": "right"})).await;
        assert_eq!(specs[0].ticks, 1);
        let (_, specs) = run(json!({"turn": {"yaw": -90}})).await;
        assert_eq!(
            specs[0].turn,
            Some(Turn {
                yaw: -90.0,
                pitch: 0.0
            })
        );
    }

    #[tokio::test]
    async fn malformed_calls_are_refused_before_touching_the_door() {
        for arguments in [
            json!({}),
            json!({"keys": ["q"]}),
            json!({"keys": "w"}),
            json!({"mouse": "middle"}),
            json!({"keys": ["w"], "seconds": 0}),
            json!({"keys": ["w"], "seconds": 60}),
            json!({"turn": {"yaw": "left"}}),
            json!({"turn": 30}),
        ] {
            let (result, specs) = run(arguments.clone()).await;
            assert_eq!(result.status, ToolResultStatus::Error, "{arguments}");
            assert!(specs.is_empty(), "{arguments} 不该到达门");
        }
    }

    #[test]
    fn the_tool_is_a_body_tool_so_open_screens_suppress_it() {
        let tools = InputTools::new(Arc::new(RecordingDoor::default()));
        let listed = tools.tools();
        assert_eq!(listed.len(), 1);
        assert!(matches!(listed[0].1, ToolClass::Body { .. }));
    }
}
