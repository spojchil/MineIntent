use std::sync::{
    atomic::{AtomicI64, Ordering},
    Arc,
};

use mineintent_middle::information::{
    contracts::{
        InformationAllInterfaces, InformationAllowedInterfaces, InformationAudience,
        InformationConnectionState, InformationGrant, InformationGrantPurpose,
        InformationInterfaceId, InformationInvalidationEvent, InformationScopeSnapshot,
        InformationSelectorRef, InformationSelectorRefProtocol,
    },
    cursor_store::{
        InformationCursorIssueInput, InformationCursorResolution, InformationCursorResolveInput,
        InformationCursorStore, InformationCursorStoreError, InformationCursorStoreOptions,
        DEFAULT_CURSOR_TTL_MS, DEFAULT_MAX_CURSOR_ENTRIES,
        DEFAULT_MAX_CURSOR_ENTRIES_PER_INTERFACE, DEFAULT_MAX_CURSOR_ENTRIES_PER_PRINCIPAL,
        DEFAULT_MAX_CURSOR_PAGE_STATE_BYTES,
    },
    ref_store::InformationRefClock,
};
use serde_json::{json, Value};
use uuid::Uuid;

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

fn selector(id: &str) -> InformationSelectorRef {
    InformationSelectorRef {
        protocol: InformationSelectorRefProtocol::V1,
        id: id.to_owned(),
        interface_id: InformationInterfaceId::InventoryInformation,
        connection_epoch: 4,
        world_id: Some("world-1".to_owned()),
        screen_instance_id: Some("screen-1".to_owned()),
        based_on_information_revision: 7,
        valid_until: None,
    }
}

fn issue_input(page_state: Value) -> InformationCursorIssueInput {
    InformationCursorIssueInput {
        interface_id: InformationInterfaceId::InventoryInformation,
        fields: vec!["slots".to_owned()],
        selector: None,
        information_revision: 9,
        limit: 20,
        page_state,
        principal_id: "model-1".to_owned(),
        grant: grant(),
        scope: scope(),
    }
}

fn resolve_input(cursor: String) -> InformationCursorResolveInput {
    InformationCursorResolveInput {
        cursor,
        interface_id: InformationInterfaceId::InventoryInformation,
        fields: vec!["slots".to_owned()],
        selector: None,
        limit: 20,
        principal_id: "model-1".to_owned(),
        grant: grant(),
        scope: scope(),
    }
}

fn options(clock: Arc<dyn InformationRefClock>) -> InformationCursorStoreOptions {
    InformationCursorStoreOptions {
        clock,
        ..InformationCursorStoreOptions::default()
    }
}

#[test]
fn cursors_bind_query_shape_and_are_one_time_continuations() {
    let clock = Arc::new(FixedClock::new(FIXED_NOW_MS));
    let store = InformationCursorStore::new(options(clock)).expect("valid cursor options");
    let cursor = store
        .issue(issue_input(json!({"offset": 20})))
        .expect("cursor should issue");
    let uuid = cursor
        .strip_prefix("icur_")
        .and_then(|value| Uuid::parse_str(value).ok())
        .expect("cursor must be an opaque icur_ UUID");
    assert_eq!(uuid.get_version_num(), 4);

    let mut wrong_fields = resolve_input(cursor.clone());
    wrong_fields.fields.push("selected".to_owned());
    assert_eq!(store.resolve(wrong_fields), Ok(None));
    assert_eq!(store.size(), Ok(1), "invalid resolve must not consume");

    assert_eq!(
        store.resolve(resolve_input(cursor.clone())),
        Ok(Some(InformationCursorResolution {
            state: json!({"offset": 20}),
            information_revision: 9,
        }))
    );
    assert_eq!(store.resolve(resolve_input(cursor)), Ok(None));
}

#[test]
fn cursor_state_and_per_principal_capacity_are_bounded() {
    let limited = InformationCursorStore::new(InformationCursorStoreOptions {
        max_entries: 4,
        max_entries_per_principal: 1,
        max_entries_per_interface: 2,
        max_page_state_bytes: 32,
        ..InformationCursorStoreOptions::default()
    })
    .expect("valid capacity options");
    limited
        .issue(issue_input(json!({"offset": 10})))
        .expect("first principal cursor should issue");
    let mut same_principal_other_interface = issue_input(json!({"offset": 20}));
    same_principal_other_interface.interface_id = InformationInterfaceId::HotbarInformation;
    assert_eq!(
        limited.issue(same_principal_other_interface),
        Err(InformationCursorStoreError::CapacityExceeded)
    );

    let state_limited = InformationCursorStore::new(InformationCursorStoreOptions {
        max_page_state_bytes: 8,
        ..InformationCursorStoreOptions::default()
    })
    .expect("valid byte options");
    assert!(matches!(
        state_limited.issue(issue_input(json!({"opaque": "far too large"}))),
        Err(InformationCursorStoreError::PageStateByteLimitExceeded { .. })
    ));
}

#[test]
fn contract_defaults_metadata_ttl_cleanup_and_size_match_oracle() {
    assert_eq!(DEFAULT_MAX_CURSOR_ENTRIES, 2_048);
    assert_eq!(DEFAULT_MAX_CURSOR_ENTRIES_PER_PRINCIPAL, 512);
    assert_eq!(DEFAULT_MAX_CURSOR_ENTRIES_PER_INTERFACE, 256);
    assert_eq!(DEFAULT_MAX_CURSOR_PAGE_STATE_BYTES, 8_192);
    assert_eq!(DEFAULT_CURSOR_TTL_MS, 60_000);
    assert!(matches!(
        InformationCursorStore::new(InformationCursorStoreOptions {
            ttl_ms: 0,
            ..InformationCursorStoreOptions::default()
        }),
        Err(InformationCursorStoreError::InvalidLimits)
    ));

    let clock = Arc::new(FixedClock::new(FIXED_NOW_MS));
    let store = InformationCursorStore::new(InformationCursorStoreOptions {
        max_entries: 1,
        ttl_ms: 1_000,
        clock: clock.clone(),
        ..InformationCursorStoreOptions::default()
    })
    .expect("valid ttl options");
    let mut invalid = issue_input(json!({"offset": 0}));
    invalid.limit = 0;
    assert_eq!(
        store.issue(invalid),
        Err(InformationCursorStoreError::InvalidMetadata)
    );
    let expired = store
        .issue(issue_input(json!({"offset": 1})))
        .expect("cursor should issue");
    assert_eq!(store.size(), Ok(1));
    clock.set(FIXED_NOW_MS + 1_000);
    assert_eq!(store.size(), Ok(1), "size must not perform TTL cleanup");
    let replacement = store
        .issue(issue_input(json!({"offset": 2})))
        .expect("expired cleanup should restore capacity");
    assert_eq!(store.size(), Ok(1));
    assert_eq!(store.resolve(resolve_input(expired)), Ok(None));
    assert!(store
        .resolve(resolve_input(replacement))
        .expect("replacement resolve should work")
        .is_some());
}

#[test]
fn contract_global_and_per_interface_capacities_are_independent() {
    let per_interface = InformationCursorStore::new(InformationCursorStoreOptions {
        max_entries: 3,
        max_entries_per_principal: 3,
        max_entries_per_interface: 1,
        ..InformationCursorStoreOptions::default()
    })
    .expect("valid interface options");
    per_interface
        .issue(issue_input(json!({"offset": 1})))
        .expect("first interface cursor should issue");
    let mut other_principal = issue_input(json!({"offset": 2}));
    other_principal.principal_id = "model-2".to_owned();
    assert_eq!(
        per_interface.issue(other_principal),
        Err(InformationCursorStoreError::CapacityExceeded)
    );

    let global = InformationCursorStore::new(InformationCursorStoreOptions {
        max_entries: 1,
        max_entries_per_principal: 2,
        max_entries_per_interface: 2,
        ..InformationCursorStoreOptions::default()
    })
    .expect("valid global options");
    global
        .issue(issue_input(json!({"offset": 1})))
        .expect("first global cursor should issue");
    let mut second = issue_input(json!({"offset": 2}));
    second.interface_id = InformationInterfaceId::HotbarInformation;
    second.principal_id = "model-2".to_owned();
    assert_eq!(
        global.issue(second),
        Err(InformationCursorStoreError::CapacityExceeded)
    );
}

#[test]
fn contract_resolve_binds_every_query_field_but_only_the_selector_id() {
    let store = InformationCursorStore::default();
    let mut issued = issue_input(json!({"offset": 20}));
    issued.fields = vec!["slots".to_owned(), "selected".to_owned()];
    issued.selector = Some(selector("iref-selector"));
    let cursor = store.issue(issued).expect("cursor should issue");
    let valid = {
        let mut input = resolve_input(cursor.clone());
        input.fields = vec!["slots".to_owned(), "selected".to_owned()];
        let mut same_id = selector("iref-selector");
        same_id.interface_id = InformationInterfaceId::CurrentStatus;
        same_id.connection_epoch = 99;
        same_id.based_on_information_revision = 999;
        input.selector = Some(same_id);
        input
    };

    let mut mismatches = Vec::new();
    let mut input = valid.clone();
    input.fields.reverse();
    mismatches.push(input);
    let mut input = valid.clone();
    input.interface_id = InformationInterfaceId::HotbarInformation;
    mismatches.push(input);
    let mut input = valid.clone();
    input.principal_id = "other-model".to_owned();
    mismatches.push(input);
    let mut input = valid.clone();
    input.grant.id = "other-grant".to_owned();
    mismatches.push(input);
    let mut input = valid.clone();
    input.grant.audience = InformationAudience::Controller;
    mismatches.push(input);
    let mut input = valid.clone();
    input.limit = 21;
    mismatches.push(input);
    let mut input = valid.clone();
    input.selector = Some(selector("other-selector"));
    mismatches.push(input);
    let mut input = valid.clone();
    input.scope.connection_epoch = 5;
    mismatches.push(input);
    let mut input = valid.clone();
    input.scope.world_id = Some("world-2".to_owned());
    mismatches.push(input);
    let mut input = valid.clone();
    input.scope.dimension = Some("minecraft:the_nether".to_owned());
    mismatches.push(input);
    let mut input = valid.clone();
    input.scope.screen_instance_id = Some("screen-2".to_owned());
    mismatches.push(input);
    let mut input = valid.clone();
    input.scope.screen_revision = Some(4);
    mismatches.push(input);

    for mismatch in mismatches {
        assert_eq!(store.resolve(mismatch), Ok(None));
        assert_eq!(store.size(), Ok(1), "mismatch must preserve the cursor");
    }
    assert!(store.resolve(valid).expect("resolve should work").is_some());
}

#[test]
fn contract_all_invalidations_match_cursor_specific_oracle_semantics() {
    fn populated(input: InformationCursorIssueInput) -> InformationCursorStore {
        let store = InformationCursorStore::default();
        store.issue(input).expect("cursor should issue");
        store
    }

    let store = populated(issue_input(json!({"offset": 1})));
    store
        .invalidate(&InformationInvalidationEvent::GrantEnded {
            grant_id: "grant-1".to_owned(),
        })
        .expect("grant invalidation should work");
    assert_eq!(store.size(), Ok(0));

    let store = populated(issue_input(json!({"offset": 2})));
    store
        .invalidate(&InformationInvalidationEvent::ConnectionChanged {
            connection_epoch: 5,
        })
        .expect("connection invalidation should work");
    assert_eq!(store.size(), Ok(0));

    let store = populated(issue_input(json!({"offset": 3})));
    store
        .invalidate(&InformationInvalidationEvent::WorldChanged {
            world_id: Some("world-1".to_owned()),
            dimension: Some("minecraft:the_nether".to_owned()),
        })
        .expect("world invalidation should work");
    assert_eq!(store.size(), Ok(0));

    let mut no_screen = issue_input(json!({"offset": 4}));
    no_screen.scope.screen_instance_id = None;
    no_screen.scope.screen_revision = None;
    let store = populated(no_screen);
    store
        .invalidate(&InformationInvalidationEvent::ScreenChanged {
            screen_instance_id: Some("screen-1".to_owned()),
            screen_revision: Some(3),
        })
        .expect("screen invalidation should work");
    assert_eq!(
        store.size(),
        Ok(0),
        "TS compares screen fields for even an unbound cursor"
    );

    let store = populated(issue_input(json!({"offset": 5})));
    store
        .invalidate(&InformationInvalidationEvent::ScreenChanged {
            screen_instance_id: Some("screen-1".to_owned()),
            screen_revision: Some(3),
        })
        .expect("matching screen should be retained");
    assert_eq!(store.size(), Ok(1));
}

#[test]
fn contract_page_state_limit_counts_utf8_json_bytes() {
    let value = json!({"x": "界"});
    assert_eq!(
        serde_json::to_vec(&value)
            .expect("JSON value serializes")
            .len(),
        11
    );
    let rejected = InformationCursorStore::new(InformationCursorStoreOptions {
        max_page_state_bytes: 10,
        ..InformationCursorStoreOptions::default()
    })
    .expect("valid byte options");
    assert_eq!(
        rejected.issue(issue_input(value.clone())),
        Err(InformationCursorStoreError::PageStateByteLimitExceeded {
            actual: 11,
            maximum: 10,
        })
    );
    let accepted = InformationCursorStore::new(InformationCursorStoreOptions {
        max_page_state_bytes: 11,
        ..InformationCursorStoreOptions::default()
    })
    .expect("valid byte options");
    let cursor = accepted
        .issue(issue_input(value.clone()))
        .expect("exact byte boundary should pass");
    assert_eq!(
        accepted
            .resolve(resolve_input(cursor))
            .expect("resolve should work")
            .map(|resolved| resolved.state),
        Some(value)
    );
}
