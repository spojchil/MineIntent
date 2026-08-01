use mineintent_contracts::{information::*, minecraft::*};
use serde_json::{json, Value};

fn fixture() -> InformationFixtureSet {
    serde_json::from_str(include_str!("../testdata/i02/information_facade.json"))
        .expect("versioned I02 fixture must deserialize")
}

#[test]
fn information_fixture_matches_deterministic_builder() {
    assert_eq!(fixture(), fixture_information_set());
    assert_eq!(fixture(), InformationFixtureBuilder::canonical().build());
}

#[test]
fn backend_ports_read_snapshot_straight_through() {
    let source = fixture_snapshot();
    let values = fixture().plain_values;
    assert_eq!(
        values.current_status.health,
        Some(source.self_snapshot.health)
    );
    assert_eq!(values.current_status.food, Some(source.self_snapshot.food));
    assert_eq!(
        values.inventory.selected_hotbar_slot,
        Some(source.inventory.selected_hotbar_slot)
    );
    assert_eq!(values.inventory.slots, Some(source.inventory.slots));
}

#[test]
fn perception_excludes_self_and_maps_block_loading() {
    let values = fixture().plain_values.viewport;
    assert_eq!(values.standing_on_block.unwrap().name, "stone");
    assert_eq!(values.visible_entities.items.len(), 1);
    assert_eq!(
        values.visible_entities.items[0].player.as_deref(),
        Some("Observer")
    );
    assert_ne!(
        values.visible_entities.items[0].player.as_deref(),
        Some("MineFixture")
    );
}

#[test]
fn sound_history_is_relative_bounded_and_revisioned() {
    let sounds = fixture()
        .plain_values
        .sound
        .recent_sounds
        .expect("sound fixture");
    assert_eq!(sounds.len(), 1);
    assert_eq!(sounds[0].distance, 4.5);
    assert_eq!(sounds[0].direction, RelativeDirection::Ahead);
    let encoded = serde_json::to_value(sounds).unwrap();
    assert!(!contains_key(&encoded, "informationRevision"));
    assert!(!contains_key(&encoded, "providerRevision"));
}

#[test]
fn scope_maps_backend_state_and_ready_world() {
    let scope = fixture().scope;
    assert_eq!(scope.connection_state, InformationConnectionState::Play);
    assert_eq!(scope.connection_epoch, 1);
    assert_eq!(scope.world_id.as_deref(), Some("world-fixture"));
    assert_eq!(scope.dimension.as_deref(), Some("minecraft:overworld"));
}

#[test]
fn compose_passive_observations_has_fixed_four_interface_shape() {
    let composed = fixture().composed;
    assert!(composed.current_status.is_some());
    assert!(composed.inventory.is_some());
    assert!(composed.sound.is_some());
    let passive_viewport = composed.viewport.expect("passive viewport");
    assert!(passive_viewport.frame.is_some());
    assert!(composed.omissions.is_empty());
}

#[test]
fn viewport_provider_satisfies_five_field_contract() {
    let projection = fixture().plain_values.viewport;
    assert_eq!(
        projection.frame.coordinates,
        ViewportCoordinateSystem::MinecraftWorldAbsolute
    );
    assert!(projection.standing_on_block.is_some());
    assert!(projection.looked_at_block.is_some());
    assert!(!projection.visible_entities.items.is_empty());
    assert!(!projection.visible_blocks.blocks.is_empty());
}

#[test]
fn atomic_viewport_read_keeps_projection_source_and_revision_together() {
    let fixtures = fixture();
    assert_eq!(
        fixtures.viewport_read.projection,
        fixtures.plain_values.viewport
    );
    assert_eq!(fixtures.viewport_read.source, FactSource::ServerObserved);
    assert_eq!(fixtures.viewport_read.revision, 9);
}

#[test]
fn denied_fixture_is_an_explicit_omission() {
    let denied = fixture().denied;
    assert!(denied.current_status.is_none());
    assert_eq!(denied.omissions.len(), 1);
    assert_eq!(
        denied.omissions[0].reason,
        InformationOmissionReason::AudienceDenied
    );
}

#[test]
fn unavailable_fixture_keeps_field_reason() {
    let unavailable = fixture().unavailable;
    assert_eq!(
        unavailable.omissions[0].fields[0].reason,
        InformationUnavailableReason::NotConnected
    );
}

#[test]
fn partial_fixture_preserves_values_without_guessing_missing_facts() {
    let partial = fixture().partial;
    let status = partial.current_status.expect("partial values");
    assert_eq!(status.health, Some(18.0));
    assert_eq!(status.oxygen, None);
    assert_eq!(status.experience_level, None);
    assert_eq!(
        partial.omissions[0].reason,
        InformationOmissionReason::Partial
    );
    assert_eq!(partial.omissions[0].fields.len(), 2);
}

#[test]
fn timeout_fixture_is_an_omission_not_a_fabricated_viewport() {
    let timeout = fixture().timeout;
    assert!(timeout.viewport.is_none());
    assert_eq!(
        timeout.omissions[0].reason,
        InformationOmissionReason::DeadlineExceeded
    );
}

#[test]
fn scope_snapshot_is_owned_and_strict() {
    let original = fixture().scope;
    let mut detached = original.clone();
    detached.connection_epoch = 99;
    assert_eq!(original.connection_epoch, 1);

    let mut value = serde_json::to_value(original).unwrap();
    value["unexpected"] = json!(true);
    assert!(serde_json::from_value::<InformationScopeSnapshot>(value).is_err());
}

#[test]
fn facade_wire_does_not_expose_catalog_refs_cursors_or_provider_revision() {
    let value = serde_json::to_value(fixture()).unwrap();
    for forbidden in [
        "catalogRevision",
        "schemaRevision",
        "informationRevision",
        "providerRevision",
        "adapterRevision",
        "cursor",
        "nextCursor",
        "reference",
    ] {
        assert!(!contains_key(&value, forbidden), "leaked key: {forbidden}");
    }
    assert!(contains_key(&value, "revision"));
}

#[test]
fn memory_is_not_part_of_i02_contract() {
    let value = serde_json::to_value(fixture()).unwrap();
    assert!(!contains_key(&value, "memory"));
    assert!(!contains_key(&value, "memories"));
}

#[test]
fn information_error_is_structured_and_strict() {
    let error = InformationError::ScopeChanged {
        before_epoch: 1,
        after_epoch: 2,
    };
    assert_eq!(
        serde_json::to_value(&error).unwrap(),
        json!({"code":"scope_changed","beforeEpoch":1,"afterEpoch":2})
    );
    let invalid = json!({"code":"scope_changed","beforeEpoch":1,"afterEpoch":2,"extra":true});
    assert!(serde_json::from_value::<InformationError>(invalid).is_err());
}

#[test]
fn information_facade_is_object_safe() {
    let _: Option<&dyn InformationFacade> = None;
}

fn contains_key(value: &Value, expected: &str) -> bool {
    match value {
        Value::Object(object) => {
            object.contains_key(expected)
                || object.values().any(|value| contains_key(value, expected))
        }
        Value::Array(values) => values.iter().any(|value| contains_key(value, expected)),
        _ => false,
    }
}
