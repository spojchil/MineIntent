//! Command admission, active-movement release, and stop finalization.

use super::*;

impl SharedRuntime {
    pub(super) fn command_execution_allowed(&self) -> bool {
        let _admission = self.command_admission.lock();
        self.command_execution_allowed_without_lock()
    }

    pub(super) fn with_command_admission<T>(&self, actuator: impl FnOnce() -> T) -> Result<T, ()> {
        let _admission = self.command_admission.lock();
        if !self.command_execution_allowed_without_lock() {
            return Err(());
        }
        // The closure contains only synchronous actuator operations.  It does
        // not await, emit events, or invoke completion callbacks while the
        // admission lock is held.
        Ok(actuator())
    }

    pub(super) fn client_command_error_locked(
        &self,
        client: &Client,
        connection_epoch: u64,
        operation: &str,
    ) -> Option<BackendError> {
        let current_epoch = self.writer.lock().connection_epoch;
        if current_epoch != connection_epoch {
            return Some(BackendError::StaleEpoch {
                bound_epoch: connection_epoch,
                current_epoch,
            });
        }
        if !self.command_execution_allowed_without_lock()
            || !self.client_is_current_owner_locked(client)
        {
            return Some(BackendError::Cancelled {
                operation: operation.to_owned(),
            });
        }
        None
    }

    pub(super) fn client_command_error(
        &self,
        client: &Client,
        connection_epoch: u64,
        operation: &str,
    ) -> Option<BackendError> {
        let _admission = self.command_admission.lock();
        self.client_command_error_locked(client, connection_epoch, operation)
    }

    pub(super) fn with_client_command_admission<T>(
        &self,
        client: &Client,
        connection_epoch: u64,
        operation: &str,
        actuator: impl FnOnce() -> T,
    ) -> Result<T, BackendError> {
        let _admission = self.command_admission.lock();
        if let Some(error) = self.client_command_error_locked(client, connection_epoch, operation) {
            return Err(error);
        }
        // The owner proof and the synchronous actuator share one admission
        // point; reconnect cannot replace the entity/token between them.
        Ok(actuator())
    }

    /// ConnectionFailed has already sealed the current attempt, so the normal
    /// command predicate (which rejects a disconnected attempt) is too
    /// narrow. This admits only the local disconnect actuator while keeping
    /// stop/stopped linearization on the same lock.
    pub(super) fn with_disconnect_admission<T>(
        &self,
        actuator: impl FnOnce() -> T,
    ) -> Result<T, ()> {
        let _admission = self.command_admission.lock();
        if self.stopping.load(Ordering::Acquire) || self.stopped_reported.load(Ordering::Acquire) {
            return Err(());
        }
        Ok(actuator())
    }

    pub(super) fn with_active_movement_admission<T>(
        &self,
        command_id: &str,
        generation: u64,
        completion: &Option<Arc<CommandCompletionState>>,
        actuator: impl FnOnce() -> T,
    ) -> Result<T, ()> {
        let _admission = self.command_admission.lock();
        if !self.command_execution_allowed_without_lock()
            || self.movement_generation.load(Ordering::Acquire) != generation
            || self.active_movement_id.lock().as_deref() != Some(command_id)
            || completion
                .as_ref()
                .is_some_and(|completion| completion.cancelled.load(Ordering::Acquire))
        {
            return Err(());
        }
        Ok(actuator())
    }

    pub(super) fn with_client_active_movement_admission<T>(
        &self,
        client: &Client,
        connection_epoch: u64,
        command_id: &str,
        generation: u64,
        completion: &Option<Arc<CommandCompletionState>>,
        actuator: impl FnOnce() -> T,
    ) -> Result<T, BackendError> {
        let _admission = self.command_admission.lock();
        let operation = format!("command:{command_id}");
        if let Some(error) =
            self.client_command_error_locked(client, connection_epoch, &operation)
        {
            return Err(error);
        }
        if self.movement_generation.load(Ordering::Acquire) != generation
            || self.active_movement_id.lock().as_deref() != Some(command_id)
            || completion
                .as_ref()
                .is_some_and(|completion| completion.cancelled.load(Ordering::Acquire))
        {
            return Err(BackendError::Cancelled { operation });
        }
        Ok(actuator())
    }

    pub(super) fn finalize_stop_if_ready(&self) {
        let should_drain = {
            let _admission = self.command_admission.lock();
            if !self.stopping.load(Ordering::Acquire)
                || self.active_movement_registration.load(Ordering::Acquire)
                || self.active_movement_completion.lock().is_some()
                || self.active_movement_id.lock().is_some()
            {
                return;
            }

            // The readiness check precedes taking the reason and both are
            // serialized with every cleanup caller. A second finalizer can no
            // longer observe an empty reason while the first one is about to
            // put it back.
            #[cfg(test)]
            self.invoke_finalize_stop_hook();

            let Some(reason) = self.stop_reason.lock().take() else {
                return;
            };
            self.enqueue_stopped_locked(reason, true)
        };
        if should_drain {
            self.drain_events();
        }
        self.request_shutdown();
    }

    /// 唯一的 Stopped reducer；正常 cleanup 与 watchdog 都通过这里发布终态。
    pub(super) fn enqueue_stopped_locked(&self, reason: String, cancel_watchdog: bool) -> bool {
        if self.stopped_reported.swap(true, Ordering::AcqRel) {
            return false;
        }
        self.invalidate_stop_watchdog_locked();
        // This is a one-shot permit, not a broadcast.  `notify_one` retains the
        // permit when the watchdog task has been spawned but has not been polled
        // yet, so normal cleanup cannot leave the long stop timer holding the
        // runtime alive.  The unique Stopped gate above keeps forced cleanup or
        // duplicate finalizers from issuing another permit.
        if cancel_watchdog {
            self.stop_watchdog_cancel.notify_one();
        }
        self.set_backend_state(BackendState::Stopped {
            reason: Some(reason.clone()),
        });
        self.enqueue_event(
            FactSource::Commanded,
            BackendEventPayload::Lifecycle(BackendLifecyclePayload::Stopped { reason }),
        )
    }

    pub(super) fn enqueue_command_if_running(
        &self,
        command: BackendCommandEnvelope,
        expected_epoch: Option<u64>,
    ) -> Result<(), String> {
        let _admission = self.command_admission.lock();
        if self.stopping.load(Ordering::Acquire) {
            return Err("runtime is stopping".to_owned());
        }
        let connection_epoch = self.writer.lock().connection_epoch;
        if expected_epoch.is_some_and(|expected| expected != connection_epoch) {
            return Err(format!(
                "stale command epoch: bound {expected_epoch:?}, current {connection_epoch}"
            ));
        }
        self.commands.lock().push_back(QueuedCommand {
            connection_epoch,
            envelope: command,
            completion: None,
        });
        Ok(())
    }

    pub(super) fn enqueue_command_with_completion_if_running(
        &self,
        command: BackendCommandEnvelope,
        completion: Arc<CommandCompletionState>,
        expected_epoch: Option<u64>,
    ) -> Result<(), String> {
        let _admission = self.command_admission.lock();
        if self.stopping.load(Ordering::Acquire) {
            return Err("runtime is stopping".to_owned());
        }
        let connection_epoch = self.writer.lock().connection_epoch;
        if expected_epoch.is_some_and(|expected| expected != connection_epoch) {
            return Err(format!(
                "stale command epoch: bound {expected_epoch:?}, current {connection_epoch}"
            ));
        }
        self.commands.lock().push_back(QueuedCommand {
            connection_epoch,
            envelope: command,
            completion: Some(completion),
        });
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn pop_command(&self) -> Option<QueuedCommand> {
        self.commands.lock().pop_front()
    }

    #[cfg(test)]
    pub(super) fn requeue_front(&self, command: QueuedCommand) {
        self.commands.lock().push_front(command);
    }

    pub(super) fn next_command_for_processing(&self) -> Option<QueuedCommand> {
        self.next_command_for_processing_with_hook(|| {})
    }

    /// Prove the high-level Client and dequeue one command under the same
    /// admission lock. A stale per-attempt Event copy must never consume work
    /// that the current owner would otherwise execute.
    pub(super) fn next_command_for_client(&self, client: &Client) -> Option<QueuedCommand> {
        let _admission = self.command_admission.lock();
        if !self.command_execution_allowed_without_lock()
            || !self.client_is_current_owner_locked(client)
        {
            return None;
        }
        self.next_command_for_processing()
    }

    pub(super) fn next_command_for_processing_with_hook(
        &self,
        before_decision: impl FnOnce(),
    ) -> Option<QueuedCommand> {
        let mut commands = self.commands.lock();
        before_decision();
        let should_defer = commands.front().is_some_and(|command| {
            !self.ready.load(Ordering::Acquire)
                && !matches!(&command.envelope.command, BackendCommand::Respawn)
        });
        if should_defer {
            return None;
        }
        let command = commands.pop_front()?;
        Some(command)
    }

    pub(super) fn cancel_pending_commands(&self) {
        for command in self.commands.lock().drain(..) {
            if let Some(completion) = command.completion {
                completion.cancel(format!("command:{}", command.envelope.id), false);
            }
        }
    }

    /// Declare a Move before touching the Azalea actuator.  The admission
    /// lock covers the only state transition that must be atomic with stop or
    /// disconnect: checking that the command may start and marking the
    /// registration window as live.  The rest of the registration is allowed
    /// to run without that lock so cancellation never waits on a callback or
    /// a bot operation.
    pub(super) fn register_active_movement(
        &self,
        command_id: &str,
        generation: u64,
        duration_ms: u64,
        completion: &Option<Arc<CommandCompletionState>>,
    ) -> ActiveMovementRegistration {
        self.register_active_movement_for_owner(
            None,
            command_id,
            generation,
            duration_ms,
            completion,
        )
    }

    pub(super) fn register_active_movement_for_client(
        &self,
        client: &Client,
        connection_epoch: u64,
        command_id: &str,
        generation: u64,
        duration_ms: u64,
        completion: &Option<Arc<CommandCompletionState>>,
    ) -> ActiveMovementRegistration {
        self.register_active_movement_for_owner(
            Some((client, connection_epoch)),
            command_id,
            generation,
            duration_ms,
            completion,
        )
    }

    fn register_active_movement_for_owner(
        &self,
        owner: Option<(&Client, u64)>,
        command_id: &str,
        generation: u64,
        duration_ms: u64,
        completion: &Option<Arc<CommandCompletionState>>,
    ) -> ActiveMovementRegistration {
        let admitted = {
            let _admission = self.command_admission.lock();
            let operation = format!("command:{command_id}");
            let allowed = owner.map_or_else(
                || self.command_execution_allowed_without_lock(),
                |(client, connection_epoch)| {
                    self.client_command_error_locked(client, connection_epoch, &operation)
                        .is_none()
                },
            );
            if allowed {
                self.active_movement_registration
                    .store(true, Ordering::Release);
                self.active_movement.store(true, Ordering::Release);
                *self.active_movement_id.lock() = Some(command_id.to_owned());
                true
            } else {
                false
            }
        };
        if !admitted {
            if let Some(completion) = completion {
                completion.cancel(format!("command:{command_id}"), false);
            }
            return ActiveMovementRegistration::Cancelled;
        }

        #[cfg(test)]
        self.invoke_active_movement_registration_hook();

        let cancel_signal = (duration_ms > 0).then(|| Arc::new(Notify::new()));
        *self.active_movement_cancel_signal.lock() = cancel_signal.clone();
        if let (Some(completion), Some(signal)) = (completion.as_ref(), cancel_signal.as_ref()) {
            completion.begin_active_release(signal.clone());
            *self.active_movement_completion.lock() = Some(completion.clone());
        }

        let cancelled = owner.map_or_else(
            || !self.command_execution_allowed(),
            |(client, connection_epoch)| {
                self.client_command_error(client, connection_epoch, &format!("command:{command_id}"))
                    .is_some()
            },
        )
            || completion
                .as_ref()
                .is_some_and(|completion| completion.cancelled.load(Ordering::Acquire));
        if cancelled {
            if let Some(completion) = completion {
                completion.cancel(format!("command:{command_id}"), true);
            }
            self.clear_registered_active_movement(
                command_id,
                generation,
                &cancel_signal,
                completion,
            );
            finish_command(
                completion,
                Err(BackendError::Cancelled {
                    operation: format!("command:{command_id}"),
                }),
            );
            self.finish_active_movement_registration();
            return ActiveMovementRegistration::Cancelled;
        }

        ActiveMovementRegistration::Started { cancel_signal }
    }

    pub(super) fn command_execution_allowed_without_lock(&self) -> bool {
        !self.stopping.load(Ordering::Acquire) && !self.disconnect_reported.load(Ordering::Acquire)
    }

    pub(super) fn arm_stop_watchdog_locked(&self) -> Option<StopWatchdogToken> {
        Some(StopWatchdogToken {
            generation: checked_atomic_increment(&self.stop_watchdog_generation)?,
        })
    }

    pub(super) fn invalidate_stop_watchdog_locked(&self) {
        let _ = checked_atomic_increment(&self.stop_watchdog_generation);
    }

    pub(super) fn spawn_stop_watchdog(self: &Arc<Self>, token: StopWatchdogToken) {
        if !self.timers_enabled.load(Ordering::Acquire) {
            return;
        }
        let shared = self.clone();
        let cancel = self.stop_watchdog_cancel.clone();
        let duration = Duration::from_millis(self.config.timeouts.stop_ms);
        #[cfg(test)]
        let completion_probe = self.stop_watchdog_completion_probe.lock().take();
        tokio::task::spawn_local(async move {
            tokio::select! {
                _ = tokio::time::sleep(duration) => shared.fire_stop_watchdog(token),
                _ = cancel.notified() => {}
            }
            #[cfg(test)]
            {
                // Drop the task's last RuntimeShared capture before resolving
                // the probe, so the test can also verify Arc lifetime rather
                // than merely observing a late inert callback.
                drop(shared);
                if let Some(probe) = completion_probe {
                    let _ = probe.send(());
                }
            }
        });
    }

    pub(super) fn fire_stop_watchdog(&self, token: StopWatchdogToken) {
        let (reason, should_drain, client, swarm, completion, pending) = {
            let _admission = self.command_admission.lock();
            if !self.stopping.load(Ordering::Acquire)
                || self.stopped_reported.load(Ordering::Acquire)
                || self.stop_watchdog_generation.load(Ordering::Acquire) != token.generation
            {
                return;
            }
            self.invalidate_stop_watchdog_locked();
            let reason = self
                .stop_reason
                .lock()
                .take()
                .unwrap_or_else(|| "stop_watchdog".to_owned());
            self.phase_generation.store(u64::MAX, Ordering::Release);
            self.stable_generation.store(u64::MAX, Ordering::Release);
            self.reconnect_add_pending.store(false, Ordering::Release);
            self.reconnect_pending.store(false, Ordering::Release);
            let _ = checked_atomic_increment(&self.reconnect_attempt_token);
            self.ready.store(false, Ordering::Release);
            self.active_movement_registration
                .store(false, Ordering::Release);
            self.active_movement.store(false, Ordering::Release);
            self.movement_generation.store(u64::MAX, Ordering::Release);
            *self.active_movement_id.lock() = None;
            self.active_movement_cancel_signal.lock().take();
            let completion = self.active_movement_completion.lock().take();
            let client = self.active_client.lock().take();
            let swarm = self.swarm.lock().take();
            let pending = self
                .commands
                .lock()
                .drain(..)
                .filter_map(|command| command.completion)
                .collect::<Vec<_>>();
            // The watchdog task is already executing this forced path; do not
            // leave a cancellation permit behind for a hypothetical later
            // generation.  The unique reducer still seals Stopped exactly once.
            let should_drain = self.enqueue_stopped_locked(reason.clone(), false);
            (reason, should_drain, client, swarm, completion, pending)
        };

        self.phase_cancel.notify_waiters();
        self.stable_cancel.notify_waiters();
        self.reconnect_cancel.notify_one();
        if let Some(completion) = completion {
            completion.cancel("stop_watchdog".to_owned(), false);
            completion.finish(Err(BackendError::Cancelled {
                operation: "stop_watchdog".to_owned(),
            }));
        }
        for completion in pending {
            completion.cancel("stop_watchdog".to_owned(), false);
            completion.finish(Err(BackendError::Cancelled {
                operation: "stop_watchdog".to_owned(),
            }));
        }
        self.clear_observations();
        if let Some(client) = client {
            client.disconnect();
        }
        if let Some(swarm) = swarm {
            swarm.exit();
        }
        if should_drain {
            self.drain_events();
        }
        let _ = reason;
        self.request_shutdown();
    }

    pub(super) fn clear_registered_active_movement(
        &self,
        command_id: &str,
        generation: u64,
        cancel_signal: &Option<Arc<Notify>>,
        completion: &Option<Arc<CommandCompletionState>>,
    ) -> bool {
        let owns_active_id = self.movement_generation.load(Ordering::Acquire) == generation
            && self.active_movement_id.lock().as_deref() == Some(command_id);
        if owns_active_id {
            self.active_movement.store(false, Ordering::Release);
            *self.active_movement_id.lock() = None;
        }

        if let Some(expected_signal) = cancel_signal {
            let mut current_signal = self.active_movement_cancel_signal.lock();
            if current_signal
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, expected_signal))
            {
                current_signal.take();
            }
        }
        if let Some(expected_completion) = completion {
            let mut current_completion = self.active_movement_completion.lock();
            if current_completion
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, expected_completion))
            {
                current_completion.take();
            }
        }
        owns_active_id
    }

    pub(super) fn clear_idle_movement_state(&self, generation: u64) {
        let _admission = self.command_admission.lock();
        if self.movement_generation.load(Ordering::Acquire) != generation
            || self.active_movement_registration.load(Ordering::Acquire)
            || self.active_movement.load(Ordering::Acquire)
            || self.active_movement_id.lock().is_some()
        {
            return;
        }
        self.active_movement_cancel_signal.lock().take();
        self.active_movement_completion.lock().take();
    }

    pub(super) fn finish_active_movement_registration(&self) {
        self.active_movement_registration
            .store(false, Ordering::Release);
        self.finalize_stop_if_ready();
    }

    pub(super) fn cancel_registered_active_movement(
        &self,
        command_id: &str,
        generation: u64,
        cancel_signal: &Option<Arc<Notify>>,
        completion: &Option<Arc<CommandCompletionState>>,
    ) {
        if let Some(completion) = completion {
            completion.cancel(format!("command:{command_id}"), true);
        }
        self.clear_registered_active_movement(command_id, generation, cancel_signal, completion);
        finish_command(
            completion,
            Err(BackendError::Cancelled {
                operation: format!("command:{command_id}"),
            }),
        );
        self.active_movement_registration
            .store(false, Ordering::Release);
        self.finalize_stop_if_ready();
    }

    pub(super) fn cancel_active_movement(
        &self,
        release_on_cancel: bool,
    ) -> Option<Arc<CommandCompletionState>> {
        if !release_on_cancel {
            self.movement_generation.fetch_add(1, Ordering::AcqRel);
        }
        let completion = self.active_movement_completion.lock().clone();
        let cancel_signal = self.active_movement_cancel_signal.lock().clone();
        if let Some(completion) = completion.as_ref() {
            completion.cancel(
                "movement superseded or stopped".to_owned(),
                release_on_cancel,
            );
        }
        if let Some(signal) = cancel_signal.as_ref() {
            signal.notify_one();
        }
        let deferred_release = release_on_cancel
            && (cancel_signal.is_some()
                || completion
                    .as_ref()
                    .is_some_and(|completion| completion.active_release.load(Ordering::Acquire)));
        if !deferred_release {
            self.active_movement.store(false, Ordering::Release);
            if let Some(completion) = completion.as_ref() {
                let mut active = self.active_movement_completion.lock();
                if active
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, completion))
                {
                    active.take();
                }
            }
            self.active_movement_cancel_signal.lock().take();
            *self.active_movement_id.lock() = None;
        } else {
            self.active_movement_cancel_signal.lock().take();
        }
        completion
    }

    pub(super) fn initiate_stop(self: &Arc<Self>, reason: &str) {
        // Signal cancellation before taking command admission. A producer may
        // already hold that lock while waiting for this finite dispatch queue;
        // stop must be able to wake that producer instead of waiting for it.
        self.stop_requested.store(true, Ordering::Release);
        self.cancel_event_admission();
        #[cfg(test)]
        self.invoke_stop_signal_hook();
        let watchdog_token = {
            // Make stop admission atomic with the start of a Move
            // registration.  Once either side owns this short lock, the
            // other side has a clear linearization point and stopped cannot
            // be finalized in the registration gap.
            let _admission = self.command_admission.lock();
            if self.stopping.swap(true, Ordering::AcqRel) {
                return;
            }
            let reason = reason.to_owned();
            *self.stop_reason.lock() = Some(reason.clone());
            let epoch = self.connection_epoch();
            self.set_backend_state(BackendState::Stopping {
                epoch: (epoch != 0).then_some(epoch),
                reason,
            });
            self.reconnect_add_pending.store(false, Ordering::Release);
            let _ = checked_atomic_increment(&self.reconnect_attempt_token);
            self.reconnect_pending.store(false, Ordering::Release);
            self.ready.store(false, Ordering::Release);
            self.invalidate_phase_locked();
            self.invalidate_stable_reset_locked();
            self.arm_stop_watchdog_locked()
        };
        self.reconnect_cancel.notify_one();
        self.phase_cancel.notify_waiters();
        self.stable_cancel.notify_waiters();
        if let Some(token) = watchdog_token {
            self.spawn_stop_watchdog(token);
        }
        self.cancel_pending_commands();
        if self.connection_epoch() > 0 && !self.disconnect_reported.load(Ordering::Acquire) {
            self.mark_disconnected(Some("deliberate_stop".to_owned()));
        } else {
            self.cancel_active_movement(true);
        }
        self.exit_swarm();
        self.finalize_stop_if_ready();
    }
}
