mod information_provider_contract_support;

use std::sync::{Arc, RwLock};

use information_provider_contract_support::{
    assert_information_provider_contract, read, request, ProviderFixture,
};
use mineintent_middle::information::{
    contracts::{
        InformationAcquisition, InformationAudience, InformationCatalogEntryAvailability,
        InformationInterfaceId, InformationPrecision, InformationProvider,
        InformationProviderLimits, InformationScopeDependency, InformationSourceKind,
    },
    providers::CurrentStatusProvider,
    registry::InformationRegistry,
    source_ports::{SelfExperienceSnapshot, SelfVitalsPort, SelfVitalsSnapshot},
};
use serde_json::json;

struct FakeSelfVitalsPort {
    vitals: RwLock<SelfVitalsSnapshot>,
}

impl FakeSelfVitalsPort {
    fn new(vitals: SelfVitalsSnapshot) -> Self {
        Self {
            vitals: RwLock::new(vitals),
        }
    }

    fn set(&self, vitals: SelfVitalsSnapshot) {
        *self
            .vitals
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = vitals;
    }

    fn snapshot(&self) -> SelfVitalsSnapshot {
        self.vitals
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl SelfVitalsPort for FakeSelfVitalsPort {
    fn current(&self) -> SelfVitalsSnapshot {
        self.snapshot()
    }
}

fn vitals() -> SelfVitalsSnapshot {
    SelfVitalsSnapshot {
        health: 20.0,
        food: 18.0,
        food_saturation: 5.0,
        oxygen: Some(20.0),
        experience: Some(SelfExperienceSnapshot {
            level: 3.0,
            progress: 0.5,
            total: 100.0,
        }),
        effects: Vec::new(),
    }
}

#[tokio::test]
async fn ts_current_status_provider_satisfies_the_provider_contract() {
    let port = Arc::new(FakeSelfVitalsPort::new(vitals()));
    let provider = CurrentStatusProvider::new(port);
    assert_information_provider_contract(
        &provider,
        &ProviderFixture::new(),
        request(&[
            "health",
            "food",
            "foodSaturation",
            "oxygen",
            "experienceLevel",
            "statusEffects",
        ]),
    )
    .await;
}

#[tokio::test]
async fn ts_current_status_defaults_missing_oxygen_to_full_and_reads_experience_level() {
    let port = Arc::new(FakeSelfVitalsPort::new(SelfVitalsSnapshot {
        health: 10.0,
        food: 5.0,
        food_saturation: 0.0,
        oxygen: None,
        experience: None,
        effects: Vec::new(),
    }));
    let provider = CurrentStatusProvider::new(port);
    let result = read(
        &provider,
        &ProviderFixture::new(),
        request(&["oxygen", "experienceLevel"]),
    )
    .await;

    assert_eq!(result.values.len(), 2);
    assert_eq!(result.values["oxygen"].as_f64(), Some(20.0));
    assert_eq!(result.values["experienceLevel"].as_f64(), Some(0.0));
    assert!(result.unavailable.is_empty());
    assert_eq!(result.information_revision, 1);
    assert_eq!(result.source.kind, InformationSourceKind::ClientState);
    assert_eq!(result.source.adapter_revision, "current-status-provider.v1");
    assert_eq!(result.source.source_revision, 1);
    assert_eq!(
        result.source.acquisition,
        InformationAcquisition::ImmediateClientState
    );
    assert_eq!(result.observed_at, "2026-08-01T00:00:01.000Z");
    assert!(result.evidence_ids.is_empty());
}

#[test]
fn ts_current_status_bumps_revision_only_when_vitals_change() {
    let port = Arc::new(FakeSelfVitalsPort::new(SelfVitalsSnapshot {
        health: 20.0,
        food: 20.0,
        food_saturation: 5.0,
        oxygen: None,
        experience: None,
        effects: Vec::new(),
    }));
    let provider = CurrentStatusProvider::new(port.clone());
    let fixture = ProviderFixture::new();
    let first = provider
        .availability(&fixture.context())
        .information_revision;
    let same = provider
        .availability(&fixture.context())
        .information_revision;
    assert_eq!(first, 1);
    assert_eq!(same, first);

    let mut changed = port.snapshot();
    changed.health = 15.0;
    port.set(changed);
    assert_eq!(
        provider
            .availability(&fixture.context())
            .information_revision,
        first + 1
    );
}

#[test]
fn rust_contract_current_status_definition_and_runtime_schemas_match_oracle() {
    let provider = CurrentStatusProvider::new(Arc::new(FakeSelfVitalsPort::new(vitals())));
    let definition = provider.definition();
    assert_eq!(definition.id, InformationInterfaceId::CurrentStatus);
    assert_eq!(
        definition.description,
        "站立不动时可直接得知的自身状态：生命、饥饿、氧气、经验和药水效果"
    );
    assert_eq!(definition.schema_revision, "current-status:1");
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
            max_fields_per_read: 6,
            max_result_bytes: 8_192,
            timeout_ms: 2_000,
        }
    );

    let expected = [
        ("health", "当前生命值", "number"),
        ("food", "当前饥饿值（0-20）", "number"),
        ("foodSaturation", "当前饱和度", "number"),
        ("oxygen", "当前氧气值；不在水下通常为满值", "number"),
        ("experienceLevel", "当前经验等级", "number"),
        ("statusEffects", "当前生效的药水/状态效果", "array"),
    ];
    assert_eq!(definition.fields.len(), expected.len());
    for (id, description, value_type) in expected {
        let field = &definition.fields[id];
        assert_eq!(field.description, description);
        assert_eq!(field.value_type, value_type);
        assert_eq!(field.precision, InformationPrecision::ExactlyDisplayed);
        assert_eq!(field.source_kinds, [InformationSourceKind::ClientState]);
        assert!(field.unit.is_none());
        assert!(field.requires.is_none());
        assert!(field.notes.is_none());
    }

    assert!(definition.fields["health"]
        .value_schema
        .parse(json!(0))
        .is_ok());
    assert!(definition.fields["health"]
        .value_schema
        .parse(json!(-0.01))
        .is_err());
    assert!(definition.fields["food"]
        .value_schema
        .parse(json!(20))
        .is_ok());
    assert!(definition.fields["food"]
        .value_schema
        .parse(json!(21))
        .is_err());
    assert!(definition.fields["experienceLevel"]
        .value_schema
        .parse(json!(3.5))
        .is_err());
    assert!(definition.fields["experienceLevel"]
        .value_schema
        .parse(json!(9_007_199_254_740_992_u64))
        .is_err());
    let parsed_effects = definition.fields["statusEffects"]
        .value_schema
        .parse(json!([{
            "name": "速度",
            "amplifier": 1,
            "durationTicks": 20,
            "unknown": true
        }]))
        .expect("Zod object schema strips unknown keys");
    assert_eq!(
        parsed_effects,
        json!([{"name": "速度", "amplifier": 1, "durationTicks": 20}])
    );
    for invalid in [
        json!([{"name": "", "amplifier": 1}]),
        json!([{"name": "速度", "amplifier": 1.5}]),
        json!([{"name": "速度", "amplifier": 1, "durationTicks": null}]),
    ] {
        assert!(definition.fields["statusEffects"]
            .value_schema
            .parse(invalid)
            .is_err());
    }
}

#[test]
fn rust_contract_current_status_is_registry_usable_thread_safe_and_negative_zero_stable() {
    let port = Arc::new(FakeSelfVitalsPort::new(SelfVitalsSnapshot {
        food_saturation: 0.0,
        ..vitals()
    }));
    let provider = Arc::new(CurrentStatusProvider::new(port.clone()));
    let registry = InformationRegistry::new();
    let object_safe: Arc<dyn InformationProvider> = provider.clone();
    registry
        .register(object_safe)
        .expect("provider should register through the object-safe SPI");

    let availability = provider.availability(&ProviderFixture::new().context());
    assert_eq!(
        availability.overall,
        InformationCatalogEntryAvailability::Available
    );
    assert!(availability.fields.is_empty());
    assert_eq!(availability.information_revision, 1);

    let revisions = (0..8)
        .map(|_| {
            let provider = provider.clone();
            std::thread::spawn(move || {
                provider
                    .availability(&ProviderFixture::new().context())
                    .information_revision
            })
        })
        .map(|thread| thread.join().expect("availability thread should finish"))
        .collect::<Vec<_>>();
    assert_eq!(revisions, vec![1; 8]);

    let mut negative_zero = port.snapshot();
    negative_zero.food_saturation = -0.0;
    port.set(negative_zero);
    assert_eq!(
        provider
            .availability(&ProviderFixture::new().context())
            .information_revision,
        1
    );
}
