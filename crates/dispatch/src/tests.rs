use std::sync::Mutex as StdMutex;

use agent::{RunId, ToolBatchAttemptId, ToolBatchId, ToolCallSlot};
use serde_json::json;

use super::*;

/// 记录 settled 上报的假 reporter：新契约下结果只从这里出。
#[derive(Default)]
struct RecordingReporter {
    settled: StdMutex<Vec<(ToolCallSlot, ToolResult)>>,
}

impl ToolCallReporter for RecordingReporter {
    fn settled<'a>(
        &'a self,
        slot: ToolCallSlot,
        result: ToolResult,
    ) -> PortFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            self.settled.lock().unwrap().push((slot, result));
            Ok(())
        })
    }
}

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

/// 可翻转的生命闸门：测死亡压制用。
struct SwitchableLife(std::sync::atomic::AtomicBool);

impl SwitchableLife {
    fn alive() -> Arc<Self> {
        Arc::new(Self(std::sync::atomic::AtomicBool::new(true)))
    }

    fn kill(&self) {
        self.0.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

impl LifeGate for SwitchableLife {
    fn alive(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

fn fixture() -> Fixture {
    fixture_with_life(Arc::new(AlwaysAlive)).0
}

fn fixture_with_life(life: Arc<dyn LifeGate>) -> (Fixture, Arc<StdMutex<Vec<String>>>) {
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
        FakeProvider::new("presence", ToolClass::Vital, log.clone()),
    ];
    let dispatcher = Dispatcher::new(providers, occupancy.clone(), life).unwrap();
    (
        Fixture {
            dispatcher,
            log: log.clone(),
            occupancy,
        },
        log,
    )
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
async fn incremental_submit_executes_immediately_and_settles_through_the_reporter() {
    let fixture = fixture();
    let reporter = Arc::new(RecordingReporter::default());
    let mut run = fixture
        .dispatcher
        .begin_incremental(start(), reporter.clone())
        .await
        .unwrap()
        .expect("编排应接管增量批");

    run.submit(incremental_call(0, "a", "remember"))
        .await
        .unwrap();
    // 尚未封口、尚未 commit，第一个调用已经执行且已上报——层级 2 的核心断言。
    assert_eq!(*fixture.log.lock().unwrap(), vec!["remember"]);
    assert_eq!(reporter.settled.lock().unwrap().len(), 1);

    run.submit(incremental_call(1, "b", "go_to")).await.unwrap();
    run.calls_sealed(2).await.unwrap();
    run.commit().await.unwrap();

    let settled = reporter.settled.lock().unwrap();
    assert_eq!(settled.len(), 2);
    assert_eq!(settled[0].0, ToolCallSlot::new(0));
    assert_eq!(settled[0].1.call_id.as_str(), "a");
    assert_eq!(settled[1].0, ToolCallSlot::new(1));
    assert_eq!(settled[1].1.call_id.as_str(), "b");
}

#[tokio::test]
async fn abort_reports_no_pending_slots_and_leaves_occupancy_untouched() {
    let fixture = fixture();
    // 屏在上一批就开着；本批中断不得动它——settled 上报已把执行事实告知内核。
    fixture.occupancy.occupy(Domain::Screen);

    let reporter = Arc::new(RecordingReporter::default());
    let mut run = fixture
        .dispatcher
        .begin_incremental(start(), reporter.clone())
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

    // 执行同步于 submit：凡提交必已 settled，中止报告里不存在悬而未决的槽。
    assert!(report.slots.is_empty());
    assert_eq!(reporter.settled.lock().unwrap().len(), 1);
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
    let error = Dispatcher::new(
        vec![first, second],
        Arc::new(Occupancy::new()),
        Arc::new(AlwaysAlive),
    )
    .err()
    .expect("重名注册必须失败");
    assert!(error.summary.contains("chat_box"));
}

/// 死亡拦身体类、放行感知与内心。口径对齐原版：死人看得见世界
/// （死亡屏不暂停），但开不了口（聊天键只在无屏时处理）。
#[tokio::test]
async fn death_blocks_body_tools_but_not_perception_or_memory() {
    let life = SwitchableLife::alive();
    let (fixture, log) = fixture_with_life(life.clone());
    life.kill();

    let results = fixture
        .dispatcher
        .dispatch(batch(vec![
            call("a", "go_to"),
            call("b", "chat_box"),
            call("c", "remember"),
        ]))
        .await
        .unwrap();

    assert_eq!(results.results[0].status, agent::ToolResultStatus::Error);
    assert_eq!(results.results[1].status, agent::ToolResultStatus::Error);
    assert_eq!(results.results[2].status, agent::ToolResultStatus::Success);
    // 被拦的两件根本没到供应者手里——拒绝不是"执行了但失败"。
    assert_eq!(log.lock().unwrap().as_slice(), ["remember"]);
}

/// Vital 类不受生命闸门压制。归错类的话，死了就没有出路了。
#[tokio::test]
async fn death_still_lets_vital_tools_through() {
    let life = SwitchableLife::alive();
    let (fixture, log) = fixture_with_life(life.clone());
    life.kill();

    let results = fixture
        .dispatcher
        .dispatch(batch(vec![call("a", "presence")]))
        .await
        .unwrap();

    assert_eq!(results.results[0].status, agent::ToolResultStatus::Success);
    assert_eq!(log.lock().unwrap().as_slice(), ["presence"]);
}

/// 生命闸门先于界面压制：死了连屏都开不了，报"有界面开着"是答非所问。
#[tokio::test]
async fn death_message_takes_precedence_over_screen_suppression() {
    let life = SwitchableLife::alive();
    let (fixture, _log) = fixture_with_life(life.clone());
    fixture.occupancy.occupy(Domain::Screen);
    life.kill();

    let results = fixture
        .dispatcher
        .dispatch(batch(vec![call("a", "go_to")]))
        .await
        .unwrap();

    let text: String = results.results[0]
        .content
        .iter()
        .filter_map(|part| match part {
            agent::ContentPart::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(text.contains("已经死亡"), "{text}");
    assert!(!text.contains("界面开着"), "{text}");
}

/// 活着时闸门完全不出现在路径上——不能因为加了闸门就改变原有行为。
#[tokio::test]
async fn alive_leaves_every_class_untouched() {
    let (fixture, log) = fixture_with_life(SwitchableLife::alive());
    let results = fixture
        .dispatcher
        .dispatch(batch(vec![
            call("a", "go_to"),
            call("b", "remember"),
            call("c", "presence"),
        ]))
        .await
        .unwrap();

    assert!(results
        .results
        .iter()
        .all(|result| result.status == agent::ToolResultStatus::Success));
    assert_eq!(
        log.lock().unwrap().as_slice(),
        ["go_to", "remember", "presence"]
    );
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
    assert_eq!(names, vec!["chat_box", "go_to", "remember", "presence"]);
}
