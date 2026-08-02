//! 模型可见 capability 与 backend 适配器。
//!
//! 本模块只提供 `view` 这一条生产能力及其可复用的 viewport reader。它不负责工具
//! registry、dispatcher 或 participant/app 组装；后续装配层可以把本模块公开的对象接入
//! 自己的 `ToolDispatcher`。

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use mineintent_contracts::{
    agent::{AgentError, AgentErrorCode, ContractFuture, ExecutionControl},
    capability::{
        view_parameters_schema, CapabilityExecutionContext, CapabilityInvocation,
        ExecutionResource, ToolCapability, ToolResultProtocol, ViewArguments, ViewMode,
    },
    minecraft::{
        BackendError, BoxFuture, CancellationSignal as BackendCancellationSignal,
        Deadline as BackendDeadline, DirectedViewportError, DirectedViewportProjection,
        MinecraftBackendApi, OperationControl, ProtocolObservationSource, ViewportProjection,
    },
};
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
    Full(ViewportProjection),
    Directed(DirectedViewportProjection),
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
    ) -> ContractFuture<'a, Result<ViewportProjection, AgentError>> {
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
            Ok(read.projection)
        })
    }

    pub fn read_directed<'a>(
        &'a self,
        positions: Vec<(i32, i32, i32)>,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<DirectedViewportProjection, AgentError>> {
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
            Ok(read)
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
