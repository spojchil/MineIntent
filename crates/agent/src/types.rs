//! 本模块自有的基础类型。不依赖任何项目契约。

use serde_json::Value;

/// wire 形状的消息对象（role + content + …）。
pub type JsonObject = serde_json::Map<String, Value>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentErrorKind {
    /// 状态机被以非法顺序驱动。
    InvalidRequest,
    /// 模型响应缺失或形状不可用。
    Provider,
    /// 工具结果无法回放。
    Tool,
    /// tool-call 批不合法（ID 缺失/重复、结果批不配对）。
    InvalidToolCall,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentError {
    pub kind: AgentErrorKind,
    pub summary: String,
}

impl AgentError {
    pub fn new(kind: AgentErrorKind, summary: impl Into<String>) -> Self {
        Self {
            kind,
            summary: summary.into(),
        }
    }
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.summary)
    }
}

impl std::error::Error for AgentError {}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

macro_rules! string_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Eq, Hash, PartialEq)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

string_id!(
    /// 一轮的标识。
    RunId
);
string_id!(
    /// 模型给出的 tool-call 标识；一轮内一次性 claim，不得复用。
    ToolCallId
);
string_id!(
    /// 工具名。
    ToolName
);

/// 一次工具调用：已解析、可交给工具端口执行。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolInvocation {
    pub run_id: RunId,
    pub tool_call_id: ToolCallId,
    pub name: ToolName,
    pub arguments: JsonObject,
}

/// 工具定义的中立形状；由模型端口的实现渲染成 provider wire。
#[derive(Clone, Debug, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema。
    pub parameters: Value,
}
