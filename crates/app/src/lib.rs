//! MineIntent 组合根，等价迁移自 TS oracle `src/app/mineintent-app.ts`。
//! 差异（均有裁定）：模型走进程内 AgentRunner + 脚本化假 provider（更新-01
//! 删 Python/HTTP 边界；gate b 完成门槛为假模型纵向）；debug HTTP 服务面
//! 未迁（DebugStateStore 保留，HTTP 面与 W08 系列同期另议）。
//! 启动顺序：backend start→Ready → 装配 → runtime start_worker；
//! 停机顺序：runtime stop → frame source dispose → backend stop（best-effort，
//! 聚合首个错误，与 oracle closeBestEffort 同义）。

mod config;
mod factory;
mod model;

use std::sync::Arc;
use std::time::Duration;

pub use config::{load_app_config, AppConfig, ConfigError};

use mineintent_backend::facade::MinecraftBackendFacade;
use mineintent_contracts::agent::{
    ModelName, PromptTemplateKey, PromptTemplateRef, PromptTemplateVersion,
};
use mineintent_contracts::minecraft::{
    BackendError, BoxFuture, CancellationSignal, MinecraftBackendApi, OperationControl,
};
use mineintent_middle::capability::{
    build_production_capability_registry, ProductionCapabilityServices, ViewportReader,
};
use mineintent_middle::events::JsonlEventJournal;
use mineintent_middle::memory::MemoryStore;
use mineintent_middle::participant::{
    ParticipantAgentAssembly, ParticipantRuntime, ParticipantRuntimeConfig,
    ProductionParticipantFrameSource, SystemUtcClock, WakeRegistry,
};
use mineintent_middle::speech::{SpeechScheduler, SpeechSchedulerOptions, SpeechTransport};
use mineintent_middle::telemetry::DebugStateStore;

use factory::AppAgentFactory;
use model::default_vertical_script;

/// oracle 的 run 期限由模型服务边界承担；进程内形态下由 runtime 统一持有。
/// 值与 TS 假模型集成测试的上限一致，Paper 纵向实测时按需登记调整。
const RUN_DEADLINE: Duration = Duration::from_secs(120);

struct NeverCancelled;

impl CancellationSignal for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(std::future::pending())
    }
}

struct BackendChatTransport {
    backend: Arc<dyn MinecraftBackendApi>,
}

impl SpeechTransport for BackendChatTransport {
    type Error = BackendError;

    fn send(&self, message: &str) -> Result<(), Self::Error> {
        self.backend.send_chat(message.to_owned())
    }
}

pub struct MineIntentApp {
    backend: Arc<dyn MinecraftBackendApi>,
    runtime: Arc<ParticipantRuntime<ParticipantAgentAssembly>>,
    frame_source: Arc<ProductionParticipantFrameSource>,
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("backend 启动失败：{0}")]
    Backend(#[from] BackendError),
    #[error("装配失败：{0}")]
    Assembly(String),
}

impl MineIntentApp {
    pub async fn start(config: AppConfig) -> Result<Self, AppError> {
        std::fs::create_dir_all(&config.data_directory)
            .map_err(|error| AppError::Assembly(format!("数据目录不可用：{error}")))?;

        let facade = MinecraftBackendFacade::new(config.minecraft.clone())?;
        let backend: Arc<dyn MinecraftBackendApi> = Arc::new(facade);

        let control = OperationControl::new(Arc::new(NeverCancelled), None);
        let ready = backend.start(control).await?;

        let session_id = uuid::Uuid::new_v4().to_string();
        let journal = Arc::new(
            JsonlEventJournal::new(
                config.data_directory.join("events.jsonl"),
                config.minecraft.world_id.clone(),
                session_id.clone(),
            )
            .map_err(|error| AppError::Assembly(format!("journal 初始化失败：{error}")))?,
        );
        let memory = Arc::new(MemoryStore::new(config.data_directory.join("memory.txt")));
        let speech = Arc::new(
            SpeechScheduler::new(
                BackendChatTransport {
                    backend: Arc::clone(&backend),
                },
                SpeechSchedulerOptions::default(),
            )
            .map_err(|error| AppError::Assembly(format!("speech 初始化失败：{error}")))?,
        );
        let viewport_reader = Arc::new(ViewportReader::new(Arc::clone(&backend)));
        let registry = build_production_capability_registry(ProductionCapabilityServices::new(
            Arc::clone(&backend),
            viewport_reader,
            journal.clone(),
            speech.clone(),
            memory.clone(),
        ))
        .map_err(|error| AppError::Assembly(format!("capability registry 构建失败：{error}")))?;

        let factory = Arc::new(AppAgentFactory::new(
            registry,
            default_vertical_script(),
            ModelName::new("scripted-vertical")
                .map_err(|error| AppError::Assembly(format!("模型名无效：{error}")))?,
        ));
        let assembly = Arc::new(ParticipantAgentAssembly::new(Arc::clone(&factory)
            as Arc<dyn mineintent_middle::participant::ParticipantAgentFactory>));
        let frame_source = Arc::new(
            ProductionParticipantFrameSource::new(Arc::clone(&backend))
                .map_err(|error| AppError::Assembly(format!("frame source 初始化失败：{error}")))?,
        );

        let runtime = ParticipantRuntime::try_new(ParticipantRuntimeConfig {
            backend: Arc::clone(&backend),
            agent: assembly,
            frame_source: frame_source.clone(),
            memory: memory.clone(),
            journal,
            speech,
            debug: Arc::new(DebugStateStore::default()),
            clock: Arc::new(SystemUtcClock),
            // NEW-04 纪律：v2 由装配方显式选择，不做隐式默认。
            prompt_template: PromptTemplateRef {
                key: PromptTemplateKey::new("participant-system")
                    .map_err(|error| AppError::Assembly(format!("prompt key 无效：{error}")))?,
                version: PromptTemplateVersion::new("v2")
                    .map_err(|error| AppError::Assembly(format!("prompt version 无效：{error}")))?,
            },
            run_deadline: RUN_DEADLINE,
            wake_registry: WakeRegistry::initial(),
            run_id_namespace: format!("{}:{}", config.minecraft.world_id, session_id),
        })
        .map_err(|error| AppError::Assembly(format!("runtime 构建失败：{error}")))?;

        factory.bind_runtime(&runtime);
        runtime
            .start_worker()
            .map_err(|error| AppError::Assembly(format!("runtime 启动失败：{error}")))?;

        // BackendReady 的连接细节由 backend 生命周期事件承载；入口只需成功信号。
        drop(ready);
        Ok(Self {
            backend,
            runtime,
            frame_source,
        })
    }

    /// 停机顺序照 oracle：runtime 先收束（内部取消 run/speech、释放身体），
    /// 再 dispose frame source（退订 backend），最后停 backend。
    pub async fn stop(&self, reason: &str) -> Result<(), AppError> {
        let mut first_error: Option<AppError> = None;
        if let Err(error) = self.runtime.stop().await {
            first_error.get_or_insert(AppError::Assembly(format!("runtime 停止失败：{error}")));
        }
        self.frame_source.dispose();
        let control = OperationControl::new(Arc::new(NeverCancelled), None);
        if let Err(error) = self.backend.stop(reason.to_owned(), control).await {
            first_error.get_or_insert(AppError::Backend(error));
        }
        match first_error {
            None => Ok(()),
            Some(error) => Err(error),
        }
    }
}
