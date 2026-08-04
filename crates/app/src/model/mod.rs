//! 模型 provider。
//!
//! 分层按**协议形状**而不是供应商切：一个 provider 对应一种 wire 协议，
//! 供应商之间的差别（端点、模型名、key、思考强度）是配置，不是代码。
//! `responses` 就是这样的一层——DeepSeek 只是当前用它的一家。
//!
//! 所有 provider 对上都实现契约层的 `ModelProvider`，并把自己的响应归一化成
//! `message { content, tool_calls }`。Agent 状态机只认这一个形状，
//! 因此新增供应商不会波及上层。

pub mod responses;
pub mod scripted;

pub use responses::{ResponsesConfig, ResponsesModelProvider};
pub use scripted::{default_vertical_script, JsonObject, ScriptedModelProvider};
