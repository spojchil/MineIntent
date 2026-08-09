//! 用同一套场景验证 OpenAI Chat、OpenAI Responses 与 Anthropic Messages 兼容入口。
//!
//! `MODEL_API_KEY` 仅在真正运行示例时读取，测试与编译不会访问它。模型名、完整
//! endpoint 与日志开关由 `config` 模块统一解析，协议细节位于 `adapter` 模块。

mod adapter;
mod config;
mod harness;

use std::error::Error;

use config::SmokeConfig;
use harness::run_smoke;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let config = SmokeConfig::from_process()?;

    for protocol in config.protocols.iter().copied() {
        run_smoke(protocol, &config).await?;
    }
    Ok(())
}
