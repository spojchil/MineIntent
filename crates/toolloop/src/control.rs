//! 取消、deadline 与 panic 隔离——循环把外部实现接进来时唯一需要的三件事。
//!
//! 这三件都与领域无关：无论工具是在挖方块还是在查数据库，「等它的时候要能被取消」
//! 「超时要能干净收场」「它 panic 了不能连累循环」都成立。

use std::{
    future::Future,
    panic::{catch_unwind, AssertUnwindSafe},
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};

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

/// 把一个 future 的 panic 转成结构化错误。
pub async fn catch_future_panic<Output, F>(
    future: F,
    code: AgentErrorCode,
    summary: &'static str,
) -> Result<Output, AgentError>
where
    F: Future<Output = Result<Output, AgentError>> + Send,
{
    CatchUnwindFuture::new(future)
        .await
        .map_err(|()| AgentError::new(code, summary))?
}

/// `std` 没有 async `catch_unwind`；把每次 poll 单独围住即可隔离同步与异步 panic。
pub struct CatchUnwindFuture<F> {
    future: Pin<Box<F>>,
}

impl<F> CatchUnwindFuture<F> {
    pub fn new(future: F) -> Self {
        Self {
            future: Box::pin(future),
        }
    }
}

impl<F> Future for CatchUnwindFuture<F>
where
    F: Future,
{
    type Output = Result<F::Output, ()>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        catch_unwind(AssertUnwindSafe(|| this.future.as_mut().poll(context)))
            .map_or(Poll::Ready(Err(())), |poll| poll.map(Ok))
    }
}
