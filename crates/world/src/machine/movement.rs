//! 移动 job 的终局判定与每 tick 轮询。
//!
//! 判定本身是纯函数（[`movement_poll_step`]）：把寻路器三个状态位、自身
//! 方块位与两个时限压成一个结论，可以脱离 azalea 穷举单测。副作用只在
//! [`poll_movement_job`] 里落——出窗、清槽、停寻路。
//!
//! 判据全部来自 azalea 的可观察状态，不是猜的：
//! 到达 = goal 被清空（`pathfinder::execute` 的目标达成分支）；
//! 走完未达 = goal 留 `Some` 而 `ExecutingPath` 移除（同处 else 分支）；
//! 卡住 = `ticks_since_last_node_reached` 超阈值——只通知不取消，
//! 因为 azalea 自己会补路自救。

use std::sync::atomic::Ordering;

use azalea::entity::Position;
use azalea::pathfinder::{ExecutingPath, Pathfinder, PathfinderClientExt};
use azalea::Client;

use super::state::Inner;
use super::{MOVEMENT_ARM_GRACE_TICKS, MOVEMENT_STALL_TICKS};
use crate::JobOutcome;

/// 在途的移动任务（单意图槽）。终局判定按 azalea 寻路器的可观察状态：
/// 成功到达时它把 goal 置 None（execute/mod.rs 目标达成分支）；走完未达时
/// ExecutingPath 移除但 goal 留 Some。我们自己下的停止/顶替在命令处就地标注。
pub(super) struct MovementJob {
    pub(super) destination: [i32; 3],
    pub(super) started_tick: u64,
    /// 见过寻路器活动（计算中/执行中/goal 已挂）后才允许判终局，
    /// 避开 GotoEvent 尚未被调度的起步窗口。
    pub(super) armed: bool,
    /// 卡住通知只发一次。
    pub(super) stall_notified: bool,
}

/// 移动 job 轮询的行动结论（纯函数，可单测）。
#[derive(Debug, PartialEq, Eq)]
enum MovementPollStep {
    /// 保持现状（含宽限期内等待）。
    Keep,
    /// 寻路器已可见，进入武装状态。
    Arm,
    /// 任务终局：出窗并清槽。
    End(JobOutcome),
    /// 卡住通知（任务继续）。
    Stall,
}

/// 判定表输入：本 tick 的全部可观察事实。
/// `goal_some`/`calculating`/`executing` 是寻路器三个状态位；
/// `at_destination` = 自身所在方块 == 目的地（站在目的地时空路径也算到达）；
/// `grace_exceeded` = 起步宽限已过；`stalled_long` = 无推进 tick 数超阈值。
#[derive(Clone, Copy, Debug, Default)]
struct MovementPoll {
    armed: bool,
    goal_some: bool,
    calculating: bool,
    executing: bool,
    at_destination: bool,
    grace_exceeded: bool,
    stalled_long: bool,
    stall_notified: bool,
}

fn movement_poll_step(poll: MovementPoll) -> MovementPollStep {
    if !poll.armed {
        if poll.goal_some || poll.calculating || poll.executing {
            return MovementPollStep::Arm;
        }
        if poll.grace_exceeded {
            // 出发都没出发（消息丢失等罕见情形）——按走完未达收束，不装作还在走。
            return MovementPollStep::End(JobOutcome::PathEnded);
        }
        return MovementPollStep::Keep;
    }
    if !poll.calculating && !poll.executing {
        if !poll.goal_some {
            // 只有寻路器的目标达成分支会在无外停时清 goal。
            return MovementPollStep::End(JobOutcome::Arrived);
        }
        // goal 还挂着但执行已停：走完未达（不可达/局部路径尽头）。
        // 站在目的地上的空路径情形按到达算——goal.success 的判据就是方块相等。
        return MovementPollStep::End(if poll.at_destination {
            JobOutcome::Arrived
        } else {
            JobOutcome::PathEnded
        });
    }
    if poll.executing && poll.stalled_long && !poll.stall_notified {
        return MovementPollStep::Stall;
    }
    MovementPollStep::Keep
}

/// 移动 job 每 tick 轮询：读寻路器三个状态位与自身方块位，
/// 交给纯判定表（`movement_poll_step`），只在这里落副作用。
pub(super) fn poll_movement_job(inner: &Inner, bot: &Client) {
    let mut slot = inner.movement_job.lock();
    let Some(job) = slot.as_mut() else { return };

    let Ok((pathfinder, stall_ticks, block_pos)) =
        bot.try_query_self::<(Option<&Pathfinder>, Option<&ExecutingPath>, &Position), _>(
            |(pathfinder, executing, position)| {
                (
                    pathfinder.map(|p| (p.goal.is_some(), p.is_calculating)),
                    executing.map(|e| e.ticks_since_last_node_reached),
                    [
                        position.x.floor() as i32,
                        position.y.floor() as i32,
                        position.z.floor() as i32,
                    ],
                )
            },
        )
    else {
        return;
    };
    let (goal_some, calculating) = pathfinder.unwrap_or((false, false));
    let step = movement_poll_step(MovementPoll {
        armed: job.armed,
        goal_some,
        calculating,
        executing: stall_ticks.is_some(),
        at_destination: block_pos == job.destination,
        grace_exceeded: inner
            .tick
            .load(Ordering::Acquire)
            .saturating_sub(job.started_tick)
            > MOVEMENT_ARM_GRACE_TICKS,
        stalled_long: stall_ticks.is_some_and(|ticks| ticks > MOVEMENT_STALL_TICKS),
        stall_notified: job.stall_notified,
    });
    match step {
        MovementPollStep::Keep => {}
        MovementPollStep::Arm => job.armed = true,
        MovementPollStep::Stall => {
            job.stall_notified = true;
            let destination = job.destination;
            drop(slot);
            inner.push_job(destination, JobOutcome::Stalled);
        }
        MovementPollStep::End(outcome) => {
            let job = slot.take().expect("上面刚借到 Some");
            drop(slot);
            if outcome == JobOutcome::PathEnded {
                // goal 还挂在寻路器上（走完未达不清 goal）；清掉僵尸目标，
                // 避免下次判定被旧 goal 干扰。
                bot.stop_pathfinding();
            }
            inner.push_job(job.destination, outcome);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn movement_poll_step_covers_the_observable_states() {
        use MovementPollStep as Step;
        let poll = MovementPoll::default;
        // 起步宽限内：寻路器不可见→等待；可见→武装。
        assert_eq!(movement_poll_step(poll()), Step::Keep);
        assert_eq!(
            movement_poll_step(MovementPoll {
                calculating: true,
                ..poll()
            }),
            Step::Arm
        );
        assert_eq!(
            movement_poll_step(MovementPoll {
                goal_some: true,
                ..poll()
            }),
            Step::Arm
        );
        // 宽限耗尽还没起步：按走完未达收束。
        assert_eq!(
            movement_poll_step(MovementPoll {
                grace_exceeded: true,
                ..poll()
            }),
            Step::End(JobOutcome::PathEnded)
        );
        // 武装后：goal 清空且不在算不在走 = 寻路器宣告到达。
        assert_eq!(
            movement_poll_step(MovementPoll {
                armed: true,
                ..poll()
            }),
            Step::End(JobOutcome::Arrived)
        );
        // goal 还挂着但停了：不在目的地=走完未达；在目的地=空路径到达。
        assert_eq!(
            movement_poll_step(MovementPoll {
                armed: true,
                goal_some: true,
                ..poll()
            }),
            Step::End(JobOutcome::PathEnded)
        );
        assert_eq!(
            movement_poll_step(MovementPoll {
                armed: true,
                goal_some: true,
                at_destination: true,
                ..poll()
            }),
            Step::End(JobOutcome::Arrived)
        );
        // 执行中：正常走→保持；久无推进→通知一次，此后沉默。
        let walking = MovementPoll {
            armed: true,
            goal_some: true,
            executing: true,
            ..poll()
        };
        assert_eq!(movement_poll_step(walking), Step::Keep);
        assert_eq!(
            movement_poll_step(MovementPoll {
                stalled_long: true,
                ..walking
            }),
            Step::Stall
        );
        assert_eq!(
            movement_poll_step(MovementPoll {
                stalled_long: true,
                stall_notified: true,
                ..walking
            }),
            Step::Keep
        );
        // 重算中（部分路径续算）不是终局。
        assert_eq!(
            movement_poll_step(MovementPoll {
                armed: true,
                goal_some: true,
                calculating: true,
                ..poll()
            }),
            Step::Keep
        );
    }
}
