//! Deterministic Information context-composer tests.

use std::{
    collections::VecDeque,
    future::pending,
    sync::{Arc, Mutex},
};

use mineintent_contracts::{
    information::{fixture_information_set, InformationOmissionReason, PassiveInterfaceId},
    minecraft::{BoxFuture, CancellationSignal, Deadline, OperationControl},
};
use mineintent_middle::information::{
    compose_passive_observations, contracts::InformationAcquisition,
    contracts::InformationErrorCode, contracts::InformationErrorProtocol,
    contracts::InformationInterfaceId, contracts::InformationReadProtocol,
    contracts::InformationReadResult, contracts::InformationReadSource,
    contracts::InformationReadUnavailableReason, contracts::InformationRequestError,
    contracts::InformationSourceKind, contracts::InformationToolResult,
    contracts::InformationUnavailableField, contracts::TrustedInformationCaller,
    InformationRuntimePort,
};
use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
struct RecordedCall {
    request: String,
    cancellation_data: usize,
    deadline_data: Option<usize>,
}

struct RecordingRuntime {
    responses: Mutex<VecDeque<InformationToolResult>>,
    calls: Mutex<Vec<RecordedCall>>,
}

impl RecordingRuntime {
    fn new(responses: Vec<InformationToolResult>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().expect("calls lock").clone()
    }
}

impl InformationRuntimePort for RecordingRuntime {
    fn catalog(
        &self,
        _caller: &TrustedInformationCaller,
        _request: &str,
    ) -> Result<
        mineintent_middle::information::contracts::InformationCatalogResult,
        InformationRequestError,
    > {
        Err(error_response(
            InformationInterfaceId::CurrentStatus,
            InformationErrorCode::ProviderFailed,
            "catalog is not part of composer fixtures",
        ))
    }

    fn query<'a>(
        &'a self,
        _caller: &'a TrustedInformationCaller,
        request: &'a str,
        control: OperationControl,
    ) -> BoxFuture<'a, InformationToolResult> {
        let cancellation_data =
            control.cancellation() as *const dyn CancellationSignal as *const () as usize;
        let deadline_data = control
            .deadline()
            .map(|deadline| deadline as *const dyn Deadline as *const () as usize);
        self.calls.lock().expect("calls lock").push(RecordedCall {
            request: request.to_owned(),
            cancellation_data,
            deadline_data,
        });
        let response = self
            .responses
            .lock()
            .expect("responses lock")
            .pop_front()
            .unwrap_or_else(|| {
                InformationToolResult::Error(error_response(
                    InformationInterfaceId::CurrentStatus,
                    InformationErrorCode::ProviderFailed,
                    "fixture received an unexpected extra query",
                ))
            });
        Box::pin(async move { response })
    }
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

struct NeverDeadline;

impl Deadline for NeverDeadline {
    fn has_elapsed(&self) -> bool {
        false
    }

    fn elapsed(&self) -> BoxFuture<'_, ()> {
        Box::pin(pending())
    }
}

fn control() -> OperationControl {
    OperationControl::new(Arc::new(NeverCancelled), Some(Arc::new(NeverDeadline)))
}

fn caller() -> TrustedInformationCaller {
    TrustedInformationCaller {
        principal_id: "context-composer".to_owned(),
        grant_id: "grant-1".to_owned(),
        purpose:
            mineintent_middle::information::contracts::InformationGrantPurpose::ParticipantContext,
        correlation_id: "correlation-1".to_owned(),
        decision_run_id: None,
        controller_lease_id: None,
    }
}

fn read_response(
    interface_id: InformationInterfaceId,
    schema_revision: &str,
    values: Value,
    unavailable: Vec<InformationUnavailableField>,
) -> InformationToolResult {
    let Value::Object(values) = values else {
        panic!("test read values must be an object");
    };
    InformationToolResult::Read(InformationReadResult {
        protocol: InformationReadProtocol::V1,
        read_id: format!("read-{interface_id:?}"),
        interface_id,
        schema_revision: schema_revision.to_owned(),
        information_revision: 1,
        connection_epoch: 1,
        world_id: Some("world-1".to_owned()),
        dimension: Some("minecraft:overworld".to_owned()),
        observed_at: "2026-08-01T00:00:00Z".to_owned(),
        valid_until: None,
        source: InformationReadSource {
            kind: source_kind(interface_id),
            adapter_revision: "fixture:1".to_owned(),
            source_revision: 1,
            acquisition: InformationAcquisition::CurrentPerception,
        },
        values: values.into_iter().collect(),
        unavailable,
        evidence_ids: Vec::new(),
        next_cursor: None,
    })
}

fn source_kind(interface_id: InformationInterfaceId) -> InformationSourceKind {
    match interface_id {
        InformationInterfaceId::CurrentStatus | InformationInterfaceId::InventoryInformation => {
            InformationSourceKind::ClientState
        }
        InformationInterfaceId::SoundInformation => InformationSourceKind::SoundProjection,
        InformationInterfaceId::ViewportInformation => InformationSourceKind::ViewportProjection,
        other => panic!("unexpected fixture interface {other:?}"),
    }
}

fn error_response(
    interface_id: InformationInterfaceId,
    code: InformationErrorCode,
    message: &str,
) -> InformationRequestError {
    InformationRequestError {
        protocol: InformationErrorProtocol::V1,
        interface_id: Some(interface_id),
        code,
        message: message.to_owned(),
        current_catalog_revision: None,
        current_schema_revision: None,
        rejected_fields: None,
    }
}

fn error_result(
    interface_id: InformationInterfaceId,
    code: InformationErrorCode,
    message: &str,
) -> InformationToolResult {
    InformationToolResult::Error(error_response(interface_id, code, message))
}

fn canonical_responses() -> Vec<InformationToolResult> {
    let fixture = fixture_information_set();
    let mut current_status =
        serde_json::to_value(fixture.plain_values.current_status).expect("status JSON");
    current_status["health"] = json!(20.0);
    let mut inventory =
        serde_json::to_value(fixture.plain_values.inventory).expect("inventory JSON");
    inventory["selectedHotbarSlot"] = json!(0);
    vec![
        read_response(
            InformationInterfaceId::CurrentStatus,
            "current-status:1",
            current_status,
            Vec::new(),
        ),
        read_response(
            InformationInterfaceId::InventoryInformation,
            "inventory-information:1",
            inventory,
            Vec::new(),
        ),
        read_response(
            InformationInterfaceId::SoundInformation,
            "sound-information:1",
            json!({"recentSounds": []}),
            Vec::new(),
        ),
        read_response(
            InformationInterfaceId::ViewportInformation,
            "viewport-information:10",
            json!({
                "frame": fixture
                    .composed
                    .viewport
                    .expect("fixture passive viewport")
                    .frame
                    .expect("fixture viewport frame")
            }),
            Vec::new(),
        ),
    ]
}

#[tokio::test]
async fn ts_compose_passive_observations_flattens_four_plain_values() {
    let runtime = RecordingRuntime::new(canonical_responses());
    let result = compose_passive_observations(&runtime, &caller(), control()).await;

    assert_eq!(
        result
            .current_status
            .as_ref()
            .and_then(|values| values.health),
        Some(20.0)
    );
    assert_eq!(
        result
            .inventory
            .as_ref()
            .and_then(|values| values.selected_hotbar_slot),
        Some(0)
    );
    assert_eq!(
        result
            .sound
            .as_ref()
            .and_then(|values| values.recent_sounds.as_ref())
            .map(Vec::len),
        Some(0)
    );
    let viewport = result.viewport.expect("viewport frame");
    assert_eq!(
        viewport.frame.as_ref().expect("viewport frame").coordinates,
        mineintent_contracts::minecraft::ViewportCoordinateSystem::MinecraftWorldAbsolute
    );
    assert!(result.omissions.is_empty());
    let encoded = serde_json::to_value(viewport).expect("passive viewport JSON");
    assert!(encoded.get("visibleBlocks").is_none());
    assert!(encoded.get("lookedAtBlock").is_none());
}

#[tokio::test]
async fn ts_compose_passive_observations_keeps_current_status_when_three_reads_are_denied() {
    let mut responses = canonical_responses();
    responses[1] = error_result(
        InformationInterfaceId::InventoryInformation,
        InformationErrorCode::AudienceDenied,
        "inventory denied",
    );
    responses[2] = error_result(
        InformationInterfaceId::SoundInformation,
        InformationErrorCode::AudienceDenied,
        "sound denied",
    );
    responses[3] = error_result(
        InformationInterfaceId::ViewportInformation,
        InformationErrorCode::AudienceDenied,
        "viewport denied",
    );

    let runtime = RecordingRuntime::new(responses);
    let result = compose_passive_observations(&runtime, &caller(), control()).await;

    assert_eq!(
        result
            .current_status
            .as_ref()
            .and_then(|values| values.health),
        Some(20.0)
    );
    assert!(result.inventory.is_none());
    assert_eq!(result.omissions.len(), 3);
    assert!(result.omissions.iter().all(|omission| {
        omission.reason == InformationOmissionReason::AudienceDenied && omission.fields.is_empty()
    }));
    assert_eq!(
        result
            .omissions
            .iter()
            .map(|omission| omission.interface_id)
            .collect::<Vec<_>>(),
        vec![
            PassiveInterfaceId::InventoryInformation,
            PassiveInterfaceId::SoundInformation,
            PassiveInterfaceId::ViewportInformation,
        ]
    );
}

#[tokio::test]
async fn composer_uses_fixed_order_exact_schema_fields_and_shared_control() {
    let runtime = RecordingRuntime::new(canonical_responses());
    let _ = compose_passive_observations(&runtime, &caller(), control()).await;
    let calls = runtime.calls();

    let expected = vec![
        json!({
            "interfaceId": "current_status",
            "operation": "read",
            "schemaRevision": "current-status:1",
            "fields": ["health", "food", "foodSaturation", "oxygen", "experienceLevel", "statusEffects"],
        }),
        json!({
            "interfaceId": "inventory_information",
            "operation": "read",
            "schemaRevision": "inventory-information:1",
            "fields": ["selectedHotbarSlot", "slots"],
        }),
        json!({
            "interfaceId": "sound_information",
            "operation": "read",
            "schemaRevision": "sound-information:1",
            "fields": ["recentSounds"],
        }),
        json!({
            "interfaceId": "viewport_information",
            "operation": "read",
            "schemaRevision": "viewport-information:10",
            "fields": ["frame"],
        }),
    ];
    assert_eq!(calls.len(), expected.len());
    for (call, expected) in calls.iter().zip(expected) {
        assert_eq!(
            serde_json::from_str::<Value>(&call.request).expect("request JSON"),
            expected
        );
    }
    assert!(calls
        .windows(2)
        .all(|calls| calls[0].cancellation_data == calls[1].cancellation_data));
    assert!(calls
        .windows(2)
        .all(|calls| calls[0].deadline_data == calls[1].deadline_data));
}

#[tokio::test]
async fn composer_preserves_partial_and_unavailable_fields_and_continues() {
    let current_unavailable = vec![
        InformationUnavailableField {
            field: "food".to_owned(),
            reason: InformationReadUnavailableReason::NotCurrentlyDisplayed,
        },
        InformationUnavailableField {
            field: "foodSaturation".to_owned(),
            reason: InformationReadUnavailableReason::NotExposed,
        },
        InformationUnavailableField {
            field: "oxygen".to_owned(),
            reason: InformationReadUnavailableReason::NotExposed,
        },
        InformationUnavailableField {
            field: "experienceLevel".to_owned(),
            reason: InformationReadUnavailableReason::NotExposed,
        },
        InformationUnavailableField {
            field: "statusEffects".to_owned(),
            reason: InformationReadUnavailableReason::NotExposed,
        },
    ];
    let responses = vec![
        read_response(
            InformationInterfaceId::CurrentStatus,
            "current-status:1",
            json!({"health": 18.0}),
            current_unavailable,
        ),
        canonical_responses().remove(1),
        read_response(
            InformationInterfaceId::SoundInformation,
            "sound-information:1",
            json!({}),
            vec![InformationUnavailableField {
                field: "recentSounds".to_owned(),
                reason: InformationReadUnavailableReason::NotConnected,
            }],
        ),
        error_result(
            InformationInterfaceId::ViewportInformation,
            InformationErrorCode::ScopeChanged,
            "scope changed",
        ),
    ];
    let runtime = RecordingRuntime::new(responses);
    let result = compose_passive_observations(&runtime, &caller(), control()).await;

    let status = result.current_status.expect("partial status values");
    assert_eq!(status.health, Some(18.0));
    assert_eq!(status.oxygen, None);
    let status_omission = result
        .omissions
        .iter()
        .find(|omission| omission.interface_id == PassiveInterfaceId::CurrentStatus)
        .expect("status omission");
    assert_eq!(status_omission.reason, InformationOmissionReason::Partial);
    assert_eq!(status_omission.fields.len(), 5);
    assert_eq!(
        status_omission.fields[0].reason,
        mineintent_contracts::information::InformationUnavailableReason::NotCurrentlyDisplayed
    );

    let sound_omission = result
        .omissions
        .iter()
        .find(|omission| omission.interface_id == PassiveInterfaceId::SoundInformation)
        .expect("sound omission");
    assert_eq!(
        sound_omission.reason,
        InformationOmissionReason::Unavailable
    );
    assert_eq!(sound_omission.fields[0].field, "recentSounds");
    assert!(result.omissions.iter().any(|omission| omission.interface_id
        == PassiveInterfaceId::ViewportInformation
        && omission.reason == InformationOmissionReason::ScopeChanged));
    assert!(result.inventory.is_some());
}

#[tokio::test]
async fn composer_maps_deadline_scope_provider_and_missing_interface_failures() {
    let mut responses = canonical_responses();
    responses[1] = error_result(
        InformationInterfaceId::InventoryInformation,
        InformationErrorCode::DeadlineExceeded,
        "inventory deadline",
    );
    responses[2] = error_result(
        InformationInterfaceId::SoundInformation,
        InformationErrorCode::ProviderFailed,
        "sound provider failed",
    );
    responses[3] = error_result(
        InformationInterfaceId::ViewportInformation,
        InformationErrorCode::UnknownInterface,
        "viewport provider is not registered",
    );
    let runtime = RecordingRuntime::new(responses);
    let result = compose_passive_observations(&runtime, &caller(), control()).await;

    assert!(result.current_status.is_some());
    assert!(result.inventory.is_none());
    assert!(result.sound.is_none());
    assert!(result.viewport.is_none());
    assert_eq!(
        result
            .omissions
            .iter()
            .map(|omission| (omission.interface_id, omission.reason))
            .collect::<Vec<_>>(),
        vec![
            (
                PassiveInterfaceId::InventoryInformation,
                InformationOmissionReason::DeadlineExceeded,
            ),
            (
                PassiveInterfaceId::SoundInformation,
                InformationOmissionReason::ProviderFailed,
            ),
            (
                PassiveInterfaceId::ViewportInformation,
                InformationOmissionReason::Unavailable,
            ),
        ]
    );
}

#[tokio::test]
async fn composer_exposes_malformed_provider_values_as_provider_failed() {
    let mut responses = canonical_responses();
    responses[0] = read_response(
        InformationInterfaceId::CurrentStatus,
        "current-status:1",
        json!({"health": "20"}),
        vec![
            InformationUnavailableField {
                field: "food".to_owned(),
                reason: InformationReadUnavailableReason::NotExposed,
            },
            InformationUnavailableField {
                field: "foodSaturation".to_owned(),
                reason: InformationReadUnavailableReason::NotExposed,
            },
            InformationUnavailableField {
                field: "oxygen".to_owned(),
                reason: InformationReadUnavailableReason::NotExposed,
            },
            InformationUnavailableField {
                field: "experienceLevel".to_owned(),
                reason: InformationReadUnavailableReason::NotExposed,
            },
            InformationUnavailableField {
                field: "statusEffects".to_owned(),
                reason: InformationReadUnavailableReason::NotExposed,
            },
        ],
    );
    let runtime = RecordingRuntime::new(responses);
    let result = compose_passive_observations(&runtime, &caller(), control()).await;

    assert!(result.current_status.is_none());
    assert!(result.inventory.is_some());
    let failure = result
        .omissions
        .iter()
        .find(|omission| omission.interface_id == PassiveInterfaceId::CurrentStatus)
        .expect("malformed provider omission");
    assert_eq!(failure.reason, InformationOmissionReason::ProviderFailed);
    assert!(failure
        .message
        .as_deref()
        .is_some_and(|message| message.contains("strict passive DTO validation")));
}
