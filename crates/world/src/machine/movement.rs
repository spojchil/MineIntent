//! 移动 job 的终局判定与每 tick 轮询。
//!
//! 判定本身是纯函数（[`movement_poll_step`]）：把寻路请求状态、真实身体
//! 推进与明确的运行边界压成一个结论，可以脱离 azalea 穷举单测。副作用只在
//! [`poll_movement_job`] 里落——出窗、清槽、停寻路。
//!
//! 判据全部来自 azalea 的可观察状态，不是猜的：
//! 到达 = Azalea 口径的自身方块位等于目的地；
//! 走完未达 = 已同步排队的请求被消费，随后计算与执行都停止且仍未到达；
//! 卡住与终止边界只看真实身体换格，不读会被局部补路重置的内部计数。

use std::collections::HashSet;

use azalea::entity::Position;
use azalea::pathfinder::{
    player_pos_to_block_pos, ExecutingPath, Pathfinder, PathfinderClientExt, PathfinderOpts,
};
use azalea::{BlockPos, Client};

use super::job::{JobVerb, Step};
use super::navigation::{FrontierGoal, FrontierKey};
use super::observed::{self, PlanningKey, PlanningSnapshot};
use super::state::Inner;
use super::{
    MOVEMENT_DISPATCH_TIMEOUT_TICKS, MOVEMENT_NO_BODY_PROGRESS_TICKS, MOVEMENT_STALL_TICKS,
};
use crate::{JobFact, JobId, JobStatus, JobStatusKind, MoveEvent};

/// 一次知识边界观察：四个水平象限 × 水平/向下/向上。视口纵向半角为 35°，
/// pitch 0/±55 的并集覆盖 -90°..+90°；水平半角约 51°，四向也完整覆盖一圈。
/// 每一眼都由 `scan_current_view` 当场按真实视锥吸收，不靠定时等待。
const SURVEY_VIEWS: [(f32, f32); 12] = [
    (0.0, 0.0),
    (90.0, 0.0),
    (-90.0, 0.0),
    (180.0, 0.0),
    (0.0, 55.0),
    (90.0, 55.0),
    (-90.0, 55.0),
    (180.0, 55.0),
    (0.0, -55.0),
    (90.0, -55.0),
    (-90.0, -55.0),
    (180.0, -55.0),
];

/// 战争迷雾位于开放世界，若精确目标不可达，就不存在一个有限的 frontier 集可供
/// “全部搜完”。给每次任务一份随初始曼哈顿距离增长的有限工作预算：投递一条 A*
/// 路段与身体每换一格都消耗一单位。这样原地重算、A↔B 往返和被外力持续推动都
/// 有同一个下降量；预算终局只说机器主动收束，不冒充不可达证明。
const MIN_NAVIGATION_EFFORT: u64 = 64;
/// 防止极端坐标把“有限”额度放大成实际上不可承受的 HashSet/运行时占用。命中仍只
/// 报机器资源边界，调用方可分段提出更近目标，不冒充世界不可达。
const MAX_NAVIGATION_EFFORT: u64 = 4_096;
const NAVIGATION_EFFORT_PER_DISTANCE: u64 = 8;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct PlanKey {
    at: BlockPos,
    knowledge_key: PlanningKey,
}

#[derive(Debug)]
enum NavigationPhase {
    /// 最终精确目标。
    Direct,
    /// 去一个动作级 frontier；达成只代表获得了新 viewpoint，不代表最终到达。
    Frontier {
        started_at: BlockPos,
        goal: FrontierGoal,
    },
    /// 路段结束后的主动环视。扫完才允许依据新版本重规划。
    Survey {
        at: BlockPos,
        base_yaw: f32,
        view: usize,
        reached_frontier: Option<FrontierKey>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NextPlan {
    Direct,
    Frontier,
    Stop,
}

#[derive(Clone, Debug)]
enum NavigationEffect {
    SurveyView {
        yaw: f32,
        pitch: f32,
        retire: bool,
    },
    Direct(PlanningSnapshot),
    Frontier {
        goal: FrontierGoal,
        snapshot: PlanningSnapshot,
    },
}

/// 在途的移动任务（单意图槽）。终局判定按 azalea 寻路器的可观察状态和真实
/// 身体位置交叉判断；fork 在关闭 partial continuation 后会收掉已经结束的
/// goal/opts。我们自己下的停止、顶替和高层换段在命令处同步 retirement。
pub(super) struct MovementJob {
    pub(super) destination: [i32; 3],
    /// 当前 A* 路段何时同步进入 Azalea 的 stamped Goto 队列；只用于判定
    /// `queued_goto_id` 长期不被 listener 消费的调度故障。
    pub(super) leg_issued_tick: u64,
    /// 卡住通知只发一次。
    pub(super) stall_notified: bool,
    /// 已经告诉过模型的这一程终点。**只读观察**——路一变就重说一句，
    /// 不据此判断到达。Azalea 只执行当前冻结快照算出的路段；路段结束后的
    /// Survey/Direct/Frontier 选择归本状态机。
    pub(super) announced_leg_end: Option<[i32; 3]>,
    /// 上一次实际身体方块位。只有它变化才是物理推进。
    pub(super) last_block_pos: [i32; 3],
    /// 最近一次真实身体换格；投递新腿、换 patch 或宣布 Leg 都不会刷新它。
    pub(super) last_body_progress_tick: u64,
    phase: NavigationPhase,
    /// 同一身体格 + 同一知识版本，每种计划至多跑一次。它把“可以重规划”绑定到
    /// 身体推进或新观察，阻断同一 key 的定时忙重试；跨格往返由全局工作预算收束。
    direct_attempts: HashSet<PlanKey>,
    frontier_attempts: HashSet<PlanKey>,
    /// 没带来新信息的 viewpoint。key 含局部 Unseen 依赖集合；相关格一旦被观察，
    /// 同一 stance 会自然变成另一个 key，可以重新开放。
    exhausted_frontiers: HashSet<FrontierKey>,
    navigation_effort_limit: u64,
    plans_started: u64,
    travelled: u64,
}

impl MovementJob {
    pub(super) fn new(
        destination: [i32; 3],
        started_tick: u64,
        current: BlockPos,
        knowledge_key: Option<PlanningKey>,
    ) -> Self {
        let direct_attempts = knowledge_key
            .map(|knowledge_key| {
                HashSet::from([PlanKey {
                    at: current,
                    knowledge_key,
                }])
            })
            .unwrap_or_default();
        Self {
            destination,
            leg_issued_tick: started_tick,
            stall_notified: false,
            announced_leg_end: None,
            last_block_pos: [current.x, current.y, current.z],
            last_body_progress_tick: started_tick,
            phase: NavigationPhase::Direct,
            direct_attempts,
            frontier_attempts: HashSet::new(),
            exhausted_frontiers: HashSet::new(),
            navigation_effort_limit: navigation_effort_limit(current, as_block_pos(destination)),
            plans_started: u64::from([current.x, current.y, current.z] != destination),
            travelled: 0,
        }
    }

    /// 只有实际换格才算推进。
    fn observe_block_pos(&mut self, block_pos: [i32; 3], now_tick: u64) -> bool {
        let at = as_block_pos(block_pos);
        let previous = std::mem::replace(&mut self.last_block_pos, block_pos);
        if previous != block_pos {
            self.last_body_progress_tick = now_tick;
            self.stall_notified = false;
            self.travelled = self
                .travelled
                .saturating_add(manhattan(as_block_pos(previous), at));
            true
        } else {
            false
        }
    }

    fn reset_leg(&mut self, now_tick: u64) {
        self.leg_issued_tick = now_tick;
        self.announced_leg_end = None;
    }

    fn begin_direct(&mut self, at: BlockPos, knowledge_key: PlanningKey, now_tick: u64) {
        self.record_plan();
        self.direct_attempts.insert(PlanKey { at, knowledge_key });
        self.phase = NavigationPhase::Direct;
        self.reset_leg(now_tick);
    }

    fn begin_frontier(
        &mut self,
        at: BlockPos,
        snapshot: PlanningSnapshot,
        now_tick: u64,
    ) -> FrontierGoal {
        self.record_plan();
        let knowledge_key = snapshot.key();
        self.frontier_attempts.insert(PlanKey { at, knowledge_key });
        let destination = as_block_pos(self.destination);
        let goal = FrontierGoal::new(
            snapshot.memory().clone(),
            destination,
            at,
            self.exhausted_frontiers.clone(),
        );
        self.phase = NavigationPhase::Frontier {
            started_at: at,
            goal: goal.clone(),
        };
        self.reset_leg(now_tick);
        goal
    }

    fn begin_survey(&mut self, at: BlockPos, reached_frontier: Option<FrontierKey>) -> (f32, f32) {
        let base_yaw = yaw_towards(at, as_block_pos(self.destination));
        self.phase = NavigationPhase::Survey {
            at,
            base_yaw,
            view: 0,
            reached_frontier,
        };
        self.announced_leg_end = None;
        survey_direction(base_yaw, 0)
    }

    fn next_plan(&self, at: BlockPos, knowledge_key: PlanningKey) -> NextPlan {
        let key = PlanKey { at, knowledge_key };
        if !self.direct_attempts.contains(&key) {
            NextPlan::Direct
        } else if !self.frontier_attempts.contains(&key) {
            NextPlan::Frontier
        } else {
            NextPlan::Stop
        }
    }

    fn record_plan(&mut self) {
        self.plans_started = self.plans_started.saturating_add(1);
    }

    fn navigation_effort(&self) -> u64 {
        self.plans_started.saturating_add(self.travelled)
    }

    fn navigation_limit_event(&self, at: BlockPos) -> Option<MoveEvent> {
        (self.navigation_effort() >= self.navigation_effort_limit).then_some(
            MoveEvent::NavigationLimitReached {
                at: [at.x, at.y, at.z],
                plans: self.plans_started,
                travelled: self.travelled,
            },
        )
    }
}

fn as_block_pos([x, y, z]: [i32; 3]) -> BlockPos {
    BlockPos::new(x, y, z)
}

fn manhattan(a: BlockPos, b: BlockPos) -> u64 {
    u64::from(a.x.abs_diff(b.x))
        .saturating_add(u64::from(a.y.abs_diff(b.y)))
        .saturating_add(u64::from(a.z.abs_diff(b.z)))
}

fn navigation_effort_limit(from: BlockPos, destination: BlockPos) -> u64 {
    MIN_NAVIGATION_EFFORT
        .max(manhattan(from, destination).saturating_mul(NAVIGATION_EFFORT_PER_DISTANCE))
        .min(MAX_NAVIGATION_EFFORT)
}

fn navigation_terminal(
    job: &MovementJob,
    current: BlockPos,
    destination_rejected: bool,
    observed_navigation: bool,
) -> Option<MoveEvent> {
    if [current.x, current.y, current.z] == job.destination {
        Some(MoveEvent::Arrived)
    } else if destination_rejected {
        Some(MoveEvent::DestinationRejected {
            at: [current.x, current.y, current.z],
        })
    } else if observed_navigation {
        job.navigation_limit_event(current)
    } else {
        None
    }
}

fn yaw_towards(from: BlockPos, to: BlockPos) -> f32 {
    let dx = f64::from(to.x) - f64::from(from.x);
    let dz = f64::from(to.z) - f64::from(from.z);
    if dx == 0.0 && dz == 0.0 {
        0.0
    } else {
        (-dx).atan2(dz).to_degrees() as f32
    }
}

fn survey_direction(base_yaw: f32, view: usize) -> (f32, f32) {
    let (offset, pitch) = SURVEY_VIEWS[view];
    (base_yaw + offset, pitch)
}

fn scheduled_survey_direction(job: &MovementJob) -> Option<(f32, f32)> {
    let NavigationPhase::Survey { base_yaw, view, .. } = &job.phase else {
        return None;
    };
    Some(survey_direction(*base_yaw, *view))
}

impl JobVerb for MovementJob {
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
                leg: self.announced_leg_end,
            },
            started_tick,
            elapsed_ticks: now_tick.saturating_sub(started_tick),
        }
    }
}

/// 移动 job 轮询的行动结论（纯函数，可单测）。
#[derive(Debug, PartialEq, Eq)]
enum MovementPollStep {
    /// 保持现状。
    Keep,
    /// 任务终局：出窗并清槽。
    End(MoveEvent),
    /// 卡住通知（任务继续）。
    Stall,
}

/// 判定表输入：本 tick 的全部可观察事实。`queued` 来自 fork 持久化的 stamped
/// request 状态；`active` 还包含 goal/opts/calculating/executing。
#[derive(Clone, Copy, Debug, Default)]
struct MovementPoll {
    at: [i32; 3],
    active: bool,
    queued: bool,
    executing: bool,
    dispatch_ticks: u64,
    body_inactive_ticks: u64,
    stall_notified: bool,
}

fn movement_poll_step(poll: MovementPoll) -> MovementPollStep {
    if !poll.active {
        // queued 是同步稳定状态；它消失后又没有任何计算/执行状态，说明 listener
        // 已经处理完请求。身体坐标由 navigation_terminal 更早判过，未到即走完未达。
        return MovementPollStep::End(MoveEvent::PathEnded);
    }
    if poll.queued && poll.dispatch_ticks >= MOVEMENT_DISPATCH_TIMEOUT_TICKS {
        return MovementPollStep::End(MoveEvent::DispatchNotObserved {
            at: poll.at,
            ticks: poll.dispatch_ticks,
        });
    }
    if poll.body_inactive_ticks >= MOVEMENT_NO_BODY_PROGRESS_TICKS {
        return MovementPollStep::End(MoveEvent::NoBodyProgressLimitReached {
            at: poll.at,
            ticks: poll.body_inactive_ticks,
        });
    }
    if poll.executing && poll.body_inactive_ticks >= MOVEMENT_STALL_TICKS && !poll.stall_notified {
        return MovementPollStep::Stall;
    }
    MovementPollStep::Keep
}

fn observed_snapshot(inner: &Inner) -> Option<PlanningSnapshot> {
    inner.observed.lock().clone()?.snapshot()
}

fn freeze_observed_source(inner: &Inner, snapshot: &PlanningSnapshot) {
    if let Some(source) = inner.observed.lock().clone() {
        source.freeze(snapshot);
    }
}

fn refresh_observed_execution_source(inner: &Inner, snapshot: &PlanningSnapshot) {
    if let Some(source) = inner.observed.lock().clone() {
        source.refresh_execution_source(snapshot);
    }
}

fn apply_navigation_effect(
    inner: &Inner,
    bot: &Client,
    destination: [i32; 3],
    effect: NavigationEffect,
) {
    match effect {
        NavigationEffect::SurveyView { yaw, pitch, retire } => {
            if retire {
                bot.force_retire_pathfinding();
            }
            bot.set_direction(yaw, pitch);
        }
        NavigationEffect::Direct(snapshot) => {
            bot.force_retire_pathfinding();
            freeze_observed_source(inner, &snapshot);
            super::door::begin_goto(bot, destination);
        }
        NavigationEffect::Frontier { goal, snapshot } => {
            bot.force_retire_pathfinding();
            freeze_observed_source(inner, &snapshot);
            bot.start_goto_with_opts(
                goal,
                PathfinderOpts::new()
                    .allow_mining(false)
                    .retry_on_no_path(false)
                    .recalculate_partial_paths(false),
            );
        }
    }
}

/// 移动 job 每 tick 轮询：读寻路器三个状态位与自身方块位，
/// 交给纯判定表（`movement_poll_step`），只在这里落副作用。
pub(super) fn poll_movement_job(inner: &Inner, bot: &Client) {
    let Some((destination, survey_view)) = inner
        .movement_job
        .peek(|job| (job.destination, scheduled_survey_direction(job)))
    else {
        return;
    };

    // 命令队列先于本轮移动轮询执行；外部 face/look_at 可能刚覆盖上一 tick 安排的
    // 朝向。扫描前重放当前 view 的精确方向，确保十二眼没有被错认或漏掉。
    let survey_scan = survey_view.map(|(yaw, pitch)| {
        bot.set_direction(yaw, pitch);
        observed::scan_current_view(inner, bot)
    });
    let knowledge = observed_snapshot(inner);
    let destination_rejected = knowledge.as_ref().is_some_and(|snapshot| {
        observed::stance_is_rejected(snapshot.memory(), as_block_pos(destination))
    });

    let Ok((pathfinder, executing, leg_end, block_pos)) =
        bot.try_query_self::<(Option<&Pathfinder>, Option<&ExecutingPath>, &Position), _>(
            |(pathfinder, executing, position)| {
                let block_pos = player_pos_to_block_pos(**position);
                (
                    pathfinder.map(|p| {
                        (
                            p.queued_goto_id.is_some(),
                            p.goal.is_some() || p.opts.is_some() || p.is_calculating,
                        )
                    }),
                    executing.is_some(),
                    executing.and_then(|e| {
                        e.path.back().map(|edge| {
                            let target = edge.movement.target;
                            [target.x, target.y, target.z]
                        })
                    }),
                    [block_pos.x, block_pos.y, block_pos.z],
                )
            },
        )
    else {
        return;
    };
    let (queued, pathfinder_active) = pathfinder.unwrap_or((false, false));
    let current = as_block_pos(block_pos);
    // A* 只读冻结快照；一旦出现 ExecutingPath，当前腿的障碍检查和局部 patch 每 tick
    // 直接切到本 tick 同一份最新观察快照，不再另存一位“已经 handoff”的重复状态。
    if executing {
        if let Some(snapshot) = knowledge.as_ref() {
            refresh_observed_execution_source(inner, snapshot);
        }
    }

    let mut effect = None;
    let landed = inner.movement_job.poll(inner, |job| {
        let now_tick = inner.now_tick();
        job.observe_block_pos(block_pos, now_tick);
        let body_inactive_ticks = now_tick.saturating_sub(job.last_body_progress_tick);

        if let Some(event) =
            navigation_terminal(
                job,
                current,
                destination_rejected,
                knowledge.is_some(),
            )
        {
            if matches!(event, MoveEvent::NavigationLimitReached { .. }) {
                println!(
                    "[导航] 本次工作达到上限：位置 {current:?}，已投递 {} 段、累计移动 {} 格；主动收束",
                    job.plans_started, job.travelled
                );
            }
            return Step::End(event);
        }

        // Survey 不读 Azalea 的 goal 位：进入该相之前已经同步 retirement。
        let survey = match &job.phase {
            NavigationPhase::Survey {
                at,
                base_yaw,
                view,
                reached_frontier,
            } => Some((*at, *base_yaw, *view, reached_frontier.clone())),
            NavigationPhase::Direct | NavigationPhase::Frontier { .. } => None,
        };
        if let Some((at, base_yaw, view, reached_frontier)) = survey {
            if body_inactive_ticks >= MOVEMENT_NO_BODY_PROGRESS_TICKS {
                return Step::End(MoveEvent::NoBodyProgressLimitReached {
                    at: block_pos,
                    ticks: body_inactive_ticks,
                });
            }
            match survey_scan.as_ref() {
                Some(Ok(_)) => {}
                Some(Err(reason)) => {
                    eprintln!("[导航] 战争迷雾观察失败：{reason}");
                    return Step::End(MoveEvent::PathEnded);
                }
                None => return Step::Keep,
            }
            let Some(snapshot) = knowledge.as_ref() else {
                return Step::End(MoveEvent::PathEnded);
            };

            // 环视期间若被水流、碰撞或实体推离原格，先前几眼不再属于同一个
            // viewpoint。丢弃旧 frontier 结论，从新姿态重新开始完整覆盖。
            if current != at {
                let (yaw, pitch) = job.begin_survey(current, None);
                effect = Some(NavigationEffect::SurveyView {
                    yaw,
                    pitch,
                    retire: true,
                });
                println!("[导航] 环视期间位置漂移：{at:?} -> {current:?}；重新环视");
                return Step::Keep;
            }

            if view + 1 < SURVEY_VIEWS.len() {
                let next_view = view + 1;
                if let NavigationPhase::Survey { view, .. } = &mut job.phase {
                    *view = next_view;
                }
                let (yaw, pitch) = survey_direction(base_yaw, next_view);
                effect = Some(NavigationEffect::SurveyView {
                    yaw,
                    pitch,
                    retire: false,
                });
                return Step::Keep;
            }

            if let Some(key) = reached_frontier {
                job.exhausted_frontiers.insert(key);
            }
            let key_after = snapshot.key();
            println!("[导航] 环视完成：位置 {current:?}，起始 {at:?}");
            match job.next_plan(current, key_after) {
                NextPlan::Direct => {
                    job.begin_direct(current, key_after, inner.now_tick());
                    effect = Some(NavigationEffect::Direct(snapshot.clone()));
                    println!("[导航] 新证据或新起点：重试最终目标 {:?}", job.destination);
                    return Step::Keep;
                }
                NextPlan::Frontier => {
                    let goal = job.begin_frontier(current, snapshot.clone(), inner.now_tick());
                    effect = Some(NavigationEffect::Frontier {
                        goal,
                        snapshot: snapshot.clone(),
                    });
                    println!("[导航] 最终目标暂无路：改走观察知识边界");
                    return Step::Keep;
                }
                // Azalea 的 `is_partial` 同时表示“图已穷尽”与“搜索超时”，这里没有
                // 足够证据宣称探索穷尽。两种计划在同一位置/知识版本都停下后只报
                // 泛化的 PathEnded；这会终止任务、防忙重试，但不把未知说成不可达。
                NextPlan::Stop => return Step::End(MoveEvent::PathEnded),
            }
        }

        let step = movement_poll_step(MovementPoll {
            at: block_pos,
            active: queued || pathfinder_active || executing,
            queued,
            executing,
            dispatch_ticks: now_tick.saturating_sub(job.leg_issued_tick),
            body_inactive_ticks,
            stall_notified: job.stall_notified,
        });

        if let MovementPollStep::End(MoveEvent::PathEnded) = step {
            if knowledge.is_none() {
                // 没安装观察地图时保持普通 Azalea 语义，不擅自启动探索。
                return Step::End(MoveEvent::PathEnded);
            }
            let (kind, reached_frontier) = match &job.phase {
                NavigationPhase::Direct => ("direct", None),
                NavigationPhase::Frontier {
                    started_at,
                    goal,
                } => (
                    "frontier",
                    (current != *started_at)
                        .then(|| goal.key_at(current))
                        .flatten(),
                ),
                NavigationPhase::Survey { .. } => unreachable!("survey 已在上面处理"),
            };
            let (yaw, pitch) = job.begin_survey(current, reached_frontier);
            effect = Some(NavigationEffect::SurveyView {
                yaw,
                pitch,
                retire: true,
            });
            println!("[导航] {kind} 路段结束在 {current:?}；开始主动环视");
            return Step::Keep;
        }
        if let MovementPollStep::End(event) = step {
            return Step::End(event);
        }

        // 这一程的终点由寻路器算完才知道，所以在它出现（或被 patch 改动）时才说。
        // 说的是意图不是承诺：走不走得到，终局判定照常优先。
        if let Some(leg_end) = leg_end {
            if job.announced_leg_end != Some(leg_end) {
                job.announced_leg_end = Some(leg_end);
                return Step::Progress(MoveEvent::Leg { to: leg_end });
            }
        }

        if step == MovementPollStep::Stall {
            job.stall_notified = true;
            return Step::Progress(MoveEvent::Stalled);
        }

        Step::Keep
    });

    if landed.is_some_and(|event| event.is_terminal()) {
        // 所有终局都走同一个同步 retirement；不能只处理 PathEnded，也不能依赖
        // 实体此刻恰好有 ExecutingPath。
        bot.force_retire_pathfinding();
    } else if let Some(effect) = effect {
        apply_navigation_effect(inner, bot, destination, effect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn knowledge(revision: u64) -> PlanningKey {
        PlanningKey::for_test(revision)
    }

    fn job_at(destination: [i32; 3], current: BlockPos, revision: u64) -> MovementJob {
        MovementJob::new(destination, 0, current, Some(knowledge(revision)))
    }

    #[test]
    fn movement_poll_step_covers_the_observable_states() {
        use MovementPollStep as Step;
        let poll = MovementPoll::default;
        // fork 的 queued 状态是稳定接单事实；没有 queued/计算/执行即路段已结束。
        assert_eq!(movement_poll_step(poll()), Step::End(MoveEvent::PathEnded));
        let queued = MovementPoll {
            active: true,
            queued: true,
            ..poll()
        };
        assert_eq!(movement_poll_step(queued), Step::Keep);
        assert_eq!(
            movement_poll_step(MovementPoll {
                dispatch_ticks: MOVEMENT_DISPATCH_TIMEOUT_TICKS,
                ..queued
            }),
            Step::End(MoveEvent::DispatchNotObserved {
                at: [0, 0, 0],
                ticks: MOVEMENT_DISPATCH_TIMEOUT_TICKS,
            })
        );

        // 计算中或执行中都还是活请求；只要身体期限未到就继续。
        assert_eq!(
            movement_poll_step(MovementPoll {
                active: true,
                ..poll()
            }),
            Step::Keep
        );
        assert_eq!(
            movement_poll_step(MovementPoll {
                at: [3, 64, 4],
                active: true,
                body_inactive_ticks: MOVEMENT_NO_BODY_PROGRESS_TICKS,
                ..poll()
            }),
            Step::End(MoveEvent::NoBodyProgressLimitReached {
                at: [3, 64, 4],
                ticks: MOVEMENT_NO_BODY_PROGRESS_TICKS,
            })
        );

        // 执行中：正常走→保持；久无推进→通知一次，此后沉默。
        let walking = MovementPoll {
            active: true,
            executing: true,
            ..poll()
        };
        assert_eq!(movement_poll_step(walking), Step::Keep);
        assert_eq!(
            movement_poll_step(MovementPoll {
                body_inactive_ticks: MOVEMENT_STALL_TICKS,
                ..walking
            }),
            Step::Stall
        );
        assert_eq!(
            movement_poll_step(MovementPoll {
                body_inactive_ticks: MOVEMENT_STALL_TICKS,
                stall_notified: true,
                ..walking
            }),
            Step::Keep
        );
    }

    #[test]
    fn only_an_actual_block_position_change_counts_as_progress() {
        let mut job = job_at([10, 64, 0], BlockPos::new(0, 64, 0), 0);
        assert!(!job.observe_block_pos([0, 64, 0], 7), "同一格不是推进");
        assert_eq!(job.last_body_progress_tick, 0);
        assert!(
            job.observe_block_pos([1, 64, 0], 8),
            "身体换格必须刷新物理进展"
        );
        assert_eq!(job.last_body_progress_tick, 8);
        assert!(!job.observe_block_pos([1, 64, 0], 9));
        assert_eq!(job.last_body_progress_tick, 8);
    }

    #[test]
    fn the_same_position_and_map_are_never_planned_twice() {
        let at = BlockPos::new(0, 64, 0);
        let mut job = job_at([10, 64, 0], at, 7);
        assert_eq!(job.next_plan(at, knowledge(7)), NextPlan::Frontier);

        job.frontier_attempts.insert(PlanKey {
            at,
            knowledge_key: knowledge(7),
        });
        assert_eq!(job.next_plan(at, knowledge(7)), NextPlan::Stop);
        assert_eq!(
            job.next_plan(BlockPos::new(1, 64, 0), knowledge(7)),
            NextPlan::Direct,
            "真实换位会产生新的规划 key"
        );
        assert_eq!(
            job.next_plan(at, knowledge(8)),
            NextPlan::Direct,
            "新观察版本也会产生新的规划 key"
        );
    }

    #[test]
    fn changing_world_cannot_replan_forever_without_body_progress() {
        let at = BlockPos::new(0, 64, 0);
        let mut job = job_at([10, 64, 0], at, 0);
        for revision in 1_u64..1_000 {
            if job.navigation_limit_event(at).is_some() {
                break;
            }
            job.begin_direct(at, knowledge(revision), revision);
        }
        assert!(matches!(
            job.navigation_limit_event(at),
            Some(MoveEvent::NavigationLimitReached { .. })
        ));
        assert_eq!(job.travelled, 0);
    }

    #[test]
    fn alternating_positions_and_new_maps_still_consume_a_finite_budget() {
        let a = BlockPos::new(0, 64, 0);
        let b = BlockPos::new(0, 64, 1);
        let mut job = job_at([1, 64, 0], a, 0);
        assert!(!job.observe_block_pos([a.x, a.y, a.z], 0));

        for revision in 1_u64..1_000 {
            let at = if revision % 2 == 0 { a } else { b };
            job.observe_block_pos([at.x, at.y, at.z], revision);
            if job.navigation_limit_event(at).is_some() {
                break;
            }
            job.begin_direct(at, knowledge(revision), revision);
        }

        let event = job
            .navigation_limit_event(as_block_pos(job.last_block_pos))
            .expect("A↔B 换格与新地图都不能永久刷新任务");
        assert!(matches!(event, MoveEvent::NavigationLimitReached { .. }));
        assert!(job.plans_started > 1);
        assert!(job.travelled > 1);
        assert_eq!(job.navigation_effort_limit, MIN_NAVIGATION_EFFORT);
    }

    #[test]
    fn effort_budget_is_overflow_safe_and_arrival_remains_the_first_terminal_check() {
        assert_eq!(
            navigation_effort_limit(
                BlockPos::new(i32::MIN, i32::MIN, i32::MIN),
                BlockPos::new(i32::MAX, i32::MAX, i32::MAX)
            ),
            MAX_NAVIGATION_EFFORT,
            "极端坐标也必须受实际可承受的硬上限约束"
        );

        let destination = [4, 64, 0];
        let current = as_block_pos(destination);
        let mut job = job_at(destination, current, 0);
        job.navigation_effort_limit = 1;
        job.plans_started = 1;
        assert!(job.navigation_limit_event(current).is_some());
        assert_eq!(
            navigation_terminal(&job, current, true, true),
            Some(MoveEvent::Arrived),
            "最后一单位恰好精确到达时，成功必须优先于占用与预算终局"
        );

        let before = BlockPos::new(3, 64, 0);
        assert_eq!(
            navigation_terminal(&job, before, true, true),
            Some(MoveEvent::DestinationRejected { at: [3, 64, 0] }),
            "已观察到目标节点被规则拒绝，是比工作预算更具体的终局"
        );
        assert!(matches!(
            navigation_terminal(&job, before, false, true),
            Some(MoveEvent::NavigationLimitReached { .. })
        ));
        assert_eq!(
            navigation_terminal(&job, before, false, false),
            None,
            "未启用观察图时不把普通寻路强套进战争迷雾预算"
        );
    }

    #[test]
    fn survey_covers_every_quadrant_at_ground_and_ceiling_pitch() {
        let directions: Vec<(i32, i32)> = (0..SURVEY_VIEWS.len())
            .map(|view| {
                let (yaw, pitch) = survey_direction(15.0, view);
                (yaw.round() as i32, pitch.round() as i32)
            })
            .collect();
        assert_eq!(directions.len(), 12);
        assert_eq!(
            directions.iter().filter(|(_, pitch)| *pitch == 0).count(),
            4
        );
        assert_eq!(directions.iter().filter(|(_, pitch)| *pitch > 0).count(), 4);
        assert_eq!(directions.iter().filter(|(_, pitch)| *pitch < 0).count(), 4);
        assert!(directions.contains(&(15, 0)), "第一眼必须朝最终目标方向");
        assert!(directions.contains(&(195, -55)), "背后与头顶也不能饿死");
    }

    #[test]
    fn survey_phase_keeps_the_exact_direction_that_must_be_reapplied_before_scan() {
        let at = BlockPos::new(0, 64, 0);
        let mut job = job_at([10, 64, 0], at, 0);
        let first = job.begin_survey(at, None);
        assert_eq!(scheduled_survey_direction(&job), Some(first));

        if let NavigationPhase::Survey { view, .. } = &mut job.phase {
            *view = 9;
        }
        assert_eq!(
            scheduled_survey_direction(&job),
            Some(survey_direction(
                yaw_towards(at, as_block_pos(job.destination)),
                9
            ))
        );
    }

    #[test]
    fn survey_yaw_is_finite_across_the_full_coordinate_range() {
        let yaw = yaw_towards(
            BlockPos::new(i32::MIN, 0, i32::MIN),
            BlockPos::new(i32::MAX, 0, i32::MAX),
        );
        assert!(yaw.is_finite());
        assert_eq!(yaw, -45.0);
    }
}
