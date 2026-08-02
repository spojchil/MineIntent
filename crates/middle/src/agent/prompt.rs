use std::fmt;

use mineintent_contracts::agent::{
    AgentDecisionContextV4, JsonObject, PromptTemplateKey, PromptTemplateRef, PromptTemplateVersion,
};
use serde::Serialize;
use serde_json::Value;

const PARTICIPANT_SYSTEM_V1: &str = include_str!("prompts/participant-system/v1.txt");

/// Errors from the closed, compile-time prompt catalog.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PromptError {
    UnknownTemplate { key: String, version: String },
    FrameSerialization { summary: String },
}

impl PromptError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownTemplate { .. } => "unknown_prompt_template",
            Self::FrameSerialization { .. } => "prompt_frame_serialization_failed",
        }
    }
}

impl fmt::Display for PromptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTemplate { key, version } => {
                write!(formatter, "unknown_prompt_template:{key}@{version}")
            }
            Self::FrameSerialization { summary } => {
                write!(formatter, "prompt_frame_serialization_failed:{summary}")
            }
        }
    }
}

impl std::error::Error for PromptError {}

/// Resolves a prompt only from the explicit key/version supplied by assembly.
///
/// The source file has one terminal newline because it is a repository text
/// artifact; the oracle's `system_prompt()` returns the text without that file
/// delimiter, so one LF or CRLF delimiter is removed here. Prompt content
/// itself is otherwise untouched.
pub fn template_text(
    key: &PromptTemplateKey,
    version: &PromptTemplateVersion,
) -> Result<&'static str, PromptError> {
    match (key.as_str(), version.as_str()) {
        ("participant-system", "v1") => Ok(strip_one_file_terminator(PARTICIPANT_SYSTEM_V1)),
        _ => Err(PromptError::UnknownTemplate {
            key: key.as_str().to_owned(),
            version: version.as_str().to_owned(),
        }),
    }
}

fn strip_one_file_terminator(value: &str) -> &str {
    value
        .strip_suffix("\r\n")
        .or_else(|| value.strip_suffix('\n'))
        .unwrap_or(value)
}

/// Builds the stable system message once for a run.
pub fn system_prompt(template: &PromptTemplateRef, memory: &str) -> Result<String, PromptError> {
    let base = template_text(&template.key, &template.version)?;
    let mut prompt = base.to_owned();
    if !memory.is_empty() {
        prompt.push_str("\n\n## 你记得的事\n");
        prompt.push_str(memory);
    }
    Ok(prompt)
}

/// Creates the initial stable-system plus appended opening-frame messages.
///
/// The frame is serialized once and handed to `AgentRun`; subsequent model
/// requests replay the same prefix and append only assistant/tool messages.
pub fn initial_messages<Status, Inventory, Sound, Omission>(
    context: &AgentDecisionContextV4<Status, Inventory, Sound, Omission>,
    template: &PromptTemplateRef,
) -> Result<Vec<JsonObject>, PromptError>
where
    Status: Serialize,
    Inventory: Serialize,
    Sound: Serialize,
    Omission: Serialize,
{
    let stable = system_prompt(template, &context.stable.memory)?;
    let frame =
        serde_json::to_string(&context.frame).map_err(|error| PromptError::FrameSerialization {
            summary: error.to_string(),
        })?;

    Ok(vec![message("system", stable), message("user", frame)])
}

fn message(role: &str, content: String) -> JsonObject {
    let mut value = JsonObject::new();
    value.insert("role".to_owned(), Value::String(role.to_owned()));
    value.insert("content".to_owned(), Value::String(content));
    value
}

#[cfg(test)]
mod tests {
    use super::strip_one_file_terminator;

    #[test]
    fn strips_one_lf_or_crlf_without_trimming_other_content() {
        assert_eq!(strip_one_file_terminator("body\n"), "body");
        assert_eq!(strip_one_file_terminator("body\r\n"), "body");
        assert_eq!(strip_one_file_terminator("body\r"), "body\r");
        assert_eq!(strip_one_file_terminator("body\r\n\n"), "body\r\n");
        assert_eq!(strip_one_file_terminator("body\n\r\n"), "body\n");
    }
}
