//! 三种 wire codec 共用的 HTTP 传输层。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use agent::{
    AgentError, AgentErrorKind, ContentPart, Model, ModelRequest, ModelResponse, PortFuture,
};
use reqwest::RequestBuilder;
use serde_json::Value;

use super::anthropic_messages::AnthropicMessagesCodec;
use super::openai_chat::OpenAiChatCodec;
use super::openai_responses::OpenAiResponsesCodec;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Protocol {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
}

impl Protocol {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::OpenAiChat => "openai-chat",
            Self::OpenAiResponses => "openai-responses",
            Self::AnthropicMessages => "anthropic-messages",
        }
    }

    fn codec(self) -> Arc<dyn WireCodec> {
        match self {
            Self::OpenAiChat => Arc::new(OpenAiChatCodec),
            Self::OpenAiResponses => Arc::new(OpenAiResponsesCodec),
            Self::AnthropicMessages => Arc::new(AnthropicMessagesCodec),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WireLogPolicy {
    Off,
    Full,
}

#[derive(Clone, Copy)]
pub(super) enum AuthStyle {
    Bearer,
    Anthropic,
}

/// codec 只负责 wire 转换；重试、流式聚合和 HTTP 状态处理由传输层负责。
pub(super) trait WireCodec: Send + Sync {
    fn auth_style(&self) -> AuthStyle;

    fn encode_request(&self, request: &ModelRequest, model: &str) -> Result<Value, AgentError>;

    fn decode_response(&self, response: Value) -> Result<ModelResponse, AgentError>;
}

pub(crate) struct HttpModel {
    client: reqwest::Client,
    api_key: String,
    model: String,
    endpoint: String,
    protocol: Protocol,
    wire_log: WireLogPolicy,
    request_sequence: AtomicU64,
    codec: Arc<dyn WireCodec>,
}

impl HttpModel {
    pub(crate) fn new(
        api_key: String,
        model: String,
        endpoint: String,
        protocol: Protocol,
        timeout: Duration,
        wire_log: WireLogPolicy,
    ) -> Result<Self, AgentError> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| model_error(format!("transport_client_failed: {error}")))?;
        Ok(Self {
            client,
            api_key,
            model,
            endpoint,
            protocol,
            wire_log,
            request_sequence: AtomicU64::new(0),
            codec: protocol.codec(),
        })
    }
}

impl Model for HttpModel {
    fn complete<'a>(
        &'a self,
        request: ModelRequest,
    ) -> PortFuture<'a, Result<ModelResponse, AgentError>> {
        Box::pin(async move {
            // 在 await 前完成规范请求到 wire 请求的投影，避免 codec 借用跨越网络等待。
            let auth_style = self.codec.auth_style();
            let body = self.codec.encode_request(&request, &self.model)?;
            let request_sequence = self.request_sequence.fetch_add(1, Ordering::Relaxed) + 1;
            self.log_request(request_sequence, &body);
            let request = authorize(
                self.client.post(&self.endpoint).json(&body),
                auth_style,
                &self.api_key,
            );
            let response = request
                .send()
                .await
                .map_err(|error| model_error(format!("transport_request_failed: {error}")))?;

            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(|error| model_error(format!("transport_body_failed: {error}")))?;
            self.log_response(request_sequence, status.as_u16(), &body);
            if !status.is_success() {
                let code = provider_error_code(&body)
                    .map(|code| format!(":{code}"))
                    .unwrap_or_default();
                return Err(model_error(format!(
                    "transport_http_{}{code}",
                    status.as_u16()
                )));
            }
            let value = serde_json::from_str(&body)
                .map_err(|error| model_error(format!("transport_json_failed: {error}")))?;
            self.codec.decode_response(value)
        })
    }
}

impl HttpModel {
    fn log_request(&self, sequence: u64, body: &Value) {
        if self.wire_log != WireLogPolicy::Full {
            return;
        }
        let body = serde_json::to_string_pretty(body).unwrap_or_else(|_| body.to_string());
        eprintln!(
            "[transport/{}][wire][request:{sequence}] {}\n{body}",
            self.protocol.label(),
            self.endpoint,
        );
    }

    fn log_response(&self, sequence: u64, status: u16, body: &str) {
        if self.wire_log != WireLogPolicy::Full {
            return;
        }
        let formatted = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|value| serde_json::to_string_pretty(&value).ok())
            .unwrap_or_else(|| body.to_owned());
        eprintln!(
            "[transport/{}][wire][response:{sequence}][http:{status}]\n{formatted}",
            self.protocol.label()
        );
    }
}

fn authorize(builder: RequestBuilder, style: AuthStyle, key: &str) -> RequestBuilder {
    match style {
        AuthStyle::Bearer => builder.bearer_auth(key),
        AuthStyle::Anthropic => builder
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
    }
}

pub(super) fn model_error(summary: impl Into<String>) -> AgentError {
    AgentError::new(AgentErrorKind::Model, summary)
}

pub(super) fn content_as_text(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => text.clone(),
            ContentPart::Json { value } => value.to_string(),
            ContentPart::Opaque { data, .. } => data.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn parse_arguments(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

fn provider_error_code(body: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let code = value
        .pointer("/error/code")
        .or_else(|| value.pointer("/error/type"))
        .or_else(|| value.get("code"))?
        .as_str()?;
    let safe = code
        .chars()
        .take(80)
        .filter(|character| character.is_ascii_alphanumeric() || "._-".contains(*character))
        .collect::<String>();
    (!safe.is_empty()).then_some(safe)
}
