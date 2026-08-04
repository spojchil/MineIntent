//! 脚本化假模型 provider：gate b 纵向验收用的确定性回放（真实模型是加分项）。
//! 每个 wake 由工厂新建实例，游标从 0 起；脚本耗尽后固定返回 closing 文本，
//! 保证任何 run 都能有界收束。

use std::sync::atomic::{AtomicUsize, Ordering};

use mineintent_contracts::agent::{AgentError, ContractFuture, ExecutionControl, ModelProvider};
use mineintent_middle::agent::{AgentModelRequest, ModelCompletion};
use serde_json::{json, Map, Value};

pub type JsonObject = Map<String, Value>;

pub struct ScriptedModelProvider {
    script: Vec<JsonObject>,
    cursor: AtomicUsize,
}

impl ScriptedModelProvider {
    pub fn new(script: Vec<JsonObject>) -> Self {
        Self {
            script,
            cursor: AtomicUsize::new(0),
        }
    }

    fn closing_message() -> JsonObject {
        match json!({
            "role": "assistant",
            "content": "（脚本结束）本轮行动完成。",
        }) {
            Value::Object(message) => message,
            _ => unreachable!("json! 对象字面量必为 Object"),
        }
    }
}

impl ModelProvider for ScriptedModelProvider {
    type Request = AgentModelRequest;
    type Response = ModelCompletion;

    fn complete<'a>(
        &'a self,
        _request: Self::Request,
        _control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<Self::Response, AgentError>> {
        let index = self.cursor.fetch_add(1, Ordering::SeqCst);
        let message = self
            .script
            .get(index)
            .cloned()
            .unwrap_or_else(Self::closing_message);
        Box::pin(async move {
            Ok(ModelCompletion {
                message: Some(message),
                finish_reason: None,
                usage: None,
            })
        })
    }
}

/// 默认纵向脚本：两轮覆盖全部五个 capability（view/say + look/move/remember），
/// 随后 closing。gate b 判定：五工具往返各自返回成功、say 在服务端可见、
/// move 有真实位移、remember 落盘。
pub fn default_vertical_script() -> Vec<JsonObject> {
    [
        json!({
            "role": "assistant",
            "tool_calls": [
                {
                    "id": "scripted-1",
                    "type": "function",
                    "function": { "name": "view", "arguments": "{\"mode\":\"full\"}" }
                },
                {
                    "id": "scripted-2",
                    "type": "function",
                    "function": { "name": "say", "arguments": "{\"text\":\"我在，看了一眼周围。\"}" }
                }
            ]
        }),
        json!({
            "role": "assistant",
            "tool_calls": [
                {
                    "id": "scripted-3",
                    "type": "function",
                    "function": {
                        "name": "look_relative",
                        "arguments": "{\"yaw_degrees\":30.0,\"pitch_degrees\":0.0}"
                    }
                },
                {
                    "id": "scripted-4",
                    "type": "function",
                    "function": {
                        "name": "move_input",
                        "arguments": "{\"directions\":[\"forward\"],\"duration_ms\":400}"
                    }
                },
                {
                    "id": "scripted-5",
                    "type": "function",
                    "function": {
                        "name": "remember",
                        "arguments": "{\"operation\":\"append\",\"text\":\"纵向实测：玩家让我看看周围，我看了、转身并走了几步。\"}"
                    }
                }
            ]
        }),
    ]
    .into_iter()
    .map(|value| match value {
        Value::Object(message) => message,
        _ => unreachable!("json! 对象字面量必为 Object"),
    })
    .collect()
}
