use serde::{Deserialize, Serialize};

use super::BackendError;

pub const TARGET_MINECRAFT_VERSION: &str = "26.1.2";
pub const TARGET_PROTOCOL_VERSION: u32 = 775;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    Offline,
    Microsoft,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MinecraftBackendConfig {
    pub world_id: String,
    pub server: MinecraftServerConfig,
    pub identity: MinecraftIdentityConfig,
    pub timeouts: BackendTimeouts,
    pub reconnect: ReconnectPolicy,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MinecraftServerConfig {
    pub host: String,
    pub port: u16,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MinecraftIdentityConfig {
    pub username: String,
    pub auth: AuthKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profiles_folder: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackendTimeouts {
    pub connect_ms: u64,
    pub login_ms: u64,
    pub spawn_ms: u64,
    pub stop_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReconnectPolicy {
    pub enabled: bool,
    pub initial_delay_ms: u64,
    pub multiplier: f64,
    pub max_delay_ms: u64,
    pub jitter_ratio: f64,
    pub stable_reset_ms: u64,
}

impl MinecraftBackendConfig {
    /// 按 TS parser 校验并返回归一化后的配置。
    ///
    /// 本方法消费原值，避免调用方在校验成功后继续误用尚未 trim 的 DTO。
    /// `profilesFolder` 与 oracle 一样只检查原字符串非空，不做 trim。Microsoft
    /// 枚举可反序列化，但在这里明确返回 `UnsupportedAuth`。
    pub fn validate_and_normalize(mut self) -> Result<Self, BackendError> {
        self.world_id = self.world_id.trim().to_owned();
        self.server.host = self.server.host.trim().to_owned();
        self.identity.username = self.identity.username.trim().to_owned();

        if self.world_id.is_empty() || javascript_string_len(&self.world_id) > 128 {
            return Err(BackendError::InvalidConfig {
                field: "worldId".to_owned(),
                message: "must contain from 1 to 128 UTF-16 code units after trim".to_owned(),
            });
        }
        if self.server.host.is_empty() {
            return Err(BackendError::InvalidConfig {
                field: "server.host".to_owned(),
                message: "must not be empty".to_owned(),
            });
        }
        if self.server.port == 0 {
            return Err(BackendError::InvalidConfig {
                field: "server.port".to_owned(),
                message: "must be greater than zero".to_owned(),
            });
        }
        if self.server.version != TARGET_MINECRAFT_VERSION {
            return Err(BackendError::UnsupportedVersion {
                expected: TARGET_MINECRAFT_VERSION.to_owned(),
                actual: self.server.version.clone(),
            });
        }
        if self.identity.username.is_empty() || javascript_string_len(&self.identity.username) > 64
        {
            return Err(BackendError::InvalidConfig {
                field: "identity.username".to_owned(),
                message: "must contain from 1 to 64 UTF-16 code units after trim".to_owned(),
            });
        }
        if self
            .identity
            .profiles_folder
            .as_ref()
            .is_some_and(String::is_empty)
        {
            return Err(BackendError::InvalidConfig {
                field: "identity.profilesFolder".to_owned(),
                message: "must not be empty when present".to_owned(),
            });
        }
        for (field, value) in [
            ("timeouts.connectMs", self.timeouts.connect_ms),
            ("timeouts.loginMs", self.timeouts.login_ms),
            ("timeouts.spawnMs", self.timeouts.spawn_ms),
            ("timeouts.stopMs", self.timeouts.stop_ms),
        ] {
            if value == 0 {
                return Err(BackendError::InvalidConfig {
                    field: field.to_owned(),
                    message: "must be greater than zero".to_owned(),
                });
            }
        }
        if !self.reconnect.multiplier.is_finite() || self.reconnect.multiplier < 1.0 {
            return Err(BackendError::InvalidConfig {
                field: "reconnect.multiplier".to_owned(),
                message: "must be finite and at least 1".to_owned(),
            });
        }
        if !self.reconnect.jitter_ratio.is_finite()
            || !(0.0..=1.0).contains(&self.reconnect.jitter_ratio)
        {
            return Err(BackendError::InvalidConfig {
                field: "reconnect.jitterRatio".to_owned(),
                message: "must be finite and between 0 and 1".to_owned(),
            });
        }
        if self.identity.auth != AuthKind::Offline {
            return Err(BackendError::UnsupportedAuth {
                auth: self.identity.auth,
            });
        }
        // The three delay fields are u64, which exactly encodes the oracle's integer/nonnegative
        // constraint. In particular, initialDelayMs is not ordered against maxDelayMs.
        Ok(self)
    }
}

/// JavaScript/Zod string length is measured in UTF-16 code units rather than UTF-8 bytes.
fn javascript_string_len(value: &str) -> usize {
    value.encode_utf16().count()
}
