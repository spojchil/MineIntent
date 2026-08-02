use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use mineintent_contracts::{
    agent::{
        AgentError, AgentErrorCode, CancellationSignal as AgentCancellationSignal, Deadline,
        ExecutionControl, RunId, ToolCallId,
    },
    capability::{
        view_parameters_schema, CapabilityExecutionContext, CapabilityInvocation, ScopeGuard,
        ToolCapability,
    },
    minecraft::{
        BackendError, BackendEventListener, BackendState, BlockPosition, BoxFuture,
        DirectedViewportError, DirectedViewportProjection, FactSource, MinecraftBackendApi,
        MinecraftMotorDriverApi, MinecraftSnapshotV1, ObservationEventListener, OperationControl,
        ProtocolEntitySnapshot, ProtocolObservationSource, SelfPose, Subscription, ViewportFrame,
        ViewportLegend, ViewportProjection, ViewportRead, ViewportSelfPose, VisibleBlocksView,
        VisibleEntitiesView,
    },
};
use mineintent_middle::{
    agent::{BackendRoundViewportSampler, FixedUtcTimestampSource, RoundViewportSampler},
    capability::{ViewCapability, ViewportReader},
};
use serde_json::{json, Value};
use tokio::sync::Notify;

const TIMEOUT: Duration = Duration::from_secs(1);

#[tokio::test]
async fn view_definition_schema_resource_and_description_are_frozen() {
    let backend = Arc::new(FakeBackend::new());
    let reader = Arc::new(ViewportReader::new(backend));
    let capability = ViewCapability::new(reader);
    let definition = capability.definition();

    assert_eq!(definition.function.name.as_str(), "view");
    assert_eq!(
        capability.resource(),
        Some(mineintent_contracts::capability::ExecutionResource::Viewport)
    );
    assert_eq!(definition.function.parameters, view_parameters_schema());
    assert!(definition.function.description.contains("可能因预算截断"));
    assert!(definition
        .function
        .description
        .contains("未列出不代表不可见"));
    assert!(definition.function.description.contains("directed"));
    assert!(definition.function.description.contains("不可见时不泄露"));
}

#[tokio::test]
async fn invalid_arguments_fail_without_binding_source_or_reading() {
    let backend = Arc::new(FakeBackend::new());
    let source = Arc::clone(&backend.source);
    let reader = Arc::new(ViewportReader::new(backend.clone()));
    let capability = ViewCapability::new(reader);
    let cancellation = TestCancellation::new();
    let scope = TestScope::current();

    for arguments in [
        json!({}),
        json!({"mode":"future"}),
        json!({"mode":"full","positions":[]}),
        json!({"mode":"directed"}),
        json!({"mode":"directed","positions":null}),
        json!({"mode":"directed","positions":[]}),
        json!({"mode":"directed","positions":[[0,64,0],[0,64,0]]}),
        json!({"mode":"directed","positions":[[0,64,0]],"unknown":true}),
    ] {
        let result = capability
            .execute(
                invocation(arguments),
                context(&cancellation, &scope, Duration::from_secs(5)),
            )
            .await
            .expect("invalid arguments are ordinary tool failures");
        assert_eq!(result["protocol"], json!("mineintent.tool-result.v1"));
        assert_eq!(result["status"], json!("failed"));
        assert!(result["summary"].as_str().unwrap().chars().count() <= 300);
    }

    assert_eq!(backend.source_calls(), 0);
    assert_eq!(source.full_reads(), 0);
    assert_eq!(source.directed_reads(), 0);
}

#[tokio::test]
async fn full_and_directed_bind_one_source_and_call_one_atomic_read() {
    let backend = Arc::new(FakeBackend::new());
    let source = Arc::clone(&backend.source);
    let reader = Arc::new(ViewportReader::new(backend.clone()));
    let capability = ViewCapability::new(reader);
    let cancellation = TestCancellation::new();
    let scope = TestScope::current();

    let full = capability
        .execute(
            invocation(json!({"mode":"full"})),
            context(&cancellation, &scope, Duration::from_secs(5)),
        )
        .await
        .expect("full view completes");
    assert_eq!(full["protocol"], json!("mineintent.tool-result.v1"));
    assert_eq!(full["status"], json!("completed"));
    assert!(full["viewport"].get("source").is_none());
    assert!(full["viewport"].get("revision").is_none());
    assert_eq!(source.full_reads(), 1);
    assert_eq!(source.directed_reads(), 0);

    let positions = vec![[124, 65, -37], [10, 64, 3]];
    let directed = capability
        .execute(
            invocation(json!({
                "mode":"directed",
                "positions": positions,
            })),
            context(&cancellation, &scope, Duration::from_secs(5)),
        )
        .await
        .expect("directed view completes");
    assert_eq!(directed["protocol"], json!("mineintent.tool-result.v1"));
    assert_eq!(directed["status"], json!("completed"));
    assert_eq!(directed["viewport"]["seen"][0]["at"], json!([124, 65, -37]));
    assert!(directed["viewport"].get("source").is_none());
    assert!(directed["viewport"].get("revision").is_none());
    assert_eq!(source.full_reads(), 1);
    assert_eq!(source.directed_reads(), 1);
    assert_eq!(
        source.last_directed_positions(),
        vec![
            BlockPosition {
                x: 124,
                y: 65,
                z: -37
            },
            BlockPosition { x: 10, y: 64, z: 3 },
        ]
    );
    assert_eq!(backend.source_calls(), 2);
}

#[tokio::test]
async fn ordinary_backend_and_source_failures_are_failed_results_with_bounded_summary() {
    let backend = Arc::new(FakeBackend::new());
    backend.set_source_error(Some(BackendError::BackendFailure {
        failure: mineintent_contracts::minecraft::BackendFailure {
            code: mineintent_contracts::minecraft::BackendFailureCode::ProtocolError,
            message: "x".repeat(500),
            retryable: false,
        },
    }));
    let reader = Arc::new(ViewportReader::new(backend.clone()));
    let capability = ViewCapability::new(reader);
    let cancellation = TestCancellation::new();
    let scope = TestScope::current();

    let source_failure = capability
        .execute(
            invocation(json!({"mode":"full"})),
            context(&cancellation, &scope, Duration::from_secs(5)),
        )
        .await
        .expect("ordinary source failure is a failed tool result");
    assert_eq!(source_failure["status"], json!("failed"));
    assert!(source_failure["summary"].as_str().unwrap().chars().count() <= 300);

    backend.set_source_error(None);
    backend.source.set_full_mode(ReadMode::OrdinaryFailure);
    let read_failure = capability
        .execute(
            invocation(json!({"mode":"full"})),
            context(&cancellation, &scope, Duration::from_secs(5)),
        )
        .await
        .expect("ordinary read failure is a failed tool result");
    assert_eq!(read_failure["status"], json!("failed"));
    assert!(read_failure["summary"].as_str().unwrap().chars().count() <= 300);
}

#[tokio::test]
async fn reader_control_wins_when_source_binding_fails_during_cancellation() {
    let backend = Arc::new(FakeBackend::new());
    let cancellation = Arc::new(TestCancellation::new());
    backend.cancel_agent_then_fail_source(Arc::clone(&cancellation));
    let reader = ViewportReader::new(backend.clone());
    let control = ExecutionControl::new(
        cancellation.as_ref(),
        Deadline::after(Instant::now(), Duration::from_secs(5)).unwrap(),
    );

    let error = reader
        .read_full(control)
        .await
        .expect_err("cancellation must win a simultaneous source-binding error");

    assert_eq!(error.code, AgentErrorCode::RunCancelled);
    assert_eq!(backend.source_calls(), 1);
    assert_eq!(backend.source.full_reads(), 0);
}

#[tokio::test]
async fn pre_cancelled_or_expired_control_does_not_start_backend_read() {
    let backend = Arc::new(FakeBackend::new());
    let source = Arc::clone(&backend.source);
    let reader = Arc::new(ViewportReader::new(backend.clone()));
    let capability = ViewCapability::new(reader);
    let scope = TestScope::current();

    let cancelled = TestCancellation::new();
    cancelled.trigger();
    let cancellation_error = capability
        .execute(
            invocation(json!({"mode":"full"})),
            context(&cancelled, &scope, Duration::from_secs(5)),
        )
        .await
        .expect_err("cancelled view must bubble AgentError");
    assert_eq!(cancellation_error.code, AgentErrorCode::RunCancelled);
    assert_eq!(backend.source_calls(), 0);
    assert_eq!(source.full_reads(), 0);

    let active = TestCancellation::new();
    let expired = ExecutionControl::new(
        &active,
        Deadline::at(
            Instant::now()
                .checked_sub(Duration::from_millis(1))
                .unwrap(),
        ),
    );
    let deadline_error = capability
        .execute(
            invocation(json!({"mode":"full"})),
            CapabilityExecutionContext::new("world", "chat", expired, &scope),
        )
        .await
        .expect_err("expired view must bubble AgentError");
    assert_eq!(deadline_error.code, AgentErrorCode::DeadlineExceeded);
    assert_eq!(backend.source_calls(), 0);
    assert_eq!(source.full_reads(), 0);
}

#[tokio::test]
async fn backend_cancel_and_deadline_map_to_structured_agent_errors() {
    let backend = Arc::new(FakeBackend::new());
    let reader = Arc::new(ViewportReader::new(backend.clone()));
    let capability = ViewCapability::new(reader);
    let scope = TestScope::current();
    let cancellation = TestCancellation::new();

    backend.source.set_full_mode(ReadMode::BackendCancelled);
    let cancelled = capability
        .execute(
            invocation(json!({"mode":"full"})),
            context(&cancellation, &scope, Duration::from_secs(5)),
        )
        .await
        .expect_err("backend cancellation must not be disguised as a tool result");
    assert_eq!(cancelled.code, AgentErrorCode::RunCancelled);

    backend.source.set_full_mode(ReadMode::BackendDeadline);
    let deadline = capability
        .execute(
            invocation(json!({"mode":"full"})),
            context(&cancellation, &scope, Duration::from_secs(5)),
        )
        .await
        .expect_err("backend deadline must not be disguised as a tool result");
    assert_eq!(deadline.code, AgentErrorCode::DeadlineExceeded);
}

#[tokio::test]
async fn reader_control_wins_when_backend_completion_makes_cancellation_ready() {
    let backend = Arc::new(FakeBackend::new());
    let source = Arc::clone(&backend.source);
    let reader = ViewportReader::new(backend);
    let cancellation = Arc::new(TestCancellation::new());
    source.cancel_agent_then_fail_on_full(Arc::clone(&cancellation));
    let control = ExecutionControl::new(
        cancellation.as_ref(),
        Deadline::after(Instant::now(), Duration::from_secs(5)).unwrap(),
    );

    let error = reader
        .read_full(control)
        .await
        .expect_err("cancellation becoming ready with a backend error must win");

    assert_eq!(error.code, AgentErrorCode::RunCancelled);
    assert_eq!(source.full_reads(), 1);
}

#[tokio::test]
async fn reader_relays_cancellation_arriving_during_source_read_creation() {
    let backend = Arc::new(FakeBackend::new());
    let source = Arc::clone(&backend.source);
    let reader = ViewportReader::new(backend);
    let cancellation = Arc::new(TestCancellation::new());
    source.cancel_agent_before_future_on_full(Arc::clone(&cancellation));
    let control = ExecutionControl::new(
        cancellation.as_ref(),
        Deadline::after(Instant::now(), Duration::from_secs(5)).unwrap(),
    );

    let error = reader
        .read_full(control)
        .await
        .expect_err("cancellation during source read creation must bubble");

    assert_eq!(error.code, AgentErrorCode::RunCancelled);
    assert!(source
        .last_full_control()
        .expect("source captured its backend control")
        .cancellation()
        .is_cancelled());
}

#[tokio::test]
async fn in_flight_cancel_and_deadline_wake_backend_read_with_bounded_completion() {
    let backend = Arc::new(FakeBackend::new());
    let source = Arc::clone(&backend.source);
    source.set_full_mode(ReadMode::Block);
    let reader = Arc::new(ViewportReader::new(backend.clone()));
    let capability = ViewCapability::new(reader);
    let scope = TestScope::current();

    let cancellation = TestCancellation::new();
    let mut future = Box::pin(capability.execute(
        invocation(json!({"mode":"full"})),
        context(&cancellation, &scope, Duration::from_secs(5)),
    ));
    tokio::time::timeout(TIMEOUT, async {
        tokio::select! {
            _ = source.full_started.notified() => {}
            result = &mut future => panic!("full read completed before cancellation: {result:?}"),
        }
    })
    .await
    .expect("full backend read started");
    cancellation.trigger();
    let cancelled = tokio::time::timeout(TIMEOUT, &mut future)
        .await
        .expect("cancellation must wake the blocked read")
        .expect_err("cancellation must bubble");
    assert_eq!(cancelled.code, AgentErrorCode::RunCancelled);
    assert!(source.backend_cancelled.load(Ordering::SeqCst));

    source.set_full_mode(ReadMode::Block);
    let active = TestCancellation::new();
    let deadline = ExecutionControl::new(
        &active,
        Deadline::after(Instant::now(), Duration::from_millis(40)).unwrap(),
    );
    let mut future = Box::pin(capability.execute(
        invocation(json!({"mode":"full"})),
        CapabilityExecutionContext::new("world", "chat", deadline, &scope),
    ));
    tokio::time::timeout(TIMEOUT, async {
        tokio::select! {
            _ = source.full_started.notified() => {}
            result = &mut future => panic!("full read completed before deadline: {result:?}"),
        }
    })
    .await
    .expect("second full backend read started");
    let deadline_error = tokio::time::timeout(TIMEOUT, &mut future)
        .await
        .expect("deadline must wake the blocked read")
        .expect_err("deadline must bubble");
    assert_eq!(deadline_error.code, AgentErrorCode::DeadlineExceeded);
    assert!(source.backend_deadline.load(Ordering::SeqCst));
}

#[tokio::test]
async fn scope_is_checked_after_read_and_sampler_uses_only_projection_and_injected_utc() {
    let backend = Arc::new(FakeBackend::new());
    let source = Arc::clone(&backend.source);
    source.set_full_mode(ReadMode::Block);
    let reader = Arc::new(ViewportReader::new(backend.clone()));
    let capability = ViewCapability::new(Arc::clone(&reader));
    let cancellation = TestCancellation::new();
    let scope = TestScope::current();

    let mut capability_future = Box::pin(capability.execute(
        invocation(json!({"mode":"full"})),
        context(&cancellation, &scope, Duration::from_secs(5)),
    ));
    tokio::time::timeout(TIMEOUT, async {
        tokio::select! {
            _ = source.full_started.notified() => {}
            result = &mut capability_future => panic!("scope test read completed early: {result:?}"),
        }
    })
    .await
    .expect("scope test read started");
    scope.invalidate();
    source.release.notify_waiters();
    let scope_error = tokio::time::timeout(TIMEOUT, &mut capability_future)
        .await
        .expect("scope invalidation must finish")
        .expect_err("stale projection must not be published");
    assert_eq!(scope_error.code, AgentErrorCode::ScopeInvalid);

    source.set_full_mode(ReadMode::Success);
    let scope = TestScope::current();
    let sampler = BackendRoundViewportSampler::with_timestamp_source(
        Arc::clone(&reader),
        Arc::new(FixedUtcTimestampSource::new("2026-08-03T00:00:00Z")),
    );
    assert_eq!(sampler.timestamp(), "2026-08-03T00:00:00Z");
    let sampler_result = sampler
        .sample(context(&cancellation, &scope, Duration::from_secs(5)).control())
        .await
        .expect("sampler full read succeeds");
    let serialized = serde_json::to_value(&sampler_result).unwrap();
    assert!(serialized.get("source").is_none());
    assert!(serialized.get("revision").is_none());
    let frame =
        mineintent_contracts::agent::ViewportFrameMessage::success(sampler.timestamp(), serialized)
            .expect("projection can be placed in the frozen frame envelope");
    let frame_wire = serde_json::to_value(frame).unwrap();
    assert_eq!(
        frame_wire["protocol"],
        json!("mineintent.viewport-frame.v1")
    );
    assert_eq!(frame_wire["at"], json!("2026-08-03T00:00:00Z"));
    assert!(frame_wire.get("source").is_none());
    assert!(frame_wire.get("revision").is_none());
    assert_eq!(source.full_reads(), 2);
    assert_eq!(backend.source_calls(), 2);
}

fn invocation(arguments: Value) -> CapabilityInvocation {
    CapabilityInvocation {
        run_id: RunId::new("run-1").unwrap(),
        tool_call_id: ToolCallId::new("call-1").unwrap(),
        arguments: arguments.as_object().cloned().unwrap(),
        action_id: "action-1".to_owned(),
        started_at: "2026-08-03T00:00:00Z".to_owned(),
    }
}

fn context<'a>(
    cancellation: &'a TestCancellation,
    scope: &'a TestScope,
    duration: Duration,
) -> CapabilityExecutionContext<'a> {
    CapabilityExecutionContext::new(
        "world",
        "chat",
        ExecutionControl::new(
            cancellation,
            Deadline::after(Instant::now(), duration).unwrap(),
        ),
        scope,
    )
}

struct TestCancellation {
    cancelled: AtomicBool,
    notify: Notify,
}

impl TestCancellation {
    fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    fn trigger(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
        self.notify.notify_one();
    }
}

impl AgentCancellationSignal for TestCancellation {
    fn cancellation_error(&self) -> Option<AgentError> {
        self.cancelled
            .load(Ordering::SeqCst)
            .then(AgentError::run_cancelled)
    }

    fn cancelled(&self) -> std::pin::Pin<Box<dyn Future<Output = AgentError> + Send + '_>> {
        Box::pin(async move {
            loop {
                if let Some(error) = self.cancellation_error() {
                    return error;
                }
                self.notify.notified().await;
            }
        })
    }
}

struct TestScope {
    current: AtomicBool,
}

impl TestScope {
    fn current() -> Self {
        Self {
            current: AtomicBool::new(true),
        }
    }

    fn invalidate(&self) {
        self.current.store(false, Ordering::SeqCst);
    }
}

impl ScopeGuard for TestScope {
    fn check_current(&self) -> Result<(), AgentError> {
        if self.is_current() {
            Ok(())
        } else {
            Err(AgentError::new(
                AgentErrorCode::ScopeInvalid,
                "scope_invalid",
            ))
        }
    }

    fn is_current(&self) -> bool {
        self.current.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Copy)]
enum ReadMode {
    Success,
    OrdinaryFailure,
    BackendCancelled,
    BackendDeadline,
    Block,
    CancelAgentThenFail,
    CancelAgentBeforeFuture,
}

impl ReadMode {
    fn as_u8(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::OrdinaryFailure => 1,
            Self::BackendCancelled => 2,
            Self::BackendDeadline => 3,
            Self::Block => 4,
            Self::CancelAgentThenFail => 5,
            Self::CancelAgentBeforeFuture => 6,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::OrdinaryFailure,
            2 => Self::BackendCancelled,
            3 => Self::BackendDeadline,
            4 => Self::Block,
            5 => Self::CancelAgentThenFail,
            6 => Self::CancelAgentBeforeFuture,
            _ => Self::Success,
        }
    }
}

struct FakeSource {
    full_mode: AtomicU8,
    directed_mode: AtomicU8,
    full_reads: AtomicUsize,
    directed_reads: AtomicUsize,
    last_directed: Mutex<Vec<BlockPosition>>,
    full_started: Notify,
    release: Notify,
    backend_cancelled: AtomicBool,
    backend_deadline: AtomicBool,
    cancel_agent_on_full: Mutex<Option<Arc<TestCancellation>>>,
    last_full_control: Mutex<Option<OperationControl>>,
}

impl FakeSource {
    fn new() -> Self {
        Self {
            full_mode: AtomicU8::new(ReadMode::Success.as_u8()),
            directed_mode: AtomicU8::new(ReadMode::Success.as_u8()),
            full_reads: AtomicUsize::new(0),
            directed_reads: AtomicUsize::new(0),
            last_directed: Mutex::new(Vec::new()),
            full_started: Notify::new(),
            release: Notify::new(),
            backend_cancelled: AtomicBool::new(false),
            backend_deadline: AtomicBool::new(false),
            cancel_agent_on_full: Mutex::new(None),
            last_full_control: Mutex::new(None),
        }
    }

    fn set_full_mode(&self, mode: ReadMode) {
        self.full_mode.store(mode.as_u8(), Ordering::SeqCst);
    }

    fn cancel_agent_then_fail_on_full(&self, cancellation: Arc<TestCancellation>) {
        *self.cancel_agent_on_full.lock().unwrap() = Some(cancellation);
        self.set_full_mode(ReadMode::CancelAgentThenFail);
    }

    fn cancel_agent_before_future_on_full(&self, cancellation: Arc<TestCancellation>) {
        *self.cancel_agent_on_full.lock().unwrap() = Some(cancellation);
        self.set_full_mode(ReadMode::CancelAgentBeforeFuture);
    }

    fn last_full_control(&self) -> Option<OperationControl> {
        self.last_full_control.lock().unwrap().clone()
    }

    fn full_reads(&self) -> usize {
        self.full_reads.load(Ordering::SeqCst)
    }

    fn directed_reads(&self) -> usize {
        self.directed_reads.load(Ordering::SeqCst)
    }

    fn last_directed_positions(&self) -> Vec<BlockPosition> {
        self.last_directed.lock().unwrap().clone()
    }

    async fn wait_or_return(
        &self,
        control: OperationControl,
        mode: ReadMode,
        operation: &'static str,
    ) -> Result<(), BackendError> {
        control.preflight(operation)?;
        match mode {
            ReadMode::Block => {
                self.full_started.notify_one();
                let cancellation = control.cancelled();
                let deadline = control.deadline_elapsed().unwrap();
                tokio::pin!(cancellation);
                tokio::pin!(deadline);
                tokio::select! {
                    _ = self.release.notified() => control.preflight(operation),
                    _ = &mut cancellation => {
                        self.backend_cancelled.store(true, Ordering::SeqCst);
                        Err(BackendError::Cancelled { operation: operation.to_owned() })
                    }
                    _ = &mut deadline => {
                        self.backend_deadline.store(true, Ordering::SeqCst);
                        Err(BackendError::DeadlineExceeded { operation: operation.to_owned() })
                    }
                }
            }
            ReadMode::Success => Ok(()),
            ReadMode::OrdinaryFailure => Err(BackendError::BackendFailure {
                failure: mineintent_contracts::minecraft::BackendFailure {
                    code: mineintent_contracts::minecraft::BackendFailureCode::ProtocolError,
                    message: "ordinary backend failure".to_owned(),
                    retryable: false,
                },
            }),
            ReadMode::BackendCancelled => Err(BackendError::Cancelled {
                operation: operation.to_owned(),
            }),
            ReadMode::BackendDeadline => Err(BackendError::DeadlineExceeded {
                operation: operation.to_owned(),
            }),
            ReadMode::CancelAgentThenFail => {
                let cancellation = self
                    .cancel_agent_on_full
                    .lock()
                    .unwrap()
                    .take()
                    .expect("cancel-then-fail mode has an agent signal");
                cancellation.trigger();
                Err(BackendError::BackendFailure {
                    failure: mineintent_contracts::minecraft::BackendFailure {
                        code: mineintent_contracts::minecraft::BackendFailureCode::ProtocolError,
                        message: "backend failed while cancellation became ready".to_owned(),
                        retryable: false,
                    },
                })
            }
            ReadMode::CancelAgentBeforeFuture => Ok(()),
        }
    }
}

impl ProtocolObservationSource for FakeSource {
    fn epoch(&self) -> u64 {
        1
    }

    fn self_pose(&self) -> Result<SelfPose, BackendError> {
        Err(not_ready())
    }

    fn list_tracked_entities(&self) -> Result<Vec<ProtocolEntitySnapshot>, BackendError> {
        Err(not_ready())
    }

    fn read_block(
        &self,
        _position: BlockPosition,
    ) -> Result<mineintent_contracts::minecraft::BlockReadResult, BackendError> {
        Err(not_ready())
    }

    fn subscribe(
        &self,
        _listener: Arc<dyn ObservationEventListener>,
    ) -> Result<Box<dyn Subscription>, BackendError> {
        Err(not_ready())
    }

    fn read_viewport(
        &self,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<ViewportRead, BackendError>> {
        self.full_reads.fetch_add(1, Ordering::SeqCst);
        let mode = ReadMode::from_u8(self.full_mode.load(Ordering::SeqCst));
        *self.last_full_control.lock().unwrap() = Some(control.clone());
        if matches!(mode, ReadMode::CancelAgentBeforeFuture) {
            self.cancel_agent_on_full
                .lock()
                .unwrap()
                .take()
                .expect("cancel-before-future mode has an agent signal")
                .trigger();
        }
        Box::pin(async move {
            self.wait_or_return(control, mode, "read_viewport").await?;
            Ok(ViewportRead {
                projection: projection(),
                source: FactSource::ServerObserved,
                revision: 42,
            })
        })
    }

    fn read_directed_viewport(
        &self,
        positions: Vec<BlockPosition>,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<DirectedViewportProjection, DirectedViewportError>> {
        self.directed_reads.fetch_add(1, Ordering::SeqCst);
        *self.last_directed.lock().unwrap() = positions;
        let mode = ReadMode::from_u8(self.directed_mode.load(Ordering::SeqCst));
        Box::pin(async move {
            self.wait_or_return(control, mode, "read_directed_viewport")
                .await
                .map_err(DirectedViewportError::Backend)?;
            Ok(DirectedViewportProjection {
                seen: vec![mineintent_contracts::minecraft::DirectedSeenBlock {
                    at: [124, 65, -37],
                    block: mineintent_contracts::minecraft::BlockInfo::bare("stone"),
                }],
                unseen: Vec::new(),
            })
        })
    }
}

struct FakeBackend {
    source: Arc<FakeSource>,
    source_calls: AtomicUsize,
    source_error: Mutex<Option<BackendError>>,
    cancel_agent_on_source: Mutex<Option<Arc<TestCancellation>>>,
}

impl FakeBackend {
    fn new() -> Self {
        Self {
            source: Arc::new(FakeSource::new()),
            source_calls: AtomicUsize::new(0),
            source_error: Mutex::new(None),
            cancel_agent_on_source: Mutex::new(None),
        }
    }

    fn source_calls(&self) -> usize {
        self.source_calls.load(Ordering::SeqCst)
    }

    fn set_source_error(&self, error: Option<BackendError>) {
        *self.source_error.lock().unwrap() = error;
    }

    fn cancel_agent_then_fail_source(&self, cancellation: Arc<TestCancellation>) {
        *self.cancel_agent_on_source.lock().unwrap() = Some(cancellation);
        self.set_source_error(Some(BackendError::BackendFailure {
            failure: mineintent_contracts::minecraft::BackendFailure {
                code: mineintent_contracts::minecraft::BackendFailureCode::ProtocolError,
                message: "source binding failed while cancellation became ready".to_owned(),
                retryable: false,
            },
        }));
    }
}

impl MinecraftBackendApi for FakeBackend {
    fn start(
        &self,
        _control: OperationControl,
    ) -> BoxFuture<'_, Result<mineintent_contracts::minecraft::BackendReady, BackendError>> {
        Box::pin(async { Err(not_ready()) })
    }

    fn stop(
        &self,
        _reason: String,
        _control: OperationControl,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async { Err(not_ready()) })
    }

    fn state(&self) -> BackendState {
        BackendState::Idle
    }

    fn snapshot(&self) -> Result<MinecraftSnapshotV1, BackendError> {
        Err(not_ready())
    }

    fn subscribe(
        &self,
        _listener: Arc<dyn BackendEventListener>,
    ) -> Result<Box<dyn Subscription>, BackendError> {
        Err(not_ready())
    }

    fn observation_source(&self) -> Result<Arc<dyn ProtocolObservationSource>, BackendError> {
        self.source_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(cancellation) = self.cancel_agent_on_source.lock().unwrap().take() {
            cancellation.trigger();
        }
        if let Some(error) = self.source_error.lock().unwrap().clone() {
            Err(error)
        } else {
            Ok(Arc::clone(&self.source) as Arc<dyn ProtocolObservationSource>)
        }
    }

    fn motor(&self) -> Result<Arc<dyn MinecraftMotorDriverApi>, BackendError> {
        Err(not_ready())
    }

    fn send_chat(&self, _message: String) -> Result<(), BackendError> {
        Err(not_ready())
    }
}

fn not_ready() -> BackendError {
    BackendError::NotReady {
        state: "fake".to_owned(),
    }
}

fn projection() -> ViewportProjection {
    ViewportProjection {
        frame: ViewportFrame {
            coordinates:
                mineintent_contracts::minecraft::ViewportCoordinateSystem::MinecraftWorldAbsolute,
            self_pose: ViewportSelfPose {
                position: [0.5, 64.0, 0.5],
                yaw_degrees: 0.0,
                pitch_degrees: 0.0,
            },
            legend: ViewportLegend {
                visible_entities: "entities".to_owned(),
                visible_blocks: "blocks".to_owned(),
            },
        },
        standing_on_block: None,
        looked_at_block: None,
        visible_entities: VisibleEntitiesView {
            items: Vec::new(),
            truncated: false,
        },
        visible_blocks: VisibleBlocksView {
            blocks: Vec::new(),
            truncated: false,
        },
    }
}
