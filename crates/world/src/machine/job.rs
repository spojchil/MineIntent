//! 任务槽：把「后台任务」这个心智模型落在类型和 API 上。
//!
//! # 为什么要有这一层
//!
//! 此前每个动词各写各的：移动在 `poll` 里手写 `slot.take()` 再 `push_job`，
//! 挖掘把收槽封在 `state.rs` 的 `end_mining_job`。两边说是同构，实际连
//! 「卡住算不算结束」都不一样。第三个动词（`pillar_up`）来时没有模板可抄，
//! 只能再发明一遍，而它发明出来的那一版恰好漏了终局——83 次调用一条终局都没
//! 发出，模型只好自己猜，猜成了「工具有 bug」并写进长期记忆。
//!
//! # 不变量：沉默不等于成功
//!
//! **每个任务恰好一条终局，任何终止路径都必须写出它。**这里靠 API 形状保证：
//! 槽位内部的 `Option` 不外露，清空只有四条路——[`JobSlot::begin`] 顶替、
//! [`JobSlot::poll`] 返回 [`Step::End`]、[`JobSlot::cancel`]、连接生命周期结束——
//! 每条都必然落一条终局事实。**没有对外的 `take()`，所以忘不掉。**
//!
//! 槽位本身不设泛化看门狗：那会把动词漏掉终局的实现缺陷伪装成普通超时。
//! 真正需要时间边界的动词必须在自己的可观察状态机里明确建模并测试。

use parking_lot::Mutex;

use super::state::Inner;
use crate::{JobFact, JobId, JobStatus};

/// 一个动词要回答的问题。每个持续动作实现它，形状就统一了。
pub(super) trait JobVerb: Sized {
    /// 该动词的事件枚举（`MoveEvent` / `MineEvent`）。
    type Event: Copy + std::fmt::Debug;

    /// 用当前任务参数 + 事件拼一条事实。
    fn fact(&self, event: Self::Event) -> JobFact;

    /// 被新意图顶替时落哪个事件。
    fn replaced() -> Self::Event;
    /// 被停止动词取消时落哪个事件。
    fn cancelled() -> Self::Event;
    /// 连接或整个模块结束时落哪个事件。
    fn connection_ended() -> Self::Event;

    /// 只读现状，供任务表用。
    fn status(&self, id: JobId, started_tick: u64, now_tick: u64) -> JobStatus;
}

/// 轮询的结论。**收槽的唯一出口是 [`Step::End`]。**
pub(super) enum Step<E> {
    /// 什么都不落，任务继续。
    Keep,
    /// 落一条进展，任务继续。事件必须是非终局的。
    Progress(E),
    /// 落一条终局，槽位就此清空。
    End(E),
}

struct Live<J> {
    id: JobId,
    job: J,
    started_tick: u64,
}

/// 单意图槽：同一动词同一时刻只有一个任务，新的顶替旧的。
pub(super) struct JobSlot<J> {
    live: Mutex<Option<Live<J>>>,
}

impl<J> Default for JobSlot<J> {
    fn default() -> Self {
        Self {
            live: Mutex::new(None),
        }
    }
}

impl<J: JobVerb> JobSlot<J> {
    /// 开新任务。**旧的自动以「被顶替」出窗**，返回新任务的 id。
    pub(super) fn begin(&self, inner: &Inner, job: J) -> JobId {
        let now = inner.now_tick();
        let id = inner.next_job_id();
        let mut slot = self.live.lock();
        let previous = slot.take();
        *slot = Some(Live {
            id,
            job,
            started_tick: now,
        });
        if let Some(live) = previous {
            // 固定锁序为 slot → jobs_window。状态转换与事实提交必须原子，否则并发
            // 生命周期收束可能夹进来，让同一任务的终局排在旧 Progress/Replaced 前面。
            inner.push_job_fact(live.id, live.job.fact(J::replaced()));
        }
        id
    }

    /// 每 tick 轮询一次。闭包拿到任务本体，返回该落什么。
    ///
    /// 返回本次落下的事件，供调用方做收尾副作用（如 `stop_pathfinding`）。
    /// 没有在途任务就什么都不做。
    pub(super) fn poll(
        &self,
        inner: &Inner,
        decide: impl FnOnce(&mut J) -> Step<J::Event>,
    ) -> Option<J::Event> {
        let mut slot = self.live.lock();
        let live = slot.as_mut()?;

        match decide(&mut live.job) {
            Step::Keep => None,
            Step::Progress(event) => {
                let fact = live.job.fact(event);
                debug_assert!(!fact.is_terminal(), "Step::Progress 收到终局事件 {event:?}");
                inner.push_job_fact(live.id, fact);
                Some(event)
            }
            Step::End(event) => {
                let live = slot.take().expect("上面刚借到 Some");
                let fact = live.job.fact(event);
                debug_assert!(fact.is_terminal(), "Step::End 收到非终局事件 {event:?}");
                inner.push_job_fact(live.id, fact);
                Some(event)
            }
        }
    }

    /// 取消在途任务。没有任务时不是错误（与 `release`／`stop` 的语义一致）。
    pub(super) fn cancel(&self, inner: &Inner) {
        self.finish(inner, J::cancelled());
    }

    /// 连接一旦结束就不再有 tick；在生命周期边界同步收槽。
    /// 返回是否真的结束了一个任务，重复的 client/swarm/thread 断线通知因此无害。
    pub(super) fn connection_ended(&self, inner: &Inner) -> bool {
        self.finish(inner, J::connection_ended())
    }

    fn finish(&self, inner: &Inner, event: J::Event) -> bool {
        let mut slot = self.live.lock();
        let Some(live) = slot.take() else {
            return false;
        };
        let fact = live.job.fact(event);
        debug_assert!(fact.is_terminal(), "生命周期收槽收到非终局事件 {event:?}");
        inner.push_job_fact(live.id, fact);
        true
    }

    /// 在途任务的只读现状。`None` = 现在没有在跑。
    pub(super) fn status(&self, now_tick: u64) -> Option<JobStatus> {
        let slot = self.live.lock();
        let live = slot.as_ref()?;
        Some(live.job.status(live.id, live.started_tick, now_tick))
    }

    /// 借用在途任务读一读（不改状态、不落事实）。
    pub(super) fn peek<T>(&self, read: impl FnOnce(&J) -> T) -> Option<T> {
        self.live.lock().as_ref().map(|live| read(&live.job))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use super::*;
    use crate::{JobStatusKind, MoveEvent};

    struct Fake {
        destination: [i32; 3],
    }

    impl JobVerb for Fake {
        type Event = MoveEvent;
        fn fact(&self, event: MoveEvent) -> JobFact {
            JobFact::Move {
                destination: self.destination,
                event,
            }
        }
        fn replaced() -> MoveEvent {
            MoveEvent::Replaced
        }
        fn cancelled() -> MoveEvent {
            MoveEvent::Cancelled
        }
        fn connection_ended() -> MoveEvent {
            MoveEvent::ConnectionEnded
        }
        fn status(&self, id: JobId, started_tick: u64, now_tick: u64) -> JobStatus {
            JobStatus {
                id,
                kind: JobStatusKind::Move {
                    destination: self.destination,
                    leg: None,
                },
                started_tick,
                elapsed_ticks: now_tick.saturating_sub(started_tick),
            }
        }
    }

    struct BlockingFake {
        entered_fact: Arc<Barrier>,
        release_fact: Arc<Barrier>,
        blocked_once: AtomicBool,
    }

    impl JobVerb for BlockingFake {
        type Event = MoveEvent;

        fn fact(&self, event: MoveEvent) -> JobFact {
            if matches!(event, MoveEvent::Leg { .. })
                && !self.blocked_once.swap(true, Ordering::AcqRel)
            {
                self.entered_fact.wait();
                self.release_fact.wait();
            }
            JobFact::Move {
                destination: [1, 0, 0],
                event,
            }
        }

        fn replaced() -> MoveEvent {
            MoveEvent::Replaced
        }
        fn cancelled() -> MoveEvent {
            MoveEvent::Cancelled
        }
        fn connection_ended() -> MoveEvent {
            MoveEvent::ConnectionEnded
        }

        fn status(&self, id: JobId, started_tick: u64, now_tick: u64) -> JobStatus {
            JobStatus {
                id,
                kind: JobStatusKind::Move {
                    destination: [1, 0, 0],
                    leg: None,
                },
                started_tick,
                elapsed_ticks: now_tick.saturating_sub(started_tick),
            }
        }
    }

    fn events(inner: &Inner) -> Vec<MoveEvent> {
        inner
            .jobs_window_now()
            .entries
            .iter()
            .map(|entry| match entry.fact {
                JobFact::Move { event, .. } => event,
                ref other => panic!("期望移动事实，得到 {other:?}"),
            })
            .collect()
    }

    /// 顶替必然为被顶替者写一条终局——**沉默不等于成功**。
    #[test]
    fn replacing_always_closes_the_previous_job() {
        let inner = Inner::new();
        let slot = JobSlot::<Fake>::default();
        let first = slot.begin(
            &inner,
            Fake {
                destination: [1, 0, 0],
            },
        );
        let second = slot.begin(
            &inner,
            Fake {
                destination: [2, 0, 0],
            },
        );
        assert_ne!(first, second, "顶替时新旧任务必须是两个 id");
        assert_eq!(events(&inner), vec![MoveEvent::Replaced]);
    }

    #[test]
    fn connection_end_closes_a_live_job_exactly_once_without_more_ticks() {
        let inner = Inner::new();
        let slot = JobSlot::<Fake>::default();
        slot.begin(
            &inner,
            Fake {
                destination: [1, 0, 0],
            },
        );

        assert!(slot.connection_ended(&inner));
        assert!(!slot.connection_ended(&inner));
        assert_eq!(events(&inner), vec![MoveEvent::ConnectionEnded]);
        assert!(slot.status(inner.now_tick()).is_none());
    }

    #[test]
    fn concurrent_connection_end_cannot_overtake_an_inflight_progress_fact() {
        let inner = Arc::new(Inner::new());
        let slot = Arc::new(JobSlot::<BlockingFake>::default());
        let entered_fact = Arc::new(Barrier::new(2));
        let release_fact = Arc::new(Barrier::new(2));
        slot.begin(
            &inner,
            BlockingFake {
                entered_fact: entered_fact.clone(),
                release_fact: release_fact.clone(),
                blocked_once: AtomicBool::new(false),
            },
        );

        let polling = {
            let inner = inner.clone();
            let slot = slot.clone();
            std::thread::spawn(move || {
                slot.poll(&inner, |_| Step::Progress(MoveEvent::Leg { to: [1, 0, 0] }))
            })
        };
        entered_fact.wait();

        let (ended_tx, ended_rx) = std::sync::mpsc::channel();
        let end_started = Arc::new(Barrier::new(2));
        let ending = {
            let inner = inner.clone();
            let slot = slot.clone();
            let end_started = end_started.clone();
            std::thread::spawn(move || {
                end_started.wait();
                ended_tx.send(slot.connection_ended(&inner)).unwrap();
            })
        };
        end_started.wait();
        assert!(
            ended_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "连接终局必须等在途 Progress 完成事实提交"
        );

        release_fact.wait();
        assert_eq!(
            polling.join().unwrap(),
            Some(MoveEvent::Leg { to: [1, 0, 0] })
        );
        ending.join().unwrap();
        assert!(ended_rx.recv().unwrap());
        assert_eq!(
            events(&inner),
            vec![MoveEvent::Leg { to: [1, 0, 0] }, MoveEvent::ConnectionEnded],
            "终局之后不能再补写进展"
        );
    }

    /// 取消也必然收口；没有任务时取消不是事件。
    #[test]
    fn cancelling_closes_and_cancelling_nothing_is_silent() {
        let inner = Inner::new();
        let slot = JobSlot::<Fake>::default();
        slot.begin(
            &inner,
            Fake {
                destination: [1, 0, 0],
            },
        );
        slot.cancel(&inner);
        slot.cancel(&inner);
        assert_eq!(events(&inner), vec![MoveEvent::Cancelled]);
        assert!(slot.status(0).is_none());
    }
}
