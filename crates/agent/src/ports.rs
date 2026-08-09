//! 本模块要求的四个接口。端口说 agent 的语言，不说机制的语言；
//! 每个端口的形状裁定与前人对照见 `TEMP_顶层模块接口.md` §1。

use std::future::Future;
use std::pin::Pin;

use mineintent_contracts::agent::{AgentError, JsonObject, ModelUsage, ToolInvocation, WireToolDefinition};
use serde_json::Value;

use crate::run::ToolResult;

/// 消息保持 wire 形状（role + content + …），无损；
/// 不把任何一家 provider 的私有字段提升为公共契约。
pub type Message = JsonObject;

/// 端口返回的 future。不引第三方 trait 库，标准库自足。
pub type PortFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// ① 提示词源。拆两半的理由：压缩的保护区边界由此天然成立——
/// persona 稳定、situation 每轮重导出，两者都不经压缩策略的手。
pub trait PromptSource: Send + Sync {
    /// 稳定段：系统提示词 / 人格。
    fn persona(&self) -> Vec<Message>;
    /// 每轮重导出段：开场处境。实现者拉不拉世界快照是实现者的事。
    fn situation(&self) -> Vec<Message>;
}

/// ② 工具：定义 + 执行，一体。agent 只见擦除的 wire 形状；
/// 实现侧的有类型定型与一处擦除是模块二的约定，不在此处。
pub trait Tools: Send + Sync {
    fn definitions(&self) -> Vec<WireToolDefinition>;
    fn call<'a>(&'a self, invocation: ToolInvocation) -> PortFuture<'a, ToolResult>;
}

/// ③ 上下文压缩策略。触发归会话（声明式阈值），本接口只答"怎么压"；
/// 只见对话段——persona 与 situation 在保护区外，压缩后由①重导出新鲜的。
/// 需要模型的实现（摘要式）在构造时注入④，调用签名保持纯。
pub trait Compaction: Send + Sync {
    fn compact<'a>(&'a self, conversation: &'a [Message]) -> PortFuture<'a, Vec<Message>>;
}

/// 一次模型请求：累积消息 + 工具定义。
pub struct ModelRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<WireToolDefinition>,
}

/// Provider 已归一化的一次 assistant completion。
/// `message` 保持 JSON object 无损（reasoning_content、tool_calls 原样保留）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelCompletion {
    pub message: Option<JsonObject>,
    /// 缺字段用 `None`；本模块 v0 只记录不判断（截断处理属适配器）。
    pub finish_reason: Option<Value>,
    pub usage: Option<ModelUsage>,
}

/// ④ 模型。一次调用一次语义；重试/退避/超时在适配器或调用方，不在接口里。
/// 流式变体留位不实现（say-streaming 落地时补）。
pub trait Model: Send + Sync {
    fn complete<'a>(&'a self, request: ModelRequest) -> PortFuture<'a, Result<ModelCompletion, AgentError>>;
}
