//! Stable runtime handle exposed to the facade and tests.

use super::*;

/// Runtime's internal representation of one atomic frame capture.  It is
/// converted to `mineintent_contracts::minecraft::MinecraftFrameFacts` by the
/// facade after this read lock has already captured all three values.
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeFrameFacts {
    pub snapshot: MinecraftSnapshotV1,
    pub armor: Option<u8>,
    pub light: Option<u8>,
}

/// 对齐 MineIntent `snapshot/subscribe/motor/sendChat` 边界的本地运行时句柄。
#[derive(Clone)]
pub struct RuntimeHandle {
    pub(super) shared: Arc<SharedRuntime>,
}

impl RuntimeHandle {
    pub fn new(config: RunConfig) -> Self {
        Self {
            shared: Arc::new(SharedRuntime::new(config)),
        }
    }

    /// Read the lifecycle state owned by the real runtime admission paths.
    /// Facades should delegate to this value instead of maintaining a second
    /// state machine from a best-effort event subscription.
    pub fn state(&self) -> BackendState {
        self.shared.backend_state()
    }

    /// Return the epoch owned by the runtime admission state.  Facade-owned
    /// observation and motor handles use this value to reject a handle from a
    /// previous connection attempt before delegating to the concrete runtime
    /// seam.
    pub fn connection_epoch(&self) -> u64 {
        self.shared.connection_epoch()
    }

    pub fn snapshot(&self) -> Option<MinecraftSnapshotV1> {
        self.shared.stored_snapshot()
    }

    /// Capture snapshot, armor, and self-light from one observation read.
    pub fn capture_frame_facts(&self) -> Option<RuntimeFrameFacts> {
        self.shared.capture_frame_facts()
    }

    /// 测试专用：直接落一个生命周期状态。脚本驱动只投事件不跑真实 reducer，
    /// facade 侧的状态相关准入需要一个确定性的造态点。
    #[cfg(test)]
    pub(crate) fn test_set_backend_state(&self, state: BackendState) {
        self.shared.set_backend_state(state);
    }

    #[cfg(test)]
    pub(crate) fn test_install_frame_facts(
        &self,
        snapshot: MinecraftSnapshotV1,
        armor: Option<u8>,
        light: Option<u8>,
    ) {
        let scope_generation = self.shared.entity_producer.lock().scope_generation;
        let mut observation = self.shared.observation.write();
        observation.snapshot = Some(snapshot.clone());
        observation.snapshot_scope_generation = scope_generation;
        observation.armor = armor;
        observation.armor_epoch = Some(snapshot.connection_epoch);

        let geometry = LightSectionGeometry {
            min_light_section: (snapshot.world.min_y >> 4) - 1,
            light_section_count: (snapshot.world.height / 16 + 2) as usize,
        };
        observation.light_cache.reset_scope(
            snapshot.connection_epoch,
            scope_generation,
            Some(snapshot.world.dimension.clone()),
            Some(false),
        );
        observation.light_cache.context =
            observation.light_cache.context.take().map(|mut context| {
                context.geometry = Some(geometry);
                context
            });
        if let Some(light) = light {
            let Some(x) = floor_block_coordinate(snapshot.self_snapshot.position.x) else {
                return;
            };
            let Some(y) = floor_block_coordinate(snapshot.self_snapshot.position.y) else {
                return;
            };
            let Some(z) = floor_block_coordinate(snapshot.self_snapshot.position.z) else {
                return;
            };
            let Some(section_index) = geometry.index_for_section_y(y.div_euclid(16)) else {
                return;
            };
            let layer_index = ((y.rem_euclid(16) as usize) << 8)
                | ((z.rem_euclid(16) as usize) << 4)
                | (x.rem_euclid(16) as usize);
            let mut layer = Box::new([0; 4096]);
            layer[layer_index] = light;
            let mut chunk = CachedLightChunk {
                sky: vec![None; geometry.light_section_count],
                block: vec![None; geometry.light_section_count],
            };
            chunk.block[section_index] = Some(layer);
            observation
                .light_cache
                .chunks
                .insert((x.div_euclid(16), z.div_euclid(16)), chunk);
        }
        observation.bump_generation();
    }

    /// 返回当前 `snapshot()` 的事实来源；调用方不得把 `client_predicted`
    /// 快照当作服务端确认状态。
    pub fn snapshot_source(&self) -> Option<FactSource> {
        self.shared.observation.read().source
    }

    pub fn subscribe(&self) -> RuntimeEventReceiver {
        #[cfg(test)]
        let queue =
            RuntimeEventQueue::new(self.shared.runtime_broker_backpressure_hook.lock().clone());
        #[cfg(not(test))]
        let queue = RuntimeEventQueue::new();
        self.shared.subscribers.lock().push(queue.clone());
        RuntimeEventReceiver { queue }
    }

    pub fn observation_source(&self) -> RuntimeObservationSource {
        RuntimeObservationSource {
            shared: self.shared.clone(),
            bound_epoch: self.shared.connection_epoch(),
        }
    }

    pub fn send_command(&self, command: BackendCommandEnvelope) -> Result<(), String> {
        self.send_command_for_optional_epoch(command, None)
    }

    fn send_command_for_optional_epoch(
        &self,
        command: BackendCommandEnvelope,
        expected_epoch: Option<u64>,
    ) -> Result<(), String> {
        if command.protocol != BACKEND_COMMAND_PROTOCOL {
            return Err(format!(
                "不支持的命令协议：{}，期望 {}",
                command.protocol, BACKEND_COMMAND_PROTOCOL
            ));
        }
        validate_command(&command.command)?;
        self.shared
            .enqueue_command_if_running(command, expected_epoch)
    }

    fn send_command_with_completion(
        &self,
        command: BackendCommand,
    ) -> Result<CommandCompletion, String> {
        self.send_command_with_completion_for_optional_epoch(command, None)
    }

    fn send_command_with_completion_for_optional_epoch(
        &self,
        command: BackendCommand,
        expected_epoch: Option<u64>,
    ) -> Result<CommandCompletion, String> {
        validate_command(&command)?;
        let envelope = self.next_command(command);
        let (completion, state) = CommandCompletion::channel(envelope.id.clone());
        match self.shared.enqueue_command_with_completion_if_running(
            envelope,
            state.clone(),
            expected_epoch,
        ) {
            Ok(()) => Ok(completion),
            Err(error) => {
                state.cancel(format!("command:{}", completion.command_id), false);
                Err(error)
            }
        }
    }

    fn next_command(&self, command: BackendCommand) -> BackendCommandEnvelope {
        let id = self.shared.command_revision.fetch_add(1, Ordering::AcqRel) + 1;
        BackendCommandEnvelope {
            protocol: BACKEND_COMMAND_PROTOCOL.to_owned(),
            id: format!("command-{id}"),
            issued_at: now_utc(),
            command,
        }
    }

    pub fn send_chat(&self, message: impl Into<String>) -> Result<(), String> {
        self.send_command(self.next_command(BackendCommand::SendChat {
            message: message.into(),
        }))
    }

    pub(crate) fn send_chat_for_epoch(
        &self,
        expected_epoch: u64,
        message: impl Into<String>,
    ) -> Result<(), String> {
        self.send_command_for_optional_epoch(
            self.next_command(BackendCommand::SendChat {
                message: message.into(),
            }),
            Some(expected_epoch),
        )
    }

    /// 发送与主仓库 motor `lookRelative` 同语义的相对视角输入，并返回一次性完成 future。
    pub fn look_relative(
        &self,
        yaw_degrees: f32,
        pitch_degrees: f32,
    ) -> Result<CommandCompletion, String> {
        self.send_command_with_completion(BackendCommand::LookRelative {
            yaw_degrees,
            pitch_degrees,
        })
    }

    pub(crate) fn look_relative_for_epoch(
        &self,
        expected_epoch: u64,
        yaw_degrees: f32,
        pitch_degrees: f32,
    ) -> Result<CommandCompletion, String> {
        self.send_command_with_completion_for_optional_epoch(
            BackendCommand::LookRelative {
                yaw_degrees,
                pitch_degrees,
            },
            Some(expected_epoch),
        )
    }

    /// 发送按键式移动输入；校验范围与主仓库 motor 的 50–1500ms 边界一致，
    /// 并返回在释放动作完成时 resolve 的 future。
    pub fn move_input(
        &self,
        directions: Vec<MotorDirection>,
        duration_ms: u64,
        sprint: Option<bool>,
        jump: Option<bool>,
        crouch: Option<bool>,
    ) -> Result<CommandCompletion, String> {
        self.send_command_with_completion(BackendCommand::Move {
            directions,
            duration_ms,
            sprint,
            jump,
            crouch,
        })
    }

    pub(crate) fn move_input_for_epoch(
        &self,
        expected_epoch: u64,
        directions: Vec<MotorDirection>,
        duration_ms: u64,
        sprint: Option<bool>,
        jump: Option<bool>,
        crouch: Option<bool>,
    ) -> Result<CommandCompletion, String> {
        self.send_command_with_completion_for_optional_epoch(
            BackendCommand::Move {
                directions,
                duration_ms,
                sprint,
                jump,
                crouch,
            },
            Some(expected_epoch),
        )
    }

    /// 释放全部移动/跳跃/潜行输入。
    pub fn release_all(&self) -> Result<CommandCompletion, String> {
        self.send_command_with_completion(BackendCommand::ReleaseAll)
    }

    pub(crate) fn release_all_for_epoch(
        &self,
        expected_epoch: u64,
    ) -> Result<CommandCompletion, String> {
        self.send_command_with_completion_for_optional_epoch(
            BackendCommand::ReleaseAll,
            Some(expected_epoch),
        )
    }

    /// 显式请求服务端执行重生；死亡后不会由运行时自动触发。
    pub fn respawn(&self) -> Result<(), String> {
        self.send_command(self.next_command(BackendCommand::Respawn))
    }

    /// 绑定世代的重生：与其余马达命令同一条准入链，陈旧世代拒于入队。
    pub(crate) fn respawn_for_epoch(
        &self,
        expected_epoch: u64,
    ) -> Result<CommandCompletion, String> {
        self.send_command_with_completion_for_optional_epoch(
            BackendCommand::Respawn,
            Some(expected_epoch),
        )
    }

    /// 主动结束运行时；停止动作本身会写入 `commanded` 事件。
    pub fn stop(&self, reason: &str) {
        self.shared.initiate_stop(reason);
    }

    #[cfg(test)]
    pub(crate) async fn test_wait_for_shutdown(&self) {
        self.shared.shutdown.notified().await;
    }

    #[cfg(test)]
    pub(crate) fn test_drive_event(&self, source: FactSource, payload: BackendEventPayload) {
        match payload.clone() {
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected) => {
                self.shared.emit_transport_connected();
            }
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::LoggedIn {
                version,
                dimension,
            }) => {
                self.shared.emit_logged_in(version, dimension);
            }
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Ready {
                snapshot_revision,
            }) => {
                self.shared.emit_ready(snapshot_revision);
            }
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Faulted { failure }) => {
                self.shared.emit_faulted(failure);
            }
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Stopped { ref reason }) => {
                self.shared.initiate_stop(reason);
            }
            _ => self.shared.emit(source, payload),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_set_event_dispatch_backpressure_hook(
        &self,
        hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        self.shared.set_event_dispatch_backpressure_hook(hook);
    }

    #[cfg(test)]
    pub(crate) fn test_set_stop_signal_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.shared.set_stop_signal_hook(hook);
    }

    #[cfg(test)]
    pub(crate) fn test_set_runtime_broker_backpressure_hook(
        &self,
        hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) {
        self.shared.set_runtime_broker_backpressure_hook(hook);
    }

    #[cfg(test)]
    pub(crate) fn test_event_dispatch_counts(&self) -> (usize, usize, usize, usize) {
        self.shared.event_dispatch_counts()
    }

    #[cfg(test)]
    pub(crate) fn test_settle_next_command(&self, result: Result<(), BackendError>) -> bool {
        while let Some(command) = self.shared.pop_command() {
            if command
                .completion
                .as_ref()
                .is_some_and(|completion| completion.cancelled.load(Ordering::Acquire))
            {
                continue;
            }
            finish_command(&command.completion, result);
            return true;
        }
        false
    }

    #[cfg(test)]
    pub(crate) fn test_has_pending_command(&self) -> bool {
        !self.shared.commands.lock().is_empty()
    }
}
