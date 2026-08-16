//! 界面域工具：屏的行为与开合转换。
//!
//! 互斥的账本与压制裁决都在编排（`dispatch::Occupancy`）——本模块只在
//! 自己的状态转换处占域、释放：开屏占，关屏放。闭环动词（说、历史）
//! 结束必关屏，唯有显式"开"使屏保持打开。
//!
//! 屏的真相归属：聊天屏与物品栏屏在本地（服务端没有"开着"的概念）；
//! 容器屏（工作台）的真相在服务端快照——开屏由 hand use_on 触发、组合根
//! 随屏事实占域，关闭义务（ServerboundContainerClose）在 close 动词，
//! 服务端强关同样经屏事实收口。

mod chat;
mod container;
mod inventory;
mod segment;

pub use chat::{ChatBox, ChatDoor, ChatHistory, ChatReadMark};
pub use container::{container_usage, ContainerScreen, CONTAINER_USAGE};
pub use inventory::{InventoryDoor, InventoryScreen, ScreenKind, ScreenState, DISCARD_SLOT};
