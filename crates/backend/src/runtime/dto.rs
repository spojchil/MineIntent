//! Pure conversion between backend-local values and frozen contract DTOs.

use super::*;

pub(super) fn contract_vec3(value: Vec3Value) -> ContractVec3Value {
    ContractVec3Value {
        x: value.x,
        y: value.y,
        z: value.z,
    }
}

pub(super) fn contract_self_pose(pose: PoseSnapshot) -> ContractSelfPose {
    ContractSelfPose {
        position: contract_vec3(pose.position),
        velocity: contract_vec3(pose.velocity),
        yaw: f64::from(pose.yaw),
        pitch: f64::from(pose.pitch),
    }
}

pub(super) fn dto_conversion_error(field: &str, message: impl Into<String>) -> BackendError {
    BackendError::BackendFailure {
        failure: BackendFailure {
            code: BackendFailureCode::ProtocolError,
            message: format!(
                "cannot convert backend observation DTO {field}: {}",
                message.into()
            ),
            retryable: false,
        },
    }
}

pub(super) fn contract_entity_snapshot(
    entity: ProtocolEntitySnapshot,
) -> Result<ContractProtocolEntitySnapshot, BackendError> {
    let equipment = entity
        .equipment
        .into_iter()
        .map(|item| {
            let count = u32::try_from(item.count).map_err(|_| {
                dto_conversion_error(
                    "entity.equipment.count",
                    format!("negative item count {}", item.count),
                )
            })?;
            Ok(ContractEntityEquipmentSnapshot {
                slot: u32::from(item.slot),
                item_name: item.item_name,
                count,
            })
        })
        .collect::<Result<Vec<_>, BackendError>>()?;

    Ok(ContractProtocolEntitySnapshot {
        entity_key: entity.entity_key,
        protocol_entity_id: entity.protocol_entity_id,
        entity_type: entity.entity_type,
        name: entity.name,
        username: entity.username,
        uuid: entity.uuid,
        position: contract_vec3(entity.position),
        velocity: contract_vec3(entity.velocity),
        yaw: f64::from(entity.yaw),
        pitch: f64::from(entity.pitch),
        head_yaw: entity.head_yaw.map(f64::from),
        width: f64::from(entity.width),
        height: f64::from(entity.height),
        on_ground: entity.on_ground,
        pose: entity.pose,
        held_item_name: entity.held_item_name,
        equipment,
        valid: entity.valid,
    })
}

pub(super) fn contract_block_snapshot(
    block: ProtocolBlockSnapshot,
) -> ContractProtocolBlockSnapshot {
    ContractProtocolBlockSnapshot {
        position: ContractBlockPosition {
            x: block.position.x,
            y: block.position.y,
            z: block.position.z,
        },
        name: block.name,
        state_id: block.state_id,
        properties: block
            .properties
            .into_iter()
            .map(|(key, value)| (key, parse_block_property_value(&value)))
            .collect(),
        collision_shapes: block.collision_shapes,
        transparent_hint: block.transparent_hint,
        bounding_box: match block.bounding_box {
            BlockBoundingBox::Block => ContractBlockBoundingBox::Block,
            BlockBoundingBox::Empty => ContractBlockBoundingBox::Empty,
        },
    }
}

pub(super) fn contract_block_read_result(result: BlockReadResult) -> ContractBlockReadResult {
    match result {
        BlockReadResult::Loaded { block } => ContractBlockReadResult::Loaded {
            block: contract_block_snapshot(block),
        },
        BlockReadResult::Unloaded => ContractBlockReadResult::Unloaded,
        BlockReadResult::OutOfWorld => ContractBlockReadResult::OutOfWorld,
    }
}

pub(super) fn contract_event_metadata(
    event: &BackendEventEnvelope,
) -> ContractBackendEventMetadata {
    ContractBackendEventMetadata {
        id: event.id.clone(),
        occurred_at: event.occurred_at.clone(),
        process_session_id: event.process_session_id.clone(),
        connection_epoch: event.connection_epoch,
        connection_attempt_id: event.connection_attempt_id.clone(),
        world_id: event.world_id.clone(),
        dimension: event.dimension.clone(),
    }
}

pub(super) fn contract_event_kind(kind: BackendEventKind) -> ContractBackendEventKind {
    match kind {
        BackendEventKind::Entity => ContractBackendEventKind::Entity,
        BackendEventKind::Block => ContractBackendEventKind::Block,
        BackendEventKind::Sound => ContractBackendEventKind::Sound,
        _ => unreachable!("non-observation event cannot enter typed observation adapter"),
    }
}

pub(super) fn observation_event_from_backend(
    event: &BackendEventEnvelope,
) -> Option<ObservationEvent> {
    let metadata = contract_event_metadata(event);
    let source = contract_fact_source(event.source);
    match (&event.kind, &event.payload) {
        (BackendEventKind::Entity, BackendEventPayload::Entity(payload)) => {
            Some(ObservationEvent::Entity(ContractBackendEventEnvelope::new(
                metadata,
                contract_event_kind(event.kind),
                source,
                payload.clone(),
            )))
        }
        (BackendEventKind::Block, BackendEventPayload::Block(payload)) => {
            Some(ObservationEvent::Block(ContractBackendEventEnvelope::new(
                metadata,
                contract_event_kind(event.kind),
                source,
                payload.clone(),
            )))
        }
        (BackendEventKind::Sound, BackendEventPayload::Sound(payload)) => {
            Some(ObservationEvent::Sound(ContractBackendEventEnvelope::new(
                metadata,
                contract_event_kind(event.kind),
                source,
                payload.clone(),
            )))
        }
        _ => None,
    }
}

pub(super) fn contract_fact_source(source: FactSource) -> ContractFactSource {
    match source {
        FactSource::Commanded => ContractFactSource::Commanded,
        FactSource::ClientPredicted => ContractFactSource::ClientPredicted,
        FactSource::ServerObserved => ContractFactSource::ServerObserved,
    }
}

pub(super) fn backend_error_from_directed(error: DirectedViewportError) -> BackendError {
    match error {
        DirectedViewportError::Backend(error) => error,
        DirectedViewportError::OutOfWorld { .. } => BackendError::BackendFailure {
            failure: BackendFailure {
                code: BackendFailureCode::ProtocolError,
                message: "full viewport encountered an out-of-world ray coordinate".to_owned(),
                retryable: false,
            },
        },
    }
}

pub(super) fn contract_viewport_projection(
    projection: ViewportProjection,
) -> ContractViewportProjection {
    ContractViewportProjection {
        frame: ContractViewportFrame {
            coordinates:
                mineintent_contracts::minecraft::ViewportCoordinateSystem::MinecraftWorldAbsolute,
            self_pose: ContractViewportSelfPose {
                position: projection.frame.self_pose.position,
                yaw_degrees: projection.frame.self_pose.yaw_degrees,
                pitch_degrees: projection.frame.self_pose.pitch_degrees,
            },
            legend: ContractViewportLegend {
                visible_entities: projection.frame.legend.visible_entities,
                visible_blocks: projection.frame.legend.visible_blocks,
            },
        },
        standing_on_block: projection.standing_on_block.map(contract_viewport_block),
        looked_at_block: projection.looked_at_block.map(contract_viewport_block),
        visible_entities: ContractVisibleEntitiesView {
            items: projection
                .visible_entities
                .items
                .into_iter()
                .map(|entity| ContractVisibleEntityView {
                    entity_type: entity.entity_type,
                    player: entity.player,
                    position: entity.position,
                })
                .collect(),
            truncated: projection.visible_entities.truncated,
        },
        visible_blocks: ContractVisibleBlocksView {
            blocks: projection.visible_blocks.blocks,
            truncated: projection.visible_blocks.truncated,
        },
    }
}

pub(super) fn contract_viewport_block(block: ViewportBlock) -> ContractViewportBlock {
    ContractViewportBlock {
        block: block.block,
        position: block.position.map(f64::from),
    }
}
