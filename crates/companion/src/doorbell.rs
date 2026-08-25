//! 门铃：`wait` 期间「有没有它还没看见的唤醒」。
//!
//! # 判据不是「等待期间有事」，是「有它还没看见的事」
//!
//! 天真的写法是等待期间挂一个通知回调。它会漏掉一段：模型**想**等的那一刻，到
//! `wait` 真正开始等，中间隔着一次模型推理（实测 3–4 秒）。这段时间里送达的唤醒
//! 已经并进了这一轮，模型却要等到 `wait` 返回才看得见。
//!
//! 所以判据取两个计数的差：唤醒每投递一批记一次数（[`Doorbell::ring`]），每开始
//! 一次模型请求把当时的计数拍一张快照（`ModelRequestStarted`）。**计数比快照大 =
//! 有它没看过的唤醒**，那就一秒都不该等。
//!
//! # 为什么由组合根敲铃，而不是听 `MailboxEnqueued`
//!
//! 因为投递类别分不出来。组合根有两个投递点，**都走 `NextModelRequest`**：
//!
//! - 唤醒：别人说话、受伤、任务终局。
//! - 帧：处境变化、拾取、在途 job 的**进展**（见 [`crate::frame`]）。
//!
//! 帧的发车闸门是「有非终局进展」，也就是说**只要同伴在动，帧就一直来**。若把帧
//! 算作打断，等待会在它最主要的用途上当场失效：开始挖 → `wait` → 进展帧 → 立刻
//! 醒 → 再 `wait` → ……——正是这件工具本来要消灭的轮询。
//!
//! 帧曾经走 `WhenIdle`，那时按投递类别分是够的；改成 `NextModelRequest` 之后就不
//! 够了。**把判据钉在唯一知道区别的地方**（组合根的唤醒投递点），比钉在一个已经
//! 不承载这个区分的类别上更结实——后者会在类别改动时静默失效，而且单测照样绿。
//!
//! 这与 `wake` 的既有口径一致：进展「是『还在走』，不是『出事了』」。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use agent::{AgentEvent, PortFuture};
use wait::{Interruptions, Woke};

#[derive(Debug, Default)]
pub struct Doorbell {
    /// 投递过多少批唤醒。
    enqueued: AtomicU64,
    /// 最近一次模型请求开始时的计数：模型看见的世界停在这一刻。
    seen: AtomicU64,
    bell: tokio::sync::Notify,
}

impl Doorbell {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 唤醒**已经进了信箱**之后敲一次。帧不敲。
    ///
    /// 敲在投递之后而不是之前：这样「铃响」蕴含「信确实在信箱里」，等待被叫醒
    /// 时工具说的「就在下面」才是真的。
    pub fn ring(&self) {
        self.enqueued.fetch_add(1, Ordering::SeqCst);
        self.bell.notify_waiters();
    }

    fn has_unseen_wake(&self) -> bool {
        self.enqueued.load(Ordering::SeqCst) > self.seen.load(Ordering::SeqCst)
    }
}

/// 只认一种事件：模型请求开始。观察端是旁路，不改任何行为。
impl agent::Observer for Doorbell {
    fn observe(&self, event: &AgentEvent) {
        if matches!(event, AgentEvent::ModelRequestStarted { .. }) {
            self.seen
                .store(self.enqueued.load(Ordering::SeqCst), Ordering::SeqCst);
        }
    }
}

impl Interruptions for Doorbell {
    fn until_woken<'a>(&'a self, at_most: Duration) -> PortFuture<'a, Woke> {
        Box::pin(async move {
            // 先注册再查计数。`notify_waiters` 不留 permit，反过来会在「查完」到
            // 「注册上」之间丢掉铃声。
            let mut ringing = std::pin::pin!(self.bell.notified());
            ringing.as_mut().enable();

            if self.has_unseen_wake() {
                return Woke::Interrupted;
            }

            tokio::select! {
                _ = ringing => Woke::Interrupted,
                _ = tokio::time::sleep(at_most) => Woke::Timeout,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use agent::{Observer, RunId};

    use super::*;

    fn request_started() -> AgentEvent {
        AgentEvent::ModelRequestStarted {
            sequence: 2,
            run_id: RunId::new("run-1"),
            request_index: 1,
            transcript_items: 1,
            function_tools: 1,
        }
    }

    /// 模型「决定要等」到「等真的开始」之间隔着一次推理（实测 3–4 秒）。这段里
    /// 送达的唤醒已经并进本轮，模型却还没看见——不能让它去睡。
    #[tokio::test]
    async fn a_wake_that_landed_while_the_model_was_thinking_skips_the_wait() {
        let bell = Doorbell::new();
        bell.observe(&request_started());
        bell.ring();

        let at = std::time::Instant::now();
        assert_eq!(
            bell.until_woken(Duration::from_secs(300)).await,
            Woke::Interrupted
        );
        assert!(at.elapsed() < Duration::from_secs(1), "不该真的去睡");
    }

    /// **帧不敲铃，所以帧不打断等待。** 帧的闸门是「有非终局进展」，只要同伴在动
    /// 它就一直来；算作打断的话，等待会在它最主要的用途上当场失效——开始挖、
    /// `wait`、进展帧、立刻醒、再 `wait`，正是这件工具要消灭的轮询。
    ///
    /// 这条不能靠「帧走别的投递类别」来保证：帧和唤醒现在都走
    /// `NextModelRequest`，唯一的区别就是组合根敲不敲铃。
    #[tokio::test]
    async fn anything_that_does_not_ring_leaves_the_wait_alone() {
        let bell = Doorbell::new();
        bell.observe(&request_started());

        assert_eq!(
            bell.until_woken(Duration::from_millis(50)).await,
            Woke::Timeout
        );
    }

    /// 快照跟上之后，同一批唤醒不能第二次把等待掐断。
    #[tokio::test]
    async fn a_wake_the_model_has_already_seen_does_not_count() {
        let bell = Doorbell::new();
        bell.ring();
        bell.observe(&request_started());

        assert_eq!(
            bell.until_woken(Duration::from_millis(50)).await,
            Woke::Timeout
        );
    }

    /// 等待中途到的唤醒要当场把它叫醒，而不是等满。
    #[tokio::test]
    async fn a_wake_arriving_mid_wait_rings_the_bell() {
        let bell = Doorbell::new();
        bell.observe(&request_started());

        let ringer = bell.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            ringer.ring();
        });

        let at = std::time::Instant::now();
        assert_eq!(
            bell.until_woken(Duration::from_secs(300)).await,
            Woke::Interrupted
        );
        assert!(at.elapsed() < Duration::from_secs(5), "该被叫醒，不是等满");
    }
}
