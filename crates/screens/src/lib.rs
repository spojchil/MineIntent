//! 界面互斥域：任意时刻最多一个屏打开。
//!
//! 单槽结构使"同时开两个屏"在类型上不可表达；开新屏顶替现任（各屏的关闭义务
//! 由其类型决定），占槽状态经 [`ScreenSlot::occupied`] 供跨域压制查询。
//! 闭环动词（说、历史）结束必关屏，唯有显式"开"使屏保持打开。

mod chat;
mod segment;

use std::sync::Mutex;

pub use chat::{ChatBox, ChatDoor, ChatHistory};

/// 槽位词汇。聊天屏与自有背包屏的真相在本地（服务端没有"开着"的概念）；
/// 容器屏的真相在服务端快照（可被强开强关），加入时在此扩展。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenScreen {
    Chat,
}

impl OpenScreen {
    /// 被顶替或关闭时需要向服务端履行的义务。聊天屏无包；
    /// 容器屏加入后在此返回对应的关闭包指令。
    pub fn close_obligation(self) -> Option<CloseObligation> {
        match self {
            Self::Chat => None,
        }
    }
}

/// 关屏的 wire 义务。目前没有任何屏需要；容器屏（ServerboundContainerClose）
/// 加入时首个变体在此出现。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseObligation {}

/// 全模块唯一的屏槽。所有屏工具共享同一实例。
#[derive(Default)]
pub struct ScreenSlot {
    current: Mutex<Option<OpenScreen>>,
}

impl ScreenSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// 跨域压制的唯一问句：现在开着什么屏。
    pub fn occupied(&self) -> Option<OpenScreen> {
        *self.current.lock().expect("屏槽锁中毒")
    }

    /// 顶替式写入，返回被送走的现任；其关闭义务由调用处兑现。
    pub fn replace(&self, next: Option<OpenScreen>) -> Option<OpenScreen> {
        std::mem::replace(&mut *self.current.lock().expect("屏槽锁中毒"), next)
    }
}
