//! 感知：主动看世界的工具。Free 类——看不占身体域、不受屏压制。
//!
//! 薄壳：几何（视锥、遮挡、定向分类）全在接入模块的视口内核，这里只做
//! 参数校验、门调用与呈现（措辞归渲染层）。视口变焦（真权衡参数）与
//! 信息工具（Day #/群系）随裁定落位。

use std::sync::{Arc, Mutex};

use agent::{PortFuture, ToolCall, ToolDefinition, ToolResult};
use dispatch::{ToolClass, ToolProvider};
use serde_json::{json, Value};
use world::{BlockMemory, DirectedProjection, ViewportProjection, MAX_DIRECTED_VIEW_POSITIONS};

/// 接入模块视口面的窄化：全景用当前姿态，定向按坐标逐个分类，
/// 增量对比方块记忆只报变化（记忆推进在门后完成）。
pub trait ViewportDoor: Send + Sync {
    fn scan<'a>(&'a self) -> PortFuture<'a, Result<ViewportProjection, String>>;
    fn scan_directed<'a>(
        &'a self,
        positions: Vec<[i32; 3]>,
    ) -> PortFuture<'a, Result<DirectedProjection, String>>;
    fn scan_changes<'a>(&'a self) -> PortFuture<'a, Result<Vec<world::BlockChange>, String>>;
}

const TOOL_NAME: &str = "scan";

pub struct PerceptionTools {
    door: Arc<dyn ViewportDoor>,
    /// 方块记忆（组合根注入的共享认知状态）：scan 的回执送达模型即成为
    /// 「模型知道的事实」，在此吸收。内核的 settled 通道保证已定回执必达，
    /// 所以产出时上账即可，不需要请求级 commit 钩子。
    memory: Arc<Mutex<BlockMemory>>,
}

impl PerceptionTools {
    pub fn new(door: Arc<dyn ViewportDoor>, memory: Arc<Mutex<BlockMemory>>) -> Self {
        Self { door, memory }
    }

    fn absorb_projection(&self, projection: &ViewportProjection) {
        let mut memory = self.memory.lock().expect("方块记忆锁不应中毒");
        memory.absorb_visible(&projection.visible_blocks.blocks);
        // 脚下与注视方块同样呈现给了模型，一并上账。
        for block in [&projection.standing_on_block, &projection.looked_at_block]
            .into_iter()
            .flatten()
        {
            memory.absorb_visible(std::slice::from_ref(block));
        }
    }

    async fn dispatch(&self, call: ToolCall) -> ToolResult {
        let call_id = call.id.clone();
        let Some(arguments) = call.arguments.as_object() else {
            return ToolResult::failure(call_id, "参数必须是 JSON 对象；请改写调用");
        };
        let changes_mode = matches!(arguments.get("changes"), Some(Value::Bool(true)));
        if changes_mode && arguments.get("at").is_some() {
            return ToolResult::failure(
                call_id,
                "changes 与 at 一次只能用一种模式；请改写调用",
            );
        }
        if changes_mode {
            // 记忆的 diff 与推进都在门后同一把锁内完成，这里只管呈现。
            return match self.door.scan_changes().await {
                Ok(changes) => ToolResult::success(
                    call_id,
                    vec![agent::ContentPart::text(render::render_block_changes(
                        &changes,
                    ))],
                ),
                Err(reason) => ToolResult::failure(call_id, reason),
            };
        }
        match arguments.get("at") {
            None => match self.door.scan().await {
                Ok(projection) => {
                    self.absorb_projection(&projection);
                    ToolResult::success(
                        call_id,
                        vec![agent::ContentPart::text(render::render_viewport(
                            &projection,
                        ))],
                    )
                }
                Err(reason) => ToolResult::failure(call_id, reason),
            },
            Some(at) => {
                let positions: Option<Vec<[i32; 3]>> = at.as_array().map(|rows| {
                    rows.iter()
                        .filter_map(|row| {
                            let coords: Vec<i64> =
                                row.as_array()?.iter().filter_map(Value::as_i64).collect();
                            if coords.len() == 3
                                && coords.iter().all(|axis| i32::try_from(*axis).is_ok())
                            {
                                Some([coords[0] as i32, coords[1] as i32, coords[2] as i32])
                            } else {
                                None
                            }
                        })
                        .collect()
                });
                let Some(positions) = positions.filter(|positions| {
                    !positions.is_empty()
                        && positions.len() == at.as_array().map(Vec::len).unwrap_or(0)
                }) else {
                    return ToolResult::failure(
                        call_id,
                        format!(
                            "at 需要 1..={MAX_DIRECTED_VIEW_POSITIONS} 个 [x, y, z] 整数坐标；请改写调用"
                        ),
                    );
                };
                match self.door.scan_directed(positions).await {
                    Ok(projection) => {
                        self.memory
                            .lock()
                            .expect("方块记忆锁不应中毒")
                            .absorb_directed(&projection);
                        ToolResult::success(
                            call_id,
                            vec![agent::ContentPart::text(render::render_directed(
                                &projection,
                            ))],
                        )
                    }
                    Err(reason) => ToolResult::failure(call_id, reason),
                }
            }
        }
    }
}

impl ToolProvider for PerceptionTools {
    fn tools(&self) -> Vec<(ToolDefinition, ToolClass)> {
        let mut definition = ToolDefinition::new(
            TOOL_NAME,
            json!({
                "type": "object",
                "properties": {
                    "at": {
                        "type": "array",
                        "items": { "type": "array", "items": { "type": "integer" } },
                        "description": "可选：定向查看这些方块坐标 [[x,y,z],..]（至多 16 个），逐个告诉你看得见还是被什么挡住。不给则环视当前朝向的整个视野"
                    },
                    "changes": {
                        "type": "boolean",
                        "description": "可选：只报自上次以来视野内的变化（新看到/变了/亲眼见空），比环视省得多。没列出的坐标不代表没东西；想确认具体位置用 at。与 at 互斥"
                    }
                },
                "additionalProperties": false
            }),
        );
        definition.description = Some(
            "看。三种用法：环视（默认，用当前朝向，视锥+遮挡，看不见背后和被挡住的东西）、\
定向（at，确认指定坐标可见与否）、增量（changes，只报自上次以来的变化）。不打断任何动作。"
                .to_owned(),
        );
        vec![(definition, ToolClass::Free)]
    }

    fn call<'a>(&'a self, call: ToolCall) -> PortFuture<'a, ToolResult> {
        Box::pin(async move { self.dispatch(call).await })
    }
}

#[cfg(test)]
mod tests {
    use agent::ToolResultStatus;
    use serde_json::json;

    use super::*;

    struct CannedDoor;

    impl ViewportDoor for CannedDoor {
        fn scan<'a>(&'a self) -> PortFuture<'a, Result<ViewportProjection, String>> {
            Box::pin(async {
                Ok(ViewportProjection {
                    pose: world::ViewportPose {
                        position: [0.5, 64.0, 0.5],
                        yaw_degrees: 0.0,
                        pitch_degrees: 0.0,
                    },
                    standing_on_block: None,
                    looked_at_block: None,
                    visible_entities: world::VisibleEntitiesResult {
                        items: Vec::new(),
                        truncated: false,
                    },
                    visible_blocks: world::VisibleBlocksResult {
                        blocks: Vec::new(),
                        truncated: false,
                    },
                })
            })
        }

        fn scan_directed<'a>(
            &'a self,
            positions: Vec<[i32; 3]>,
        ) -> PortFuture<'a, Result<DirectedProjection, String>> {
            Box::pin(async move {
                Ok(DirectedProjection {
                    seen: positions
                        .into_iter()
                        .map(|at| world::DirectedSeenBlock {
                            at,
                            name: "stone".to_owned(),
                            properties: Default::default(),
                        })
                        .collect(),
                    unseen: Vec::new(),
                })
            })
        }

        fn scan_changes<'a>(&'a self) -> PortFuture<'a, Result<Vec<world::BlockChange>, String>> {
            Box::pin(async {
                Ok(vec![world::BlockChange::Appeared {
                    at: [3, 64, 3],
                    fact: world::BlockFact {
                        name: "oak_log".to_owned(),
                        properties: Default::default(),
                    },
                }])
            })
        }
    }

    fn call(arguments: serde_json::Value) -> ToolCall {
        ToolCall::new("call-1", TOOL_NAME, arguments)
    }

    fn tools_with_memory() -> (PerceptionTools, Arc<Mutex<BlockMemory>>) {
        let memory = Arc::new(Mutex::new(BlockMemory::new()));
        (
            PerceptionTools::new(Arc::new(CannedDoor), memory.clone()),
            memory,
        )
    }

    #[tokio::test]
    async fn panoramic_scan_renders_text() {
        let (tools, _) = tools_with_memory();
        let result = tools.call(call(json!({}))).await;
        assert_eq!(result.status, ToolResultStatus::Success);
    }

    #[tokio::test]
    async fn directed_scan_accepts_coordinates_and_rejects_garbage() {
        let (tools, _) = tools_with_memory();
        let ok = tools.call(call(json!({"at": [[1, 64, -3]]}))).await;
        assert_eq!(ok.status, ToolResultStatus::Success);

        for bad in [
            json!({"at": []}),
            json!({"at": [[1, 2]]}),
            json!({"at": "x"}),
        ] {
            let result = tools.call(call(json!({"at": bad["at"]}))).await;
            assert_eq!(result.status, ToolResultStatus::Error, "{bad}");
        }
    }

    /// 增量模式：走门取变化并呈现；与 at 连用如实拒绝。
    #[tokio::test]
    async fn changes_mode_renders_deltas_and_excludes_directed() {
        let (tools, _) = tools_with_memory();
        let ok = tools.call(call(json!({"changes": true}))).await;
        assert_eq!(ok.status, ToolResultStatus::Success);
        let text = format!("{:?}", ok.content);
        assert!(text.contains("新看到"), "{text}");
        assert!(text.contains("定向查看"), "{text}");

        let both = tools
            .call(call(json!({"changes": true, "at": [[1, 2, 3]]})))
            .await;
        assert_eq!(both.status, ToolResultStatus::Error);
    }

    /// 观察源接入：scan 回执产出即吸收进方块记忆。
    #[tokio::test]
    async fn successful_scans_feed_the_block_memory() {
        let (tools, memory) = tools_with_memory();
        assert!(memory.lock().unwrap().is_empty());
        let ok = tools.call(call(json!({"at": [[1, 64, -3]]}))).await;
        assert_eq!(ok.status, ToolResultStatus::Success);
        let facts = memory.lock().unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts.get([1, 64, -3]).unwrap().name, "stone");
    }

    #[test]
    fn registers_one_free_tool() {
        let (tools, _) = tools_with_memory();
        let registered = ToolProvider::tools(&tools);
        assert_eq!(registered.len(), 1);
        assert_eq!(registered[0].0.name.as_str(), "scan");
        assert_eq!(registered[0].1, ToolClass::Free);
    }
}
