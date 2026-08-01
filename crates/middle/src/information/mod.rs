//! A 独占的 Information 协议与阶段 1/2 叶子实现。
//!
//! 当前包含协议、纯数学、source-port、registry、scope/trace、ref/cursor store、访问策略，
//! 以及 current-status/inventory 两个叶子 provider。
//! 其余 provider、runtime、context composer 与 viewport 投影 kernel 尚未迁入。

pub mod access_policy;
pub mod contracts;
pub mod cursor_store;
pub mod geometry;
pub mod providers;
pub mod ref_store;
pub mod registry;
pub mod scope;
pub mod source_ports;
pub mod trace;

mod support;

pub use support::{InformationClock, SystemInformationClock};
