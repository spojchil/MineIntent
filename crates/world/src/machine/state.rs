//! 机器与外界的共享面：最新快照、三个时间窗、写口队列、在途移动 job。
//!
//! 纯状态转换都在这里，**可以脱离 azalea 单测**——本文件的测试就是这么写的。
//!
//! 三个窗（聊天/伤害/任务）共用一个单调 `fact_seq`：跨窗可比先后，
//! 而各读方的游标互不干扰。tick 在同一游戏刻内会重复，要「恰好一次」地
//! 消费必须用 seq 做游标。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use azalea::protocol::packets::game::c_game_event;
use parking_lot::{Mutex, RwLock};
use tokio::sync::{oneshot, watch, Notify};

use super::door::{DoorCommand, PendingCommand};
use super::movement::MovementJob;
use super::{DAMAGE_WINDOW_ENTRIES, INVENTORY_WINDOW_ENTRIES, JOBS_WINDOW_ENTRIES};
use crate::{
    ChatContent, ChatEntry, ChatPosition, ConnectionPhase, DamageEntry, Epoch, FactSource,
    InventoryChangeEntry, JobEntry, JobKind, JobOutcome, PlayerRef, TickSnapshot, Window,
    WorldMeta, CHAT_WINDOW_LINES,
};

/// 预期回声的时限：swap/丢弃后这么多 tick 内，同格的 SetSlot 视为自己
/// 动作的回声（Commanded）。服务器确认通常一两个 tick 内到达。
const EXPECTED_SLOT_TICKS: u64 = 40;

/// 机器与外界的共享面。纯状态转换都在这里，可脱离 azalea 单测。
pub(crate) struct Inner {
    pub(super) latest: RwLock<Arc<TickSnapshot>>,
    pub(super) ticked_tx: watch::Sender<u64>,
    pub(super) chat_window: Mutex<VecDeque<ChatEntry>>,
    pub(super) damage_window: Mutex<VecDeque<DamageEntry>>,
    pub(super) jobs_window: Mutex<VecDeque<JobEntry>>,
    /// 上一 tick 的生命值；下降即产伤害条目。None = 尚无基线（首帧不产）。
    pub(super) last_health: Mutex<Option<f64>>,
    /// 在途移动任务（单意图槽）。
    pub(super) movement_job: Mutex<Option<MovementJob>>,
    pub(super) inventory_window: Mutex<VecDeque<InventoryChangeEntry>>,
    /// 预期会变的格位（我们刚 swap/丢弃过）：(菜单号, 失效 tick)。
    pub(super) expected_slots: Mutex<Vec<(u16, u64)>>,
    pub(super) pending: Mutex<Vec<PendingCommand>>,
    /// 一次性跳跃的复位标记：跳跃布尔保持一整 tick 后放开。
    pub(super) jump_reset: AtomicBool,
    /// azalea 世界模型句柄（Spawn 登记）。方块读取走它的读锁，
    /// 可在任意线程进行——世界模型不是 ECS。
    pub(super) world_handle: Mutex<Option<Arc<RwLock<azalea::world::World>>>>,
    pub(super) stopping: AtomicBool,
    pub(super) shutdown: Notify,
    pub(super) tick: AtomicU64,
    /// 全部事实窗共用的单调到达序号（聊天/伤害/任务）：
    /// 跨窗可比先后，各窗游标互不干扰。
    pub(super) fact_seq: AtomicU64,
    pub(super) day_time: AtomicU64,
    /// (rain_level, thunder_level)。
    pub(super) weather: Mutex<(f32, f32)>,
    pub(super) dimension: Mutex<String>,
}

pub(super) const EPOCH: Epoch = Epoch(1);

impl Inner {
    pub(super) fn new() -> Self {
        let (ticked_tx, _) = watch::channel(0);
        Self {
            latest: RwLock::new(Arc::new(TickSnapshot::empty(
                EPOCH,
                0,
                ConnectionPhase::Connecting,
            ))),
            ticked_tx,
            chat_window: Mutex::new(VecDeque::new()),
            damage_window: Mutex::new(VecDeque::new()),
            jobs_window: Mutex::new(VecDeque::new()),
            last_health: Mutex::new(None),
            movement_job: Mutex::new(None),
            inventory_window: Mutex::new(VecDeque::new()),
            expected_slots: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            jump_reset: AtomicBool::new(false),
            world_handle: Mutex::new(None),
            stopping: AtomicBool::new(false),
            shutdown: Notify::new(),
            tick: AtomicU64::new(0),
            fact_seq: AtomicU64::new(0),
            day_time: AtomicU64::new(0),
            weather: Mutex::new((0.0, 0.0)),
            dimension: Mutex::new("minecraft:overworld".to_owned()),
        }
    }

    pub(super) fn publish(&self, snapshot: TickSnapshot) {
        // 停机后在途回调不得把终态改写回 Ready/Disconnected；
        // Stopped 相自己（stop 发布的那次）放行。
        if self.stopping.load(Ordering::Acquire)
            && !matches!(snapshot.phase, ConnectionPhase::Stopped { .. })
        {
            return;
        }
        *self.latest.write() = Arc::new(snapshot);
        // 通知合并：错过十次只醒一次，醒后读方自己取 latest。
        self.ticked_tx.send_modify(|counter| *counter += 1);
    }

    /// 非就绪相的快照：连接事实 + 各事实窗（窗是机器的记忆，不随相清空）。
    pub(super) fn publish_phase(&self, phase: ConnectionPhase) {
        let mut snapshot = TickSnapshot::empty(EPOCH, self.tick.load(Ordering::Acquire), phase);
        snapshot.chat = self.chat_window_now();
        snapshot.damage = self.damage_window_now();
        snapshot.jobs = self.jobs_window_now();
        snapshot.inventory_changes = self.inventory_window_now();
        self.publish(snapshot);
    }

    pub(super) fn chat_window_now(&self) -> Window<ChatEntry> {
        Window {
            entries: self.chat_window.lock().iter().cloned().collect(),
        }
    }

    pub(super) fn damage_window_now(&self) -> Window<DamageEntry> {
        Window {
            entries: self.damage_window.lock().iter().cloned().collect(),
        }
    }

    pub(super) fn jobs_window_now(&self) -> Window<JobEntry> {
        Window {
            entries: self.jobs_window.lock().iter().cloned().collect(),
        }
    }

    pub(super) fn inventory_window_now(&self) -> Window<InventoryChangeEntry> {
        Window {
            entries: self.inventory_window.lock().iter().cloned().collect(),
        }
    }

    /// 标记这些菜单格即将因我们的动作而变：时限内的 SetSlot 算回声。
    pub(super) fn mark_expected_slots(&self, slots: &[u16]) {
        let until = self.tick.load(Ordering::Acquire) + EXPECTED_SLOT_TICKS;
        let mut expected = self.expected_slots.lock();
        for &slot in slots {
            expected.push((slot, until));
        }
    }

    /// 容器 0 的格位变化入窗（ContainerSetSlot 包直译）。
    /// 在预期时限内的格标 Commanded（自己动作的回声），其余 ServerObserved。
    pub(super) fn push_inventory_change(
        &self,
        slot: u16,
        item_name: Option<String>,
        count: u32,
    ) {
        let tick = self.tick.load(Ordering::Acquire);
        let source = {
            let mut expected = self.expected_slots.lock();
            expected.retain(|(_, until)| *until >= tick);
            if expected.iter().any(|(expected_slot, _)| *expected_slot == slot) {
                FactSource::Commanded
            } else {
                FactSource::ServerObserved
            }
        };
        let entry = InventoryChangeEntry {
            seq: self.fact_seq.fetch_add(1, Ordering::AcqRel),
            tick,
            occurred_at: SystemTime::now(),
            source,
            slot,
            item_name,
            count,
        };
        let mut window = self.inventory_window.lock();
        window.push_back(entry);
        while window.len() > INVENTORY_WINDOW_ENTRIES {
            window.pop_front();
        }
    }

    /// 生命对比产伤害条目。回升（治疗/重生）只更新基线，不产条目。
    ///
    /// 由 `ClientboundSetHealth` 包驱动，不做每 tick 采样：自动重生把
    /// 死亡瞬间的 14→0→20 压进一个 tick 里，采样会整个错过 0（实测发生），
    /// 而每次 SetHealth 包都是一次权威变化，原版客户端也以它为准。
    pub(super) fn track_health(&self, health: f64) {
        let mut last = self.last_health.lock();
        let previous = last.replace(health);
        let Some(previous) = previous else { return };
        // 阈值挡住浮点噪声；真实伤害最小半颗心（1.0）。
        if health < previous - 0.01 {
            let entry = DamageEntry {
                seq: self.fact_seq.fetch_add(1, Ordering::AcqRel),
                tick: self.tick.load(Ordering::Acquire),
                occurred_at: SystemTime::now(),
                health_before: previous as f32,
                health_after: health as f32,
                // SetHealth 不携带伤因；有伤因的事件源落位后再填。
                cause: None,
            };
            let mut window = self.damage_window.lock();
            window.push_back(entry);
            while window.len() > DAMAGE_WINDOW_ENTRIES {
                window.pop_front();
            }
        }
    }

    pub(super) fn push_job(&self, destination: [i32; 3], outcome: JobOutcome) {
        let entry = JobEntry {
            seq: self.fact_seq.fetch_add(1, Ordering::AcqRel),
            tick: self.tick.load(Ordering::Acquire),
            occurred_at: SystemTime::now(),
            job: JobKind::MoveTo { destination },
            outcome,
        };
        let mut window = self.jobs_window.lock();
        window.push_back(entry);
        while window.len() > JOBS_WINDOW_ENTRIES {
            window.pop_front();
        }
    }

    /// 移动任务开槽：旧任务被顶替即出窗。
    pub(super) fn begin_movement_job(&self, destination: [i32; 3]) {
        let mut slot = self.movement_job.lock();
        if let Some(job) = slot.take() {
            self.push_job(job.destination, JobOutcome::Replaced);
        }
        *slot = Some(MovementJob {
            destination,
            started_tick: self.tick.load(Ordering::Acquire),
            armed: false,
            stall_notified: false,
        });
    }

    /// 停止动词：在途任务如实出窗。没任务时不是错误。
    pub(super) fn end_movement_job_stopped(&self) {
        if let Some(job) = self.movement_job.lock().take() {
            self.push_job(job.destination, JobOutcome::Stopped);
        }
    }

    pub(super) fn push_chat(&self, sender: Option<(String, Option<String>)>, plain_text: String) {
        let entry = ChatEntry {
            seq: self.fact_seq.fetch_add(1, Ordering::AcqRel),
            tick: self.tick.load(Ordering::Acquire),
            occurred_at: SystemTime::now(),
            source: FactSource::ServerObserved,
            sender: sender.map(|(username, uuid)| PlayerRef { username, uuid }),
            content: ChatContent {
                plain_text,
                position: Some(ChatPosition::Chat),
                verified: None,
            },
        };
        let mut window = self.chat_window.lock();
        window.push_back(entry);
        while window.len() > CHAT_WINDOW_LINES {
            window.pop_front();
        }
    }

    pub(super) fn apply_set_time(&self, overworld_total_ticks: u64) {
        self.day_time
            .store(overworld_total_ticks, Ordering::Release);
    }

    pub(super) fn apply_game_event(&self, event: c_game_event::EventType, param: f32) {
        let mut weather = self.weather.lock();
        match event {
            c_game_event::EventType::StartRaining => weather.0 = 1.0,
            c_game_event::EventType::StopRaining => weather.0 = 0.0,
            c_game_event::EventType::RainLevelChange => weather.0 = param,
            c_game_event::EventType::ThunderLevelChange => weather.1 = param,
            _ => {}
        }
    }

    pub(super) fn world_meta_now(&self) -> WorldMeta {
        let (rain_level, thunder_level) = *self.weather.lock();
        WorldMeta {
            dimension: self.dimension.lock().clone(),
            day_time: self.day_time.load(Ordering::Acquire),
            rain_level,
            thunder_level,
        }
    }

    /// 写口动词入队；tick 回调里执行。机器已停/断线时立即拒绝。
    ///
    /// 检查与插入在同一把 pending 锁内：排空方（断线/停机）同样持这把锁，
    /// 所以任何入队要么先于排空（被排空如实拒绝），要么后于排空（此时
    /// 相/停机标志已可见，进不了队列）——没有错过排空的第三种命运。
    pub(super) fn enqueue_command(
        &self,
        command: DoorCommand,
    ) -> oneshot::Receiver<Result<(), String>> {
        let (ack, receiver) = oneshot::channel();
        let mut pending = self.pending.lock();
        let phase_ready = matches!(self.latest.read().phase, ConnectionPhase::Ready);
        if self.stopping.load(Ordering::Acquire) || !phase_ready {
            drop(pending);
            let _ = ack.send(Err("尚未连接到世界，无法行动".to_owned()));
            return receiver;
        }
        pending.push(PendingCommand { command, ack });
        receiver
    }

    pub(super) fn fail_all_pending_chat(&self, reason: &str) {
        for pending in self.pending.lock().drain(..) {
            let _ = pending.ack.send(Err(reason.to_owned()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_window_keeps_at_most_the_vanilla_line_count() {
        let inner = Inner::new();
        for index in 0..150 {
            inner.push_chat(Some(("alice".to_owned(), None)), format!("第 {index} 句"));
        }
        let window = inner.chat_window_now();
        assert_eq!(window.entries.len(), CHAT_WINDOW_LINES);
        assert_eq!(window.entries[0].content.plain_text, "第 50 句");
        assert_eq!(window.entries[99].content.plain_text, "第 149 句");
        // seq 单调且不随窗口逐出重置：同 tick 多条消息靠它做恰好一次消费。
        assert_eq!(window.entries[0].seq, 50);
        assert_eq!(window.entries[99].seq, 149);
    }

    #[test]
    fn health_drop_produces_damage_entry_and_recovery_does_not() {
        let inner = Inner::new();
        inner.track_health(20.0); // 基线，不产条目
        inner.track_health(20.0); // 不变
        inner.track_health(13.5); // 下降
        inner.track_health(17.0); // 治疗回升
        inner.track_health(17.0 - 0.001); // 浮点噪声，不产
        inner.track_health(0.0); // 致死一击

        let window = inner.damage_window_now();
        assert_eq!(window.entries.len(), 2);
        assert_eq!(window.entries[0].health_before, 20.0);
        assert_eq!(window.entries[0].health_after, 13.5);
        assert_eq!(window.entries[1].health_after, 0.0);
        // seq 与聊天同源单调。
        assert!(window.entries[0].seq < window.entries[1].seq);
    }

    #[test]
    fn movement_job_replacement_and_stop_are_recorded() {
        let inner = Inner::new();
        inner.begin_movement_job([10, 64, -3]);
        inner.begin_movement_job([20, 64, 5]); // 顶替
        inner.end_movement_job_stopped(); // 停止
        inner.end_movement_job_stopped(); // 没任务时不是事件

        let window = inner.jobs_window_now();
        let outcomes: Vec<JobOutcome> = window.entries.iter().map(|entry| entry.outcome).collect();
        assert_eq!(outcomes, vec![JobOutcome::Replaced, JobOutcome::Stopped]);
        assert_eq!(
            window.entries[0].job,
            JobKind::MoveTo {
                destination: [10, 64, -3]
            }
        );
    }

    /// 判定表全景：armed 前后各态的行动结论。
    #[test]
    fn weather_and_time_transitions_follow_game_events() {
        let inner = Inner::new();
        inner.apply_set_time(37_500);
        inner.apply_game_event(c_game_event::EventType::StartRaining, 0.0);
        let meta = inner.world_meta_now();
        assert_eq!(meta.day_time, 37_500);
        assert_eq!(meta.rain_level, 1.0);

        inner.apply_game_event(c_game_event::EventType::RainLevelChange, 0.4);
        inner.apply_game_event(c_game_event::EventType::ThunderLevelChange, 0.7);
        let meta = inner.world_meta_now();
        assert_eq!(meta.rain_level, 0.4);
        assert_eq!(meta.thunder_level, 0.7);

        inner.apply_game_event(c_game_event::EventType::StopRaining, 0.0);
        assert_eq!(inner.world_meta_now().rain_level, 0.0);
    }

    #[test]
    fn phase_snapshots_keep_the_chat_window_and_notify() {
        let inner = Inner::new();
        let ticked = inner.ticked_tx.subscribe();
        inner.push_chat(None, "服务器广播".to_owned());
        // 停机后 Ready 快照不得改写状态。
        inner.stopping.store(true, Ordering::Release);
        inner.publish(TickSnapshot::empty(EPOCH, 9, ConnectionPhase::Ready));
        assert!(
            !matches!(inner.latest.read().phase, ConnectionPhase::Ready),
            "停机期间的在途 Ready 快照应被丢弃"
        );
        inner.stopping.store(false, Ordering::Release);
        inner.publish_phase(ConnectionPhase::Disconnected {
            reason: "网络断开".to_owned(),
        });

        let latest = inner.latest.read().clone();
        assert!(
            matches!(&latest.phase, ConnectionPhase::Disconnected { reason } if reason == "网络断开")
        );
        assert_eq!(latest.chat.entries.len(), 1);
        assert!(ticked.has_changed().unwrap());
    }

    #[tokio::test]
    async fn chat_enqueued_before_ready_is_rejected_immediately() {
        let inner = Inner::new();
        let receiver = inner.enqueue_command(DoorCommand::Chat("你好".to_owned()));
        let outcome = receiver.await.expect("ack 应送达");
        assert!(outcome.is_err());
        assert!(inner.pending.lock().is_empty());
    }
}
