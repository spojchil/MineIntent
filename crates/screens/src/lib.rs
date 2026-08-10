//! 界面域工具：屏的行为与开合转换。
//!
//! 互斥的账本与压制裁决都在编排（`dispatch::Occupancy`）——本模块只在
//! 自己的状态转换处占域、释放：开屏占，关屏放。闭环动词（说、历史）
//! 结束必关屏，唯有显式"开"使屏保持打开。
//!
//! 屏的真相归属备忘：聊天屏在本地（服务端没有"开着"的概念）；将来容器屏
//! 的真相在服务端快照（可被强开强关），加入时其种类状态与关闭义务
//! （ServerboundContainerClose）在本模块内扩展。

mod chat;
mod segment;

pub use chat::{ChatBox, ChatDoor, ChatHistory, ChatReadMark};
