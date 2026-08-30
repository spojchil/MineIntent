use agent::adapters::http::Protocol;
use agent::adapters::openai::{chat, responses};
use serde_json::json;

/// 按协议名装配 wire 协议。`reasoning_effort` 给了才发，各协议按自己的字段写：
/// Chat Completions 是顶层 `reasoning_effort`，Responses 是 `reasoning.effort`；
/// Anthropic Messages 没有档位字段（思考预算是另一回事），给了就拒绝，不静默丢掉。
pub fn protocol(
    name: &str,
    pictures: bool,
    reasoning_effort: Option<&str>,
) -> Result<Protocol, String> {
    match (name, reasoning_effort) {
        ("chat", _) if pictures => Err("view 图片工具需要 MODEL_PROTOCOL=responses 或 anthropic；Chat Completions 不支持原生图片工具回执".to_owned()),
        ("chat", None) => Ok(Protocol::openai_chat()),
        ("chat", Some(effort)) => Ok(Protocol::openai_chat_with(
            chat::RequestOptions::new().with_additional_field("reasoning_effort", effort),
        )),
        ("responses", None) => Ok(Protocol::openai_responses()),
        ("responses", Some(effort)) => Ok(Protocol::openai_responses_with(
            responses::RequestOptions::new()
                .with_additional_field("reasoning", json!({ "effort": effort })),
        )),
        ("anthropic", None) => Ok(Protocol::anthropic_messages()),
        ("anthropic", Some(_)) => Err(
            "MODEL_REASONING_EFFORT 不适用于 MODEL_PROTOCOL=anthropic（该协议没有档位字段）"
                .to_owned(),
        ),
        _ => Err("MODEL_PROTOCOL 必须是 chat、responses 或 anthropic".to_owned()),
    }
}

pub fn default_endpoint(protocol: &Protocol) -> &'static str {
    match protocol {
        Protocol::OpenAiResponses(_) => "https://api.deepseek.com/responses",
        Protocol::AnthropicMessages(_) => "https://api.deepseek.com/anthropic/v1/messages",
        _ => "https://api.deepseek.com/chat/completions",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_incompatible_image_configuration_before_connecting() {
        assert!(protocol("chat", true, None).is_err());
        assert!(protocol("chat", false, None).is_ok());
        assert!(protocol("responses", true, None).is_ok());
        assert!(protocol("anthropic", true, None).is_ok());
        assert!(protocol("typo", false, None).is_err());
        assert_eq!(
            default_endpoint(&protocol("responses", true, None).unwrap()),
            "https://api.deepseek.com/responses"
        );
    }

    #[test]
    fn reasoning_effort_uses_each_protocols_own_field() {
        match protocol("chat", false, Some("max")).unwrap() {
            Protocol::OpenAiChat(options) => {
                assert_eq!(options.additional_fields["reasoning_effort"], "max");
            }
            other => panic!("{other:?}"),
        }
        match protocol("responses", true, Some("high")).unwrap() {
            Protocol::OpenAiResponses(options) => {
                assert_eq!(
                    options.additional_fields["reasoning"],
                    json!({"effort": "high"})
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(protocol("anthropic", true, Some("high")).is_err());
    }
}
