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
        ParticipantRuntimeError::Source(ParticipantSourceError::MissingLight) => {
            "opening_frame_light_missing"
        }
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
