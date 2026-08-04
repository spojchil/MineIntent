//! DeepSeek 真模型 provider（gate b 加分项）。
//!
//! 当前 vendored 依赖图没有 HTTP/TLS 栈（azalea 精简特性剪掉了 reqwest），
//! 纵向阶段经系统 `curl` 走 HTTPS；生产 HTTP 栈选型属迁回后的正式议题。
//! API key 只经环境变量注入 curl 配置文件（0 权限泄露面：配置文件写进
//! 数据目录、用后即删，key 不上命令行、不进日志）。

use std::path::PathBuf;
use std::time::Instant;

use mineintent_contracts::agent::{
    AgentError, AgentErrorCode, ContractFuture, ExecutionControl, ModelProvider, ModelUsage,
};
use mineintent_middle::agent::{AgentModelRequest, ModelCompletion};

use crate::devlog;

use serde_json::{json, Map, Value};

#[derive(Clone, Debug)]
pub struct DeepSeekConfig {
    pub endpoint: String,
    pub model: String,
    pub scratch_dir: PathBuf,
}

pub struct DeepSeekModelProvider {
    config: DeepSeekConfig,
    api_key: String,
}

fn provider_error(message: impl Into<String>) -> AgentError {
    let message = message.into();
    devlog::log("model", format!("provider 失败：{message}"));
    AgentError::new(AgentErrorCode::ProviderFailed, message)
}

impl DeepSeekModelProvider {
    pub fn new(config: DeepSeekConfig, api_key: String) -> Self {
        Self { config, api_key }
    }

    fn build_payload(&self, request: &AgentModelRequest) -> Result<Value, AgentError> {
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                serde_json::to_value(tool).map_err(|error| provider_error(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!({
            "model": self.config.model,
            "messages": request.messages,
            "tools": tools,
        }))
    }
}

impl ModelProvider for DeepSeekModelProvider {
    type Request = AgentModelRequest;
    type Response = ModelCompletion;

    fn complete<'a>(
        &'a self,
        request: Self::Request,
        _control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<Self::Response, AgentError>> {
        Box::pin(async move {
            let payload = self.build_payload(&request)?;
            let stamp = format!(
                "{}-{}",
                request.run_id.as_str().replace(['/', '\\'], "_"),
                uuid::Uuid::new_v4().simple()
            );
            devlog::log(
                "model",
                format!(
                    "请求 run={} 消息数={} 工具数={}",
                    request.run_id.as_str(),
                    request.messages.len(),
                    request.tools.len()
                ),
            );
            devlog::dump_model_io(&stamp, "req", &payload.to_string());
            let io_dir = self.config.scratch_dir.join("model-io");
            tokio::fs::create_dir_all(&io_dir)
                .await
                .map_err(|error| provider_error(format!("model-io 目录创建失败：{error}")))?;
            let payload_path = io_dir.join(format!("req-{stamp}.json"));
            let config_path = io_dir.join(format!("cfg-{stamp}"));
            tokio::fs::write(&payload_path, payload.to_string())
                .await
                .map_err(|error| provider_error(format!("请求写盘失败：{error}")))?;
            // key 走 curl 配置文件而非命令行参数，避免进程表可见。
            let curl_config = format!(
                "url = \"{}\"\nheader = \"Content-Type: application/json\"\nheader = \"Authorization: Bearer {}\"\ndata = \"@{}\"\n",
                self.config.endpoint,
                self.api_key,
                payload_path.display().to_string().replace('\\', "/"),
            );
            tokio::fs::write(&config_path, curl_config)
                .await
                .map_err(|error| provider_error(format!("curl 配置写盘失败：{error}")))?;
            let started = Instant::now();
            let output = tokio::process::Command::new("curl")
                .arg("-sS")
                .arg("--max-time")
                .arg("110")
                .arg("--config")
                .arg(&config_path)
                .output()
                .await;
            // 配置文件含 key，任何模式下都必须删；请求正文在 dev 模式下
            // 已另存一份可读副本，这里删的是 curl 的临时输入。
            // 配置文件含 key，任何模式下都必须删；请求正文在 dev 模式下
            // 已另存一份可读副本，这里删的是 curl 的临时输入。
            let _ = tokio::fs::remove_file(&config_path).await;
            let _ = tokio::fs::remove_file(&payload_path).await;
            let output =
                output.map_err(|error| provider_error(format!("curl 启动失败：{error}")))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(provider_error(format!(
                    "curl 退出码 {:?}：{}",
                    output.status.code(),
                    stderr.chars().take(300).collect::<String>()
                )));
            }
            devlog::dump_model_io(&stamp, "res", &String::from_utf8_lossy(&output.stdout));
            devlog::dump_model_io(&stamp, "res", &String::from_utf8_lossy(&output.stdout));
            let body: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
                provider_error(format!(
                    "响应非 JSON：{error}；前 200 字节：{}",
                    String::from_utf8_lossy(&output.stdout)
                        .chars()
                        .take(200)
                        .collect::<String>()
                ))
            })?;
            if let Some(api_error) = body.get("error") {
                return Err(provider_error(format!("DeepSeek 报错：{api_error}")));
            }
            let message = body
                .pointer("/choices/0/message")
                .and_then(Value::as_object)
                .cloned()
                .ok_or_else(|| provider_error("响应缺少 choices[0].message"))?;
            let finish_reason = body.pointer("/choices/0/finish_reason").cloned();
            let usage = body.get("usage").map(map_usage);
            println!(
                "[deepseek] 一轮完成：{}ms，finish={}，usage={}",
                started.elapsed().as_millis(),
                finish_reason
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "?".to_owned()),
                body.get("usage").cloned().unwrap_or(Value::Null),
            );
            let tool_names: Vec<String> = message
                .get("tool_calls")
                .and_then(Value::as_array)
                .map(|calls| {
                    calls
                        .iter()
                        .filter_map(|call| {
                            call.pointer("/function/name")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .collect()
                })
                .unwrap_or_default();
            devlog::log(
                "model",
                format!(
                    "响应 run={} finish={} 工具={:?} content={}",
                    request.run_id.as_str(),
                    finish_reason
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "?".to_owned()),
                    tool_names,
                    message
                        .get("content")
                        .and_then(Value::as_str)
                        .map(|text| text.chars().take(120).collect::<String>())
                        .unwrap_or_default()
                ),
            );
            let tool_names: Vec<String> = message
                .get("tool_calls")
                .and_then(Value::as_array)
                .map(|calls| {
                    calls
                        .iter()
                        .filter_map(|call| {
                            call.pointer("/function/name")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .collect()
                })
                .unwrap_or_default();
            devlog::log(
                "model",
                format!(
                    "响应 run={} finish={} 工具={:?} content={}",
                    request.run_id.as_str(),
                    finish_reason
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "?".to_owned()),
                    tool_names,
                    message
                        .get("content")
                        .and_then(Value::as_str)
                        .map(|text| text.chars().take(120).collect::<String>())
                        .unwrap_or_default()
                ),
            );
            Ok(ModelCompletion {
                message: Some(as_object(message)),
                finish_reason,
                usage,
            })
        })
    }
}

fn as_object(map: Map<String, Value>) -> Map<String, Value> {
    map
}

fn map_usage(usage: &Value) -> ModelUsage {
    let read = |key: &str| usage.get(key).and_then(Value::as_u64);
    ModelUsage {
        input_tokens: read("prompt_tokens"),
        output_tokens: read("completion_tokens"),
        cache_read_tokens: read("prompt_cache_hit_tokens"),
        cache_write_tokens: None,
    }
}
