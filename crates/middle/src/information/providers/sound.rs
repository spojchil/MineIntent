use std::sync::Arc;

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
    source_ports::SoundHistoryPort,
    support::parse_javascript_date_millis,
};

use super::schema::{error, parse_non_empty_string, parse_number};

const RECENT_SOUND_LIMIT: f64 = 20.0;
const JS_MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

pub struct SoundInformationProvider {
    port: Arc<dyn SoundHistoryPort>,
    definition: InformationProviderDefinition,
}

impl SoundInformationProvider {
    pub fn new(port: Arc<dyn SoundHistoryPort>) -> Self {
        Self {
            port,
            definition: definition(),
        }
    }

    fn read_snapshot(
        &self,
        observed_at: String,
        request: ProviderReadRequest,
    ) -> Result<ProviderReadResult, InformationProviderError> {
        let sounds = if request.fields.iter().any(|field| field == "recentSounds") {
            Some(self.port.recent(RECENT_SOUND_LIMIT))
        } else {
            None
        };
        let revision = revision_value(self.port.revision())?;
        let mut values = std::collections::BTreeMap::new();
        if let Some(sounds) = sounds {
            let value =
                serde_json::to_value(sounds).map_err(|error| InformationProviderError::Failed {
                    message: format!("recent sounds are not JSON serializable: {error}"),
                })?;
            values.insert("recentSounds".to_owned(), value);
        }
        Ok(ProviderReadResult {
            information_revision: revision,
            values,
            unavailable: Vec::new(),
            source: InformationReadSource {
                kind: InformationSourceKind::SoundProjection,
                adapter_revision: "sound-provider.v1".to_owned(),
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

impl InformationProvider for SoundInformationProvider {
    fn definition(&self) -> &InformationProviderDefinition {
        &self.definition
    }

    fn availability(&self, _context: &InformationProviderContext<'_>) -> ProviderAvailability {
        let information_revision = revision_value(self.port.revision()).unwrap_or(0);
        ProviderAvailability {
            overall: InformationCatalogEntryAvailability::Available,
            information_revision,
            fields: std::collections::BTreeMap::new(),
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

struct RecentSoundsSchema;

impl InformationValueSchema for RecentSoundsSchema {
    fn parse(&self, value: Value) -> Result<Value, InformationValueSchemaError> {
        let Value::Array(sounds) = value else {
            return Err(error("expected recent sounds array"));
        };
        sounds
            .into_iter()
            .map(parse_sound)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array)
    }
}

fn parse_sound(value: Value) -> Result<Value, InformationValueSchemaError> {
    let Value::Object(sound) = value else {
        return Err(error("expected sound observation object"));
    };
    for optional in ["soundName", "category"] {
        if let Some(value) = sound.get(optional) {
            parse_non_empty_string(value)?;
        }
    }
    let distance = sound
        .get("distance")
        .ok_or_else(|| error("sound distance is required"))?;
    parse_number(distance, Some(0.0), None, false)?;
    let direction = sound
        .get("direction")
        .ok_or_else(|| error("sound direction is required"))?;
    let Value::String(direction) = direction else {
        return Err(error("sound direction must be a string"));
    };
    if !matches!(direction.as_str(), "ahead" | "right" | "behind" | "left") {
        return Err(error("sound direction is not a relative direction"));
    }
    let volume = sound
        .get("volume")
        .ok_or_else(|| error("sound volume is required"))?;
    parse_number(volume, Some(0.0), None, false)?;
    let pitch = sound
        .get("pitch")
        .ok_or_else(|| error("sound pitch is required"))?;
    parse_number(pitch, None, None, false)?;
    let observed_at = sound
        .get("observedAt")
        .ok_or_else(|| error("sound observedAt is required"))?;
    let Value::String(observed_at) = observed_at else {
        return Err(error("sound observedAt must be an ISO datetime"));
    };
    if !is_zod_iso_datetime(observed_at) {
        return Err(error("sound observedAt must be an ISO datetime"));
    }

    let mut parsed = Map::new();
    for optional in ["soundName", "category"] {
        if let Some(value) = sound.get(optional) {
            parsed.insert(optional.to_owned(), value.clone());
        }
    }
    parsed.insert("distance".to_owned(), distance.clone());
    parsed.insert("direction".to_owned(), Value::String(direction.clone()));
    parsed.insert("volume".to_owned(), volume.clone());
    parsed.insert("pitch".to_owned(), pitch.clone());
    parsed.insert("observedAt".to_owned(), Value::String(observed_at.clone()));
    Ok(Value::Object(parsed))
}

fn is_zod_iso_datetime(value: &str) -> bool {
    value.ends_with('Z') && value.contains('T') && parse_javascript_date_millis(value).is_some()
}

fn definition() -> InformationProviderDefinition {
    InformationProviderDefinition {
        id: InformationInterfaceId::SoundInformation,
        description: "站立不动时能听到的最近声音，含相对距离和方向".to_owned(),
        schema_revision: "sound-information:1".to_owned(),
        audiences: vec![InformationAudience::Participant],
        fields: std::collections::BTreeMap::from([(
            "recentSounds".to_owned(),
            InformationFieldDefinition {
                description: "最近听到的声音，按时间从新到旧排列".to_owned(),
                value_schema: Arc::new(RecentSoundsSchema),
                value_type: "array".to_owned(),
                unit: None,
                precision: InformationPrecision::Quantized,
                source_kinds: vec![InformationSourceKind::SoundProjection],
                requires: None,
                notes: Some(
                    "距离和方向是从协议声音包位置换算得到的近似值，不是精确音源坐标".to_owned(),
                ),
            },
        )]),
        scope_dependencies: vec![
            InformationScopeDependency::Connection,
            InformationScopeDependency::World,
        ],
        selectors: None,
        pagination: None,
        limits: InformationProviderLimits {
            max_fields_per_read: 1,
            max_result_bytes: 16_384,
            timeout_ms: 2_000,
        },
    }
}

fn revision_value(value: f64) -> Result<u64, InformationProviderError> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value > JS_MAX_SAFE_INTEGER {
        return Err(InformationProviderError::Failed {
            message: "sound source revision must be a non-negative JavaScript safe integer"
                .to_owned(),
        });
    }
    Ok(value as u64)
}
