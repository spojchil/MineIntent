//! 任务表：模型看一眼「我现在有什么在跑」。
//!
//! # 为什么要有它
//!
//! 工具描述里让模型「别急着重发，完成会通知你」——**要求它不重发，就得给它
//! 查看的手段**，否则它只能靠重发来试探。`pillar_up` 实测 83 次调用净上升 1 格
//! 就是这么来的：它收不到终局，又没处查，只能一遍遍试。
//!
//! 它同时是「必有终局」这条不变量的旁证：某个任务在表里挂着不动，就是看门狗
//! 该发终局而没发——这种失败此前只能靠猜。
//!
//! # 为什么是 Free 类
//!
//! 只读、不碰身体、不占互斥域。死亡期间照样能看（看得见世界是原版事实）。

use std::sync::Arc;

use agent::{PortFuture, ToolCall, ToolDefinition, ToolResult};
use dispatch::{ToolClass, ToolProvider};
use serde_json::{json, Value};
use world::{JobStatus, JobStatusKind};

/// 接入模块只读口的窄化。
pub trait JobsDoor: Send + Sync {
    /// 全部在途任务；空表 = 什么都没在跑。
    fn in_flight<'a>(&'a self) -> PortFuture<'a, Vec<JobStatus>>;
}

const TOOL_NAME: &str = "jobs";

pub struct JobsTools {
    door: Arc<dyn JobsDoor>,
}

impl JobsTools {
    pub fn new(door: Arc<dyn JobsDoor>) -> Self {
        Self { door }
    }

    async fn dispatch(&self, call: ToolCall) -> ToolResult {
        let call_id = call.id.clone();
        let Some(arguments) = call.arguments.as_object() else {
            return ToolResult::failure(call_id, "参数必须是 JSON 对象；请改写调用");
        };
        match arguments.get("action").and_then(Value::as_str) {
            Some("list") => {
                let jobs = self.door.in_flight().await;
                ToolResult::success_json(
                    call_id,
                    json!({ "jobs": jobs.iter().map(describe).collect::<Vec<_>>() }),
                )
            }
            Some(other) => ToolResult::failure(
                call_id,
                format!("jobs 没有 {other} 这个动作；当前只有 list"),
            ),
            None => ToolResult::failure(call_id, "jobs 需要字符串参数 action；请改写调用"),
        }
    }
}

/// 一条任务的只读描述。**不替模型判断「卡住了没有」**——给出在途时长，
/// 它自己看。机器下这个结论就是把推测讲成事实。
fn describe(status: &JobStatus) -> Value {
    let common = json!({
        "id": status.id.0,
        "已在途游戏刻": status.elapsed_ticks,
    });
    let mut value = common;
    let object = value.as_object_mut().expect("上面就是对象");
    match &status.kind {
        JobStatusKind::Move { destination, leg } => {
            object.insert("做什么".to_owned(), json!("走路"));
            object.insert("目的地".to_owned(), json!(destination));
            if let Some(leg) = leg {
                object.insert("这一程走到".to_owned(), json!(leg));
            }
        }
        JobStatusKind::Mine {
            targets,
            done,
            current,
        } => {
            object.insert("做什么".to_owned(), json!("挖掘"));
            object.insert("已挖".to_owned(), json!(done));
            object.insert("总数".to_owned(), json!(targets.len()));
            if let Some(current) = current {
                object.insert("正在挖".to_owned(), json!(current));
            }
        }
    }
    value
}

impl ToolProvider for JobsTools {
    fn tools(&self) -> Vec<(ToolDefinition, ToolClass)> {
        let mut definition = ToolDefinition::new(
            TOOL_NAME,
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list"],
                        "description": "list=列出现在有哪些任务在跑"
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        );
        definition.description = Some(
            "看一眼你现在有什么在后台跑（走路、挖掘）。**不要轮询**：任务做完、\
放弃或卡住都会主动通知你，这个动作只在你确实拿不准时用一次。\
表里给的是在途时长，久不久由你自己判断——机器不替你下「卡住了」这个结论。"
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
    use std::sync::Mutex;

    use agent::{ContentPart, ToolResultStatus};
    use world::JobId;

    use super::*;

    struct FakeDoor(Mutex<Vec<JobStatus>>);

    impl JobsDoor for FakeDoor {
        fn in_flight<'a>(&'a self) -> PortFuture<'a, Vec<JobStatus>> {
            Box::pin(async move { self.0.lock().expect("锁").clone() })
        }
    }

    fn tools(jobs: Vec<JobStatus>) -> JobsTools {
        JobsTools::new(Arc::new(FakeDoor(Mutex::new(jobs))))
    }

    fn call(arguments: Value) -> ToolCall {
        ToolCall::new("call-1", TOOL_NAME, arguments)
    }

    fn json_of(result: &ToolResult) -> Value {
        match &result.content[0] {
            ContentPart::Json { value } => value.clone(),
            other => panic!("期望 JSON，得到 {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_table_is_not_an_error() {
        let result = tools(vec![]).call(call(json!({"action": "list"}))).await;
        assert_eq!(result.status, ToolResultStatus::Success);
        assert_eq!(json_of(&result)["jobs"].as_array().expect("数组").len(), 0);
    }

    #[tokio::test]
    async fn both_verbs_appear_with_their_own_shape() {
        let jobs = vec![
            JobStatus {
                id: JobId(1),
                kind: JobStatusKind::Move {
                    destination: [10, 64, -3],
                    leg: Some([5, 64, -1]),
                },
                started_tick: 0,
                elapsed_ticks: 40,
            },
            JobStatus {
                id: JobId(2),
                kind: JobStatusKind::Mine {
                    targets: vec![[1, 2, 3], [1, 3, 3]],
                    done: 1,
                    current: Some([1, 3, 3]),
                },
                started_tick: 10,
                elapsed_ticks: 30,
            },
        ];
        let result = tools(jobs).call(call(json!({"action": "list"}))).await;
        let listed = json_of(&result);
        let listed = listed["jobs"].as_array().expect("数组");
        assert_eq!(listed[0]["做什么"], "走路");
        assert_eq!(listed[0]["id"], 1);
        assert_eq!(listed[1]["做什么"], "挖掘");
        assert_eq!(listed[1]["已挖"], 1);
    }

    /// 机器只给在途时长，不替模型判断「卡住了」——那是推测，不是事实。
    #[tokio::test]
    async fn the_table_states_elapsed_time_and_does_not_conclude_stuck() {
        let jobs = vec![JobStatus {
            id: JobId(9),
            kind: JobStatusKind::Move {
                destination: [0, 0, 0],
                leg: None,
            },
            started_tick: 0,
            elapsed_ticks: 9_999,
        }];
        let result = tools(jobs).call(call(json!({"action": "list"}))).await;
        let text = serde_json::to_string(&json_of(&result)).expect("序列化");
        assert!(text.contains("9999"), "{text}");
        assert!(!text.contains("卡住"), "机器不该下这个结论：{text}");
    }

    #[test]
    fn registers_as_free_so_it_reads_without_occupying_anything() {
        let registered = ToolProvider::tools(&tools(vec![]));
        assert_eq!(registered.len(), 1);
        assert_eq!(registered[0].0.name.as_str(), TOOL_NAME);
        assert_eq!(registered[0].1, ToolClass::Free);
    }
}
