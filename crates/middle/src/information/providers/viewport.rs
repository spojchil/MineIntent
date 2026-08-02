use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use mineintent_contracts::minecraft::{
    BackendError, BoxFuture, MinecraftBackendApi, OperationControl, ViewportProjection,
    ViewportRead,
};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::information::contracts::{
    InformationAcquisition, InformationAudience, InformationCatalogEntryAvailability,
    InformationFieldDefinition, InformationInterfaceId, InformationPrecision, InformationProvider,
    InformationProviderContext, InformationProviderDefinition, InformationProviderError,
    InformationProviderLimits, InformationReadSource, InformationScopeDependency,
    InformationSourceKind, InformationValueSchema, InformationValueSchemaError,
    ProviderAvailability, ProviderReadRequest, ProviderReadResult,
};

use super::schema::{error, parse_non_empty_string, parse_number};

const SCHEMA_REVISION: &str = "viewport-information:10";
const ADAPTER_REVISION: &str = "viewport-provider.v3";

/// Information wrapper for the backend's one-and-only viewport kernel.
///
/// This type deliberately has no perception geometry or source-port fallback. A read binds the
/// current observation source once and consumes one atomic ViewportRead from it; the provider only
/// selects the requested fields from that already coherent projection.
pub struct ViewportInformationProvider {
    backend: Arc<dyn MinecraftBackendApi>,
    definition: InformationProviderDefinition,
    last_published_revision: AtomicU64,
}

impl ViewportInformationProvider {
    pub fn new(backend: Arc<dyn MinecraftBackendApi>) -> Self {
        Self {
            backend,
            definition: definition(),
            last_published_revision: AtomicU64::new(0),
        }
    }

    fn result_from_read(
        &self,
        observed_at: String,
        request: ProviderReadRequest,
        read: ViewportRead,
    ) -> Result<ProviderReadResult, InformationProviderError> {
        let values = requested_values(&read.projection, &request.fields)?;
        let revision = read.revision;

        // A read is published only after every requested field has been serialized successfully.
        // fetch_max makes an older read that completes later unable to move availability backwards.
        self.last_published_revision
            .fetch_max(revision, Ordering::AcqRel);

        Ok(ProviderReadResult {
            information_revision: revision,
            values,
            unavailable: Vec::new(),
            source: InformationReadSource {
                kind: InformationSourceKind::ViewportProjection,
                adapter_revision: ADAPTER_REVISION.to_owned(),
                source_revision: revision,
                acquisition: InformationAcquisition::CurrentPerception,
            },
            observed_at,
            valid_until: None,
            evidence_ids: Vec::new(),
            next_page_state: None,
        })
    }
}

impl InformationProvider for ViewportInformationProvider {
    fn definition(&self) -> &InformationProviderDefinition {
        &self.definition
    }

    fn availability(&self, _context: &InformationProviderContext<'_>) -> ProviderAvailability {
        ProviderAvailability {
            overall: InformationCatalogEntryAvailability::Available,
            information_revision: self.last_published_revision.load(Ordering::Acquire),
            fields: BTreeMap::new(),
        }
    }

    fn read<'a>(
        &'a self,
        context: InformationProviderContext<'a>,
        request: ProviderReadRequest,
        control: OperationControl,
    ) -> BoxFuture<'a, Result<ProviderReadResult, InformationProviderError>> {
        let observed_at = context.now.to_owned();
        let source = match self.backend.observation_source() {
            Ok(source) => source,
            Err(error) => {
                return Box::pin(async move { Err(map_backend_error(error)) });
            }
        };

        Box::pin(async move {
            let read = source
                .read_viewport(control)
                .await
                .map_err(map_backend_error)?;
            self.result_from_read(observed_at, request, read)
        })
    }
}

fn map_backend_error(error: BackendError) -> InformationProviderError {
    match error {
        BackendError::Cancelled { .. } => InformationProviderError::Cancelled,
        BackendError::DeadlineExceeded { .. } => InformationProviderError::DeadlineExceeded,
        other => InformationProviderError::Failed {
            message: format!("viewport backend read failed: {other}"),
        },
    }
}

fn requested_values(
    projection: &ViewportProjection,
    fields: &[String],
) -> Result<BTreeMap<String, Value>, InformationProviderError> {
    let mut values = BTreeMap::new();
    for field in fields {
        let value = match field.as_str() {
            "frame" => serialize_projection_value(&projection.frame, field),
            "standingOnBlock" => serialize_projection_value(&projection.standing_on_block, field),
            "lookedAtBlock" => serialize_projection_value(&projection.looked_at_block, field),
            "visibleEntities" => serialize_projection_value(&projection.visible_entities, field),
            "visibleBlocks" => serialize_projection_value(&projection.visible_blocks, field),
            _ => continue,
        }?;
        values.insert(field.clone(), value);
    }
    Ok(values)
}

fn serialize_projection_value<T: Serialize>(
    value: &T,
    field: &str,
) -> Result<Value, InformationProviderError> {
    serde_json::to_value(value).map_err(|error| InformationProviderError::Failed {
        message: format!("viewport field {field} is not JSON serializable: {error}"),
    })
}

fn definition() -> InformationProviderDefinition {
    InformationProviderDefinition {
        id: InformationInterfaceId::ViewportInformation,
        description: "粗略第一人称视野；所有位置都使用 Minecraft 世界绝对坐标，方块为整数体素"
            .to_owned(),
        schema_revision: SCHEMA_REVISION.to_owned(),
        audiences: vec![InformationAudience::Participant],
        fields: BTreeMap::from([
            (
                "frame".to_owned(),
                InformationFieldDefinition {
                    description: "本次观察的姿态与坐标系图例".to_owned(),
                    value_schema: Arc::new(FrameSchema),
                    value_type: "object".to_owned(),
                    unit: None,
                    precision: InformationPrecision::ExactlyDisplayed,
                    source_kinds: vec![InformationSourceKind::ViewportProjection],
                    requires: None,
                    notes: None,
                },
            ),
            (
                "standingOnBlock".to_owned(),
                InformationFieldDefinition {
                    description: "脚下可见方块及其绝对体素坐标".to_owned(),
                    value_schema: Arc::new(BlockSchema),
                    value_type: "object".to_owned(),
                    unit: None,
                    precision: InformationPrecision::Inferred,
                    source_kinds: vec![InformationSourceKind::ViewportProjection],
                    requires: None,
                    notes: None,
                },
            ),
            (
                "lookedAtBlock".to_owned(),
                InformationFieldDefinition {
                    description: "准星射线首先命中的可见方块及其绝对体素坐标".to_owned(),
                    value_schema: Arc::new(BlockSchema),
                    value_type: "object".to_owned(),
                    unit: None,
                    precision: InformationPrecision::Inferred,
                    source_kinds: vec![InformationSourceKind::ViewportProjection],
                    requires: None,
                    notes: None,
                },
            ),
            (
                "visibleEntities".to_owned(),
                InformationFieldDefinition {
                    description:
                        "可见实体；items 每项为{type,player?,position}，按距离从近到远，truncated 表示更远处还有未列出的"
                            .to_owned(),
                    value_schema: Arc::new(VisibleEntitiesSchema),
                    value_type: "object".to_owned(),
                    unit: None,
                    precision: InformationPrecision::Inferred,
                    source_kinds: vec![InformationSourceKind::ViewportProjection],
                    requires: None,
                    notes: None,
                },
            ),
            (
                "visibleBlocks".to_owned(),
                InformationFieldDefinition {
                    description:
                        "可见方块（朝观察者的暴露面无遮挡可达）；每项为[名称,x,y,z]整数体素，按距离从近到远，可能截断"
                            .to_owned(),
                    value_schema: Arc::new(VisibleBlocksSchema),
                    value_type: "object".to_owned(),
                    unit: None,
                    precision: InformationPrecision::Inferred,
                    source_kinds: vec![InformationSourceKind::ViewportProjection],
                    requires: None,
                    notes: None,
                },
            ),
        ]),
        scope_dependencies: vec![
            InformationScopeDependency::Connection,
            InformationScopeDependency::World,
        ],
        selectors: None,
        pagination: None,
        limits: InformationProviderLimits {
            max_fields_per_read: 5,
            max_result_bytes: 65_536,
            timeout_ms: 5_000,
        },
    }
}

struct FrameSchema;

impl InformationValueSchema for FrameSchema {
    fn parse(&self, value: Value) -> Result<Value, InformationValueSchemaError> {
        let object = strict_object(&value, "frame", ["coordinates", "self", "legend"])?;
        let coordinates = required(object, "coordinates", "frame coordinates")?;
        if parse_non_empty_string(coordinates)? != "minecraft_world_absolute" {
            return Err(error("frame coordinates must be minecraft_world_absolute"));
        }

        let self_value = required(object, "self", "frame self")?;
        let self_object = strict_object(
            self_value,
            "frame self",
            ["position", "yawDegrees", "pitchDegrees"],
        )?;
        parse_position(required(self_object, "position", "frame self position")?)?;
        let yaw = required(self_object, "yawDegrees", "frame yawDegrees")?;
        parse_number(yaw, None, None, false)?;
        let pitch = required(self_object, "pitchDegrees", "frame pitchDegrees")?;
        parse_number(pitch, None, None, false)?;

        let legend_value = required(object, "legend", "frame legend")?;
        let legend = strict_object(
            legend_value,
            "frame legend",
            ["visibleEntities", "visibleBlocks"],
        )?;
        parse_non_empty_string(required(
            legend,
            "visibleEntities",
            "frame visibleEntities legend",
        )?)?;
        parse_non_empty_string(required(
            legend,
            "visibleBlocks",
            "frame visibleBlocks legend",
        )?)?;

        // All objects in this schema are strict, so retaining the original value is the same wire
        // result as zod's strictObject after validation.
        Ok(value)
    }
}

struct BlockSchema;

impl InformationValueSchema for BlockSchema {
    fn parse(&self, value: Value) -> Result<Value, InformationValueSchemaError> {
        parse_block(value)
    }
}

struct VisibleEntitiesSchema;

impl InformationValueSchema for VisibleEntitiesSchema {
    fn parse(&self, value: Value) -> Result<Value, InformationValueSchemaError> {
        let object = object(&value, "visible entities")?;
        let items = required(object, "items", "visible entities items")?;
        let Value::Array(items) = items else {
            return Err(error("visible entities items must be an array"));
        };
        let mut parsed_items = Vec::with_capacity(items.len());
        for item in items {
            let item_object =
                strict_object(item, "visible entity", ["type", "player", "position"])?;
            let entity_type = required(item_object, "type", "visible entity type")?;
            parse_non_empty_string(entity_type)?;
            let position = parse_position(required(
                item_object,
                "position",
                "visible entity position",
            )?)?;
            let mut parsed = Map::new();
            parsed.insert("type".to_owned(), entity_type.clone());
            if let Some(player) = item_object.get("player") {
                parse_non_empty_string(player)?;
                parsed.insert("player".to_owned(), player.clone());
            }
            parsed.insert("position".to_owned(), position);
            parsed_items.push(Value::Object(parsed));
        }
        let truncated = required(object, "truncated", "visible entities truncated")?;
        if !truncated.is_boolean() {
            return Err(error("visible entities truncated must be a boolean"));
        }

        Ok(Value::Object(Map::from_iter([
            ("items".to_owned(), Value::Array(parsed_items)),
            ("truncated".to_owned(), truncated.clone()),
        ])))
    }
}

struct VisibleBlocksSchema;

impl InformationValueSchema for VisibleBlocksSchema {
    fn parse(&self, value: Value) -> Result<Value, InformationValueSchemaError> {
        let object = object(&value, "visible blocks")?;
        let blocks = required(object, "blocks", "visible blocks blocks")?;
        let Value::Array(blocks) = blocks else {
            return Err(error("visible blocks blocks must be an array"));
        };
        let mut parsed_blocks = Vec::with_capacity(blocks.len());
        for block in blocks {
            let Value::Array(tuple) = block else {
                return Err(error("visible block must be a four-item tuple"));
            };
            if tuple.len() != 4 {
                return Err(error("visible block must be a four-item tuple"));
            }
            parse_non_empty_string(&tuple[0])?;
            for coordinate in &tuple[1..] {
                parse_number(coordinate, None, None, false)?;
            }
            parsed_blocks.push(Value::Array(tuple.clone()));
        }
        let truncated = required(object, "truncated", "visible blocks truncated")?;
        if !truncated.is_boolean() {
            return Err(error("visible blocks truncated must be a boolean"));
        }

        Ok(Value::Object(Map::from_iter([
            ("blocks".to_owned(), Value::Array(parsed_blocks)),
            ("truncated".to_owned(), truncated.clone()),
        ])))
    }
}

fn parse_block(value: Value) -> Result<Value, InformationValueSchemaError> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    let object = object(&value, "block")?;
    let name = required(object, "name", "block name")?;
    parse_non_empty_string(name)?;
    let position = parse_position(required(object, "position", "block position")?)?;
    Ok(Value::Object(Map::from_iter([
        ("name".to_owned(), name.clone()),
        ("position".to_owned(), position),
    ])))
}

fn parse_position(value: &Value) -> Result<Value, InformationValueSchemaError> {
    let Value::Array(position) = value else {
        return Err(error("position must be a three-item tuple"));
    };
    if position.len() != 3 {
        return Err(error("position must be a three-item tuple"));
    }
    for coordinate in position {
        parse_number(coordinate, None, None, false)?;
    }
    Ok(Value::Array(position.clone()))
}

fn object<'a>(
    value: &'a Value,
    field: &str,
) -> Result<&'a Map<String, Value>, InformationValueSchemaError> {
    let Value::Object(object) = value else {
        return Err(error(&format!("{field} must be an object")));
    };
    Ok(object)
}

fn strict_object<'a, const N: usize>(
    value: &'a Value,
    field: &str,
    allowed: [&str; N],
) -> Result<&'a Map<String, Value>, InformationValueSchemaError> {
    let object = object(value, field)?;
    let allowed = allowed.into_iter().collect::<BTreeSet<_>>();
    if object.keys().any(|key| !allowed.contains(key.as_str())) {
        return Err(error(&format!("{field} contains an unknown key")));
    }
    Ok(object)
}

fn required<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    field: &str,
) -> Result<&'a Value, InformationValueSchemaError> {
    object
        .get(key)
        .ok_or_else(|| error(&format!("{field} is required")))
}
