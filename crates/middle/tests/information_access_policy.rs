use std::collections::BTreeMap;

use mineintent_middle::information::{
    access_policy::{
        InMemoryInformationAccessPolicy, InformationAccessPolicy,
        InformationAuthorizationDenialReason, InformationAuthorizationOperation,
        InformationAuthorizationResult,
    },
    contracts::{
        InformationAllInterfaces, InformationAllowedInterfaces, InformationAudience,
        InformationConnectionState, InformationGrant, InformationGrantPurpose,
        InformationInterfaceId, InformationProviderDescriptor, InformationScopeSnapshot,
    },
};

const DENIED: InformationAuthorizationResult = InformationAuthorizationResult::Denied {
    reason: InformationAuthorizationDenialReason::AudienceDenied,
};

fn grant() -> InformationGrant {
    InformationGrant {
        id: "grant-1".to_owned(),
        principal_id: "model-1".to_owned(),
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

fn provider() -> InformationProviderDescriptor {
    InformationProviderDescriptor {
        id: InformationInterfaceId::CurrentStatus,
        description: "Current status".to_owned(),
        schema_revision: "current_status:1".to_owned(),
        audiences: vec![InformationAudience::Participant],
        field_ids: vec!["health".to_owned(), "food_display".to_owned()],
    }
}

fn scope() -> InformationScopeSnapshot {
    InformationScopeSnapshot {
        process_session_id: "process-1".to_owned(),
        connection_state: InformationConnectionState::Play,
        connection_epoch: 4,
        world_id: Some("world-1".to_owned()),
        dimension: Some("minecraft:overworld".to_owned()),
        ui_revision: 8,
        screen_instance_id: Some("screen-1".to_owned()),
        screen_revision: Some(3),
        captured_at: "2026-07-14T00:00:00.000Z".to_owned(),
    }
}

fn authorize(
    policy: &dyn InformationAccessPolicy,
    grant: &InformationGrant,
    provider: &InformationProviderDescriptor,
    operation: InformationAuthorizationOperation,
    fields: &[&str],
    scope: &InformationScopeSnapshot,
) -> InformationAuthorizationResult {
    policy.authorize(
        grant,
        provider,
        operation,
        &fields
            .iter()
            .map(|field| (*field).to_owned())
            .collect::<Vec<_>>(),
        scope,
    )
}

#[test]
fn source_characterization_put_resolve_replace_and_revoke_follow_grant_id_and_principal() {
    let policy = InMemoryInformationAccessPolicy::new();
    let original = grant();
    policy.put(&original).expect("put should work");
    assert_eq!(
        policy.resolve("grant-1", "model-1"),
        Ok(Some(original.clone()))
    );
    assert_eq!(policy.resolve("grant-1", "other-model"), Ok(None));
    assert_eq!(policy.resolve("missing", "model-1"), Ok(None));

    let mut replacement = original;
    replacement.principal_id = "model-2".to_owned();
    replacement.audience = InformationAudience::Controller;
    policy.put(&replacement).expect("replacement should work");
    assert_eq!(policy.resolve("grant-1", "model-1"), Ok(None));
    assert_eq!(policy.resolve("grant-1", "model-2"), Ok(Some(replacement)));

    policy.revoke("grant-1").expect("revoke should work");
    assert_eq!(policy.resolve("grant-1", "model-2"), Ok(None));
    policy
        .revoke("missing")
        .expect("revoking a missing id is a no-op");
}

#[test]
fn source_characterization_expiry_denies_only_when_both_timestamps_parse() {
    let policy = InMemoryInformationAccessPolicy::new();
    let provider = provider();
    let scope = scope();
    let mut candidate = grant();

    candidate.valid_until = Some("2026-07-14T00:00:00.000Z".to_owned());
    assert_eq!(
        authorize(
            &policy,
            &candidate,
            &provider,
            InformationAuthorizationOperation::Read,
            &["health"],
            &scope,
        ),
        DENIED
    );
    candidate.valid_until = Some("2026-07-14T00:00:00.001Z".to_owned());
    assert_eq!(
        authorize(
            &policy,
            &candidate,
            &provider,
            InformationAuthorizationOperation::Read,
            &["health"],
            &scope,
        ),
        InformationAuthorizationResult::Allowed
    );
    candidate.valid_until = Some("not-a-timestamp".to_owned());
    assert_eq!(
        authorize(
            &policy,
            &candidate,
            &provider,
            InformationAuthorizationOperation::Read,
            &["health"],
            &scope,
        ),
        InformationAuthorizationResult::Allowed
    );
    candidate.valid_until = Some("2026-07-13".to_owned());
    assert_eq!(
        authorize(
            &policy,
            &candidate,
            &provider,
            InformationAuthorizationOperation::Read,
            &["health"],
            &scope,
        ),
        DENIED,
        "ECMAScript date-only input is UTC midnight and already expired"
    );
    candidate.valid_until = Some("2026-07-13T00:00:00.000Z".to_owned());
    let mut invalid_now = scope;
    invalid_now.captured_at = "invalid-now".to_owned();
    assert_eq!(
        authorize(
            &policy,
            &candidate,
            &provider,
            InformationAuthorizationOperation::Read,
            &["health"],
            &invalid_now,
        ),
        InformationAuthorizationResult::Allowed
    );
}

#[test]
fn source_characterization_provider_audience_and_interface_rules_are_closed() {
    let policy = InMemoryInformationAccessPolicy::new();
    let mut candidate = grant();
    let mut descriptor = provider();
    let scope = scope();

    assert_eq!(
        authorize(
            &policy,
            &candidate,
            &descriptor,
            InformationAuthorizationOperation::Catalog,
            &[],
            &scope,
        ),
        InformationAuthorizationResult::Allowed
    );
    descriptor.audiences = vec![InformationAudience::Controller];
    assert_eq!(
        authorize(
            &policy,
            &candidate,
            &descriptor,
            InformationAuthorizationOperation::Catalog,
            &[],
            &scope,
        ),
        DENIED
    );

    descriptor = provider();
    candidate.allowed_interfaces = InformationAllowedInterfaces::Interfaces(vec![descriptor.id]);
    assert_eq!(
        authorize(
            &policy,
            &candidate,
            &descriptor,
            InformationAuthorizationOperation::Help,
            &[],
            &scope,
        ),
        InformationAuthorizationResult::Allowed
    );
    candidate.allowed_interfaces = InformationAllowedInterfaces::Interfaces(vec![
        InformationInterfaceId::InventoryInformation,
    ]);
    assert_eq!(
        authorize(
            &policy,
            &candidate,
            &descriptor,
            InformationAuthorizationOperation::Help,
            &[],
            &scope,
        ),
        DENIED
    );
}

#[test]
fn source_characterization_optional_scope_bindings_are_enforced_independently() {
    let policy = InMemoryInformationAccessPolicy::new();
    let provider = provider();
    let scope = scope();
    let mut candidate = grant();
    candidate.connection_epoch = Some(4);
    candidate.world_id = Some("world-1".to_owned());
    candidate.screen_instance_id = Some("screen-1".to_owned());
    assert_eq!(
        authorize(
            &policy,
            &candidate,
            &provider,
            InformationAuthorizationOperation::Read,
            &["health"],
            &scope,
        ),
        InformationAuthorizationResult::Allowed
    );

    let mut wrong = scope.clone();
    wrong.connection_epoch = 5;
    assert_eq!(
        authorize(
            &policy,
            &candidate,
            &provider,
            InformationAuthorizationOperation::Read,
            &["health"],
            &wrong,
        ),
        DENIED
    );
    wrong = scope.clone();
    wrong.world_id = None;
    assert_eq!(
        authorize(
            &policy,
            &candidate,
            &provider,
            InformationAuthorizationOperation::Read,
            &["health"],
            &wrong,
        ),
        DENIED
    );
    wrong = scope;
    wrong.screen_instance_id = Some("screen-2".to_owned());
    assert_eq!(
        authorize(
            &policy,
            &candidate,
            &provider,
            InformationAuthorizationOperation::Read,
            &["health"],
            &wrong,
        ),
        DENIED
    );

    candidate.connection_epoch = None;
    candidate.world_id = None;
    candidate.screen_instance_id = None;
    assert_eq!(
        authorize(
            &policy,
            &candidate,
            &provider,
            InformationAuthorizationOperation::Read,
            &["health"],
            &wrong,
        ),
        InformationAuthorizationResult::Allowed
    );
}

#[test]
fn source_characterization_field_allowlist_is_optional_and_per_interface() {
    let policy = InMemoryInformationAccessPolicy::new();
    let provider = provider();
    let scope = scope();
    let mut candidate = grant();

    candidate.allowed_fields = Some(BTreeMap::from([(
        InformationInterfaceId::InventoryInformation,
        vec!["slots".to_owned()],
    )]));
    assert_eq!(
        authorize(
            &policy,
            &candidate,
            &provider,
            InformationAuthorizationOperation::Read,
            &["health", "food_display"],
            &scope,
        ),
        InformationAuthorizationResult::Allowed
    );

    candidate
        .allowed_fields
        .as_mut()
        .expect("map exists")
        .insert(provider.id, vec!["health".to_owned()]);
    assert_eq!(
        authorize(
            &policy,
            &candidate,
            &provider,
            InformationAuthorizationOperation::Read,
            &["health"],
            &scope,
        ),
        InformationAuthorizationResult::Allowed
    );
    assert_eq!(
        authorize(
            &policy,
            &candidate,
            &provider,
            InformationAuthorizationOperation::Read,
            &["health", "food_display"],
            &scope,
        ),
        DENIED
    );
    assert_eq!(
        authorize(
            &policy,
            &candidate,
            &provider,
            InformationAuthorizationOperation::Read,
            &[],
            &scope,
        ),
        InformationAuthorizationResult::Allowed
    );
}

#[test]
fn source_characterization_catalog_help_and_read_use_the_same_authorization_rule() {
    let policy = InMemoryInformationAccessPolicy::new();
    let provider = provider();
    let scope = scope();
    let mut candidate = grant();
    candidate.allowed_interfaces = InformationAllowedInterfaces::Interfaces(Vec::new());

    for operation in [
        InformationAuthorizationOperation::Catalog,
        InformationAuthorizationOperation::Help,
        InformationAuthorizationOperation::Read,
    ] {
        assert_eq!(
            authorize(
                &policy,
                &candidate,
                &provider,
                operation,
                &["health"],
                &scope,
            ),
            DENIED
        );
    }
}

#[test]
fn rust_contract_trait_object_and_owned_clones_isolate_the_store() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<InMemoryInformationAccessPolicy>();

    let policy = InMemoryInformationAccessPolicy::new();
    let mut external = grant();
    external.allowed_interfaces =
        InformationAllowedInterfaces::Interfaces(vec![InformationInterfaceId::CurrentStatus]);
    external.allowed_fields = Some(BTreeMap::from([(
        InformationInterfaceId::CurrentStatus,
        vec!["health".to_owned()],
    )]));
    policy.put(&external).expect("put should work");

    external.allowed_interfaces = InformationAllowedInterfaces::Interfaces(Vec::new());
    external
        .allowed_fields
        .as_mut()
        .expect("external map exists")
        .get_mut(&InformationInterfaceId::CurrentStatus)
        .expect("external fields exist")
        .push("mutated".to_owned());

    let port: &dyn InformationAccessPolicy = &policy;
    let mut first = port
        .resolve("grant-1", "model-1")
        .expect("resolve should work")
        .expect("grant should exist");
    assert_eq!(
        first.allowed_interfaces,
        InformationAllowedInterfaces::Interfaces(vec![InformationInterfaceId::CurrentStatus])
    );
    first.allowed_fields.as_mut().expect("map exists").clear();
    let second = port
        .resolve("grant-1", "model-1")
        .expect("resolve should work")
        .expect("grant should still exist");
    assert_eq!(
        second.allowed_fields,
        Some(BTreeMap::from([(
            InformationInterfaceId::CurrentStatus,
            vec!["health".to_owned()],
        )]))
    );
    assert_eq!(
        authorize(
            port,
            &second,
            &provider(),
            InformationAuthorizationOperation::Read,
            &["health"],
            &scope(),
        ),
        InformationAuthorizationResult::Allowed
    );
}
