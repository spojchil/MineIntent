use std::{collections::HashSet, future::pending, sync::Arc};

use mineintent_contracts::minecraft::{BoxFuture, CancellationSignal, OperationControl};
use mineintent_middle::information::contracts::{
    InformationAudience, InformationConnectionState, InformationGrantPurpose, InformationProvider,
    InformationProviderCaller, InformationProviderContext, InformationReferenceIssueError,
    InformationReferenceIssueRequest, InformationReferenceIssuer, InformationScopeSnapshot,
    InformationSelectorRef, ProviderReadRequest, ProviderReadResult,
};

struct NeverCancelled;

impl CancellationSignal for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(pending())
    }
}

struct UnusedReferenceIssuer;

impl InformationReferenceIssuer for UnusedReferenceIssuer {
    fn issue(
        &self,
        _request: InformationReferenceIssueRequest,
    ) -> Result<InformationSelectorRef, InformationReferenceIssueError> {
        Err(InformationReferenceIssueError::InvalidMetadata)
    }
}

pub struct ProviderFixture {
    scope: InformationScopeSnapshot,
    refs: UnusedReferenceIssuer,
}

impl ProviderFixture {
    pub fn new() -> Self {
        Self {
            scope: InformationScopeSnapshot {
                process_session_id: "s".to_owned(),
                connection_state: InformationConnectionState::Play,
                connection_epoch: 1,
                world_id: None,
                dimension: None,
                ui_revision: 0,
                screen_instance_id: None,
                screen_revision: None,
                captured_at: "2026-08-01T00:00:00.000Z".to_owned(),
            },
            refs: UnusedReferenceIssuer,
        }
    }

    pub fn context(&self) -> InformationProviderContext<'_> {
        InformationProviderContext {
            now: "2026-08-01T00:00:01.000Z",
            scope: &self.scope,
            caller: InformationProviderCaller {
                audience: InformationAudience::Participant,
                purpose: InformationGrantPurpose::ParticipantContext,
            },
            refs: &self.refs,
        }
    }
}

pub fn operation_control() -> OperationControl {
    OperationControl::new(Arc::new(NeverCancelled), None)
}

pub async fn read(
    provider: &dyn InformationProvider,
    fixture: &ProviderFixture,
    request: ProviderReadRequest,
) -> ProviderReadResult {
    provider
        .read(fixture.context(), request, operation_control())
        .await
        .expect("provider fixture read should succeed")
}

/// Mechanical Rust counterpart of `information/testing/provider-contract.ts`.
pub async fn assert_information_provider_contract(
    provider: &dyn InformationProvider,
    fixture: &ProviderFixture,
    request: ProviderReadRequest,
) {
    let definition = provider.definition();
    let field_ids = definition.fields.keys().collect::<Vec<_>>();
    assert!(!field_ids.is_empty(), "provider must define fields");
    assert_eq!(
        field_ids.iter().copied().collect::<HashSet<_>>().len(),
        field_ids.len(),
        "provider field ids must be unique"
    );
    for (field_id, field) in &definition.fields {
        assert!(
            !field.description.trim().is_empty(),
            "{field_id} must have a description"
        );
        assert!(
            !field.value_type.trim().is_empty(),
            "{field_id} must have a value type"
        );
        assert!(
            !field.source_kinds.is_empty(),
            "{field_id} must declare a source kind"
        );
    }

    let availability = provider.availability(&fixture.context());
    assert_non_negative_integer_revision(availability.information_revision);

    let requested = request.fields.iter().cloned().collect::<HashSet<_>>();
    let result = read(provider, fixture, request).await;
    assert_non_negative_integer_revision(result.information_revision);
    let mut unavailable = HashSet::new();
    for item in &result.unavailable {
        assert!(
            requested.contains(&item.field),
            "provider returned unrequested unavailable field {}",
            item.field
        );
        assert!(
            unavailable.insert(item.field.clone()),
            "provider repeated unavailable field {}",
            item.field
        );
    }
    for (field_id, value) in &result.values {
        assert!(
            requested.contains(field_id),
            "provider returned unrequested value {field_id}"
        );
        assert!(
            !unavailable.contains(field_id),
            "{field_id} cannot be both available and unavailable"
        );
        definition.fields[field_id]
            .value_schema
            .parse(value.clone())
            .unwrap_or_else(|error| panic!("{field_id} failed its runtime schema: {error}"));
    }
    for field_id in requested {
        assert!(
            result.values.contains_key(&field_id) || unavailable.contains(&field_id),
            "{field_id} was omitted without reason"
        );
    }
}

fn assert_non_negative_integer_revision(revision: u64) {
    let wire_value = serde_json::to_value(revision)
        .expect("a provider revision should serialize as a JSON integer");
    assert_eq!(wire_value.as_u64(), Some(revision));
}

pub fn request(fields: &[&str]) -> ProviderReadRequest {
    ProviderReadRequest {
        fields: fields.iter().map(|field| (*field).to_owned()).collect(),
        selector: None,
        page: mineintent_middle::information::contracts::ProviderPageRequest {
            limit: 1,
            state: None,
        },
    }
}
