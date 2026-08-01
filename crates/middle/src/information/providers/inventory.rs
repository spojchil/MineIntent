use std::{collections::BTreeMap, sync::Arc};

use mineintent_contracts::minecraft::{BoxFuture, OperationControl};
use serde_json::{Map, Value};

use crate::information::{
    contracts::{
        InformationAcquisition, InformationAudience, InformationCatalogEntryAvailability,
        InformationFieldDefinition, InformationInterfaceId, InformationPrecision,
        InformationProvider, InformationProviderContext, InformationProviderDefinition,
        InformationProviderError, InformationProviderLimits, InformationReadSource,
        InformationScopeDependency, InformationSourceKind, InformationValueSchema,
        InformationValueSchemaError, ProviderAvailability, ProviderReadRequest, ProviderReadResult,
    },
    source_ports::{InventoryPort, InventorySlotSnapshot, InventoryStateSnapshot},
};

use super::{
    schema::{error, parse_non_empty_string, parse_number, NumberSchema},
    RevisionTracker,
};

pub struct InventoryProvider {
    port: Arc<dyn InventoryPort>,
    definition: InformationProviderDefinition,
    revision: RevisionTracker<InventoryStateSnapshot>,
}

impl InventoryProvider {
    pub fn new(port: Arc<dyn InventoryPort>) -> Self {
        Self {
            port,
            definition: definition(),
            revision: RevisionTracker::default(),
        }
    }

    fn revision_for(&self, inventory: &InventoryStateSnapshot) -> u64 {
        self.revision.revision_for(inventory)
    }

    fn read_snapshot(
        &self,
        observed_at: String,
        request: ProviderReadRequest,
    ) -> Result<ProviderReadResult, InformationProviderError> {
        let inventory = self.port.current();
        let revision = self.revision_for(&inventory);
        let mut values = BTreeMap::new();
        for field in request.fields {
            let value = match field.as_str() {
                "selectedHotbarSlot" => {
                    finite_number(inventory.selected_hotbar_slot, "selectedHotbarSlot")?
                }
                "slots" => slots_value(&inventory.slots)?,
                _ => continue,
            };
            values.insert(field, value);
        }
        Ok(ProviderReadResult {
            information_revision: revision,
            values,
            unavailable: Vec::new(),
            source: InformationReadSource {
                kind: InformationSourceKind::ClientState,
                adapter_revision: "inventory-provider.v1".to_owned(),
                source_revision: revision,
                acquisition: InformationAcquisition::ImmediateClientState,
            },
            observed_at,
            valid_until: None,
            evidence_ids: Vec::new(),
            next_page_state: None,
        })
    }
}

impl InformationProvider for InventoryProvider {
    fn definition(&self) -> &InformationProviderDefinition {
        &self.definition
    }

    fn availability(&self, _context: &InformationProviderContext<'_>) -> ProviderAvailability {
        let inventory = self.port.current();
        ProviderAvailability {
            overall: InformationCatalogEntryAvailability::Available,
            information_revision: self.revision_for(&inventory),
            fields: BTreeMap::new(),
        }
    }

    fn read<'a>(
        &'a self,
        context: InformationProviderContext<'a>,
        request: ProviderReadRequest,
        _control: OperationControl,
    ) -> BoxFuture<'a, Result<ProviderReadResult, InformationProviderError>> {
        let result = self.read_snapshot(context.now.to_owned(), request);
        Box::pin(async move { result })
    }
}

struct SlotsSchema;

impl InformationValueSchema for SlotsSchema {
    fn parse(&self, value: Value) -> Result<Value, InformationValueSchemaError> {
        let Value::Array(slots) = value else {
            return Err(error("expected inventory slot array"));
        };
        slots
            .into_iter()
            .map(parse_slot)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array)
    }
}

fn parse_slot(value: Value) -> Result<Value, InformationValueSchemaError> {
    let Value::Object(slot) = value else {
        return Err(error("expected inventory slot object"));
    };
    let slot_number = slot
        .get("slot")
        .ok_or_else(|| error("inventory slot index is required"))?;
    parse_number(slot_number, Some(0.0), None, true)?;
    let item_name = slot
        .get("itemName")
        .ok_or_else(|| error("inventory itemName is required"))?;
    parse_non_empty_string(item_name)?;
    let count = slot
        .get("count")
        .ok_or_else(|| error("inventory item count is required"))?;
    let count_number = parse_number(count, None, None, true)?;
    if count_number <= 0.0 {
        return Err(error("inventory item count must be positive"));
    }
    for optional in ["metadata", "durabilityUsed"] {
        if let Some(value) = slot.get(optional) {
            parse_number(value, None, None, true)?;
        }
    }

    let mut parsed = Map::new();
    parsed.insert("slot".to_owned(), slot_number.clone());
    parsed.insert("itemName".to_owned(), item_name.clone());
    parsed.insert("count".to_owned(), count.clone());
    for optional in ["metadata", "durabilityUsed"] {
        if let Some(value) = slot.get(optional) {
            parsed.insert(optional.to_owned(), value.clone());
        }
    }
    Ok(Value::Object(parsed))
}

fn definition() -> InformationProviderDefinition {
    InformationProviderDefinition {
        id: InformationInterfaceId::InventoryInformation,
        description: "站立不动时可直接得知的背包内容与当前选中快捷栏槽".to_owned(),
        schema_revision: "inventory-information:1".to_owned(),
        audiences: vec![InformationAudience::Participant],
        fields: BTreeMap::from([
            (
                "selectedHotbarSlot".to_owned(),
                field(
                    "当前选中的快捷栏槽位（0-8）",
                    Arc::new(NumberSchema::new(Some(0.0), Some(8.0), true)),
                    "number",
                ),
            ),
            (
                "slots".to_owned(),
                field("背包中所有非空槽位", Arc::new(SlotsSchema), "array"),
            ),
        ]),
        scope_dependencies: vec![
            InformationScopeDependency::Connection,
            InformationScopeDependency::World,
        ],
        selectors: None,
        pagination: None,
        limits: InformationProviderLimits {
            max_fields_per_read: 2,
            max_result_bytes: 16_384,
            timeout_ms: 2_000,
        },
    }
}

fn field(
    description: &str,
    value_schema: Arc<dyn InformationValueSchema>,
    value_type: &str,
) -> InformationFieldDefinition {
    InformationFieldDefinition {
        description: description.to_owned(),
        value_schema,
        value_type: value_type.to_owned(),
        unit: None,
        precision: InformationPrecision::ExactlyDisplayed,
        source_kinds: vec![InformationSourceKind::ClientState],
        requires: None,
        notes: None,
    }
}

fn finite_number(value: f64, field: &str) -> Result<Value, InformationProviderError> {
    serde_json::Number::from_f64(value)
        .map(Value::Number)
        .ok_or_else(|| InformationProviderError::Failed {
            message: format!("inventory field {field} is not a finite JSON number"),
        })
}

fn slots_value(slots: &[InventorySlotSnapshot]) -> Result<Value, InformationProviderError> {
    serde_json::to_value(slots).map_err(|error| InformationProviderError::Failed {
        message: format!("inventory slots are not JSON serializable: {error}"),
    })
}
