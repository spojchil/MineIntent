//! B 可消费的最小 Information facade 契约。
//!
//! catalog、selector/reference、cursor、provider/adapter revision 均是 information 内部
//! 细节，不出现在本模块。viewport 的几何投影只由 backend kernel 生产，本模块只做
//! 权限、omission/error 与 plain DTO 封装。

mod facade;
mod fixtures;
mod values;

pub use facade::*;
pub use fixtures::*;
pub use values::*;
