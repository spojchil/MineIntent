//! Capability 注册、调用与执行上下文的公共契约命名空间。
//!
//! 这里只冻结 catalog、dispatch 与 scope 边界；具体 capability 动作由后续实现提供。

mod contracts;
pub mod fixtures;
mod schemas;

pub use contracts::{
    CapabilityExecutionContext, CapabilityInvocation, ExecutionResource, ScopeGuard,
    ToolCapability, ToolCapabilityRegistry, ToolDispatcher, ToolResultProtocol,
};
pub use schemas::{
    directed_view_result_schema, move_input_parameters_schema, validate_directed_positions,
    view_parameters_schema, MoveDirection, MoveInputArguments, ViewArguments, ViewMode,
    ViewPosition, MAX_DIRECTED_VIEW_POSITIONS,
};
