//! 会话要求的四个端口。

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use crate::run::ToolResult;
use crate::types::{AgentError, JsonObject, ModelUsage, ToolDefinition, ToolInvocation};

/// 消息保持 wire 形状（role + content + …），无损；
/// 不把任何一家 provider 的私有字段提升为公共契约。
pub type Message = JsonObject;

/// 端口返回的 future。不引第三方 trait 库，标准库自足。
pub type PortFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 提示词源。persona 稳定；situation 每轮重新获取。
/// 两段都不进入压缩策略的输入。
pub trait PromptSource: Send + Sync {
    /// 系统提示词 / 人格。
    fn persona(&self) -> Vec<Message>;
    /// 开场处境。
    fn situation(&self) -> Vec<Message>;
}

/// 工具：定义与执行。
pub trait Tools: Send + Sync {
    fn definitions(&self) -> Vec<ToolDefinition>;
    fn call<'a>(&'a self, invocation: ToolInvocation) -> PortFuture<'a, ToolResult>;
}

/// 上下文压缩策略。何时压由会话按阈值判定，本接口只负责怎么压；
/// 输入仅为对话段。需要模型的实现在构造时自行持有。
pub trait Compaction: Send + Sync {
    fn compact<'a>(&'a self, conversation: &'a [Message]) -> PortFuture<'a, Vec<Message>>;
}

/// 一次模型请求：累积消息 + 工具定义。
pub struct ModelRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
}

/// Provider 已归一化的一次 assistant completion。
/// `message` 保持 JSON object 无损（reasoning_content、tool_calls 原样保留）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelCompletion {
    pub message: Option<JsonObject>,
    /// 缺字段用 `None`。仅记录，不据此判断。
    pub finish_reason: Option<Value>,
    pub usage: Option<ModelUsage>,
}

/// 模型。一次调用一次语义；重试与超时由实现方处理。
pub trait Model: Send + Sync {
    fn complete<'a>(&'a self, request: ModelRequest) -> PortFuture<'a, Result<ModelCompletion, AgentError>>;
}
