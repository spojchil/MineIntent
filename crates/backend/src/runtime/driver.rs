//! Azalea application composition, client/swarm event driving, and runtime entrypoints.

use super::*;

#[derive(Clone, Component)]
struct BotState {
    shared: Arc<SharedRuntime>,
}

impl Default for BotState {
    fn default() -> Self {
        Self {
            shared: Arc::new(SharedRuntime::new(RunConfig::default())),
        }
    }
}

#[derive(Clone, Resource)]
pub(super) struct SwarmState {
    pub(super) shared: Arc<SharedRuntime>,
}

impl Default for SwarmState {
    fn default() -> Self {
        Self {
            shared: Arc::new(SharedRuntime::new(RunConfig::default())),
        }
    }
}

/// 在 Azalea 自己的 ECS schedule 内发送退出消息，避免跨任务直接写消息时
/// 与 Bevy 的双缓冲消息更新时序竞争。
struct RuntimeShutdownPlugin;

impl Plugin for RuntimeShutdownPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, emit_app_exit_when_stopping);
        // 死亡后冻结本地物理状态必须在 Azalea 的常规 Update 查询完成后执行，
        // 否则会与更新碰撞盒、准星命中结果的系统产生无序写入警告。
        app.add_systems(PostUpdate, freeze_dead_local_player);
    }
}

/// 禁用自动重生时，死亡是一个需要保持的事实；冻结本地物理，避免死后
/// 因客户端重力继续把观察位置推进到世界边界之外。
fn freeze_dead_local_player(
    mut query: Query<(&mut Physics, &Position), (With<LocalEntity>, With<Dead>)>,
) {
    for (mut physics, position) in &mut query {
        physics.velocity = azalea::Vec3::ZERO;
        physics.set_on_ground(true);
        physics.set_old_pos(*position);
    }
}

fn emit_app_exit_when_stopping(mut app_exit: MessageWriter<AppExit>, state: Res<SwarmState>) {
    if state.shared.stopping.load(Ordering::Acquire) {
        app_exit.write(AppExit::Success);
    }
}

async fn handle_client(bot: Client, event: Event, state: BotState) {
    let shared = &state.shared;
    if matches!(event, Event::Spawn | Event::Tick) {
        if !shared.client_is_current_owner(&bot) || !shared.command_execution_allowed() {
            return;
        }
        process_pending_commands(&bot, &state.shared);
    }
    match event {
        Event::Init => {
            // Swarm 重连在某些路径复用已有本地玩家事件发送器，不一定再次发出
            // Event::Init；重连调度器会预留 epoch，若 Init 到达则消费该预留，避免
            // 同一次握手被错误地记成两个 epoch。
            if shared
                .consume_attempt_for_transport_init_and_bind_with_token(
                    bot.entity,
                    bot.attempt_token(),
                )
                .is_none()
                || !shared.command_execution_allowed()
                || !shared.set_active_client_if_current(&bot)
            {
                return;
            }
        }
        Event::Login => {
            if !shared.client_is_current_owner(&bot) || !shared.command_execution_allowed() {
                return;
            }
            let dimension = bot
                .try_query_self::<Option<&azalea::world::WorldName>, _>(|world_name| {
                    world_name
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "minecraft:overworld".to_owned())
                })
                .unwrap_or_else(|_| "minecraft:overworld".to_owned());
            if !shared.command_execution_allowed() {
                return;
            }
            shared.emit_logged_in("26.1.2", dimension);
        }
        Event::Spawn => {
            if !shared.client_is_current_owner(&bot) || !shared.command_execution_allowed() {
                return;
            }
            let (spawn_allowed, was_dead) = {
                let _admission = shared.command_admission.lock();
                let was_dead = shared.death_reported.load(Ordering::Acquire);
                if !shared.command_execution_allowed_without_lock() {
                    (false, was_dead)
                } else {
                    shared.ready.store(true, Ordering::Release);
                    shared.death_reported.store(false, Ordering::Release);
                    (true, was_dead)
                }
            };
            if !spawn_allowed {
                return;
            }
            if !shared.set_world_if_running(bot.world()) {
                return;
            }
            let snapshot = shared.refresh_snapshot(&bot, true, FactSource::ServerObserved);
            if let Some(snapshot) = snapshot.as_ref() {
                if !shared.set_dimension_if_running(snapshot.world.dimension.clone()) {
                    return;
                }
            }
            if let Some(snapshot) = snapshot {
                if was_dead {
                    shared.emit_respawned(snapshot.world.dimension.clone());
                }
                shared.emit_ready(snapshot.snapshot_revision);
                shared.emit_snapshot(snapshot, FactSource::ServerObserved);
            } else {
                shared.emit_ready(shared.snapshot_revision.load(Ordering::Acquire));
            }

            let _ = shared.with_command_admission(|| {
                if !shared.initial_chat_sent.swap(true, Ordering::AcqRel) {
                    if let Some(message) = shared.config.initial_chat.clone() {
                        bot.chat(message);
                    }
                }
            });

            let start_timer = {
                let _admission = shared.command_admission.lock();
                shared.command_execution_allowed_without_lock()
                    && !shared.timer_started.swap(true, Ordering::AcqRel)
            };
            if start_timer && shared.config.auto_stop {
                let duration = shared.config.duration;
                let shared = state.shared.clone();
                tokio::task::spawn_local(async move {
                    tokio::time::sleep(duration).await;
                    shared.initiate_stop("duration_elapsed");
                });
            }
        }
        Event::KeepAlive(_id) => {}
        Event::Chat(packet) => {
            if !shared.client_is_current_owner(&bot) {
                return;
            }
            shared.emit_if_running(
                FactSource::ServerObserved,
                BackendEventPayload::Chat(ContractProtocolChatEvent {
                    sender_username: packet.sender(),
                    plain_text: packet.content(),
                    position: Some(ChatPosition::Chat),
                    verified: None,
                }),
            );
        }
        Event::Death(_) => {
            if !shared.client_is_current_owner(&bot) {
                return;
            }
            if shared
                .admit_death_and_release(|| {
                    let released = try_set_movement_flags(&bot, false, false);
                    bot.walk(WalkDirection::None);
                    released
                })
                .is_none()
            {
                return;
            }
            if let Some(snapshot) = shared.refresh_snapshot(&bot, true, FactSource::ServerObserved)
            {
                shared.emit_snapshot(snapshot, FactSource::ServerObserved);
            }
        }
        Event::Disconnect(_reason) => {
            // This channel has no attempt token. Canonical DisconnectEvent
            // admission already ran before the listener copied this event.
            // Disconnect 会由 Azalea 同步移除本地玩家的运动组件；此处只
            // 更新运行时状态，不再向已失效的实体投递 walk/jump/crouch 消息。
        }
        Event::ConnectionFailed(_error) => {
            // The canonical ECS source already admitted and closed this
            // exact entity/epoch.  A high-level event without that bounded
            // hand-off is source-less and must have no lifecycle side effect.
            if !shared.take_canonical_connection_failure_followup_with_token(
                bot.entity,
                bot.attempt_token(),
            ) {
                return;
            }
            // ConnectionFailed 不一定伴随单独的 swarm disconnect；显式断开让
            // 统一的 close/reconnect 分支接管，不把内部错误泄漏成旧 error kind。
            let _ = shared.with_disconnect_admission(|| bot.disconnect());
        }
        Event::AddPlayer(info) => {
            if !shared.client_is_current_owner(&bot) {
                return;
            }
            shared.emit_if_running(
                FactSource::ServerObserved,
                BackendEventPayload::PlayerList(ContractProtocolPlayerListEvent::Add {
                    uuid: info.uuid.to_string(),
                    username: info.profile.name,
                }),
            );
        }
        Event::RemovePlayer(info) => {
            if !shared.client_is_current_owner(&bot) {
                return;
            }
            shared.emit_if_running(
                FactSource::ServerObserved,
                BackendEventPayload::PlayerList(ContractProtocolPlayerListEvent::Remove {
                    uuid: info.uuid.to_string(),
                    username: info.profile.name,
                }),
            );
        }
        Event::UpdatePlayer(info) => {
            if !shared.client_is_current_owner(&bot) {
                return;
            }
            shared.emit_if_running(
                FactSource::ServerObserved,
                BackendEventPayload::PlayerList(ContractProtocolPlayerListEvent::Update {
                    uuid: info.uuid.to_string(),
                    username: info.profile.name,
                }),
            );
        }
        Event::ReceiveChunk(position) => {
            // ChunkLoaded is produced once from the completed canonical
            // ReceiveChunkEvent boundary.  The high-level Event::ReceiveChunk
            // is retained for Azalea's other high-level behavior but must not
            // produce a second observation envelope.
            let _ = position;
        }
        Event::Tick => {
            if shared.client_is_current_owner(&bot)
                && shared.command_execution_allowed()
                && shared.ready.load(Ordering::Acquire)
            {
                let tick = shared.tick_revision.fetch_add(1, Ordering::AcqRel);
                if tick % 5 != 0 {
                    return;
                }
                if let Some(snapshot) =
                    shared.refresh_snapshot(&bot, false, FactSource::ClientPredicted)
                {
                    // Tick 中的 Position/Physics 可能是 Azalea 本地物理预测；
                    // 不把它作为服务端事实发出，服务端事件仍单独保留为 observed。
                    shared.emit_snapshot(snapshot, FactSource::ClientPredicted);
                }
            }
        }
        _ => {}
    }
}

async fn handle_swarm(swarm: Swarm, event: SwarmEvent, state: SwarmState) {
    let shared = state.shared;
    if matches!(event, SwarmEvent::Init) {
        if !shared.set_swarm(swarm.clone()) {
            return;
        }
    }
    if let SwarmEvent::Disconnect(account, join_opts, attempt_token) = event {
        if !shared.claim_reconnect_with_token(attempt_token) {
            return;
        }
        if shared.stopping.load(Ordering::Acquire) {
            shared.reconnect_pending.store(false, Ordering::Release);
            return;
        }

        // SwarmEvent::Disconnect 是重连状态机的兜底边界：azalea 在复用
        // LocalPlayerEvents 时可能没有再发出 Event::Disconnect。
        let close = shared.mark_disconnected(None);
        if shared.stopping.load(Ordering::Acquire) || close.deliberate {
            shared.reconnect_pending.store(false, Ordering::Release);
            return;
        }
        if !close.retryable || !shared.config.reconnect.enabled {
            shared.emit_faulted(shared.failure_for_close(&close));
            shared.request_shutdown();
            shared.reconnect_pending.store(false, Ordering::Release);
            return;
        }
        let Some(delay) = shared.emit_reconnect_scheduled(&close) else {
            shared.finish_reconnect_attempt(0);
            return;
        };
        let reconnect_cancel = shared.reconnect_cancel.clone();
        tokio::task::spawn_local(async move {
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = reconnect_cancel.notified() => {
                    shared.finish_reconnect_attempt(0);
                    return;
                }
            }
            let Some(token) = shared.admit_reconnect_attempt() else {
                shared.finish_reconnect_attempt(0);
                return;
            };
            if !shared.reconnect_add_is_allowed(token) {
                shared.finish_reconnect_attempt(token);
                return;
            }
            let state = BotState {
                shared: shared.clone(),
            };
            // Do not drop add_with_opts after its first poll: it may already
            // have started Client::start_client. Stop invalidates the token
            // and exits the swarm; once this future returns, an invalid token
            // explicitly disconnects the returned client as the final guard.
            let client = swarm.add_with_opts(&account, state, &join_opts).await;
            // Bind while the reservation/token/stop admission is held by the
            // backend.  Do not perform a separate read-then-bind check: a
            // stop or token transition between those reads must invalidate
            // the returned client instead of installing its owner.
            let bound = shared
                .bind_reconnect_return_with_token(token, client.entity, client.attempt_token())
                .is_some();
            if !bound {
                client.disconnect();
            } else {
                let _ = shared.set_active_client_if_current(&client);
            }
            shared.finish_reconnect_attempt(token);
        });
    }
}

/// 启动 M1 连接/登录事件流，并在真实断线后按自有状态机再次加入。
pub async fn run(config: RunConfig) -> Result<(), Box<dyn Error + Send + Sync>> {
    let handle = RuntimeHandle::new(config.clone());
    run_with_handle(handle, config).await
}

/// 使用外部句柄启动运行时，供主仓库适配层调用 `snapshot/subscribe/motor/sendChat`。
pub async fn run_with_handle(
    handle: RuntimeHandle,
    config: RunConfig,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    validate_run_config(&config)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let shared = handle.shared.clone();
    shared.timers_enabled.store(true, Ordering::Release);
    if !shared.begin_connection_attempt() {
        return Ok(());
    }
    let account = Account::offline(&config.username);
    let socket: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    let address = ResolvedAddr {
        server: ServerAddr::from(socket),
        socket,
    };
    let shutdown = shared.shutdown.clone();
    let bot_state = BotState {
        shared: shared.clone(),
    };
    let swarm_state = SwarmState { shared };
    let plugins = (
        DefaultPlugins.build(),
        DefaultBotPlugins
            .build()
            .disable::<AutoRespawnPlugin>()
            .disable::<AcceptResourcePacksPlugin>()
            .disable::<AutoReconnectPlugin>(),
        EntityProducerPlugin,
        BlockSoundProducerPlugin,
        ServerPositionCorrectionPlugin,
        RuntimeShutdownPlugin,
        DefaultSwarmPlugins,
    );
    let start = SwarmBuilder::new_without_plugins()
        .add_plugins(plugins)
        .set_handler(handle_client)
        .set_swarm_handler(handle_swarm)
        .set_swarm_state(swarm_state)
        .add_account_with_state(account, bot_state)
        .reconnect_after(None)
        .start(&address);
    tokio::select! {
        _ = start => {}
        _ = shutdown.notified() => {
            // 先让 SwarmBuilder 自己尝试清理；若其内部仍在等待 AppExit，
            // 丢弃 start future 后由 Tokio runtime 回收剩余任务。
        }
    }
    Ok(())
}

pub(super) fn validate_run_config(config: &RunConfig) -> Result<(), String> {
    if config.host.trim().is_empty() {
        return Err("服务器 host 不能为空".to_owned());
    }
    if config.port == 0 {
        return Err("服务器 port 不能为 0".to_owned());
    }
    if config.username.is_empty()
        || config.username.len() > 16
        || !config
            .username
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err("offline 用户名必须是 1–16 个 ASCII 字母、数字或下划线".to_owned());
    }
    if config.world_id.trim().is_empty() {
        return Err("world_id 不能为空".to_owned());
    }
    if config.timeouts.connect_ms == 0
        || config.timeouts.login_ms == 0
        || config.timeouts.spawn_ms == 0
        || config.timeouts.stop_ms == 0
    {
        return Err("transport timeout 必须大于 0".to_owned());
    }
    if !config.reconnect.multiplier.is_finite() || config.reconnect.multiplier < 1.0 {
        return Err("reconnect multiplier 必须是有限且不小于 1 的数".to_owned());
    }
    if !config.reconnect.jitter_ratio.is_finite()
        || !(0.0..=1.0).contains(&config.reconnect.jitter_ratio)
    {
        return Err("reconnect jitter ratio 必须在 0 到 1 之间".to_owned());
    }
    Ok(())
}
