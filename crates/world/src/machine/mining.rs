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
//! **判据来自可观察状态，不是计时器猜的**：目标格变成空气 = 这块挖碎了。
//! 徒手挖原木要好几秒，按时间猜必然错；按世界状态判则与工具、附魔、方块
//! 硬度全都无关。

use std::sync::atomic::Ordering;

use azalea::Client;

use super::state::Inner;
use super::MINING_STALL_TICKS;
use crate::JobOutcome;

/// 在途的挖掘任务（单意图槽，与移动同款）。
pub(super) struct MiningJob {
    pub(super) targets: Vec<[i32; 3]>,
    /// 下一块要挖的下标；等于 `targets.len()` 表示全挖完了。
    pub(super) cursor: usize,
    /// 当前这块从哪个 tick 开始挖的（判「迟迟不碎」用）。
    pub(super) since_tick: u64,
}

/// 轮询的行动结论（纯函数，可单测）。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum MiningPollStep {
    /// 当前这块还没碎，继续挖（`reissue` 为真时要重发 start_mining——
    /// 挖掘被外力打断后 Mining 组件会消失，自愈靠它）。
    Keep { reissue: bool },
    /// 当前这块碎了，推进到下一块。
    Advance,
    /// 整队挖完。
    Finished,
    /// 卡住：迟迟不碎。
    Blocked,
}

/// 纯判定：由「目标是否已空」「是否还在挖」「挖了多久」得出结论。
///
/// `target_is_air`：目标格现在是不是空气（碎了的唯一判据）。
/// `is_mining`：azalea 的 `Mining` 组件还在不在。
/// `elapsed`：当前这块挖了多少 tick。
pub(super) fn mining_poll_step(
    target_is_air: bool,
    is_mining: bool,
    elapsed: u64,
    remaining_after_this: usize,
) -> MiningPollStep {
    if target_is_air {
        return if remaining_after_this == 0 {
            MiningPollStep::Finished
        } else {
            MiningPollStep::Advance
        };
    }
    if elapsed >= MINING_STALL_TICKS {
        return MiningPollStep::Blocked;
    }
    // 组件没了说明挖掘被打断（走开、被顶掉的动作等）：重发一次自愈。
    MiningPollStep::Keep {
        reissue: !is_mining,
    }
}

/// 每 tick 轮询在途挖掘任务，落副作用。
pub(super) fn poll_mining_job(inner: &Inner, bot: &Client) {
    let tick = inner.tick.load(Ordering::Acquire);
    let (target, elapsed, remaining_after_this) = {
        let job = inner.mining_job.lock();
        let Some(job) = job.as_ref() else { return };
        let Some(target) = job.targets.get(job.cursor).copied() else {
            return;
        };
        (
            target,
            tick.saturating_sub(job.since_tick),
            job.targets.len() - job.cursor - 1,
        )
    };

    // 读不到（未加载等）：当作没碎（false），交给时限去判卡住。
    let target_is_air = super::door::block_is_air(inner, target).unwrap_or_default();
    let is_mining = bot.get_component::<azalea::mining::Mining>().is_some();

    match mining_poll_step(target_is_air, is_mining, elapsed, remaining_after_this) {
        MiningPollStep::Keep { reissue } => {
            if reissue {
                super::door::begin_mining(bot, target);
            }
        }
        MiningPollStep::Advance => {
            let (next, progress) = {
                let mut job = inner.mining_job.lock();
                let Some(job) = job.as_mut() else { return };
                job.cursor += 1;
                job.since_tick = tick;
                (
                    job.targets.get(job.cursor).copied(),
                    (
                        crate::JobKind::Mine {
                            targets: job.targets.clone(),
                            done: job.cursor,
                        },
                        crate::JobProgress::Mined {
                            done: job.cursor,
                            total: job.targets.len(),
                        },
                    ),
                )
            };
            // 一块碎了就说一句：整队挖完才是终局，中途不该沉默到底。
            inner.push_job_progress(progress.0, progress.1);
            if let Some(next) = next {
                super::door::begin_mining(bot, next);
            }
        }
        MiningPollStep::Finished => {
            inner.end_mining_job(JobOutcome::Mined);
        }
        MiningPollStep::Blocked => {
            inner.end_mining_job(JobOutcome::MineBlocked);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 判据是「目标变空气」，不是计时——徒手挖原木要好几秒，按时间猜必然错。
    #[test]
    fn air_means_broken_and_drives_the_cursor() {
        assert_eq!(mining_poll_step(true, false, 3, 2), MiningPollStep::Advance);
        assert_eq!(
            mining_poll_step(true, false, 3, 0),
            MiningPollStep::Finished
        );
    }

    /// 没碎就继续；组件还在说明正常开挖中，不必重发。
    #[test]
    fn still_solid_keeps_going_without_reissue_while_mining() {
        assert_eq!(
            mining_poll_step(false, true, 5, 1),
            MiningPollStep::Keep { reissue: false }
        );
    }

    /// 组件没了 = 挖掘被打断，重发一次自愈（旧实现在这里会永远停住）。
    #[test]
    fn losing_the_mining_component_reissues() {
        assert_eq!(
            mining_poll_step(false, false, 5, 1),
            MiningPollStep::Keep { reissue: true }
        );
    }

    /// 超时限仍不碎才判卡住——够不着、被挡、方块挖不动都落这里。
    #[test]
    fn stalling_past_the_limit_blocks_the_queue() {
        assert_eq!(
            mining_poll_step(false, true, MINING_STALL_TICKS, 3),
            MiningPollStep::Blocked
        );
        // 碎了优先于卡住：同一 tick 内两者都成立时按碎了算。
        assert_eq!(
            mining_poll_step(true, true, MINING_STALL_TICKS, 3),
            MiningPollStep::Advance
        );
    }
}
