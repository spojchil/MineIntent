//! Command validation, completion ownership, and Azalea motor execution.

use super::*;

/// 服务端先发送死亡/生命值更新、再在同一 tick 设置 waitingForRespawn；显式
/// respawn 若紧贴 DeathEvent 发出会落在这段窗口里。这个延迟只作用于上层已经
/// 明确请求的重生，不会在死亡事件上自动创建请求。
const RESPAWN_SETTLE_DELAY: Duration = Duration::from_millis(100);

pub(super) fn validate_command(command: &BackendCommand) -> Result<(), String> {
    match command {
        BackendCommand::SendChat { message } => {
            if message.is_empty() || message.contains(['\r', '\n', '\0']) {
                return Err("聊天消息必须是非空的单行文本".to_owned());
            }
        }
        BackendCommand::LookRelative {
            yaw_degrees,
            pitch_degrees,
        } => {
            if !yaw_degrees.is_finite() || yaw_degrees.abs() > 90.0 {
                return Err("相对 yaw 必须是 ±90 度以内的有限数".to_owned());
            }
            if !pitch_degrees.is_finite() || pitch_degrees.abs() > 90.0 {
                return Err("相对 pitch 必须是 ±90 度以内的有限数".to_owned());
            }
        }
        BackendCommand::Move {
            directions,
            duration_ms,
            ..
        } => {
            if directions.is_empty() || directions.len() > 4 {
                return Err("移动方向必须包含 1 到 4 个按键".to_owned());
            }
            if directions
                .iter()
                .enumerate()
                .any(|(index, direction)| directions[index + 1..].contains(direction))
            {
                return Err("移动方向不能重复".to_owned());
            }
            if !(50..=1_500).contains(duration_ms) {
                return Err("移动时长必须是 50 到 1500 毫秒".to_owned());
            }
        }
        BackendCommand::ReleaseAll | BackendCommand::Respawn => {}
    }
    Ok(())
}

pub(super) struct CommandCompletionState {
    pub(super) sender: parking_lot::Mutex<Option<oneshot::Sender<Result<(), BackendError>>>>,
    pub(super) settled_result: parking_lot::Mutex<Option<Result<(), BackendError>>>,
    pub(super) settled_cv: parking_lot::Condvar,
    /// Owns the single finishing transition. `settled` is published only
    /// after result, physical-release bookkeeping, and the oneshot have all
    /// been published under this ownership.
    pub(super) finish_lock: parking_lot::Mutex<()>,
    pub(super) cancelled: AtomicBool,
    pub(super) active_release: AtomicBool,
    pub(super) release_on_cancel: AtomicBool,
    pub(super) cancel_signal: parking_lot::Mutex<Option<Arc<Notify>>>,
    pub(super) settled: AtomicBool,
    pub(super) settled_signal: Notify,
}

impl CommandCompletionState {
    pub(super) fn finish(&self, result: Result<(), BackendError>) {
        let _finish = self.finish_lock.lock();
        if self.settled.load(Ordering::Acquire) {
            return;
        }
        *self.settled_result.lock() = Some(result.clone());
        self.active_release.store(false, Ordering::Release);
        if let Some(sender) = self.sender.lock().take() {
            let _ = sender.send(result);
        }
        // This is deliberately the last publication in the finishing
        // transition. Waiters that observe `settled` therefore also observe
        // the result and the completed physical-release bookkeeping.
        self.settled.store(true, Ordering::Release);
        self.settled_cv.notify_all();
        self.settled_signal.notify_one();
    }

    pub(super) fn set_cancel_signal(&self, signal: Arc<Notify>) {
        let already_cancelled = self.cancelled.load(Ordering::Acquire);
        *self.cancel_signal.lock() = Some(signal.clone());
        if already_cancelled {
            signal.notify_one();
        }
    }

    pub(super) fn begin_active_release(&self, signal: Arc<Notify>) {
        self.active_release.store(true, Ordering::Release);
        self.set_cancel_signal(signal);
    }

    #[cfg(test)]
    pub(super) async fn wait_settled(&self) {
        while !self.settled.load(Ordering::Acquire) {
            self.settled_signal.notified().await;
        }
    }

    pub(super) fn cancel(&self, operation: String, release_on_cancel: bool) {
        self.cancelled.store(true, Ordering::Release);
        self.release_on_cancel
            .fetch_or(release_on_cancel, Ordering::AcqRel);
        if let Some(signal) = self.cancel_signal.lock().as_ref() {
            signal.notify_one();
        }
        // An active Move owns the physical release.  Its task finishes the
        // oneshot only after inputs and active state have been cleared.  A
        // queued/superseded command has no physical work left and settles now.
        if !self.active_release.load(Ordering::Acquire)
            || !self.release_on_cancel.load(Ordering::Acquire)
        {
            self.finish(Err(BackendError::Cancelled { operation }));
        }
    }
}

/// Minimal command completion seam used by the runtime motor queue.
///
/// It is intentionally not a backend facade: callers only get the command id,
/// cancellation, and one ordered result for the queued motor action.
pub struct CommandCompletion {
    pub(super) command_id: String,
    receiver: oneshot::Receiver<Result<(), BackendError>>,
    state: Arc<CommandCompletionState>,
}

impl CommandCompletion {
    pub(super) fn channel(command_id: String) -> (Self, Arc<CommandCompletionState>) {
        let (sender, receiver) = oneshot::channel();
        let state = Arc::new(CommandCompletionState {
            sender: parking_lot::Mutex::new(Some(sender)),
            settled_result: parking_lot::Mutex::new(None),
            settled_cv: parking_lot::Condvar::new(),
            finish_lock: parking_lot::Mutex::new(()),
            cancelled: AtomicBool::new(false),
            active_release: AtomicBool::new(false),
            release_on_cancel: AtomicBool::new(false),
            cancel_signal: parking_lot::Mutex::new(None),
            settled: AtomicBool::new(false),
            settled_signal: Notify::new(),
        });
        (
            Self {
                command_id,
                receiver,
                state: state.clone(),
            },
            state,
        )
    }

    pub fn command_id(&self) -> &str {
        &self.command_id
    }

    pub fn cancel(&self) {
        self.state
            .cancel(format!("command:{}", self.command_id), true);
    }

    pub async fn wait(self) -> Result<(), BackendError> {
        match self.receiver.await {
            Ok(result) => result,
            Err(_) => Err(BackendError::Cancelled {
                operation: format!("command:{}", self.command_id),
            }),
        }
    }

    /// Synchronous companion used by the frozen `release_all` facade method.
    /// The condition variable is independent of Tokio, so this remains safe
    /// to call from a caller that happens to be inside another async runtime.
    pub(crate) fn wait_blocking(self) -> Result<(), BackendError> {
        let mut result = self.state.settled_result.lock();
        while result.is_none() {
            self.state.settled_cv.wait(&mut result);
        }
        result
            .take()
            .expect("settled result exists after completion wait")
    }

    pub(crate) fn cancellation_handle(&self) -> CommandCompletionCancellation {
        CommandCompletionCancellation(self.state.clone())
    }
}

#[derive(Clone)]
pub(crate) struct CommandCompletionCancellation(pub(super) Arc<CommandCompletionState>);

impl CommandCompletionCancellation {
    pub(crate) fn cancel(&self) {
        self.0
            .cancel("command completion cancelled by caller".to_owned(), true);
    }

    pub(crate) async fn wait_settled(&self) {
        while !self.0.settled.load(Ordering::Acquire) {
            self.0.settled_signal.notified().await;
        }
    }
}

pub(super) struct QueuedCommand {
    /// Runtime epoch observed atomically with queue admission. Epochs never
    /// repeat, so reconnect cannot transfer an A command to B.
    pub(super) connection_epoch: u64,
    pub(super) envelope: BackendCommandEnvelope,
    pub(super) completion: Option<Arc<CommandCompletionState>>,
}

pub(super) fn direction_for(directions: &[MotorDirection]) -> WalkDirection {
    let forward = directions.contains(&MotorDirection::Forward);
    let back = directions.contains(&MotorDirection::Back);
    let left = directions.contains(&MotorDirection::Left);
    let right = directions.contains(&MotorDirection::Right);
    match (forward, back, left, right) {
        (true, false, true, false) => WalkDirection::ForwardLeft,
        (true, false, false, true) => WalkDirection::ForwardRight,
        (false, true, true, false) => WalkDirection::BackwardLeft,
        (false, true, false, true) => WalkDirection::BackwardRight,
        (true, false, false, false) => WalkDirection::Forward,
        (false, true, false, false) => WalkDirection::Backward,
        (false, false, true, false) => WalkDirection::Left,
        (false, false, false, true) => WalkDirection::Right,
        _ => WalkDirection::None,
    }
}

pub(super) fn sprint_direction(direction: WalkDirection) -> Option<SprintDirection> {
    match direction {
        WalkDirection::Forward => Some(SprintDirection::Forward),
        WalkDirection::ForwardLeft => Some(SprintDirection::ForwardLeft),
        WalkDirection::ForwardRight => Some(SprintDirection::ForwardRight),
        _ => None,
    }
}

/// 断线时本地玩家实体可能已经被 Azalea 移除；运动清理必须使用可失败查询。
pub(super) fn try_set_movement_flags(bot: &Client, jumping: bool, crouching: bool) -> bool {
    bot.try_query_self::<(&mut azalea::entity::Jumping, &mut azalea::PhysicsState), _>(
        |(mut jumping_component, mut physics)| {
            **jumping_component = jumping;
            physics.trying_to_crouch = crouching;
        },
    )
    .is_ok()
}

pub(super) fn finish_command(
    completion: &Option<Arc<CommandCompletionState>>,
    result: Result<(), BackendError>,
) {
    if let Some(completion) = completion {
        completion.finish(result);
    }
}

pub(super) fn reject_command_after_stop(
    shared: &Arc<SharedRuntime>,
    command_id: &str,
    completion: &Option<Arc<CommandCompletionState>>,
) -> bool {
    if shared.command_execution_allowed() {
        return false;
    }
    finish_command(
        completion,
        Err(BackendError::Cancelled {
            operation: format!("command:{command_id}"),
        }),
    );
    true
}

pub(super) fn command_component_failure(operation: &str) -> BackendError {
    BackendError::BackendFailure {
        failure: BackendFailure {
            code: BackendFailureCode::ProtocolError,
            message: format!("{operation} requires an active local player"),
            retryable: true,
        },
    }
}

/// Release one active Move and settle its completion only after the physical
/// release attempt and the shared active-state cleanup have both completed.
/// The actuator is injected so the ordering seam is testable without creating
/// an Azalea client; production supplies the real walk/flag release closure.
pub(super) fn release_active_movement_and_finish(
    shared: &Arc<SharedRuntime>,
    command_id: &str,
    generation: u64,
    completion: &Option<Arc<CommandCompletionState>>,
    release_inputs: impl FnOnce() -> bool,
    failure_operation: &str,
    result_if_released: Result<(), BackendError>,
) {
    release_active_movement_and_finish_for_owner(
        shared,
        None,
        command_id,
        generation,
        completion,
        release_inputs,
        failure_operation,
        result_if_released,
    );
}

fn release_client_active_movement_and_finish(
    shared: &Arc<SharedRuntime>,
    client: &Client,
    connection_epoch: u64,
    command_id: &str,
    generation: u64,
    completion: &Option<Arc<CommandCompletionState>>,
    release_inputs: impl FnOnce() -> bool,
    failure_operation: &str,
    result_if_released: Result<(), BackendError>,
) {
    release_active_movement_and_finish_for_owner(
        shared,
        Some((client, connection_epoch)),
        command_id,
        generation,
        completion,
        release_inputs,
        failure_operation,
        result_if_released,
    );
}

fn release_active_movement_and_finish_for_owner(
    shared: &Arc<SharedRuntime>,
    owner: Option<(&Client, u64)>,
    command_id: &str,
    generation: u64,
    completion: &Option<Arc<CommandCompletionState>>,
    release_inputs: impl FnOnce() -> bool,
    failure_operation: &str,
    result_if_released: Result<(), BackendError>,
) {
    let result = {
        let _admission = shared.command_admission.lock();
        let owns_movement = shared.movement_generation.load(Ordering::Acquire) == generation
            && shared.active_movement_id.lock().as_deref() == Some(command_id);
        if !owns_movement {
            return;
        }
        if let Some((client, connection_epoch)) = owner {
            if let Some(error) = shared.client_command_error_locked(
                client,
                connection_epoch,
                &format!("command:{command_id}"),
            ) {
                // The old task still owns its bookkeeping, but it must not
                // touch an entity that may now represent the next attempt.
                error
            } else if release_inputs() {
                result_if_released
            } else {
                command_component_failure(failure_operation)
            }
        } else if release_inputs() {
            result_if_released
        } else {
            command_component_failure(failure_operation)
        }
    };
    // Keep the active completion/id visible to stop until the physical release
    // result has been settled. A stop racing this section must defer stopped;
    // the generation/id checks below prevent an old task from clearing a new
    // movement that was admitted after its release.
    finish_command(completion, result.map_or_else(Err, |_| Ok(())));
    {
        let _admission = shared.command_admission.lock();
        if shared.clear_registered_active_movement(command_id, generation, &None, completion) {
            shared.active_movement_cancel_signal.lock().take();
        }
    }
    shared.finalize_stop_if_ready();
}

pub(super) fn handle_command(bot: &Client, shared: &Arc<SharedRuntime>, queued: QueuedCommand) {
    let QueuedCommand {
        envelope,
        completion,
    } = queued;
    let command_id = envelope.id;
    if completion
        .as_ref()
        .is_some_and(|completion| completion.cancelled.load(Ordering::Acquire))
    {
        return;
    }
    if reject_command_after_stop(shared, &command_id, &completion) {
        return;
    }
    match envelope.command {
        BackendCommand::SendChat { message } => {
            match shared.with_command_admission(|| bot.chat(message)) {
                Ok(()) => finish_command(&completion, Ok(())),
                Err(()) => {
                    finish_command(
                        &completion,
                        Err(BackendError::Cancelled {
                            operation: format!("command:{command_id}"),
                        }),
                    );
                }
            }
        }
        BackendCommand::LookRelative {
            yaw_degrees,
            pitch_degrees,
        } => {
            let result = shared.with_command_admission(|| {
                let direction = bot.direction();
                bot.set_direction(
                    direction.y_rot() - yaw_degrees,
                    (direction.x_rot() - pitch_degrees).clamp(-90.0, 90.0),
                );
            });
            match result {
                Ok(()) => finish_command(&completion, Ok(())),
                Err(()) => finish_command(
                    &completion,
                    Err(BackendError::Cancelled {
                        operation: format!("command:{command_id}"),
                    }),
                ),
            }
        }
        BackendCommand::Move {
            directions,
            duration_ms,
            sprint,
            jump,
            crouch,
        } => {
            shared.cancel_active_movement(false);
            let direction = direction_for(&directions);
            let generation = shared.movement_generation.fetch_add(1, Ordering::AcqRel) + 1;
            let registration =
                shared.register_active_movement(&command_id, generation, duration_ms, &completion);
            let ActiveMovementRegistration::Started { cancel_signal } = registration else {
                return;
            };

            // The cancellation/generation check and the first actuator call
            // share one admission point. A cancellation that wins cannot
            // touch the bot; an actuator that wins leaves the same generation
            // for the release task to clean up.
            let actuator_result =
                shared.with_active_movement_admission(&command_id, generation, &completion, || {
                    if sprint.unwrap_or(false) {
                        if let Some(sprint_direction) = sprint_direction(direction) {
                            bot.sprint(sprint_direction);
                        } else {
                            bot.walk(direction);
                        }
                    } else {
                        bot.walk(direction);
                    }
                    if !try_set_movement_flags(bot, jump.unwrap_or(false), crouch.unwrap_or(false))
                    {
                        bot.walk(WalkDirection::None);
                        return false;
                    }
                    if duration_ms == 0 {
                        bot.walk(WalkDirection::None);
                    }
                    true
                });
            let started = match actuator_result {
                Ok(started) => started,
                Err(()) => {
                    shared.cancel_registered_active_movement(
                        &command_id,
                        generation,
                        &cancel_signal,
                        &completion,
                    );
                    return;
                }
            };

            if !started {
                shared.clear_registered_active_movement(
                    &command_id,
                    generation,
                    &cancel_signal,
                    &completion,
                );
                finish_command(&completion, Err(command_component_failure("move")));
                shared.finish_active_movement_registration();
                return;
            }

            if duration_ms == 0 {
                shared.clear_registered_active_movement(
                    &command_id,
                    generation,
                    &cancel_signal,
                    &completion,
                );
                finish_command(&completion, Ok(()));
                shared.finish_active_movement_registration();
            } else {
                let cancel_signal = cancel_signal.expect("duration-positive move signal");
                let bot_to_stop = bot.clone();
                let shared = shared.clone();
                let task_shared = shared.clone();
                let completion_for_task = completion.clone();
                tokio::task::spawn_local(async move {
                    let duration = tokio::time::sleep(Duration::from_millis(duration_ms));
                    tokio::pin!(duration);
                    tokio::select! {
                        _ = &mut duration => {
                            let cancelled = completion_for_task
                                .as_ref()
                                .is_some_and(|completion| completion.cancelled.load(Ordering::Acquire))
                                || task_shared.stopping.load(Ordering::Acquire);
                            release_active_movement_and_finish(
                                &task_shared,
                                &command_id,
                                generation,
                                &completion_for_task,
                                || {
                                    let released = try_set_movement_flags(&bot_to_stop, false, false);
                                    bot_to_stop.walk(WalkDirection::None);
                                    released
                                },
                                "move release",
                                if cancelled {
                                    Err(BackendError::Cancelled {
                                        operation: format!("command:{command_id}"),
                                    })
                                } else {
                                    Ok(())
                                },
                            );
                        }
                        _ = cancel_signal.notified() => {
                            release_active_movement_and_finish(
                                &task_shared,
                                &command_id,
                                generation,
                                &completion_for_task,
                                || {
                                    let released = try_set_movement_flags(&bot_to_stop, false, false);
                                    bot_to_stop.walk(WalkDirection::None);
                                    released
                                },
                                "cancel move",
                                Err(BackendError::Cancelled {
                                    operation: format!("command:{command_id}"),
                                }),
                            );
                        }
                    }
                });
                shared.finish_active_movement_registration();
            }
        }
        BackendCommand::ReleaseAll => {
            let previous_id = shared.active_movement_id.lock().clone();
            let previous_generation = shared.movement_generation.load(Ordering::Acquire);
            let previous_completion = shared
                .cancel_active_movement(true)
                .map(Some)
                .unwrap_or(None);
            if let Some(previous_id) = previous_id {
                release_active_movement_and_finish(
                    shared,
                    &previous_id,
                    previous_generation,
                    &previous_completion,
                    || {
                        let released = try_set_movement_flags(bot, false, false);
                        bot.walk(WalkDirection::None);
                        released
                    },
                    "release_all move",
                    Err(BackendError::Cancelled {
                        operation: format!("command:{previous_id}"),
                    }),
                );
            } else {
                let released = match shared.with_command_admission(|| {
                    let released = try_set_movement_flags(bot, false, false);
                    bot.walk(WalkDirection::None);
                    released
                }) {
                    Ok(released) => released,
                    Err(()) => {
                        finish_command(
                            &completion,
                            Err(BackendError::Cancelled {
                                operation: format!("command:{command_id}"),
                            }),
                        );
                        return;
                    }
                };
                shared.clear_idle_movement_state(previous_generation);
                finish_command(
                    &previous_completion,
                    if released {
                        Err(BackendError::Cancelled {
                            operation: "movement released by release_all".to_owned(),
                        })
                    } else {
                        Err(command_component_failure("release_all move"))
                    },
                );
            }
            match shared.with_command_admission(|| {
                let released = try_set_movement_flags(bot, false, false);
                bot.walk(WalkDirection::None);
                released
            }) {
                Ok(true) => finish_command(&completion, Ok(())),
                Ok(false) => {
                    finish_command(&completion, Err(command_component_failure("release_all")))
                }
                Err(()) => finish_command(
                    &completion,
                    Err(BackendError::Cancelled {
                        operation: format!("command:{command_id}"),
                    }),
                ),
            }
        }
        BackendCommand::Respawn => {
            // 服务端的死亡包与 waitingForRespawn 状态可能跨一个网络 tick；
            // 只延迟这一条已经明确请求的动作，避免请求在服务端状态切换前到达。
            // 仍走 Azalea 自带 RespawnPlugin 的消息链，保持实体绑定和 ECS 时序。
            let delayed_bot = bot.clone();
            let delayed_shared = shared.clone();
            tokio::task::spawn_local(async move {
                tokio::time::sleep(RESPAWN_SETTLE_DELAY).await;
                let _ = delayed_shared.with_command_admission(|| {
                    if delayed_bot
                        .try_query_self::<&LocalEntity, _>(|_| ())
                        .is_err()
                    {
                        return;
                    }
                    delayed_bot
                        .ecs
                        .write()
                        .write_message(azalea::respawn::PerformRespawnEvent {
                            entity: delayed_bot.entity,
                        });
                });
            });
            finish_command(&completion, Ok(()));
        }
    }
}

pub(super) fn process_pending_commands(bot: &Client, shared: &Arc<SharedRuntime>) {
    // 连接建立前的命令保留在队列中，避免把 chat/motor 静默丢在握手阶段。
    if !bot.logged_in() {
        return;
    }
    while let Some(command) = shared.next_command_for_client(bot) {
        handle_command(bot, shared, command);
    }
}
