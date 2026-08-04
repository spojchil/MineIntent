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
    providers::InventoryProvider,
    registry::InformationRegistry,
    source_ports::{InventoryPort, InventorySlotSnapshot, InventoryStateSnapshot},
};
use serde_json::json;

struct FakeInventoryPort {
    state: RwLock<InventoryStateSnapshot>,
}

impl FakeInventoryPort {
    fn new(state: InventoryStateSnapshot) -> Self {
        Self {
            state: RwLock::new(state),
        }
    }

    fn set(&self, state: InventoryStateSnapshot) {
        *self
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = state;
    }

    fn snapshot(&self) -> InventoryStateSnapshot {
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl InventoryPort for FakeInventoryPort {
    fn current(&self) -> InventoryStateSnapshot {
        self.snapshot()
    }
}

fn inventory() -> InventoryStateSnapshot {
    InventoryStateSnapshot {
        selected_hotbar_slot: 0.0,
        slots: vec![InventorySlotSnapshot {
            slot: 9.0,
            item_name: "oak_log".to_owned(),
            count: 4.0,
            metadata: None,
            durability_used: None,
        }],
    }
}

#[tokio::test]
async fn ts_inventory_provider_satisfies_the_provider_contract() {
    let provider = InventoryProvider::new(Arc::new(FakeInventoryPort::new(inventory())));
    assert_information_provider_contract(
        &provider,
        &ProviderFixture::new(),
        request(&["selectedHotbarSlot", "slots"]),
    )
    .await;
}

#[tokio::test]
async fn ts_inventory_provider_reports_current_slots_and_selected_hotbar_slot() {
    let provider =
        InventoryProvider::new(Arc::new(FakeInventoryPort::new(InventoryStateSnapshot {
            selected_hotbar_slot: 3.0,
            slots: vec![InventorySlotSnapshot {
                slot: 36.0,
                item_name: "stone".to_owned(),
                count: 64.0,
                metadata: None,
                durability_used: None,
            }],
        })));
    let result = read(
        &provider,
        &ProviderFixture::new(),
        request(&["selectedHotbarSlot", "slots"]),
    )
    .await;

    assert_eq!(result.values["selectedHotbarSlot"].as_f64(), Some(3.0));
    let slots = result.values["slots"]
        .as_array()
        .expect("slots should be an array");
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0]["slot"].as_f64(), Some(36.0));
    assert_eq!(slots[0]["itemName"], json!("stone"));
    assert_eq!(slots[0]["count"].as_f64(), Some(64.0));
    assert!(slots[0].get("metadata").is_none());
    assert!(slots[0].get("durabilityUsed").is_none());
    assert_eq!(result.values.len(), 2);
    assert!(result.unavailable.is_empty());
    assert_eq!(result.information_revision, 1);
    assert_eq!(result.source.kind, InformationSourceKind::ClientState);
    assert_eq!(result.source.adapter_revision, "inventory-provider.v1");
    assert_eq!(result.source.source_revision, 1);
    assert_eq!(
        result.source.acquisition,
        InformationAcquisition::ImmediateClientState
    );
    assert_eq!(result.observed_at, "2026-08-01T00:00:01.000Z");
    assert!(result.evidence_ids.is_empty());
}

#[test]
fn rust_contract_inventory_definition_and_runtime_schemas_match_oracle() {
    let provider = InventoryProvider::new(Arc::new(FakeInventoryPort::new(inventory())));
    let definition = provider.definition();
    assert_eq!(definition.id, InformationInterfaceId::InventoryInformation);
    assert_eq!(
        definition.description,
        "站立不动时可直接得知的背包内容与当前选中快捷栏槽"
    );
    assert_eq!(definition.schema_revision, "inventory-information:1");
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
            max_fields_per_read: 2,
            max_result_bytes: 16_384,
            timeout_ms: 2_000,
        }
    );

    let selected = &definition.fields["selectedHotbarSlot"];
    assert_eq!(selected.description, "当前选中的快捷栏槽位（0-8）");
    assert_eq!(selected.value_type, "number");
    assert_eq!(selected.precision, InformationPrecision::ExactlyDisplayed);
    assert_eq!(selected.source_kinds, [InformationSourceKind::ClientState]);
    assert!(selected.value_schema.parse(json!(0)).is_ok());
    assert!(selected.value_schema.parse(json!(8)).is_ok());
    assert!(selected.value_schema.parse(json!(9)).is_err());
    assert!(selected.value_schema.parse(json!(1.5)).is_err());

    let slots = &definition.fields["slots"];
    assert_eq!(slots.description, "背包中所有非空槽位");
    assert_eq!(slots.value_type, "array");
    assert_eq!(slots.precision, InformationPrecision::ExactlyDisplayed);
    assert_eq!(slots.source_kinds, [InformationSourceKind::ClientState]);
    let parsed = slots
        .value_schema
        .parse(json!([{
            "slot": 36,
            "itemName": "stone",
            "count": 64,
            "metadata": -1,
            "durabilityUsed": 2,
            "unknown": true
        }]))
        .expect("Zod object schema strips unknown keys");
    assert_eq!(
        parsed,
        json!([{
            "slot": 36,
            "itemName": "stone",
            "count": 64,
            "metadata": -1,
            "durabilityUsed": 2
        }])
    );
    for invalid in [
        json!([{"slot": -1, "itemName": "stone", "count": 1}]),
        json!([{"slot": 0, "itemName": "", "count": 1}]),
        json!([{"slot": 0, "itemName": "stone", "count": 0}]),
        json!([{"slot": 0, "itemName": "stone", "count": 1, "metadata": null}]),
    ] {
        assert!(slots.value_schema.parse(invalid).is_err());
    }
}

#[tokio::test]
async fn rust_contract_inventory_is_registry_usable_thread_safe_requested_only_and_negative_zero_stable(
) {
    let port = Arc::new(FakeInventoryPort::new(inventory()));
    let provider = Arc::new(InventoryProvider::new(port.clone()));
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
    negative_zero.selected_hotbar_slot = -0.0;
    port.set(negative_zero);
    assert_eq!(
        provider
            .availability(&ProviderFixture::new().context())
            .information_revision,
        1
    );

    let only_slots = read(
        provider.as_ref(),
        &ProviderFixture::new(),
        request(&["slots"]),
    )
    .await;
    assert_eq!(only_slots.values.keys().collect::<Vec<_>>(), ["slots"]);

    let mut changed = port.snapshot();
    changed.slots[0].count = 5.0;
    port.set(changed);
    assert_eq!(
        provider
            .availability(&ProviderFixture::new().context())
            .information_revision,
        2
    );
    assert_eq!(
        provider
            .availability(&ProviderFixture::new().context())
            .information_revision,
        2
    );
}
