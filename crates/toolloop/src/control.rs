//! 取消与 deadline——循环把外部实现接进来时需要的两件事。
//!
//! 两件都与领域无关：无论工具是在挖方块还是在查数据库，「等它的时候要能被取消」
//! 和「超时要能干净收场」都成立。
//!
//! 这里**没有** panic 隔离。工具或 provider panic 时循环不再拦截，让它照常传播：
//! 调用方的任务边界（生产路径上是 `process_wake` 里被 await 的那个
//! `tokio::spawn`）会把它接成 `JoinError`，从而走失败流与 journal。压成一条
//! 模型可见的普通失败只会让缺陷看起来像世界事实，而且模型会重试——panic 必然
//! 可重现，重试注定再次 panic。

use std::{future::Future, time::Instant};

use mineintent_contracts::agent::{AgentError, AgentErrorCode, ExecutionControl};

/// 终结整轮的错误，与「某个工具失败了但循环继续」区分开。
///
/// 判据是**谁被终结**：这三种说的是这一轮不再有意义，其余说的是某次调用没成功。
pub fn is_run_control_error(code: AgentErrorCode) -> bool {
    matches!(
        code,
        AgentErrorCode::RunCancelled
            | AgentErrorCode::DeadlineExceeded
            | AgentErrorCode::ScopeInvalid
    )
}

/// 在取消信号、deadline 与 future 完成三者之间等待。
///
/// `biased` 让取消优先于同时到期的 deadline——两者同时可见时，说「被取消了」
/// 比说「超时了」更接近发生的事。
pub async fn await_with_control<F, Output>(
    future: F,
    control: ExecutionControl<'_>,
) -> Result<Output, AgentError>
where
    F: Future<Output = Result<Output, AgentError>> + Send,
{
    control.check_at(Instant::now())?;
    let cancellation = control.cancelled();
    let timer = tokio::time::sleep_until(tokio::time::Instant::from_std(
        control.deadline().expires_at(),
    ));
    tokio::pin!(future);
    tokio::pin!(cancellation);
    tokio::pin!(timer);

    tokio::select! {
        biased;
        cancellation_error = &mut cancellation => {
            match control.check_at(Instant::now()) {
                Err(error) => Err(error),
                Ok(()) => Err(cancellation_error),
            }
        }
        _ = &mut timer => {
            match control.check_at(Instant::now()) {
                Err(error) => Err(error),
                Ok(()) => Err(AgentError::deadline_exceeded()),
            }
        }
        result = &mut future => {
            control.check_at(Instant::now())?;
            result
        }
    }
}
