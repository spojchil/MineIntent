use super::*;

#[tokio::test]
async fn read_viewport_returns_projection_source_and_unique_revision() {
    let (_handle, source, _world) = ready_viewport_source();

    let first = source
        .read_viewport(no_deadline_control())
        .await
        .expect("ready viewport read should succeed");
    assert_eq!(first.projection.frame.self_pose.position, [1.0, 64.0, 2.0]);
    assert_eq!(
        first.source,
        mineintent_contracts::minecraft::FactSource::ServerObserved
    );
    assert!(first.revision > 0);

    let second = source
        .read_viewport(no_deadline_control())
        .await
        .expect("second ready viewport read should succeed");
    assert!(second.revision > first.revision);
    assert_eq!(second.projection, first.projection);
    assert_eq!(second.source, first.source);
}

#[tokio::test]
async fn read_directed_viewport_uses_atomic_capture_and_revision_discipline() {
    let (handle, source, _world) = ready_viewport_source();
    let position = ContractBlockPosition { x: 0, y: 64, z: -1 };
    let first = source
        .read_directed_viewport(vec![position], no_deadline_control())
        .await
        .expect("directed unloaded target should still return a strict result");
    assert!(first.seen.is_empty());
    assert_eq!(first.unseen.len(), 1);
    assert_eq!(first.unseen[0].at, [0, 64, -1]);
    assert!(first.unseen[0]
        .why
        .contains(&mineintent_contracts::minecraft::DirectedWhy::ChunkNotLoaded));
    assert!(first.unseen[0].by.is_none());
    let revision = handle.shared.viewport_revision.load(Ordering::Acquire);

    let second = source
        .read_directed_viewport(vec![position], no_deadline_control())
        .await
        .expect("second directed read should succeed");
    assert_eq!(second, first);
    assert_eq!(
        handle.shared.viewport_revision.load(Ordering::Acquire),
        revision + 1
    );
}

#[tokio::test]
async fn read_directed_viewport_rejects_duplicates_and_serializes_out_of_world_rows() {
    let (handle, source, _world) = ready_viewport_source();
    let duplicate = source
        .read_directed_viewport(
            vec![
                ContractBlockPosition { x: 0, y: 64, z: -1 },
                ContractBlockPosition { x: 0, y: 64, z: -1 },
            ],
            no_deadline_control(),
        )
        .await;
    assert!(matches!(
        duplicate,
        Err(DirectedViewportError::Backend(BackendError::InvalidCommand {
            field,
            ..
        })) if field == "positions"
    ));

    {
        let mut observation = handle.shared.observation.write();
        observation.snapshot = Some(snapshot_at(1, 0.0, 9_999.0, 0.0));
        observation.bump_generation();
    }
    let out_of_world = handle
        .observation_source()
        .read_directed_viewport(
            vec![ContractBlockPosition {
                x: 0,
                y: 10_000,
                z: -1,
            }],
            no_deadline_control(),
        )
        .await;
    let out_of_world = out_of_world.expect("out-of-world coordinates are row-wise answers");
    assert!(out_of_world.seen.is_empty());
    assert_eq!(out_of_world.unseen.len(), 1);
    assert_eq!(out_of_world.unseen[0].at, [0, 10_000, -1]);
    assert_eq!(
        out_of_world.unseen[0].why,
        [mineintent_contracts::minecraft::DirectedWhy::OutOfWorld]
    );
    assert!(out_of_world.unseen[0].by.is_none());
    assert!(serde_json::to_value(&out_of_world.unseen[0])
        .unwrap()
        .get("block")
        .is_none());
}

#[tokio::test]
async fn read_directed_viewport_uses_current_world_height_bounds_rowwise() {
    let (handle, source, world) = ready_viewport_source();
    let (min_y, height) = {
        let world = world.read();
        (world.chunks.min_y(), world.chunks.height())
    };
    let upper_y = i32::try_from(i64::from(min_y) + i64::from(height))
        .expect("test world height upper bound fits i32 coordinates");

    let mut lower_snapshot = snapshot_at(1, 0.5, f64::from(min_y), 0.5);
    lower_snapshot.self_snapshot.pitch = -35.0;
    {
        let mut observation = handle.shared.observation.write();
        observation.snapshot = Some(lower_snapshot);
        observation.bump_generation();
    }
    let lower = source
        .read_directed_viewport(
            vec![
                ContractBlockPosition {
                    x: 0,
                    y: min_y - 1,
                    z: -3,
                },
                ContractBlockPosition {
                    x: 0,
                    y: min_y,
                    z: -3,
                },
            ],
            no_deadline_control(),
        )
        .await
        .expect("lower boundary should be answered per coordinate");
    assert_eq!(
        lower
            .unseen
            .iter()
            .find(|item| item.at == [0, min_y - 1, -3])
            .expect("lower out-of-world row")
            .why,
        [mineintent_contracts::minecraft::DirectedWhy::OutOfWorld]
    );
    assert!(lower
        .unseen
        .iter()
        .find(|item| item.at == [0, min_y, -3])
        .is_some_and(|item| {
            !item
                .why
                .contains(&mineintent_contracts::minecraft::DirectedWhy::OutOfWorld)
        }));

    let mut upper_snapshot = snapshot_at(1, 0.5, f64::from(upper_y - 1), 0.5);
    upper_snapshot.self_snapshot.pitch = 0.0;
    {
        let mut observation = handle.shared.observation.write();
        observation.snapshot = Some(upper_snapshot);
        observation.bump_generation();
    }
    let upper = source
        .read_directed_viewport(
            vec![
                ContractBlockPosition {
                    x: 0,
                    y: upper_y - 1,
                    z: -3,
                },
                ContractBlockPosition {
                    x: 0,
                    y: upper_y,
                    z: -3,
                },
            ],
            no_deadline_control(),
        )
        .await
        .expect("upper boundary should be answered per coordinate");
    assert_eq!(
        upper
            .unseen
            .iter()
            .find(|item| item.at == [0, upper_y, -3])
            .expect("upper out-of-world row")
            .why,
        [mineintent_contracts::minecraft::DirectedWhy::OutOfWorld]
    );
    assert!(upper
        .unseen
        .iter()
        .find(|item| item.at == [0, upper_y - 1, -3])
        .is_some_and(|item| {
            !item
                .why
                .contains(&mineintent_contracts::minecraft::DirectedWhy::OutOfWorld)
        }));
}

#[tokio::test]
async fn read_directed_viewport_cancel_and_deadline_keep_full_kernel_boundaries() {
    let (_handle, source, _world) = ready_viewport_source();
    let cancelled = source
        .read_directed_viewport(
            vec![ContractBlockPosition { x: 0, y: 64, z: -1 }],
            test_control(TestCancellation::new(true, None, false, None), None),
        )
        .await;
    assert_eq!(
        cancelled,
        Err(DirectedViewportError::Backend(BackendError::Cancelled {
            operation: "read_directed_viewport".to_owned()
        }))
    );

    let deadline = TestDeadline::new(true, None, None);
    let expired = source
        .read_directed_viewport(
            vec![ContractBlockPosition { x: 0, y: 64, z: -1 }],
            test_control(
                TestCancellation::new(false, None, false, None),
                Some(deadline),
            ),
        )
        .await;
    assert_eq!(
        expired,
        Err(DirectedViewportError::Backend(
            BackendError::DeadlineExceeded {
                operation: "read_directed_viewport".to_owned()
            }
        ))
    );
}

#[tokio::test]
async fn read_directed_viewport_worker_wakeup_preserves_operation_name() {
    let (_handle, source, _world) = ready_viewport_source();
    let cancellation = WorkerWakeCancellation::new();
    let cancelled = tokio::time::timeout(
        Duration::from_secs(1),
        source.read_directed_viewport(
            vec![ContractBlockPosition { x: 0, y: 64, z: -1 }],
            OperationControl::new(cancellation.clone(), None),
        ),
    )
    .await
    .expect("worker cancellation test must be bounded");
    assert!(cancellation.worker_started.load(Ordering::SeqCst));
    assert_eq!(
        cancelled,
        Err(DirectedViewportError::Backend(BackendError::Cancelled {
            operation: "read_directed_viewport".to_owned()
        }))
    );

    let (_handle, source, _world) = ready_viewport_source();
    let deadline = WorkerWakeDeadline::new();
    let expired = tokio::time::timeout(
        Duration::from_secs(1),
        source.read_directed_viewport(
            vec![ContractBlockPosition { x: 0, y: 64, z: -1 }],
            OperationControl::new(
                TestCancellation::new(false, None, false, None),
                Some(deadline.clone()),
            ),
        ),
    )
    .await
    .expect("worker deadline test must be bounded");
    assert!(deadline.worker_started.load(Ordering::SeqCst));
    assert_eq!(
        expired,
        Err(DirectedViewportError::Backend(
            BackendError::DeadlineExceeded {
                operation: "read_directed_viewport".to_owned()
            }
        ))
    );
}

#[tokio::test]
async fn read_directed_viewport_retries_generation_and_rejects_stale_epoch_atomically() {
    let (handle, source, world) = ready_viewport_source();
    let replacement = snapshot_at(1, 9.0, 64.0, 10.0);
    let shared = handle.shared.clone();
    let action: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let mut observation = shared.observation.write();
        observation.snapshot = Some(replacement.clone());
        observation.source = Some(FactSource::ClientPredicted);
        observation.tracked_entities = vec![observation_entity("replacement")];
        observation.world = Some(world.clone());
        observation.bump_generation();
    });
    let trigger = TestCancellation::new(false, Some(4), false, Some(action));
    let read = source
        .read_directed_viewport(
            vec![ContractBlockPosition { x: 0, y: 64, z: -1 }],
            test_control(trigger, None),
        )
        .await
        .expect("changed directed capture should retry");
    assert_eq!(read.unseen[0].at, [0, 64, -1]);

    let (handle, source, _world) = ready_viewport_source();
    let shared = handle.shared.clone();
    let action: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        shared.begin_connection_attempt();
    });
    let trigger = TestCancellation::new(false, Some(4), false, Some(action));
    let stale = source
        .read_directed_viewport(
            vec![ContractBlockPosition { x: 0, y: 64, z: -1 }],
            test_control(trigger, None),
        )
        .await;
    assert!(matches!(
        stale,
        Err(DirectedViewportError::Backend(BackendError::StaleEpoch {
            bound_epoch: 1,
            current_epoch: 2,
        }))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_viewport_revision_is_unique_for_concurrent_successes() {
    let (_handle, source, _world) = ready_viewport_source();
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let source = source.clone();
        tasks.push(tokio::spawn(async move {
            source
                .read_viewport(no_deadline_control())
                .await
                .expect("concurrent viewport read should succeed")
                .revision
        }));
    }

    let mut revisions = Vec::new();
    for task in tasks {
        revisions.push(task.await.expect("viewport task should not panic"));
    }
    revisions.sort_unstable();
    assert!(revisions.windows(2).all(|window| window[0] < window[1]));
}

#[tokio::test]
async fn read_viewport_preflight_cancel_and_deadline_do_not_scan() {
    let handle = RuntimeHandle::new(RunConfig::default());
    handle.shared.begin_connection_attempt();
    let source = handle.observation_source();

    let cancellation = TestCancellation::new(true, None, false, None);
    let cancelled = source
        .read_viewport(test_control(cancellation.clone(), None))
        .await;
    assert_eq!(
        cancelled,
        Err(BackendError::Cancelled {
            operation: "read_viewport".to_owned()
        })
    );
    assert_eq!(cancellation.checks.load(Ordering::SeqCst), 1);

    let deadline_cancellation = TestCancellation::new(false, None, false, None);
    let deadline = TestDeadline::new(true, None, None);
    let expired = source
        .read_viewport(test_control(
            deadline_cancellation.clone(),
            Some(deadline.clone()),
        ))
        .await;
    assert_eq!(
        expired,
        Err(BackendError::DeadlineExceeded {
            operation: "read_viewport".to_owned()
        })
    );
    assert_eq!(deadline_cancellation.checks.load(Ordering::SeqCst), 1);
    assert_eq!(deadline.checks.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn read_viewport_observes_cancellation_and_deadline_during_projection() {
    let (_handle, source, _world) = ready_viewport_source();

    let cancellation = TestCancellation::new(false, Some(7), true, None);
    let cancelled = source
        .read_viewport(test_control(cancellation.clone(), None))
        .await;
    assert_eq!(
        cancelled,
        Err(BackendError::Cancelled {
            operation: "read_viewport".to_owned()
        })
    );
    assert!(cancellation.checks.load(Ordering::SeqCst) >= 7);

    let deadline_cancellation = TestCancellation::new(false, None, false, None);
    let deadline = TestDeadline::new(false, Some(7), None);
    let expired = source
        .read_viewport(test_control(deadline_cancellation, Some(deadline.clone())))
        .await;
    assert_eq!(
        expired,
        Err(BackendError::DeadlineExceeded {
            operation: "read_viewport".to_owned()
        })
    );
    assert!(deadline.checks.load(Ordering::SeqCst) >= 7);
}

#[tokio::test]
async fn read_viewport_retries_when_capture_generation_changes() {
    let (handle, source, world) = ready_viewport_source();
    let replacement = snapshot_at(1, 9.0, 64.0, 10.0);
    let shared = handle.shared.clone();
    let action: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let mut observation = shared.observation.write();
        observation.snapshot = Some(replacement.clone());
        observation.source = Some(FactSource::ClientPredicted);
        observation.tracked_entities = vec![observation_entity("replacement")];
        observation.world = Some(world.clone());
        observation.bump_generation();
    });
    let trigger = TestCancellation::new(false, Some(4), false, Some(action));

    let read = source
        .read_viewport(test_control(trigger, None))
        .await
        .expect("a changed capture should be retried, not mixed");
    assert_eq!(read.projection.frame.self_pose.position, [9.0, 64.0, 10.0]);
    assert_eq!(
        read.source,
        mineintent_contracts::minecraft::FactSource::ClientPredicted
    );
}

#[tokio::test]
async fn read_viewport_rejects_epoch_change_after_capture() {
    let (handle, source, _world) = ready_viewport_source();
    let shared = handle.shared.clone();
    let action: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        shared.begin_connection_attempt();
    });
    let trigger = TestCancellation::new(false, Some(4), false, Some(action));

    let read = source.read_viewport(test_control(trigger, None)).await;
    assert_eq!(
        read,
        Err(BackendError::StaleEpoch {
            bound_epoch: 1,
            current_epoch: 2,
        })
    );
}

#[tokio::test]
async fn read_viewport_rejects_missing_ready_capture_parts() {
    let handle = RuntimeHandle::new(RunConfig::default());
    handle.shared.begin_connection_attempt();
    let source = handle.observation_source();
    handle.shared.ready.store(true, Ordering::Release);

    let missing_snapshot = source.read_viewport(no_deadline_control()).await;
    assert_eq!(
        missing_snapshot,
        Err(BackendError::NotReady {
            state: "viewport_snapshot_unavailable".to_owned()
        })
    );

    let world = empty_world();
    {
        let mut observation = handle.shared.observation.write();
        observation.snapshot = Some(observation_snapshot(1));
        observation.source = None;
        observation.world = Some(world.clone());
        observation.bump_generation();
    }
    let missing_source = source.read_viewport(no_deadline_control()).await;
    assert_eq!(
        missing_source,
        Err(BackendError::NotReady {
            state: "viewport_source_unavailable".to_owned()
        })
    );

    {
        let mut observation = handle.shared.observation.write();
        observation.source = Some(FactSource::ServerObserved);
        observation.bump_generation();
    }
    let ready = source.read_viewport(no_deadline_control()).await;
    assert!(ready.is_ok());

    {
        let mut observation = handle.shared.observation.write();
        observation.world = None;
        observation.bump_generation();
    }
    let missing_world = source.read_viewport(no_deadline_control()).await;
    assert_eq!(
        missing_world,
        Err(BackendError::NotReady {
            state: "viewport_world_unavailable".to_owned()
        })
    );
}
