//! 示例应用的命令行、环境变量与强类型配置。

use std::env;
use std::io;
use std::time::Duration;

use agent::adapters::http::WireLogPolicy;
use agent::LevelFilter;

const DEFAULT_TIMEOUT_SECS: u64 = 90;
const USAGE: &str =
    "cargo run --example protocol_smoke --features all-adapters -- [chat|responses|anthropic|all]";

/// 冒烟示例可选择的协议；与携带请求选项的公共 [`agent::adapters::http::Protocol`]
/// 分开保存，便于作为 endpoint 配置的稳定键。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SmokeProtocol {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
}

impl SmokeProtocol {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::OpenAiChat => "openai-chat",
            Self::OpenAiResponses => "openai-responses",
            Self::AnthropicMessages => "anthropic-messages",
        }
    }
}

/// 示例应用汇总后的强类型配置；核心 crate 与协议适配器不读取这些外部来源。
pub(crate) struct SmokeConfig {
    pub(crate) api_key: String,
    pub(crate) model: String,
    pub(crate) level: LevelFilter,
    pub(crate) timeout: Duration,
    pub(crate) wire_log: WireLogPolicy,
    pub(crate) protocols: Vec<SmokeProtocol>,
    endpoints: Vec<(SmokeProtocol, String)>,
}

impl SmokeConfig {
    /// 按“命令行选择协议、环境变量提供运行配置”的规则读取当前进程。
    pub(crate) fn from_process() -> Result<Self, io::Error> {
        let selection = parse_selection(env::args().skip(1))?;
        let protocols = select_protocols(&selection)?;
        let level = parse_level(env::var("AGENT_LOG").as_deref().unwrap_or("info"))?;
        let wire_log = parse_wire_log(env::var("MODEL_WIRE_LOG").as_deref().unwrap_or("off"))?;
        let api_key = read_required_env("MODEL_API_KEY")?;
        let model = read_required_env("MODEL_NAME")?;
        let endpoints = parse_selected_endpoints(&protocols, |protocol| {
            read_required_env(endpoint_variable(protocol))
        })?;
        let timeout_secs = match env::var("MODEL_TIMEOUT_SECS") {
            Ok(value) => parse_timeout(&value)?,
            Err(env::VarError::NotPresent) => DEFAULT_TIMEOUT_SECS,
            Err(env::VarError::NotUnicode(_)) => {
                return Err(invalid_input("MODEL_TIMEOUT_SECS 必须是有效的 UTF-8 整数"));
            }
        };

        Ok(Self {
            api_key,
            model,
            level,
            timeout: Duration::from_secs(timeout_secs),
            wire_log,
            protocols,
            endpoints,
        })
    }

    /// 返回指定协议对应的完整 HTTP endpoint。
    pub(crate) fn endpoint(&self, protocol: SmokeProtocol) -> &str {
        self.endpoints
            .iter()
            .find_map(|(candidate, endpoint)| (*candidate == protocol).then_some(endpoint.as_str()))
            .expect("SmokeConfig 只会保存已完整校验的选中协议")
    }
}

fn parse_selection(arguments: impl IntoIterator<Item = String>) -> Result<String, io::Error> {
    let mut arguments = arguments.into_iter();
    let selection = arguments.next().unwrap_or_else(|| "chat".to_owned());
    if arguments.next().is_some() {
        return Err(invalid_input(format!("用法：{USAGE}")));
    }
    Ok(selection)
}

fn select_protocols(value: &str) -> Result<Vec<SmokeProtocol>, io::Error> {
    match value.to_ascii_lowercase().as_str() {
        "chat" => Ok(vec![SmokeProtocol::OpenAiChat]),
        "responses" => Ok(vec![SmokeProtocol::OpenAiResponses]),
        "anthropic" => Ok(vec![SmokeProtocol::AnthropicMessages]),
        "all" => Ok(vec![
            SmokeProtocol::OpenAiChat,
            SmokeProtocol::OpenAiResponses,
            SmokeProtocol::AnthropicMessages,
        ]),
        value => Err(invalid_input(format!(
            "未知协议：{value}；用法：chat|responses|anthropic|all"
        ))),
    }
}

fn endpoint_variable(protocol: SmokeProtocol) -> &'static str {
    match protocol {
        SmokeProtocol::OpenAiChat => "MODEL_CHAT_ENDPOINT",
        SmokeProtocol::OpenAiResponses => "MODEL_RESPONSES_ENDPOINT",
        SmokeProtocol::AnthropicMessages => "MODEL_ANTHROPIC_ENDPOINT",
    }
}

/// 只读取并解析本次实际选择的协议，未选择的协议不要求配置 endpoint。
fn parse_selected_endpoints(
    protocols: &[SmokeProtocol],
    mut read: impl FnMut(SmokeProtocol) -> Result<String, io::Error>,
) -> Result<Vec<(SmokeProtocol, String)>, io::Error> {
    protocols
        .iter()
        .copied()
        .map(|protocol| {
            let variable = endpoint_variable(protocol);
            let endpoint = read(protocol)?;
            parse_endpoint(variable, &endpoint).map(|endpoint| (protocol, endpoint))
        })
        .collect()
}

fn read_required_env(name: &str) -> Result<String, io::Error> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        Ok(_) | Err(env::VarError::NotPresent) => Err(invalid_input(format!("请设置 {name}"))),
        Err(env::VarError::NotUnicode(_)) => {
            Err(invalid_input(format!("{name} 必须是有效的 UTF-8 字符串")))
        }
    }
}

fn parse_level(value: &str) -> Result<LevelFilter, io::Error> {
    match value.to_ascii_lowercase().as_str() {
        "off" => Ok(LevelFilter::Off),
        "error" => Ok(LevelFilter::Error),
        "warn" => Ok(LevelFilter::Warn),
        "info" => Ok(LevelFilter::Info),
        "debug" => Ok(LevelFilter::Debug),
        "trace" => Ok(LevelFilter::Trace),
        value => Err(invalid_input(format!(
            "未知 AGENT_LOG 级别：{value}；应为 off/error/warn/info/debug/trace"
        ))),
    }
}

fn parse_wire_log(value: &str) -> Result<WireLogPolicy, io::Error> {
    match value.to_ascii_lowercase().as_str() {
        "off" => Ok(WireLogPolicy::Off),
        "full" => Ok(WireLogPolicy::Full),
        value => Err(invalid_input(format!(
            "未知 MODEL_WIRE_LOG 策略：{value}；应为 off/full"
        ))),
    }
}

fn parse_timeout(value: &str) -> Result<u64, io::Error> {
    value
        .parse::<u64>()
        .ok()
        .filter(|seconds| *seconds > 0)
        .ok_or_else(|| invalid_input("MODEL_TIMEOUT_SECS 必须是大于 0 的整数"))
}

fn parse_endpoint(variable: &str, value: &str) -> Result<String, io::Error> {
    let parsed = reqwest::Url::parse(value)
        .map_err(|_| invalid_input(format!("{variable} 必须是有效的完整 HTTP(S) URL")))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err(invalid_input(format!(
            "{variable} 只允许不含凭据和片段的完整 HTTP(S) URL"
        )));
    }
    Ok(parsed.to_string())
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_selection_and_log_configuration_are_explicit() {
        assert_eq!(
            select_protocols("all").unwrap(),
            vec![
                SmokeProtocol::OpenAiChat,
                SmokeProtocol::OpenAiResponses,
                SmokeProtocol::AnthropicMessages
            ]
        );
        assert!(select_protocols("unknown").is_err());
        assert_eq!(parse_level("trace").unwrap(), LevelFilter::Trace);
        assert!(parse_level("verbose").is_err());
        assert_eq!(parse_wire_log("full").unwrap(), WireLogPolicy::Full);
        assert!(parse_wire_log("body").is_err());
    }

    #[test]
    fn cli_defaults_to_chat_and_rejects_extra_arguments() {
        assert_eq!(parse_selection(Vec::new()).unwrap(), "chat");
        assert_eq!(
            parse_selection(vec!["responses".to_owned()]).unwrap(),
            "responses"
        );
        assert!(parse_selection(vec!["chat".to_owned(), "extra".to_owned()]).is_err());
    }

    #[test]
    fn timeout_must_be_positive() {
        assert_eq!(parse_timeout("90").unwrap(), 90);
        assert!(parse_timeout("0").is_err());
        assert!(parse_timeout("invalid").is_err());
    }

    #[test]
    fn selected_endpoint_parser_does_not_require_unselected_protocols() {
        let endpoints = parse_selected_endpoints(&[SmokeProtocol::OpenAiResponses], |protocol| {
            assert_eq!(protocol, SmokeProtocol::OpenAiResponses);
            Ok("https://gateway.example/v1/responses?api-version=latest".to_owned())
        })
        .unwrap();

        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].0, SmokeProtocol::OpenAiResponses);
        assert_eq!(
            endpoints[0].1,
            "https://gateway.example/v1/responses?api-version=latest"
        );
    }

    #[test]
    fn every_selected_protocol_requires_a_valid_full_endpoint() {
        let protocols = [SmokeProtocol::OpenAiChat, SmokeProtocol::AnthropicMessages];
        let error = parse_selected_endpoints(&protocols, |protocol| match protocol {
            SmokeProtocol::OpenAiChat => {
                Ok("https://gateway.example/v1/chat/completions".to_owned())
            }
            SmokeProtocol::AnthropicMessages => Ok("not-a-url".to_owned()),
            SmokeProtocol::OpenAiResponses => unreachable!(),
        })
        .unwrap_err();

        assert!(error.to_string().contains("MODEL_ANTHROPIC_ENDPOINT"));
        assert!(parse_endpoint(
            "MODEL_CHAT_ENDPOINT",
            "https://name:secret@example.com/v1/chat/completions"
        )
        .is_err());
        assert!(parse_endpoint("MODEL_CHAT_ENDPOINT", "file:///tmp/socket").is_err());
    }
}
