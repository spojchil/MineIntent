//! 未落盘事实的计数、worker 单步闸门，以及 journal 收录判定。

use super::*;

/// 可重建普通事实的按类型计数。
///
/// 它们不进 journal（见 `journal_type_for`），但「摄入了多少、什么类型」仍是
/// 排障要看的量。计数在 worker 线程上串行更新，用一把小锁即可，不引入
/// 额外依赖；读取方是 debug 快照与停机汇总。
#[derive(Debug, Default)]
pub struct IngestCounters {
    pub(super) counts: Mutex<std::collections::BTreeMap<String, u64>>,
}

impl IngestCounters {
    pub(super) fn record(&self, event_type: &str) {
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = counts.get_mut(event_type) {
            *existing = existing.saturating_add(1);
            return;
        }
        // 事件类型来自后端枚举与内部事实，是有界集合；仍然设一个上限，
        // 避免将来有人把可变字符串塞进 event_type 时这里无声长成内存泄漏。
        if counts.len() < 64 {
            counts.insert(event_type.to_owned(), 1);
        } else {
            *counts.entry("other".to_owned()).or_insert(0) += 1;
        }
    }

    /// 当前计数快照，按类型名有序。
    pub fn snapshot(&self) -> std::collections::BTreeMap<String, u64> {
        self.counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// `entity=32579,block=1243` 形式的单行摘要；无摄入时返回 None。
    pub fn summary_line(&self) -> Option<String> {
        let counts = self.snapshot();
        if counts.is_empty() {
            return None;
        }
        Some(
            counts
                .iter()
                .map(|(name, count)| format!("{name}={count}"))
                .collect::<Vec<_>>()
                .join(","),
        )
    }
}

/// worker 的单步闸门（测试用准入 seam，生产恒为放行）。
///
/// 饱和测试要的是「让 worker 恰好再处理一条」以腾出一个队列槽位。原先这是
/// 靠「每条事件都要等一次 journal 落盘」间接得到的副作用；journal 收窄后
/// 普通事实不再落盘，那个副作用消失。与其把磁盘写回热路径来喂测试，不如
/// 给出一个直说的闸门——那些测试本来测的就是队列准入语义，不是落盘时机。
///
/// 未 `limit()` 时 `pass()` 只做一次原子读即返回，生产路径无额外开销。
#[derive(Debug)]
pub struct WorkerGate {
    pub(super) limited: AtomicBool,
    pub(super) permits: tokio::sync::Semaphore,
    pub(super) entered: std::sync::atomic::AtomicU64,
    pub(super) entered_signal: tokio::sync::Notify,
}

impl Default for WorkerGate {
    fn default() -> Self {
        Self {
            limited: AtomicBool::new(false),
            permits: tokio::sync::Semaphore::new(0),
            entered: std::sync::atomic::AtomicU64::new(0),
            entered_signal: tokio::sync::Notify::new(),
        }
    }
}

impl WorkerGate {
    /// 开始限流：此后每条 item 都要消耗一个 `allow` 发放的许可。
    pub fn limit(&self) {
        self.limited.store(true, Ordering::Release);
    }

    /// 再放行 n 条。
    pub fn allow(&self, n: usize) {
        self.permits.add_permits(n);
    }

    /// 解除限流并唤醒所有等待者，避免测试收尾时 worker 卡在闸门上。
    pub fn release_all(&self) {
        self.limited.store(false, Ordering::Release);
        self.permits.add_permits(Self::RELEASE_PERMITS);
    }

    pub(super) const RELEASE_PERMITS: usize = 1024;

    /// worker 到达闸门的累计次数。
    pub fn entered(&self) -> u64 {
        self.entered.load(Ordering::Acquire)
    }

    /// 等到 worker 至少到达闸门 n 次（即已经停在那里）。
    pub async fn wait_entered(&self, n: u64) {
        loop {
            if self.entered() >= n {
                return;
            }
            let signal = self.entered_signal.notified();
            if self.entered() >= n {
                return;
            }
            signal.await;
        }
    }

    pub(super) async fn pass(&self) {
        if !self.limited.load(Ordering::Acquire) {
            return;
        }
        self.entered.fetch_add(1, Ordering::AcqRel);
        self.entered_signal.notify_waiters();
        if let Ok(permit) = self.permits.acquire().await {
            permit.forget();
        }
    }
}

/// 这条 WorkItem 该不该进 journal，以及以什么类型进。
///
/// journal 是产品事实的持久记录，读者是事后翻看的人；它没有读取 API，
/// 全部价值就是信噪比。把每条摄入事件都写进去会把真正的产品事实淹掉
/// （实测 100 秒 36,764 条信封对 4 条事实，且信封 payload 不含事实内容）。
///
/// oracle 对照：TS 侧 12 个 journal 写入点全是产品事实
/// （runtime.ts:152/166/247/303 与各 capability），从来没有「每条摄入事件
/// 记一笔」。可重建的普通事实（实体/方块/名单增量、遗漏标记）改为计数。
pub(super) fn journal_type_for(item: &WorkItem) -> Option<&'static str> {
    if item.wake.is_some() {
        // 被指名叫醒是产品事实，且是模型这一轮的起因。
        return Some("player.chat.received");
    }
    if item.terminal || item.scope_control || item.overflow.is_some() {
        // scope 迁移、终态与丢弃标记都保留落盘。
        //
        // 丢弃标记（overflow）本身把 scope_control 置了 true，本可以顺手一起
        // 收窄——实测一次 100 秒运行有 2,928 条，仍是本文件最大的单一来源。
        // 但 NEW-11 裁定 A 明确要求「可重建事实可丢并在原 loss position 形成
        // omission/overflow 事实」，且有具名回归钉住它与 scope 迁移的 ticket
        // 次序。那是已裁定的产品语义，不在本次收窄的授权范围内，留给维护者。
        return Some("participant.event");
    }
    None
}
