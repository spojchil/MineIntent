mod information_provider_contract_support;

use std::sync::{Arc, Mutex};

use information_provider_contract_support::{
    assert_information_provider_contract, read, request, ProviderFixture,
};
use mineintent_middle::information::{
    contracts::{
        InformationAcquisition, InformationAudience, InformationCatalogEntryAvailability,
        InformationInterfaceId, InformationPrecision, InformationProvider,
        InformationProviderError, InformationProviderLimits, InformationScopeDependency,
        InformationSourceKind,
    },
    providers::SoundInformationProvider,
    registry::InformationRegistry,
    source_ports::{SoundHistoryPort, SoundObservation},
};
use serde_json::json;

struct FakeSoundHistoryPort {
    entries: Mutex<Vec<SoundObservation>>,
    revision: f64,
    last_limit: Mutex<Option<f64>>,
}

impl FakeSoundHistoryPort {
    fn new(entries: Vec<SoundObservation>) -> Self {
        let revision = entries.len() as f64;
        Self {
            entries: Mutex::new(entries),
            revision,
            last_limit: Mutex::new(None),
        }
    }

    fn with_revision(entries: Vec<SoundObservation>, revision: f64) -> Self {
        Self {
            entries: Mutex::new(entries),
            revision,
            last_limit: Mutex::new(None),
        }
    }

    fn last_limit(&self) -> Option<f64> {
        *self
            .last_limit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl SoundHistoryPort for FakeSoundHistoryPort {
    fn recent(&self, limit: f64) -> Vec<SoundObservation> {
        *self
            .last_limit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(limit);
        let limit = limit as usize;
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .take(limit)
            .cloned()
            .collect()
    }

    fn revision(&self) -> f64 {
        self.revision
    }
}

fn sound(observed_at: &str) -> SoundObservation {
    SoundObservation {
        sound_name: Some("entity.cow.ambient".to_owned()),
        category: Some("neutral".to_owned()),
        distance: 4.0,
        direction: mineintent_middle::information::geometry::RelativeDirection::Ahead,
        volume: 1.0,
        pitch: 1.0,
        observed_at: observed_at.to_owned(),
    }
}

#[tokio::test]
async fn ts_sound_provider_satisfies_the_provider_contract() {
    let provider = SoundInformationProvider::new(Arc::new(FakeSoundHistoryPort::new(vec![sound(
        "2026-08-01T00:00:00.000Z",
    )])));
    assert_information_provider_contract(
        &provider,
        &ProviderFixture::new(),
        request(&["recentSounds"]),
    )
    .await;
}

#[tokio::test]
async fn ts_sound_provider_returns_an_empty_list_not_unavailable_when_nothing_was_heard() {
    let provider = SoundInformationProvider::new(Arc::new(FakeSoundHistoryPort::new(Vec::new())));
    let result = read(
        &provider,
        &ProviderFixture::new(),
        request(&["recentSounds"]),
    )
    .await;
    assert_eq!(result.values["recentSounds"], json!([]));
    assert!(result.unavailable.is_empty());
    assert_eq!(result.information_revision, 0);
}

#[test]
fn rust_contract_sound_definition_and_runtime_schema_match_oracle() {
    let provider = SoundInformationProvider::new(Arc::new(FakeSoundHistoryPort::new(Vec::new())));
    let definition = provider.definition();
    assert_eq!(definition.id, InformationInterfaceId::SoundInformation);
    assert_eq!(
        definition.description,
        "站立不动时能听到的最近声音，含相对距离和方向"
    );
    assert_eq!(definition.schema_revision, "sound-information:1");
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
            max_fields_per_read: 1,
            max_result_bytes: 16_384,
            timeout_ms: 2_000,
        }
    );
    let field = &definition.fields["recentSounds"];
    assert_eq!(field.description, "最近听到的声音，按时间从新到旧排列");
    assert_eq!(field.value_type, "array");
    assert_eq!(field.precision, InformationPrecision::Quantized);
    assert_eq!(field.source_kinds, [InformationSourceKind::SoundProjection]);
    assert_eq!(
        field.notes.as_deref(),
        Some("距离和方向是从协议声音包位置换算得到的近似值，不是精确音源坐标")
    );

    let parsed = field
        .value_schema
        .parse(json!([{
            "soundName": "entity.cow.ambient",
            "category": "neutral",
            "distance": 4,
            "direction": "ahead",
            "volume": 1,
            "pitch": 1,
            "observedAt": "2026-08-01T00:00:00.000Z",
            "unknown": true
        }]))
        .expect("valid sound schema should parse");
    assert_eq!(
        parsed,
        json!([{
            "soundName": "entity.cow.ambient",
            "category": "neutral",
            "distance": 4,
            "direction": "ahead",
            "volume": 1,
            "pitch": 1,
            "observedAt": "2026-08-01T00:00:00.000Z"
        }])
    );
    for invalid in [
        json!([{"distance": -1, "direction": "ahead", "volume": 1, "pitch": 1, "observedAt": "2026-08-01T00:00:00Z"}]),
        json!([{"distance": 1, "direction": "up", "volume": 1, "pitch": 1, "observedAt": "2026-08-01T00:00:00Z"}]),
        json!([{"distance": 1, "direction": "ahead", "volume": 1, "pitch": 1, "observedAt": "2026-02-30T00:00:00Z"}]),
        json!([{"soundName": null, "distance": 1, "direction": "ahead", "volume": 1, "pitch": 1, "observedAt": "2026-08-01T00:00:00Z"}]),
    ] {
        assert!(field.value_schema.parse(invalid).is_err());
    }
}

#[tokio::test]
async fn rust_contract_sound_is_object_safe_limited_and_preserves_wire_metadata() {
    let entries = (0..25)
        .map(|index| sound(&format!("2026-08-01T00:00:{index:02}.000Z")))
        .collect::<Vec<_>>();
    let port = Arc::new(FakeSoundHistoryPort::with_revision(entries, 25.0));
    let provider = Arc::new(SoundInformationProvider::new(port.clone()));
    let object_safe: Arc<dyn InformationProvider> = provider.clone();
    let registry = InformationRegistry::new();
    registry
        .register(object_safe)
        .expect("sound provider should register through object-safe SPI");

    let fixture = ProviderFixture::new();
    let availability = provider.availability(&fixture.context());
    assert_eq!(
        availability.overall,
        InformationCatalogEntryAvailability::Available
    );
    assert!(availability.fields.is_empty());
    assert_eq!(availability.information_revision, 25);

    let result = read(&*provider, &fixture, request(&["recentSounds"])).await;
    assert_eq!(result.information_revision, 25);
    assert_eq!(result.source.kind, InformationSourceKind::SoundProjection);
    assert_eq!(result.source.adapter_revision, "sound-provider.v1");
    assert_eq!(result.source.source_revision, 25);
    assert_eq!(
        result.source.acquisition,
        InformationAcquisition::ImmediateClientState
    );
    assert_eq!(
        result.values["recentSounds"].as_array().map(Vec::len),
        Some(20)
    );
    assert_eq!(port.last_limit(), Some(20.0));
    assert!(result.unavailable.is_empty());
    assert_eq!(result.observed_at, "2026-08-01T00:00:01.000Z");
    assert!(result.evidence_ids.is_empty());
}

#[tokio::test]
async fn rust_contract_sound_invalid_revision_is_structured_not_a_panic() {
    for revision in [
        -1.0,
        1.5,
        9_007_199_254_740_992.0,
        u64::MAX as f64,
        f64::NAN,
        f64::INFINITY,
    ] {
        let provider = SoundInformationProvider::new(Arc::new(
            FakeSoundHistoryPort::with_revision(Vec::new(), revision),
        ));
        assert_eq!(
            provider
                .availability(&ProviderFixture::new().context())
                .information_revision,
            0
        );
        let result = provider
            .read(
                ProviderFixture::new().context(),
                request(&["recentSounds"]),
                information_provider_contract_support::operation_control(),
            )
            .await;
        assert!(matches!(
            result,
            Err(InformationProviderError::Failed { .. })
        ));
    }
}
