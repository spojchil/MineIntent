use mineintent_middle::information::{
    geometry::Point3,
    source_ports::{
        InventoryPort, InventoryStateSnapshot, PerceptionBlockAt, PerceptionPort,
        PerceptionUnloaded, SelfVitalsPort, SelfVitalsSnapshot, SoundHistoryPort, SoundObservation,
        VisibleBlocksOptions,
    },
};
use serde_json::json;

#[test]
fn rust_contract_source_port_traits_are_object_safe() {
    fn accepts_trait_objects(
        _vitals: Option<&dyn SelfVitalsPort>,
        _inventory: Option<&dyn InventoryPort>,
        _sound: Option<&dyn SoundHistoryPort>,
        _perception: Option<&dyn PerceptionPort>,
    ) {
    }

    accepts_trait_objects(None, None, None, None);
}

#[test]
fn rust_contract_source_port_dtos_are_strict_and_preserve_unicode() {
    let vitals: SelfVitalsSnapshot = serde_json::from_value(json!({
        "health": 19.5,
        "food": 20,
        "foodSaturation": 4.25,
        "experience": {"level": 3, "progress": 0.5, "total": 21},
        "effects": [{"name": "速度😀", "amplifier": 1}]
    }))
    .expect("strict vitals DTO should parse");
    assert_eq!(vitals.effects[0].name, "速度😀");

    assert!(serde_json::from_value::<SelfVitalsSnapshot>(json!({
        "health": 20,
        "food": 20,
        "foodSaturation": 5,
        "effects": [],
        "extra": true
    }))
    .is_err());
    assert!(serde_json::from_value::<InventoryStateSnapshot>(json!({
        "selectedHotbarSlot": 0,
        "slots": [{"slot": 36, "itemName": "stone", "count": 1, "extra": 0}]
    }))
    .is_err());
    assert!(serde_json::from_value::<SoundObservation>(json!({
        "distance": 1,
        "direction": "ahead",
        "volume": 1,
        "pitch": 1,
        "observedAt": "2026-08-01T00:00:00Z",
        "extra": false
    }))
    .is_err());
}

#[test]
fn rust_contract_optional_fields_reject_explicit_null() {
    assert!(serde_json::from_value::<SelfVitalsSnapshot>(json!({
        "health": 20,
        "food": 20,
        "foodSaturation": 5,
        "oxygen": null,
        "effects": []
    }))
    .is_err());
    assert!(serde_json::from_value::<InventoryStateSnapshot>(json!({
        "selectedHotbarSlot": 0,
        "slots": [{"slot": 36, "itemName": "stone", "count": 1, "metadata": null}]
    }))
    .is_err());
    assert!(serde_json::from_value::<SoundObservation>(json!({
        "soundName": null,
        "distance": 1,
        "direction": "ahead",
        "volume": 1,
        "pitch": 1,
        "observedAt": "2026-08-01T00:00:00Z"
    }))
    .is_err());
    assert!(serde_json::from_value::<VisibleBlocksOptions>(json!({
        "horizontalRadius": 8,
        "verticalRadius": 4,
        "maxDistance": 12,
        "frustum": {"verticalHalfAngle": 0.6, "horizontalHalfAngle": 0.9},
        "limit": 32,
        "predicate": null
    }))
    .is_err());
}

#[test]
fn rust_contract_numeric_dtos_reject_non_finite_serialization() {
    assert!(serde_json::to_string(&Point3 {
        x: f64::NAN,
        y: 0.0,
        z: 0.0,
    })
    .is_err());
    assert!(serde_json::to_string(&SelfVitalsSnapshot {
        health: f64::INFINITY,
        food: 20.0,
        food_saturation: 5.0,
        oxygen: None,
        experience: None,
        effects: Vec::new(),
    })
    .is_err());
}

#[test]
fn rust_contract_perception_unloaded_is_an_explicit_closed_enum() {
    let unloaded = serde_json::from_str::<PerceptionBlockAt>(r#""unloaded""#)
        .expect("unloaded marker should parse");
    assert_eq!(
        unloaded,
        PerceptionBlockAt::Unloaded(PerceptionUnloaded::Unloaded)
    );
    assert!(serde_json::from_str::<PerceptionBlockAt>(r#""unknown""#).is_err());
    assert!(serde_json::from_value::<PerceptionBlockAt>(json!({
        "name": "glass",
        "visible": true,
        "occludes": false,
        "extra": true
    }))
    .is_err());
}
