//! Production model-visible capabilities, the single viewport reader, and registry dispatch.
//!
//! This remains a middle-layer adapter: it does not compose Participant Runtime, app state, a
//! concrete backend facade, or round frames. Body capabilities measure only the backend pose
//! effect; the explicit observation-after port is the seam for a later runtime owner.

use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use crate::{
    events::{JournalError, JsonlEventJournal},
    execution::{
        AcquireDecision, ExecutionArbiter, ExecutionRefusalCode, ExecutionRequest,
        ResourceLeaseHandle,
    },
    memory::{MemoryError, MemoryStore},
    speech::{SpeechRequest, SpeechScheduleError, SpeechScheduler, SpeechTransport},
};
use mineintent_contracts::agent::{
    JsonObject, ToolDefinitionName, ToolDefinitionType, ToolExecution, ToolInvocation,
};
use mineintent_contracts::{
    agent::{AgentError, AgentErrorCode, ContractFuture, ExecutionControl},
    capability::{
        look_relative_parameters_schema, move_input_parameters_schema, remember_parameters_schema,
        view_parameters_schema, CapabilityExecutionContext, CapabilityInvocation,
        ExecutionResource, LookRelativeArguments, MoveDirection, MoveInputArguments,
        RememberArguments, RememberOperation, ToolCapability, ToolCapabilityRegistry,
        ToolDispatcher as ContractToolDispatcher, ToolResultProtocol, ViewArguments, ViewMode,
    },
    minecraft::{
        present_directed_viewport_v2, present_viewport_v2, BackendError, BoxFuture,
        CancellationSignal as BackendCancellationSignal, Deadline as BackendDeadline,
        DirectedViewportError, DirectedViewportProjection, LookRelativeRequest,
        MinecraftBackendApi, MinecraftMotorDriverApi, MotorMoveDirection, MoveInputRequest,
        OperationControl, ProtocolObservationSource, SelfPose, ViewportDirectedV2, ViewportFullV2,
    },
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::sync::Notify;

const VIEW_CAPABILITY_NAME: &str = "view";
const VIEW_CAPABILITY_DESCRIPTION: &str =
    "读取当前视口。full 可能因预算截断；未列出不代表不可见或不存在，想核对坐标时使用 directed。directed 按坐标给出可见事实或不可见原因；不可见时不泄露目标方块的身份或状态。";
const MAX_TOOL_SUMMARY_CHARS: usize = 300;
const CONTROL_SETTLE_TIMEOUT: Duration = Duration::from_millis(250);

/// 一个读取结果只包含模型要看的投影；`ViewportRead` 的 source/revision 留在 backend
/// 原子契约内，不穿过 capability 或轮末 frame。
#[derive(Clone, Debug, PartialEq)]
pub enum ViewportValue {
    Full(ViewportFullV2),
    Directed(ViewportDirectedV2),
}

/// 复用 backend 原子 viewport 读的 middle adapter。
///
/// 每次 `read` 都先从 `MinecraftBackendApi` 绑定一次最新 source，再调用一次对应的
/// atomic read。这里不复制 viewport 几何，也不经过 Information runtime，避免把其
/// provider-level deadline/失败语义带进 Agent capability。
pub struct ViewportReader {
    backend: Arc<dyn MinecraftBackendApi>,
}

impl ViewportReader {
    pub fn new(backend: Arc<dyn MinecraftBackendApi>) -> Self {
        Self { backend }
    }

    pub fn read<'a>(
        &'a self,
        arguments: ViewArguments,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<ViewportValue, AgentError>> {
        Box::pin(async move {
            match arguments.mode {
                ViewMode::Full => self.read_full(control).await.map(ViewportValue::Full),
                ViewMode::Directed => match arguments.positions {
                    Some(positions) => self
                        .read_directed(positions, control)
                        .await
                        .map(ViewportValue::Directed),
                    None => Err(AgentError::new(
                        AgentErrorCode::InvalidToolInvocation,
                        "directed view requires positions",
                    )),
                },
            }
        })
    }

    pub fn read_full<'a>(
        &'a self,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<ViewportFullV2, AgentError>> {
        Box::pin(async move {
            control.check_at(Instant::now())?;
            let source = self.bind_source(control)?;
            control.check_at(Instant::now())?;
            let operation = "read_viewport";
            let bridge = BackendControlBridge::new();
            bridge
                .control
                .preflight(operation)
                .map_err(map_backend_error)?;
            let future = source.read_viewport(bridge.control.clone());
            let read = await_backend(future, control, bridge).await?;
            control.check_at(Instant::now())?;
            Ok(present_viewport_v2(&read.projection))
        })
    }

    pub fn read_directed<'a>(
        &'a self,
        positions: Vec<(i32, i32, i32)>,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<ViewportDirectedV2, AgentError>> {
        Box::pin(async move {
            control.check_at(Instant::now())?;
            let source = self.bind_source(control)?;
            control.check_at(Instant::now())?;
            let positions = positions
                .into_iter()
                .map(|(x, y, z)| mineintent_contracts::minecraft::BlockPosition { x, y, z })
                .collect();
            let operation = "read_directed_viewport";
            let bridge = BackendControlBridge::new();
            bridge
                .control
                .preflight(operation)
                .map_err(map_backend_error)?;
            let future = source.read_directed_viewport(positions, bridge.control.clone());
            let read = await_backend_directed(future, control, bridge).await?;
            control.check_at(Instant::now())?;
            present_directed_viewport_v2(&read)
                .map_err(|error| AgentError::new(AgentErrorCode::ToolFailed, error))
        })
    }

    fn bind_source<'a>(
        &'a self,
        control: ExecutionControl<'a>,
    ) -> Result<Arc<dyn ProtocolObservationSource>, AgentError> {
        control.check_at(Instant::now())?;
        let source = self.backend.observation_source();
        // Source binding is synchronous but user code may still make control ready before it
        // returns. Preserve the same cancellation/deadline priority as async backend reads.
        control.check_at(Instant::now())?;
        source.map_err(map_backend_error)
    }
}

/// 模型可见的 `view` capability。结果刻意沿用旧 capability 外壳；模式由所选 viewport DTO
/// 表达，不新增 mode 或其他 wire 字段。
pub struct ViewCapability {
    definition: mineintent_contracts::agent::WireToolDefinition,
    reader: Arc<ViewportReader>,
}

impl ViewCapability {
    pub fn new(reader: Arc<ViewportReader>) -> Self {
        Self {
            definition: view_definition(),
            reader,
        }
    }
}

fn view_definition() -> mineintent_contracts::agent::WireToolDefinition {
    mineintent_contracts::agent::WireToolDefinition {
        r#type: mineintent_contracts::agent::ToolDefinitionType::Function,
        function: mineintent_contracts::agent::FunctionToolDefinition {
            name: mineintent_contracts::agent::ToolDefinitionName::new(VIEW_CAPABILITY_NAME)
                .expect("view is a valid tool name"),
            description: VIEW_CAPABILITY_DESCRIPTION.to_owned(),
            parameters: view_parameters_schema(),
        },
    }
}

pub fn create_view_capability(reader: Arc<ViewportReader>) -> Arc<dyn ToolCapability> {
    Arc::new(ViewCapability::new(reader))
}

impl ToolCapability for ViewCapability {
    fn definition(&self) -> &mineintent_contracts::agent::WireToolDefinition {
        &self.definition
    }

    fn resource(&self) -> Option<ExecutionResource> {
        Some(ExecutionResource::Viewport)
    }

    fn execute<'a>(
        &'a self,
        invocation: CapabilityInvocation,
        context: CapabilityExecutionContext<'a>,
    ) -> ContractFuture<'a, Result<Value, AgentError>> {
        let reader = Arc::clone(&self.reader);
        Box::pin(async move {
            context.check_at(Instant::now())?;
            let arguments = match serde_json::from_value::<ViewArguments>(Value::Object(
                invocation.arguments,
            )) {
                Ok(arguments) => arguments,
                Err(error) => {
                    return Ok(failed_result(truncate_summary(error.to_string())));
                }
            };

            let result = reader.read(arguments, context.control()).await;
            // This check is intentionally after the read. A completed projection from a scope
            // that changed during the read must never be published as current model context.
            context.check_at(Instant::now())?;
            match result {
                Ok(ViewportValue::Full(viewport)) => Ok(completed_result(viewport)),
                Ok(ViewportValue::Directed(viewport)) => Ok(completed_result(viewport)),
                Err(error) if is_structured_control_error(error.code) => Err(error),
                Err(error) => Ok(failed_result(truncate_summary(error.summary))),
            }
        })
    }
}

fn completed_result<T: serde::Serialize>(viewport: T) -> Value {
    let viewport = match serde_json::to_value(viewport) {
        Ok(viewport) if !viewport.is_null() => viewport,
        _ => return failed_result("view_result_serialization_failed".to_owned()),
    };
    Value::Object(Map::from_iter([
        (
            "protocol".to_owned(),
            serde_json::to_value(ToolResultProtocol::V1)
                .unwrap_or_else(|_| Value::String("mineintent.tool-result.v1".to_owned())),
        ),
        ("status".to_owned(), Value::String("completed".to_owned())),
        ("viewport".to_owned(), viewport),
    ]))
}

fn failed_result(summary: String) -> Value {
    json!({
        "protocol": ToolResultProtocol::V1,
        "status": "failed",
        "summary": summary,
    })
}

fn is_structured_control_error(code: AgentErrorCode) -> bool {
    matches!(
        code,
        AgentErrorCode::RunCancelled
            | AgentErrorCode::DeadlineExceeded
            | AgentErrorCode::ScopeInvalid
    )
}

fn truncate_summary(summary: impl AsRef<str>) -> String {
    summary
        .as_ref()
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_TOOL_SUMMARY_CHARS)
        .collect()
}

fn map_backend_error(error: BackendError) -> AgentError {
    match error {
        BackendError::Cancelled { .. } => AgentError::run_cancelled(),
        BackendError::DeadlineExceeded { .. } => AgentError::deadline_exceeded(),
        other => AgentError::new(
            AgentErrorCode::ToolFailed,
            truncate_summary(format!("viewport backend read failed: {other}")),
        ),
    }
}

fn map_directed_error(error: DirectedViewportError) -> AgentError {
    match error {
        DirectedViewportError::Backend(error) => map_backend_error(error),
        DirectedViewportError::OutOfWorld { position } => AgentError::new(
            AgentErrorCode::ToolFailed,
            truncate_summary(format!(
                "viewport directed read failed at ({}, {}, {}): out_of_world",
                position.x, position.y, position.z
            )),
        ),
    }
}

/// 静态 backend control；它的唤醒由 reader 的异步边界 relay。
struct BackendControlBridge {
    control: OperationControl,
    cancellation: Arc<RelayCancellation>,
    deadline: Arc<RelayDeadline>,
}

impl BackendControlBridge {
    fn new() -> Self {
        let cancellation = Arc::new(RelayCancellation::default());
        let deadline = Arc::new(RelayDeadline::default());
        let control = OperationControl::new(
            Arc::clone(&cancellation) as Arc<dyn BackendCancellationSignal>,
            Some(Arc::clone(&deadline) as Arc<dyn BackendDeadline>),
        );
        Self {
            control,
            cancellation,
            deadline,
        }
    }

    fn trigger_for(&self, error: &AgentError) {
        if error.code == AgentErrorCode::DeadlineExceeded {
            self.deadline.trigger();
        } else {
            self.cancellation.trigger();
        }
    }
}

#[derive(Default)]
struct RelayCancellation {
    triggered: AtomicBool,
    notify: Notify,
}

impl RelayCancellation {
    fn trigger(&self) {
        if !self.triggered.swap(true, Ordering::SeqCst) {
            self.notify.notify_waiters();
            self.notify.notify_one();
        }
    }
}

impl BackendCancellationSignal for RelayCancellation {
    fn is_cancelled(&self) -> bool {
        self.triggered.load(Ordering::SeqCst)
    }

    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            loop {
                if self.is_cancelled() {
                    return;
                }
                self.notify.notified().await;
            }
        })
    }
}

#[derive(Default)]
struct RelayDeadline {
    triggered: AtomicBool,
    notify: Notify,
}

impl RelayDeadline {
    fn trigger(&self) {
        if !self.triggered.swap(true, Ordering::SeqCst) {
            self.notify.notify_waiters();
            self.notify.notify_one();
        }
    }
}

impl BackendDeadline for RelayDeadline {
    fn has_elapsed(&self) -> bool {
        self.triggered.load(Ordering::SeqCst)
    }

    fn elapsed(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            loop {
                if self.has_elapsed() {
                    return;
                }
                self.notify.notified().await;
            }
        })
    }
}

async fn await_backend<'a>(
    future: BoxFuture<'a, Result<mineintent_contracts::minecraft::ViewportRead, BackendError>>,
    control: ExecutionControl<'a>,
    bridge: BackendControlBridge,
) -> Result<mineintent_contracts::minecraft::ViewportRead, AgentError> {
    match await_backend_future(future, control, &bridge).await {
        Ok(read) => Ok(read),
        Err(AwaitBackendError::Control(error)) => Err(error),
        Err(AwaitBackendError::Backend(error)) => Err(map_backend_error(error)),
    }
}

async fn await_backend_directed<'a>(
    future: BoxFuture<'a, Result<DirectedViewportProjection, DirectedViewportError>>,
    control: ExecutionControl<'a>,
    bridge: BackendControlBridge,
) -> Result<DirectedViewportProjection, AgentError> {
    match await_backend_future(future, control, &bridge).await {
        Ok(read) => Ok(read),
        Err(AwaitBackendError::Control(error)) => Err(error),
        Err(AwaitBackendError::Backend(error)) => Err(map_directed_error(error)),
    }
}

enum AwaitBackendError<E> {
    Control(AgentError),
    Backend(E),
}

async fn await_backend_future<'a, T, E>(
    future: BoxFuture<'a, Result<T, E>>,
    control: ExecutionControl<'a>,
    bridge: &BackendControlBridge,
) -> Result<T, AwaitBackendError<E>>
where
    E: Send,
{
    let mut backend_future = future;
    if let Err(error) = control.check_at(Instant::now()) {
        // The source has already returned its future at this point. Relay the late control
        // transition before dropping it in case construction started backend-owned work.
        bridge.trigger_for(&error);
        let _ = tokio::time::timeout(CONTROL_SETTLE_TIMEOUT, &mut backend_future).await;
        return Err(AwaitBackendError::Control(error));
    }
    let cancellation = control.cancelled();
    let deadline = tokio::time::sleep_until(tokio::time::Instant::from_std(
        control.deadline().expires_at(),
    ));
    tokio::pin!(cancellation);
    tokio::pin!(deadline);

    tokio::select! {
        biased;
        cancellation_error = &mut cancellation => {
            let error = control.check_at(Instant::now()).err().unwrap_or(cancellation_error);
            bridge.trigger_for(&error);
            let _ = tokio::time::timeout(CONTROL_SETTLE_TIMEOUT, &mut backend_future).await;
            Err(AwaitBackendError::Control(error))
        }
        _ = &mut deadline => {
            let error = control
                .check_at(Instant::now())
                .err()
                .unwrap_or_else(AgentError::deadline_exceeded);
            bridge.trigger_for(&error);
            let _ = tokio::time::timeout(CONTROL_SETTLE_TIMEOUT, &mut backend_future).await;
            Err(AwaitBackendError::Control(error))
        }
        result = &mut backend_future => {
            // Not redundant with the entry preflight: a backend future may make cancellation
            // ready and return an error in the same poll. Control must still win that boundary.
            if let Err(error) = control.check_at(Instant::now()) {
                bridge.trigger_for(&error);
                Err(AwaitBackendError::Control(error))
            } else {
                result.map_err(AwaitBackendError::Backend)
            }
        }
    }
}

/// Supplies the two invocation fields that must not be manufactured by a capability or by a
/// hidden process-global clock/counter.
pub trait CapabilityActionIdSource: Send + Sync {
    fn next_action_id(&self, invocation: &ToolInvocation) -> Result<String, AgentError>;
}

pub trait CapabilityUtcTimestampSource: Send + Sync {
    fn now_utc(&self) -> Result<String, AgentError>;
}

pub trait CapabilityInvocationAssembler: Send + Sync {
    fn assemble(&self, invocation: &ToolInvocation) -> Result<CapabilityInvocation, AgentError>;
}

pub struct ExplicitCapabilityInvocationAssembler {
    action_ids: Arc<dyn CapabilityActionIdSource>,
    clock: Arc<dyn CapabilityUtcTimestampSource>,
}

impl ExplicitCapabilityInvocationAssembler {
    pub fn new(
        action_ids: Arc<dyn CapabilityActionIdSource>,
        clock: Arc<dyn CapabilityUtcTimestampSource>,
    ) -> Self {
        Self { action_ids, clock }
    }
}

impl CapabilityInvocationAssembler for ExplicitCapabilityInvocationAssembler {
    fn assemble(&self, invocation: &ToolInvocation) -> Result<CapabilityInvocation, AgentError> {
        let action_id = self.action_ids.next_action_id(invocation)?;
        if action_id.is_empty() {
            return Err(AgentError::new(
                AgentErrorCode::InvalidRequest,
                "action_id must not be empty",
            ));
        }
        let started_at = self.clock.now_utc()?;
        if started_at.is_empty() {
            return Err(AgentError::new(
                AgentErrorCode::InvalidRequest,
                "started_at must not be empty",
            ));
        }
        Ok(CapabilityInvocation {
            run_id: invocation.run_id.clone(),
            tool_call_id: invocation.tool_call_id.clone(),
            arguments: invocation.arguments.clone(),
            action_id,
            started_at,
        })
    }
}

/// The scope/world/chat tuple is assembled explicitly and borrowed by each capability context.
pub struct CapabilityScopeAssembly {
    world_id: String,
    chat_event_id: String,
    scope_guard: Arc<dyn mineintent_contracts::capability::ScopeGuard>,
}

impl CapabilityScopeAssembly {
    pub fn new(
        world_id: impl Into<String>,
        chat_event_id: impl Into<String>,
        scope_guard: Arc<dyn mineintent_contracts::capability::ScopeGuard>,
    ) -> Self {
        Self {
            world_id: world_id.into(),
            chat_event_id: chat_event_id.into(),
            scope_guard,
        }
    }

    fn context<'a>(&'a self, control: ExecutionControl<'a>) -> CapabilityExecutionContext<'a> {
        CapabilityExecutionContext::new(
            &self.world_id,
            &self.chat_event_id,
            control,
            self.scope_guard.as_ref(),
        )
    }
}

pub trait CapabilityJournal: Send + Sync {
    fn append<'a>(
        &'a self,
        event_type: String,
        payload: JsonObject,
    ) -> ContractFuture<'a, Result<(), AgentError>>;
}

impl CapabilityJournal for JsonlEventJournal {
    fn append<'a>(
        &'a self,
        event_type: String,
        payload: JsonObject,
    ) -> ContractFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            JsonlEventJournal::append(self, event_type, payload)
                .await
                .map(|_| ())
                .map_err(map_journal_error)
        })
    }
}

pub trait SpeechSchedulerPort: Send + Sync {
    fn schedule(&self, request: SpeechRequest) -> Result<usize, SpeechScheduleError>;
}

impl<T> SpeechSchedulerPort for SpeechScheduler<T>
where
    T: SpeechTransport + 'static,
{
    fn schedule(&self, request: SpeechRequest) -> Result<usize, SpeechScheduleError> {
        SpeechScheduler::schedule(self, request)
    }
}

pub trait MemoryStorePort: Send + Sync {
    fn append<'a>(&'a self, text: String) -> ContractFuture<'a, Result<(), MemoryError>>;

    fn replace<'a>(
        &'a self,
        old_text: String,
        new_text: String,
    ) -> ContractFuture<'a, Result<(), MemoryError>>;

    fn rewrite<'a>(&'a self, text: String) -> ContractFuture<'a, Result<(), MemoryError>>;
}

impl MemoryStorePort for MemoryStore {
    fn append<'a>(&'a self, text: String) -> ContractFuture<'a, Result<(), MemoryError>> {
        Box::pin(async move { MemoryStore::append(self, text).await.map(|_| ()) })
    }

    fn replace<'a>(
        &'a self,
        old_text: String,
        new_text: String,
    ) -> ContractFuture<'a, Result<(), MemoryError>> {
        Box::pin(async move {
            MemoryStore::replace(self, old_text, new_text)
                .await
                .map(|_| ())
        })
    }

    fn rewrite<'a>(&'a self, text: String) -> ContractFuture<'a, Result<(), MemoryError>> {
        Box::pin(async move { MemoryStore::rewrite(self, text).await.map(|_| ()) })
    }
}

pub trait QueuedSayObserver: Send + Sync {
    fn record_queued(&self, run_id: &mineintent_contracts::agent::RunId);
}

#[derive(Default)]
pub struct NoopQueuedSayObserver;

impl QueuedSayObserver for NoopQueuedSayObserver {
    fn record_queued(&self, _run_id: &mineintent_contracts::agent::RunId) {}
}

pub trait ObservationAfterSource: Send + Sync {
    fn observe_after<'a>(
        &'a self,
        invocation: CapabilityInvocation,
        resource: ExecutionResource,
        result: Value,
        context: CapabilityExecutionContext<'a>,
    ) -> ContractFuture<'a, Result<Option<JsonObject>, AgentError>>;
}

/// Explicit default for this slice: no observation facts are injected. Participant Runtime can
/// replace it with a body-only, non-viewport source without changing the dispatcher or tools.
#[derive(Default)]
pub struct NullObservationAfter;

impl ObservationAfterSource for NullObservationAfter {
    fn observe_after<'a>(
        &'a self,
        _invocation: CapabilityInvocation,
        _resource: ExecutionResource,
        _result: Value,
        context: CapabilityExecutionContext<'a>,
    ) -> ContractFuture<'a, Result<Option<JsonObject>, AgentError>> {
        Box::pin(async move {
            context.check_at(Instant::now())?;
            Ok(None)
        })
    }
}

pub struct ProductionCapabilityServices {
    pub backend: Arc<dyn MinecraftBackendApi>,
    pub viewport_reader: Arc<ViewportReader>,
    pub journal: Arc<dyn CapabilityJournal>,
    pub speech: Arc<dyn SpeechSchedulerPort>,
    pub memory: Arc<dyn MemoryStorePort>,
    pub queued_say_observer: Arc<dyn QueuedSayObserver>,
}

impl ProductionCapabilityServices {
    pub fn new(
        backend: Arc<dyn MinecraftBackendApi>,
        viewport_reader: Arc<ViewportReader>,
        journal: Arc<dyn CapabilityJournal>,
        speech: Arc<dyn SpeechSchedulerPort>,
        memory: Arc<dyn MemoryStorePort>,
    ) -> Self {
        Self {
            backend,
            viewport_reader,
            journal,
            speech,
            memory,
            queued_say_observer: Arc::new(NoopQueuedSayObserver),
        }
    }

    pub fn with_queued_say_observer(mut self, observer: Arc<dyn QueuedSayObserver>) -> Self {
        self.queued_say_observer = observer;
        self
    }
}

pub fn build_production_capability_registry(
    services: ProductionCapabilityServices,
) -> Result<Arc<ToolCapabilityRegistry>, AgentError> {
    let capabilities: Vec<Arc<dyn ToolCapability>> = vec![
        create_look_relative_capability(
            Arc::clone(&services.backend),
            Arc::clone(&services.journal),
        ),
        create_move_input_capability(Arc::clone(&services.backend), Arc::clone(&services.journal)),
        create_view_capability(Arc::clone(&services.viewport_reader)),
        create_say_capability(
            Arc::clone(&services.speech),
            Arc::clone(&services.journal),
            Arc::clone(&services.queued_say_observer),
        ),
        create_remember_capability(Arc::clone(&services.memory), Arc::clone(&services.journal)),
    ];
    Ok(Arc::new(ToolCapabilityRegistry::new(capabilities)?))
}

const LOOK_RELATIVE_CAPABILITY_NAME: &str = "look_relative";
const MOVE_INPUT_CAPABILITY_NAME: &str = "move_input";
const SAY_CAPABILITY_NAME: &str = "say";
const REMEMBER_CAPABILITY_NAME: &str = "remember";
const LOOK_EFFECT_EPSILON_DEGREES: f64 = 0.01;
const MOVE_EFFECT_EPSILON: f64 = 0.01;

const LOOK_RELATIVE_CAPABILITY_DESCRIPTION: &str =
    "相对当前视线转动一次视角，随后返回实际转动效果。玩家提到的东西不在当前视野里，或行动前需要先转向别处时调用。";
const MOVE_INPUT_CAPABILITY_DESCRIPTION: &str =
    "想往一个方向挪一点、或斜着靠近已经看见的东西时，短暂按住一组真实移动键再一起松开，随后返回实际移动效果。前后键或左右键同时按会互相抵消，对应轴不会移动。没有寻路也不会跳跃：一次最多走几格，障碍不会被自动绕开，返回时身体可能仍在滑行或下落。";
const SAY_CAPABILITY_DESCRIPTION: &str =
    "把一句话交给聊天发送队列。返回只表示已排队，不表示玩家已经看到：长句会被切成几条依次发出，发送有间隔，离开当前世界会取消未发出的部分。想说话时调用；不需要说、或想保持沉默时，不调用即可。动作要花时间，行动前先简短说一句往往更自然。";
const REMEMBER_CAPABILITY_DESCRIPTION: &str =
    "编辑持久的单文本记忆，供以后回忆。明确选择 append 追加、replace 用唯一原文锚点替换（newText 可为空以删除），或 rewrite 重写全文；不同操作的字段不能混用。";

fn function_definition(
    name: &'static str,
    description: &'static str,
    parameters: Map<String, Value>,
) -> mineintent_contracts::agent::WireToolDefinition {
    mineintent_contracts::agent::WireToolDefinition {
        r#type: ToolDefinitionType::Function,
        function: mineintent_contracts::agent::FunctionToolDefinition {
            name: ToolDefinitionName::new(name).expect("capability name is valid"),
            description: description.to_owned(),
            parameters,
        },
    }
}

pub struct LookRelativeCapability {
    definition: mineintent_contracts::agent::WireToolDefinition,
    backend: Arc<dyn MinecraftBackendApi>,
    journal: Arc<dyn CapabilityJournal>,
}

impl LookRelativeCapability {
    pub fn new(backend: Arc<dyn MinecraftBackendApi>, journal: Arc<dyn CapabilityJournal>) -> Self {
        Self {
            definition: function_definition(
                LOOK_RELATIVE_CAPABILITY_NAME,
                LOOK_RELATIVE_CAPABILITY_DESCRIPTION,
                look_relative_parameters_schema(),
            ),
            backend,
            journal,
        }
    }
}

pub fn create_look_relative_capability(
    backend: Arc<dyn MinecraftBackendApi>,
    journal: Arc<dyn CapabilityJournal>,
) -> Arc<dyn ToolCapability> {
    Arc::new(LookRelativeCapability::new(backend, journal))
}

impl ToolCapability for LookRelativeCapability {
    fn definition(&self) -> &mineintent_contracts::agent::WireToolDefinition {
        &self.definition
    }

    fn resource(&self) -> Option<ExecutionResource> {
        Some(ExecutionResource::Body)
    }

    fn execute<'a>(
        &'a self,
        invocation: CapabilityInvocation,
        context: CapabilityExecutionContext<'a>,
    ) -> ContractFuture<'a, Result<Value, AgentError>> {
        let backend = Arc::clone(&self.backend);
        let journal = Arc::clone(&self.journal);
        Box::pin(async move {
            context.check_at(Instant::now())?;
            let arguments = match serde_json::from_value::<LookRelativeArguments>(Value::Object(
                invocation.arguments.clone(),
            )) {
                Ok(arguments) => arguments,
                Err(error) => return Ok(failed_result(truncate_summary(error.to_string()))),
            };

            let motor = match backend.motor() {
                Ok(motor) => motor,
                Err(error) => {
                    return body_failed_result(
                        journal.as_ref(),
                        &invocation,
                        LOOK_RELATIVE_CAPABILITY_NAME,
                        context,
                        map_motor_backend_error(error),
                    )
                    .await;
                }
            };
            let _release = ReleaseAllOnDrop::new(Arc::clone(&motor));
            context.check_at(Instant::now())?;
            let source = match backend.observation_source() {
                Ok(source) => source,
                Err(error) => {
                    return body_failed_result(
                        journal.as_ref(),
                        &invocation,
                        LOOK_RELATIVE_CAPABILITY_NAME,
                        context,
                        map_motor_backend_error(error),
                    )
                    .await;
                }
            };
            let before = match source.self_pose() {
                Ok(pose) => pose,
                Err(error) => {
                    return body_failed_result(
                        journal.as_ref(),
                        &invocation,
                        LOOK_RELATIVE_CAPABILITY_NAME,
                        context,
                        map_motor_backend_error(error),
                    )
                    .await;
                }
            };
            context.check_at(Instant::now())?;

            let bridge = BackendControlBridge::new();
            bridge
                .control
                .preflight(LOOK_RELATIVE_CAPABILITY_NAME)
                .map_err(map_motor_backend_error)?;
            let request = LookRelativeRequest {
                yaw_degrees: arguments.yaw_degrees,
                pitch_degrees: arguments.pitch_degrees,
            };
            match await_backend_future(
                motor.look_relative(request, bridge.control.clone()),
                context.control(),
                &bridge,
            )
            .await
            {
                Ok(()) => {}
                Err(AwaitBackendError::Control(error)) => return Err(error),
                Err(AwaitBackendError::Backend(error)) => {
                    return body_failed_result(
                        journal.as_ref(),
                        &invocation,
                        LOOK_RELATIVE_CAPABILITY_NAME,
                        context,
                        map_motor_backend_error(error),
                    )
                    .await;
                }
            };
            context.check_at(Instant::now())?;
            let after = match source.self_pose() {
                Ok(pose) => pose,
                Err(error) => {
                    return body_failed_result(
                        journal.as_ref(),
                        &invocation,
                        LOOK_RELATIVE_CAPABILITY_NAME,
                        context,
                        map_motor_backend_error(error),
                    )
                    .await;
                }
            };
            let result = completed_body_result(measured_look_effect(&before, &after));
            let journal_result = journal_event(
                journal.as_ref(),
                "body_tool.completed",
                body_journal_payload(
                    &invocation,
                    LOOK_RELATIVE_CAPABILITY_NAME,
                    json!({
                        "internal": {"before": before, "after": after}
                    }),
                ),
                context.control(),
            )
            .await;
            if let Err(error) = journal_result {
                return body_failed_result(
                    journal.as_ref(),
                    &invocation,
                    LOOK_RELATIVE_CAPABILITY_NAME,
                    context,
                    error,
                )
                .await;
            }
            context.check_at(Instant::now())?;
            Ok(result)
        })
    }
}

pub struct MoveInputCapability {
    definition: mineintent_contracts::agent::WireToolDefinition,
    backend: Arc<dyn MinecraftBackendApi>,
    journal: Arc<dyn CapabilityJournal>,
}

impl MoveInputCapability {
    pub fn new(backend: Arc<dyn MinecraftBackendApi>, journal: Arc<dyn CapabilityJournal>) -> Self {
        Self {
            definition: function_definition(
                MOVE_INPUT_CAPABILITY_NAME,
                MOVE_INPUT_CAPABILITY_DESCRIPTION,
                move_input_parameters_schema(),
            ),
            backend,
            journal,
        }
    }
}

pub fn create_move_input_capability(
    backend: Arc<dyn MinecraftBackendApi>,
    journal: Arc<dyn CapabilityJournal>,
) -> Arc<dyn ToolCapability> {
    Arc::new(MoveInputCapability::new(backend, journal))
}

impl ToolCapability for MoveInputCapability {
    fn definition(&self) -> &mineintent_contracts::agent::WireToolDefinition {
        &self.definition
    }

    fn resource(&self) -> Option<ExecutionResource> {
        Some(ExecutionResource::Body)
    }

    fn execute<'a>(
        &'a self,
        invocation: CapabilityInvocation,
        context: CapabilityExecutionContext<'a>,
    ) -> ContractFuture<'a, Result<Value, AgentError>> {
        let backend = Arc::clone(&self.backend);
        let journal = Arc::clone(&self.journal);
        Box::pin(async move {
            context.check_at(Instant::now())?;
            let arguments = match serde_json::from_value::<MoveInputArguments>(Value::Object(
                invocation.arguments.clone(),
            )) {
                Ok(arguments) => arguments,
                Err(error) => return Ok(failed_result(truncate_summary(error.to_string()))),
            };

            let motor = match backend.motor() {
                Ok(motor) => motor,
                Err(error) => {
                    return body_failed_result(
                        journal.as_ref(),
                        &invocation,
                        MOVE_INPUT_CAPABILITY_NAME,
                        context,
                        map_motor_backend_error(error),
                    )
                    .await;
                }
            };
            let _release = ReleaseAllOnDrop::new(Arc::clone(&motor));
            context.check_at(Instant::now())?;
            let source = match backend.observation_source() {
                Ok(source) => source,
                Err(error) => {
                    return body_failed_result(
                        journal.as_ref(),
                        &invocation,
                        MOVE_INPUT_CAPABILITY_NAME,
                        context,
                        map_motor_backend_error(error),
                    )
                    .await;
                }
            };
            let before = match source.self_pose() {
                Ok(pose) => pose,
                Err(error) => {
                    return body_failed_result(
                        journal.as_ref(),
                        &invocation,
                        MOVE_INPUT_CAPABILITY_NAME,
                        context,
                        map_motor_backend_error(error),
                    )
                    .await;
                }
            };
            context.check_at(Instant::now())?;

            let bridge = BackendControlBridge::new();
            bridge
                .control
                .preflight(MOVE_INPUT_CAPABILITY_NAME)
                .map_err(map_motor_backend_error)?;
            let request = MoveInputRequest {
                directions: arguments
                    .directions
                    .iter()
                    .copied()
                    .map(motor_direction)
                    .collect(),
                duration_ms: arguments.duration_ms,
                sprint: arguments.sprint,
            };
            match await_backend_future(
                motor.move_input(request, bridge.control.clone()),
                context.control(),
                &bridge,
            )
            .await
            {
                Ok(()) => {}
                Err(AwaitBackendError::Control(error)) => return Err(error),
                Err(AwaitBackendError::Backend(error)) => {
                    return body_failed_result(
                        journal.as_ref(),
                        &invocation,
                        MOVE_INPUT_CAPABILITY_NAME,
                        context,
                        map_motor_backend_error(error),
                    )
                    .await;
                }
            }
            context.check_at(Instant::now())?;
            let after = match source.self_pose() {
                Ok(pose) => pose,
                Err(error) => {
                    return body_failed_result(
                        journal.as_ref(),
                        &invocation,
                        MOVE_INPUT_CAPABILITY_NAME,
                        context,
                        map_motor_backend_error(error),
                    )
                    .await;
                }
            };
            let result = completed_body_result(measured_move_effect(&before, &after));
            let journal_result = journal_event(
                journal.as_ref(),
                "body_tool.completed",
                body_journal_payload(
                    &invocation,
                    MOVE_INPUT_CAPABILITY_NAME,
                    json!({
                        "internal": {"before": before, "after": after}
                    }),
                ),
                context.control(),
            )
            .await;
            if let Err(error) = journal_result {
                return body_failed_result(
                    journal.as_ref(),
                    &invocation,
                    MOVE_INPUT_CAPABILITY_NAME,
                    context,
                    error,
                )
                .await;
            }
            context.check_at(Instant::now())?;
            Ok(result)
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SayArguments {
    text: String,
}

pub struct SayCapability {
    definition: mineintent_contracts::agent::WireToolDefinition,
    speech: Arc<dyn SpeechSchedulerPort>,
    journal: Arc<dyn CapabilityJournal>,
    queued_observer: Arc<dyn QueuedSayObserver>,
}

impl SayCapability {
    pub fn new(
        speech: Arc<dyn SpeechSchedulerPort>,
        journal: Arc<dyn CapabilityJournal>,
        queued_observer: Arc<dyn QueuedSayObserver>,
    ) -> Self {
        Self {
            definition: function_definition(
                SAY_CAPABILITY_NAME,
                SAY_CAPABILITY_DESCRIPTION,
                say_parameters_schema(),
            ),
            speech,
            journal,
            queued_observer,
        }
    }
}

pub fn create_say_capability(
    speech: Arc<dyn SpeechSchedulerPort>,
    journal: Arc<dyn CapabilityJournal>,
    queued_observer: Arc<dyn QueuedSayObserver>,
) -> Arc<dyn ToolCapability> {
    Arc::new(SayCapability::new(speech, journal, queued_observer))
}

impl ToolCapability for SayCapability {
    fn definition(&self) -> &mineintent_contracts::agent::WireToolDefinition {
        &self.definition
    }

    fn resource(&self) -> Option<ExecutionResource> {
        Some(ExecutionResource::Chat)
    }

    fn execute<'a>(
        &'a self,
        invocation: CapabilityInvocation,
        context: CapabilityExecutionContext<'a>,
    ) -> ContractFuture<'a, Result<Value, AgentError>> {
        let speech = Arc::clone(&self.speech);
        let journal = Arc::clone(&self.journal);
        let queued_observer = Arc::clone(&self.queued_observer);
        Box::pin(async move {
            context.check_at(Instant::now())?;
            let arguments = match serde_json::from_value::<SayArguments>(Value::Object(
                invocation.arguments.clone(),
            )) {
                Ok(arguments) if arguments.text.chars().count() <= 500 => arguments,
                Ok(_) => {
                    return Ok(failed_result(
                        "say text must contain at most 500 characters".to_owned(),
                    ));
                }
                Err(error) => return Ok(failed_result(truncate_summary(error.to_string()))),
            };
            let text = arguments.text.trim().to_owned();
            if text.is_empty() {
                return Ok(failed_result("say requires a non-empty text".to_owned()));
            }
            context.check_at(Instant::now())?;
            let segments = match speech.schedule(SpeechRequest {
                id: invocation.action_id.clone(),
                text: text.clone(),
            }) {
                Ok(segments) => segments,
                Err(error) => return Ok(failed_result(truncate_summary(error.to_string()))),
            };
            queued_observer.record_queued(&invocation.run_id);
            let journal_result = journal_event(
                journal.as_ref(),
                "say.queued",
                object(json!({
                    "actionId": invocation.action_id,
                    "runId": invocation.run_id,
                    "toolCallId": invocation.tool_call_id,
                    "segments": segments,
                    "characters": text.chars().count(),
                })),
                context.control(),
            )
            .await;
            if let Err(error) = journal_result {
                if is_structured_control_error(error.code) {
                    return Err(error);
                }
                // Scheduling is the externally visible side effect. A normal journal failure
                // cannot retract an already queued speech request, so preserve `queued`.
            }
            context.check_at(Instant::now())?;
            Ok(json!({
                "protocol": ToolResultProtocol::V1,
                "status": "queued",
                "segments": segments,
            }))
        })
    }
}

pub struct RememberCapability {
    definition: mineintent_contracts::agent::WireToolDefinition,
    memory: Arc<dyn MemoryStorePort>,
    journal: Arc<dyn CapabilityJournal>,
}

impl RememberCapability {
    pub fn new(memory: Arc<dyn MemoryStorePort>, journal: Arc<dyn CapabilityJournal>) -> Self {
        Self {
            definition: function_definition(
                REMEMBER_CAPABILITY_NAME,
                REMEMBER_CAPABILITY_DESCRIPTION,
                remember_parameters_schema(),
            ),
            memory,
            journal,
        }
    }
}

pub fn create_remember_capability(
    memory: Arc<dyn MemoryStorePort>,
    journal: Arc<dyn CapabilityJournal>,
) -> Arc<dyn ToolCapability> {
    Arc::new(RememberCapability::new(memory, journal))
}

impl ToolCapability for RememberCapability {
    fn definition(&self) -> &mineintent_contracts::agent::WireToolDefinition {
        &self.definition
    }

    fn resource(&self) -> Option<ExecutionResource> {
        Some(ExecutionResource::Memory)
    }

    fn execute<'a>(
        &'a self,
        invocation: CapabilityInvocation,
        context: CapabilityExecutionContext<'a>,
    ) -> ContractFuture<'a, Result<Value, AgentError>> {
        let memory = Arc::clone(&self.memory);
        let journal = Arc::clone(&self.journal);
        Box::pin(async move {
            context.check_at(Instant::now())?;
            let arguments = match serde_json::from_value::<RememberArguments>(Value::Object(
                invocation.arguments.clone(),
            )) {
                Ok(arguments) => arguments,
                Err(error) => return Ok(failed_result(truncate_summary(error.to_string()))),
            };
            let operation = arguments.operation;
            let operation_name = remember_operation_name(operation);
            let memory_result = match operation {
                RememberOperation::Append => {
                    let waited = await_controlled(
                        memory.append(arguments.text.expect("append was validated")),
                        context.control(),
                    )
                    .await;
                    match waited {
                        Ok(result) => result,
                        Err(error) => return Err(error),
                    }
                }
                RememberOperation::Replace => {
                    let waited = await_controlled(
                        memory.replace(
                            arguments.old_text.expect("replace was validated"),
                            arguments.new_text.expect("replace was validated"),
                        ),
                        context.control(),
                    )
                    .await;
                    match waited {
                        Ok(result) => result,
                        Err(error) => return Err(error),
                    }
                }
                RememberOperation::Rewrite => {
                    let waited = await_controlled(
                        memory.rewrite(arguments.text.expect("rewrite was validated")),
                        context.control(),
                    )
                    .await;
                    match waited {
                        Ok(result) => result,
                        Err(error) => return Err(error),
                    }
                }
            };
            if let Err(error) = memory_result {
                // MemoryStore errors are ordinary capability failures; control errors are
                // produced by `await_controlled` before this branch.
                return Ok(failed_result(truncate_summary(format!(
                    "{}: {}",
                    error.code(),
                    error
                ))));
            }
            context.check_at(Instant::now())?;
            let journal_result = journal_event(
                journal.as_ref(),
                "memory.remembered",
                object(json!({
                    "actionId": invocation.action_id,
                    "runId": invocation.run_id,
                    "toolCallId": invocation.tool_call_id,
                    "operation": operation_name,
                })),
                context.control(),
            )
            .await;
            if let Err(error) = journal_result {
                if is_structured_control_error(error.code) {
                    return Err(error);
                }
                return Ok(failed_result(truncate_summary(error.summary)));
            }
            context.check_at(Instant::now())?;
            Ok(json!({
                "protocol": ToolResultProtocol::V1,
                "status": "completed",
            }))
        })
    }
}

fn say_parameters_schema() -> Map<String, Value> {
    object(json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {
            "text": {
                "description": "要说的话，一次一句。",
                "type": "string",
                "minLength": 1,
                "maxLength": 500
            }
        },
        "required": ["text"],
        "additionalProperties": false
    }))
}

struct ReleaseAllOnDrop {
    motor: Option<Arc<dyn MinecraftMotorDriverApi>>,
}

impl ReleaseAllOnDrop {
    fn new(motor: Arc<dyn MinecraftMotorDriverApi>) -> Self {
        Self { motor: Some(motor) }
    }
}

impl Drop for ReleaseAllOnDrop {
    fn drop(&mut self) {
        let Some(motor) = self.motor.take() else {
            return;
        };
        // Cleanup is synchronous and best effort. A failing or panicking release must not keep
        // the arbiter lease alive; the dispatcher owns that lease separately.
        let _ = catch_unwind(AssertUnwindSafe(|| motor.release_all()));
    }
}

fn completed_body_result(effect: Value) -> Value {
    json!({
        "protocol": ToolResultProtocol::V1,
        "status": "completed",
        "effect": effect,
    })
}

fn measured_look_effect(before: &SelfPose, after: &SelfPose) -> Value {
    let yaw_degrees = radians_to_degrees(normalize_radians(before.yaw - after.yaw));
    let pitch_degrees = radians_to_degrees(before.pitch - after.pitch);
    json!({
        "relativeTurnDegrees": {
            "yaw": without_negative_zero(yaw_degrees),
            "pitch": without_negative_zero(pitch_degrees),
        },
        "turned": (yaw_degrees * yaw_degrees + pitch_degrees * pitch_degrees).sqrt()
            > LOOK_EFFECT_EPSILON_DEGREES,
    })
}

fn measured_move_effect(before: &SelfPose, after: &SelfPose) -> Value {
    let delta_x = after.position.x - before.position.x;
    let delta_y = after.position.y - before.position.y;
    let delta_z = after.position.z - before.position.z;
    let forward_x = -before.yaw.sin();
    let forward_z = -before.yaw.cos();
    let right_x = -forward_z;
    let right_z = forward_x;
    let relative_displacement = [
        without_negative_zero(delta_x * right_x + delta_z * right_z),
        without_negative_zero(delta_y),
        without_negative_zero(delta_x * forward_x + delta_z * forward_z),
    ];
    let distance = (delta_x * delta_x + delta_y * delta_y + delta_z * delta_z).sqrt();
    json!({
        "coordinates": "body_relative_before_move",
        "legend": "relativeDisplacement 是 [右, 上, 前] 三个格数，相对移动前的朝向，不是世界绝对坐标",
        "relativeDisplacement": relative_displacement,
        "distance": without_negative_zero(distance),
        "movement": if distance > MOVE_EFFECT_EPSILON { "changed" } else { "no_effect" },
    })
}

fn normalize_radians(value: f64) -> f64 {
    let mut normalized = value % (std::f64::consts::PI * 2.0);
    if normalized > std::f64::consts::PI {
        normalized -= std::f64::consts::PI * 2.0;
    }
    if normalized < -std::f64::consts::PI {
        normalized += std::f64::consts::PI * 2.0;
    }
    normalized
}

fn radians_to_degrees(value: f64) -> f64 {
    value * 180.0 / std::f64::consts::PI
}

fn without_negative_zero(value: f64) -> f64 {
    if value == 0.0 {
        0.0
    } else {
        value
    }
}

fn motor_direction(direction: MoveDirection) -> MotorMoveDirection {
    match direction {
        MoveDirection::Forward => MotorMoveDirection::Forward,
        MoveDirection::Back => MotorMoveDirection::Back,
        MoveDirection::Left => MotorMoveDirection::Left,
        MoveDirection::Right => MotorMoveDirection::Right,
    }
}

fn remember_operation_name(operation: RememberOperation) -> &'static str {
    match operation {
        RememberOperation::Append => "append",
        RememberOperation::Replace => "replace",
        RememberOperation::Rewrite => "rewrite",
    }
}

fn body_journal_payload(
    invocation: &CapabilityInvocation,
    tool: &'static str,
    extra: Value,
) -> JsonObject {
    let mut payload = object(json!({
        "actionId": invocation.action_id,
        "runId": invocation.run_id,
        "toolCallId": invocation.tool_call_id,
        "tool": tool,
        "startedAt": invocation.started_at,
    }));
    if let Value::Object(extra) = extra {
        payload.extend(extra);
    }
    payload
}

async fn body_failed_result<'a>(
    journal: &'a dyn CapabilityJournal,
    invocation: &CapabilityInvocation,
    tool: &'static str,
    context: CapabilityExecutionContext<'a>,
    error: AgentError,
) -> Result<Value, AgentError> {
    if is_structured_control_error(error.code) {
        return Err(error);
    }
    let summary = truncate_summary(error.summary);
    context.check_at(Instant::now())?;
    let journal_result = journal_event(
        journal,
        "body_tool.failed",
        body_journal_payload(invocation, tool, json!({"summary": summary})),
        context.control(),
    )
    .await;
    if let Err(journal_error) = journal_result {
        if is_structured_control_error(journal_error.code) {
            return Err(journal_error);
        }
    }
    context.check_at(Instant::now())?;
    Ok(failed_result(summary))
}

async fn journal_event<'a>(
    journal: &'a dyn CapabilityJournal,
    event_type: &'static str,
    payload: JsonObject,
    control: ExecutionControl<'a>,
) -> Result<(), AgentError> {
    await_controlled(journal.append(event_type.to_owned(), payload), control).await?
}

async fn await_controlled<'future, 'control, T>(
    future: ContractFuture<'future, T>,
    control: ExecutionControl<'control>,
) -> Result<T, AgentError>
where
    T: Send,
{
    control.check_at(Instant::now())?;
    let mut future = future;
    let cancellation = control.cancelled();
    let deadline = tokio::time::sleep_until(tokio::time::Instant::from_std(
        control.deadline().expires_at(),
    ));
    tokio::pin!(cancellation);
    tokio::pin!(deadline);
    tokio::select! {
        biased;
        cancellation_error = &mut cancellation => {
            Err(control.check_at(Instant::now()).err().unwrap_or(cancellation_error))
        }
        _ = &mut deadline => {
            Err(control
                .check_at(Instant::now())
                .err()
                .unwrap_or_else(AgentError::deadline_exceeded))
        }
        result = &mut future => {
            control.check_at(Instant::now())?;
            Ok(result)
        }
    }
}

fn map_motor_backend_error(error: BackendError) -> AgentError {
    match error {
        BackendError::Cancelled { .. } => AgentError::run_cancelled(),
        BackendError::DeadlineExceeded { .. } => AgentError::deadline_exceeded(),
        other => AgentError::new(
            AgentErrorCode::ToolFailed,
            truncate_summary(format!("motor operation failed: {other}")),
        ),
    }
}

fn map_journal_error(error: JournalError) -> AgentError {
    AgentError::new(
        AgentErrorCode::ToolFailed,
        truncate_summary(format!("journal append failed: {error}")),
    )
}

fn object(value: Value) -> JsonObject {
    value.as_object().cloned().unwrap_or_default()
}

struct LeaseDropGuard(Option<ResourceLeaseHandle>);

impl LeaseDropGuard {
    fn new(lease: ResourceLeaseHandle) -> Self {
        Self(Some(lease))
    }
}

impl Drop for LeaseDropGuard {
    fn drop(&mut self) {
        if let Some(lease) = self.0.take() {
            lease.release();
        }
    }
}

pub struct RegistryToolDispatcher {
    registry: Arc<ToolCapabilityRegistry>,
    arbiter: ExecutionArbiter,
    assembler: Arc<dyn CapabilityInvocationAssembler>,
    scope: Arc<CapabilityScopeAssembly>,
    observation_after: Arc<dyn ObservationAfterSource>,
}

impl RegistryToolDispatcher {
    pub fn new(
        registry: Arc<ToolCapabilityRegistry>,
        arbiter: ExecutionArbiter,
        assembler: Arc<dyn CapabilityInvocationAssembler>,
        scope: Arc<CapabilityScopeAssembly>,
    ) -> Self {
        Self {
            registry,
            arbiter,
            assembler,
            scope,
            observation_after: Arc::new(NullObservationAfter),
        }
    }

    pub fn with_observation_after(
        mut self,
        observation_after: Arc<dyn ObservationAfterSource>,
    ) -> Self {
        self.observation_after = observation_after;
        self
    }

    pub fn registry(&self) -> Arc<ToolCapabilityRegistry> {
        Arc::clone(&self.registry)
    }

    pub fn arbiter(&self) -> ExecutionArbiter {
        self.arbiter.clone()
    }
}

impl ContractToolDispatcher for RegistryToolDispatcher {
    type Observation = JsonObject;

    fn resource(&self, invocation: &ToolInvocation) -> Option<ExecutionResource> {
        self.registry
            .resolve(invocation.name.as_str())
            .and_then(|capability| capability.resource())
    }

    fn dispatch<'a>(
        &'a self,
        invocation: ToolInvocation,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<ToolExecution<Self::Observation>, AgentError>> {
        let capability = self.registry.resolve(invocation.name.as_str());
        Box::pin(async move {
            let capability = capability.ok_or_else(|| {
                AgentError::new(
                    AgentErrorCode::UnknownTool,
                    format!("unknown tool: {}", invocation.name),
                )
            })?;
            let resource = capability.resource().ok_or_else(|| {
                AgentError::new(
                    AgentErrorCode::InvalidToolDefinition,
                    format!("tool {} has no execution resource", invocation.name),
                )
            })?;
            let context = self.scope.context(control);
            context.check_at(Instant::now())?;

            let _lease = match self.arbiter.acquire(ExecutionRequest {
                resource,
                run_id: invocation.run_id.to_string(),
                tool_name: invocation.name.to_string(),
            }) {
                AcquireDecision::Granted(lease) => LeaseDropGuard::new(lease),
                AcquireDecision::Refused(refusal) => {
                    return Err(map_execution_refusal(refusal.code, refusal.summary));
                }
            };

            let assembled = self.assembler.assemble(&invocation)?;
            context.check_at(Instant::now())?;
            let capability_result =
                await_controlled(capability.execute(assembled.clone(), context), control).await?;
            let result = match capability_result {
                Ok(result) => result,
                Err(error) if is_structured_control_error(error.code) => return Err(error),
                Err(error) => failed_result(truncate_summary(error.summary)),
            };

            let observation = if resource == ExecutionResource::Body {
                let observation_context = self.scope.context(control);
                observation_context.check_at(Instant::now())?;
                let observation_result = await_controlled(
                    self.observation_after.observe_after(
                        assembled,
                        resource,
                        result.clone(),
                        observation_context,
                    ),
                    control,
                )
                .await?;
                match observation_result {
                    Ok(observation) => observation,
                    Err(error) if is_structured_control_error(error.code) => return Err(error),
                    Err(_) => None,
                }
            } else {
                None
            };
            let final_context = self.scope.context(control);
            final_context.check_at(Instant::now())?;
            Ok(ToolExecution::new(result, observation))
        })
    }
}

fn map_execution_refusal(code: ExecutionRefusalCode, summary: String) -> AgentError {
    let code = match code {
        ExecutionRefusalCode::ResourceBusy => AgentErrorCode::ResourceBusy,
        ExecutionRefusalCode::UnknownTool => AgentErrorCode::UnknownTool,
        ExecutionRefusalCode::ScopeInvalid => AgentErrorCode::ScopeInvalid,
    };
    AgentError::new(code, truncate_summary(summary))
}
