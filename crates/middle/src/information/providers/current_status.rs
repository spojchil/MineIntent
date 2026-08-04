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
    source_ports::{SelfEffectSnapshot, SelfVitalsPort, SelfVitalsSnapshot},
};

use super::{
    schema::{error, parse_non_empty_string, parse_number, NumberSchema},
    RevisionTracker,
};

pub struct CurrentStatusProvider {
    port: Arc<dyn SelfVitalsPort>,
    definition: InformationProviderDefinition,
    revision: RevisionTracker<SelfVitalsSnapshot>,
}

impl CurrentStatusProvider {
    pub fn new(port: Arc<dyn SelfVitalsPort>) -> Self {
        Self {
            port,
            definition: definition(),
            revision: RevisionTracker::default(),
        }
    }

    fn revision_for(&self, vitals: &SelfVitalsSnapshot) -> u64 {
        self.revision.revision_for(vitals)
    }

    fn read_snapshot(
        &self,
        observed_at: String,
        request: ProviderReadRequest,
    ) -> Result<ProviderReadResult, InformationProviderError> {
        let vitals = self.port.current();
        let revision = self.revision_for(&vitals);
        let mut values = BTreeMap::new();
        for field in request.fields {
            let value = match field.as_str() {
                "health" => finite_number(vitals.health, "health")?,
                "food" => finite_number(vitals.food, "food")?,
                "foodSaturation" => finite_number(vitals.food_saturation, "foodSaturation")?,
                "oxygen" => finite_number(vitals.oxygen.unwrap_or(20.0), "oxygen")?,
                "experienceLevel" => finite_number(
                    vitals
                        .experience
                        .as_ref()
                        .map_or(0.0, |experience| experience.level),
                    "experienceLevel",
                )?,
                "statusEffects" => status_effects_value(&vitals.effects)?,
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
                adapter_revision: "current-status-provider.v1".to_owned(),
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

impl InformationProvider for CurrentStatusProvider {
    fn definition(&self) -> &InformationProviderDefinition {
        &self.definition
    }

    fn availability(&self, _context: &InformationProviderContext<'_>) -> ProviderAvailability {
        let vitals = self.port.current();
        ProviderAvailability {
            overall: InformationCatalogEntryAvailability::Available,
            information_revision: self.revision_for(&vitals),
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

struct StatusEffectsSchema;

impl InformationValueSchema for StatusEffectsSchema {
    fn parse(&self, value: Value) -> Result<Value, InformationValueSchemaError> {
        let Value::Array(effects) = value else {
            return Err(error("expected status effect array"));
        };
        effects
            .into_iter()
            .map(parse_status_effect)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array)
    }
}

fn parse_status_effect(value: Value) -> Result<Value, InformationValueSchemaError> {
    let Value::Object(effect) = value else {
        return Err(error("expected status effect object"));
    };
    let name = effect
        .get("name")
        .ok_or_else(|| error("status effect name is required"))?;
    parse_non_empty_string(name)?;
    let amplifier = effect
        .get("amplifier")
        .ok_or_else(|| error("status effect amplifier is required"))?;
    parse_number(amplifier, None, None, true)?;
    if let Some(duration) = effect.get("durationTicks") {
        parse_number(duration, None, None, true)?;
    }

    let mut parsed = Map::new();
    parsed.insert("name".to_owned(), name.clone());
    parsed.insert("amplifier".to_owned(), amplifier.clone());
    if let Some(duration) = effect.get("durationTicks") {
        parsed.insert("durationTicks".to_owned(), duration.clone());
    }
    Ok(Value::Object(parsed))
}

fn definition() -> InformationProviderDefinition {
    InformationProviderDefinition {
        id: InformationInterfaceId::CurrentStatus,
        description: "站立不动时可直接得知的自身状态：生命、饥饿、氧气、经验和药水效果".to_owned(),
        schema_revision: "current-status:1".to_owned(),
        audiences: vec![InformationAudience::Participant],
        fields: BTreeMap::from([
            (
                "experienceLevel".to_owned(),
                field(
                    "当前经验等级",
                    Arc::new(NumberSchema::new(Some(0.0), None, true)),
                    "number",
                ),
            ),
            (
                "food".to_owned(),
                field(
                    "当前饥饿值（0-20）",
                    Arc::new(NumberSchema::new(Some(0.0), Some(20.0), false)),
                    "number",
                ),
            ),
            (
                "foodSaturation".to_owned(),
                field(
                    "当前饱和度",
                    Arc::new(NumberSchema::new(Some(0.0), None, false)),
                    "number",
                ),
            ),
            (
                "health".to_owned(),
                field(
                    "当前生命值",
                    Arc::new(NumberSchema::new(Some(0.0), None, false)),
                    "number",
                ),
            ),
            (
                "oxygen".to_owned(),
                field(
                    "当前氧气值；不在水下通常为满值",
                    Arc::new(NumberSchema::new(Some(0.0), None, false)),
                    "number",
                ),
            ),
            (
                "statusEffects".to_owned(),
                field(
                    "当前生效的药水/状态效果",
                    Arc::new(StatusEffectsSchema),
                    "array",
                ),
            ),
        ]),
        scope_dependencies: vec![
            InformationScopeDependency::Connection,
            InformationScopeDependency::World,
        ],
        selectors: None,
        pagination: None,
        limits: InformationProviderLimits {
            max_fields_per_read: 6,
            max_result_bytes: 8_192,
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
            message: format!("current status field {field} is not a finite JSON number"),
        })
}

fn status_effects_value(effects: &[SelfEffectSnapshot]) -> Result<Value, InformationProviderError> {
    serde_json::to_value(effects).map_err(|error| InformationProviderError::Failed {
        message: format!("current status effects are not JSON serializable: {error}"),
    })
}
