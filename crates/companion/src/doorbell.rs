//! 门铃：`wait` 期间「有没有它还没看见的信」。
//!
//! # 判据不是「等待期间有事」，是「有它还没看见的事」
//!
//! 天真的写法是等待期间挂一个通知回调。它会漏掉一段：模型**想**等的那一刻，
//! 到 `wait` 真正开始等，中间隔着一次模型推理（实测 3–4 秒）。这段时间里送达
//! 的信已经并进了这一轮，模型却要等到 `wait` 返回才看得见。
//!
//! 所以判据取两个计数的差：内核每收一批信记一次数（`MailboxEnqueued`），每开始
//! 一次模型请求把当时的计数拍一张快照（`ModelRequestStarted`）。**计数比快照大
//! = 有它没看过的信**，那就一秒都不该等。
//!
//! # 只有 `NextModelRequest` 算数：进展不是「出事了」
//!
//! 组合根有两条投递通道，装的东西完全不同：
//!
//! - `NextModelRequest`（唤醒）：别人说话、受伤、任务**终局**。
//! - `WhenIdle`（帧）：任务**进展**、处境、拾取。
//!
//! 只有前者能打断等待。帧的闸门是「任务还在进行中」（见 `frame::FrameComposer`
//! 的 `progress.is_empty()`），也就是说**只要同伴在动，帧就一直来**。把帧算作
//! 打断，等待就会在它最主要的用途上当场失效：开始挖 → `wait` → 进展帧 →
//! 立刻醒 → 再 `wait` → ……——正是这件工具本来要消灭的轮询。
//!
//! `wake.rs` 早就把这条原则写下来了：进展「是『还在走』，不是『出事了』」。
//! 工具描述承诺的也正是那三件：别人说话、你受伤、你下的任务有了结果。
//!
//! `Passive` 更不算：它按定义就是「知道就好，不值得为它跑一轮」。
//!
//! # 醒来时把攒着的帧捎上
//!
//! [`Delivery::WhenIdle`] 只在「本次运行原本将要结束时」排空，而 `wait` 是一次
//! 工具调用——轮还在跑，帧就一直扣着。等待被打断时下一次请求马上就要发生，此时
//! 把攒下的帧提成 `NextModelRequest` 让它搭车，比堆到轮末一次性倒出来好。
//!
//! 只在**醒来**时提，不在开始等时提：那样等待自己会把帧变成打断源，绕回上面
//! 那个环。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use agent::{AgentEvent, AgentSession, Delivery, PortFuture};
use wait::{Interruptions, Woke};

pub struct Doorbell {
    /// 收到过多少批**会叫醒**的信。
    enqueued: AtomicU64,
    /// 最近一次模型请求开始时的计数：模型看见的世界停在这一刻。
    seen: AtomicU64,
    bell: tokio::sync::Notify,
    /// 迟绑：会话要装完观察者才建得出来，而观察者就是本类型。
    /// 存 `Weak` 断开环——会话经 FanOut 持有本类型。
    session: OnceLock<Weak<AgentSession>>,
}

impl Doorbell {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            enqueued: AtomicU64::new(0),
            seen: AtomicU64::new(0),
            bell: tokio::sync::Notify::new(),
            session: OnceLock::new(),
        })
    }

    /// 会话建好后回填。只能填一次；重复填是装配错误，直接忽略。
    pub fn attach(&self, session: &Arc<AgentSession>) {
        let _ = self.session.set(Arc::downgrade(session));
    }

    fn has_unseen_mail(&self) -> bool {
        self.enqueued.load(Ordering::SeqCst) > self.seen.load(Ordering::SeqCst)
    }

    /// 把扣在「轮末」的帧提到「下次请求前」。只在醒来那一刻调用。
    async fn deliver_idle_mail(&self) {
        let Some(session) = self.session.get().and_then(Weak::upgrade) else {
            // 没绑上会话：等待仍然可用，只是 `WhenIdle` 照旧扣到轮末。
            return;
        };
        if let Err(error) = session
            .reschedule(Delivery::WhenIdle, Delivery::NextModelRequest)
            .await
        {
            eprintln!("[门铃] 提投递失败（等待仍然照常）：{error}");
        }
    }
}

/// 只认两种事件，其余一概不管；观察端是旁路，不改任何行为。
impl agent::Observer for Doorbell {
    fn observe(&self, event: &AgentEvent) {
        match event {
            // 只有唤醒通道算数；帧（`WhenIdle`）是进展，`Passive` 按定义不叫醒。
            AgentEvent::MailboxEnqueued { delivery, .. }
                if *delivery == Delivery::NextModelRequest =>
            {
                self.enqueued.fetch_add(1, Ordering::SeqCst);
                self.bell.notify_waiters();
            }
            AgentEvent::ModelRequestStarted { .. } => {
                self.seen
                    .store(self.enqueued.load(Ordering::SeqCst), Ordering::SeqCst);
            }
            _ => {}
        }
    }
}

impl Interruptions for Doorbell {
    fn until_woken<'a>(&'a self, at_most: Duration) -> PortFuture<'a, Woke> {
        Box::pin(async move {
            // 先注册再查计数。`notify_waiters` 不留 permit，反过来会在
            // 「查完」到「注册上」之间丢掉通知。
            let mut ringing = std::pin::pin!(self.bell.notified());
            ringing.as_mut().enable();

            if self.has_unseen_mail() {
                return Woke::Interrupted;
            }

            let woke = tokio::select! {
                _ = ringing => Woke::Interrupted,
                _ = tokio::time::sleep(at_most) => Woke::Timeout,
            };
            if woke == Woke::Interrupted {
                // 下一次请求马上就发生，让攒着的帧搭这趟车。
                self.deliver_idle_mail().await;
            }
            woke
        })
    }
}

#[cfg(test)]
mod tests {
    use agent::{Observer, RunId};

    use super::*;

    fn enqueued(delivery: Delivery) -> AgentEvent {
        AgentEvent::MailboxEnqueued {
            sequence: 1,
            run_id: None,
            delivery,
            item_count: 1,
        }
    }

    fn request_started() -> AgentEvent {
        AgentEvent::ModelRequestStarted {
            sequence: 2,
            run_id: RunId::new("run-1"),
            request_index: 1,
            transcript_items: 1,
            function_tools: 1,
        }
    }

    /// 没绑会话也要能用：提投递失败不该把等待一起拖垮。
    #[tokio::test]
    async fn mail_that_arrived_while_the_model_was_thinking_skips_the_wait() {
        let bell = Doorbell::new();
        bell.observe(&request_started());
        bell.observe(&enqueued(Delivery::NextModelRequest));

        let at = std::time::Instant::now();
        assert_eq!(
            bell.until_woken(Duration::from_secs(300)).await,
            Woke::Interrupted
        );
        assert!(at.elapsed() < Duration::from_secs(1), "不该真的去睡");
    }

    /// **帧不能打断等待。** 帧的闸门是「任务还在进行中」，所以只要同伴在动它
    /// 就一直来；把它算作打断，等待会在它最主要的用途上当场失效——开始挖、
    /// `wait`、进展帧、立刻醒、再 `wait`，正是这件工具要消灭的轮询。
    /// `Passive` 同理：它按定义就不叫醒。
    #[tokio::test]
    async fn progress_frames_and_passive_mail_do_not_cut_a_wait_short() {
        for delivery in [Delivery::WhenIdle, Delivery::Passive] {
            let bell = Doorbell::new();
            bell.observe(&request_started());
            bell.observe(&enqueued(delivery));

            assert_eq!(
                bell.until_woken(Duration::from_millis(50)).await,
                Woke::Timeout,
                "{delivery:?} 不该把等待掐断"
            );
        }
    }

    /// 快照跟上之后，同一批信不能第二次把等待掐断。
    #[tokio::test]
    async fn mail_the_model_has_already_seen_does_not_count() {
        let bell = Doorbell::new();
        bell.observe(&enqueued(Delivery::NextModelRequest));
        bell.observe(&request_started());

        assert_eq!(
            bell.until_woken(Duration::from_millis(50)).await,
            Woke::Timeout
        );
    }

    /// 等待中途到的信要当场把它叫醒，而不是等满。
    #[tokio::test]
    async fn mail_arriving_mid_wait_rings_the_bell() {
        let bell = Doorbell::new();
        bell.observe(&request_started());

        let ringer = bell.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            ringer.observe(&enqueued(Delivery::NextModelRequest));
        });

        let at = std::time::Instant::now();
        assert_eq!(
            bell.until_woken(Duration::from_secs(300)).await,
            Woke::Interrupted
        );
        assert!(at.elapsed() < Duration::from_secs(5), "该被叫醒，不是等满");
    }
}
