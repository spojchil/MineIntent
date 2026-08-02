mod information_provider_contract_support;

use std::{
    collections::{BTreeSet, VecDeque},
    future::pending,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use information_provider_contract_support::{read, request, ProviderFixture};
use mineintent_contracts::minecraft::{
    BackendError, BackendEventListener, BackendFailure, BackendFailureCode, BackendReady,
    BackendState, BlockPosition, BlockReadResult, BoxFuture, CancellationSignal, FactSource,
    MinecraftBackendApi, MinecraftMotorDriverApi, MinecraftSnapshotV1, ObservationEventListener,
    OperationControl, ProtocolObservationSource, SelfPose, Subscription, ViewportBlock,
    ViewportCoordinateSystem, ViewportFrame, ViewportLegend, ViewportProjection, ViewportRead,
    ViewportSelfPose, VisibleBlocksView, VisibleEntitiesView, VisibleEntityView,
};
use mineintent_middle::information::{
    contracts::{
        InformationAcquisition, InformationAudience, InformationInterfaceId, InformationPrecision,
        InformationProvider, InformationProviderError, InformationProviderLimits,
        InformationScopeDependency, InformationSourceKind,
    },
    providers::ViewportInformationProvider,
};
use serde_json::{json, Value};
use tokio::sync::oneshot;

struct TestCancellation(u64);

impl CancellationSignal for TestCancellation {
    fn is_cancelled(&self) -> bool {
        let _ = self.0;
        false
    }

    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(pending::<()>())
    }
}

struct ReadPlan {
    gate: Option<oneshot::Receiver<()>>,
    result: Result<ViewportRead, BackendError>,
}

impl ReadPlan {
    fn success(read: ViewportRead) -> Self {
        Self {
            gate: None,
            result: Ok(read),
        }
    }

    fn failure(error: BackendError) -> Self {
        Self {
            gate: None,
            result: Err(error),
        }
    }

    fn gated(gate: oneshot::Receiver<()>, read: ViewportRead) -> Self {
        Self {
            gate: Some(gate),
            result: Ok(read),
        }
    }
}

struct FakeObservationSource {
    plans: Mutex<VecDeque<ReadPlan>>,
    read_calls: AtomicUsize,
    legacy_calls: AtomicUsize,
    expected_cancellation: Option<Arc<dyn CancellationSignal>>,
    matching_controls: AtomicUsize,
}

impl FakeObservationSource {
    fn new(plans: Vec<ReadPlan>) -> Self {
        Self {
            plans: Mutex::new(plans.into()),
            read_calls: AtomicUsize::new(0),
            legacy_calls: AtomicUsize::new(0),
            expected_cancellation: None,
            matching_controls: AtomicUsize::new(0),
        }
    }

    fn with_expected_control(
        expected_cancellation: Arc<dyn CancellationSignal>,
        plans: Vec<ReadPlan>,
    ) -> Self {
        Self {
            expected_cancellation: Some(expected_cancellation),
            ..Self::new(plans)
        }
    }

    fn pop_plan(&self) -> ReadPlan {
        self.plans
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
            .unwrap_or_else(|| ReadPlan::failure(test_backend_error("unexpected viewport read")))
    }

    fn legacy_calls(&self) -> usize {
        self.legacy_calls.load(Ordering::SeqCst)
    }

    fn read_calls(&self) -> usize {
        self.read_calls.load(Ordering::SeqCst)
    }

    fn matching_controls(&self) -> usize {
        self.matching_controls.load(Ordering::SeqCst)
    }
}

impl ProtocolObservationSource for FakeObservationSource {
    fn epoch(&self) -> u64 {
        1
    }

    fn self_pose(&self) -> Result<SelfPose, BackendError> {
        self.legacy_calls.fetch_add(1, Ordering::SeqCst);
        Err(test_backend_error("legacy self_pose was called"))
    }

    fn list_tracked_entities(
        &self,
    ) -> Result<Vec<mineintent_contracts::minecraft::ProtocolEntitySnapshot>, BackendError> {
        self.legacy_calls.fetch_add(1, Ordering::SeqCst);
        Err(test_backend_error(
            "legacy list_tracked_entities was called",
        ))
    }

    fn read_block(&self, _position: BlockPosition) -> Result<BlockReadResult, BackendError> {
        self.legacy_calls.fetch_add(1, Ordering::SeqCst);
        Err(test_backend_error("legacy read_block was called"))
    }

    fn subscribe(
        &self,
        _listener: Arc<dyn ObservationEventListener>,
    ) -> Result<Box<dyn Subscription>, BackendError> {
        self.legacy_calls.fetch_add(1, Ordering::SeqCst);
        Err(test_backend_error(
            "legacy observation subscribe was called",
        ))
    }

    fn read_viewport(
        &self,
        control: OperationControl,
    ) -> BoxFuture<'_, Result<ViewportRead, BackendError>> {
        self.read_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(expected) = &self.expected_cancellation {
            if std::ptr::eq::<dyn CancellationSignal>(control.cancellation(), expected.as_ref()) {
                self.matching_controls.fetch_add(1, Ordering::SeqCst);
            }
        }
        let ReadPlan { gate, result } = self.pop_plan();
        Box::pin(async move {
            if let Some(gate) = gate {
                let _ = gate.await;
            }
            result
        })
    }
}

struct FakeBackend {
    sources: Mutex<VecDeque<Result<Arc<dyn ProtocolObservationSource>, BackendError>>>,
    observation_source_calls: AtomicUsize,
    snapshot_calls: AtomicUsize,
}

impl FakeBackend {
    fn with_sources(
        sources: Vec<Result<Arc<dyn ProtocolObservationSource>, BackendError>>,
    ) -> Self {
        Self {
            sources: Mutex::new(sources.into()),
            observation_source_calls: AtomicUsize::new(0),
            snapshot_calls: AtomicUsize::new(0),
        }
    }

    fn observation_source_calls(&self) -> usize {
        self.observation_source_calls.load(Ordering::SeqCst)
    }

    fn snapshot_calls(&self) -> usize {
        self.snapshot_calls.load(Ordering::SeqCst)
    }
}

impl MinecraftBackendApi for FakeBackend {
    fn start(
        &self,
        _control: OperationControl,
    ) -> BoxFuture<'_, Result<BackendReady, BackendError>> {
        Box::pin(async { Err(test_backend_error("start was called")) })
    }

    fn stop(
        &self,
        _reason: String,
        _control: OperationControl,
    ) -> BoxFuture<'_, Result<(), BackendError>> {
        Box::pin(async { Err(test_backend_error("stop was called")) })
    }

    fn state(&self) -> BackendState {
        BackendState::Idle
    }

    fn snapshot(&self) -> Result<MinecraftSnapshotV1, BackendError> {
        self.snapshot_calls.fetch_add(1, Ordering::SeqCst);
        Err(test_backend_error("legacy snapshot was called"))
    }

    fn subscribe(
        &self,
        _listener: Arc<dyn BackendEventListener>,
    ) -> Result<Box<dyn Subscription>, BackendError> {
        Err(test_backend_error("backend subscribe was called"))
    }

    fn observation_source(&self) -> Result<Arc<dyn ProtocolObservationSource>, BackendError> {
        self.observation_source_calls.fetch_add(1, Ordering::SeqCst);
        self.sources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
            .unwrap_or_else(|| Err(test_backend_error("no observation source")))
    }

    fn motor(&self) -> Result<Arc<dyn MinecraftMotorDriverApi>, BackendError> {
        Err(test_backend_error("motor was called"))
    }

    fn send_chat(&self, _message: String) -> Result<(), BackendError> {
        Err(test_backend_error("send_chat was called"))
    }
}

fn test_backend_error(message: &str) -> BackendError {
    BackendError::BackendFailure {
        failure: BackendFailure {
            code: BackendFailureCode::ProtocolError,
            message: message.to_owned(),
            retryable: false,
        },
    }
}

fn source_arc(source: Arc<FakeObservationSource>) -> Arc<dyn ProtocolObservationSource> {
    source
}

fn provider_with_sources(
    sources: Vec<Result<Arc<dyn ProtocolObservationSource>, BackendError>>,
) -> (ViewportInformationProvider, Arc<FakeBackend>) {
    let backend = Arc::new(FakeBackend::with_sources(sources));
    let backend_api: Arc<dyn MinecraftBackendApi> = backend.clone();
    (ViewportInformationProvider::new(backend_api), backend)
}

fn complete_read(marker: &str, revision: u64) -> ViewportRead {
    ViewportRead {
        projection: complete_projection(marker),
        source: FactSource::ClientPredicted,
        revision,
    }
}

fn complete_projection(marker: &str) -> ViewportProjection {
    ViewportProjection {
        frame: ViewportFrame {
            coordinates: ViewportCoordinateSystem::MinecraftWorldAbsolute,
            self_pose: ViewportSelfPose {
                position: [1.25, 64.0, -2.5],
                yaw_degrees: 28.6,
                pitch_degrees: -14.3,
            },
            legend: ViewportLegend {
                visible_entities:
                    "items 每项为 {type, player?, position}：type 是原版实体类型（玩家为 player），player 只有玩家才有，position 是 Minecraft 世界绝对坐标。按距离从近到远，truncated 为真表示更远处还有实体没列出"
                        .to_owned(),
                visible_blocks:
                    "[block_name, x, y, z]，同一坐标系的整数体素，按距离从近到远，可能截断"
                        .to_owned(),
            },
        },
        standing_on_block: Some(ViewportBlock {
            name: format!("{marker}-standing"),
            position: [1.0, 63.0, -3.0],
        }),
        looked_at_block: Some(ViewportBlock {
            name: format!("{marker}-looked"),
            position: [1.0, 65.0, -5.0],
        }),
        visible_entities: VisibleEntitiesView {
            items: vec![
                VisibleEntityView {
                    entity_type: "player".to_owned(),
                    player: Some(format!("{marker}-player")),
                    position: [4.0, 64.0, -2.0],
                },
                VisibleEntityView {
                    entity_type: "sheep".to_owned(),
                    player: None,
                    position: [5.0, 64.0, -2.0],
                },
            ],
            truncated: true,
        },
        visible_blocks: VisibleBlocksView {
            blocks: vec![
                (format!("{marker}-block"), 1, 65, -5),
                (format!("{marker}-far"), 2, 65, -5),
            ],
            truncated: true,
        },
    }
}

fn no_deadline_control() -> OperationControl {
    let cancellation: Arc<dyn CancellationSignal> = Arc::new(TestCancellation(1));
    OperationControl::new(cancellation, None)
}

#[tokio::test]
async fn ts_viewport_provider_satisfies_the_five_field_contract() {
    let source = Arc::new(FakeObservationSource::new(vec![ReadPlan::success(
        complete_read("contract", 9),
    )]));
    let (provider, _backend) = provider_with_sources(vec![Ok(source_arc(source))]);

    information_provider_contract_support::assert_information_provider_contract(
        &provider,
        &ProviderFixture::new(),
        request(&[
            "frame",
            "standingOnBlock",
            "lookedAtBlock",
            "visibleEntities",
            "visibleBlocks",
        ]),
    )
    .await;
}

#[test]
fn rust_contract_viewport_definition_and_runtime_schemas_match_oracle() {
    let source = Arc::new(FakeObservationSource::new(Vec::new()));
    let (provider, _backend) = provider_with_sources(vec![Ok(source_arc(source))]);
    let definition = provider.definition();

    assert_eq!(definition.id, InformationInterfaceId::ViewportInformation);
    assert_eq!(
        definition.description,
        "粗略第一人称视野；所有位置都使用 Minecraft 世界绝对坐标，方块为整数体素"
    );
    assert_eq!(definition.schema_revision, "viewport-information:10");
    assert_eq!(definition.audiences, [InformationAudience::Participant]);
    assert_eq!(
        definition.scope_dependencies,
        [
            InformationScopeDependency::Connection,
            InformationScopeDependency::World
        ]
    );
    assert!(definition.selectors.is_none());
    assert!(definition.pagination.is_none());
    assert_eq!(
        definition.limits,
        InformationProviderLimits {
            max_fields_per_read: 5,
            max_result_bytes: 65_536,
            timeout_ms: 5_000,
        }
    );
    assert_eq!(
        definition.fields.keys().cloned().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "frame".to_owned(),
            "standingOnBlock".to_owned(),
            "lookedAtBlock".to_owned(),
            "visibleEntities".to_owned(),
            "visibleBlocks".to_owned(),
        ])
    );

    let frame = &definition.fields["frame"];
    assert_eq!(frame.description, "本次观察的姿态与坐标系图例");
    assert_eq!(frame.value_type, "object");
    assert_eq!(frame.precision, InformationPrecision::ExactlyDisplayed);
    assert_eq!(
        frame.source_kinds,
        [InformationSourceKind::ViewportProjection]
    );
    assert_eq!(frame.requires, None);
    assert_eq!(frame.notes, None);

    let standing = &definition.fields["standingOnBlock"];
    assert_eq!(standing.description, "脚下可见方块及其绝对体素坐标");
    assert_eq!(standing.value_type, "object");
    assert_eq!(standing.precision, InformationPrecision::Inferred);
    assert_eq!(
        standing.source_kinds,
        [InformationSourceKind::ViewportProjection]
    );

    let looked = &definition.fields["lookedAtBlock"];
    assert_eq!(
        looked.description,
        "准星射线首先命中的可见方块及其绝对体素坐标"
    );
    assert_eq!(looked.precision, InformationPrecision::Inferred);
    assert_eq!(
        looked.source_kinds,
        [InformationSourceKind::ViewportProjection]
    );

    let entities = &definition.fields["visibleEntities"];
    assert_eq!(
        entities.description,
        "可见实体；items 每项为{type,player?,position}，按距离从近到远，truncated 表示更远处还有未列出的"
    );
    assert_eq!(entities.value_type, "object");
    assert_eq!(entities.precision, InformationPrecision::Inferred);
    assert_eq!(
        entities.source_kinds,
        [InformationSourceKind::ViewportProjection]
    );

    let blocks = &definition.fields["visibleBlocks"];
    assert_eq!(
        blocks.description,
        "可见方块（朝观察者的暴露面无遮挡可达）；每项为[名称,x,y,z]整数体素，按距离从近到远，可能截断"
    );
    assert_eq!(blocks.value_type, "object");
    assert_eq!(blocks.precision, InformationPrecision::Inferred);
    assert_eq!(
        blocks.source_kinds,
        [InformationSourceKind::ViewportProjection]
    );

    let block_value = json!({
        "name": "stone",
        "position": [1, 63, -3],
        "hidden": "zod object strips this"
    });
    assert_eq!(
        standing.value_schema.parse(block_value).unwrap(),
        json!({"name": "stone", "position": [1, 63, -3]})
    );
    assert_eq!(
        standing.value_schema.parse(Value::Null).unwrap(),
        Value::Null
    );

    let frame_value = serde_json::to_value(&complete_projection("schema").frame).unwrap();
    assert_eq!(
        frame.value_schema.parse(frame_value.clone()).unwrap(),
        frame_value
    );
    assert!(frame
        .value_schema
        .parse(json!({
            "coordinates": "minecraft_world_absolute",
            "self": {
                "position": [0, 64, 0],
                "yawDegrees": 0,
                "pitchDegrees": 0,
                "unexpected": true
            },
            "legend": {"visibleEntities": "e", "visibleBlocks": "b"}
        }))
        .is_err());

    let entity_value = json!({
        "items": [
            {"type": "player", "player": "sheep", "position": [0, 64, -3]},
            {"type": "sheep", "position": [1, 64, -5]}
        ],
        "truncated": true,
        "unknown": "stripped"
    });
    assert_eq!(
        entities.value_schema.parse(entity_value).unwrap(),
        json!({
            "items": [
                {"type": "player", "player": "sheep", "position": [0, 64, -3]},
                {"type": "sheep", "position": [1, 64, -5]}
            ],
            "truncated": true
        })
    );
    assert!(entities
        .value_schema
        .parse(json!({
            "items": [{"type": "sheep", "position": [0, 64, -3], "unknown": true}],
            "truncated": false
        }))
        .is_err());

    let block_list = &definition.fields["visibleBlocks"];
    assert_eq!(
        block_list
            .value_schema
            .parse(json!({
                "blocks": [["stone", 1, 63, -3]],
                "truncated": false,
                "unknown": true
            }))
            .unwrap(),
        json!({"blocks": [["stone", 1, 63, -3]], "truncated": false})
    );
}

#[tokio::test]
async fn viewport_provider_projects_requested_subset_from_one_atomic_read_and_preserves_wire_metadata(
) {
    let source = Arc::new(FakeObservationSource::new(vec![ReadPlan::success(
        complete_read("subset", 42),
    )]));
    let (provider, backend) = provider_with_sources(vec![Ok(source_arc(source.clone()))]);
    let fixture = ProviderFixture::new();
    let result = read(
        &provider,
        &fixture,
        request(&["frame", "lookedAtBlock", "visibleBlocks"]),
    )
    .await;

    assert_eq!(result.information_revision, 42);
    assert_eq!(
        result.source.kind,
        InformationSourceKind::ViewportProjection
    );
    assert_eq!(result.source.adapter_revision, "viewport-provider.v3");
    assert_eq!(result.source.source_revision, 42);
    assert_eq!(
        result.source.acquisition,
        InformationAcquisition::CurrentPerception
    );
    assert_eq!(
        result.values["frame"]["coordinates"],
        "minecraft_world_absolute"
    );
    assert_eq!(
        result.values["frame"]["self"]["position"],
        json!([1.25, 64.0, -2.5])
    );
    assert_eq!(result.observed_at, "2026-08-01T00:00:01.000Z");
    assert!(result.unavailable.is_empty());
    assert!(result.evidence_ids.is_empty());
    assert_eq!(result.next_page_state, None);
    assert_eq!(result.values["lookedAtBlock"]["name"], "subset-looked");
    assert_eq!(
        result.values["visibleBlocks"]["blocks"][0][0],
        "subset-block"
    );
    assert!(result.values.contains_key("frame"));
    assert!(!result.values.contains_key("standingOnBlock"));
    assert!(!result.values.contains_key("visibleEntities"));
    assert_eq!(source.read_calls(), 1);
    assert_eq!(source.legacy_calls(), 0);
    assert_eq!(backend.observation_source_calls(), 1);
    assert_eq!(backend.snapshot_calls(), 0);

    let wire = serde_json::to_value(&result).unwrap();
    assert_eq!(wire["informationRevision"], 42);
    assert_eq!(wire["source"]["sourceRevision"], 42);
    assert_eq!(wire["source"]["adapterRevision"], "viewport-provider.v3");
    assert_eq!(wire["source"]["acquisition"], "current_perception");
    assert_eq!(wire["observedAt"], "2026-08-01T00:00:01.000Z");
    assert_eq!(wire["evidenceIds"], json!([]));
    assert!(wire.get("validUntil").is_none());
    assert!(wire.get("nextPageState").is_none());
    assert!(wire["values"].get("standingOnBlock").is_none());
    assert!(wire.get("factSource").is_none());
}

#[tokio::test]
async fn viewport_provider_preserves_nullable_blocks_legend_players_and_truncated() {
    let mut projection = complete_projection("nullable");
    projection.standing_on_block = None;
    projection.looked_at_block = None;
    let source = Arc::new(FakeObservationSource::new(vec![ReadPlan::success(
        ViewportRead {
            projection,
            source: FactSource::ServerObserved,
            revision: 12,
        },
    )]));
    let (provider, _backend) = provider_with_sources(vec![Ok(source_arc(source))]);
    let result = read(
        &provider,
        &ProviderFixture::new(),
        request(&[
            "frame",
            "standingOnBlock",
            "lookedAtBlock",
            "visibleEntities",
        ]),
    )
    .await;

    assert_eq!(result.values["standingOnBlock"], Value::Null);
    assert_eq!(result.values["lookedAtBlock"], Value::Null);
    assert!(result.values["frame"]["legend"]["visibleEntities"]
        .as_str()
        .unwrap()
        .contains("player 只有玩家才有"));
    assert_eq!(result.values["visibleEntities"]["truncated"], true);
    let items = result.values["visibleEntities"]["items"]
        .as_array()
        .unwrap();
    assert_eq!(items[0]["type"], "player");
    assert_eq!(items[0]["player"], "nullable-player");
    assert_eq!(items[1]["type"], "sheep");
    assert!(items[1].get("player").is_none());
}

#[tokio::test]
async fn viewport_provider_gets_the_latest_observation_source_once_per_read() {
    let first_source = Arc::new(FakeObservationSource::new(vec![ReadPlan::success(
        complete_read("first", 21),
    )]));
    let second_source = Arc::new(FakeObservationSource::new(vec![ReadPlan::success(
        complete_read("second", 22),
    )]));
    let (provider, backend) = provider_with_sources(vec![
        Ok(source_arc(first_source.clone())),
        Ok(source_arc(second_source.clone())),
    ]);
    let fixture = ProviderFixture::new();

    let first = read(
        &provider,
        &fixture,
        request(&["standingOnBlock", "visibleBlocks"]),
    )
    .await;
    let second = read(
        &provider,
        &fixture,
        request(&["standingOnBlock", "visibleBlocks"]),
    )
    .await;

    assert_eq!(first.values["standingOnBlock"]["name"], "first-standing");
    assert_eq!(first.values["visibleBlocks"]["blocks"][0][0], "first-block");
    assert_eq!(second.values["standingOnBlock"]["name"], "second-standing");
    assert_eq!(
        second.values["visibleBlocks"]["blocks"][0][0],
        "second-block"
    );
    assert_eq!(backend.observation_source_calls(), 2);
    assert_eq!(first_source.read_calls(), 1);
    assert_eq!(second_source.read_calls(), 1);
    assert_eq!(first_source.legacy_calls(), 0);
    assert_eq!(second_source.legacy_calls(), 0);
    assert_eq!(backend.snapshot_calls(), 0);
}

#[tokio::test]
async fn viewport_provider_passes_the_original_operation_control_to_atomic_read() {
    let cancellation: Arc<dyn CancellationSignal> = Arc::new(TestCancellation(99));
    let source = Arc::new(FakeObservationSource::with_expected_control(
        cancellation.clone(),
        vec![ReadPlan::success(complete_read("control", 30))],
    ));
    let (provider, _backend) = provider_with_sources(vec![Ok(source_arc(source.clone()))]);
    let control = OperationControl::new(cancellation, None);

    provider
        .read(
            ProviderFixture::new().context(),
            request(&["frame"]),
            control,
        )
        .await
        .expect("atomic read should succeed");

    assert_eq!(source.read_calls(), 1);
    assert_eq!(source.matching_controls(), 1);
    assert_eq!(source.legacy_calls(), 0);
}

#[tokio::test]
async fn viewport_provider_maps_cancellation_deadline_and_backend_errors() {
    let cases = [
        (
            BackendError::Cancelled {
                operation: "read_viewport".to_owned(),
            },
            InformationProviderError::Cancelled,
        ),
        (
            BackendError::DeadlineExceeded {
                operation: "read_viewport".to_owned(),
            },
            InformationProviderError::DeadlineExceeded,
        ),
    ];
    for (backend_error, expected) in cases {
        let source = Arc::new(FakeObservationSource::new(vec![ReadPlan::failure(
            backend_error,
        )]));
        let (provider, _backend) = provider_with_sources(vec![Ok(source_arc(source))]);
        let result = provider
            .read(
                ProviderFixture::new().context(),
                request(&["frame"]),
                no_deadline_control(),
            )
            .await;
        assert_eq!(result, Err(expected));
    }

    let source = Arc::new(FakeObservationSource::new(vec![ReadPlan::failure(
        test_backend_error("atomic projection failed"),
    )]));
    let (provider, _backend) = provider_with_sources(vec![Ok(source_arc(source))]);
    let result = provider
        .read(
            ProviderFixture::new().context(),
            request(&["frame"]),
            no_deadline_control(),
        )
        .await;
    match result {
        Err(InformationProviderError::Failed { message }) => {
            assert!(message.contains("atomic projection failed"));
        }
        other => panic!("ordinary backend error must map to Failed, got {other:?}"),
    }

    let (provider, _backend) = provider_with_sources(vec![Err(test_backend_error(
        "observation source unavailable",
    ))]);
    let result = provider
        .read(
            ProviderFixture::new().context(),
            request(&["frame"]),
            no_deadline_control(),
        )
        .await;
    match result {
        Err(InformationProviderError::Failed { message }) => {
            assert!(message.contains("observation source unavailable"));
        }
        other => panic!("observation-source error must map to Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn viewport_availability_is_side_effect_free_and_never_moves_backwards() {
    let (release_first, first_gate) = oneshot::channel();
    let first_source = Arc::new(FakeObservationSource::new(vec![ReadPlan::gated(
        first_gate,
        complete_read("old", 7),
    )]));
    let second_source = Arc::new(FakeObservationSource::new(vec![ReadPlan::success(
        complete_read("new", 8),
    )]));
    let (provider, backend) = provider_with_sources(vec![
        Ok(source_arc(first_source.clone())),
        Ok(source_arc(second_source.clone())),
    ]);
    let fixture = ProviderFixture::new();

    assert_eq!(
        provider
            .availability(&fixture.context())
            .information_revision,
        0
    );
    assert_eq!(
        provider
            .availability(&fixture.context())
            .information_revision,
        0
    );
    assert_eq!(backend.observation_source_calls(), 0);
    assert_eq!(first_source.read_calls(), 0);
    assert_eq!(second_source.read_calls(), 0);
    let first = provider.read(
        fixture.context(),
        request(&["frame"]),
        no_deadline_control(),
    );
    let second = provider.read(
        fixture.context(),
        request(&["frame"]),
        no_deadline_control(),
    );
    let second_result = second.await.expect("newer read should succeed first");
    assert_eq!(second_result.information_revision, 8);
    assert_eq!(
        provider
            .availability(&fixture.context())
            .information_revision,
        8
    );
    release_first
        .send(())
        .expect("the older read should still be waiting");
    let first_result = first.await.expect("older read should eventually succeed");
    assert_eq!(first_result.information_revision, 7);
    assert_eq!(
        provider
            .availability(&fixture.context())
            .information_revision,
        8
    );
    assert_eq!(backend.observation_source_calls(), 2);
    assert_eq!(first_source.read_calls(), 1);
    assert_eq!(second_source.read_calls(), 1);
}
