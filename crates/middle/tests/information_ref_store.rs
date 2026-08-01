use std::sync::{
    atomic::{AtomicI64, Ordering},
    Arc,
};

use mineintent_middle::information::{
    contracts::{
        InformationAllInterfaces, InformationAllowedInterfaces, InformationAudience,
        InformationConnectionState, InformationGrant, InformationGrantPurpose,
        InformationInterfaceId, InformationInvalidationEvent, InformationReferenceIssueRequest,
        InformationScopeSnapshot,
    },
    ref_store::{
        InformationRefClock, InformationRefIssuer, InformationRefIssuerInput,
        InformationRefResolveInput, InformationRefStore, InformationRefStoreError,
        InformationRefStoreOptions, DEFAULT_MAX_REFERENCE_ENTRIES,
        DEFAULT_MAX_REFERENCE_ENTRIES_PER_INTERFACE, DEFAULT_MAX_REFERENCE_ENTRIES_PER_PRINCIPAL,
        DEFAULT_MAX_REFERENCE_ISSUES_PER_ISSUER, DEFAULT_MAX_REFERENCE_PAYLOAD_BYTES,
        DEFAULT_REFERENCE_TTL_MS,
    },
};
use serde_json::{json, Value};

const FIXED_NOW_MS: i64 = 1_783_987_200_000;

struct FixedClock {
    now: AtomicI64,
}

impl FixedClock {
    fn new(now: i64) -> Self {
        Self {
            now: AtomicI64::new(now),
        }
    }

    fn set(&self, now: i64) {
        self.now.store(now, Ordering::SeqCst);
    }
}

impl InformationRefClock for FixedClock {
    fn now_millis(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
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

fn options(clock: Arc<dyn InformationRefClock>) -> InformationRefStoreOptions {
    InformationRefStoreOptions {
        clock,
        ..InformationRefStoreOptions::default()
    }
}

fn issuer(
    store: &InformationRefStore,
    interface_id: InformationInterfaceId,
    principal_id: &str,
    grant: InformationGrant,
    scope: InformationScopeSnapshot,
) -> InformationRefIssuer {
    store.issuer(InformationRefIssuerInput {
        interface_id,
        principal_id: principal_id.to_owned(),
        grant,
        scope,
    })
}

fn item_request(slot: i64, bind_to_screen: bool) -> InformationReferenceIssueRequest {
    InformationReferenceIssueRequest {
        kind: "item".to_owned(),
        payload: json!({"slot": slot}),
        allowed_interfaces: vec![InformationInterfaceId::ItemTooltipInformation],
        based_on_information_revision: 1,
        valid_until: None,
        bind_to_screen: bind_to_screen.then_some(true),
    }
}

fn resolve_input(
    reference: mineintent_middle::information::contracts::InformationSelectorRef,
) -> InformationRefResolveInput {
    InformationRefResolveInput {
        reference,
        target_interface: InformationInterfaceId::ItemTooltipInformation,
        principal_id: "model-1".to_owned(),
        grant: grant(),
        scope: scope(),
        accepted_kinds: None,
    }
}

#[test]
fn opaque_references_bind_principal_grant_scope_target_and_full_ref_content() {
    let clock = Arc::new(FixedClock::new(FIXED_NOW_MS));
    let store = InformationRefStore::new(options(clock)).expect("valid store options");
    let reference = issuer(
        &store,
        InformationInterfaceId::InventoryInformation,
        "model-1",
        grant(),
        scope(),
    )
    .issue(InformationReferenceIssueRequest {
        kind: "item".to_owned(),
        payload: json!({"slot": 5, "internalId": 72}),
        allowed_interfaces: vec![InformationInterfaceId::ItemTooltipInformation],
        based_on_information_revision: 11,
        valid_until: None,
        bind_to_screen: Some(true),
    })
    .expect("reference should issue");

    let mut valid = resolve_input(reference.clone());
    valid.accepted_kinds = Some(vec!["item".to_owned()]);
    assert_eq!(
        store.resolve(valid).expect("resolve should not fail"),
        Some(json!({"slot": 5, "internalId": 72}))
    );

    let mut tampered = reference.clone();
    tampered.based_on_information_revision = 12;
    assert_eq!(store.resolve(resolve_input(tampered)), Ok(None));

    let mut wrong_target = resolve_input(reference.clone());
    wrong_target.target_interface = InformationInterfaceId::CurrentStatus;
    assert_eq!(store.resolve(wrong_target), Ok(None));

    let mut wrong_principal = resolve_input(reference.clone());
    wrong_principal.principal_id = "other-model".to_owned();
    assert_eq!(store.resolve(wrong_principal), Ok(None));

    let mut wrong_dimension = resolve_input(reference);
    wrong_dimension.scope.dimension = Some("minecraft:the_nether".to_owned());
    assert_eq!(store.resolve(wrong_dimension), Ok(None));

    store
        .invalidate(&InformationInvalidationEvent::ScreenChanged {
            screen_instance_id: Some("screen-2".to_owned()),
            screen_revision: Some(1),
        })
        .expect("screen invalidation should succeed");
    assert_eq!(store.size(), Ok(0));
}

#[test]
fn screen_bound_references_require_a_concrete_screen_revision() {
    let store = InformationRefStore::default();
    let mut no_screen = scope();
    no_screen.screen_instance_id = None;
    no_screen.screen_revision = None;
    let issuer = issuer(
        &store,
        InformationInterfaceId::InventoryInformation,
        "model-1",
        grant(),
        no_screen,
    );
    assert_eq!(
        issuer.issue(item_request(1, true)),
        Err(InformationRefStoreError::ActiveScreenRevisionRequired)
    );
}

#[test]
fn reference_limits_isolate_principals_and_interfaces_and_bound_per_read_payloads() {
    let clock = Arc::new(FixedClock::new(FIXED_NOW_MS));
    let limited = InformationRefStore::new(InformationRefStoreOptions {
        max_entries: 4,
        max_entries_per_principal: 1,
        max_entries_per_interface: 2,
        max_payload_bytes: 32,
        max_issues_per_issuer: 1,
        ttl_ms: 1_000,
        clock,
    })
    .expect("valid limited options");
    let limited_issuer = issuer(
        &limited,
        InformationInterfaceId::InventoryInformation,
        "model-1",
        grant(),
        scope(),
    );
    limited_issuer
        .issue(item_request(1, false))
        .expect("first issue should succeed");
    assert_eq!(
        limited_issuer.issue(item_request(2, false)),
        Err(InformationRefStoreError::PerIssuerLimitExceeded)
    );
    assert_eq!(
        issuer(
            &limited,
            InformationInterfaceId::HotbarInformation,
            "model-1",
            grant(),
            scope(),
        )
        .issue(item_request(2, false)),
        Err(InformationRefStoreError::CapacityExceeded)
    );

    let payload_limited = InformationRefStore::new(InformationRefStoreOptions {
        max_payload_bytes: 8,
        ..InformationRefStoreOptions::default()
    })
    .expect("valid payload options");
    let request = InformationReferenceIssueRequest {
        payload: json!({"hidden": "far too large"}),
        ..item_request(1, false)
    };
    assert!(matches!(
        issuer(
            &payload_limited,
            InformationInterfaceId::InventoryInformation,
            "model-1",
            grant(),
            scope(),
        )
        .issue(request),
        Err(InformationRefStoreError::PayloadByteLimitExceeded { .. })
    ));
}

#[test]
fn contract_defaults_ttl_expiry_cleanup_and_accepted_kind_match_oracle() {
    assert_eq!(DEFAULT_MAX_REFERENCE_ENTRIES, 2_048);
    assert_eq!(DEFAULT_MAX_REFERENCE_ENTRIES_PER_PRINCIPAL, 512);
    assert_eq!(DEFAULT_MAX_REFERENCE_ENTRIES_PER_INTERFACE, 256);
    assert_eq!(DEFAULT_MAX_REFERENCE_PAYLOAD_BYTES, 8_192);
    assert_eq!(DEFAULT_MAX_REFERENCE_ISSUES_PER_ISSUER, 32);
    assert_eq!(DEFAULT_REFERENCE_TTL_MS, 60_000);

    let clock = Arc::new(FixedClock::new(FIXED_NOW_MS));
    let store = InformationRefStore::new(options(clock.clone())).expect("valid options");
    let reference = issuer(
        &store,
        InformationInterfaceId::InventoryInformation,
        "model-1",
        grant(),
        scope(),
    )
    .issue(item_request(1, false))
    .expect("reference should issue");
    assert_eq!(
        reference.valid_until.as_deref(),
        Some("2026-07-14T00:01:00.000Z")
    );
    let mut wrong_kind = resolve_input(reference.clone());
    wrong_kind.accepted_kinds = Some(vec!["entity".to_owned()]);
    assert_eq!(store.resolve(wrong_kind), Ok(None));

    let mut returned = store
        .resolve(resolve_input(reference.clone()))
        .expect("resolve should not fail")
        .expect("reference should resolve");
    returned["slot"] = json!(999);
    assert_eq!(
        store.resolve(resolve_input(reference.clone())),
        Ok(Some(json!({"slot": 1})))
    );

    clock.set(FIXED_NOW_MS + 60_000);
    assert_eq!(store.size(), Ok(1));
    assert_eq!(store.resolve(resolve_input(reference)), Ok(None));
    assert_eq!(store.size(), Ok(0));

    let cleanup_store = InformationRefStore::new(InformationRefStoreOptions {
        max_entries: 1,
        ttl_ms: 1,
        clock: clock.clone(),
        ..InformationRefStoreOptions::default()
    })
    .expect("valid cleanup options");
    issuer(
        &cleanup_store,
        InformationInterfaceId::InventoryInformation,
        "model-1",
        grant(),
        scope(),
    )
    .issue(item_request(1, false))
    .expect("first cleanup reference");
    clock.set(FIXED_NOW_MS + 60_002);
    issuer(
        &cleanup_store,
        InformationInterfaceId::HotbarInformation,
        "model-2",
        grant(),
        scope(),
    )
    .issue(item_request(2, false))
    .expect("expired entry should be evicted before capacity check");
    assert_eq!(cleanup_store.size(), Ok(1));
}

#[test]
fn contract_global_and_per_interface_capacities_are_independent() {
    let store = InformationRefStore::new(InformationRefStoreOptions {
        max_entries: 2,
        max_entries_per_principal: 2,
        max_entries_per_interface: 1,
        ..InformationRefStoreOptions::default()
    })
    .expect("valid capacity options");
    issuer(
        &store,
        InformationInterfaceId::InventoryInformation,
        "model-1",
        grant(),
        scope(),
    )
    .issue(item_request(1, false))
    .expect("first entry");
    assert_eq!(
        issuer(
            &store,
            InformationInterfaceId::InventoryInformation,
            "model-2",
            grant(),
            scope(),
        )
        .issue(item_request(2, false)),
        Err(InformationRefStoreError::CapacityExceeded)
    );
    issuer(
        &store,
        InformationInterfaceId::HotbarInformation,
        "model-2",
        grant(),
        scope(),
    )
    .issue(item_request(2, false))
    .expect("different interface and principal should fit");
    assert_eq!(
        issuer(
            &store,
            InformationInterfaceId::CurrentStatus,
            "model-3",
            grant(),
            scope(),
        )
        .issue(item_request(3, false)),
        Err(InformationRefStoreError::CapacityExceeded)
    );
}

#[test]
fn contract_resolve_checks_every_binding_and_every_ref_field() {
    let clock = Arc::new(FixedClock::new(FIXED_NOW_MS));
    let store = InformationRefStore::new(options(clock)).expect("valid options");
    let reference = issuer(
        &store,
        InformationInterfaceId::InventoryInformation,
        "model-1",
        grant(),
        scope(),
    )
    .issue(item_request(4, true))
    .expect("reference should issue");

    let mut wrong_grant = resolve_input(reference.clone());
    wrong_grant.grant.id = "grant-2".to_owned();
    assert_eq!(store.resolve(wrong_grant), Ok(None));
    let mut wrong_audience = resolve_input(reference.clone());
    wrong_audience.grant.audience = InformationAudience::Controller;
    assert_eq!(store.resolve(wrong_audience), Ok(None));
    let mut wrong_epoch = resolve_input(reference.clone());
    wrong_epoch.scope.connection_epoch = 5;
    assert_eq!(store.resolve(wrong_epoch), Ok(None));
    let mut wrong_world = resolve_input(reference.clone());
    wrong_world.scope.world_id = Some("world-2".to_owned());
    assert_eq!(store.resolve(wrong_world), Ok(None));
    let mut wrong_screen_revision = resolve_input(reference.clone());
    wrong_screen_revision.scope.screen_revision = Some(4);
    assert_eq!(store.resolve(wrong_screen_revision), Ok(None));
    let mut deliberately_unbound_scope_fields = resolve_input(reference.clone());
    deliberately_unbound_scope_fields.scope.process_session_id = "process-2".to_owned();
    deliberately_unbound_scope_fields.scope.ui_revision += 1;
    deliberately_unbound_scope_fields.scope.captured_at = "2026-07-14T00:00:30.000Z".to_owned();
    assert_eq!(
        store.resolve(deliberately_unbound_scope_fields),
        Ok(Some(json!({"slot": 4})))
    );

    let mut mutations = Vec::new();
    let mut changed = reference.clone();
    changed.id.push_str("-forged");
    mutations.push(changed);
    let mut changed = reference.clone();
    changed.interface_id = InformationInterfaceId::HotbarInformation;
    mutations.push(changed);
    let mut changed = reference.clone();
    changed.connection_epoch += 1;
    mutations.push(changed);
    let mut changed = reference.clone();
    changed.world_id = Some("world-2".to_owned());
    mutations.push(changed);
    let mut changed = reference.clone();
    changed.screen_instance_id = Some("screen-2".to_owned());
    mutations.push(changed);
    let mut changed = reference.clone();
    changed.based_on_information_revision += 1;
    mutations.push(changed);
    let mut changed = reference;
    changed.valid_until = Some("2026-07-14T00:00:30.000Z".to_owned());
    mutations.push(changed);
    for changed in mutations {
        assert_eq!(store.resolve(resolve_input(changed)), Ok(None));
    }
}

#[test]
fn contract_all_invalidations_clear_and_size_match_store_semantics() {
    fn issued_store(bind_to_screen: bool) -> (InformationRefStore, Value) {
        let store = InformationRefStore::new(options(Arc::new(FixedClock::new(FIXED_NOW_MS))))
            .expect("valid options");
        let reference = issuer(
            &store,
            InformationInterfaceId::InventoryInformation,
            "model-1",
            grant(),
            scope(),
        )
        .issue(item_request(1, bind_to_screen))
        .expect("reference should issue");
        (
            store,
            serde_json::to_value(reference).expect("reference DTO serializes"),
        )
    }

    let (store, _) = issued_store(false);
    store
        .invalidate(&InformationInvalidationEvent::GrantEnded {
            grant_id: "other-grant".to_owned(),
        })
        .expect("invalidation");
    assert_eq!(store.size(), Ok(1));
    store
        .invalidate(&InformationInvalidationEvent::GrantEnded {
            grant_id: "grant-1".to_owned(),
        })
        .expect("invalidation");
    assert_eq!(store.size(), Ok(0));

    let (store, _) = issued_store(false);
    store
        .invalidate(&InformationInvalidationEvent::ConnectionChanged {
            connection_epoch: 4,
        })
        .expect("invalidation");
    assert_eq!(store.size(), Ok(1));
    store
        .invalidate(&InformationInvalidationEvent::ConnectionChanged {
            connection_epoch: 5,
        })
        .expect("invalidation");
    assert_eq!(store.size(), Ok(0));

    let (store, _) = issued_store(false);
    store
        .invalidate(&InformationInvalidationEvent::WorldChanged {
            world_id: Some("world-1".to_owned()),
            dimension: Some("minecraft:overworld".to_owned()),
        })
        .expect("invalidation");
    assert_eq!(store.size(), Ok(1));
    store
        .invalidate(&InformationInvalidationEvent::WorldChanged {
            world_id: Some("world-1".to_owned()),
            dimension: Some("minecraft:the_nether".to_owned()),
        })
        .expect("invalidation");
    assert_eq!(store.size(), Ok(0));

    let (unbound, _) = issued_store(false);
    unbound
        .invalidate(&InformationInvalidationEvent::ScreenChanged {
            screen_instance_id: Some("screen-2".to_owned()),
            screen_revision: Some(4),
        })
        .expect("invalidation");
    assert_eq!(unbound.size(), Ok(1));
    unbound.clear().expect("clear should succeed");
    assert_eq!(unbound.size(), Ok(0));

    let (bound, _) = issued_store(true);
    bound
        .invalidate(&InformationInvalidationEvent::ScreenChanged {
            screen_instance_id: Some("screen-1".to_owned()),
            screen_revision: Some(3),
        })
        .expect("invalidation");
    assert_eq!(bound.size(), Ok(1));
    bound
        .invalidate(&InformationInvalidationEvent::ScreenChanged {
            screen_instance_id: Some("screen-2".to_owned()),
            screen_revision: Some(4),
        })
        .expect("invalidation");
    assert_eq!(bound.size(), Ok(0));
}

#[test]
fn contract_metadata_lifetime_and_option_limits_return_structured_errors() {
    let invalid = InformationRefStoreOptions {
        max_entries: 0,
        ..InformationRefStoreOptions::default()
    };
    assert!(matches!(
        InformationRefStore::new(invalid),
        Err(InformationRefStoreError::InvalidLimits)
    ));

    let clock = Arc::new(FixedClock::new(FIXED_NOW_MS));
    let store = InformationRefStore::new(InformationRefStoreOptions {
        ttl_ms: 1_000,
        clock,
        ..InformationRefStoreOptions::default()
    })
    .expect("valid options");
    let metadata_issuer = issuer(
        &store,
        InformationInterfaceId::InventoryInformation,
        "model-1",
        grant(),
        scope(),
    );
    let mut request = item_request(1, false);
    request.allowed_interfaces.clear();
    assert_eq!(
        metadata_issuer.issue(request),
        Err(InformationRefStoreError::AllowedTargetRequired)
    );
    let mut request = item_request(1, false);
    request.kind = "  ".to_owned();
    assert_eq!(
        metadata_issuer.issue(request),
        Err(InformationRefStoreError::InvalidMetadata)
    );
    let mut request = item_request(1, false);
    request.valid_until = Some("not-a-date".to_owned());
    assert_eq!(
        metadata_issuer.issue(request),
        Err(InformationRefStoreError::InvalidMetadata)
    );
    let mut request = item_request(1, false);
    request.valid_until = Some("2026-07-14T00:00:01.001Z".to_owned());
    assert_eq!(
        metadata_issuer.issue(request),
        Err(InformationRefStoreError::LifetimeExceeded)
    );

    let utf8_limited = InformationRefStore::new(InformationRefStoreOptions {
        max_payload_bytes: 10,
        ..InformationRefStoreOptions::default()
    })
    .expect("valid UTF-8 limit");
    let request = InformationReferenceIssueRequest {
        payload: json!({"x": "矿"}),
        ..item_request(1, false)
    };
    assert!(matches!(
        issuer(
            &utf8_limited,
            InformationInterfaceId::InventoryInformation,
            "model-1",
            grant(),
            scope(),
        )
        .issue(request),
        Err(InformationRefStoreError::PayloadByteLimitExceeded {
            actual: 11,
            maximum: 10
        })
    ));
}
