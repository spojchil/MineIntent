//! 挖掘 job 的终局判定与每 tick 轮询。
//!
//! 与 [`super::movement`] 同构：判定是纯函数（[`mining_poll_step`]），可脱离
//! azalea 穷举单测；副作用只在 [`poll_mining_job`] 里落。
//!
//! **为什么是队列而不是单块。** azalea 的 `start_mining` 是单目标槽——换目标
//! 就是放弃上一个。旧接口一次只收一块，模型以为在排队，实测连发四次把四块
//! 全掐断了，一块没挖掉，还反复定向扫描去确认「怎么没挖动」。
//! 队列把「一次表达完整意图」还给模型，把「一块一块来」留给机器。
//!
//! **每种边界只解释自己的产生方。** 目标格变成空气才算挖碎；`MineProgress`
//! 严格增长才算挖掘进展。总工期不设上限；队列未消费、预测未确认和活跃挖掘
//! 无进展各有独立结论，互不冒充。

use azalea::{
    interact::BlockStatePredictionHandler,
    mining::{MineBlockPos, MineProgress, MineTicks, Mining, MiningQueued, StopMiningBlockEvent},
    BlockPos, Client,
};

use super::job::{JobVerb, Step};
use super::state::Inner;
use super::{
    MINING_DISPATCH_TIMEOUT_TICKS, MINING_NO_PROGRESS_TICKS, MINING_PREDICTION_SETTLE_TIMEOUT_TICKS,
};
use crate::{JobFact, JobId, JobStatus, JobStatusKind, MineEvent};

/// 在途的挖掘任务（单意图槽，与移动同款）。
pub(super) struct MiningJob {
    pub(super) targets: Vec<[i32; 3]>,
    /// 下一块要挖的下标；等于 `targets.len()` 表示全挖完了。
    pub(super) cursor: usize,
    attempt: MiningAttempt,
}

impl MiningJob {
    pub(super) fn new(targets: Vec<[i32; 3]>, now_tick: u64) -> Self {
        Self {
            targets,
            cursor: 0,
            attempt: MiningAttempt::new(now_tick),
        }
    }

    fn begin_next_target(&mut self, now_tick: u64) {
        self.attempt = MiningAttempt::new(now_tick);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MiningAttemptStage {
    /// 同目标请求已同步排队，尚未观察到匹配的 `Mining`。
    Queued,
    /// 已经观察到匹配的 `Mining`，此时 `MineProgress` 才具有停滞含义。
    Active,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct MiningAttempt {
    stage: MiningAttemptStage,
    queued_since_tick: u64,
    /// 当前目标上见过的最大 `MineProgress`。只认超过它的新值为净进展，避免补发
    /// 清零后拿同一段进度反复刷新期限。
    highest_observed_progress: Option<f32>,
    /// 上一次观察到 `MineProgress` 严格超过历史最大值的 tick。
    last_progress_tick: u64,
    /// 首次连续观察到目标仍受本地预测覆盖的 tick；预测一旦收敛就清空。
    prediction_pending_since_tick: Option<u64>,
    /// 活跃请求消失后是否已经用掉唯一一次补发机会。
    retry_used: bool,
}

impl MiningAttempt {
    fn new(now_tick: u64) -> Self {
        Self {
            stage: MiningAttemptStage::Queued,
            queued_since_tick: now_tick,
            highest_observed_progress: None,
            last_progress_tick: now_tick,
            prediction_pending_since_tick: None,
            retry_used: false,
        }
    }

    fn observe_active_progress(&mut self, progress: f32, now_tick: u64) {
        self.stage = MiningAttemptStage::Active;
        if self
            .highest_observed_progress
            .is_none_or(|highest| progress > highest)
        {
            self.highest_observed_progress = Some(progress);
            self.last_progress_tick = now_tick;
        }
    }

    fn observe_prediction_pending(&mut self, now_tick: u64) -> u64 {
        let first_tick = *self.prediction_pending_since_tick.get_or_insert(now_tick);
        now_tick.saturating_sub(first_tick)
    }
}

impl JobVerb for MiningJob {
    type Event = MineEvent;

    fn fact(&self, event: MineEvent) -> JobFact {
        JobFact::Mine {
            targets: self.targets.clone(),
            done: self.cursor,
            event,
        }
    }

    fn replaced() -> MineEvent {
        MineEvent::Replaced
    }
    fn cancelled() -> MineEvent {
        MineEvent::Cancelled
    }
    fn connection_ended() -> MineEvent {
        MineEvent::ConnectionEnded
    }

    fn status(&self, id: JobId, started_tick: u64, now_tick: u64) -> JobStatus {
        JobStatus {
            id,
            kind: JobStatusKind::Mine {
                targets: self.targets.clone(),
                done: self.cursor,
                current: self.targets.get(self.cursor).copied(),
            },
            started_tick,
            elapsed_ticks: now_tick.saturating_sub(started_tick),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TargetObservation {
    /// 当前格是空气，且没有尚待服务端确认的本地预测。
    Air,
    Solid,
    /// 本地世界值仍受方块预测覆盖，服务端可能确认也可能回滚。
    PredictionPending,
    /// 当前世界模型无法读取目标（例如区块未加载或坐标在世界高度外）。
    Unavailable,
}

/// 同一次 ECS 读锁里取得的 Azalea 挖掘请求状态。
#[derive(Clone, Copy, Debug, PartialEq)]
enum MiningRequestState {
    Queued,
    Active {
        progress: f32,
    },
    /// 同步接受过的目标既不在队列里，也不是当前活跃目标，或其组件彼此矛盾。
    Interrupted,
}

/// 轮询的行动结论（纯函数，可单测）。
#[derive(Debug, PartialEq, Eq)]
enum MiningPollStep {
    /// 当前这块还没碎，继续观察同一次请求。
    Keep,
    /// 匹配的活跃请求确实消失，使用唯一一次补发机会。
    Reissue,
    /// 当前这块碎了，推进到下一块。
    Advance,
    /// 整队挖完。
    Finished,
    /// 目标可读且仍为实心，但 `MineProgress` 连续没有增长。
    Blocked,
    /// 同步 queued 一直没有被 Azalea listener 消费。
    DispatchNotObserved { ticks: u64 },
    /// queued 曾经存在，随后在进入匹配 Active 前结束；Azalea 没有暴露更细原因。
    RequestEnded,
    /// 本地预测在协议边界内始终没有得到服务端确认或回滚。
    PredictionNotSettled { ticks: u64 },
    /// 目标当前读不到；不能把未知默认成“仍为实心”再等计时器猜。
    TargetUnavailable,
}

/// 纯判定：成功来自世界状态，停滞来自 Azalea 自己的进度产生方。
fn mining_poll_step(
    target: TargetObservation,
    request: MiningRequestState,
    attempt: &mut MiningAttempt,
    now_tick: u64,
    remaining_after_this: usize,
) -> MiningPollStep {
    match target {
        TargetObservation::Air => {
            return if remaining_after_this == 0 {
                MiningPollStep::Finished
            } else {
                MiningPollStep::Advance
            };
        }
        TargetObservation::Unavailable => return MiningPollStep::TargetUnavailable,
        // 本地预测成空气不是 W03 所要求的实际结果；等待服务端 ack/update 收敛，
        // 期间也不能重发同一次挖掘或拿无进展窗口冒充协议结论。
        TargetObservation::PredictionPending => {
            let ticks = attempt.observe_prediction_pending(now_tick);
            return if ticks >= MINING_PREDICTION_SETTLE_TIMEOUT_TICKS {
                MiningPollStep::PredictionNotSettled { ticks }
            } else {
                MiningPollStep::Keep
            };
        }
        TargetObservation::Solid => {}
    }
    attempt.prediction_pending_since_tick = None;

    match request {
        MiningRequestState::Queued => {
            if attempt.stage != MiningAttemptStage::Queued {
                // A queue observed after Active is already the one allowed recovery attempt,
                // regardless of who inserted it. It must not reopen an unlimited retry cycle.
                attempt.stage = MiningAttemptStage::Queued;
                attempt.queued_since_tick = now_tick;
                attempt.retry_used = true;
            }
            let ticks = now_tick.saturating_sub(attempt.queued_since_tick);
            if ticks >= MINING_DISPATCH_TIMEOUT_TICKS {
                MiningPollStep::DispatchNotObserved { ticks }
            } else {
                MiningPollStep::Keep
            }
        }
        MiningRequestState::Active { progress } => {
            attempt.observe_active_progress(progress, now_tick);
            if now_tick.saturating_sub(attempt.last_progress_tick) >= MINING_NO_PROGRESS_TICKS {
                MiningPollStep::Blocked
            } else {
                MiningPollStep::Keep
            }
        }
        MiningRequestState::Interrupted => match attempt.stage {
            MiningAttemptStage::Queued => MiningPollStep::RequestEnded,
            MiningAttemptStage::Active if !attempt.retry_used => {
                attempt.retry_used = true;
                attempt.stage = MiningAttemptStage::Queued;
                attempt.queued_since_tick = now_tick;
                MiningPollStep::Reissue
            }
            MiningAttemptStage::Active => MiningPollStep::RequestEnded,
        },
    }
}

fn classify_request_state(
    target: BlockPos,
    queued_target: Option<BlockPos>,
    active_target: Option<BlockPos>,
    tracked_target: Option<BlockPos>,
    progress: Option<f32>,
    mine_ticks: Option<f32>,
) -> MiningRequestState {
    // A queued replacement wins over the currently active request: it is what Azalea will
    // process next. A wrong queued target therefore already is an interruption.
    if let Some(queued_target) = queued_target {
        return if queued_target == target {
            MiningRequestState::Queued
        } else {
            MiningRequestState::Interrupted
        };
    }

    if active_target == Some(target) && tracked_target == Some(target) {
        // mine_ticks 只作为「Mining 组件本身自洽」的证据读一次，不存进状态：
        // 推进与否的判据是 progress 是否严格增长，剩余工期读了也不参与任何决定。
        // 存下来会让人以为窗口是按它算的，而窗口其实是按 tick 算的。
        if let (Some(progress), Some(mine_ticks)) = (progress, mine_ticks) {
            if progress.is_finite() && mine_ticks.is_finite() && mine_ticks >= 0.0 {
                return MiningRequestState::Active { progress };
            }
        }
    }

    MiningRequestState::Interrupted
}

fn observe_runtime_state(bot: &Client, target: BlockPos) -> (MiningRequestState, Option<bool>) {
    // One lock is deliberate: mixing independently timed component reads can fabricate a state
    // which never existed in Azalea (especially across queued -> active transition).
    let ecs = bot.ecs.read();
    let request = classify_request_state(
        target,
        ecs.get::<MiningQueued>(bot.entity)
            .map(|queued| queued.position),
        ecs.get::<Mining>(bot.entity).map(|mining| mining.pos),
        ecs.get::<MineBlockPos>(bot.entity)
            .and_then(|position| position.0),
        ecs.get::<MineProgress>(bot.entity)
            .map(|progress| progress.0),
        ecs.get::<MineTicks>(bot.entity).map(|ticks| ticks.0),
    );
    let prediction_pending = ecs
        .get::<BlockStatePredictionHandler>(bot.entity)
        .map(|handler| handler.is_prediction_pending(target));
    (request, prediction_pending)
}

/// 收掉 Azalea 的活跃/排队挖掘状态。终局不能只清 MineIntent 的 job、让身体继续挖。
fn retire_mining(bot: &Client) {
    let mut ecs = bot.ecs.write();
    let is_active = ecs.get::<Mining>(bot.entity).is_some();
    ecs.entity_mut(bot.entity).remove::<MiningQueued>();
    if is_active {
        ecs.write_message(StopMiningBlockEvent { entity: bot.entity });
    }
}

/// 每 tick 轮询在途挖掘任务，落副作用。
///
/// 收槽只经 [`Step::End`]——槽位不外露 `take()`，所以「必有终局」忘不掉。
pub(super) fn poll_mining_job(inner: &Inner, bot: &Client) {
    let tick = inner.now_tick();
    let Some((target, remaining_after_this)) = inner
        .mining_job
        .peek(|job| {
            job.targets
                .get(job.cursor)
                .copied()
                .map(|target| (target, job.targets.len() - job.cursor - 1))
        })
        .flatten()
    else {
        return;
    };

    let target_pos = BlockPos::new(target[0], target[1], target[2]);
    let (request, prediction_pending) = observe_runtime_state(bot, target_pos);
    let target_observation = match (super::door::block_is_air(inner, target), prediction_pending) {
        (Err(_), _) | (_, None) => TargetObservation::Unavailable,
        (Ok(_), Some(true)) => TargetObservation::PredictionPending,
        (Ok(true), Some(false)) => TargetObservation::Air,
        (Ok(false), Some(false)) => TargetObservation::Solid,
    };

    let mut reissue_at = None;
    let mut retire = false;
    inner.mining_job.poll(inner, |job| {
        match mining_poll_step(
            target_observation,
            request,
            &mut job.attempt,
            tick,
            remaining_after_this,
        ) {
            MiningPollStep::Keep => Step::Keep,
            MiningPollStep::Reissue => {
                reissue_at = Some(target);
                Step::Keep
            }
            MiningPollStep::Advance => {
                job.cursor += 1;
                job.begin_next_target(tick);
                reissue_at = job.targets.get(job.cursor).copied();
                // 一块碎了就说一句：整队挖完才是终局，中途不该沉默到底。
                Step::Progress(MineEvent::Broke {
                    done: job.cursor,
                    total: job.targets.len(),
                })
            }
            MiningPollStep::Finished => {
                retire = true;
                Step::End(MineEvent::Cleared)
            }
            MiningPollStep::Blocked => {
                retire = true;
                Step::End(MineEvent::Blocked { at: target })
            }
            MiningPollStep::DispatchNotObserved { ticks } => {
                retire = true;
                Step::End(MineEvent::DispatchNotObserved { at: target, ticks })
            }
            MiningPollStep::RequestEnded => {
                retire = true;
                Step::End(MineEvent::RequestEnded { at: target })
            }
            MiningPollStep::PredictionNotSettled { ticks } => {
                retire = true;
                Step::End(MineEvent::PredictionNotSettled { at: target, ticks })
            }
            MiningPollStep::TargetUnavailable => {
                retire = true;
                Step::End(MineEvent::TargetUnavailable { at: target })
            }
        }
    });

    if retire {
        retire_mining(bot);
    } else if let Some(next) = reissue_at {
        super::door::begin_mining(bot, next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active(progress: f32) -> MiningRequestState {
        MiningRequestState::Active { progress }
    }

    #[test]
    fn air_is_the_only_success_fact_and_drives_the_cursor() {
        let mut attempt = MiningAttempt::new(0);
        assert_eq!(
            mining_poll_step(TargetObservation::Air, active(0.0), &mut attempt, 999, 2,),
            MiningPollStep::Advance
        );
        assert_eq!(
            mining_poll_step(TargetObservation::Air, active(0.0), &mut attempt, 999, 0,),
            MiningPollStep::Finished
        );
    }

    #[test]
    fn pending_prediction_has_its_own_boundary_from_first_observation() {
        let mut attempt = MiningAttempt::new(0);
        attempt.observe_active_progress(0.9, 10);
        let first_pending_tick = MINING_NO_PROGRESS_TICKS * 2 + 10;
        assert_eq!(
            mining_poll_step(
                TargetObservation::PredictionPending,
                MiningRequestState::Interrupted,
                &mut attempt,
                first_pending_tick,
                0,
            ),
            MiningPollStep::Keep
        );
        assert_eq!(
            mining_poll_step(
                TargetObservation::PredictionPending,
                MiningRequestState::Interrupted,
                &mut attempt,
                first_pending_tick + MINING_PREDICTION_SETTLE_TIMEOUT_TICKS - 1,
                0,
            ),
            MiningPollStep::Keep
        );
        assert_eq!(
            mining_poll_step(
                TargetObservation::PredictionPending,
                MiningRequestState::Interrupted,
                &mut attempt,
                first_pending_tick + MINING_PREDICTION_SETTLE_TIMEOUT_TICKS,
                0,
            ),
            MiningPollStep::PredictionNotSettled {
                ticks: MINING_PREDICTION_SETTLE_TIMEOUT_TICKS,
            }
        );
    }

    #[test]
    fn predicted_air_only_succeeds_after_the_prediction_settles() {
        let mut attempt = MiningAttempt::new(0);
        assert_eq!(
            mining_poll_step(
                TargetObservation::PredictionPending,
                MiningRequestState::Interrupted,
                &mut attempt,
                20,
                0,
            ),
            MiningPollStep::Keep
        );
        assert_eq!(
            mining_poll_step(
                TargetObservation::Air,
                MiningRequestState::Interrupted,
                &mut attempt,
                21,
                0,
            ),
            MiningPollStep::Finished
        );
    }

    #[test]
    fn queued_has_a_dispatch_boundary_not_a_progress_boundary() {
        let mut attempt = MiningAttempt::new(0);
        assert_eq!(
            mining_poll_step(
                TargetObservation::Solid,
                MiningRequestState::Queued,
                &mut attempt,
                MINING_DISPATCH_TIMEOUT_TICKS - 1,
                1,
            ),
            MiningPollStep::Keep
        );
        assert_eq!(
            mining_poll_step(
                TargetObservation::Solid,
                MiningRequestState::Queued,
                &mut attempt,
                MINING_DISPATCH_TIMEOUT_TICKS,
                1,
            ),
            MiningPollStep::DispatchNotObserved {
                ticks: MINING_DISPATCH_TIMEOUT_TICKS,
            }
        );
    }

    #[test]
    fn a_queue_that_ends_before_active_is_terminal_not_reissued() {
        let mut attempt = MiningAttempt::new(0);
        assert_eq!(
            mining_poll_step(
                TargetObservation::Solid,
                MiningRequestState::Interrupted,
                &mut attempt,
                5,
                1,
            ),
            MiningPollStep::RequestEnded
        );
    }

    #[test]
    fn active_disappearance_gets_one_retry_and_never_a_cycle() {
        let mut attempt = MiningAttempt::new(0);
        assert_eq!(
            mining_poll_step(TargetObservation::Solid, active(0.25), &mut attempt, 5, 1,),
            MiningPollStep::Keep
        );
        assert_eq!(
            mining_poll_step(
                TargetObservation::Solid,
                MiningRequestState::Interrupted,
                &mut attempt,
                6,
                1,
            ),
            MiningPollStep::Reissue
        );
        assert!(attempt.retry_used);
        assert_eq!(attempt.stage, MiningAttemptStage::Queued);

        // The retry was synchronously queued but disappeared before matching Active.
        assert_eq!(
            mining_poll_step(
                TargetObservation::Solid,
                MiningRequestState::Interrupted,
                &mut attempt,
                7,
                1,
            ),
            MiningPollStep::RequestEnded
        );
    }

    #[test]
    fn a_retry_that_becomes_active_cannot_open_another_retry() {
        let mut attempt = MiningAttempt::new(0);
        assert_eq!(
            mining_poll_step(TargetObservation::Solid, active(0.2), &mut attempt, 4, 0,),
            MiningPollStep::Keep
        );
        assert_eq!(
            mining_poll_step(
                TargetObservation::Solid,
                MiningRequestState::Interrupted,
                &mut attempt,
                5,
                0,
            ),
            MiningPollStep::Reissue
        );
        assert_eq!(
            mining_poll_step(TargetObservation::Solid, active(0.3), &mut attempt, 6, 0,),
            MiningPollStep::Keep
        );
        assert_eq!(
            mining_poll_step(
                TargetObservation::Solid,
                MiningRequestState::Interrupted,
                &mut attempt,
                7,
                0,
            ),
            MiningPollStep::RequestEnded
        );
    }

    #[test]
    fn a_queue_observed_after_active_uses_the_retry_dispatch_boundary() {
        let mut attempt = MiningAttempt::new(0);
        assert_eq!(
            mining_poll_step(TargetObservation::Solid, active(0.2), &mut attempt, 4, 0,),
            MiningPollStep::Keep
        );
        assert_eq!(
            mining_poll_step(
                TargetObservation::Solid,
                MiningRequestState::Queued,
                &mut attempt,
                5,
                0,
            ),
            MiningPollStep::Keep
        );
        assert!(attempt.retry_used);
        assert_eq!(
            mining_poll_step(
                TargetObservation::Solid,
                MiningRequestState::Queued,
                &mut attempt,
                5 + MINING_DISPATCH_TIMEOUT_TICKS,
                0,
            ),
            MiningPollStep::DispatchNotObserved {
                ticks: MINING_DISPATCH_TIMEOUT_TICKS,
            }
        );
    }

    #[test]
    fn unreadable_target_is_not_fabricated_as_solid() {
        let mut attempt = MiningAttempt::new(0);
        assert_eq!(
            mining_poll_step(
                TargetObservation::Unavailable,
                MiningRequestState::Interrupted,
                &mut attempt,
                0,
                1,
            ),
            MiningPollStep::TargetUnavailable
        );
    }

    #[test]
    fn only_matching_active_uses_the_no_progress_window() {
        let mut attempt = MiningAttempt::new(0);
        assert_eq!(
            mining_poll_step(TargetObservation::Solid, active(0.0), &mut attempt, 0, 3,),
            MiningPollStep::Keep
        );
        assert_eq!(
            mining_poll_step(
                TargetObservation::Solid,
                active(0.0),
                &mut attempt,
                MINING_NO_PROGRESS_TICKS - 1,
                3,
            ),
            MiningPollStep::Keep
        );
        assert_eq!(
            mining_poll_step(
                TargetObservation::Solid,
                active(0.0),
                &mut attempt,
                MINING_NO_PROGRESS_TICKS,
                3,
            ),
            MiningPollStep::Blocked
        );
    }

    #[test]
    fn arbitrarily_slow_strict_progress_keeps_extending_the_window() {
        let mut attempt = MiningAttempt::new(0);
        for (tick, progress) in [(399, 0.000_001), (798, 0.000_002), (1_197, 0.000_003)] {
            assert_eq!(
                mining_poll_step(
                    TargetObservation::Solid,
                    active(progress),
                    &mut attempt,
                    tick,
                    1,
                ),
                MiningPollStep::Keep
            );
            assert_eq!(attempt.last_progress_tick, tick);
        }
    }

    #[test]
    fn reset_or_replayed_progress_does_not_disguise_stagnation() {
        let mut attempt = MiningAttempt::new(0);
        attempt.observe_active_progress(0.5, 10);
        attempt.observe_active_progress(0.0, 200);
        attempt.observe_active_progress(0.5, 300);

        assert_eq!(attempt.highest_observed_progress, Some(0.5));
        assert_eq!(attempt.last_progress_tick, 10);
    }

    #[test]
    fn request_state_checks_queue_active_and_tracked_targets_together() {
        let target = BlockPos::new(1, 64, 2);
        let other = BlockPos::new(2, 64, 2);
        assert_eq!(
            classify_request_state(target, Some(target), Some(other), Some(other), None, None),
            MiningRequestState::Queued
        );
        assert_eq!(
            classify_request_state(
                target,
                None,
                Some(target),
                Some(target),
                Some(0.25),
                Some(5.0),
            ),
            active(0.25)
        );
        assert_eq!(
            classify_request_state(
                target,
                Some(other),
                Some(target),
                Some(target),
                Some(0.25),
                Some(5.0),
            ),
            MiningRequestState::Interrupted
        );
        assert_eq!(
            classify_request_state(
                target,
                None,
                Some(target),
                Some(other),
                Some(0.25),
                Some(5.0),
            ),
            MiningRequestState::Interrupted
        );
    }
}
