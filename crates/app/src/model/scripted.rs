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

/// 按 `MINEINTENT_SCRIPT` 选脚本；未设或不认识的值一律回默认纵向脚本。
///
/// 假模型的职责就是把工具面按确定顺序走一遍，不同验收场景需要不同顺序，
/// 因此选择权放在环境变量而不是再造一个 provider。
pub fn script_from_env() -> Vec<JsonObject> {
    match std::env::var("MINEINTENT_SCRIPT")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "death" | "death_recovery" => death_recovery_script(),
        _ => default_vertical_script(),
    }
}

/// 死亡恢复脚本：先 respawn，再报告自己回来了。
///
/// 覆盖 default_vertical_script 覆盖不到的第六个工具。为什么要单独一份：
/// 一次唤醒里 agent 循环会把脚本按顺序连着走完，所以「死后才调 respawn」
/// 无法靠给默认脚本追加一轮实现——死亡发生时那几轮早就用掉了。验收脚本
/// 先让怪物打死同伴，再用这份脚本唤醒它。
///
/// 判定要点是 say 必须排在 respawn 之后：死亡期间动作面只放行 respawn
/// （见 backend facade 的 ensure_bound_respawnable），所以服务端公屏上能
/// 看到这句话，本身就证明重生已经真的生效，而不只是请求已派发。
pub fn death_recovery_script() -> Vec<JsonObject> {
    [
        json!({
            "role": "assistant",
            "tool_calls": [
                {
                    "id": "death-1",
                    "type": "function",
                    "function": { "name": "respawn", "arguments": "{}" }
                }
            ]
        }),
        json!({
            "role": "assistant",
            "tool_calls": [
                {
                    "id": "death-2",
                    "type": "function",
                    "function": { "name": "say", "arguments": "{\"text\":\"我又活过来了。\"}" }
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
