//! 常驻身体与标准 MCP stdio 入口。
//!
//! `companion` 持有世界与身体；`mineintent-mcp` 使用官方 rmcp SDK 服务客户端，
//! 经本机 TCP 转发工具调用。客户端退出只释放接入，身体进程继续存在。
//! 通用客户端通过工具回执和可取消的 `wait` 拉取信息，不声明专用客户端扩展。

pub mod body;
mod io_guard;
pub mod protocol;
pub mod shim;

pub use tokio_util::sync::CancellationToken;

#[cfg(test)]
mod tests;
