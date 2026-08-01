use std::{fmt, ops::Deref};

use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};

macro_rules! validated_string {
    ($name:ident, $validator:ident, $expectation:literal) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, String> {
                let value = value.into();
                if $validator(&value) {
                    Ok(Self(value))
                } else {
                    Err($expectation.to_owned())
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl Deref for $name {
            type Target = str;

            fn deref(&self) -> &Self::Target {
                self.as_str()
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(D::Error::custom)
            }
        }
    };
}

fn valid_run_id(value: &str) -> bool {
    let length = value.chars().count();
    (1..=128).contains(&length)
}

fn valid_tool_call_id(value: &str) -> bool {
    (1..=128).contains(&value.len()) && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn valid_tool_name(value: &str) -> bool {
    let length = value.chars().count();
    (1..=64).contains(&length)
}

fn valid_tool_definition_name(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_model_name(value: &str) -> bool {
    let length = value.chars().count();
    (1..=256).contains(&length)
}

fn valid_template_key(value: &str) -> bool {
    let length = value.chars().count();
    (1..=128).contains(&length) && !value.chars().any(char::is_control)
}

fn valid_template_version(value: &str) -> bool {
    let length = value.chars().count();
    (1..=64).contains(&length) && !value.chars().any(char::is_control)
}

validated_string!(
    RunId,
    valid_run_id,
    "run id must contain 1..=128 characters"
);
validated_string!(
    ToolCallId,
    valid_tool_call_id,
    "tool call id must contain 1..=128 printable ASCII bytes"
);
validated_string!(
    ToolName,
    valid_tool_name,
    "tool invocation name must contain 1..=64 characters"
);
validated_string!(
    ToolDefinitionName,
    valid_tool_definition_name,
    "advertised tool name must match [A-Za-z0-9_-]{1,64}"
);
validated_string!(
    ModelName,
    valid_model_name,
    "model name must contain 1..=256 characters"
);
validated_string!(
    PromptTemplateKey,
    valid_template_key,
    "prompt template key must contain 1..=128 non-control characters"
);
validated_string!(
    PromptTemplateVersion,
    valid_template_version,
    "prompt template version must contain 1..=64 non-control characters"
);
