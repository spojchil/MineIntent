//! 帧：模型下一次开口之前，把它落下的那几行补上。
//!
//! # 这件东西是什么
//!
//! 一帧是三样东西合成的一条 user 消息：**处境变了的那几行**（见 [`situation`]）、
//! **攒下的拾取**、**在途 job 的进展**。方块不在里面——模型看世界走画面（`view`）。
//!
//! [`situation`]: crate::situation
//!
//! # 什么时候到
//!
//! 走 `NextModelRequest`：**每个**请求边界都合格，包括一批工具结果之后那个。
//! 所以「模型下一次开口之前」是字面意思。
//!
//! 曾经走 `WhenIdle`，那条只有完成边界合格，一轮里工具批之间的边界全部跳过。
//! 换掉是因为它买到的东西是零：两种投递唤醒空闲会话的行为一模一样，完成边界上
//! 也都强制再请求一次；差别只在轮内那些边界投不投。当时的理由是「忙时排在轮末，
//! 不插队」，可帧根本不打断什么——它落在边界上，与工具结果同一个位置进来。
//!
//! 代价量过：一轮里模型请求 14 次，`WhenIdle` 在第 2 到第 14 次之前一帧都不投；
//! 一场 27 次请求的会话总共只投出去 2 帧。模型的动作按批发生，它的处境却要等到
//! 它不再调工具才更新。
//!
//! 这件事此前叫「轮末帧」。那个名字把它说歪了：听起来像时机，实际是跳过了一轮里
//! 所有工具批之间的边界——它从来不是「一轮结束之后」才到的（内核在完成边界上排
//! 空到非空内容时并不收尾，而是接着再请求一次模型）。
//!
//! # 为什么只有进展能发车
//!
//! 处境里的位置移动时每 250 ms 都在变。让处境自己触发投递就等于每 250 ms 投一
//! 帧：位置单行变化会淹没信箱，模型忙着时一次并进就是上万 token 的无意义重复。
//! 有 job 进展时位置顺势搭车出去，不必自己叫车。
//!
//! 拾取同理，**搭车，不触发**：挖矿时进展本来就在发车，自己捡的东西顺势就出去
//! 了；空闲时被人塞了东西则要等下一次投递，那属于「空闲唤醒源」（issue #135）。
//!
//! 代价是没有进展时处境一步也不动：反复下达新任务、每次把上一个顶替掉时，终局
//! 是 `Replaced`（`wake` 判它不值得叫醒），也不是进展——两条通道都按设计沉默。
//! 判据本身还没改，先让它站到能被单测钉住的地方。

use world::{JobStage, TickSnapshot};

use crate::situation::SituationTracker;

/// 久没投递时最多攒多少条拾取。
///
/// 真攒到这一步说明久没发车，旧的拾取对「我现在有什么」已经由快捷栏那行代答了。
const PICKUP_BACKLOG: usize = 32;

/// 帧眼里的一条任务事实：序号、阶段和已经写好的那句话。帧只看阶段，不认识具体动作。
struct JobLine {
    seq: u64,
    stage: JobStage,
    text: String,
}

/// 攒帧的人：拿着两条游标、一份没吐出去的拾取，和处境的比对基准。
pub struct FrameComposer {
    situation: SituationTracker,
    /// job 进展的游标，与 `wake` 那条各走各的：进展是「还在走」，不该叫醒，
    /// 由帧搭车呈现；终局才是事件，走 `wake`。
    progress_seq: Option<u64>,
    /// 拾取游标与攒下的句子。**每次都排空，投递时才吐**——不排空的话，
    /// 窗只有 64 条，挖得快就会在两次投递之间被挤掉。
    pickup_seq: Option<u64>,
    pickup_lines: Vec<String>,
}

impl FrameComposer {
    pub fn new() -> Self {
        Self {
            situation: SituationTracker::new(),
            progress_seq: None,
            pickup_seq: None,
            pickup_lines: Vec::new(),
        }
    }

    /// 下一帧把处境从头说一遍。
    ///
    /// 压缩把对话换成摘要，先前追加的处境随之消失，而摘要按指令不含世界状态。
    /// 只置位，真正重投在下一次 [`compose`] 里发生——本次没进展而不发车，位也
    /// 留着不会丢。
    ///
    /// [`compose`]: Self::compose
    pub fn resend_situation(&mut self) {
        self.situation.request_full_resend();
    }

    /// 攒一次。`None` = 这一次不发车。
    ///
    /// 拾取的收集在闸门**之前**：不发车的那些次里它照样攒着，等下一次有车顺势
    /// 带走。处境的取用在闸门**之后**：`SituationTracker::take` 会立刻推进比对
    /// 基准，算了却没投出去的差异就永远丢了。
    pub fn compose(
        &mut self,
        snapshot: &TickSnapshot,
        chat_read: (u64, u64),
    ) -> Option<Vec<String>> {
        let jobs = self.unseen_jobs(snapshot);
        self.compose_lines(snapshot, chat_read, jobs)
    }

    /// 外接代理在工具回执里拉取：没有进展也返回变化的处境，不触发额外模型请求。
    pub fn compose_on_pull(
        &mut self,
        snapshot: &TickSnapshot,
        chat_read: (u64, u64),
    ) -> Vec<String> {
        let jobs = self.unseen_jobs(snapshot);
        self.collect_pickups(snapshot);
        let progress = self.take_progress(jobs);
        self.assemble(snapshot, chat_read, progress)
    }

    fn unseen_jobs(&self, snapshot: &TickSnapshot) -> Vec<JobLine> {
        // 只给游标之后的条目写句子；游标在下面推进。
        snapshot
            .jobs
            .entries
            .iter()
            .filter(|entry| !self.progress_seq.is_some_and(|seen| entry.seq <= seen))
            .map(|entry| JobLine {
                seq: entry.seq,
                stage: entry.fact.stage,
                text: render::render_job_entry(entry),
            })
            .collect()
    }

    fn compose_lines(
        &mut self,
        snapshot: &TickSnapshot,
        chat_read: (u64, u64),
        jobs: Vec<JobLine>,
    ) -> Option<Vec<String>> {
        self.collect_pickups(snapshot);

        let progress = self.take_progress(jobs);
        if progress.is_empty() {
            return None;
        }
        Some(self.assemble(snapshot, chat_read, progress))
    }

    fn take_progress(&mut self, jobs: Vec<JobLine>) -> Vec<String> {
        let mut progress = Vec::new();
        for line in jobs {
            if self.progress_seq.is_some_and(|seen| line.seq <= seen) {
                continue;
            }
            self.progress_seq = Some(line.seq);
            if !line.stage.is_terminal() {
                progress.push(line.text);
            }
        }
        progress
    }

    fn assemble(
        &mut self,
        snapshot: &TickSnapshot,
        chat_read: (u64, u64),
        progress: Vec<String>,
    ) -> Vec<String> {
        let mut sections = self
            .situation
            .take(render::render_situation_lines(snapshot, chat_read));
        sections.append(&mut self.pickup_lines);
        sections.extend(progress);
        sections
    }

    /// 拾取：世界事件，不受开屏与否管。
    fn collect_pickups(&mut self, snapshot: &TickSnapshot) {
        let fresh: Vec<world::PickupEntry> = snapshot
            .pickups
            .entries
            .iter()
            .filter(|entry| !self.pickup_seq.is_some_and(|seen| entry.seq <= seen))
            .cloned()
            .collect();
        if let Some(last) = fresh.last() {
            self.pickup_seq = Some(last.seq);
        }
        self.pickup_lines.extend(render::render_pickups(&fresh));
        if self.pickup_lines.len() > PICKUP_BACKLOG {
            let drop = self.pickup_lines.len() - PICKUP_BACKLOG;
            self.pickup_lines.drain(..drop);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use world::{ConnectionPhase, Epoch, JobEnd, PickupEntry};

    use super::*;

    const READ: (u64, u64) = (0, 0);

    fn snap(tick: u64, x: f64) -> TickSnapshot {
        let mut snap = TickSnapshot::empty(Epoch(1), tick, ConnectionPhase::Ready);
        snap.world_meta.dimension = "minecraft:overworld".to_owned();
        snap.self_state.entity_key = "self".to_owned();
        snap.self_state.username = "companion".to_owned();
        snap.self_state.position = world::Vec3Value { x, y: 64.0, z: 0.0 };
        snap.self_state.on_ground = true;
        snap.self_state.alive = true;
        snap.self_state.health = 20.0;
        snap.self_state.food = 20.0;
        snap
    }

    fn job(seq: u64, stage: JobStage) -> Vec<JobLine> {
        vec![JobLine {
            seq,
            stage,
            text: format!("任务事实 {seq}"),
        }]
    }

    fn pickup(seq: u64, item: &str) -> PickupEntry {
        PickupEntry {
            seq,
            tick: seq,
            occurred_at: SystemTime::now(),
            by_self: true,
            by: None,
            item_name: Some(item.to_owned()),
            count: 1,
        }
    }

    /// 闸门：只有在途进展能发车。
    #[test]
    fn without_progress_there_is_no_frame() {
        let mut frames = FrameComposer::new();
        assert!(frames.compose(&snap(1, 0.5), READ).is_none());
    }

    #[test]
    fn pulling_needs_no_progress_and_returns_only_changed_situation() {
        let mut frames = FrameComposer::new();
        let mut first = snap(1, 0.5);
        first.pickups.entries.push(pickup(1, "minecraft:coal"));
        let opening = frames.compose_on_pull(&first, READ);
        assert!(opening.iter().any(|line| line.contains("位置 (0, 64, 0)")));
        assert!(opening.iter().any(|line| line.contains("coal")));
        assert!(frames.compose_on_pull(&first, READ).is_empty());

        let moved = frames.compose_on_pull(&snap(2, 9.5), READ);
        assert!(moved.iter().any(|line| line.contains("位置 (9, 64, 0)")));
        frames.resend_situation();
        assert!(frames.compose_on_pull(&snap(2, 9.5), READ).len() > moved.len());
    }

    /// 任务反复重发，每次把上一个顶替掉。`Replaced` 是终局，
    /// 走 `wake` 而不是帧，而 `wake` 判它「自己干的不必回报」——于是两条通道
    /// 都沉默。这条钉住的是**帧这一侧**确实不发车，不是说这样就对。
    #[test]
    fn a_replaced_job_alone_drives_nothing() {
        let mut frames = FrameComposer::new();
        let snapshot = snap(1, 0.5);
        let replaced = job(1, JobStage::Ended(JobEnd::Replaced));
        assert!(frames.compose_lines(&snapshot, READ, replaced).is_none());
    }

    /// 拾取搭车不叫车：没车的那些次照样攒着，等有车了一并带走。
    #[test]
    fn pickups_wait_for_a_car_instead_of_being_dropped() {
        let mut frames = FrameComposer::new();
        let mut quiet = snap(1, 0.5);
        quiet.pickups.entries.push(pickup(1, "minecraft:coal"));
        assert!(frames.compose(&quiet, READ).is_none());

        let mut driving = snap(2, 0.5);
        driving.pickups.entries.push(pickup(1, "minecraft:coal"));
        let frame = frames
            .compose_lines(&driving, READ, job(1, JobStage::Progress))
            .expect("有进展就该发车");
        assert!(
            frame.iter().any(|line| line.contains("coal")),
            "攒下的拾取没跟车走：{frame:?}"
        );
    }

    /// 没发车的那些次不能推进处境的比对基准——否则那次差异就被吞掉了，
    /// 模型再也听不到。这里位置在无车期间变了，等来车时必须还说得出来。
    #[test]
    fn a_carless_round_does_not_swallow_the_situation_it_computed() {
        let mut frames = FrameComposer::new();
        let first = snap(1, 0.5);
        let opening = frames
            .compose_lines(&first, READ, job(1, JobStage::Progress))
            .expect("开局该投全量");
        assert!(opening.iter().any(|line| line.contains("位置 (0, 64, 0)")));

        // 走到了 x=9，但这一次没有新进展：不发车。
        assert!(frames.compose(&snap(2, 9.5), READ).is_none());

        // 下一次有车了，新位置必须还在。
        let later = snap(3, 9.5);
        let frame = frames
            .compose_lines(&later, READ, job(2, JobStage::Progress))
            .expect("有进展就该发车");
        assert!(
            frame.iter().any(|line| line.contains("位置 (9, 64, 0)")),
            "无车那次把位置差异吞了：{frame:?}"
        );
    }

    /// 久没发车就只留最近的：旧拾取对「我现在有什么」已经由快捷栏那行代答。
    #[test]
    fn a_long_silence_keeps_only_the_recent_pickups() {
        let mut frames = FrameComposer::new();
        for seq in 1..=(PICKUP_BACKLOG as u64 + 8) {
            let mut quiet = snap(seq, 0.5);
            quiet
                .pickups
                .entries
                .push(pickup(seq, &format!("minecraft:item_{seq}")));
            assert!(frames.compose(&quiet, READ).is_none());
        }
        let driving = snap(100, 0.5);
        let frame = frames
            .compose_lines(&driving, READ, job(1, JobStage::Progress))
            .expect("有进展就该发车");
        assert!(
            !frame.iter().any(|line| line.contains("item_1 ")),
            "最旧的拾取该被挤掉：{frame:?}"
        );
        assert!(
            frame.iter().any(|line| line.contains("item_40")),
            "最近的拾取必须还在：{frame:?}"
        );
    }

    /// 压缩把对话换成摘要，先前追加的处境随之消失：下一帧要从头说一遍。
    #[test]
    fn compaction_makes_the_next_frame_say_everything_again() {
        let mut frames = FrameComposer::new();
        let first = snap(1, 0.5);
        let opening = frames
            .compose_lines(&first, READ, job(1, JobStage::Progress))
            .expect("开局该投全量");

        let second = snap(2, 0.5);
        let quiet = frames
            .compose_lines(&second, READ, job(2, JobStage::Progress))
            .expect("有进展就该发车");
        assert!(quiet.len() < opening.len(), "没变的行不该重复：{quiet:?}");

        frames.resend_situation();
        let third = snap(3, 0.5);
        let after = frames
            .compose_lines(&third, READ, job(3, JobStage::Progress))
            .expect("有进展就该发车");
        assert_eq!(
            after.len(),
            opening.len(),
            "压缩之后该把处境从头说一遍：{after:?}"
        );
    }
}
