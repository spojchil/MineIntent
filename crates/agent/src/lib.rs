//! 顶层模块：持有一段持续对话的纯对话机器。
//!
//! 规格：仓库根 `TEMP_顶层模块接口.md`。对世界、对 Minecraft 零知识；
//! 认识的最底层词汇是四个端口（提示词源 / 工具 / 压缩策略 / 模型）。
//!
//! 对外三个函数：`inject_if_running`（忙：插进活动轮的请求边界信箱）、
//! `start_if_idle`（闲：起一轮，玩家触发优先、竞态原物奉还）、`stop`。
//! 唤醒判断、处境组装、记忆装载都是调用方的事。
//!
//! 循环状态机从旧 `toolloop` 重写吸收，不依赖它；每轮请求数上限那类
//! 脚手架不再携带——唯一预算是上下文窗（压缩触发，声明式数据）。

mod mailbox;
mod ports;
mod run;
mod session;

pub use ports::{Compaction, Message, Model, ModelCompletion, ModelRequest, PortFuture, PromptSource, Tools};
pub use run::{PlannedToolCall, ToolResult, Turn, TurnStep};
pub use session::{AgentSession, SessionConfig, StartRejected, StartRejectedReason, TurnOutcome};
