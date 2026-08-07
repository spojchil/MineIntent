//! 无状态辅助函数：scope 判定、后端事件分类、载荷构造与格式化。

use super::*;

pub(super) fn startup_scope(facts: &MinecraftFrameFacts) -> Result<ParticipantScope, &'static str> {
    let snapshot = &facts.snapshot;
    if snapshot.process_session_id.is_empty()
        || snapshot.connection_attempt_id.is_empty()
        || snapshot.world.world_id.is_empty()
        || snapshot.world.dimension.is_empty()
        || snapshot.captured_at.is_empty()
    {
        return Err("backend startup snapshot is missing scope identity");
    }
    Ok(ParticipantScope::new(
        snapshot.process_session_id.clone(),
        snapshot.connection_epoch,
        snapshot.world.world_id.clone(),
        Some(snapshot.world.dimension.clone()),
    ))
}

pub(super) fn merge_active_cleanup(left: &mut Cleanup, right: Cleanup) {
    left.required |= right.required;
    if left.cancellation.is_none() {
        left.cancellation = right.cancellation;
    }
    if left.abort.is_none() {
        left.abort = right.abort;
    }
    if left.start_gate.is_none() {
        left.start_gate = right.start_gate;
    }
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(super) fn scope_is_stale(
    state: &RuntimeState,
    scope: &ParticipantScope,
    allow_same_epoch_transition: bool,
) -> bool {
    if state
        .retired_process_sessions
        .contains(&namespace_digest(&scope.process_session_id))
    {
        return true;
    }
    if let Some(closed) = state.closed_scope.as_ref() {
        if scope.process_session_id == closed.process_session_id
            && scope.connection_epoch <= closed.connection_epoch
        {
            return true;
        }
    }
    let Some(current) = state.scope.as_ref() else {
        return false;
    };
    scope.process_session_id == current.process_session_id
        && (scope.connection_epoch < current.connection_epoch
            || (scope.connection_epoch == current.connection_epoch
                && scope != current
                && !allow_same_epoch_transition))
}

pub(super) fn retire_process_session(state: &mut RuntimeState, process_session_id: &str) {
    state
        .retired_process_sessions
        .insert(namespace_digest(process_session_id));
}

pub(super) fn as_chat_event(
    event: &BackendEventEnvelope,
) -> Option<BackendEventEnvelope<ProtocolChatEvent>> {
    let BackendEventEnvelope {
        protocol,
        id,
        kind,
        occurred_at,
        process_session_id,
        connection_epoch,
        connection_attempt_id,
        world_id,
        dimension,
        source,
        payload,
    } = event.clone();
    let BackendEventPayload::Chat(payload) = payload else {
        return None;
    };
    Some(BackendEventEnvelope {
        protocol,
        id,
        kind,
        occurred_at,
        process_session_id,
        connection_epoch,
        connection_attempt_id,
        world_id,
        dimension,
        source,
        payload,
    })
}

pub(super) fn backend_event_type(event: &BackendEventEnvelope) -> &'static str {
    match event.kind {
        BackendEventKind::Lifecycle => "lifecycle",
        BackendEventKind::SelfState => "self",
        BackendEventKind::World => "world",
        BackendEventKind::Entity => "entity",
        BackendEventKind::Block => "block",
        BackendEventKind::Sound => "sound",
        BackendEventKind::Chat => "player_chat",
        BackendEventKind::PlayerList => "player_list",
        BackendEventKind::SnapshotChanged => "snapshot_changed",
        BackendEventKind::Overflow => "overflow",
    }
}

pub(super) fn backend_event_summary(event: &BackendEventEnvelope) -> String {
    match &event.payload {
        BackendEventPayload::Chat(_) => "player_chat_not_addressed".to_owned(),
        BackendEventPayload::Lifecycle(payload) => {
            format!("lifecycle:{}", lifecycle_name(payload))
        }
        BackendEventPayload::Sound(_) => "sound_fact".to_owned(),
        BackendEventPayload::SelfState(_) => "self_state_fact".to_owned(),
        BackendEventPayload::World(_) => "world_fact".to_owned(),
        BackendEventPayload::Entity(_) => "entity_fact".to_owned(),
        BackendEventPayload::Block(_) => "block_fact".to_owned(),
        BackendEventPayload::PlayerList(_) => "player_list_fact".to_owned(),
        BackendEventPayload::SnapshotChanged(_) => "snapshot_changed_fact".to_owned(),
        BackendEventPayload::Overflow(_) => "overflow_fact".to_owned(),
    }
}

pub(super) fn lifecycle_name(payload: &BackendLifecyclePayload) -> &'static str {
    match payload {
        BackendLifecyclePayload::ConnectionRequested { .. } => "connection_requested",
        BackendLifecyclePayload::TransportConnected => "transport_connected",
        BackendLifecyclePayload::LoggedIn { .. } => "logged_in",
        BackendLifecyclePayload::Ready { .. } => "ready",
        BackendLifecyclePayload::Died => "died",
        BackendLifecyclePayload::RespawnTransitionStarted { .. } => "respawn_transition_started",
        BackendLifecyclePayload::Respawned { .. } => "respawned",
        BackendLifecyclePayload::DimensionChanged { .. } => "dimension_changed",
        BackendLifecyclePayload::ReconnectScheduled { .. } => "reconnect_scheduled",
        BackendLifecyclePayload::ConnectionClosed { .. } => "connection_closed",
        BackendLifecyclePayload::Faulted { .. } => "faulted",
        BackendLifecyclePayload::Stopped { .. } => "stopped",
    }
}

pub(super) fn backend_event_is_terminal(event: &BackendEventEnvelope) -> bool {
    matches!(
        event.payload,
        BackendEventPayload::Lifecycle(
            BackendLifecyclePayload::Faulted { .. } | BackendLifecyclePayload::Stopped { .. }
        )
    )
}

pub(super) fn backend_event_is_control(event: &BackendEventEnvelope) -> bool {
    matches!(
        event.payload,
        BackendEventPayload::Lifecycle(_) | BackendEventPayload::Overflow(_)
    )
}

pub(super) fn backend_terminal_lifecycle(event: &BackendEventEnvelope) -> ParticipantLifecycle {
    match &event.payload {
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::Faulted { .. }) => {
            ParticipantLifecycle::Faulted
        }
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::Stopped { .. }) => {
            ParticipantLifecycle::Stopped
        }
        _ => ParticipantLifecycle::Stopped,
    }
}

pub(super) fn backend_event_is_scope_invalidation(event: &BackendEventEnvelope) -> bool {
    matches!(
        event.payload,
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionClosed { .. })
    )
}

pub(super) fn backend_event_is_reconnect_control(event: &BackendEventEnvelope) -> bool {
    matches!(
        event.payload,
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::ReconnectScheduled { .. })
    )
}

pub(super) fn backend_event_is_connection_request(event: &BackendEventEnvelope) -> bool {
    matches!(
        event.payload,
        BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionRequested { .. })
    )
}

pub(super) fn backend_event_is_scope_transition(event: &BackendEventEnvelope) -> bool {
    matches!(
        event.payload,
        BackendEventPayload::Lifecycle(
            BackendLifecyclePayload::ConnectionRequested { .. }
                | BackendLifecyclePayload::LoggedIn { .. }
                | BackendLifecyclePayload::Respawned { .. }
                | BackendLifecyclePayload::DimensionChanged { .. }
        )
    )
}

pub(super) fn same_chat_identity(left: &AgentChatMessageV5, right: &AgentChatMessageV5) -> bool {
    left.username == right.username && left.text == right.text && left.at == right.at
}

/// 这条后端事件该不该进事实队列。
///
/// 事实队列是**推送通道**：装它没人会主动去问、又必须知道的事。世界长什么样
/// 不走这里——A06 与 W08c 都写明世界信息经视口到达 AI。声音有自己的
/// `sound.recentSounds`，血量食物有 `status`，聊天有 `chat.items`，
/// 这些每一帧都在，不需要再复制一份进事实队列。
///
/// 原型（`src/participant/runtime.ts`）整份只有两处 `#pushPending`：
/// `participant.started` 和 `self.health.dropped`。实体移动、方块变化、声音
/// **从来不是事实**。移植时改成了「每条后端事件都记一条事实」，后果实测如下：
///
/// 2026-08-06 实盘 130 秒，Paper 实服 + DeepSeek 真模型——
/// 模型收到 128 条 `entity`（摘要一律是字面量 "entity_fact"，零信息），
/// 外加一条「29777 pending events omitted」。被淹掉的类型里含
/// `participant.started`：唯一有意义的那条事实，被噪声挤了出去。
/// 队列不是太小，是灌进去的东西根本不该在里面。
///
/// 判据是「玩家能不能感知到，且别的通道给不了」：
///
/// - 生命周期（死亡、复活、断线）—— 玩家当然感知得到，别处没有
/// - 维度/游戏模式变化 —— 同上
/// - 别人说了话但没点名 —— 玩家听得见；`chat.items` 也有，但它是有界窗口，
///   事实通道保留丢失位置的语义（NEW-11），两者不重复
/// - 玩家进出世界 —— 原版会在聊天里显示「X joined the game」
/// - 溢出标记 —— 它本身就是「你漏了什么」的载体，必须留
///
/// 挡掉的：
///
/// - 实体与方块 —— 视口的活（A06/W08c），且是量级最大的两类
/// - 声音 —— 已经在 `sound.recentSounds` 里，重复投递
/// - `player_list_update` —— tab 列表的延迟刷新，玩家根本感知不到
/// - `SelfState` —— 目前只有 `ServerPositionCorrection`，是协议层的回拉纠正，
///   不是玩家能察觉的事件；位置本身每帧都在 `pose` 里
/// - `SnapshotChanged` —— 内部记账
/// 这条后端事件该不该占用参与者队列的一个槽。
///
/// 与 [`backend_event_is_fact`] 是两件事：那条决定「记不记成事实」，这条决定
/// 「进不进队列」。此前只有前者，于是实体与方块照样入队、占槽、被丢，并产生
/// 队列自己的溢出标记——它们在队列里两条出路都是死的：
///
/// - 成为事实：`backend_event_is_fact` 已经挡住
/// - 触发唤醒：`evaluate_backend_wake` 只认聊天，非聊天一律 `None`
///
/// 代价是真实的，不只是噪声。第四层的车道划分是「有 wake 或 scope_control 走
/// control(8)，否则走 ordinary(16)」，而 `scope_control` 只含 Lifecycle 与
/// Overflow。**没点名的玩家聊天因此和实体流水抢同一条 16 格车道**，而实盘两分
/// 钟的实体摄入是 59945 条。实盘确实观察到 `event_type=player_chat` 被丢，
/// 而聊天不可重建（撞 `产品.md` W08b）。
///
/// 声音**保留**：它现在既不是事实也不唤醒，但按维护者裁定，声音将来要作唤醒源
/// 走事件通道进帧，与聊天同路。提前挡掉会让那条通道届时无处接。
///
/// 实体与方块不保留：世界长什么样按 A06/W08c 经视口到达 AI，是拉取式的，
/// 丢一条「方块变了」不影响下一次看。
pub(super) fn backend_event_enters_queue(event: &BackendEventEnvelope) -> bool {
    !matches!(
        event.payload,
        BackendEventPayload::Entity(_) | BackendEventPayload::Block(_)
    )
}

pub(super) fn backend_event_is_fact(event: &BackendEventEnvelope) -> bool {
    match &event.payload {
        BackendEventPayload::Lifecycle(_)
        | BackendEventPayload::World(_)
        | BackendEventPayload::Chat(_) => true,
        BackendEventPayload::PlayerList(payload) => !matches!(
            payload,
            mineintent_contracts::minecraft::ProtocolPlayerListEvent::Update { .. }
        ),
        // 后端的溢出标记只有在「丢掉的东西本来会成为事实」时才有意义。后端
        // 自己的队列满时丢的绝大多数是实体流水，而实体流水根本不进事实通道
        // ——为它报一条「你漏了什么」，漏掉的却是本来就不该在这儿的东西。
        BackendEventPayload::Overflow(payload) => {
            payload.dropped_kinds.iter().any(backend_kind_is_fact)
        }
        BackendEventPayload::Entity(_)
        | BackendEventPayload::Block(_)
        | BackendEventPayload::Sound(_)
        | BackendEventPayload::SelfState(_)
        | BackendEventPayload::SnapshotChanged(_) => false,
    }
}

/// 只看种类的粗判据，供溢出标记使用：标记里没有载荷，判不了
/// `player_list` 到底是进出还是延迟刷新，从宽算作会成为事实。
fn backend_kind_is_fact(kind: &BackendEventKind) -> bool {
    match kind {
        BackendEventKind::Lifecycle
        | BackendEventKind::World
        | BackendEventKind::Chat
        | BackendEventKind::PlayerList => true,
        BackendEventKind::Entity
        | BackendEventKind::Block
        | BackendEventKind::Sound
        | BackendEventKind::SelfState
        | BackendEventKind::SnapshotChanged
        | BackendEventKind::Overflow => false,
    }
}

pub(super) fn backend_fact_type(event: &BackendEventEnvelope) -> &'static str {
    if matches!(event.payload, BackendEventPayload::Chat(_)) {
        "player_chat_not_addressed"
    } else {
        backend_event_type(event)
    }
}

pub(crate) fn safe_fact_event_type(event_type: &str) -> String {
    if event_type == "player_chat" {
        "player_chat_fact".to_owned()
    } else {
        event_type.to_owned()
    }
}

pub(super) fn event_payload(item: &WorkItem) -> JsonObject {
    let wake = item.wake.as_ref().map(|wake| {
        json!({
            "kind": "player_chat",
            "addressed": true,
            "sender": wake.trigger.sender.username,
        })
    });
    let mut value = json!({
        "id": item.event_id,
        "admissionTicket": item.ticket,
        "ordinal": item.ordinal,
        "occurredAt": item.occurred_at,
        "eventType": item.event_type,
        "scope": {
            "processSessionId": item.scope.process_session_id,
            "connectionEpoch": item.scope.connection_epoch,
            "worldId": item.scope.world_id,
            "dimension": item.scope.dimension,
        },
        "wake": wake,
    });
    if let (Some(object), Some(overflow)) = (value.as_object_mut(), item.overflow.as_ref()) {
        object.insert(
            "overflow".to_owned(),
            json!({
                "droppedCount": overflow.dropped_count,
                "droppedTypes": overflow.dropped_types,
            }),
        );
    }
    value.as_object().cloned().unwrap_or_default()
}

pub(super) fn bounded_summary(value: impl AsRef<str>) -> String {
    value.as_ref().chars().take(256).collect()
}

pub(super) fn add_overflow_type(types: &mut Vec<String>, event_type: &str) {
    if types.iter().any(|known| known == event_type)
        || types.len() >= PARTICIPANT_MAX_OVERFLOW_TYPES
    {
        return;
    }
    types.push(bounded_summary(event_type));
}

pub(super) fn add_pending_omitted_type(types: &mut Vec<String>, event_type: &str) {
    if types.iter().any(|known| known == event_type)
        || types.len() >= PARTICIPANT_MAX_PENDING_OMITTED_TYPES
    {
        return;
    }
    types.push(bounded_summary(event_type));
}

pub(super) fn namespace_digest(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(super) fn base36_u64(mut value: u64) -> String {
    pub(super) const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".to_owned();
    }
    let mut digits = [0_u8; 13];
    let mut index = digits.len();
    while value > 0 {
        index -= 1;
        digits[index] = DIGITS[(value % 36) as usize];
        value /= 36;
    }
    String::from_utf8(digits[index..].to_vec()).expect("base36 digits are valid UTF-8")
}

pub(super) fn utc_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let hour = day_seconds / 3_600;
    let minute = (day_seconds % 3_600) / 60;
    let second = day_seconds % 60;

    // Civil-from-days conversion, using the proleptic Gregorian calendar.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096).div_euclid(365);
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let month = (5 * doy + 2).div_euclid(153);
    let day = doy - (153 * month + 2).div_euclid(5) + 1;
    let month = month + if month < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// 摘要保持与错误种类一一对应，**不携带内层文本**：内层可能嵌入私聊内容，
/// 而失败摘要会进 journal 与 debug 面（既有隐私回归钉住了这条）。
/// 排障所需的「为什么」由开发者模式在进程侧补齐，不放进持久记录。
pub(super) fn handler_summary(error: &ParticipantRuntimeError) -> String {
    match error {
        ParticipantRuntimeError::InvalidConfig(_) => {
            "participant runtime configuration invalid".to_owned()
        }
        ParticipantRuntimeError::Frame(_) => "frame assembly failed".to_owned(),
        ParticipantRuntimeError::Source(_) => "opening frame source failed".to_owned(),
        ParticipantRuntimeError::Memory(_) => "memory read failed".to_owned(),
        ParticipantRuntimeError::Handler(_) => "participant handler failed".to_owned(),
        ParticipantRuntimeError::Backend(_) => "backend operation failed".to_owned(),
        ParticipantRuntimeError::QueueClosed => "participant event queue closed".to_owned(),
        ParticipantRuntimeError::NotStarted => "participant runtime not started".to_owned(),
        ParticipantRuntimeError::AlreadyStarted => "participant runtime already started".to_owned(),
        ParticipantRuntimeError::Stopped => "participant runtime stopped".to_owned(),
        ParticipantRuntimeError::Faulted => "participant runtime faulted".to_owned(),
    }
}

pub(super) fn handler_code(error: &ParticipantRuntimeError) -> &'static str {
    match error {
        ParticipantRuntimeError::InvalidConfig(_) => "invalid_runtime_configuration",
        ParticipantRuntimeError::Frame(_) => "frame_assembly_failed",
        ParticipantRuntimeError::Source(_) => "opening_frame_source_failed",
        ParticipantRuntimeError::Memory(_) => "memory_read_failed",
        ParticipantRuntimeError::Handler(_) => "participant_handler_failed",
        ParticipantRuntimeError::Backend(_) => "backend_failed",
        ParticipantRuntimeError::QueueClosed => "queue_closed",
        ParticipantRuntimeError::NotStarted => "not_started",
        ParticipantRuntimeError::AlreadyStarted => "already_started",
        ParticipantRuntimeError::Stopped => "stopped",
        ParticipantRuntimeError::Faulted => "faulted",
    }
}

pub(super) fn startup_lifecycle_error(lifecycle: ParticipantLifecycle) -> ParticipantRuntimeError {
    match lifecycle {
        ParticipantLifecycle::Stopped | ParticipantLifecycle::Stopping => {
            ParticipantRuntimeError::Stopped
        }
        ParticipantLifecycle::Faulted => ParticipantRuntimeError::Faulted,
        ParticipantLifecycle::Created | ParticipantLifecycle::Running => {
            ParticipantRuntimeError::Handler("participant startup did not complete".to_owned())
        }
    }
}

pub(super) fn is_recoverable_wake_error(error: &ParticipantRuntimeError) -> bool {
    matches!(
        error,
        ParticipantRuntimeError::Source(_)
            | ParticipantRuntimeError::Frame(_)
            | ParticipantRuntimeError::Memory(_)
    )
}

pub(super) fn failure_source(error: &ParticipantRuntimeError) -> ParticipantFailureSource {
    match error {
        ParticipantRuntimeError::Source(_) | ParticipantRuntimeError::Frame(_) => {
            ParticipantFailureSource::Source
        }
        ParticipantRuntimeError::Memory(_) => ParticipantFailureSource::Runtime,
        ParticipantRuntimeError::Backend(_) => ParticipantFailureSource::Backend,
        ParticipantRuntimeError::Handler(_) | ParticipantRuntimeError::QueueClosed => {
            ParticipantFailureSource::Runtime
        }
        ParticipantRuntimeError::InvalidConfig(_) => ParticipantFailureSource::Runtime,
        ParticipantRuntimeError::NotStarted
        | ParticipantRuntimeError::AlreadyStarted
        | ParticipantRuntimeError::Stopped
        | ParticipantRuntimeError::Faulted => ParticipantFailureSource::Runtime,
    }
}

pub(super) fn is_normal_agent_error(error: &AgentError) -> bool {
    matches!(
        error.code,
        AgentErrorCode::RunCancelled
            | AgentErrorCode::DeadlineExceeded
            | AgentErrorCode::ScopeInvalid
    )
}

#[cfg(test)]
mod fact_channel_tests {
    use super::*;
    use mineintent_contracts::minecraft::{
        BackendEventProtocol, BackendOverflowPayload, ChatPosition, FactSource, HeardSoundType,
        OverflowType, ProtocolBlockEvent, ProtocolChatEvent, ProtocolEntityEvent,
        ProtocolPlayerListEvent, ProtocolSelfEvent, ProtocolSnapshotChangedEvent,
        ProtocolSoundPayload, ProtocolSoundSource, ProtocolWorldEvent, RelativeMovementFlags,
        Vec3Value,
    };

    fn envelope(payload: BackendEventPayload) -> BackendEventEnvelope {
        BackendEventEnvelope {
            protocol: BackendEventProtocol::V2,
            id: "event-1".to_owned(),
            kind: payload.kind(),
            occurred_at: "2026-08-06T00:00:00Z".to_owned(),
            process_session_id: "process".to_owned(),
            connection_epoch: 1,
            connection_attempt_id: "attempt".to_owned(),
            world_id: "world".to_owned(),
            dimension: Some("minecraft:overworld".to_owned()),
            source: FactSource::ServerObserved,
            payload,
        }
    }

    fn origin() -> Vec3Value {
        Vec3Value {
            x: 0.0,
            y: 64.0,
            z: 0.0,
        }
    }

    /// 世界长什么样经视口到达（A06 / W08c），不从事实通道灌。这几类正是
    /// 2026-08-06 实盘里 130 秒堆出 29777 条丢弃的来源。
    #[test]
    fn world_traffic_is_not_a_fact() {
        let denied = [
            BackendEventPayload::Entity(ProtocolEntityEvent::Animation {
                entity_key: "1:7".to_owned(),
                animation: "swing_main_hand".to_owned(),
            }),
            BackendEventPayload::Block(ProtocolBlockEvent::Updated {
                old_block: None,
                new_block: None,
            }),
            BackendEventPayload::Sound(ProtocolSoundPayload {
                event_type: HeardSoundType::Heard,
                sound_key: "minecraft:entity.zombie.step".to_owned(),
                sound_name: None,
                sound_id: None,
                category: None,
                source_position: origin(),
                volume: 1.0,
                pitch: 1.0,
                protocol_source: ProtocolSoundSource::SoundEffect,
            }),
            BackendEventPayload::SnapshotChanged(ProtocolSnapshotChangedEvent {
                group: "world".to_owned(),
                snapshot_revision: 3,
            }),
            BackendEventPayload::SelfState(ProtocolSelfEvent::ServerPositionCorrection {
                teleport_id: 1,
                position: origin(),
                velocity: Vec3Value {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                yaw: 0.0,
                pitch: 0.0,
                relative: RelativeMovementFlags {
                    x: false,
                    y: false,
                    z: false,
                    yaw: false,
                    pitch: false,
                    delta_x: false,
                    delta_y: false,
                    delta_z: false,
                    rotate_delta: false,
                },
            }),
        ];
        for payload in denied {
            let event = envelope(payload);
            assert!(
                !backend_event_is_fact(&event),
                "{:?} 不该进事实队列",
                event.kind
            );
        }
    }

    /// 推送通道该装的：玩家感知得到、且别的通道给不了的东西。
    #[test]
    fn perceivable_events_without_another_channel_stay_facts() {
        let allowed = [
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Died),
            BackendEventPayload::World(ProtocolWorldEvent::GameChanged {
                dimension: Some("minecraft:the_nether".to_owned()),
                game_mode: None,
            }),
            BackendEventPayload::Chat(ProtocolChatEvent {
                plain_text: "随口说一句".to_owned(),
                sender_username: Some("Alice".to_owned()),
                verified: None,
                position: Some(ChatPosition::Chat),
            }),
            BackendEventPayload::Overflow(BackendOverflowPayload {
                event_type: OverflowType::Overflow,
                dropped_count: 5,
                dropped_kinds: vec![BackendEventKind::Lifecycle],
            }),
        ];
        for payload in allowed {
            let event = envelope(payload);
            assert!(
                backend_event_is_fact(&event),
                "{:?} 该进事实队列",
                event.kind
            );
        }
    }

    /// 进出世界原版会在聊天里显示；tab 列表的延迟刷新玩家根本感知不到。
    #[test]
    fn player_list_keeps_join_and_leave_but_drops_latency_refreshes() {
        let join = envelope(BackendEventPayload::PlayerList(
            ProtocolPlayerListEvent::Add {
                uuid: "u".to_owned(),
                username: "Alice".to_owned(),
            },
        ));
        let leave = envelope(BackendEventPayload::PlayerList(
            ProtocolPlayerListEvent::Remove {
                uuid: "u".to_owned(),
                username: "Alice".to_owned(),
            },
        ));
        let refresh = envelope(BackendEventPayload::PlayerList(
            ProtocolPlayerListEvent::Update {
                uuid: "u".to_owned(),
                username: "Alice".to_owned(),
            },
        ));
        assert!(backend_event_is_fact(&join));
        assert!(backend_event_is_fact(&leave));
        assert!(!backend_event_is_fact(&refresh));
    }

    /// 后端队列满时丢的绝大多数是实体流水。为它报一条「你漏了什么」，漏掉的
    /// 却是本来就不该进事实通道的东西——2026-08-06 实盘里这样的标记有 30 条。
    #[test]
    fn an_overflow_marker_about_world_traffic_only_is_not_a_fact() {
        let world_traffic_only = envelope(BackendEventPayload::Overflow(BackendOverflowPayload {
            event_type: OverflowType::Overflow,
            dropped_count: 900,
            dropped_kinds: vec![BackendEventKind::Entity, BackendEventKind::Block],
        }));
        assert!(!backend_event_is_fact(&world_traffic_only));

        let lost_a_real_fact = envelope(BackendEventPayload::Overflow(BackendOverflowPayload {
            event_type: OverflowType::Overflow,
            dropped_count: 900,
            dropped_kinds: vec![BackendEventKind::Entity, BackendEventKind::Chat],
        }));
        assert!(backend_event_is_fact(&lost_a_real_fact));
    }
}
