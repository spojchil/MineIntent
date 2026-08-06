//! MineIntent 的进程内协调层。
//!
//! 提供应用事件 journal、资源与 job 协调、单文本记忆、信息查询及玩家聊天输入/输出边界。

pub mod agent;
pub mod capability;
pub mod events;
pub mod information;
pub mod memory;
pub mod participant;
pub mod speech;
pub mod telemetry;
pub mod viewport;
