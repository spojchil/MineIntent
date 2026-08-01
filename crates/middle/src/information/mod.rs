//! A 独占的 Information 集成边界命名空间。
//!
//! 本模块只承载 Information 的协议、纯数学和 source-port 边界。provider/store/runtime
//! 与 viewport 投影 kernel 不属于这一层。

pub mod contracts;
pub mod geometry;
pub mod source_ports;
