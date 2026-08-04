//! Entity post-state reduction, residual authority, and contract conversion.

use super::*;

impl SharedRuntime {
    #[cfg(test)]
    pub(super) fn apply_entity_input_for_owner(
        &self,
        owner: bevy_ecs::entity::Entity,
        epoch: u64,
        input: EntityProducerInput,
    ) -> Option<NormalizedEntityEvent> {
        self.apply_entity_input_for_owner_with_generation(owner, epoch, input)
            .map(|(event, _generation)| event)
    }

    pub(super) fn apply_entity_input_for_owner_with_generation(
        &self,
        owner: bevy_ecs::entity::Entity,
        epoch: u64,
        input: EntityProducerInput,
    ) -> Option<(NormalizedEntityEvent, u64)> {
        let mut producer = self.entity_producer.lock();
        if producer.owner != Some((owner, epoch)) {
            return None;
        }
        producer
            .cache
            .apply(epoch, input)
            .map(|event| (event, producer.scope_generation))
    }

    pub(super) fn emit_entity_input(
        &self,
        owner: bevy_ecs::entity::Entity,
        epoch: u64,
        input: EntityProducerInput,
    ) -> bool {
        let residual_action = match &input {
            EntityProducerInput::Spawn { .. } => EntityResidualAction::Clear,
            _ => EntityResidualAction::Retain,
        };
        self.emit_entity_input_with_velocity_residual(owner, epoch, input, residual_action)
    }

    pub(super) fn emit_entity_input_with_velocity_residual(
        &self,
        owner: bevy_ecs::entity::Entity,
        epoch: u64,
        input: EntityProducerInput,
        residual_action: EntityResidualAction,
    ) -> bool {
        let normalized = self.apply_entity_input_for_owner_with_generation(owner, epoch, input);
        let Some((normalized, scope_generation)) = normalized else {
            return false;
        };
        #[cfg(test)]
        self.invoke_entity_publish_after_apply_hook();

        let should_drain = {
            let _admission = self.command_admission.lock();
            let producer = self.entity_producer.lock();
            if producer.owner != Some((owner, epoch))
                || producer.scope_generation != scope_generation
            {
                return false;
            }
            // The packet producer's shadow is the immediate post-state
            // authority.  Publish the same state into the public observation
            // list before queueing the event so an observation callback can
            // read it synchronously.
            if !self.apply_entity_observation_event_locked(epoch, &normalized, residual_action) {
                return false;
            }
            let payload =
                BackendEventPayload::Entity(normalized_entity_event_to_contract(normalized));
            let Some(should_drain) = self.enqueue_entity_event_if_running_locked(
                epoch,
                FactSource::ServerObserved,
                payload,
            ) else {
                return false;
            };
            // Keep the owner stable through envelope construction and queue
            // insertion. Queue draining and callbacks happen below, lock-free.
            drop(producer);
            should_drain
        };
        if should_drain {
            self.drain_events();
        }
        true
    }

    pub(super) fn emit_entity_motion_residual(
        &self,
        owner: bevy_ecs::entity::Entity,
        epoch: u64,
        token: EntityProducerToken,
        entity: EntityIdentity,
        velocity: [f64; 3],
    ) -> bool {
        let normalized = {
            let mut producer = self.entity_producer.lock();
            if producer.owner != Some((owner, epoch)) {
                return false;
            }
            producer
                .cache
                .apply_velocity_residual(epoch, token, entity, velocity)
                .map(|snapshot| (snapshot, producer.scope_generation))
        };
        let Some((normalized, scope_generation)) = normalized else {
            return false;
        };
        let _admission = self.command_admission.lock();
        let producer = self.entity_producer.lock();
        if producer.owner != Some((owner, epoch)) || producer.scope_generation != scope_generation {
            return false;
        }
        let mut observation = self.observation.write();
        let Some(snapshot) = normalized_entity_snapshot_to_protocol(&normalized) else {
            return false;
        };
        upsert_entity_observation(&mut observation, snapshot);
        record_entity_residual(
            &mut observation,
            &normalized.entity_key(),
            None,
            Some(normalized.velocity),
            EntityResidualAction::Update,
        );
        true
    }

    /// Synchronize the public tracked-entity observation with the exact
    /// producer post-state that is about to be emitted.  The caller holds
    /// `command_admission` and the producer guard; callbacks/drain remain
    /// outside all of those locks.
    pub(super) fn apply_entity_observation_event_locked(
        &self,
        epoch: u64,
        event: &NormalizedEntityEvent,
        residual_action: EntityResidualAction,
    ) -> bool {
        let mut observation = self.observation.write();
        match event {
            NormalizedEntityEvent::Spawned { entity }
            | NormalizedEntityEvent::Moved { entity }
            | NormalizedEntityEvent::Updated { entity, .. } => {
                let Some(snapshot) = normalized_entity_snapshot_to_protocol(entity) else {
                    return false;
                };
                if entity.identity.epoch != epoch {
                    return false;
                }
                let key = snapshot.entity_key.clone();
                upsert_entity_observation(&mut observation, snapshot);
                record_entity_residual(
                    &mut observation,
                    &key,
                    entity.head_yaw,
                    Some(entity.velocity),
                    residual_action,
                );
                true
            }
            NormalizedEntityEvent::Removed { entity, .. } => {
                if entity.epoch != epoch {
                    return false;
                }
                let key = entity.key();
                let before = observation.tracked_entities.len();
                let residual_before = observation.entity_residuals.len();
                observation
                    .tracked_entities
                    .retain(|snapshot| snapshot.entity_key != key);
                observation
                    .entity_residuals
                    .retain(|residual| residual.entity_key != key);
                if before != observation.tracked_entities.len()
                    || residual_before != observation.entity_residuals.len()
                {
                    observation.bump_generation();
                }
                true
            }
            NormalizedEntityEvent::Animation { .. } | NormalizedEntityEvent::Hurt { .. } => true,
        }
    }

    pub(super) fn enqueue_entity_event_if_running_locked(
        &self,
        expected_epoch: u64,
        source: FactSource,
        payload: BackendEventPayload,
    ) -> Option<bool> {
        if !self.command_execution_allowed_without_lock() {
            return None;
        }
        #[cfg(test)]
        self.invoke_event_admission_hook();

        let mut dispatch = self.event_dispatch.lock();
        let event = {
            let mut writer = self.writer.lock();
            // Metadata is created while this exact check is protected by the
            // attempt admission lock; an entity payload can never be stamped
            // with a later connection's envelope epoch.
            if writer.connection_epoch != expected_epoch {
                return None;
            }
            writer.emit(source, payload)
        };
        Some(self.enqueue_dispatch_locked(&mut dispatch, event))
    }
}

pub(super) fn finite_f32(value: f64) -> Option<f32> {
    let value = value as f32;
    value.is_finite().then_some(value)
}

pub(super) fn finite_f64(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

pub(super) fn normalized_entity_snapshot_to_protocol(
    snapshot: &NormalizedEntitySnapshot,
) -> Option<ProtocolEntitySnapshot> {
    let position = [
        finite_f64(snapshot.position[0])?,
        finite_f64(snapshot.position[1])?,
        finite_f64(snapshot.position[2])?,
    ];
    let velocity = [
        finite_f64(snapshot.velocity[0])?,
        finite_f64(snapshot.velocity[1])?,
        finite_f64(snapshot.velocity[2])?,
    ];
    let yaw = finite_f32(snapshot.yaw)?;
    let pitch = finite_f32(snapshot.pitch)?;
    let head_yaw = match snapshot.head_yaw {
        Some(value) => Some(finite_f32(value)?),
        None => None,
    };
    let width = finite_f32(snapshot.width)?;
    let height = finite_f32(snapshot.height)?;
    let equipment = snapshot
        .equipment
        .iter()
        .map(|(slot, item_name, count)| {
            Some(crate::snapshot::EntityEquipmentSnapshot {
                slot: u8::try_from(*slot).ok()?,
                item_name: item_name.clone(),
                count: i32::try_from(*count).ok()?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(ProtocolEntitySnapshot {
        entity_key: snapshot.entity_key(),
        protocol_entity_id: snapshot.identity.protocol_id,
        entity_type: snapshot.entity_type.clone(),
        name: snapshot.name.clone(),
        username: snapshot.username.clone(),
        uuid: snapshot.uuid.clone(),
        position: Vec3Value {
            x: position[0],
            y: position[1],
            z: position[2],
        },
        velocity: Vec3Value {
            x: velocity[0],
            y: velocity[1],
            z: velocity[2],
        },
        yaw,
        pitch,
        head_yaw,
        width,
        height,
        on_ground: snapshot.on_ground,
        pose: snapshot.pose.clone(),
        held_item_name: snapshot.held_item_name.clone(),
        equipment,
        valid: snapshot.valid,
    })
}

pub(super) fn upsert_entity_observation(
    observation: &mut ObservationState,
    snapshot: ProtocolEntitySnapshot,
) {
    if let Some(existing) = observation
        .tracked_entities
        .iter_mut()
        .find(|existing| existing.entity_key == snapshot.entity_key)
    {
        *existing = snapshot;
    } else {
        observation.tracked_entities.push(snapshot);
    }
    observation
        .tracked_entities
        .sort_by(|left, right| left.entity_key.cmp(&right.entity_key));
    observation.bump_generation();
}

pub(super) fn record_entity_residual(
    observation: &mut ObservationState,
    entity_key: &str,
    head_yaw: Option<f64>,
    velocity: Option<[f64; 3]>,
    action: EntityResidualAction,
) {
    if matches!(action, EntityResidualAction::Clear) {
        observation
            .entity_residuals
            .retain(|residual| residual.entity_key != entity_key);
    }

    let update_head_yaw = head_yaw.and_then(finite_f32);
    let update_velocity = match action {
        EntityResidualAction::Update => velocity,
        EntityResidualAction::Retain | EntityResidualAction::Clear => None,
    };
    if update_head_yaw.is_none() && update_velocity.is_none() {
        return;
    }

    if let Some(existing) = observation
        .entity_residuals
        .iter_mut()
        .find(|residual| residual.entity_key == entity_key)
    {
        if update_head_yaw.is_some() {
            existing.head_yaw = update_head_yaw;
        }
        if update_velocity.is_some() {
            existing.velocity = update_velocity;
        }
        return;
    }

    if observation.entity_residuals.len() >= ENTITY_OBSERVATION_RESIDUAL_CAPACITY {
        observation.entity_residuals.remove(0);
    }
    observation
        .entity_residuals
        .push(EntityObservationResidual {
            entity_key: entity_key.to_owned(),
            head_yaw: update_head_yaw,
            velocity: update_velocity,
        });
}

pub(super) fn normalized_entity_snapshot_to_contract(
    snapshot: NormalizedEntitySnapshot,
) -> ContractProtocolEntitySnapshot {
    contract_entity_snapshot(
        normalized_entity_snapshot_to_protocol(&snapshot)
            .expect("entity producer admits only finite representable snapshots"),
    )
    .expect("normalized entity snapshot should satisfy contract conversion")
}

pub(super) fn merge_refreshed_tracked_entities(
    mut captured: Vec<ProtocolEntitySnapshot>,
    residuals: &mut Vec<EntityObservationResidual>,
    connection_epoch: u64,
) -> Vec<ProtocolEntitySnapshot> {
    residuals.retain(|residual| {
        residual
            .entity_key
            .starts_with(&format!("{connection_epoch}:"))
            && captured
                .iter()
                .any(|snapshot| snapshot.entity_key == residual.entity_key)
    });
    for snapshot in &mut captured {
        if !snapshot
            .entity_key
            .starts_with(&format!("{connection_epoch}:"))
        {
            continue;
        }
        let Some(residual) = residuals
            .iter()
            .find(|current| current.entity_key == snapshot.entity_key)
        else {
            continue;
        };
        // ECS owns the fields Azalea captures (position, velocity, body look,
        // dimensions, pose, UUID, and on-ground). Only fields with an explicit
        // packet residual survive refresh: head rotation is not represented
        // by capture, and PositionSync/SetEntityMotion velocity is retained
        // only when the lower handler does not write it into Physics.
        if residual.head_yaw.is_some() {
            snapshot.head_yaw = residual.head_yaw;
        }
        if let Some(velocity) = residual.velocity {
            snapshot.velocity = Vec3Value {
                x: velocity[0],
                y: velocity[1],
                z: velocity[2],
            };
        }
    }
    captured.sort_by(|left, right| left.entity_key.cmp(&right.entity_key));
    captured
}

pub(super) fn normalized_entity_event_to_contract(
    event: NormalizedEntityEvent,
) -> ContractProtocolEntityEvent {
    match event {
        NormalizedEntityEvent::Spawned { entity } => ContractProtocolEntityEvent::Spawned {
            entity: normalized_entity_snapshot_to_contract(entity),
        },
        NormalizedEntityEvent::Moved { entity } => ContractProtocolEntityEvent::Moved {
            entity: normalized_entity_snapshot_to_contract(entity),
        },
        NormalizedEntityEvent::Updated { entity, changed } => {
            ContractProtocolEntityEvent::Updated {
                entity: normalized_entity_snapshot_to_contract(entity),
                changed,
            }
        }
        NormalizedEntityEvent::Animation {
            entity, animation, ..
        } => ContractProtocolEntityEvent::Animation {
            entity_key: entity.key(),
            animation,
        },
        NormalizedEntityEvent::Hurt {
            entity,
            possible_source,
        } => ContractProtocolEntityEvent::Hurt {
            entity_key: entity.key(),
            possible_source_entity_key: possible_source.map(EntityIdentity::key),
        },
        NormalizedEntityEvent::Removed { entity, last } => ContractProtocolEntityEvent::Removed {
            entity_key: entity.key(),
            last: normalized_entity_snapshot_to_contract(last),
            reason: ContractEntityRemovalReason::ProtocolRemoved,
        },
    }
}
