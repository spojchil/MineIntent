//! Epoch-bound observation reads and cancellable viewport projection.

use super::*;

const MAX_VIEWPORT_CAPTURE_ATTEMPTS: usize = 3;

/// One immutable observation generation captured before viewport projection.
#[derive(Clone)]
struct ViewportCapture {
    generation: u64,
    world: SharedWorld,
    world_bounds: WorldHeightBounds,
    pose: PoseSnapshot,
    entities: Vec<ProtocolEntitySnapshot>,
    source: FactSource,
}

enum ViewportReadAttempt {
    Complete(ViewportReadComplete),
    Retry,
}

enum ViewportReadComplete {
    Full(ContractViewportRead),
    Directed(DirectedViewportProjection),
}

#[derive(Clone)]
enum ViewportProjectionRequest {
    Full,
    Directed(Vec<ContractBlockPosition>),
}

enum ViewportProjectionWorkerResult {
    Complete {
        capture: ViewportCapture,
        projection: ViewportKernelProjection,
    },
    Retry,
}

enum ViewportKernelProjection {
    Full(ViewportProjection),
    Directed(DirectedViewportProjection),
}

/// 对齐 MineIntent `ProtocolObservationSource` 的只读 concrete observation seam。
///
/// `bound_epoch` 是创建 source 时捕获的值；所有 observation 方法都在读前后检查它。
#[derive(Clone)]
pub struct RuntimeObservationSource {
    pub(super) shared: Arc<SharedRuntime>,
    pub(super) bound_epoch: u64,
}

impl RuntimeObservationSource {
    pub fn epoch(&self) -> u64 {
        self.bound_epoch
    }

    pub(super) fn ensure_current_epoch(&self) -> Result<(), BackendError> {
        let current_epoch = self.shared.connection_epoch();
        if current_epoch != self.bound_epoch {
            return Err(BackendError::StaleEpoch {
                bound_epoch: self.bound_epoch,
                current_epoch,
            });
        }
        Ok(())
    }

    pub(super) fn self_pose_snapshot(&self) -> Result<Option<PoseSnapshot>, BackendError> {
        self.ensure_current_epoch()?;
        let pose = self
            .shared
            .observation
            .read()
            .snapshot
            .as_ref()
            .map(|snapshot| PoseSnapshot {
                position: snapshot.self_snapshot.position.clone(),
                velocity: snapshot.self_snapshot.velocity.clone(),
                yaw: snapshot.self_snapshot.yaw,
                pitch: snapshot.self_snapshot.pitch,
                on_ground: snapshot.self_snapshot.on_ground,
            });
        self.ensure_current_epoch()?;
        Ok(pose)
    }

    pub fn self_pose(&self) -> Result<ContractSelfPose, BackendError> {
        let pose = self.self_pose_snapshot()?;
        self.ensure_current_epoch()?;
        let pose = pose.ok_or_else(|| BackendError::NotReady {
            state: "self_pose_unavailable".to_owned(),
        })?;
        Ok(contract_self_pose(pose))
    }

    pub fn snapshot_source(&self) -> Result<Option<FactSource>, BackendError> {
        self.ensure_current_epoch()?;
        let source = self.shared.observation.read().source;
        self.ensure_current_epoch()?;
        Ok(source)
    }

    pub fn list_tracked_players(&self) -> Result<Vec<TrackedPlayerSnapshot>, BackendError> {
        self.ensure_current_epoch()?;
        let observation = self.shared.observation.read();
        let players = observation
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.tracked_players.clone())
            .unwrap_or_default();
        self.ensure_current_epoch()?;
        Ok(players)
    }

    pub(super) fn list_tracked_entities_snapshot(
        &self,
    ) -> Result<Vec<ProtocolEntitySnapshot>, BackendError> {
        self.ensure_current_epoch()?;
        let entities = self.shared.observation.read().tracked_entities.clone();
        self.ensure_current_epoch()?;
        Ok(entities)
    }

    pub fn list_tracked_entities(
        &self,
    ) -> Result<Vec<ContractProtocolEntitySnapshot>, BackendError> {
        let entities = self.list_tracked_entities_snapshot()?;
        let converted = entities
            .into_iter()
            .map(contract_entity_snapshot)
            .collect::<Result<Vec<_>, _>>()?;
        self.ensure_current_epoch()?;
        Ok(converted)
    }

    /// 对齐 MineIntent viewport 的只读投影；所有坐标仍是 Minecraft 世界绝对坐标。
    ///
    /// 投影不会把本地缓存的方块直接宣称为可见：它会对视锥内候选执行暴露面和
    /// 遮挡射线判断。这个旧方法保留可配置 kernel 的 backend seam，但不是 atomic
    /// VIEW-02 seam；它不会携带与 projection 同次 capture 的 source/revision。
    /// 需要三项一致结果时必须使用 `read_viewport(OperationControl)`。
    pub fn viewport(
        &self,
        options: &ViewportOptions,
    ) -> Result<Option<ViewportProjection>, BackendError> {
        self.ensure_current_epoch()?;
        let Some(pose) = self.self_pose_snapshot()? else {
            return Ok(None);
        };
        let entities = self.list_tracked_entities_snapshot()?;
        let Some(world) = self.shared.observation.read().world.clone() else {
            self.ensure_current_epoch()?;
            return Ok(None);
        };
        // 一次投影只读一个世界视图，避免候选扫描的每次体素访问都重新获取
        // RwLock；独立的 read_block() 仍保持短锁，供增量读取使用。
        let world = world.read();
        project_viewport(
            &pose,
            &entities,
            |position| read_block_from_world(&world, position),
            options,
        )
        .map(Some)
        .map_err(|message| BackendError::InvalidCommand {
            field: "viewport".to_owned(),
            message,
        })
        .and_then(|projection| {
            self.ensure_current_epoch()?;
            Ok(projection)
        })
    }

    /// 读取已加载世界中的绝对方块状态；结果不等于视线可见性。
    ///
    /// 上层 viewport 应基于 `transparentHint`、碰撞/轮廓几何和观察者姿态
    /// 做射线或暴露面判断，避免把“客户端缓存里有数据”误报成“玩家看到了”。
    pub fn read_block(
        &self,
        position: ContractBlockPosition,
    ) -> Result<ContractBlockReadResult, BackendError> {
        let result = self.read_block_with_post_read_hook(
            BlockPosition {
                x: position.x,
                y: position.y,
                z: position.z,
            },
            || {},
        )?;
        self.ensure_current_epoch()?;
        Ok(contract_block_read_result(result))
    }

    pub(super) fn read_block_with_post_read_hook(
        &self,
        position: BlockPosition,
        after_read: impl FnOnce(),
    ) -> Result<BlockReadResult, BackendError> {
        self.ensure_current_epoch()?;
        let Some(world) = self.shared.observation.read().world.clone() else {
            after_read();
            self.ensure_current_epoch()?;
            return Ok(BlockReadResult::Unloaded);
        };
        let world = world.read();
        let result = read_block_from_world(&world, position);
        after_read();
        self.ensure_current_epoch()?;
        Ok(result)
    }

    /// Read one coherent viewport capture and attach its provenance and read
    /// revision. The default options deliberately stay in the backend kernel;
    /// callers that need custom options may use the legacy non-atomic `viewport`
    /// method, but cannot combine it with `snapshot_source()` to form this seam.
    pub fn read_viewport(
        &self,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<ContractViewportRead, BackendError>> {
        Box::pin(async move {
            control.preflight("read_viewport")?;
            let request = ViewportProjectionRequest::Full;
            for attempt in 0..MAX_VIEWPORT_CAPTURE_ATTEMPTS {
                match self
                    .read_viewport_attempt(&control, request.clone())
                    .await
                    .map_err(backend_error_from_directed)?
                {
                    ViewportReadAttempt::Complete(ViewportReadComplete::Full(read)) => {
                        return Ok(read)
                    }
                    ViewportReadAttempt::Complete(ViewportReadComplete::Directed(_)) => {
                        unreachable!("full request cannot produce directed projection")
                    }
                    ViewportReadAttempt::Retry if attempt + 1 < MAX_VIEWPORT_CAPTURE_ATTEMPTS => {
                        control.preflight("read_viewport")?;
                        tokio::task::yield_now().await;
                    }
                    ViewportReadAttempt::Retry => {}
                }
            }
            control.preflight("read_viewport")?;
            self.ensure_current_epoch()?;
            Err(BackendError::NotReady {
                state: "viewport_capture_changed".to_owned(),
            })
        })
    }

    /// Read directed coordinates against the same atomic capture and viewport kernel as full.
    /// The captured world height is the only metadata used for zero-read out-of-world geometry
    /// classification; a target read that independently returns `OutOfWorld` becomes a row.
    pub fn read_directed_viewport(
        &self,
        positions: Vec<ContractBlockPosition>,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<DirectedViewportProjection, DirectedViewportError>> {
        Box::pin(async move {
            control.preflight("read_directed_viewport")?;
            let tuples = positions
                .iter()
                .map(|position| (position.x, position.y, position.z))
                .collect::<Vec<_>>();
            validate_directed_positions(&tuples).map_err(|message| {
                DirectedViewportError::Backend(BackendError::InvalidCommand {
                    field: "positions".to_owned(),
                    message,
                })
            })?;
            let request = ViewportProjectionRequest::Directed(positions);
            for attempt in 0..MAX_VIEWPORT_CAPTURE_ATTEMPTS {
                match self
                    .read_viewport_attempt(&control, request.clone())
                    .await?
                {
                    ViewportReadAttempt::Complete(ViewportReadComplete::Directed(projection)) => {
                        return Ok(projection)
                    }
                    ViewportReadAttempt::Complete(ViewportReadComplete::Full(_)) => {
                        unreachable!("directed request cannot produce full projection")
                    }
                    ViewportReadAttempt::Retry if attempt + 1 < MAX_VIEWPORT_CAPTURE_ATTEMPTS => {
                        control.preflight("read_directed_viewport")?;
                        tokio::task::yield_now().await;
                    }
                    ViewportReadAttempt::Retry => {}
                }
            }
            control.preflight("read_directed_viewport")?;
            self.ensure_current_epoch()?;
            Err(DirectedViewportError::Backend(BackendError::NotReady {
                state: "viewport_capture_changed".to_owned(),
            }))
        })
    }

    async fn read_viewport_attempt(
        &self,
        control: &OperationControl,
        request: ViewportProjectionRequest,
    ) -> Result<ViewportReadAttempt, DirectedViewportError> {
        self.ensure_current_epoch()?;
        let operation = match &request {
            ViewportProjectionRequest::Full => "read_viewport",
            ViewportProjectionRequest::Directed(_) => "read_directed_viewport",
        };
        control.preflight(operation)?;

        let (world, initial_generation) = {
            let observation = self.shared.observation.read();
            let writer = self.shared.writer.lock();
            if writer.connection_epoch != self.bound_epoch {
                return Err(DirectedViewportError::Backend(BackendError::StaleEpoch {
                    bound_epoch: self.bound_epoch,
                    current_epoch: writer.connection_epoch,
                }));
            }
            if !self.shared.ready.load(Ordering::Acquire) {
                return Err(DirectedViewportError::Backend(BackendError::NotReady {
                    state: "not_ready".to_owned(),
                }));
            }
            if observation.snapshot.is_none() {
                return Err(DirectedViewportError::Backend(BackendError::NotReady {
                    state: "viewport_snapshot_unavailable".to_owned(),
                }));
            }
            if observation.source.is_none() {
                return Err(DirectedViewportError::Backend(BackendError::NotReady {
                    state: "viewport_source_unavailable".to_owned(),
                }));
            }
            let Some(world) = observation.world.clone() else {
                return Err(DirectedViewportError::Backend(BackendError::NotReady {
                    state: "viewport_world_unavailable".to_owned(),
                }));
            };
            (world, observation.generation)
        };

        control.preflight(operation)?;
        let projection_shared = self.shared.clone();
        let projection_world = world.clone();
        let projection_initial_generation = initial_generation;
        let projection_bound_epoch = self.bound_epoch;
        let projection_control = control.clone();
        let projection_request = request;
        let mut projection_task = tokio::task::spawn_blocking(move || {
            // Acquire the world-owned read guard before cloning the state
            // values. This makes the world view and the published metadata one
            // capture while keeping the shared observation lock short-lived.
            let world_read = projection_world.read();
            let capture = {
                let observation = projection_shared.observation.read();
                let writer = projection_shared.writer.lock();
                if writer.connection_epoch != projection_bound_epoch {
                    return Err(DirectedViewportError::Backend(BackendError::StaleEpoch {
                        bound_epoch: projection_bound_epoch,
                        current_epoch: writer.connection_epoch,
                    }));
                }
                if !projection_shared.ready.load(Ordering::Acquire) {
                    return Err(DirectedViewportError::Backend(BackendError::NotReady {
                        state: "not_ready".to_owned(),
                    }));
                }
                if observation.generation != projection_initial_generation
                    || !observation
                        .world
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &projection_world))
                {
                    return Ok(ViewportProjectionWorkerResult::Retry);
                }
                let Some(snapshot) = observation.snapshot.as_ref() else {
                    return Err(DirectedViewportError::Backend(BackendError::NotReady {
                        state: "viewport_snapshot_unavailable".to_owned(),
                    }));
                };
                if snapshot.connection_epoch != writer.connection_epoch {
                    return Err(DirectedViewportError::Backend(BackendError::NotReady {
                        state: "viewport_snapshot_epoch_mismatch".to_owned(),
                    }));
                }
                let Some(source) = observation.source else {
                    return Err(DirectedViewportError::Backend(BackendError::NotReady {
                        state: "viewport_source_unavailable".to_owned(),
                    }));
                };
                ViewportCapture {
                    generation: observation.generation,
                    world: projection_world.clone(),
                    world_bounds: WorldHeightBounds::new(
                        world_read.chunks.min_y(),
                        world_read.chunks.height(),
                    ),
                    pose: PoseSnapshot {
                        position: snapshot.self_snapshot.position.clone(),
                        velocity: snapshot.self_snapshot.velocity.clone(),
                        yaw: snapshot.self_snapshot.yaw,
                        pitch: snapshot.self_snapshot.pitch,
                        on_ground: snapshot.self_snapshot.on_ground,
                    },
                    entities: observation.tracked_entities.clone(),
                    source,
                }
            };
            projection_control.preflight(operation)?;
            let projection = match projection_request {
                ViewportProjectionRequest::Full => ViewportKernelProjection::Full(
                    project_viewport_with_checkpoint(
                        &capture.pose,
                        &capture.entities,
                        |position| read_block_from_world(&world_read, position),
                        &ViewportOptions::default(),
                        || projection_control.preflight(operation),
                    )
                    .map_err(DirectedViewportError::Backend)?,
                ),
                ViewportProjectionRequest::Directed(positions) => {
                    let positions = positions
                        .into_iter()
                        .map(|position| [position.x, position.y, position.z])
                        .collect::<Vec<_>>();
                    ViewportKernelProjection::Directed(project_directed_viewport(
                        &capture.pose,
                        &positions,
                        |position| read_block_from_world(&world_read, position),
                        &ViewportOptions::default(),
                        capture.world_bounds,
                        || projection_control.preflight(operation),
                    )?)
                }
            };
            Ok(ViewportProjectionWorkerResult::Complete {
                capture,
                projection,
            })
        });
        let cancellation = control.cancelled();
        let deadline = async {
            if let Some(deadline) = control.deadline_elapsed() {
                deadline.await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::pin!(cancellation);
        tokio::pin!(deadline);
        let worker_result = tokio::select! {
            result = &mut projection_task => result
                .map_err(|error| DirectedViewportError::Backend(BackendError::BackendFailure {
                    failure: BackendFailure {
                        code: BackendFailureCode::ProtocolError,
                        message: format!("viewport projection task failed: {error}"),
                        retryable: true,
                    },
                }))??,
            _ = &mut cancellation => {
                projection_task.abort();
                return Err(DirectedViewportError::Backend(control_wakeup_error(
                    control, operation,
                )));
            }
            _ = &mut deadline => {
                projection_task.abort();
                return Err(DirectedViewportError::Backend(control_wakeup_error(
                    control, operation,
                )));
            }
        };
        let (capture, projection) = match worker_result {
            ViewportProjectionWorkerResult::Complete {
                capture,
                projection,
            } => (capture, projection),
            ViewportProjectionWorkerResult::Retry => return Ok(ViewportReadAttempt::Retry),
        };
        control.preflight(operation)?;

        let observation = self.shared.observation.read();
        let writer = self.shared.writer.lock();
        if writer.connection_epoch != self.bound_epoch {
            return Err(DirectedViewportError::Backend(BackendError::StaleEpoch {
                bound_epoch: self.bound_epoch,
                current_epoch: writer.connection_epoch,
            }));
        }
        if !self.shared.ready.load(Ordering::Acquire) {
            return Err(DirectedViewportError::Backend(BackendError::NotReady {
                state: "not_ready".to_owned(),
            }));
        }
        if observation.generation != capture.generation
            || !observation
                .world
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &capture.world))
        {
            return Ok(ViewportReadAttempt::Retry);
        }
        if observation.source != Some(capture.source) {
            return Ok(ViewportReadAttempt::Retry);
        }
        let revision = self.shared.viewport_revision.fetch_add(1, Ordering::AcqRel) + 1;
        let complete = match projection {
            ViewportKernelProjection::Full(projection) => {
                ViewportReadComplete::Full(ContractViewportRead {
                    projection: contract_viewport_projection(projection),
                    source: contract_fact_source(capture.source),
                    revision,
                })
            }
            ViewportKernelProjection::Directed(projection) => {
                ViewportReadComplete::Directed(projection)
            }
        };
        Ok(ViewportReadAttempt::Complete(complete))
    }

    pub(super) fn subscribe_listener(
        &self,
        listener: Arc<dyn ObservationEventListener>,
        post_register_hook: Option<&dyn Fn()>,
    ) -> Result<RuntimeObservationSubscription, BackendError> {
        self.ensure_current_epoch()?;
        let (id, state) = self
            .shared
            .add_observation_subscription(self.bound_epoch, listener);
        if let Some(hook) = post_register_hook {
            hook();
        }
        if let Err(error) = self.ensure_current_epoch() {
            self.shared.remove_observation_subscription(id, &state);
            return Err(error);
        }
        Ok(RuntimeObservationSubscription {
            shared: self.shared.clone(),
            id,
            state,
            closed: false,
        })
    }

    #[cfg(test)]
    pub(super) fn subscribe_with_post_register_hook(
        &self,
        listener: Arc<dyn ObservationEventListener>,
        hook: impl Fn(),
    ) -> Result<RuntimeObservationSubscription, BackendError> {
        self.subscribe_listener(listener, Some(&hook))
    }
}

impl ProtocolObservationSource for RuntimeObservationSource {
    fn epoch(&self) -> u64 {
        RuntimeObservationSource::epoch(self)
    }

    fn self_pose(&self) -> Result<ContractSelfPose, BackendError> {
        RuntimeObservationSource::self_pose(self)
    }

    fn list_tracked_entities(&self) -> Result<Vec<ContractProtocolEntitySnapshot>, BackendError> {
        RuntimeObservationSource::list_tracked_entities(self)
    }

    fn read_block(
        &self,
        position: ContractBlockPosition,
    ) -> Result<ContractBlockReadResult, BackendError> {
        RuntimeObservationSource::read_block(self, position)
    }

    fn subscribe(
        &self,
        listener: Arc<dyn ObservationEventListener>,
    ) -> Result<Box<dyn Subscription>, BackendError> {
        self.subscribe_listener(listener, None)
            .map(|subscription| Box::new(subscription) as Box<dyn Subscription>)
    }

    fn read_viewport(
        &self,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<ContractViewportRead, BackendError>> {
        // Fully qualify the inherent method so the trait adapter cannot recurse.
        RuntimeObservationSource::read_viewport(self, control)
    }

    fn read_directed_viewport(
        &self,
        positions: Vec<ContractBlockPosition>,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<DirectedViewportProjection, DirectedViewportError>> {
        RuntimeObservationSource::read_directed_viewport(self, positions, control)
    }
}

fn control_wakeup_error(control: &OperationControl, operation: &str) -> BackendError {
    match control.preflight(operation) {
        Err(error) => error,
        Ok(()) => BackendError::BackendFailure {
            failure: BackendFailure {
                code: BackendFailureCode::ProtocolError,
                message: format!("{operation} control woke without cancellation or deadline"),
                retryable: true,
            },
        },
    }
}
