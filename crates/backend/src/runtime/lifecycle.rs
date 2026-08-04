//! Connection phases, lifecycle reduction, close classification, and reconnect timing.

use super::*;

#[derive(Clone, Debug)]
pub(super) struct CloseEvidence {
    code: String,
    retryable: bool,
    deliberate: bool,
    kick: Option<BackendKick>,
    error: Option<BackendCloseError>,
    end_reason: Option<String>,
    failure: Option<BackendFailure>,
}

impl SharedRuntime {
    pub(super) fn connection_identity(&self) -> (u64, String, u32) {
        let writer = self.writer.lock();
        let retry_ordinal = self.retry_ordinal.load(Ordering::Acquire);
        (
            writer.connection_epoch,
            writer.connection_attempt_id.clone(),
            u32::try_from(retry_ordinal).unwrap_or(u32::MAX),
        )
    }

    pub(super) fn arm_phase_deadline_locked(
        &self,
        phase: TransportPhase,
    ) -> Option<PhaseDeadlineToken> {
        let generation = checked_atomic_increment(&self.phase_generation)?;
        let epoch = self.writer.lock().connection_epoch;
        Some(PhaseDeadlineToken {
            epoch,
            attempt: self.retry_ordinal.load(Ordering::Acquire),
            phase,
            generation,
        })
    }

    pub(super) fn arm_stable_reset_locked(&self) -> Option<StableResetToken> {
        let generation = checked_atomic_increment(&self.stable_generation)?;
        let epoch = self.writer.lock().connection_epoch;
        Some(StableResetToken {
            epoch,
            attempt: self.retry_ordinal.load(Ordering::Acquire),
            generation,
        })
    }

    pub(super) fn invalidate_phase_locked(&self) {
        let _ = checked_atomic_increment(&self.phase_generation);
    }

    pub(super) fn invalidate_stable_reset_locked(&self) {
        let _ = checked_atomic_increment(&self.stable_generation);
    }

    pub(super) fn phase_timeout(&self, phase: TransportPhase) -> Duration {
        let millis = match phase {
            TransportPhase::Connecting => self.config.timeouts.connect_ms,
            TransportPhase::LoggingIn => self.config.timeouts.login_ms,
            TransportPhase::Spawning => self.config.timeouts.spawn_ms,
        };
        Duration::from_millis(millis)
    }

    pub(super) fn phase_deadline_matches_locked(&self, token: PhaseDeadlineToken) -> bool {
        if self.stopping.load(Ordering::Acquire)
            || self.stop_requested.load(Ordering::Acquire)
            || self.stopped_reported.load(Ordering::Acquire)
            || self.disconnect_reported.load(Ordering::Acquire)
            || self.phase_generation.load(Ordering::Acquire) != token.generation
            || self.retry_ordinal.load(Ordering::Acquire) != token.attempt
            || self.writer.lock().connection_epoch != token.epoch
        {
            return false;
        }
        let attempt = u32::try_from(token.attempt).unwrap_or(u32::MAX);
        match self.backend_state() {
            BackendState::Connecting {
                epoch,
                attempt: state_attempt,
                ..
            } => {
                token.phase == TransportPhase::Connecting
                    && epoch == token.epoch
                    && state_attempt == attempt
            }
            BackendState::LoggingIn {
                epoch,
                attempt: state_attempt,
                ..
            } => {
                token.phase == TransportPhase::LoggingIn
                    && epoch == token.epoch
                    && state_attempt == attempt
            }
            BackendState::Spawning {
                epoch,
                attempt: state_attempt,
                ..
            } => {
                token.phase == TransportPhase::Spawning
                    && epoch == token.epoch
                    && state_attempt == attempt
            }
            _ => false,
        }
    }

    pub(super) fn stable_reset_matches_locked(&self, token: StableResetToken) -> bool {
        if self.stopping.load(Ordering::Acquire)
            || self.stop_requested.load(Ordering::Acquire)
            || self.stopped_reported.load(Ordering::Acquire)
            || self.disconnect_reported.load(Ordering::Acquire)
            || self.stable_generation.load(Ordering::Acquire) != token.generation
            || self.retry_ordinal.load(Ordering::Acquire) != token.attempt
            || self.writer.lock().connection_epoch != token.epoch
        {
            return false;
        }
        matches!(
            self.backend_state(),
            BackendState::Ready { epoch, .. } if epoch == token.epoch
        )
    }

    pub(super) fn spawn_phase_deadline(self: &Arc<Self>, token: PhaseDeadlineToken) {
        // The pre-Init connect phase is deliberately not scheduled here:
        // Azalea 0.16 does not expose a safe cancellation handle for its
        // already-polled add/start future.  Login and spawn begin only after
        // a Client/Init identity exists and are production-safe to cancel via
        // the active Client path.
        if token.phase == TransportPhase::Connecting || !self.timers_enabled.load(Ordering::Acquire)
        {
            return;
        }
        let shared = self.clone();
        let cancel = self.phase_cancel.clone();
        let duration = self.phase_timeout(token.phase);
        tokio::task::spawn_local(async move {
            tokio::select! {
                _ = tokio::time::sleep(duration) => shared.fire_phase_deadline(token),
                _ = cancel.notified() => {}
            }
        });
    }

    pub(super) fn spawn_stable_reset(self: &Arc<Self>, token: StableResetToken) {
        if !self.timers_enabled.load(Ordering::Acquire) {
            return;
        }
        let shared = self.clone();
        let cancel = self.stable_cancel.clone();
        let duration = Duration::from_millis(self.config.reconnect.stable_reset_ms);
        tokio::task::spawn_local(async move {
            tokio::select! {
                _ = tokio::time::sleep(duration) => shared.fire_stable_reset(token),
                _ = cancel.notified() => {}
            }
        });
    }

    pub(super) fn fire_stable_reset(&self, token: StableResetToken) {
        let _admission = self.command_admission.lock();
        if self.stable_reset_matches_locked(token) {
            self.retry_ordinal.store(0, Ordering::Release);
        }
    }

    pub(super) fn timeout_close_evidence(phase: TransportPhase) -> CloseEvidence {
        let code = match phase {
            TransportPhase::Connecting => "connection_timeout",
            TransportPhase::LoggingIn => "login_timeout",
            TransportPhase::Spawning => "spawn_timeout",
        };
        CloseEvidence {
            code: code.to_owned(),
            retryable: true,
            deliberate: false,
            kick: None,
            error: None,
            end_reason: Some(code.to_owned()),
            failure: None,
        }
    }

    pub(super) fn fire_phase_deadline(&self, token: PhaseDeadlineToken) {
        let (close, should_drain, duplicate_cleanup, client) = {
            let _admission = self.command_admission.lock();
            if !self.phase_deadline_matches_locked(token) {
                return;
            }
            self.invalidate_phase_locked();
            let client = self.active_client.lock().clone();
            let result = self
                .mark_disconnected_evidence_locked(Self::timeout_close_evidence(token.phase), None);
            (result.0, result.1, result.2, client)
        };
        self.phase_cancel.notify_waiters();
        if duplicate_cleanup {
            self.cancel_active_movement(true);
            self.cancel_pending_commands();
            self.clear_observations();
        }
        if should_drain {
            self.drain_events();
        }
        if close.deliberate || self.stopping.load(Ordering::Acquire) {
            return;
        }
        // A login/spawn timeout has an active Client.  Let Azalea's canonical
        // DisconnectEvent/SwarmEvent path supply the account/join options for
        // the one reconnect policy; this avoids a second lifecycle reducer.
        if let Some(client) = client {
            client.disconnect();
        }
        if !self.config.reconnect.enabled {
            self.emit_faulted(self.failure_for_close(&close));
            self.request_shutdown();
        }
    }

    #[cfg(test)]
    pub(super) fn test_current_phase_token(&self, phase: TransportPhase) -> PhaseDeadlineToken {
        PhaseDeadlineToken {
            epoch: self.connection_epoch(),
            attempt: self.retry_ordinal.load(Ordering::Acquire),
            phase,
            generation: self.phase_generation.load(Ordering::Acquire),
        }
    }

    #[cfg(test)]
    pub(super) fn test_current_stable_token(&self) -> StableResetToken {
        StableResetToken {
            epoch: self.connection_epoch(),
            attempt: self.retry_ordinal.load(Ordering::Acquire),
            generation: self.stable_generation.load(Ordering::Acquire),
        }
    }

    #[cfg(test)]
    pub(super) fn test_current_stop_watchdog_token(&self) -> StopWatchdogToken {
        StopWatchdogToken {
            generation: self.stop_watchdog_generation.load(Ordering::Acquire),
        }
    }

    pub(super) fn set_swarm(&self, swarm: Swarm) -> bool {
        let _admission = self.command_admission.lock();
        if self.stopping.load(Ordering::Acquire) || self.stopped_reported.load(Ordering::Acquire) {
            return false;
        }
        *self.swarm.lock() = Some(swarm);
        true
    }

    pub(super) fn set_active_client_if_current(&self, client: &Client) -> bool {
        let _admission = self.command_admission.lock();
        let epoch = self.writer.lock().connection_epoch;
        if !self.command_execution_allowed_without_lock()
            || self.entity_producer.lock().owner != Some((client.entity, epoch))
        {
            return false;
        }
        let producer = self.entity_producer.lock();
        let source_token_matches = match client.attempt_token() {
            Some(token) => producer.source_token_bindings.matches(token, epoch),
            None => producer.source_fence.allows_unstamped(client.entity),
        };
        if !source_token_matches {
            return false;
        }
        drop(producer);
        *self.active_client.lock() = Some(client.clone());
        true
    }

    /// High-level `Event`s arrive paired with their `Client`.  Every side
    /// effect must first prove that the client still belongs to the current
    /// bound owner: a stamped client's token must match the current epoch's
    /// one-to-one binding, and an unstamped (legacy) client is only admitted
    /// while the legacy source fence still allows it.
    pub(super) fn client_is_current_owner(&self, client: &Client) -> bool {
        let _admission = self.command_admission.lock();
        self.client_is_current_owner_locked(client)
    }

    /// Caller holds `command_admission`. This is the identity half of command
    /// dequeue/actuation admission, so a reconnect cannot replace the owner
    /// between proving the Client and removing work from the queue.
    pub(super) fn client_is_current_owner_locked(&self, client: &Client) -> bool {
        let epoch = self.writer.lock().connection_epoch;
        let producer = self.entity_producer.lock();
        if producer.owner != Some((client.entity, epoch)) {
            return false;
        }
        match client.attempt_token() {
            Some(token) => producer.source_token_bindings.matches(token, epoch),
            None => producer.source_fence.allows_unstamped(client.entity),
        }
    }

    pub(super) fn close_evidence(&self, reason: Option<String>) -> CloseEvidence {
        let text = reason.clone().unwrap_or_default();
        let lower = text.to_ascii_lowercase();
        if text == "deliberate_stop" {
            return CloseEvidence {
                code: "deliberate_stop".to_owned(),
                retryable: false,
                deliberate: true,
                kick: None,
                error: None,
                end_reason: Some(text),
                failure: None,
            };
        }

        // A component attached to Event::Disconnect is already kick evidence;
        // its wording must not downgrade an unclassified kick to a retryable
        // ordinary connection end.
        let during_login = !self.ready.load(Ordering::Acquire);
        let server_shutdown = lower.contains("server_shutdown")
            || lower.contains("server shutdown")
            || lower.contains("server closed")
            || lower.contains("server restarting");
        if server_shutdown {
            return CloseEvidence {
                code: "server_shutdown".to_owned(),
                retryable: true,
                deliberate: false,
                kick: reason.map(|text| BackendKick { text, during_login }),
                error: None,
                end_reason: Some(text),
                failure: None,
            };
        }
        if lower.contains("banned")
            || lower.contains("whitelist")
            || lower.contains("invalid session")
            || lower.contains("authentication")
            || lower.contains("not authenticated")
        {
            let failure_code = if lower.contains("auth") || lower.contains("session") {
                BackendFailureCode::AuthenticationFailed
            } else {
                BackendFailureCode::PermissionDenied
            };
            return CloseEvidence {
                code: "permission_denied".to_owned(),
                retryable: false,
                deliberate: false,
                kick: Some(BackendKick {
                    text: text.clone(),
                    during_login,
                }),
                error: None,
                end_reason: Some(text.clone()),
                failure: Some(BackendFailure {
                    code: failure_code,
                    message: text,
                    retryable: false,
                }),
            };
        }
        if reason.is_some() {
            return CloseEvidence {
                code: "unclassified_kick".to_owned(),
                retryable: false,
                deliberate: false,
                kick: reason.map(|text| BackendKick { text, during_login }),
                error: None,
                end_reason: Some(text.clone()),
                failure: Some(BackendFailure {
                    code: BackendFailureCode::PermissionDenied,
                    message: text,
                    retryable: false,
                }),
            };
        }
        CloseEvidence {
            code: "connection_ended".to_owned(),
            retryable: true,
            deliberate: false,
            kick: None,
            error: None,
            end_reason: None,
            failure: None,
        }
    }

    #[cfg(test)]
    pub(super) fn emit_transport_connected(&self) {
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock() {
                return;
            }
            let (epoch, attempt_id, attempt) = self.connection_identity();
            self.set_backend_state(BackendState::LoggingIn {
                epoch,
                attempt_id,
                attempt,
            });
            self.enqueue_event(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::TransportConnected),
            )
        };
        if should_drain {
            self.drain_events();
        }
    }

    pub(super) fn emit_logged_in(self: &Arc<Self>, version: impl Into<String>, dimension: String) {
        let version = version.into();
        let (should_drain, phase_token) = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock() {
                return;
            }
            self.set_dimension(dimension.clone());
            let (epoch, attempt_id, attempt) = self.connection_identity();
            self.invalidate_stable_reset_locked();
            self.set_backend_state(BackendState::Spawning {
                epoch,
                attempt_id,
                attempt,
            });
            let phase_token = self.arm_phase_deadline_locked(TransportPhase::Spawning);
            let should_drain = self.enqueue_event(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::LoggedIn {
                    version,
                    dimension,
                }),
            );
            (should_drain, phase_token)
        };
        if should_drain {
            self.drain_events();
        }
        self.phase_cancel.notify_waiters();
        if let Some(token) = phase_token {
            self.spawn_phase_deadline(token);
        }
    }

    pub(super) fn emit_ready(self: &Arc<Self>, snapshot_revision: u64) {
        let (should_drain, stable_token) = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock() {
                return;
            }
            self.ready.store(true, Ordering::Release);
            let (epoch, attempt_id, _) = self.connection_identity();
            let ready_at = now_utc().to_rfc3339();
            self.invalidate_phase_locked();
            let stable_token = self.arm_stable_reset_locked();
            self.set_backend_state(BackendState::Ready {
                epoch,
                attempt_id,
                ready_at: ready_at.clone(),
            });
            let should_drain = self.enqueue_event_at(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::Ready {
                    snapshot_revision,
                }),
                ready_at,
            );
            (should_drain, stable_token)
        };
        if should_drain {
            self.drain_events();
        }
        self.phase_cancel.notify_waiters();
        self.stable_cancel.notify_waiters();
        if let Some(token) = stable_token {
            self.spawn_stable_reset(token);
        }
    }

    #[cfg(test)]
    pub(super) fn admit_death(&self) -> bool {
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock()
                || self.death_reported.swap(true, Ordering::AcqRel)
            {
                return false;
            }
            self.ready.store(false, Ordering::Release);
            let (epoch, attempt_id, _) = self.connection_identity();
            let died_at = now_utc().to_rfc3339();
            self.set_backend_state(BackendState::Dead {
                epoch,
                attempt_id,
                died_at: died_at.clone(),
            });
            self.enqueue_event_at(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::Died),
                died_at,
            )
        };
        if should_drain {
            self.drain_events();
        }
        true
    }

    /// Claim Death and finish all synchronous local movement cleanup before
    /// making `died` visible to subscribers. The event queue may already have
    /// another drainer, so enqueueing first and draining later would still let
    /// a re-entrant stop callback run before the physical release.
    pub(super) fn admit_death_and_release(
        &self,
        release_inputs: impl FnOnce() -> bool,
    ) -> Option<bool> {
        let (released, should_drain) = {
            let _admission = self.command_admission.lock();
            if !self.command_execution_allowed_without_lock()
                || self.death_reported.swap(true, Ordering::AcqRel)
            {
                return None;
            }
            self.ready.store(false, Ordering::Release);
            let (epoch, attempt_id, _) = self.connection_identity();
            let died_at = now_utc().to_rfc3339();
            self.set_backend_state(BackendState::Dead {
                epoch,
                attempt_id,
                died_at: died_at.clone(),
            });

            let movement_id = self.active_movement_id.lock().clone();
            let had_movement = movement_id.is_some()
                || self.active_movement_completion.lock().is_some()
                || self.active_movement_registration.load(Ordering::Acquire);
            let completion = self.active_movement_completion.lock().clone();
            let cancel_signal = self.active_movement_cancel_signal.lock().clone();
            if had_movement {
                self.movement_generation.fetch_add(1, Ordering::AcqRel);
                if let Some(completion) = completion.as_ref() {
                    completion.cancel("movement stopped by death".to_owned(), true);
                }
                if let Some(signal) = cancel_signal.as_ref() {
                    signal.notify_one();
                }
            }

            // This closure is synchronous and runs before `died` is enqueued;
            // no subscriber/callback can run while command admission is held.
            let released = release_inputs();
            self.active_movement.store(false, Ordering::Release);
            *self.active_movement_id.lock() = None;
            self.active_movement_cancel_signal.lock().take();
            self.active_movement_completion.lock().take();
            self.active_movement_registration
                .store(false, Ordering::Release);
            if let Some(completion) = completion {
                finish_command(
                    &Some(completion),
                    if released {
                        Err(BackendError::Cancelled {
                            operation: "movement stopped by death".to_owned(),
                        })
                    } else {
                        Err(command_component_failure("death move"))
                    },
                );
            }
            let should_drain = self.enqueue_event_at(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::Died),
                died_at,
            );
            (released, should_drain)
        };
        if should_drain {
            self.drain_events();
        }
        Some(released)
    }

    #[cfg(test)]
    pub(super) fn emit_died(&self) {
        let _ = self.admit_death();
    }

    pub(super) fn emit_respawn_transition_started(&self, from_dimension: String) {
        self.emit_if_running(
            FactSource::Commanded,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::RespawnTransitionStarted {
                from_dimension,
            }),
        );
    }

    pub(super) fn emit_respawned(&self, dimension: String) {
        self.emit_if_running(
            FactSource::ServerObserved,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Respawned { dimension }),
        );
    }

    pub(super) fn mark_disconnected(&self, reason: Option<String>) -> BackendClose {
        self.mark_disconnected_evidence(self.close_evidence(reason))
    }

    #[cfg(test)]
    pub(super) fn mark_connection_failed(&self, error: String) -> BackendClose {
        self.mark_disconnected_evidence(CloseEvidence {
            code: "connection_failed".to_owned(),
            retryable: true,
            deliberate: false,
            kick: None,
            error: Some(BackendCloseError {
                name: "connection_failed".to_owned(),
                message: error.clone(),
                code: None,
            }),
            end_reason: Some(error.clone()),
            failure: Some(BackendFailure {
                code: BackendFailureCode::ProtocolError,
                message: error,
                retryable: true,
            }),
        })
    }

    pub(super) fn mark_disconnected_evidence(&self, evidence: CloseEvidence) -> BackendClose {
        self.mark_disconnected_evidence_with_owner(evidence, None, None, false, None, None)
            .expect("unconditional disconnect admission cannot be rejected")
    }

    #[cfg(test)]
    pub(super) fn admit_canonical_disconnected(
        &self,
        entity: bevy_ecs::entity::Entity,
        source_epoch: u64,
        reason: Option<String>,
    ) -> Option<BackendClose> {
        self.admit_canonical_disconnected_with_token(entity, source_epoch, reason, None)
    }

    pub(super) fn admit_canonical_disconnected_with_token(
        &self,
        entity: bevy_ecs::entity::Entity,
        source_epoch: u64,
        reason: Option<String>,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<BackendClose> {
        self.mark_disconnected_evidence_with_owner(
            self.close_evidence(reason),
            Some(entity),
            Some(source_epoch),
            true,
            None,
            attempt_token,
        )
    }

    pub(super) fn admit_canonical_disconnected_source_with_token(
        &self,
        entity: bevy_ecs::entity::Entity,
        reason: Option<String>,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<BackendClose> {
        let source_epoch = self.connection_epoch();
        self.admit_canonical_disconnected_with_token(entity, source_epoch, reason, attempt_token)
    }

    #[cfg(test)]
    pub(super) fn admit_canonical_connection_failed(
        &self,
        entity: bevy_ecs::entity::Entity,
        source_epoch: u64,
        error: String,
    ) -> Option<BackendClose> {
        self.admit_canonical_connection_failed_with_token(entity, source_epoch, error, None)
    }

    pub(super) fn admit_canonical_connection_failed_with_token(
        &self,
        entity: bevy_ecs::entity::Entity,
        source_epoch: u64,
        error: String,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<BackendClose> {
        self.mark_disconnected_evidence_with_owner(
            CloseEvidence {
                code: "connection_failed".to_owned(),
                retryable: true,
                deliberate: false,
                kick: None,
                error: Some(BackendCloseError {
                    name: "connection_failed".to_owned(),
                    message: error.clone(),
                    code: None,
                }),
                end_reason: Some(error.clone()),
                failure: Some(BackendFailure {
                    code: BackendFailureCode::ProtocolError,
                    message: error,
                    retryable: true,
                }),
            },
            Some(entity),
            Some(source_epoch),
            true,
            Some(entity),
            attempt_token,
        )
    }

    pub(super) fn admit_canonical_connection_failed_source_with_token(
        &self,
        entity: bevy_ecs::entity::Entity,
        error: String,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<BackendClose> {
        let source_epoch = self.connection_epoch();
        self.admit_canonical_connection_failed_with_token(
            entity,
            source_epoch,
            error,
            attempt_token,
        )
    }

    #[cfg(test)]
    pub(super) fn take_canonical_connection_failure_followup(
        &self,
        entity: bevy_ecs::entity::Entity,
    ) -> bool {
        self.take_canonical_connection_failure_followup_with_token(entity, None)
    }

    pub(super) fn take_canonical_connection_failure_followup_with_token(
        &self,
        entity: bevy_ecs::entity::Entity,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> bool {
        let _admission = self.command_admission.lock();
        let epoch = self.writer.lock().connection_epoch;
        let mut producer = self.entity_producer.lock();
        let pending_matches = producer.pending_connection_failure == Some((entity, epoch))
            && attempt_token
                .is_none_or(|token| producer.source_token_bindings.matches(token, epoch));
        if pending_matches {
            producer.pending_connection_failure = None;
            true
        } else {
            false
        }
    }

    pub(super) fn mark_disconnected_evidence_with_owner(
        &self,
        evidence: CloseEvidence,
        entity: Option<bevy_ecs::entity::Entity>,
        expected_epoch: Option<u64>,
        allow_unbound_attempt: bool,
        failure_entity: Option<bevy_ecs::entity::Entity>,
        attempt_token: Option<azalea::join::AttemptToken>,
    ) -> Option<BackendClose> {
        let (close, should_drain, duplicate_cleanup) = {
            let _admission = self.command_admission.lock();
            if expected_epoch.is_some_and(|epoch| self.connection_epoch() != epoch) {
                return None;
            }
            if let Some(entity) = entity {
                let source_epoch = expected_epoch.unwrap_or_else(|| self.connection_epoch());
                if !self.admit_entity_lifecycle_owner_locked(
                    entity,
                    source_epoch,
                    allow_unbound_attempt,
                    attempt_token,
                ) {
                    return None;
                }
            }
            self.mark_disconnected_evidence_locked(evidence, failure_entity)
        };

        // A duplicate can race a registration that is finishing after the
        // first disconnect. Repeating cleanup is harmless and helps that
        // registration converge, while the first close already completed its
        // mandatory cleanup under admission above.
        if duplicate_cleanup {
            self.cancel_active_movement(true);
            self.cancel_pending_commands();
            self.clear_observations();
        }
        if should_drain {
            self.drain_events();
        }
        Some(close)
    }

    /// Caller holds `command_admission`; owner claiming and global close
    /// admission therefore share one lifecycle linearization point.
    pub(super) fn mark_disconnected_evidence_locked(
        &self,
        evidence: CloseEvidence,
        failure_entity: Option<bevy_ecs::entity::Entity>,
    ) -> (BackendClose, bool, bool) {
        if self.stopped_reported.load(Ordering::Acquire) {
            let close = self
                .last_close
                .lock()
                .clone()
                .unwrap_or_else(|| BackendClose {
                    epoch: self.connection_epoch(),
                    at: now_utc().to_rfc3339(),
                    code: "connection_ended".to_owned(),
                    retryable: true,
                    deliberate: false,
                    kick: None,
                    error: None,
                    end_reason: None,
                });
            return (close, false, false);
        }

        // Once stop has won admission, a late Azalea disconnect cannot
        // replace the caller's deliberate close evidence.
        let evidence = if self.stopping.load(Ordering::Acquire) && !evidence.deliberate {
            CloseEvidence {
                code: "deliberate_stop".to_owned(),
                retryable: false,
                deliberate: true,
                kick: None,
                error: None,
                end_reason: Some("deliberate_stop".to_owned()),
                failure: None,
            }
        } else {
            evidence
        };

        // Publish the disconnect bit and enqueue close under one admission
        // point. The queue is drained only after this lock is released.
        if self.disconnect_reported.swap(true, Ordering::AcqRel) {
            let close = self
                .last_close
                .lock()
                .clone()
                .unwrap_or_else(|| BackendClose {
                    epoch: self.connection_epoch(),
                    at: now_utc().to_rfc3339(),
                    code: "connection_ended".to_owned(),
                    retryable: true,
                    deliberate: false,
                    kick: None,
                    error: None,
                    end_reason: None,
                });
            (close, false, true)
        } else {
            self.ready.store(false, Ordering::Release);
            self.invalidate_phase_locked();
            self.invalidate_stable_reset_locked();
            self.active_client.lock().take();
            let close = BackendClose {
                epoch: self.connection_epoch(),
                at: now_utc().to_rfc3339(),
                code: evidence.code,
                retryable: evidence.retryable,
                deliberate: evidence.deliberate,
                kick: evidence.kick,
                error: evidence.error,
                end_reason: evidence.end_reason,
            };
            *self.last_close.lock() = Some(close.clone());
            *self.last_failure.lock() = evidence.failure;

            let mut producer = self.entity_producer.lock();
            producer.owner = None;
            producer.deactivate_scope();
            producer.attempt = AttemptAdmissionState::Closed { epoch: close.epoch };
            producer.pending_connection_failure = if close.code == "connection_failed" {
                failure_entity.map(|entity| (entity, close.epoch))
            } else {
                None
            };
            drop(producer);

            // Seal and clean the attempt before making its close visible.
            // Stop takes the same admission lock, so it cannot enqueue or
            // drain `stopped` between close admission and local cleanup.
            #[cfg(test)]
            self.invoke_disconnect_cleanup_hook();
            self.cancel_active_movement(true);
            self.cancel_pending_commands();
            self.clear_observations();

            let should_drain = self.enqueue_event(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::ConnectionClosed {
                    close: close.clone(),
                }),
            );
            (close, should_drain, false)
        }
    }

    pub(super) fn failure_for_close(&self, close: &BackendClose) -> BackendFailure {
        let recorded = self.last_failure.lock().clone();
        // A fatal classification is stronger than the reconnect policy.  In
        // particular, permission/auth/version failures must remain visible
        // instead of being rewritten as `reconnect_disabled`.
        if let Some(failure) = recorded.as_ref().filter(|failure| !failure.retryable) {
            return failure.clone();
        }
        if close.retryable && !self.config.reconnect.enabled {
            return BackendFailure {
                code: BackendFailureCode::ReconnectDisabled,
                message: format!("reconnect disabled after close {}", close.code),
                retryable: false,
            };
        }
        recorded.unwrap_or_else(|| BackendFailure {
            code: BackendFailureCode::ProtocolError,
            message: format!("backend closed with non-retryable code {}", close.code),
            retryable: false,
        })
    }

    pub(super) fn emit_faulted(&self, failure: BackendFailure) -> bool {
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.lifecycle_event_allowed_without_lock()
                || self.faulted_reported.swap(true, Ordering::AcqRel)
            {
                return false;
            }
            self.set_backend_state(BackendState::Faulted {
                failure: failure.clone(),
            });
            self.enqueue_event(
                FactSource::ServerObserved,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::Faulted { failure }),
            )
        };
        if should_drain {
            self.drain_events();
        }
        true
    }

    pub(super) fn emit_reconnect_scheduled(&self, close: &BackendClose) -> Option<Duration> {
        let current_attempt = self.retry_ordinal.load(Ordering::Acquire);
        let next_attempt = current_attempt.checked_add(1)?;
        let schedule = reconnect_schedule_at(
            &self.config.reconnect,
            current_attempt,
            next_reconnect_random(&self.reconnect_rng),
            now_utc(),
        );
        let retry_at = schedule.retry_at.to_rfc3339();
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.lifecycle_event_allowed_without_lock() {
                return None;
            }
            let attempt = u32::try_from(next_attempt).unwrap_or(u32::MAX);
            self.set_backend_state(BackendState::Reconnecting {
                attempt,
                retry_at: retry_at.clone(),
                last_close: close.clone(),
            });
            self.enqueue_event(
                FactSource::ClientPredicted,
                BackendEventPayload::Lifecycle(BackendLifecyclePayload::ReconnectScheduled {
                    attempt,
                    retry_at,
                    close_code: close.code.clone(),
                }),
            )
        };
        if should_drain {
            self.drain_events();
        }
        Some(schedule.delay)
    }

    pub(super) fn exit_swarm(&self) -> bool {
        if let Some(swarm) = self.swarm.lock().clone() {
            swarm.exit();
            true
        } else {
            false
        }
    }

    pub(super) fn request_shutdown(&self) {
        // `notify_one` 会保留一个 permit，即使 stop() 发生在 run() 开始
        // select 之前，也不会因为时序而永久等待。
        self.shutdown_requested.store(true, Ordering::Release);
        self.shutdown.notify_one();
        self.cancel_event_admission();
    }
}
