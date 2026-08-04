//! 环境变量配置，等价迁移自 TS oracle `src/app/config.ts`。
//! 差异（均有裁定）：agent-service/tool-bridge 边界已删（更新-01，进程内
//! AgentRunner）；版本钉 26.1.2/775（任务书）；auth 仅 offline（发布范围限制）。

use std::env;
use std::path::{Path, PathBuf};

use mineintent_contracts::minecraft::{
    AuthKind, BackendTimeouts, MinecraftBackendConfig, MinecraftIdentityConfig,
    MinecraftServerConfig, ReconnectPolicy,
};

/// oracle 默认值：src/minecraft/config.ts:32-41。
fn default_timeouts() -> BackendTimeouts {
    BackendTimeouts {
        connect_ms: 10_000,
        login_ms: 20_000,
        spawn_ms: 30_000,
        stop_ms: 5_000,
    }
}

fn default_reconnect() -> ReconnectPolicy {
    ReconnectPolicy {
        enabled: true,
        initial_delay_ms: 1_000,
        multiplier: 2.0,
        max_delay_ms: 30_000,
        jitter_ratio: 0.2,
        stable_reset_ms: 60_000,
    }
}

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub minecraft: MinecraftBackendConfig,
    pub data_directory: PathBuf,
    pub model: ModelChoiceConfig,
}

/// 模型选择：scripted = gate b 确定性假模型；deepseek = 真模型加分项。
#[derive(Clone, Debug)]
pub enum ModelChoiceConfig {
    Scripted,
    DeepSeek { endpoint: String, model: String },
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("环境变量 {name} 无效：{reason}")]
    Invalid { name: &'static str, reason: String },
}

fn env_trimmed(name: &'static str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// 与 oracle 相同：数据目录相对 CWD 解析，默认 `.mineintent`（NEW-02 裁定）。
pub fn load_app_config(cwd: &Path) -> Result<AppConfig, ConfigError> {
    let world_id = env_trimmed("MINEINTENT_WORLD_ID").unwrap_or_else(|| "local-world".to_owned());
    let host = env_trimmed("MINEINTENT_MC_HOST").unwrap_or_else(|| "127.0.0.1".to_owned());
    let port = match env_trimmed("MINEINTENT_MC_PORT") {
        None => 25_565,
        Some(raw) => raw
            .parse::<u16>()
            .ok()
            .filter(|port| *port >= 1)
            .ok_or_else(|| ConfigError::Invalid {
                name: "MINEINTENT_MC_PORT",
                reason: format!("{raw:?} 不是 1-65535 的端口"),
            })?,
    };
    let username =
        env_trimmed("MINEINTENT_MC_USERNAME").unwrap_or_else(|| "MineIntentBot".to_owned());
    if username.chars().count() > 64 {
        return Err(ConfigError::Invalid {
            name: "MINEINTENT_MC_USERNAME",
            reason: "长度超过 64".to_owned(),
        });
    }
    match env_trimmed("MINEINTENT_MC_AUTH").as_deref() {
        None | Some("offline") => {}
        Some(other) => {
            // 正版登录是移植后议题（2026-08-01 裁定：本次只做 offline）。
            return Err(ConfigError::Invalid {
                name: "MINEINTENT_MC_AUTH",
                reason: format!("{other:?} 不受支持，本版本仅支持 offline"),
            });
        }
    }
    let data_dir = env_trimmed("MINEINTENT_DATA_DIR").unwrap_or_else(|| ".mineintent".to_owned());

    let model = match env_trimmed("MINEINTENT_MODEL").as_deref() {
        None | Some("scripted") => ModelChoiceConfig::Scripted,
        Some("deepseek") => ModelChoiceConfig::DeepSeek {
            endpoint: env_trimmed("MINEINTENT_DEEPSEEK_URL")
                .unwrap_or_else(|| "https://api.deepseek.com/chat/completions".to_owned()),
            model: env_trimmed("MINEINTENT_DEEPSEEK_MODEL")
                .unwrap_or_else(|| "deepseek-chat".to_owned()),
        },
        Some(other) => {
            return Err(ConfigError::Invalid {
                name: "MINEINTENT_MODEL",
                reason: format!("未知模型选择 {other}（可用：scripted | deepseek）"),
            })
        }
    };
    Ok(AppConfig {
        minecraft: MinecraftBackendConfig {
            world_id,
            server: MinecraftServerConfig {
                host,
                port,
                version: "26.1.2".to_owned(),
            },
            identity: MinecraftIdentityConfig {
                username,
                auth: AuthKind::Offline,
                profiles_folder: None,
            },
            timeouts: default_timeouts(),
            reconnect: default_reconnect(),
        },
        data_directory: cwd.join(data_dir),
        model,
    })
}
