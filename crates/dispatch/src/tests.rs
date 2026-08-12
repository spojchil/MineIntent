use std::sync::Mutex as StdMutex;

use agent::{RunId, ToolBatchAttemptId, ToolBatchId};
use serde_json::json;

use super::*;

/// 记录调用顺序的假供应者。
struct FakeProvider {
    name: &'static str,
    class: ToolClass,
    log: Arc<StdMutex<Vec<String>>>,
}

impl FakeProvider {
    fn new(name: &'static str, class: ToolClass, log: Arc<StdMutex<Vec<String>>>) -> Arc<Self> {
        Arc::new(Self { name, class, log })
    }
}

impl ToolProvider for FakeProvider {
    fn tools(&self) -> Vec<(ToolDefinition, ToolClass)> {
        vec![(
            ToolDefinition::new(self.name, json!({"type": "object"})),
            self.class,
        )]
    }

    fn call<'a>(&'a self, call: ToolCall) -> PortFuture<'a, ToolResult> {
        Box::pin(async move {
            self.log.lock().unwrap().push(self.name.to_owned());
            ToolResult::success_json(call.id, json!({"tool": self.name}))
        })
    }
}

struct Fixture {
    dispatcher: Dispatcher,
    log: Arc<StdMutex<Vec<String>>>,
    occupancy: Arc<Occupancy>,
}

fn fixture() -> Fixture {
    let log = Arc::new(StdMutex::new(Vec::new()));
    let occupancy = Arc::new(Occupancy::new());
    let providers: Vec<Arc<dyn ToolProvider>> = vec![
        FakeProvider::new(
            "chat_box",
            ToolClass::Body {
                domain: Domain::Screen,
            },
            log.clone(),
        ),
        FakeProvider::new(
            "go_to",
            ToolClass::Body {
                domain: Domain::Movement,
            },
            log.clone(),
        ),
        FakeProvider::new("remember", ToolClass::Free, log.clone()),
    ];
    let dispatcher = Dispatcher::new(providers, occupancy.clone()).unwrap();
    Fixture {
        dispatcher,
        log,
        occupancy,
    }
}

fn batch(calls: Vec<ToolCall>) -> ToolCallBatch {
    ToolCallBatch {
        run_id: RunId::new("run-1"),
        batch_id: ToolBatchId::new("run-1/tools/1"),
        calls,
    }
}

fn start() -> ToolBatchStart {
    ToolBatchStart {
        run_id: RunId::new("run-1"),
        batch_attempt_id: ToolBatchAttemptId::new("run-1/attempt/1"),
    }
}

fn call(id: &str, name: &str) -> ToolCall {
    ToolCall::new(id, name, json!({}))
}

fn incremental_call(slot: u32, id: &str, name: &str) -> agent::IncrementalToolCall {
    agent::IncrementalToolCall {
        run_id: RunId::new("run-1"),
        batch_attempt_id: ToolBatchAttemptId::new("run-1/attempt/1"),
        slot: ToolCallSlot::new(slot),
        call: call(id, name),
    }
}

#[tokio::test]
async fn batch_executes_in_order_and_unknown_tool_is_a_normal_failure() {
    let fixture = fixture();
    let results = fixture
        .dispatcher
        .dispatch(batch(vec![
            call("a", "remember"),
            call("b", "no_such_tool"),
            call("c", "go_to"),
        ]))
        .await
        .unwrap();

    assert_eq!(results.results.len(), 3);
    assert_eq!(results.results[0].status, agent::ToolResultStatus::Success);
    assert_eq!(results.results[1].status, agent::ToolResultStatus::Error);
    assert_eq!(results.results[2].status, agent::ToolResultStatus::Success);
    assert_eq!(*fixture.log.lock().unwrap(), vec!["remember", "go_to"]);
}

#[tokio::test]
async fn occupied_screen_suppresses_other_body_domains_but_not_free_or_screen() {
    let fixture = fixture();
    fixture.occupancy.occupy(Domain::Screen);

    let results = fixture
        .dispatcher
        .dispatch(batch(vec![
            call("a", "go_to"),
            call("b", "remember"),
            call("c", "chat_box"),
        ]))
        .await
        .unwrap();

    assert_eq!(results.results[0].status, agent::ToolResultStatus::Error);
    assert_eq!(results.results[1].status, agent::ToolResultStatus::Success);
    assert_eq!(results.results[2].status, agent::ToolResultStatus::Success);

    fixture.occupancy.release(Domain::Screen);
    let released = fixture
        .dispatcher
        .dispatch(batch(vec![call("d", "go_to")]))
        .await
        .unwrap();
    assert_eq!(released.results[0].status, agent::ToolResultStatus::Success);
}

#[tokio::test]
async fn incremental_submit_executes_immediately_and_commit_returns_in_slot_order() {
    let fixture = fixture();
    let mut run = fixture
        .dispatcher
        .begin_incremental(start())
        .await
        .unwrap()
        .expect("编排应接管增量批");

    run.submit(incremental_call(0, "a", "remember"))
        .await
        .unwrap();
    // 尚未封口、尚未 commit，第一个调用已经执行——层级 2 的核心断言。
    assert_eq!(*fixture.log.lock().unwrap(), vec!["remember"]);

    run.submit(incremental_call(1, "b", "go_to")).await.unwrap();
    run.calls_sealed(2).await.unwrap();

    let results = run.commit().await.unwrap();
    assert_eq!(results.results.len(), 2);
    assert_eq!(results.results[0].call_id.as_str(), "a");
    assert_eq!(results.results[1].call_id.as_str(), "b");
}

#[tokio::test]
async fn abort_reports_every_submitted_call_as_settled_and_leaves_occupancy_untouched() {
    let fixture = fixture();
    // 屏在上一批就开着；本批中断不得动它——回执已把执行事实告知模型。
    fixture.occupancy.occupy(Domain::Screen);

    let mut run = fixture
        .dispatcher
        .begin_incremental(start())
        .await
        .unwrap()
        .unwrap();
    run.submit(incremental_call(0, "a", "chat_box"))
        .await
        .unwrap();

    let report = run
        .abort(ToolBatchAbortReason::ModelStreamInterrupted)
        .await
        .unwrap();

    assert_eq!(report.batch_attempt_id.as_str(), "run-1/attempt/1");
    assert_eq!(report.calls.len(), 1);
    assert!(matches!(
        report.calls[0].outcome,
        AbortedToolCallOutcome::Settled(_)
    ));
    assert!(fixture.occupancy.is_occupied(Domain::Screen));
}

#[test]
fn duplicate_tool_names_fail_at_registration() {
    let log = Arc::new(StdMutex::new(Vec::new()));
    let first = FakeProvider::new("chat_box", ToolClass::Free, log.clone());
    let second = FakeProvider::new(
        "chat_box",
        ToolClass::Body {
            domain: Domain::Screen,
        },
        log,
    );
    let error = Dispatcher::new(vec![first, second], Arc::new(Occupancy::new()))
        .err()
        .expect("重名注册必须失败");
    assert!(error.summary.contains("chat_box"));
}

#[test]
fn definitions_concatenate_all_providers() {
    let fixture = fixture();
    let names: Vec<_> = fixture
        .dispatcher
        .definitions()
        .into_iter()
        .map(|definition| definition.name.into_inner())
        .collect();
    assert_eq!(names, vec!["chat_box", "go_to", "remember"]);
}
