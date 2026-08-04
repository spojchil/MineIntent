use super::*;

/// Minecraft transport runtime configuration shared by the diagnostic binary
/// and the in-process composition root.
#[derive(Clone, Debug)]
pub struct RunConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub world_id: String,
    pub duration: Duration,
    /// 传输阶段 deadline；与上层 `OperationControl` 的 deadline 相互独立。
    pub timeouts: BackendTimeouts,
    /// 传输级指数重连策略，字段与冻结 contracts DTO 一一对应。
    pub reconnect: ReconnectPolicy,
    /// The diagnostic CLI uses a finite duration; a composition root owns the
    /// backend lifecycle and disables this timer for a production facade.
    pub auto_stop: bool,
    /// The diagnostic CLI exposes the event stream on stdout.  In-process
    /// composition roots consume the same events through `subscribe` and keep
    /// this boundary silent.
    pub emit_stdout: bool,
    /// 仅用于本地验收 M2；正式集成通过 `RuntimeHandle::send_chat`。
    pub initial_chat: Option<String>,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: 25565,
            username: "MineIntentBot".to_owned(),
            world_id: "paper-local-world".to_owned(),
            duration: Duration::from_secs(30),
            timeouts: BackendTimeouts {
                connect_ms: 10_000,
                login_ms: 20_000,
                spawn_ms: 30_000,
                stop_ms: 5_000,
            },
            reconnect: ReconnectPolicy {
                enabled: true,
                initial_delay_ms: 1_000,
                multiplier: 2.0,
                max_delay_ms: 30_000,
                jitter_ratio: 0.2,
                stable_reset_ms: 60_000,
            },
            auto_stop: true,
            emit_stdout: true,
            initial_chat: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RetrySchedule {
    pub(super) delay: Duration,
    pub(super) retry_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TransportPhase {
    Connecting,
    LoggingIn,
    Spawning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PhaseDeadlineToken {
    pub(super) epoch: u64,
    pub(super) attempt: u64,
    pub(super) phase: TransportPhase,
    pub(super) generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StableResetToken {
    pub(super) epoch: u64,
    pub(super) attempt: u64,
    pub(super) generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct StopWatchdogToken {
    pub(super) generation: u64,
}

/// 计算 TS oracle 的重连退避。`attempt` 是当前已经开始的 retry ordinal，因而首次
/// 重连使用 exponent 0。所有浮点中间值都在转入 `Duration` 前饱和，避免极大 multiplier
/// 或 ordinal 造成 panic/wrap。
pub(super) fn reconnect_schedule_at(
    policy: &ReconnectPolicy,
    attempt: u64,
    random: f64,
    now: chrono::DateTime<chrono::Utc>,
) -> RetrySchedule {
    let exponent = attempt.saturating_sub(1) as f64;
    let initial = policy.initial_delay_ms as f64;
    let maximum = policy.max_delay_ms as f64;
    let grown = initial * policy.multiplier.powf(exponent);
    let base = if !grown.is_finite() {
        maximum
    } else {
        grown.min(maximum)
    };
    let random = if random.is_finite() {
        random.clamp(0.0, 1.0)
    } else {
        0.5
    };
    let jitter = (random * 2.0 - 1.0) * policy.jitter_ratio;
    let adjusted = base * (1.0 + jitter);
    let rounded = if !adjusted.is_finite() {
        maximum
    } else {
        // All valid policy values make the intended quantity non-negative.
        // Keep the guard explicit because this helper is also the runtime
        // boundary for manually constructed RunConfig values.
        (adjusted.max(0.0) + 0.5).floor()
    };
    let delay_ms = if !rounded.is_finite() || rounded >= u64::MAX as f64 {
        u64::MAX
    } else {
        rounded as u64
    };
    let delay = Duration::from_millis(delay_ms);
    let retry_at = retry_at_with_delay(now, delay_ms);
    RetrySchedule { delay, retry_at }
}

fn retry_at_with_delay(
    now: chrono::DateTime<chrono::Utc>,
    delay_ms: u64,
) -> chrono::DateTime<chrono::Utc> {
    let chrono_ms = i64::try_from(delay_ms).unwrap_or(i64::MAX);
    now.checked_add_signed(chrono::Duration::milliseconds(chrono_ms))
        .unwrap_or(chrono::DateTime::<chrono::Utc>::MAX_UTC)
}

pub(super) fn next_reconnect_random(seed: &AtomicU64) -> f64 {
    // A small local seam is sufficient for jitter; it avoids adding a new
    // manifest dependency and can be replaced by a deterministic test seed.
    let mut current = seed.load(Ordering::Relaxed);
    loop {
        let next = current
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        match seed.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => return (next >> 11) as f64 / (1u64 << 53) as f64,
            Err(observed) => current = observed,
        }
    }
}

pub(super) fn checked_atomic_increment(counter: &AtomicU64) -> Option<u64> {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let next = current.checked_add(1)?;
        match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Some(next),
            Err(observed) => current = observed,
        }
    }
}
