//! 分层配置。
//!
//! 优先级（高覆盖低）：**环境变量 > `.env` > 配置文件 > 内置默认**。
//! 这是 Rust 生态常见做法（如 atuin：TOML 文件叠加带前缀的环境变量）；
//! 我们不引 `config` crate，因为字段少、层次固定，直接读更容易看懂。
//!
//! - 配置文件：`MINEINTENT_CONFIG` 指定，否则工作目录下的 `mineintent.toml`；
//!   没有就跳过（全部字段都有默认或环境变量来源）。
//! - `.env`：工作目录下的 `.env`，**不覆盖**已存在的环境变量。
//! - 密钥：只从环境变量或**文件路径**读，不从配置文件读——配置文件是要进
//!   版本库的，密钥不是。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use mineintent_contracts::minecraft::{
    AuthKind, BackendTimeouts, MinecraftBackendConfig, MinecraftIdentityConfig,
    MinecraftServerConfig, ReconnectPolicy,
};
use serde::Deserialize;

const DEFAULT_RESPONSES_ENDPOINT: &str = "https://api.deepseek.com/responses";
const DEFAULT_RESPONSES_MODEL: &str = "deepseek-v4-flash";
const DEFAULT_REASONING_EFFORT: &str = "none";

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub minecraft: MinecraftBackendConfig,
    pub data_directory: PathBuf,
    pub model: ModelChoiceConfig,
}

/// 模型选择：`scripted` = 确定性假模型；`responses` = 真实模型（Responses 协议）。
#[derive(Clone, Debug)]
pub enum ModelChoiceConfig {
    Scripted,
    Responses {
        endpoint: String,
        model: String,
        reasoning_effort: String,
        api_key: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("配置无效：{name}：{reason}")]
    Invalid { name: String, reason: String },
}

fn invalid(name: impl Into<String>, reason: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        name: name.into(),
        reason: reason.into(),
    }
}

/// 配置文件形状。所有字段可选：文件只用来覆盖默认，不是必填清单。
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default)]
    minecraft: FileMinecraft,
    #[serde(default)]
    model: FileModel,
    data_dir: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileMinecraft {
    host: Option<String>,
    port: Option<u16>,
    username: Option<String>,
    world_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileModel {
    /// `scripted` 或 `responses`。
    provider: Option<String>,
    endpoint: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    /// 密钥所在文件路径。**只放路径，不放密钥本身**。
    api_key_file: Option<String>,
}

/// 读 `.env` 并注入尚未设置的环境变量。已存在的变量优先，因此
/// 命令行前缀的临时覆盖不会被文件抹掉。
fn load_dotenv(cwd: &Path) {
    let path = cwd.join(".env");
    let Ok(text) = fs::read_to_string(&path) else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if key.is_empty() || env::var_os(key).is_some() {
            continue;
        }
        // Safety: 装配期单线程，尚未起任何 worker。
        #[allow(unsafe_code)]
        unsafe {
            env::set_var(key, value)
        };
    }
}

fn read_file_config(cwd: &Path) -> Result<FileConfig, ConfigError> {
    let path = match env_trimmed("MINEINTENT_CONFIG") {
        Some(explicit) => PathBuf::from(explicit),
        None => cwd.join("mineintent.toml"),
    };
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(FileConfig::default());
    };
    toml::from_str(&text).map_err(|error| invalid(path.display().to_string(), error.to_string()))
}

fn env_trimmed(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// 环境变量优先，其次文件，最后默认。
fn pick(env_name: &str, from_file: Option<String>, default: &str) -> String {
    env_trimmed(env_name)
        .or(from_file)
        .unwrap_or_else(|| default.to_owned())
}

fn read_key_file(path: &str) -> Result<String, ConfigError> {
    let key = fs::read_to_string(path)
        .map_err(|error| invalid("密钥文件", format!("{path}：{error}")))?
        .trim()
        .to_owned();
    if key.is_empty() {
        return Err(invalid("密钥文件", format!("{path} 内容为空")));
    }
    Ok(key)
}

pub fn load_app_config(cwd: &Path) -> Result<AppConfig, ConfigError> {
    load_dotenv(cwd);
    let file = read_file_config(cwd)?;

    let world_id = pick(
        "MINEINTENT_WORLD_ID",
        file.minecraft.world_id,
        "local-world",
    );
    let host = pick("MINEINTENT_MC_HOST", file.minecraft.host, "127.0.0.1");
    let port = match env_trimmed("MINEINTENT_MC_PORT") {
        Some(raw) => raw
            .parse::<u16>()
            .map_err(|error| invalid("MINEINTENT_MC_PORT", error.to_string()))?,
        None => file.minecraft.port.unwrap_or(25565),
    };
    if port == 0 {
        return Err(invalid("MINEINTENT_MC_PORT", "端口不能为 0"));
    }
    let username = pick(
        "MINEINTENT_MC_USERNAME",
        file.minecraft.username,
        "MineIntentBot",
    );
    if username.len() > 16
        || !username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(invalid(
            "MINEINTENT_MC_USERNAME",
            "离线用户名必须是 1–16 个 ASCII 字母、数字或下划线",
        ));
    }
    match env_trimmed("MINEINTENT_MC_AUTH").as_deref() {
        None | Some("offline") => {}
        Some(other) => {
            return Err(invalid(
                "MINEINTENT_MC_AUTH",
                format!("本版本只支持 offline，实得 {other}"),
            ))
        }
    }

    let data_dir = pick("MINEINTENT_DATA_DIR", file.data_dir, ".mineintent");

    let provider = pick("MINEINTENT_MODEL", file.model.provider, "scripted");
    let model = match provider.as_str() {
        "scripted" => ModelChoiceConfig::Scripted,
        "responses" => {
            let api_key = match env_trimmed("MINEINTENT_MODEL_API_KEY") {
                Some(key) => key,
                None => {
                    let path = env_trimmed("MINEINTENT_MODEL_API_KEY_FILE")
                        .or(file.model.api_key_file)
                        .ok_or_else(|| {
                            invalid(
                                "MINEINTENT_MODEL_API_KEY",
                                "responses provider 需要 MINEINTENT_MODEL_API_KEY，\
                                 或 MINEINTENT_MODEL_API_KEY_FILE / 配置文件的 model.api_key_file 指向密钥文件",
                            )
                        })?;
                    read_key_file(&path)?
                }
            };
            ModelChoiceConfig::Responses {
                endpoint: pick(
                    "MINEINTENT_MODEL_ENDPOINT",
                    file.model.endpoint,
                    DEFAULT_RESPONSES_ENDPOINT,
                ),
                model: pick(
                    "MINEINTENT_MODEL_NAME",
                    file.model.model,
                    DEFAULT_RESPONSES_MODEL,
                ),
                reasoning_effort: pick(
                    "MINEINTENT_MODEL_REASONING_EFFORT",
                    file.model.reasoning_effort,
                    DEFAULT_REASONING_EFFORT,
                ),
                api_key,
            }
        }
        other => {
            return Err(invalid(
                "MINEINTENT_MODEL",
                format!("未知 provider {other}（可用：scripted | responses）"),
            ))
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

/// 与 TS oracle `defaultMinecraftBackendConfig` 同值。
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
