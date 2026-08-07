//! Canonical Azalea packet/ECS producers and their source-admission adapters.

use super::*;

/// Publishes the entity packet facts after Azalea's packet handler has
/// applied them to ECS.  RemoveEntities is intentionally handled from the
/// shadow cache only because its handler has already despawned the entities.
pub(super) struct EntityProducerPlugin;

impl Plugin for EntityProducerPlugin {
    fn build(&self, app: &mut App) {
        // These readers sit on the canonical ECS side of Azalea's adapters.
        // The high-level Event channel has no attempt token when a reconnect
        // reuses an entity, so lifecycle admission is made before those
        // listeners copy events to LocalPlayerEvents.
        app.add_systems(
            Update,
            (
                admit_canonical_join_source.before(azalea::join::handle_start_join_server_event),
                admit_canonical_disconnect_source
                    .before(azalea::events::disconnect_listener)
                    .before(azalea::join::handle_start_join_server_event),
                admit_canonical_connection_failure_source
                    .after(azalea::join::poll_create_connection_task)
                    .before(azalea::events::connection_failed_listener),
            ),
        )
        // `read_packets` applies each raw packet to ECS and then writes the
        // ReceiveGamePacketEvent batch. Reading immediately after it in the
        // same PreUpdate keeps the source epoch tied to that ordered batch;
        // no packet message is allowed to sit across an attempt transition.
        .add_systems(
            azalea::app::PreUpdate,
            produce_entity_packet_events.after(azalea::connection::read_packets),
        );
    }
}

// 这里曾有 BlockSoundProducerPlugin：它把区块加载与方块更新转成
// Block 事件，并顺带用 CanonicalPacketSourceMetadata 给每个 vendor 队列项打来源戳。
//
// 全部删除。方块更新那个 system 尤其要说明：它 `std::mem::take` 走 azalea 的
// `QueuedServerBlockUpdates`，然后**自己把方块写进世界**（注释原话是「Match
// Azalea's vendor handler exactly」），跑在 vendor handler 之前把队列清空。删掉之后
// vendor handler 拿到满队列照常处理——职责交还 azalea，我们不再复制它。
// （已确认没有 disable 掉 azalea 的方块插件：driver.rs 只 disable 了 AutoRespawn /
// AcceptResourcePacks / AutoReconnect 三个。）
//
// 事件本身也不该存在：方块变化在 azalea 的世界模型里已经生效，视口读到的就是变化
// 之后的状态。再发一条「方块变了」，描述的是视口本来就会读到的事，而它在参与者
// 队列里既不成为事实也不能唤醒——只是占槽。判据见
// docs/participant-queue-audit.md 与 docs/vanilla-client-perception.md。

pub(super) fn canonical_sound_name(
    sound: &azalea::registry::Holder<
        azalea::registry::builtin::SoundEvent,
        azalea::core::sound::CustomSound,
    >,
) -> Option<String> {
    let name = match sound {
        azalea::registry::Holder::Direct(custom) => custom.sound_id.to_string(),
        azalea::registry::Holder::Reference(known) => known.to_string(),
    };
    (!name.is_empty()).then_some(name)
}

pub(super) fn canonical_sound_packet(
    packet: &azalea::protocol::packets::game::ClientboundSound,
) -> Option<(String, [f64; 3], f64, f64)> {
    let name = canonical_sound_name(&packet.sound)?;
    let volume = f64::from(packet.volume);
    let pitch = f64::from(packet.pitch);
    if !volume.is_finite() || volume < 0.0 || !pitch.is_finite() {
        return None;
    }
    Some((
        name,
        [
            f64::from(packet.x) / 8.0,
            f64::from(packet.y) / 8.0,
            f64::from(packet.z) / 8.0,
        ],
        volume,
        pitch,
    ))
}

pub(super) fn prove_has_skylight(
    common: &azalea::protocol::packets::common::CommonPlayerSpawnInfo,
    holder: &azalea::local_player::WorldHolder,
) -> Option<bool> {
    let world = holder.shared.read();
    let (_, dimension_data) = common.dimension_type(&world.registries)?;
    let value = dimension_data._extra.get("has_skylight")?;
    match value {
        azalea::protocol::simdnbt::owned::NbtTag::Byte(value) => match *value {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        },
        _ => None,
    }
}

pub(super) fn current_light_geometry(
    holder: &azalea::local_player::WorldHolder,
) -> Option<LightSectionGeometry> {
    let world = holder.shared.read();
    LightSectionGeometry::from_world(&world)
}

pub(super) fn admit_canonical_join_source(
    mut events: MessageReader<azalea::join::StartJoinServerEvent>,
    state: Res<SwarmState>,
) {
    for event in events.read() {
        let source_epoch = state.shared.connection_epoch();
        state
            .shared
            .admit_canonical_join_started_with_token(source_epoch, Some(event.attempt_token));
    }
}

pub(super) fn admit_canonical_disconnect_source(
    mut events: MessageReader<azalea::disconnect::DisconnectEvent>,
    state: Res<SwarmState>,
) {
    for event in events.read() {
        state.shared.admit_canonical_disconnected_source_with_token(
            event.entity,
            event.reason.as_ref().map(ToString::to_string),
            event.attempt_token,
        );
    }
}

pub(super) fn admit_canonical_connection_failure_source(
    mut events: MessageReader<azalea::join::ConnectionFailedEvent>,
    state: Res<SwarmState>,
) {
    for event in events.read() {
        state
            .shared
            .admit_canonical_connection_failed_source_with_token(
                event.entity,
                format!("{:?}", event.error),
                Some(event.attempt_token),
            );
    }
}

pub(super) fn is_admitted_non_local_entity(local_protocol_id: Option<i32>, target_id: i32) -> bool {
    local_protocol_id.is_some_and(|local_id| local_id != target_id)
}

pub(super) fn produce_entity_packet_events(
    mut packets: MessageReader<azalea::packet::game::ReceiveGamePacketEvent>,
    state: Res<SwarmState>,
    local_entities: Query<&azalea::core::entity_id::MinecraftEntityId, With<LocalEntity>>,
    world_holders: Query<&azalea::local_player::WorldHolder>,
) {
    for event in packets.read() {
        let source = state
            .shared
            .admit_canonical_source_with_token(event.entity, Some(event.attempt_token));
        match event.packet.as_ref() {
            azalea::protocol::packets::game::ClientboundGamePacket::Sound(packet) => {
                if let (Some(source), Some((sound_name, source_position, volume, pitch))) =
                    (source, canonical_sound_packet(packet))
                {
                    state.shared.emit_canonical_sound(
                        source,
                        sound_name,
                        source_position,
                        volume,
                        pitch,
                    );
                }
                continue;
            }
            azalea::protocol::packets::game::ClientboundGamePacket::ForgetLevelChunk(packet) => {
                // 只维护光照缓存。曾经这里还发一条 Block(ChunkUnloaded) 事件——
                // 已删：区块卸载在 azalea 的世界模型里已经生效，视口读到的就是
                // 卸载之后的状态（`BlockReadResult::Unloaded`），再发一条事件描述
                // 的是视口本来就会看到的事。
                if let Some(source) = source {
                    let _ = state
                        .shared
                        .remove_light_chunk(source, packet.pos.x, packet.pos.z);
                }
                continue;
            }
            // Azalea's typed packet holder cannot represent an unknown numeric
            // registry reference, and the Mineflayer oracle has no
            // soundEntity listener. Do not fabricate a SoundEffect payload or
            // attach SoundEntity to this seam.
            azalea::protocol::packets::game::ClientboundGamePacket::SoundEntity(_) => continue,
            azalea::protocol::packets::game::ClientboundGamePacket::LightUpdate(packet) => {
                if let (Some(source), Ok(_holder), Some(geometry)) = (
                    source,
                    world_holders.get(event.entity),
                    world_holders
                        .get(event.entity)
                        .ok()
                        .and_then(current_light_geometry),
                ) {
                    let _ = state.shared.apply_light_packet(
                        source,
                        geometry,
                        packet.x,
                        packet.z,
                        &packet.light_data,
                        false,
                    );
                }
                continue;
            }
            azalea::protocol::packets::game::ClientboundGamePacket::LevelChunkWithLight(packet) => {
                if let (Some(source), Ok(_holder), Some(geometry)) = (
                    source,
                    world_holders.get(event.entity),
                    world_holders
                        .get(event.entity)
                        .ok()
                        .and_then(current_light_geometry),
                ) {
                    let _ = state.shared.apply_light_packet(
                        source,
                        geometry,
                        packet.x,
                        packet.z,
                        &packet.light_data,
                        true,
                    );
                }
                continue;
            }
            _ => {}
        }

        let Some(epoch) = source.map(|source| source.epoch) else {
            // Block/chunk metadata was already recorded as None above. The
            // Update systems will still apply vendor payloads but publish no
            // observation from an invalid source.
            continue;
        };

        // Login and Respawn handlers emit WorldLoadedEvent on a separate
        // Bevy stream while connection::read_packets queues all received
        // packet messages until the raw-packet loop ends. These raw packet
        // variants are the authoritative boundary positions: reset the
        // complete scope and publish the packet's dimension before the next
        // packet in this same read batch is admitted.
        match event.packet.as_ref() {
            azalea::protocol::packets::game::ClientboundGamePacket::Login(packet) => {
                let has_skylight = world_holders
                    .get(event.entity)
                    .ok()
                    .and_then(|holder| prove_has_skylight(&packet.common, holder));
                state
                    .shared
                    .reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
                        event.entity,
                        epoch,
                        Some(packet.common.dimension.to_string()),
                        has_skylight,
                    );
                continue;
            }
            azalea::protocol::packets::game::ClientboundGamePacket::Respawn(packet) => {
                let has_skylight = world_holders
                    .get(event.entity)
                    .ok()
                    .and_then(|holder| prove_has_skylight(&packet.common, holder));
                state
                    .shared
                    .reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
                        event.entity,
                        epoch,
                        Some(packet.common.dimension.to_string()),
                        has_skylight,
                    );
                continue;
            }
            _ => {}
        }

        // The packet adapter is fail-closed: without both LocalEntity and its
        // protocol id we cannot prove that an entity packet is not self. The
        // login/respawn boundary above remains processable so it can establish
        // the next ECS scope before local identity is available again.
        let local_protocol_id = local_entities.get(event.entity).ok().map(|id| **id);

        if let azalea::protocol::packets::game::ClientboundGamePacket::UpdateAttributes(packet) =
            event.packet.as_ref()
        {
            if source.is_some_and(|source| {
                local_protocol_id == Some(packet.entity_id.0)
                    && state.shared.apply_armor_packet(source, &packet.values)
            }) {
                // The cache mutation, including an invalid armor result,
                // already happened under the canonical admission.
            }
            continue;
        }

        let admission = state.shared.next_entity_packet_admission();
        match event.packet.as_ref() {
            azalea::protocol::packets::game::ClientboundGamePacket::AddEntity(packet) => {
                if !is_admitted_non_local_entity(local_protocol_id, packet.id.0) {
                    continue;
                }
                let movement = packet.movement.to_vec3();
                let dimensions = EntityDimensions::from(packet.entity_type);
                let entity_type = canonical_entity_type(&packet.entity_type.to_string());
                state.shared.emit_entity_input(
                    event.entity,
                    epoch,
                    EntityProducerInput::Spawn {
                        token: EntityProducerToken::new(
                            epoch,
                            format!("packet-admission:{admission}:add"),
                        ),
                        snapshot: NormalizedEntitySnapshot {
                            identity: EntityIdentity::new(epoch, packet.id.0),
                            entity_type,
                            uuid: Some(packet.uuid.to_string()),
                            name: None,
                            username: None,
                            position: [packet.position.x, packet.position.y, packet.position.z],
                            velocity: [movement.x, movement.y, movement.z],
                            yaw: compact_rotation_degrees(packet.y_rot),
                            pitch: compact_pitch_degrees(packet.x_rot),
                            head_yaw: Some(compact_rotation_degrees(packet.y_head_rot)),
                            width: f64::from(dimensions.width),
                            height: f64::from(dimensions.height),
                            on_ground: false,
                            pose: Some("standing".to_owned()),
                            held_item_name: None,
                            equipment: Vec::new(),
                            valid: true,
                        },
                    },
                );
            }
            azalea::protocol::packets::game::ClientboundGamePacket::MoveEntityPos(packet) => {
                if !is_admitted_non_local_entity(local_protocol_id, packet.entity_id.0) {
                    continue;
                }
                state.shared.emit_entity_input(
                    event.entity,
                    epoch,
                    EntityProducerInput::Move {
                        token: EntityProducerToken::new(
                            epoch,
                            format!("packet-admission:{admission}:move-pos"),
                        ),
                        patch: EntityMovePatch::relative(
                            EntityIdentity::new(epoch, packet.entity_id.0),
                            Some([
                                i64::from(packet.delta.xa),
                                i64::from(packet.delta.ya),
                                i64::from(packet.delta.za),
                            ]),
                            None,
                            packet.on_ground,
                        ),
                    },
                );
            }
            azalea::protocol::packets::game::ClientboundGamePacket::MoveEntityPosRot(packet) => {
                if !is_admitted_non_local_entity(local_protocol_id, packet.entity_id.0) {
                    continue;
                }
                state.shared.emit_entity_input(
                    event.entity,
                    epoch,
                    EntityProducerInput::Move {
                        token: EntityProducerToken::new(
                            epoch,
                            format!("packet-admission:{admission}:move-pos-rot"),
                        ),
                        patch: EntityMovePatch::relative(
                            EntityIdentity::new(epoch, packet.entity_id.0),
                            Some([
                                i64::from(packet.delta.xa),
                                i64::from(packet.delta.ya),
                                i64::from(packet.delta.za),
                            ]),
                            Some([packet.look_direction.y_rot, packet.look_direction.x_rot]),
                            packet.on_ground,
                        ),
                    },
                );
            }
            azalea::protocol::packets::game::ClientboundGamePacket::MoveEntityRot(packet) => {
                if !is_admitted_non_local_entity(local_protocol_id, packet.entity_id.0) {
                    continue;
                }
                state.shared.emit_entity_input(
                    event.entity,
                    epoch,
                    EntityProducerInput::Move {
                        token: EntityProducerToken::new(
                            epoch,
                            format!("packet-admission:{admission}:move-rot"),
                        ),
                        patch: EntityMovePatch::relative(
                            EntityIdentity::new(epoch, packet.entity_id.0),
                            None,
                            Some([packet.look_direction.y_rot, packet.look_direction.x_rot]),
                            packet.on_ground,
                        ),
                    },
                );
            }
            azalea::protocol::packets::game::ClientboundGamePacket::TeleportEntity(packet) => {
                if !is_admitted_non_local_entity(local_protocol_id, packet.id.0) {
                    continue;
                }
                state.shared.emit_entity_input_with_velocity_residual(
                    event.entity,
                    epoch,
                    EntityProducerInput::Move {
                        token: EntityProducerToken::new(
                            epoch,
                            format!("packet-admission:{admission}:teleport"),
                        ),
                        patch: EntityMovePatch::teleport(
                            EntityIdentity::new(epoch, packet.id.0),
                            [
                                packet.change.pos.x,
                                packet.change.pos.y,
                                packet.change.pos.z,
                            ],
                            [
                                f64::from(packet.change.look_direction.y_rot()),
                                f64::from(packet.change.look_direction.x_rot()),
                            ],
                            [packet.relative.x, packet.relative.y, packet.relative.z],
                            [packet.relative.y_rot, packet.relative.x_rot],
                            [
                                packet.change.delta.x,
                                packet.change.delta.y,
                                packet.change.delta.z,
                            ],
                            [
                                packet.relative.delta_x,
                                packet.relative.delta_y,
                                packet.relative.delta_z,
                            ],
                            packet.relative.rotate_delta,
                            packet.on_ground,
                        ),
                    },
                    EntityResidualAction::Clear,
                );
            }
            azalea::protocol::packets::game::ClientboundGamePacket::EntityPositionSync(packet) => {
                if !is_admitted_non_local_entity(local_protocol_id, packet.id.0) {
                    continue;
                }
                state.shared.emit_entity_input_with_velocity_residual(
                    event.entity,
                    epoch,
                    EntityProducerInput::Move {
                        token: EntityProducerToken::new(
                            epoch,
                            format!("packet-admission:{admission}:position-sync"),
                        ),
                        patch: EntityMovePatch::position_sync(
                            EntityIdentity::new(epoch, packet.id.0),
                            [
                                packet.values.pos.x,
                                packet.values.pos.y,
                                packet.values.pos.z,
                            ],
                            [
                                f64::from(packet.values.look_direction.y_rot()),
                                f64::from(packet.values.look_direction.x_rot()),
                            ],
                            [
                                packet.values.delta.x,
                                packet.values.delta.y,
                                packet.values.delta.z,
                            ],
                            packet.on_ground,
                        ),
                    },
                    EntityResidualAction::Update,
                );
            }
            azalea::protocol::packets::game::ClientboundGamePacket::RotateHead(packet) => {
                if !is_admitted_non_local_entity(local_protocol_id, packet.entity_id.0) {
                    continue;
                }
                state.shared.emit_entity_input(
                    event.entity,
                    epoch,
                    EntityProducerInput::Move {
                        token: EntityProducerToken::new(
                            epoch,
                            format!("packet-admission:{admission}:rotate-head"),
                        ),
                        patch: EntityMovePatch::rotate_head(
                            EntityIdentity::new(epoch, packet.entity_id.0),
                            packet.y_head_rot,
                        ),
                    },
                );
            }
            azalea::protocol::packets::game::ClientboundGamePacket::SetEntityMotion(packet) => {
                if !is_admitted_non_local_entity(local_protocol_id, packet.id.0) {
                    continue;
                }
                let velocity = packet.delta.to_vec3();
                state.shared.emit_entity_motion_residual(
                    event.entity,
                    epoch,
                    EntityProducerToken::new(
                        epoch,
                        format!("packet-admission:{admission}:set-motion"),
                    ),
                    EntityIdentity::new(epoch, packet.id.0),
                    [velocity.x, velocity.y, velocity.z],
                );
            }
            azalea::protocol::packets::game::ClientboundGamePacket::RemoveEntities(packet) => {
                for (index, id) in packet.entity_ids.iter().copied().enumerate() {
                    if !is_admitted_non_local_entity(local_protocol_id, id.0) {
                        continue;
                    }
                    state.shared.emit_entity_input(
                        event.entity,
                        epoch,
                        EntityProducerInput::Remove {
                            token: EntityProducerToken::new(
                                epoch,
                                format!("packet-admission:{admission}:remove:{id:?}:{index}"),
                            ),
                            entity: EntityIdentity::new(epoch, id.0),
                        },
                    );
                }
            }
            _ => {}
        }
    }
}

/// 修复 Azalea 的 `Spawn` 去重标记在 26.1 跨维度/重生边界上的失效。
///
/// 曾经这里还有 `record_server_position_corrections`，把
/// `ClientboundPlayerPosition` 转成 `SelfState::ServerPositionCorrection` 事实。
/// 已删除：服务端位置回拉在 ECS 里**已经被应用**，`pose` 就是应用之后的当前值，
/// 视口与帧都直接查它。再发一条「位置被校正了」的事件，描述的是快照本来就会读到
/// 的状态变化——它既不成为事实（`backend_event_is_fact` 挡住），也不能唤醒
/// （只有聊天唤醒），入队只是占槽。
///
/// 判据与理由见 `docs/participant-queue-audit.md` 与
/// `docs/vanilla-client-perception.md`：状态走拉取，事件只留不留痕的那一类。
pub(super) struct RespawnBoundaryPlugin;

impl Plugin for RespawnBoundaryPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, reset_spawn_marker_on_world_loaded);
        app.add_observer(record_respawn_packet);
    }
}

/// Azalea 的 `Spawn` 去重标记只在 `Login` 时清除；26.1 的跨维度/重生包走
/// `WorldLoadedEvent`，如果保留旧标记，新维度的区块加载不会再产生 Spawn。
/// 重置这两个加载边界后，下一批区块会重新进入标准 Spawn 处理，避免在这里
/// 复制一套快照或生命周期逻辑。
pub(super) fn reset_spawn_marker_on_world_loaded(
    mut world_loaded: MessageReader<azalea::packet::game::WorldLoadedEvent>,
    mut commands: Commands,
    state: Res<SwarmState>,
) {
    for event in world_loaded.read() {
        if !state
            .shared
            .observe_dimension_from_world_boundary_with_token(
                event.entity,
                event.name.to_string(),
                Some(event.attempt_token),
            )
        {
            continue;
        }
        commands.entity(event.entity).remove::<(
            azalea::events::SentSpawnEvent,
            azalea::entity::InLoadedChunk,
        )>();
    }
}

pub(super) fn record_respawn_packet(
    trigger: On<azalea::packet::game::SendGamePacketEvent>,
    state: Res<SwarmState>,
) {
    let azalea::protocol::packets::game::ServerboundGamePacket::ClientCommand(packet) =
        &trigger.event().packet
    else {
        return;
    };
    if !matches!(
        packet.action,
        azalea::protocol::packets::game::s_client_command::Action::PerformRespawn
    ) {
        return;
    }
    // 这是本地明确请求的重生过渡；只有后续 Spawn 才算服务端确认。
    let from_dimension = state
        .shared
        .writer
        .lock()
        .dimension
        .clone()
        .unwrap_or_else(|| "unknown".to_owned());
    state.shared.emit_respawn_transition_started(from_dimension);
}
