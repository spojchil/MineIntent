//! Attempt identity, canonical source admission, and reconnect ownership.

use super::*;

impl SharedRuntime {
    /// Caller holds `command_admission`, which also serializes every writer
    /// epoch transition. Installing the owner and resetting its shadow are
    /// therefore part of the same attempt identity transaction.
    pub(super) fn bind_entity_producer_owner_locked(
        &self,
        entity: bevy_ecs::entity::Entity,
        epoch: u64,
    ) {
        #[cfg(test)]
        self.invoke_entity_owner_bind_hook();
        let mut producer = self.entity_producer.lock();
        producer.source_fence.bind(entity);
        producer.owner = Some((entity, epoch));
        producer.reset_scope(epoch);
    }

    #[cfg(test)]
    pub(super) fn entity_producer_epoch_for(
        &self,
        entity: bevy_ecs::entity::Entity,
    ) -> Option<u64> {
        self.entity_producer
            .lock()
            .owner
            .and_then(|(owner, epoch)| (owner == entity).then_some(epoch))
    }

    #[cfg(test)]
    pub(super) fn reset_entity_scope_for_owner(&self, entity: bevy_ecs::entity::Entity) -> bool {
        let mut producer = self.entity_producer.lock();
        let Some((owner, epoch)) = producer.owner else {
            return false;
        };
        if owner != entity {
            return false;
        }
        producer.reset_scope(epoch);
        true
    }

    #[cfg(test)]
    pub(super) fn deactivate_entity_producer_owner(
        &self,
        entity: bevy_ecs::entity::Entity,
    ) -> bool {
        let mut producer = self.entity_producer.lock();
        if producer.owner.is_none_or(|(owner, _)| owner != entity) {
            return false;
        }
        producer.owner = None;
        producer.deactivate_scope();
        true
    }

    /// A swarm-level disconnect has no Bevy client entity to compare. It is a
    /// lifecycle-wide boundary, so its reconnect claim deactivates only the
    /// single owner that is current at that admission point. Entity-specific
    /// late disconnects continue through `deactivate_entity_producer_owner`.
    pub(super) fn deactivate_current_entity_producer_owner(&self) -> bool {
        let mut producer = self.entity_producer.lock();
        if producer.owner.is_none() {
            return false;
        }
        producer.owner = None;
        producer.deactivate_scope();
        true
    }

    /// Claim an entity-specific lifecycle transition while holding
    /// `command_admission`. The caller supplies the epoch observed at the
    /// canonical ECS source. This is the source discriminator that an Azalea
    /// high-level `Event` lacks when the same Bevy entity is reused.
    pub(super) fn admit_entity_lifecycle_owner_locked(
        &self,
        entity: bevy_ecs::entity::Entity,
        expected_epoch: u64,
        allow_unbound_attempt: bool,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> bool {
        let current_epoch = self.writer.lock().connection_epoch;
        if current_epoch != expected_epoch {
            return false;
        }
        let mut producer = self.entity_producer.lock();
        if let Some(token) = attempt_token {
            // Stamped source: the token must already be bound one-to-one to
            // this exact epoch. The legacy source fence does not apply.
            if !producer
                .source_token_bindings
                .matches(token, expected_epoch)
            {
                return false;
            }
        } else if !producer.source_fence.allows_unstamped(entity) {
            // The event carries no source epoch.  In the same-entity rebind
            // case this is deliberately fail-closed; using `expected_epoch`
            // from the current writer would stamp a possible late A event as
            // B.
            return false;
        }
        match producer.attempt {
            AttemptAdmissionState::Bound {
                epoch,
                entity: bound_entity,
                attempt_token: bound_attempt_token,
                ..
            } if epoch == expected_epoch
                && bound_entity == entity
                && bound_attempt_token == attempt_token
                && producer.owner == Some((entity, expected_epoch)) =>
            {
                producer.owner = None;
                producer.deactivate_scope();
                true
            }
            AttemptAdmissionState::Reserved {
                epoch,
                join_started_epoch,
                attempt_token: reserved_attempt_token,
                ..
            } if allow_unbound_attempt
                && epoch == expected_epoch
                && reserved_attempt_token == attempt_token
                && join_started_epoch == Some(expected_epoch) =>
            {
                producer.deactivate_scope();
                true
            }
            _ => false,
        }
    }

    pub(super) fn next_entity_packet_admission(&self) -> u64 {
        self.entity_packet_admission.fetch_add(1, Ordering::AcqRel)
    }

    /// Admit an unstamped canonical Azalea source.  The returned context is
    /// intentionally short-lived: the producer must pass it back through
    /// `emit_canonical_observation_event`, which rechecks the same owner,
    /// epoch, scope generation, and source fence immediately before queue
    /// insertion.
    #[cfg(test)]
    pub(super) fn admit_canonical_source(
        &self,
        entity: bevy_ecs::entity::Entity,
    ) -> Option<CanonicalSourceAdmission> {
        self.admit_canonical_source_with_token(entity, None)
    }

    /// Stamped variant of [`Self::admit_canonical_source`]. The vendor event
    /// token must already be bound one-to-one to the current owner's epoch;
    /// the legacy source fence is never consulted for a stamped source.
    pub(super) fn admit_canonical_source_with_token(
        &self,
        entity: bevy_ecs::entity::Entity,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<CanonicalSourceAdmission> {
        let _admission = self.command_admission.lock();
        let epoch = self.writer.lock().connection_epoch;
        let producer = self.entity_producer.lock();
        let admitted = producer.owner == Some((entity, epoch))
            && match attempt_token {
                Some(token) => producer.source_token_bindings.matches(token, epoch),
                None => producer.source_fence.allows_unstamped(entity),
            };
        admitted.then_some(CanonicalSourceAdmission {
            entity,
            epoch,
            scope_generation: producer.scope_generation,
            attempt_token,
        })
    }

    pub(super) fn canonical_source_still_valid_locked(
        &self,
        source: CanonicalSourceAdmission,
    ) -> bool {
        if self.writer.lock().connection_epoch != source.epoch {
            return false;
        }
        let producer = self.entity_producer.lock();
        producer.owner == Some((source.entity, source.epoch))
            && producer.scope_generation == source.scope_generation
            && match source.attempt_token {
                Some(token) => producer.source_token_bindings.matches(token, source.epoch),
                None => producer.source_fence.allows_unstamped(source.entity),
            }
    }

    #[cfg(test)]
    pub(super) fn admit_canonical_packet_source_for_epoch(
        &self,
        entity: bevy_ecs::entity::Entity,
        source_epoch: u64,
    ) -> bool {
        let _admission = self.command_admission.lock();
        self.writer.lock().connection_epoch == source_epoch
            && self.entity_producer.lock().owner == Some((entity, source_epoch))
    }

    #[cfg(test)]
    pub(super) fn reset_entity_scope_for_owner_at_epoch(
        &self,
        entity: bevy_ecs::entity::Entity,
        expected_epoch: u64,
    ) -> bool {
        self.reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
            entity,
            expected_epoch,
            None,
            None,
        )
    }

    /// Reset every public authority at a raw Login/Respawn packet boundary.
    /// The dimension, when supplied by that same packet, is admitted after
    /// the reset while the same command-admission lock is held, preserving
    /// packet order and preventing a late boundary from mutating a new owner.
    pub(super) fn reset_entity_scope_for_owner_at_epoch_with_dimension_and_light(
        &self,
        entity: bevy_ecs::entity::Entity,
        expected_epoch: u64,
        dimension: Option<String>,
        has_skylight: Option<bool>,
    ) -> bool {
        let (accepted, should_drain) = {
            let _admission = self.command_admission.lock();
            if self.writer.lock().connection_epoch != expected_epoch {
                return false;
            }
            let mut producer = self.entity_producer.lock();
            if producer.owner != Some((entity, expected_epoch))
                || !producer.source_fence.allows_unstamped(entity)
            {
                return false;
            }
            producer.reset_scope(expected_epoch);
            let scope_generation = producer.scope_generation;
            drop(producer);

            {
                let mut observation = self.observation.write();
                observation.world = None;
                observation.snapshot = None;
                observation.snapshot_scope_generation = 0;
                observation.source = None;
                observation.tracked_entities.clear();
                observation.entity_residuals.clear();
                if observation.armor_epoch != Some(expected_epoch) {
                    observation.armor = None;
                    observation.armor_epoch = None;
                }
                // 这一步会清空光照区块缓存。缓存要靠后续的区块/光照包重新填满，
                // 在填满之前任何取帧都读不到光照——所以边界发生的时刻必须留痕，
                // 否则事后只能看到「读不到」，看不到「什么时候被清的」。
                let dropped = observation.light_cache.chunks.len();
                tracing::info!(
                    target: "mineintent_backend",
                    epoch = expected_epoch,
                    scope_generation,
                    dimension = ?dimension,
                    dropped_light_chunks = dropped,
                    "作用域边界：重置观察面并清空光照缓存"
                );
                observation.clear_light_for_scope(
                    expected_epoch,
                    scope_generation,
                    dimension.clone(),
                    has_skylight,
                );
                observation.bump_generation();
            }

            let should_drain = dimension
                .map(|dimension| {
                    let Some(previous) = self.set_dimension(dimension.clone()) else {
                        return false;
                    };
                    if previous == dimension {
                        return false;
                    }
                    self.enqueue_event(
                        FactSource::ServerObserved,
                        BackendEventPayload::Lifecycle(BackendLifecyclePayload::DimensionChanged {
                            from: previous,
                            to: dimension,
                        }),
                    )
                })
                .unwrap_or(false);
            (true, should_drain)
        };
        if should_drain {
            self.drain_events();
        }
        accepted
    }

    /// Allocate an attempt identity before network work starts. The dispatch
    /// lock covers identity allocation and `connection_requested`, so no
    /// producer can insert an event between those two facts.
    pub(super) fn begin_connection_attempt(&self) -> bool {
        let should_drain = {
            let _admission = self.command_admission.lock();
            let Some(should_drain) = self.begin_connection_attempt_locked(None) else {
                return false;
            };
            should_drain
        };
        if should_drain {
            self.drain_events();
        }
        self.phase_cancel.notify_waiters();
        self.stable_cancel.notify_waiters();
        true
    }

    pub(super) fn begin_connection_attempt_locked(
        &self,
        reconnect_token: Option<u64>,
    ) -> Option<bool> {
        if self.stopping.load(Ordering::Acquire) || self.stop_requested.load(Ordering::Acquire) {
            return None;
        }
        if let Some(token) = reconnect_token {
            if self.reconnect_attempt_token.load(Ordering::Acquire) != token {
                return None;
            }
            self.reconnect_add_pending.store(true, Ordering::Release);
        }
        let (epoch, retry_ordinal) = {
            let writer = self.writer.lock();
            (
                writer.connection_epoch.checked_add(1)?,
                self.retry_ordinal.load(Ordering::Acquire).checked_add(1)?,
            )
        };
        self.disconnect_reported.store(false, Ordering::Release);
        self.stopped_reported.store(false, Ordering::Release);
        self.faulted_reported.store(false, Ordering::Release);
        self.shutdown_requested.store(false, Ordering::Release);
        self.active_client.lock().take();
        self.invalidate_phase_locked();
        self.invalidate_stable_reset_locked();
        if !self.stop_requested.load(Ordering::Acquire) {
            self.dispatch_cancelled.store(false, Ordering::Release);
        }
        *self.stop_reason.lock() = None;
        *self.last_close.lock() = None;
        *self.last_failure.lock() = None;
        {
            let mut producer = self.entity_producer.lock();
            producer.source_fence.begin_attempt();
            producer.owner = None;
            producer.attempt = AttemptAdmissionState::NotStarted;
            producer.pending_connection_failure = None;
            producer.deactivate_scope();
        }
        self.clear_observations();
        self.lifecycle_revision.fetch_add(1, Ordering::AcqRel);

        let mut dispatch = self.event_dispatch.lock();
        let (event, epoch, attempt_id, attempt) = {
            let mut writer = self.writer.lock();
            writer.new_attempt(epoch);
            self.retry_ordinal.store(retry_ordinal, Ordering::Release);
            let attempt_id = writer.connection_attempt_id.clone();
            let attempt = u32::try_from(retry_ordinal).unwrap_or(u32::MAX);
            (
                writer.emit(
                    FactSource::Commanded,
                    BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionRequested {
                        attempt,
                    }),
                ),
                epoch,
                attempt_id,
                attempt,
            )
        };
        {
            let mut producer = self.entity_producer.lock();
            producer.attempt = AttemptAdmissionState::Reserved {
                epoch,
                reconnect_token,
                join_started_epoch: None,
                attempt_token: None,
            };
        }
        self.set_backend_state(BackendState::Connecting {
            epoch,
            attempt_id,
            attempt,
        });
        Some(self.enqueue_dispatch_locked(&mut dispatch, event))
    }

    /// `Event::Init` 消费连接发起前预留的身份，而不是再创建一个 epoch。
    /// 防御性 fallback 仍走同一入口，确保即使 Azalea 新增调用路径，也先有
    /// `connection_requested`，随后才发 transport 生命周期事件。
    #[cfg(test)]
    pub(super) fn admit_canonical_join_started(&self, source_epoch: u64) -> bool {
        self.admit_canonical_join_started_with_token(source_epoch, None)
    }

    /// Stamped variant of [`Self::admit_canonical_join_started`]: the vendor
    /// `StartJoinServerEvent.attempt_token` is bound one-to-one to the
    /// reservation's epoch. The legacy source fence only applies to the
    /// tokenless path.
    pub(super) fn admit_canonical_join_started_with_token(
        &self,
        source_epoch: u64,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> bool {
        let _admission = self.command_admission.lock();
        if self.writer.lock().connection_epoch != source_epoch {
            return false;
        }
        let mut producer = self.entity_producer.lock();
        if attempt_token.is_none() && !producer.source_fence.allows_unstamped_global() {
            // Init/StartJoin has no token.  In a same-entity reconnect window
            // it cannot safely consume the reservation belonging to B.
            return false;
        }
        let AttemptAdmissionState::Reserved {
            epoch,
            reconnect_token,
            join_started_epoch,
            attempt_token: reserved_token,
        } = producer.attempt
        else {
            return false;
        };
        if epoch != source_epoch
            || reserved_token.is_some_and(|reserved| Some(reserved) != attempt_token)
            || reconnect_token.is_some_and(|token| {
                self.reconnect_attempt_token.load(Ordering::Acquire) != token
                    || !self.reconnect_add_pending.load(Ordering::Acquire)
            })
        {
            return false;
        }
        if join_started_epoch == Some(source_epoch) {
            // Idempotence is exact. A tokenless admission cannot later be
            // upgraded to a stamped one, nor can a stamped admission switch
            // tokens after it has selected the reservation.
            return reserved_token == attempt_token;
        }
        if let Some(token) = attempt_token {
            // Binding is the transaction's final fallible step.  No rejected
            // reservation/control predicate may leave a token or epoch entry
            // behind for a later Client/reconnect-return path to consume.
            if !producer.source_token_bindings.bind(token, source_epoch) {
                return false;
            }
        }
        producer.attempt = AttemptAdmissionState::Reserved {
            epoch,
            reconnect_token,
            join_started_epoch: Some(source_epoch),
            attempt_token,
        };
        true
    }

    pub(super) fn bind_reserved_attempt_locked(
        &self,
        entity: bevy_ecs::entity::Entity,
        expected_reconnect_token: Option<u64>,
        init_path: bool,
        expected_attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<(u64, bool, Option<PhaseDeadlineToken>)> {
        if self.stopping.load(Ordering::Acquire) {
            return None;
        }
        if let Some(token) = expected_reconnect_token {
            if !self.reconnect_add_pending.load(Ordering::Acquire)
                || self.reconnect_attempt_token.load(Ordering::Acquire) != token
            {
                return None;
            }
        }

        let epoch = self.writer.lock().connection_epoch;
        let mut producer = self.entity_producer.lock();
        let attempt = producer.attempt;
        let source_token = expected_attempt_token;
        if source_token.is_some_and(|token| !producer.source_token_bindings.matches(token, epoch)) {
            // A stamped client must already be bound one-to-one to this exact
            // epoch by its StartJoinServerEvent; a token from a different
            // attempt can never consume this reservation.
            return None;
        }
        if init_path
            && matches!(
                attempt,
                AttemptAdmissionState::Reserved {
                    reconnect_token: Some(_),
                    ..
                }
            )
        {
            // `Event::Init` has no reconnect token.  It must not consume B's
            // reservation, even when the returned client happens to use a
            // different Bevy entity.  Only the stamped reconnect-return path
            // can prove which reservation it belongs to.
            return None;
        }
        if init_path
            && matches!(attempt, AttemptAdmissionState::Reserved { .. })
            && source_token.is_none()
            && !producer.source_fence.allows_unstamped(entity)
        {
            return None;
        }
        if let Some(token) = source_token {
            // Idempotent re-registration: the binding was established by
            // StartJoinServerEvent and must still be exact.
            if !producer.source_token_bindings.bind(token, epoch) {
                return None;
            }
        }
        drop(producer);
        match attempt {
            AttemptAdmissionState::Reserved {
                epoch: reserved_epoch,
                reconnect_token,
                attempt_token: reserved_attempt_token,
                ..
            } if reserved_epoch == epoch
                && (init_path || reconnect_token == expected_reconnect_token)
                && reserved_attempt_token == source_token =>
            {
                self.clear_observations();
                self.bind_entity_producer_owner_locked(entity, epoch);
                let mut producer = self.entity_producer.lock();
                producer.attempt = AttemptAdmissionState::Bound {
                    epoch,
                    entity,
                    reconnect_token,
                    attempt_token: source_token,
                };
                drop(producer);
            }
            AttemptAdmissionState::Bound {
                epoch: bound_epoch,
                entity: bound_entity,
                reconnect_token,
                attempt_token: bound_attempt_token,
            } if bound_epoch == epoch
                && bound_entity == entity
                && (init_path || reconnect_token == expected_reconnect_token)
                && bound_attempt_token == source_token =>
            {
                return Some((epoch, false, None));
            }
            _ => return None,
        }

        if !self.command_execution_allowed_without_lock() {
            return Some((epoch, false, None));
        }
        let (attempt_id, attempt) = {
            let writer = self.writer.lock();
            (
                writer.connection_attempt_id.clone(),
                u32::try_from(self.retry_ordinal.load(Ordering::Acquire)).unwrap_or(u32::MAX),
            )
        };
        self.set_backend_state(BackendState::LoggingIn {
            epoch,
            attempt_id,
            attempt,
        });
        self.invalidate_stable_reset_locked();
        let phase_token = self.arm_phase_deadline_locked(TransportPhase::LoggingIn);
        let should_drain = self.enqueue_event(
            FactSource::ServerObserved,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected),
        );
        Some((epoch, should_drain, phase_token))
    }

    pub(super) fn bind_reserved_attempt(
        self: &Arc<Self>,
        entity: bevy_ecs::entity::Entity,
        expected_reconnect_token: Option<u64>,
        init_path: bool,
        expected_attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<u64> {
        let (epoch, should_drain, phase_token) = {
            let _admission = self.command_admission.lock();
            self.bind_reserved_attempt_locked(
                entity,
                expected_reconnect_token,
                init_path,
                expected_attempt_token,
            )?
        };
        if should_drain {
            self.drain_events();
        }
        self.phase_cancel.notify_waiters();
        if let Some(token) = phase_token {
            self.spawn_phase_deadline(token);
        }
        Some(epoch)
    }

    #[cfg(test)]
    pub(super) fn consume_attempt_for_transport_init_and_bind(
        self: &Arc<Self>,
        entity: bevy_ecs::entity::Entity,
    ) -> Option<u64> {
        self.consume_attempt_for_transport_init_and_bind_with_token(entity, None)
    }

    pub(super) fn consume_attempt_for_transport_init_and_bind_with_token(
        self: &Arc<Self>,
        entity: bevy_ecs::entity::Entity,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<u64> {
        let (epoch, should_drain, phase_token) = {
            let _admission = self.command_admission.lock();
            if self.stopping.load(Ordering::Acquire) {
                return None;
            }

            let needs_fallback = {
                let producer = self.entity_producer.lock();
                matches!(producer.attempt, AttemptAdmissionState::NotStarted)
            };
            let mut should_drain = false;
            if needs_fallback {
                should_drain = self.begin_connection_attempt_locked(None)?;
            }
            self.disconnect_reported.store(false, Ordering::Release);
            let (epoch, bind_should_drain, phase_token) =
                self.bind_reserved_attempt_locked(entity, None, true, attempt_token)?;
            (epoch, should_drain || bind_should_drain, phase_token)
        };
        if should_drain {
            self.drain_events();
        }
        self.phase_cancel.notify_waiters();
        if let Some(token) = phase_token {
            self.spawn_phase_deadline(token);
        }
        Some(epoch)
    }

    #[cfg(test)]
    pub(super) fn bind_reconnect_return(
        self: &Arc<Self>,
        token: u64,
        entity: bevy_ecs::entity::Entity,
    ) -> Option<u64> {
        self.bind_reconnect_return_with_token(token, entity, None)
    }

    pub(super) fn bind_reconnect_return_with_token(
        self: &Arc<Self>,
        token: u64,
        entity: bevy_ecs::entity::Entity,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<u64> {
        self.bind_reserved_attempt(entity, Some(token), false, attempt_token)
    }

    #[cfg(test)]
    pub(super) fn claim_reconnect(&self) -> bool {
        self.claim_reconnect_with_token(None)
    }

    /// NEW-15：登记当前 pre-Init 连接尝试身份（ECS 捕获系统在
    /// `Added<CreateConnectionTask>` 时调用）。覆盖式写入：同一时刻至多
    /// 一个在途 join 任务，新登记天然取代旧的。
    pub(super) fn record_pending_connection(
        &self,
        entity: azalea::ecs::entity::Entity,
        attempt_token: azalea::join::AttemptToken,
    ) {
        let _admission = self.command_admission.lock();
        let epoch = self.writer.lock().connection_epoch;
        let ordinal = self.retry_ordinal.load(Ordering::Acquire);
        *self.pending_connection.lock() = Some(PendingConnectionAttempt {
            entity,
            attempt_token,
            epoch,
            ordinal,
        });
    }

    /// NEW-15：Connecting 超时下取走仍匹配 (epoch, ordinal) 的尝试身份。
    /// 调用方必须持有 `command_admission`；身份不匹配时不动登记（陈旧
    /// deadline 不得取消新尝试）。
    pub(super) fn take_pending_connection_locked(
        &self,
        epoch: u64,
        ordinal: u64,
    ) -> Option<(azalea::ecs::entity::Entity, azalea::join::AttemptToken)> {
        let mut pending = self.pending_connection.lock();
        match *pending {
            Some(attempt) if attempt.epoch == epoch && attempt.ordinal == ordinal => {
                *pending = None;
                Some((attempt.entity, attempt.attempt_token))
            }
            _ => None,
        }
    }

    /// NEW-15：pre-Init 超时的重连宣占。与 SwarmEvent::Disconnect 兜底共享
    /// 同一个 `reconnect_pending` 有限屏障：先到者调度，后到者拒绝，
    /// 双重调度在构造上排除。
    pub(super) fn claim_pre_init_reconnect(&self) -> bool {
        let _admission = self.command_admission.lock();
        if self.stopping.load(Ordering::Acquire) || self.reconnect_pending.load(Ordering::Acquire) {
            return false;
        }
        self.reconnect_pending.store(true, Ordering::Release);
        true
    }

    pub(super) fn claim_reconnect_with_token(
        &self,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> bool {
        let _admission = self.command_admission.lock();
        if self.stopping.load(Ordering::Acquire) || self.reconnect_pending.load(Ordering::Acquire) {
            return false;
        }
        // A SwarmEvent::Disconnect has no client entity and can also be
        // emitted by an old per-add copy task.  Once a current owner is bound,
        // only the canonical ECS disconnect admission may authorize it.  The
        // precise source runs before the high-level listener, so a real
        // current disconnect has already set this bit.
        let disconnect_reported = self.disconnect_reported.load(Ordering::Acquire);
        let current_epoch = self.writer.lock().connection_epoch;
        let (owner, attempt) = {
            let producer = self.entity_producer.lock();
            if let Some(token) = attempt_token {
                // Stamped swarm disconnect: the token must belong to the
                // current epoch. A stale A swarm copy can never claim B.
                if !producer.source_token_bindings.matches(token, current_epoch) {
                    return false;
                }
            } else if producer.source_fence.ambiguous
                || producer.source_fence.pending_rebind_entity.is_some()
            {
                // An entity-less swarm event without a token cannot prove
                // whether it belongs to A or B.  It must not close the
                // current B owner.
                return false;
            }
            (producer.owner, producer.attempt)
        };
        if !disconnect_reported
            && (owner.is_some()
                || matches!(
                    attempt,
                    AttemptAdmissionState::Reserved {
                        reconnect_token: Some(_),
                        ..
                    }
                ))
        {
            // A current bound attempt, or a reconnect reservation that is
            // already being added, must first receive its canonical close
            // evidence.  This is the finite barrier that keeps an old
            // entity-less swarm copy from claiming B.
            return false;
        }
        self.reconnect_pending.store(true, Ordering::Release);
        // SwarmEvent::Disconnect is the lifecycle-wide fallback and carries
        // no client entity. Deactivate the owner selected by this same
        // admission; a late entity-specific Event::Disconnect cannot use this
        // path because it remains owner-gated.
        self.deactivate_current_entity_producer_owner();
        true
    }

    pub(super) fn admit_reconnect_attempt(&self) -> Option<u64> {
        let (token, should_drain) = {
            let _admission = self.command_admission.lock();
            if self.stopping.load(Ordering::Acquire)
                || !self.reconnect_pending.load(Ordering::Acquire)
            {
                return None;
            }
            let token = checked_atomic_increment(&self.reconnect_attempt_token)?;
            let Some(should_drain) = self.begin_connection_attempt_locked(Some(token)) else {
                return None;
            };
            (token, should_drain)
        };
        if should_drain {
            self.drain_events();
        }
        self.phase_cancel.notify_waiters();
        self.stable_cancel.notify_waiters();
        Some(token)
    }

    pub(super) fn reconnect_add_is_allowed(&self, token: u64) -> bool {
        let _admission = self.command_admission.lock();
        !self.stopping.load(Ordering::Acquire)
            && self.reconnect_add_pending.load(Ordering::Acquire)
            && self.reconnect_attempt_token.load(Ordering::Acquire) == token
    }

    pub(super) fn finish_reconnect_attempt(&self, token: u64) {
        let _admission = self.command_admission.lock();
        if self.reconnect_attempt_token.load(Ordering::Acquire) == token {
            self.reconnect_add_pending.store(false, Ordering::Release);
            let mut producer = self.entity_producer.lock();
            if matches!(
                producer.attempt,
                AttemptAdmissionState::Reserved {
                    reconnect_token: Some(current),
                    ..
                } if current == token
            ) {
                let epoch = self.writer.lock().connection_epoch;
                producer.attempt = AttemptAdmissionState::Closed { epoch };
                producer.deactivate_scope();
            }
        }
        self.reconnect_pending.store(false, Ordering::Release);
    }

    pub(super) fn context(&self) -> (String, u64, String) {
        self.writer.lock().context()
    }

    pub(super) fn set_dimension(&self, dimension: impl Into<String>) -> Option<String> {
        let dimension = dimension.into();
        self.writer.lock().set_dimension(dimension.clone());
        self.reported_dimension.lock().replace(dimension)
    }

    pub(super) fn set_dimension_if_running(&self, dimension: impl Into<String>) -> bool {
        let _admission = self.command_admission.lock();
        if !self.command_execution_allowed_without_lock() {
            return false;
        }
        self.set_dimension(dimension);
        true
    }

    #[cfg(test)]
    pub(super) fn observe_dimension(&self, dimension: impl Into<String>) {
        let dimension = dimension.into();
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock() {
                return;
            }
            let Some(previous) = self.set_dimension(dimension.clone()) else {
                return;
            };
            if previous == dimension {
                return;
            }
            self.enqueue_event(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::DimensionChanged {
                    from: previous,
                    to: dimension,
                }),
            )
        };
        if should_drain {
            self.drain_events();
        }
    }

    /// Consume the WorldLoaded boundary only when it still belongs to the
    /// current unstamped owner.  Dimension metadata and its optional event
    /// must share the same admission as the owner/fence check; a separate
    /// check-then-write would let a delayed A boundary update B.
    #[cfg(test)]
    pub(super) fn observe_dimension_from_world_boundary(
        &self,
        entity: bevy_ecs::entity::Entity,
        dimension: impl Into<String>,
    ) -> bool {
        self.observe_dimension_from_world_boundary_with_token(entity, dimension, None)
    }

    pub(super) fn observe_dimension_from_world_boundary_with_token(
        &self,
        entity: bevy_ecs::entity::Entity,
        dimension: impl Into<String>,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> bool {
        let dimension = dimension.into();
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock() {
                return false;
            }
            let epoch = self.writer.lock().connection_epoch;
            let producer = self.entity_producer.lock();
            if producer.owner != Some((entity, epoch)) {
                return false;
            }
            let admitted = match attempt_token {
                Some(token) => producer.source_token_bindings.matches(token, epoch),
                None => producer.source_fence.allows_unstamped(entity),
            };
            if !admitted {
                return false;
            }
            drop(producer);

            let Some(previous) = self.set_dimension(dimension.clone()) else {
                return true;
            };
            if previous == dimension {
                return true;
            }
            self.enqueue_event(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::DimensionChanged {
                    from: previous,
                    to: dimension,
                }),
            )
        };
        if should_drain {
            self.drain_events();
        }
        true
    }

    pub(super) fn connection_epoch(&self) -> u64 {
        self.writer.lock().connection_epoch
    }
}
