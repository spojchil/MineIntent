//! Minecraft backend port 与 DTO 的公共契约。
//!
//! 本模块只描述进程内边界，不包含连接状态机、包解析、事件队列、重连、
//! 移动或 viewport kernel 的实现。backend 在事件发生时必须把当时已知的
//! dimension 写入事件信封；尚未进入世界时才允许省略。

mod api;
mod config;
mod error;
mod event;
mod fixtures;
mod lifecycle;
mod snapshot;
mod viewport;

pub use api::*;
pub use config::*;
pub use error::*;
pub use event::*;
pub use fixtures::*;
pub use lifecycle::*;
pub use snapshot::*;
pub use viewport::*;
