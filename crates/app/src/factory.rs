//! 每 wake 装配 runner 的生产工厂：同一 registry 派生 definitions 与 dispatch
//! （B4 单源纪律），scope guard 对照活 runtime 的 scope/generation。

use std::sync::{Arc, OnceLock, Weak};

use mineintent_contracts::agent::{AgentError, AgentErrorCode, ModelName, ToolInvocation};
use mineintent_contracts::capability::{ScopeGuard, ToolCapabilityRegistry};
use mineintent_middle::agent::{BackendRoundViewportSampler, ConcreteAgentRunner};
use mineintent_middle::capability::{
    CapabilityActionIdSource, CapabilityScopeAssembly, CapabilityUtcTimestampSource,
    ExplicitCapabilityInvocationAssembler, RegistryToolDispatcher,
};
use mineintent_middle::participant::{
    ParticipantAgentAssembly, ParticipantAgentFactory, ParticipantRuntime, ParticipantScope,
    ParticipantScopedAgentRunner,
};
use mineintent_middle::participant::{ParticipantClock, SystemUtcClock};

use mineintent_middle::capability::ViewportReader;

use crate::model::{JsonObject, ScriptedModelProvider};
use crate::model::{ResponsesConfig, ResponsesModelProvider};

/// 工厂侧模型选择（key 已在装配时校验注入）。
pub enum AppModelChoice {
    Scripted(Vec<JsonObject>),
    Responses {
        config: ResponsesConfig,
        api_key: String,
    },
}

type AppRuntime = ParticipantRuntime<ParticipantAgentAssembly>;

fn scope_invalid() -> AgentError {
    AgentError::new(AgentErrorCode::ScopeInvalid, "scope_invalid")
}

/// 生产 action id：UUID v4，与 oracle 的 randomUUID 同义。
struct UuidActionIds;

impl CapabilityActionIdSource for UuidActionIds {
    fn next_action_id(&self, _invocation: &ToolInvocation) -> Result<String, AgentError> {
        Ok(uuid::Uuid::new_v4().to_string())
    }
}

/// 生产 UTC 时间戳：复用 Participant 的系统时钟口径。
struct SystemUtcSource;

impl CapabilityUtcTimestampSource for SystemUtcSource {
    fn now_utc(&self) -> Result<String, AgentError> {
        Ok(SystemUtcClock.now())
    }
}

/// wake 时快照 (scope, generation)，执行时对照活 runtime。runtime 已灭 =
/// 一律非当前（fail closed）。
struct RuntimeScopeGuard {
    runtime: Weak<AppRuntime>,
    scope: ParticipantScope,
    generation: u64,
}

impl ScopeGuard for RuntimeScopeGuard {
    fn check_current(&self) -> Result<(), AgentError> {
        if self.is_current() {
            Ok(())
        } else {
            Err(scope_invalid())
        }
    }

    fn is_current(&self) -> bool {
        self.runtime.upgrade().is_some_and(|runtime| {
            runtime.current_generation() == self.generation
                && runtime.current_scope().as_ref() == Some(&self.scope)
        })
    }
}

pub struct AppAgentFactory {
    registry: Arc<ToolCapabilityRegistry>,
    choice: AppModelChoice,
    model_name: ModelName,
    viewport_reader: Arc<ViewportReader>,
    runtime: OnceLock<Weak<AppRuntime>>,
}

impl AppAgentFactory {
    pub fn new(
        registry: Arc<ToolCapabilityRegistry>,
        choice: AppModelChoice,
        model_name: ModelName,
        viewport_reader: Arc<ViewportReader>,
    ) -> Self {
        Self {
            registry,
            choice,
            model_name,
            viewport_reader,
            runtime: OnceLock::new(),
        }
    }

    /// runtime 构造在工厂之后（装配环），启动前必须回填一次。
    pub fn bind_runtime(&self, runtime: &Arc<AppRuntime>) {
        let _ = self.runtime.set(Arc::downgrade(runtime));
    }
}

impl ParticipantAgentFactory for AppAgentFactory {
    fn registry(&self) -> Arc<ToolCapabilityRegistry> {
        Arc::clone(&self.registry)
    }

    fn build(
        &self,
        scope: &ParticipantScope,
        generation: u64,
        trigger_event_id: &str,
    ) -> Result<Arc<dyn ParticipantScopedAgentRunner>, AgentError> {
        let runtime = self.runtime.get().cloned().ok_or_else(scope_invalid)?;
        let scope_guard: Arc<dyn ScopeGuard> = Arc::new(RuntimeScopeGuard {
            runtime,
            scope: scope.clone(),
            generation,
        });
        let dispatcher = RegistryToolDispatcher::new(
            Arc::clone(&self.registry),
            Arc::new(ExplicitCapabilityInvocationAssembler::new(
                Arc::new(UuidActionIds),
                Arc::new(SystemUtcSource),
            )),
            Arc::new(CapabilityScopeAssembly::new(
                scope.world_id.clone(),
                trigger_event_id.to_owned(),
                scope_guard,
            )),
        );
        // 轮末一帧（更新-05/06）：真实 sampler 与 view capability 共用同一 reader。
        let sampler = BackendRoundViewportSampler::new(Arc::clone(&self.viewport_reader));
        let runner: Arc<dyn ParticipantScopedAgentRunner> = match &self.choice {
            AppModelChoice::Scripted(script) => {
                Arc::new(ConcreteAgentRunner::with_viewport_sampler(
                    ScriptedModelProvider::new(script.clone()),
                    dispatcher,
                    self.model_name.clone(),
                    sampler,
                ))
            }
            AppModelChoice::Responses { config, api_key } => {
                Arc::new(ConcreteAgentRunner::with_viewport_sampler(
                    ResponsesModelProvider::new(config.clone(), api_key.clone())
                        .map_err(|error| AgentError::new(AgentErrorCode::ProviderFailed, error))?,
                    dispatcher,
                    self.model_name.clone(),
                    sampler,
                ))
            }
        };
        Ok(runner)
    }
}
