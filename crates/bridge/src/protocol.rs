//! 身体与转接器之间的线协议：一行一个 JSON。
//!
//! 只在本机两进程之间走，不对外承诺稳定。载荷直接复用内核的工具类型，
//! 工具定义与回执在两端是同一份形状，不另建一套镜像。

use agent::{ToolDefinition, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::SocketAddr;

/// 默认端口：本机回环，避开 Minecraft 的 25565 与 RCON 的 25575。
pub const DEFAULT_ADDR: &str = "127.0.0.1:25580";

/// 私有协议没有鉴权，只允许数值回环地址；配置错误应在连接世界前报出。
pub fn parse_addr(value: &str) -> Result<SocketAddr, String> {
    let addr: SocketAddr = value
        .parse()
        .map_err(|error| format!("身体地址无效（{value}）：{error}"))?;
    if !addr.ip().is_loopback() {
        return Err("身体地址必须是本机回环地址，例如 127.0.0.1:25580 或 [::1]:25580".to_owned());
    }
    Ok(addr)
}

/// 转接器 → 身体。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// 要当前工具表。
    Tools { id: u64 },
    /// 调一件工具。
    Call {
        id: u64,
        name: String,
        arguments: Value,
    },
    /// 取消该连接上尚未完成的请求；已经提交到游戏的动作不由此回滚。
    Cancel { id: u64 },
}

/// 身体 → 转接器。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Message {
    /// 已取得这具身体的接入；TCP connect 成功本身不表示取得接入。
    Ready,
    /// 对 [`Request::Tools`] 的回答。
    Tools { id: u64, tools: Vec<ToolDefinition> },
    /// 对 [`Request::Call`] 的回答。
    Result { id: u64, result: ToolResult },
    /// 未知工具属于 MCP 请求错误，不与工具执行失败混为一类。
    UnknownTool { id: u64, name: String },
    /// 工具任务异常退出等桥接失败，执行结果可能无法确认。
    Failed { id: u64, reason: String },
    /// 身体已被另一个接入占着：连上后立刻收到这一条，随即断开。
    Occupied { reason: String },
}

/// 编一行（含换行）。
pub fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    let mut line = serde_json::to_vec(value).expect("协议消息必定可序列化");
    line.push(b'\n');
    line
}
