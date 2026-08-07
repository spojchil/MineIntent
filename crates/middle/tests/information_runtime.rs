//! Stage 4 Information Runtime / Tool Session oracle mappings.
//!
//! The first thirteen tests below correspond one-for-one to the eight runtime cases, three
//! tool-session cases, and two runtime-reference cases in the TypeScript oracle.  The final
//! small tests exercise Rust-only panic/cancellation/trace edges and are not counted as oracle
//! cases.

use std::{
    collections::BTreeMap,
    future::pending,
    sync::{
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use mineintent_contracts::minecraft::{BoxFuture, CancellationSignal, OperationControl};
use mineintent_middle::information::{
    access_policy::InMemoryInformationAccessPolicy,
    contracts::{
        InformationAcquisition, InformationAllInterfaces, InformationAllowedInterfaces,
        InformationAudience, InformationCatalogEntryAvailability, InformationConnectionState,
        InformationErrorCode, InformationFieldDefinition, InformationGrant,
        InformationGrantPurpose, InformationHelpResult, InformationInterfaceId,
        InformationPrecision, InformationProvider, InformationProviderContext,
        InformationProviderDefinition, InformationProviderError, InformationProviderLimits,
        InformationProviderPagination, InformationProviderSelectors, InformationReadSource,
        InformationReferenceIssueRequest, InformationRequestError, InformationScopeDependency,
        InformationScopeSnapshot, InformationSelectorRef, InformationSourceKind,
        InformationToolResult, InformationToolSessionContext, InformationUnavailableField,
        InformationUnavailableReason, InformationValueSchema, InformationValueSchemaError,
        ProviderAvailability, ProviderPageRequest, ProviderReadRequest, ProviderReadResult,
        TrustedInformationCaller,
    },
    cursor_store::{InformationCursorStore, InformationCursorStoreOptions},
    ref_store::{InformationRefStore, InformationRefStoreOptions},
    registry::InformationRegistry,
    runtime::{InformationRuntime, InformationRuntimeOptions},
    scope::MutableInformationScopeSource,
    tool_session::{InformationRuntimePort, InformationTool, InformationToolSession},
    trace::{InMemoryInformationTrace, InformationTraceSink},
    InformationClock,
};
use serde_json::{json, Value};
use tokio::sync::Notify;

const NOW_MS: i64 = 1_783_987_200_000;
const NOW: &str = "2026-07-14T00:00:00.000Z";

struct FixedClock {
    now: AtomicI64,
}

impl FixedClock {
    fn new(now: i64) -> Arc<Self> {
        Arc::new(Self {
            now: AtomicI64::new(now),
        })
    }
}

impl InformationClock for FixedClock {
    fn now_millis(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
}

struct NumberSchema;

impl InformationValueSchema for NumberSchema {
    fn parse(&self, value: Value) -> Result<Value, InformationValueSchemaError> {
        if value.is_number() {
            Ok(value)
        } else {
            Err(InformationValueSchemaError {
                message: "expected number".to_owned(),
            })
        }
    }
}

struct JsonSchema;

impl InformationValueSchema for JsonSchema {
    fn parse(&self, value: Value) -> Result<Value, InformationValueSchemaError> {
        Ok(value)
    }
}

struct NestedHealthSchema;

impl InformationValueSchema for NestedHealthSchema {
    fn parse(&self, value: Value) -> Result<Value, InformationValueSchemaError> {
        let Value::Object(object) = value else {
            return Err(InformationValueSchemaError {
                message: "expected object".to_owned(),
            });
        };
        let Some(current) = object.get("current").filter(|value| value.is_number()) else {
            return Err(InformationValueSchemaError {
                message: "current must be a number".to_owned(),
            });
        };
        Ok(json!({"current": current}))
    }
}

type AvailabilityFn =
    Arc<dyn for<'a> Fn(&InformationProviderContext<'a>) -> ProviderAvailability + Send + Sync>;
type ReadFn = Arc<
    dyn for<'a> Fn(
            InformationProviderContext<'a>,
            ProviderReadRequest,
            OperationControl,
        ) -> BoxFuture<'a, Result<ProviderReadResult, InformationProviderError>>
        + Send
        + Sync,
>;

struct FixtureProvider {
    definition: InformationProviderDefinition,
    availability: AvailabilityFn,
    read: ReadFn,
}

impl InformationProvider for FixtureProvider {
    fn definition(&self) -> &InformationProviderDefinition {
        &self.definition
    }

    fn availability(&self, context: &InformationProviderContext<'_>) -> ProviderAvailability {
        (self.availability)(context)
    }

    fn read<'a>(
        &'a self,
        context: InformationProviderContext<'a>,
        request: ProviderReadRequest,
        control: OperationControl,
    ) -> BoxFuture<'a, Result<ProviderReadResult, InformationProviderError>> {
        (self.read)(context, request, control)
    }
}

fn field(
    schema: Arc<dyn InformationValueSchema>,
    source_kinds: Vec<InformationSourceKind>,
    value_type: &str,
) -> InformationFieldDefinition {
    InformationFieldDefinition {
        description: "fixture field".to_owned(),
        value_schema: schema,
        value_type: value_type.to_owned(),
        unit: None,
        precision: InformationPrecision::Displayed,
        source_kinds,
        requires: None,
        notes: None,
    }
}

fn status_definition() -> InformationProviderDefinition {
    InformationProviderDefinition {
        id: InformationInterfaceId::CurrentStatus,
        description: "Current status fixture".to_owned(),
        schema_revision: "current_status:1".to_owned(),
        audiences: vec![InformationAudience::Participant],
        fields: BTreeMap::from([
            (
                "food_display".to_owned(),
                field(
                    Arc::new(NumberSchema),
                    vec![InformationSourceKind::HudProjection],
                    "number",
                ),
            ),
            (
                "health".to_owned(),
                field(
                    Arc::new(NumberSchema),
                    vec![InformationSourceKind::HudProjection],
                    "number",
                ),
            ),
        ]),
        scope_dependencies: vec![
            InformationScopeDependency::Connection,
            InformationScopeDependency::World,
        ],
        selectors: None,
        pagination: None,
        limits: InformationProviderLimits {
            max_fields_per_read: 2,
            max_result_bytes: 4_096,
            timeout_ms: 100,
        },
    }
}

fn status_provider() -> Arc<dyn InformationProvider> {
    status_provider_with(
        status_definition(),
        Arc::new(|_, _, _| {
            Box::pin(async {
                Ok(ProviderReadResult {
                    information_revision: 12,
                    values: BTreeMap::from([(String::from("health"), json!(18))]),
                    unavailable: Vec::new(),
                    source: source(InformationSourceKind::HudProjection, 12),
                    observed_at: NOW.to_owned(),
                    valid_until: None,
                    evidence_ids: Vec::new(),
                    next_page_state: None,
                })
            })
        }),
    )
}

fn status_provider_with(
    definition: InformationProviderDefinition,
    read: ReadFn,
) -> Arc<dyn InformationProvider> {
    Arc::new(FixtureProvider {
        definition,
        availability: Arc::new(|_| ProviderAvailability {
            overall: InformationCatalogEntryAvailability::Available,
            information_revision: 12,
            fields: BTreeMap::new(),
        }),
        read,
    })
}

fn source(kind: InformationSourceKind, revision: u64) -> InformationReadSource {
    InformationReadSource {
        kind,
        adapter_revision: "fixture:1".to_owned(),
        source_revision: revision,
        acquisition: match kind {
            InformationSourceKind::ClientState => InformationAcquisition::ImmediateClientState,
            InformationSourceKind::HudProjection => InformationAcquisition::StructuredUiEquivalent,
            InformationSourceKind::DebugProjection => {
                InformationAcquisition::StructuredUiEquivalent
            }
            InformationSourceKind::ScreenProjection => InformationAcquisition::CurrentScreen,
            InformationSourceKind::ViewportProjection => InformationAcquisition::CurrentPerception,
            InformationSourceKind::SoundProjection => InformationAcquisition::CurrentPerception,
            InformationSourceKind::LifecycleEvent => InformationAcquisition::ImmediateClientState,
            InformationSourceKind::OperatorDiagnostic => InformationAcquisition::OperatorOnly,
        },
    }
}

fn scope() -> InformationScopeSnapshot {
    InformationScopeSnapshot {
        process_session_id: "process-1".to_owned(),
        connection_state: InformationConnectionState::Play,
        connection_epoch: 2,
        world_id: Some("world-1".to_owned()),
        dimension: Some("minecraft:overworld".to_owned()),
        ui_revision: 1,
        screen_instance_id: Some("screen-1".to_owned()),
        screen_revision: Some(3),
        captured_at: NOW.to_owned(),
    }
}

fn grant() -> InformationGrant {
    InformationGrant {
        id: "grant-participant".to_owned(),
        principal_id: "participant-model".to_owned(),
        audience: InformationAudience::Participant,
        allowed_interfaces: InformationAllowedInterfaces::All(InformationAllInterfaces::All),
        allowed_fields: None,
        connection_epoch: None,
        world_id: None,
        screen_instance_id: None,
        purpose: InformationGrantPurpose::ModelTool,
        valid_until: None,
    }
}

fn caller() -> TrustedInformationCaller {
    TrustedInformationCaller {
        principal_id: "participant-model".to_owned(),
        grant_id: "grant-participant".to_owned(),
        purpose: InformationGrantPurpose::ModelTool,
        correlation_id: "correlation-1".to_owned(),
        decision_run_id: Some("run-1".to_owned()),
        controller_lease_id: None,
    }
}

struct Harness {
    runtime: Arc<InformationRuntime>,
    trace: Arc<InMemoryInformationTrace>,
}

fn setup(providers: Vec<Arc<dyn InformationProvider>>) -> Harness {
    setup_with_scope_and_grant(
        providers,
        Arc::new(MutableInformationScopeSource::new(scope())),
        grant(),
    )
}

fn setup_with_scope_and_grant(
    providers: Vec<Arc<dyn InformationProvider>>,
    scope: Arc<MutableInformationScopeSource>,
    grant: InformationGrant,
) -> Harness {
    let trace = Arc::new(InMemoryInformationTrace::default());
    let runtime = setup_runtime_with_trace(
        providers,
        scope,
        grant,
        Arc::clone(&trace) as Arc<dyn InformationTraceSink>,
    );
    Harness { runtime, trace }
}

fn setup_runtime_with_trace(
    providers: Vec<Arc<dyn InformationProvider>>,
    scope: Arc<MutableInformationScopeSource>,
    grant: InformationGrant,
    trace: Arc<dyn InformationTraceSink>,
) -> Arc<InformationRuntime> {
    let registry = Arc::new(InformationRegistry::new());
    for provider in providers {
        registry
            .register(provider)
            .expect("fixture provider registers");
    }
    registry.seal("1.21.1").expect("fixture registry seals");
    let policy = Arc::new(InMemoryInformationAccessPolicy::new());
    policy.put(&grant).expect("fixture grant stores");
    let clock = FixedClock::new(NOW_MS);
    let ref_store = InformationRefStore::new(InformationRefStoreOptions {
        clock: Arc::clone(&clock) as Arc<dyn InformationClock>,
        ..InformationRefStoreOptions::default()
    })
    .expect("fixture ref store");
    let cursor_store = InformationCursorStore::new(InformationCursorStoreOptions {
        clock: Arc::clone(&clock) as Arc<dyn InformationClock>,
        ..InformationCursorStoreOptions::default()
    })
    .expect("fixture cursor store");
    let mut options = InformationRuntimeOptions::new(
        registry,
        policy,
        Arc::clone(&scope)
            as Arc<dyn mineintent_middle::information::scope::InformationScopeSource>,
    );
    options.ref_store = ref_store;
    options.cursor_store = cursor_store;
    options.trace = trace;
    options.clock = clock;
    Arc::new(InformationRuntime::new(options).expect("runtime initializes"))
}

fn control() -> OperationControl {
    OperationControl::new(Arc::new(NeverCancelled), None)
}

struct NeverCancelled;

impl CancellationSignal for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(pending())
    }
}

struct ManualCancellation {
    cancelled: AtomicBool,
    notify: Notify,
}

impl ManualCancellation {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            cancelled: AtomicBool::new(false),
            notify: Notify::new(),
        })
    }

    fn trigger(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
        self.notify.notify_one();
    }
}

impl CancellationSignal for ManualCancellation {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            while !self.is_cancelled() {
                self.notify.notified().await;
            }
        })
    }
}

fn result_code(
    result: InformationToolResult,
) -> Option<mineintent_middle::information::contracts::InformationErrorCode> {
    match result {
        InformationToolResult::Error(error) => Some(error.code),
        _ => None,
    }
}

fn read_request(interface: &str, revision: &str, fields: &[&str]) -> String {
    serde_json::to_string(&json!({
        "interfaceId": interface,
        "operation": "read",
        "schemaRevision": revision,
        "fields": fields,
    }))
    .expect("request serializes")
}

#[tokio::test]
async fn ts_runtime_catalog_help_read_and_trace_omits_values() {
    let harness = setup(vec![status_provider()]);
    let caller = caller();
    let catalog = harness
        .runtime
        .catalog(&caller, r#"{"operation":"list_interfaces"}"#)
        .expect("catalog succeeds");
    let mineintent_middle::information::contracts::InformationCatalogResult::Ok(catalog) = catalog
    else {
        panic!("first catalog must be ok");
    };
    assert_eq!(catalog.interfaces.len(), 1);
    assert_eq!(
        catalog.interfaces[0].id,
        InformationInterfaceId::CurrentStatus
    );

    let help = harness
        .runtime
        .query(
            &caller,
            r#"{"interfaceId":"current_status","operation":"help","availability":"current","fields":["health"]}"#,
            control(),
        )
        .await;
    let InformationToolResult::Help(InformationHelpResult { fields, .. }) = help else {
        panic!("help must succeed");
    };
    assert_eq!(
        fields
            .iter()
            .map(|field| field.id.as_str())
            .collect::<Vec<_>>(),
        ["health"]
    );

    let read = harness
        .runtime
        .query(
            &caller,
            &read_request("current_status", "current_status:1", &["health"]),
            control(),
        )
        .await;
    let InformationToolResult::Read(read) = read else {
        panic!("read must succeed");
    };
    assert_eq!(read.values.get("health"), Some(&json!(18)));
    assert_eq!(read.source.source_revision, 12);
    assert_eq!(harness.trace.records().len(), 1);
    assert_eq!(harness.trace.records()[0].fields, ["health"]);
}

#[tokio::test]
async fn ts_runtime_effective_catalog_revision_binds_visible_fields_and_purpose() {
    let harness = setup(vec![status_provider()]);
    let restricted = InformationGrant {
        allowed_fields: Some(BTreeMap::from([(
            InformationInterfaceId::CurrentStatus,
            vec!["health".to_owned()],
        )])),
        ..grant()
    };
    // The policy used at runtime is deliberately the existing policy store; replace its grant
    // through a second harness so the fixture stays the same as the TS setup.
    let restricted_harness = setup_with_scope_and_grant(
        vec![status_provider()],
        Arc::new(MutableInformationScopeSource::new(scope())),
        restricted,
    );
    let first = restricted_harness
        .runtime
        .catalog(&caller(), r#"{"operation":"list_interfaces"}"#)
        .expect("restricted catalog");
    let first_revision = match &first {
        mineintent_middle::information::contracts::InformationCatalogResult::Ok(ok) => {
            ok.catalog_revision.clone()
        }
        _ => panic!("restricted catalog must be ok"),
    };
    let full = setup(vec![status_provider()])
        .runtime
        .catalog(&caller(), r#"{"operation":"list_interfaces"}"#)
        .expect("full catalog");
    let full_revision = match full {
        mineintent_middle::information::contracts::InformationCatalogResult::Ok(ok) => {
            ok.catalog_revision
        }
        _ => panic!("full catalog must be ok"),
    };
    assert_ne!(first_revision, full_revision);
    let help = restricted_harness
        .runtime
        .query(
            &caller(),
            r#"{"interfaceId":"current_status","operation":"help"}"#,
            control(),
        )
        .await;
    let InformationToolResult::Help(help) = help else {
        panic!("restricted help must succeed");
    };
    assert_eq!(
        help.fields
            .iter()
            .map(|field| field.id.as_str())
            .collect::<Vec<_>>(),
        ["health"]
    );

    let denied = restricted_harness
        .runtime
        .query(
            &caller(),
            &read_request("current_status", "current_status:1", &["food_display"]),
            control(),
        )
        .await;
    assert_eq!(
        result_code(denied),
        Some(mineintent_middle::information::contracts::InformationErrorCode::AudienceDenied)
    );

    let wrong_purpose = TrustedInformationCaller {
        purpose: InformationGrantPurpose::Operator,
        ..caller()
    };
    let denied_catalog = restricted_harness
        .runtime
        .catalog(&wrong_purpose, r#"{"operation":"list_interfaces"}"#)
        .expect_err("wrong purpose must be denied");
    assert_eq!(
        denied_catalog.code,
        mineintent_middle::information::contracts::InformationErrorCode::AudienceDenied
    );
    let _ = harness;
}

#[tokio::test]
async fn ts_runtime_rejects_stale_schema_unknown_field_and_forged_scope_input() {
    let runtime = setup(vec![status_provider()]).runtime;
    let caller = caller();
    let stale = runtime
        .query(
            &caller,
            &read_request("current_status", "old", &["health"]),
            control(),
        )
        .await;
    assert_eq!(
        result_code(stale),
        Some(mineintent_middle::information::contracts::InformationErrorCode::StaleSchema)
    );
    let unknown = runtime
        .query(
            &caller,
            &read_request("current_status", "current_status:1", &["saturation"]),
            control(),
        )
        .await;
    assert_eq!(
        result_code(unknown),
        Some(mineintent_middle::information::contracts::InformationErrorCode::UnknownField)
    );
    let forged = runtime
        .query(
            &caller,
            r#"{"interfaceId":"current_status","operation":"help","worldId":"forged"}"#,
            control(),
        )
        .await;
    assert_eq!(
        result_code(forged),
        Some(mineintent_middle::information::contracts::InformationErrorCode::InvalidRequest)
    );
}

#[tokio::test]
async fn rust_only_cancelled_read_keeps_stale_schema_preflight_order() {
    let cancellation = ManualCancellation::new();
    cancellation.trigger();
    let cancelled = OperationControl::new(
        Arc::clone(&cancellation) as Arc<dyn CancellationSignal>,
        None,
    );
    let result = setup(vec![status_provider()])
        .runtime
        .query(
            &caller(),
            &read_request("current_status", "stale:1", &["health"]),
            cancelled,
        )
        .await;
    assert_eq!(
        result_code(result),
        Some(mineintent_middle::information::contracts::InformationErrorCode::StaleSchema)
    );
}

#[tokio::test]
async fn ts_runtime_preserves_partial_reads_without_filling_unavailable_fields() {
    let mut definition = status_definition();
    let read: ReadFn = Arc::new(|_, _, _| {
        Box::pin(async {
            Ok(ProviderReadResult {
                information_revision: 8,
                values: BTreeMap::from([(String::from("health"), json!(18))]),
                unavailable: vec![InformationUnavailableField {
                    field: "food_display".to_owned(),
                    reason: mineintent_middle::information::contracts::InformationReadUnavailableReason::NotCurrentlyDisplayed,
                }],
                source: source(InformationSourceKind::HudProjection, 13),
                observed_at: NOW.to_owned(),
                valid_until: None,
                evidence_ids: Vec::new(),
                next_page_state: None,
            })
        })
    });
    let partial = Arc::new(FixtureProvider {
        definition: {
            definition.limits.timeout_ms = 100;
            definition
        },
        availability: Arc::new(|_| ProviderAvailability {
            overall: InformationCatalogEntryAvailability::PartiallyAvailable,
            information_revision: 8,
            fields: BTreeMap::from([(
                "food_display".to_owned(),
                InformationUnavailableReason::NotCurrentlyDisplayed,
            )]),
        }),
        read,
    }) as Arc<dyn InformationProvider>;
    let result = setup(vec![partial])
        .runtime
        .query(
            &caller(),
            &read_request(
                "current_status",
                "current_status:1",
                &["health", "food_display"],
            ),
            control(),
        )
        .await;
    let InformationToolResult::Read(result) = result else {
        panic!("partial read must succeed");
    };
    assert_eq!(
        result.values,
        BTreeMap::from([(String::from("health"), json!(18))])
    );
    assert_eq!(result.unavailable.len(), 1);
    assert_eq!(result.unavailable[0].field, "food_display");
}

#[tokio::test]
async fn ts_runtime_discards_provider_leaks_and_reads_racing_scope_change() {
    let leaking_read: ReadFn = Arc::new(|_, _, _| {
        Box::pin(async {
            Ok(ProviderReadResult {
                information_revision: 1,
                values: BTreeMap::from([
                    ("health".to_owned(), json!(18)),
                    ("hidden_saturation".to_owned(), json!(4.2)),
                ]),
                unavailable: Vec::new(),
                source: source(InformationSourceKind::HudProjection, 1),
                observed_at: NOW.to_owned(),
                valid_until: None,
                evidence_ids: Vec::new(),
                next_page_state: None,
            })
        })
    });
    let leaked = setup(vec![status_provider_with(
        status_definition(),
        leaking_read,
    )])
    .runtime
    .query(
        &caller(),
        &read_request("current_status", "current_status:1", &["health"]),
        control(),
    )
    .await;
    assert_eq!(
        result_code(leaked),
        Some(mineintent_middle::information::contracts::InformationErrorCode::ProviderFailed)
    );

    let race_scope = Arc::new(MutableInformationScopeSource::new(scope()));
    let race_scope_for_provider = Arc::clone(&race_scope);
    let race_read: ReadFn = Arc::new(move |_, _, _| {
        race_scope_for_provider.update(InformationScopeSnapshot {
            connection_epoch: 3,
            captured_at: "2026-07-14T00:00:01.000Z".to_owned(),
            ..scope()
        });
        Box::pin(async {
            Ok(ProviderReadResult {
                information_revision: 1,
                values: BTreeMap::from([("health".to_owned(), json!(18))]),
                unavailable: Vec::new(),
                source: source(InformationSourceKind::HudProjection, 1),
                observed_at: NOW.to_owned(),
                valid_until: None,
                evidence_ids: Vec::new(),
                next_page_state: None,
            })
        })
    });
    let raced = setup_with_scope_and_grant(
        vec![status_provider_with(status_definition(), race_read)],
        race_scope,
        grant(),
    )
    .runtime
    .query(
        &caller(),
        &read_request("current_status", "current_status:1", &["health"]),
        control(),
    )
    .await;
    assert_eq!(
        result_code(raced),
        Some(mineintent_middle::information::contracts::InformationErrorCode::ScopeChanged)
    );
}

#[tokio::test]
async fn ts_runtime_rebuilds_nested_values_and_enforces_declared_sources() {
    let mut definition = status_definition();
    definition.schema_revision = "nested:1".to_owned();
    definition.fields = BTreeMap::from([(
        "health".to_owned(),
        field(
            Arc::new(NestedHealthSchema),
            vec![InformationSourceKind::HudProjection],
            "object",
        ),
    )]);
    definition.limits.max_fields_per_read = 1;
    let clean_read: ReadFn = Arc::new(|_, _, _| {
        Box::pin(async {
            Ok(ProviderReadResult {
                information_revision: 1,
                values: BTreeMap::from([(
                    "health".to_owned(),
                    json!({"current": 18, "hiddenSaturation": 4.2}),
                )]),
                unavailable: Vec::new(),
                source: source(InformationSourceKind::HudProjection, 1),
                observed_at: NOW.to_owned(),
                valid_until: None,
                evidence_ids: Vec::new(),
                next_page_state: None,
            })
        })
    });
    let cleaned = setup(vec![status_provider_with(
        definition.clone_for_test(),
        clean_read,
    )])
    .runtime
    .query(
        &caller(),
        &read_request("current_status", "nested:1", &["health"]),
        control(),
    )
    .await;
    let InformationToolResult::Read(cleaned) = cleaned else {
        panic!("nested read must succeed");
    };
    assert_eq!(cleaned.values["health"], json!({"current": 18}));

    let wrong_source_read: ReadFn = Arc::new(|_, _, _| {
        Box::pin(async {
            Ok(ProviderReadResult {
                information_revision: 1,
                values: BTreeMap::from([("health".to_owned(), json!({"current": 18}))]),
                unavailable: Vec::new(),
                source: source(InformationSourceKind::OperatorDiagnostic, 1),
                observed_at: NOW.to_owned(),
                valid_until: None,
                evidence_ids: Vec::new(),
                next_page_state: None,
            })
        })
    });
    let rejected = setup(vec![status_provider_with(definition, wrong_source_read)])
        .runtime
        .query(
            &caller(),
            &read_request("current_status", "nested:1", &["health"]),
            control(),
        )
        .await;
    assert_eq!(
        result_code(rejected),
        Some(mineintent_middle::information::contracts::InformationErrorCode::ProviderFailed)
    );
}

#[tokio::test]
async fn ts_runtime_aborts_provider_when_its_deadline_elapses() {
    let observed = Arc::new(AtomicBool::new(false));
    let observed_for_provider = Arc::clone(&observed);
    let mut definition = status_definition();
    definition.limits.timeout_ms = 10;
    let slow_read: ReadFn = Arc::new(move |_, _, control| {
        let observed = Arc::clone(&observed_for_provider);
        Box::pin(async move {
            let deadline = control
                .deadline_elapsed()
                .unwrap_or_else(|| Box::pin(pending()));
            tokio::pin!(deadline);
            tokio::select! {
                _ = control.cancelled() => {
                    observed.store(true, Ordering::SeqCst);
                    Err(InformationProviderError::Cancelled)
                }
                _ = &mut deadline => {
                    observed.store(true, Ordering::SeqCst);
                    Err(InformationProviderError::DeadlineExceeded)
                }
                _ = tokio::time::sleep(Duration::from_millis(200)) => {
                    Err(InformationProviderError::Failed { message: "too slow".to_owned() })
                }
            }
        })
    });
    let result = setup(vec![status_provider_with(definition, slow_read)])
        .runtime
        .query(
            &caller(),
            &read_request("current_status", "current_status:1", &["health"]),
            control(),
        )
        .await;
    assert_eq!(
        result_code(result),
        Some(mineintent_middle::information::contracts::InformationErrorCode::DeadlineExceeded)
    );
    assert!(observed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn ts_provider_contract_fixture_is_legal() {
    let provider = status_provider();
    let definition = provider.definition();
    let availability = provider.availability(&InformationProviderContext {
        now: NOW,
        scope: &scope(),
        caller: mineintent_middle::information::contracts::InformationProviderCaller {
            audience: InformationAudience::Participant,
            purpose: InformationGrantPurpose::ModelTool,
        },
        refs: &NoopRefs,
    });
    assert_eq!(
        availability.overall,
        InformationCatalogEntryAvailability::Available
    );
    let result = provider
        .read(
            InformationProviderContext {
                now: NOW,
                scope: &scope(),
                caller: mineintent_middle::information::contracts::InformationProviderCaller {
                    audience: InformationAudience::Participant,
                    purpose: InformationGrantPurpose::ModelTool,
                },
                refs: &NoopRefs,
            },
            ProviderReadRequest {
                fields: vec!["health".to_owned()],
                selector: None,
                page: ProviderPageRequest {
                    limit: 1,
                    state: None,
                },
            },
            control(),
        )
        .await
        .expect("legal provider read");
    assert!(definition.fields["health"]
        .value_schema
        .parse(result.values["health"].clone())
        .is_ok());
}

struct NoopRefs;

impl mineintent_middle::information::contracts::InformationReferenceIssuer for NoopRefs {
    fn issue(
        &self,
        _request: InformationReferenceIssueRequest,
    ) -> Result<
        InformationSelectorRef,
        mineintent_middle::information::contracts::InformationReferenceIssueError,
    > {
        Err(mineintent_middle::information::contracts::InformationReferenceIssueError::InvalidMetadata)
    }
}

fn session_context(
    max_calls: u64,
    max_read_calls: u64,
    max_returned_bytes: u64,
    deadline_at: &str,
) -> InformationToolSessionContext {
    InformationToolSessionContext {
        session_id: "session-1".to_owned(),
        decision_run_id: "run-1".to_owned(),
        correlation_id: "correlation-1".to_owned(),
        principal_id: "participant-model".to_owned(),
        grant_id: "grant-participant".to_owned(),
        budget: mineintent_middle::information::contracts::InformationToolSessionBudget {
            max_calls,
            max_read_calls,
            max_returned_bytes,
            deadline_at: deadline_at.to_owned(),
        },
    }
}

#[tokio::test]
async fn ts_tool_session_enforces_call_read_and_byte_budgets() {
    struct CountingPort {
        calls: AtomicU64,
    }
    impl InformationRuntimePort for CountingPort {
        fn catalog(
            &self,
            _caller: &TrustedInformationCaller,
            _request: &str,
        ) -> Result<
            mineintent_middle::information::contracts::InformationCatalogResult,
            InformationRequestError,
        > {
            Ok(mineintent_middle::information::contracts::InformationCatalogResult::NotModified(
                mineintent_middle::information::contracts::InformationCatalogNotModified {
                    protocol: mineintent_middle::information::contracts::InformationCatalogProtocol::V1,
                    status: mineintent_middle::information::contracts::InformationCatalogNotModifiedStatus::NotModified,
                    catalog_revision: "catalog:1".to_owned(),
                },
            ))
        }

        fn query<'a>(
            &'a self,
            _caller: &'a TrustedInformationCaller,
            _request: &'a str,
            _control: OperationControl,
        ) -> BoxFuture<'a, InformationToolResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                InformationToolResult::Error(InformationRequestError {
                    protocol: mineintent_middle::information::contracts::InformationErrorProtocol::V1,
                    interface_id: None,
                    code: mineintent_middle::information::contracts::InformationErrorCode::UnknownField,
                    message: "x".to_owned(),
                    current_catalog_revision: None,
                    current_schema_revision: None,
                    rejected_fields: None,
                })
            })
        }
    }
    let port = CountingPort {
        calls: AtomicU64::new(0),
    };
    let tool = InformationTool::new(&port);
    let session =
        InformationToolSession::new(session_context(2, 1, 1_024, "2099-01-01T00:00:00.000Z"))
            .expect("session budget");
    let request = read_request("current_status", "current_status:1", &["health"]);
    let _ = tool.invoke(&request, &session, control()).await;
    let second = tool.invoke(&request, &session, control()).await;
    assert_eq!(
        result_code(second),
        Some(mineintent_middle::information::contracts::InformationErrorCode::BudgetExceeded)
    );
    assert_eq!(port.calls.load(Ordering::SeqCst), 1);

    let byte_session =
        InformationToolSession::new(session_context(1, 1, 1, "2099-01-01T00:00:00.000Z"))
            .expect("byte session budget");
    let byte_result = tool.invoke(&request, &byte_session, control()).await;
    assert_eq!(
        result_code(byte_result),
        Some(mineintent_middle::information::contracts::InformationErrorCode::BudgetExceeded)
    );
}

#[tokio::test]
async fn ts_tool_session_deadline_aborts_an_in_progress_query() {
    struct WaitingPort {
        observed: Arc<AtomicBool>,
    }
    impl InformationRuntimePort for WaitingPort {
        fn catalog(
            &self,
            _caller: &TrustedInformationCaller,
            _request: &str,
        ) -> Result<
            mineintent_middle::information::contracts::InformationCatalogResult,
            InformationRequestError,
        > {
            Err(InformationRequestError {
                protocol: mineintent_middle::information::contracts::InformationErrorProtocol::V1,
                interface_id: None,
                code:
                    mineintent_middle::information::contracts::InformationErrorCode::ProviderFailed,
                message: "unused".to_owned(),
                current_catalog_revision: None,
                current_schema_revision: None,
                rejected_fields: None,
            })
        }

        fn query<'a>(
            &'a self,
            _caller: &'a TrustedInformationCaller,
            _request: &'a str,
            control: OperationControl,
        ) -> BoxFuture<'a, InformationToolResult> {
            let observed = Arc::clone(&self.observed);
            Box::pin(async move {
                let deadline = control
                    .deadline_elapsed()
                    .unwrap_or_else(|| Box::pin(pending()));
                tokio::pin!(deadline);
                tokio::select! {
                    _ = &mut deadline => {
                        observed.store(true, Ordering::SeqCst);
                        InformationToolResult::Error(session_error(InformationErrorCode::DeadlineExceeded, "deadline"))
                    }
                    _ = control.cancelled() => {
                        observed.store(true, Ordering::SeqCst);
                        InformationToolResult::Error(session_error(InformationErrorCode::DeadlineExceeded, "cancelled"))
                    }
                }
            })
        }
    }
    let observed = Arc::new(AtomicBool::new(false));
    let port = WaitingPort {
        observed: Arc::clone(&observed),
    };
    let session = InformationToolSession::with_clock(
        session_context(1, 1, 1_024, "2026-07-14T00:00:00.020Z"),
        FixedClock::new(NOW_MS),
    )
    .expect("deadline session");
    let result = InformationTool::new(&port)
        .invoke(
            &read_request("current_status", "current_status:1", &["health"]),
            &session,
            control(),
        )
        .await;
    assert_eq!(
        result_code(result),
        Some(mineintent_middle::information::contracts::InformationErrorCode::DeadlineExceeded)
    );
    assert!(observed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn ts_tool_session_forwards_upstream_cancellation_to_query() {
    struct WaitingPort {
        observed: Arc<AtomicBool>,
    }
    impl InformationRuntimePort for WaitingPort {
        fn catalog(
            &self,
            _caller: &TrustedInformationCaller,
            _request: &str,
        ) -> Result<
            mineintent_middle::information::contracts::InformationCatalogResult,
            InformationRequestError,
        > {
            unreachable!("catalog is not used by cancellation mapping")
        }

        fn query<'a>(
            &'a self,
            _caller: &'a TrustedInformationCaller,
            _request: &'a str,
            control: OperationControl,
        ) -> BoxFuture<'a, InformationToolResult> {
            let observed = Arc::clone(&self.observed);
            Box::pin(async move {
                tokio::select! {
                    _ = control.cancelled() => {
                        observed.store(true, Ordering::SeqCst);
                        InformationToolResult::Error(session_error(InformationErrorCode::DeadlineExceeded, "cancelled"))
                    }
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {
                        InformationToolResult::Error(session_error(InformationErrorCode::ProviderFailed, "late"))
                    }
                }
            })
        }
    }
    let observed = Arc::new(AtomicBool::new(false));
    let port = WaitingPort {
        observed: Arc::clone(&observed),
    };
    let session =
        InformationToolSession::new(session_context(1, 1, 1_024, "2099-01-01T00:00:00.000Z"))
            .expect("cancellation session");
    let cancellation = ManualCancellation::new();
    let upstream = OperationControl::new(
        Arc::clone(&cancellation) as Arc<dyn CancellationSignal>,
        None,
    );
    let tool = InformationTool::new(&port);
    let request = read_request("current_status", "current_status:1", &["health"]);
    let pending = tool.invoke(&request, &session, upstream);
    tokio::time::sleep(Duration::from_millis(5)).await;
    cancellation.trigger();
    let result = pending.await;
    assert_eq!(
        result_code(result),
        Some(mineintent_middle::information::contracts::InformationErrorCode::DeadlineExceeded)
    );
    assert!(observed.load(Ordering::SeqCst));
}

fn inventory_definition() -> InformationProviderDefinition {
    InformationProviderDefinition {
        id: InformationInterfaceId::InventoryInformation,
        description: "Inventory fixture".to_owned(),
        schema_revision: "inventory:1".to_owned(),
        audiences: vec![InformationAudience::Participant],
        fields: BTreeMap::from([(
            "item_refs".to_owned(),
            field(
                Arc::new(JsonSchema),
                vec![InformationSourceKind::ScreenProjection],
                "array",
            ),
        )]),
        scope_dependencies: vec![
            InformationScopeDependency::Connection,
            InformationScopeDependency::World,
        ],
        selectors: None,
        pagination: None,
        limits: InformationProviderLimits {
            max_fields_per_read: 1,
            max_result_bytes: 8_192,
            timeout_ms: 100,
        },
    }
}

fn tooltip_definition() -> InformationProviderDefinition {
    InformationProviderDefinition {
        id: InformationInterfaceId::ItemTooltipInformation,
        description: "Tooltip fixture".to_owned(),
        schema_revision: "tooltip:1".to_owned(),
        audiences: vec![InformationAudience::Participant],
        fields: BTreeMap::from([(
            "display_name".to_owned(),
            field(
                Arc::new(JsonSchema),
                vec![InformationSourceKind::ScreenProjection],
                "string",
            ),
        )]),
        scope_dependencies: vec![
            InformationScopeDependency::Connection,
            InformationScopeDependency::World,
            InformationScopeDependency::Screen,
        ],
        selectors: Some(InformationProviderSelectors {
            required: true,
            accepts_kinds: vec!["item".to_owned()],
        }),
        pagination: None,
        limits: InformationProviderLimits {
            max_fields_per_read: 1,
            max_result_bytes: 8_192,
            timeout_ms: 100,
        },
    }
}

#[tokio::test]
async fn ts_runtime_references_bind_target_kind_and_source_revision() {
    let source_revision = Arc::new(AtomicU64::new(2));
    let source_revision_for_availability = Arc::clone(&source_revision);
    let inventory_read: ReadFn = Arc::new(|context, _, _| {
        let reference = context.refs.issue(InformationReferenceIssueRequest {
            kind: "item".to_owned(),
            payload: json!({"slot": 2}),
            allowed_interfaces: vec![InformationInterfaceId::ItemTooltipInformation],
            based_on_information_revision: 2,
            valid_until: None,
            bind_to_screen: None,
        });
        Box::pin(async move {
            let reference = reference.map_err(|error| InformationProviderError::Failed {
                message: error.to_string(),
            })?;
            Ok(ProviderReadResult {
                information_revision: 2,
                values: BTreeMap::from([("item_refs".to_owned(), json!([reference]))]),
                unavailable: Vec::new(),
                source: source(InformationSourceKind::ScreenProjection, 2),
                observed_at: NOW.to_owned(),
                valid_until: None,
                evidence_ids: Vec::new(),
                next_page_state: None,
            })
        })
    });
    let inventory = Arc::new(FixtureProvider {
        definition: inventory_definition(),
        availability: Arc::new(move |_| ProviderAvailability {
            overall: InformationCatalogEntryAvailability::Available,
            information_revision: source_revision_for_availability.load(Ordering::SeqCst),
            fields: BTreeMap::new(),
        }),
        read: inventory_read,
    }) as Arc<dyn InformationProvider>;
    let source_revision_for_tooltip = Arc::clone(&source_revision);
    let change_source_during_tooltip = Arc::new(AtomicBool::new(false));
    let change_source_for_tooltip = Arc::clone(&change_source_during_tooltip);
    let tooltip_read: ReadFn = Arc::new(move |_, request, _| {
        let source_revision = Arc::clone(&source_revision_for_tooltip);
        let change_source = Arc::clone(&change_source_for_tooltip);
        Box::pin(async move {
            let slot = request
                .selector
                .as_ref()
                .and_then(|value| value.get("slot"))
                .and_then(Value::as_u64);
            if slot != Some(2) {
                return Err(InformationProviderError::Failed {
                    message: "wrong slot".to_owned(),
                });
            }
            if change_source.load(Ordering::SeqCst) {
                source_revision.store(3, Ordering::SeqCst);
            }
            Ok(ProviderReadResult {
                information_revision: 3,
                values: BTreeMap::from([("display_name".to_owned(), json!("Oak Log"))]),
                unavailable: Vec::new(),
                source: source(
                    InformationSourceKind::ScreenProjection,
                    source_revision.load(Ordering::SeqCst),
                ),
                observed_at: NOW.to_owned(),
                valid_until: None,
                evidence_ids: Vec::new(),
                next_page_state: None,
            })
        })
    });
    let tooltip = Arc::new(FixtureProvider {
        definition: tooltip_definition(),
        availability: Arc::new(|_| ProviderAvailability {
            overall: InformationCatalogEntryAvailability::Available,
            information_revision: 3,
            fields: BTreeMap::new(),
        }),
        read: tooltip_read,
    }) as Arc<dyn InformationProvider>;
    let runtime = setup(vec![inventory, tooltip]).runtime;
    let inventory_result = runtime
        .query(
            &caller(),
            &read_request("inventory_information", "inventory:1", &["item_refs"]),
            control(),
        )
        .await;
    let InformationToolResult::Read(inventory_result) = inventory_result else {
        panic!("inventory read must succeed");
    };
    let reference: InformationSelectorRef =
        serde_json::from_value(inventory_result.values["item_refs"][0].clone())
            .expect("provider reference is a selector ref");
    let selector = serde_json::to_string(&reference).expect("selector serializes");
    let tooltip_request = format!(
        "{{\"interfaceId\":\"item_tooltip_information\",\"operation\":\"read\",\"schemaRevision\":\"tooltip:1\",\"fields\":[\"display_name\"],\"selector\":{selector}}}"
    );
    let good = runtime.query(&caller(), &tooltip_request, control()).await;
    let InformationToolResult::Read(good) = good else {
        panic!("tooltip selector must resolve");
    };
    assert_eq!(good.values["display_name"], json!("Oak Log"));

    change_source_during_tooltip.store(true, Ordering::SeqCst);
    let stale = runtime.query(&caller(), &tooltip_request, control()).await;
    assert_eq!(
        result_code(stale),
        Some(mineintent_middle::information::contracts::InformationErrorCode::InvalidSelector)
    );

    let wrong_target = runtime
        .query(
            &caller(),
            &format!("{{\"interfaceId\":\"inventory_information\",\"operation\":\"read\",\"schemaRevision\":\"inventory:1\",\"fields\":[\"item_refs\"],\"selector\":{selector}}}"),
            control(),
        )
        .await;
    assert_eq!(
        result_code(wrong_target),
        Some(mineintent_middle::information::contracts::InformationErrorCode::InvalidSelector)
    );
}

#[tokio::test]
async fn ts_runtime_references_cursor_paging_is_bound_and_one_time() {
    let runtime = setup(vec![status_provider_with(
        {
            let mut definition = inventory_definition();
            definition.schema_revision = "paged:1".to_owned();
            definition.fields = BTreeMap::from([(
                "entries".to_owned(),
                field(
                    Arc::new(JsonSchema),
                    vec![InformationSourceKind::ClientState],
                    "array",
                ),
            )]);
            definition.pagination = Some(InformationProviderPagination {
                default_limit: 2,
                max_limit: 2,
            });
            definition
        },
        Arc::new(move |_, request, _| {
            let entries = [json!(0), json!(1), json!(2), json!(3)];
            Box::pin(async move {
                let offset = request
                    .page
                    .state
                    .as_ref()
                    .and_then(|state| state.get("offset"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                let end = (offset + request.page.limit as usize).min(entries.len());
                Ok(ProviderReadResult {
                    information_revision: 5,
                    values: BTreeMap::from([(
                        "entries".to_owned(),
                        Value::Array(entries[offset..end].to_vec()),
                    )]),
                    unavailable: Vec::new(),
                    source: source(InformationSourceKind::ClientState, 5),
                    observed_at: NOW.to_owned(),
                    valid_until: None,
                    evidence_ids: Vec::new(),
                    next_page_state: (end < entries.len()).then(|| json!({"offset": end})),
                })
            })
        }),
    )])
    .runtime;
    let first = runtime
        .query(
            &caller(),
            r#"{"interfaceId":"inventory_information","operation":"read","schemaRevision":"paged:1","fields":["entries"],"page":{"limit":2}}"#,
            control(),
        )
        .await;
    let InformationToolResult::Read(first) = first else {
        panic!("first page");
    };
    let cursor = first.next_cursor.expect("cursor");
    let second_request = format!(
        "{{\"interfaceId\":\"inventory_information\",\"operation\":\"read\",\"schemaRevision\":\"paged:1\",\"fields\":[\"entries\"],\"page\":{{\"limit\":2,\"cursor\":\"{cursor}\"}}}}"
    );
    let second = runtime.query(&caller(), &second_request, control()).await;
    let InformationToolResult::Read(second) = second else {
        panic!("second page");
    };
    assert_eq!(second.values["entries"], json!([2, 3]));
    assert!(second.next_cursor.is_none());
    let consumed = runtime.query(&caller(), &second_request, control()).await;
    assert_eq!(
        result_code(consumed),
        Some(mineintent_middle::information::contracts::InformationErrorCode::InvalidPage)
    );
}

fn session_error(
    code: mineintent_middle::information::contracts::InformationErrorCode,
    message: &str,
) -> InformationRequestError {
    InformationRequestError {
        protocol: mineintent_middle::information::contracts::InformationErrorProtocol::V1,
        interface_id: None,
        code,
        message: message.to_owned(),
        current_catalog_revision: None,
        current_schema_revision: None,
        rejected_fields: None,
    }
}

// Small test-only cloning helper keeps the fixture definition readable without touching the
// production contract, whose value schemas are intentionally trait objects.
trait CloneDefinitionForTest {
    fn clone_for_test(&self) -> InformationProviderDefinition;
}

impl CloneDefinitionForTest for InformationProviderDefinition {
    fn clone_for_test(&self) -> InformationProviderDefinition {
        InformationProviderDefinition {
            id: self.id,
            description: self.description.clone(),
            schema_revision: self.schema_revision.clone(),
            audiences: self.audiences.clone(),
            fields: self
                .fields
                .iter()
                .map(|(id, field)| {
                    (
                        id.clone(),
                        InformationFieldDefinition {
                            description: field.description.clone(),
                            value_schema: Arc::clone(&field.value_schema),
                            value_type: field.value_type.clone(),
                            unit: field.unit.clone(),
                            precision: field.precision,
                            source_kinds: field.source_kinds.clone(),
                            requires: field.requires.clone(),
                            notes: field.notes.clone(),
                        },
                    )
                })
                .collect(),
            scope_dependencies: self.scope_dependencies.clone(),
            selectors: self
                .selectors
                .as_ref()
                .map(|selectors| InformationProviderSelectors {
                    required: selectors.required,
                    accepts_kinds: selectors.accepts_kinds.clone(),
                }),
            pagination: self
                .pagination
                .as_ref()
                .map(|pagination| InformationProviderPagination {
                    default_limit: pagination.default_limit,
                    max_limit: pagination.max_limit,
                }),
            limits: InformationProviderLimits {
                max_fields_per_read: self.limits.max_fields_per_read,
                max_result_bytes: self.limits.max_result_bytes,
                timeout_ms: self.limits.timeout_ms,
            },
        }
    }
}

// 这里曾有两个测试：`rust_only_provider_panic_is_structured` 与
// `rust_only_trace_panic_is_structured_without_leaking_panic_summary`，
// 连同 `PanicTrace` 夹具。它们断言 provider / trace sink 的 panic 被转成
// `ProviderFailed`，即上一版 Information 控制面那 8 处 catch_unwind 的语义。
//
// 那些捕获已经删除，所以这两个测试自 b1c6f51 起就是红的。删掉而不是修复，
// 因为它们断言的行为本身是被推翻的那一个：把缺陷压成一条模型可见的普通失败，
// 会让缺陷看起来像世界事实，而模型必然重试——panic 可重现，重试注定再次
// panic（理由全文见 crates/toolloop/src/control.rs 开头）。
//
// 另一半理由是覆盖面：Information 控制面已由编译实验坐实不在生产路径上
// （41cdf3c），这两个测试守的是一段没有调用者的代码。
//
// 若要恢复该语义，恢复捕获与这两个测试应当一起做，不要只补测试。
