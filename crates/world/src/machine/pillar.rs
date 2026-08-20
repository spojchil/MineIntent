//! 垫柱 job：跳起来，在脚下那一格放方块，站上去，重复。
//!
//! **临时工具**——放置这条线整体要重做（受理≠放上了、寻路里的放置、什么时候
//! 该放）。这里先把「原版玩家最基本的一个动作」交到模型手上，因为没有它，同伴
//! 挖竖井下去就爬不回来（2026-08-18 长跑实录）。
//!
//! # 为什么模型自己做不到
//!
//! 派发是**串行**的（`Dispatcher::dispatch` 逐条 await），两条工具调用之间只隔
//! 一次 await——微秒级。而跳跃要下一 tick 才起效，起跳瞬间人还占着脚下那格，
//! 服务端会拒绝放置。实测（`pillar_probe`，2026-08-19）：
//!
//! | jump→place 间隔 | 结果 |
//! | --- | --- |
//! | 0ms（同批相邻） | ✗ 发出时还在地上 |
//! | 50ms | ✗ 已离地但没升过那格 |
//! | **100~300ms** | **✓ 连垫 5 格** |
//! | 400ms 以上 | ✗ 已在下落 |
//!
//! 工具批里表达不了「隔 150ms」，分两轮发又慢到窗口早过。**所以时序归机器。**
//!
//! # 判据不是计时器
//!
//! 探针量出的 100~300ms 只是这台机器上的表象。真正的条件是**人升过了那一格**
//! ——脚底高过目标格顶面，服务端才不会判成「方块与玩家重叠」。所以这里等的是
//! `position.y >= target.y + 1`，与帧率、延迟、跳跃高度全都无关。

use std::sync::atomic::Ordering;

use azalea::entity::Position;
use azalea::Client;

use super::state::Inner;
use super::PILLAR_STALL_TICKS;
use crate::{JobKind, JobOutcome, JobProgress};

/// 在途的垫柱任务（单意图槽，与移动/挖掘同款）。
pub(super) struct PillarJob {
    /// 还要垫几格。
    pub(super) remaining: usize,
    /// 一共要垫几格（措辞用）。
    pub(super) total: usize,
    /// 这一格的目标——起跳时脚下那一格。
    pub(super) target: [i32; 3],
    /// 这一格从哪个 tick 开始。
    pub(super) since_tick: u64,
    /// 这一格的放置指令发出去了没有。
    pub(super) placed: bool,
}

/// 轮询结论（纯函数，可单测）。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PillarPollStep {
    /// 还没升够高：接着等。
    Rising,
    /// 升过那一格了：现在放。
    Place,
    /// 放了但还没落定：等方块出现。
    Settling,
    /// 这一格成了：站上去了。
    Stepped,
    /// 整串垫完。
    Finished,
    /// 卡住：迟迟没升上去，或者放了不出现。
    Blocked,
}

/// 纯判定。
///
/// `feet_y`：人脚底的 y（浮点，不取整——差半格就是差半格）。
/// `target_filled`：目标格现在有方块了没有。
/// `placed`：放置指令发出去了没有。
/// `elapsed`：这一格等了多少 tick。
/// `remaining_after_this`：这一格成了之后还剩几格。
pub(super) fn pillar_poll_step(
    feet_y: f64,
    target_y: i32,
    target_filled: bool,
    placed: bool,
    elapsed: u64,
    remaining_after_this: usize,
) -> PillarPollStep {
    if target_filled {
        // 方块出现了，而且人已经站到它上面——这一格才算成。
        if feet_y >= f64::from(target_y) + 1.0 {
            return if remaining_after_this == 0 {
                PillarPollStep::Finished
            } else {
                PillarPollStep::Stepped
            };
        }
        return PillarPollStep::Settling;
    }
    if elapsed >= PILLAR_STALL_TICKS {
        return PillarPollStep::Blocked;
    }
    if placed {
        return PillarPollStep::Settling;
    }
    // 关键判据：脚底高过目标格顶面，人才不再占着那一格。
    if feet_y >= f64::from(target_y) + 1.0 {
        PillarPollStep::Place
    } else {
        PillarPollStep::Rising
    }
}

/// 每 tick 轮询在途垫柱任务，落副作用。
pub(super) fn poll_pillar_job(inner: &Inner, bot: &Client) {
    let tick = inner.tick.load(Ordering::Acquire);
    let (target, placed, elapsed, remaining_after_this, total) = {
        let job = inner.pillar_job.lock();
        let Some(job) = job.as_ref() else { return };
        (
            job.target,
            job.placed,
            tick.saturating_sub(job.since_tick),
            job.remaining.saturating_sub(1),
            job.total,
        )
    };

    let Ok(feet_y) = bot.try_query_self::<&Position, _>(|position| position.y) else {
        return;
    };
    // 读不到（未加载等）当作还没出现，交给时限判卡住。
    let target_filled = !super::door::block_is_air(inner, target).unwrap_or(true);

    match pillar_poll_step(
        feet_y,
        target[1],
        target_filled,
        placed,
        elapsed,
        remaining_after_this,
    ) {
        PillarPollStep::Rising | PillarPollStep::Settling => {}
        PillarPollStep::Place => {
            if let Some(job) = inner.pillar_job.lock().as_mut() {
                job.placed = true;
            }
            // 放置本身走既有的门层判定（手里有没有东西、有没有依附面）。
            if let Err(reason) = super::door::place_block(inner, bot, target) {
                let _ = reason;
                inner.end_pillar_job(JobOutcome::PillarBlocked);
            }
        }
        PillarPollStep::Stepped => {
            let done = total - remaining_after_this;
            inner.push_job_progress(
                JobKind::PillarUp { total, done },
                JobProgress::Pillared { done, total },
            );
            // 站到新方块上了：下一格的目标就是现在的脚下。
            let mut slot = inner.pillar_job.lock();
            if let Some(job) = slot.as_mut() {
                job.remaining -= 1;
                job.target = [job.target[0], job.target[1] + 1, job.target[2]];
                job.since_tick = tick;
                job.placed = false;
            }
            drop(slot);
            bot.set_jumping(true);
            inner.jump_reset.store(true, Ordering::Release);
        }
        PillarPollStep::Finished => inner.end_pillar_job(JobOutcome::Pillared),
        PillarPollStep::Blocked => inner.end_pillar_job(JobOutcome::PillarBlocked),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_until_the_feet_clear_the_target() {
        // 脚底还在目标格里：不能放，服务端会判方块与玩家重叠。
        assert_eq!(
            pillar_poll_step(64.0, 64, false, false, 3, 0),
            PillarPollStep::Rising
        );
        // 差一点点也不行——判据是几何，不是「差不多跳起来了」。
        assert_eq!(
            pillar_poll_step(64.9, 64, false, false, 3, 0),
            PillarPollStep::Rising
        );
        assert_eq!(
            pillar_poll_step(65.0, 64, false, false, 3, 0),
            PillarPollStep::Place
        );
    }

    #[test]
    fn placed_but_not_appeared_yet_is_not_failure() {
        assert_eq!(
            pillar_poll_step(65.2, 64, false, true, 3, 0),
            PillarPollStep::Settling
        );
    }

    /// 方块出现了但人还没落到它上面，也还不算成——落回去才算站住。
    #[test]
    fn a_block_that_appeared_still_needs_the_player_on_top() {
        assert_eq!(
            pillar_poll_step(64.5, 64, true, true, 4, 0),
            PillarPollStep::Settling
        );
        assert_eq!(
            pillar_poll_step(65.0, 64, true, true, 5, 0),
            PillarPollStep::Finished
        );
    }

    #[test]
    fn more_to_go_steps_instead_of_finishing() {
        assert_eq!(
            pillar_poll_step(65.0, 64, true, true, 5, 2),
            PillarPollStep::Stepped
        );
    }

    /// 迟迟不成就如实报卡住，不无限等。
    #[test]
    fn giving_up_is_reported_not_hidden() {
        assert_eq!(
            pillar_poll_step(64.0, 64, false, false, PILLAR_STALL_TICKS, 0),
            PillarPollStep::Blocked
        );
    }
}
