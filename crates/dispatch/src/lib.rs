//! 工具编排：内核工具端口的唯一实现方。
//!
//! 工具模块作为供应者注册进来；本层按名路由、按批内顺序逐个执行。
//! 互斥域的占用账本（[`Occupancy`]）归本层持有——工具模块只做状态转换
//! （开屏占域、关屏释放），谁占着什么、要不要压制由编排裁决。
//! 唯一的跨域规则：界面域被占时拒绝其他身体域的调用。
//!
//! 中断不回滚：执行同步于 submit，不存在半执行的调用；已执行调用的
//! 效果（含占域转换）经由内核的中断回执告知模型，占用账本保持真实状态。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use agent::{
    AbortedToolBatch, AbortedToolCall, AbortedToolCallOutcome, AgentError, IncrementalToolBatch,
    PortFuture, ToolBatchAbortReason, ToolBatchStart, ToolCall, ToolCallBatch, ToolCallSlot,
    ToolDefinition, ToolName, ToolResult, ToolResultBatch, ToolRuntime,
};

/// 编排眼中的工具类别。内心与感知合并为 Free（都不受压制、不占域）；
/// 身体类带互斥域，受界面压制。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolClass {
    Free,
    Body { domain: Domain },
}

/// 身体互斥域。2026-08-10 客户端考证裁定：手（攻击/挖掘/使用三态彼此互斥）
/// 独立成域，与移动正交；朝向独立于移动（寻路与挖掘期间由各自任务牵引）。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Domain {
    Screen,
    Movement,
    Facing,
    Hand,
}

/// 互斥域占用账本。工具模块在自己的状态转换处调用 occupy/release；
/// 压制判断只发生在编排内部。
#[derive(Default)]
pub struct Occupancy {
    occupied: Mutex<HashSet<Domain>>,
}

impl Occupancy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn occupy(&self, domain: Domain) {
        self.occupied.lock().expect("占用账本锁中毒").insert(domain);
    }

    pub fn release(&self, domain: Domain) {
        self.occupied
            .lock()
            .expect("占用账本锁中毒")
            .remove(&domain);
    }

    pub fn is_occupied(&self, domain: Domain) -> bool {
        self.occupied
            .lock()
            .expect("占用账本锁中毒")
            .contains(&domain)
    }
}

/// 工具供应者：工具模块注册进编排的身份。
pub trait ToolProvider: Send + Sync {
    fn tools(&self) -> Vec<(ToolDefinition, ToolClass)>;

    fn call<'a>(&'a self, call: ToolCall) -> PortFuture<'a, ToolResult>;
}

/// 注册期错误：工具名冲突在组合根构造时就失败，不留到运行时。
#[derive(Debug)]
pub struct RegistrationError {
    pub summary: String,
}

impl std::fmt::Display for RegistrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.summary)
    }
}

impl std::error::Error for RegistrationError {}

pub struct Dispatcher {
    providers: Vec<Arc<dyn ToolProvider>>,
    /// 工具名 → (供应者下标, 类别)。
    routes: HashMap<ToolName, (usize, ToolClass)>,
    occupancy: Arc<Occupancy>,
}

impl Dispatcher {
    pub fn new(
        providers: Vec<Arc<dyn ToolProvider>>,
        occupancy: Arc<Occupancy>,
    ) -> Result<Self, RegistrationError> {
        let mut routes = HashMap::new();
        for (index, provider) in providers.iter().enumerate() {
            for (definition, class) in provider.tools() {
                if routes
                    .insert(definition.name.clone(), (index, class))
                    .is_some()
                {
                    return Err(RegistrationError {
                        summary: format!("工具名重复注册：{}", definition.name.as_str()),
                    });
                }
            }
        }
        Ok(Self {
            providers,
            routes,
            occupancy,
        })
    }

    async fn execute_one(&self, call: ToolCall) -> ToolResult {
        let Some((provider_index, class)) = self.routes.get(&call.name) else {
            return ToolResult::failure(
                call.id,
                format!(
                    "没有名为 {} 的工具；请改用工具列表中的名字",
                    call.name.as_str()
                ),
            );
        };

        if let ToolClass::Body { domain } = class {
            if *domain != Domain::Screen && self.occupancy.is_occupied(Domain::Screen) {
                return ToolResult::failure(
                    call.id,
                    "有界面开着，无法移动或与世界交互；先关闭界面再行动",
                );
            }
        }
        self.providers[*provider_index].call(call).await
    }
}

impl ToolRuntime for Dispatcher {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.providers
            .iter()
            .flat_map(|provider| provider.tools())
            .map(|(definition, _)| definition)
            .collect()
    }

    fn dispatch<'a>(
        &'a self,
        batch: ToolCallBatch,
    ) -> PortFuture<'a, Result<ToolResultBatch, AgentError>> {
        Box::pin(async move {
            let mut results = Vec::with_capacity(batch.calls.len());
            for call in batch.calls {
                results.push(self.execute_one(call).await);
            }
            Ok(ToolResultBatch { results })
        })
    }

    fn begin_incremental<'a>(
        &'a self,
        batch: ToolBatchStart,
    ) -> PortFuture<'a, Result<Option<Box<dyn IncrementalToolBatch + 'a>>, AgentError>> {
        Box::pin(async move {
            let run: Box<dyn IncrementalToolBatch + 'a> = Box::new(IncrementalRun {
                dispatcher: self,
                start: batch,
                executed: Vec::new(),
            });
            Ok(Some(run))
        })
    }
}

/// 一次增量批：调用封口送达即执行，结果按槽序累积。
struct IncrementalRun<'a> {
    dispatcher: &'a Dispatcher,
    start: ToolBatchStart,
    executed: Vec<(ToolCallSlot, ToolResult)>,
}

impl<'a> IncrementalToolBatch for IncrementalRun<'a> {
    fn submit<'b>(
        &'b mut self,
        call: agent::IncrementalToolCall,
    ) -> PortFuture<'b, Result<(), AgentError>> {
        Box::pin(async move {
            let result = self.dispatcher.execute_one(call.call).await;
            self.executed.push((call.slot, result));
            Ok(())
        })
    }

    fn calls_sealed<'b>(&'b mut self, _call_count: u32) -> PortFuture<'b, Result<(), AgentError>> {
        Box::pin(async { Ok(()) })
    }

    fn commit<'b>(self: Box<Self>) -> PortFuture<'b, Result<ToolResultBatch, AgentError>>
    where
        Self: 'b,
    {
        Box::pin(async move {
            let results = self
                .executed
                .into_iter()
                .map(|(_, result)| result)
                .collect();
            Ok(ToolResultBatch { results })
        })
    }

    fn abort<'b>(
        self: Box<Self>,
        _reason: ToolBatchAbortReason,
    ) -> PortFuture<'b, Result<AbortedToolBatch, AgentError>>
    where
        Self: 'b,
    {
        Box::pin(async move {
            // 执行同步于 submit：凡已提交必已执行，结论全部确定。
            // CancelledBeforeStart 与 OutcomeUnknown 在进程内实现中不可达。
            // 占用账本不回滚——已执行的开屏是回执里的既成事实，账本保持真实。
            let calls = self
                .executed
                .into_iter()
                .map(|(slot, result)| AbortedToolCall {
                    slot,
                    call_id: result.call_id.clone(),
                    outcome: AbortedToolCallOutcome::Settled(result),
                })
                .collect();
            Ok(AbortedToolBatch {
                batch_attempt_id: self.start.batch_attempt_id,
                calls,
            })
        })
    }
}

#[cfg(test)]
mod tests;
