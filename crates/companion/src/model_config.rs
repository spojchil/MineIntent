use agent::adapters::http::Protocol;

pub fn protocol(name: &str, pictures: bool) -> Result<Protocol, String> {
    match name {
        "chat" if pictures => Err("view 图片工具需要 MODEL_PROTOCOL=responses 或 anthropic；Chat Completions 不支持原生图片工具回执".to_owned()),
        "chat" => Ok(Protocol::openai_chat()),
        "responses" => Ok(Protocol::openai_responses()),
        "anthropic" => Ok(Protocol::anthropic_messages()),
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
        assert!(protocol("chat", true).is_err());
        assert!(protocol("chat", false).is_ok());
        assert!(protocol("responses", true).is_ok());
        assert!(protocol("anthropic", true).is_ok());
        assert!(protocol("typo", false).is_err());
        assert_eq!(
            default_endpoint(&protocol("responses", true).unwrap()),
            "https://api.deepseek.com/responses"
        );
    }
}
