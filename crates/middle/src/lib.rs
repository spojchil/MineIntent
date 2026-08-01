//! MineIntent 中间层与 Agent 循环的 crate 边界。
//!
//! P1 共享脚手架只声明并行叶子的模块入口，不包含业务实现。

pub mod events;
pub mod execution;
pub mod information;
pub mod speech;
