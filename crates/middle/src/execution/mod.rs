//! 小型进程内执行协调层：资源租约、后台 job 生命周期和 scope epoch。
//!
//! 本模块只报告资源冲突和跟踪执行状态，不解释目标，也不实现具体工具动作。

mod arbiter;
mod contracts;

pub use arbiter::{ExecutionArbiter, JobCancellation, JobHandle, ResourceLeaseHandle};
pub use contracts::{
    AcquireDecision, ExecutionRefusal, ExecutionRefusalCode, ExecutionRequest, ExecutionResource,
    JobOutcome, JobState, ResourceLease, SettledJobState,
};
