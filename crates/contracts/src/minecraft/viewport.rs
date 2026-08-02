use std::{collections::BTreeMap, fmt, sync::Arc};

use serde::{de, ser::SerializeMap, Deserialize, Deserializer, Serialize, Serializer};

use super::{BackendError, BlockPosition, BlockPropertyValue};

pub type WorldPosition = [f64; 3];
pub type VisibleBlockTuple = (BlockInfo, i32, i32, i32);
pub const MAX_DIRECTED_VIEW_POSITIONS: usize = 16;

/// 初版模型可见的跨方块视觉属性白名单。
///
/// 这是“玩家能从方块外观/朝向读到”的状态集合，而不是协议状态全集。尤其不包含
/// 树叶 `distance`/`persistent` 等内部维护属性；新增或删除属性必须同步登记。
pub const INITIAL_VISIBLE_BLOCK_PROPERTY_NAMES: &[&str] = &[
    "age",
    "attached",
    "attachment",
    "axis",
    "bites",
    "bottom",
    "candles",
    "conditional",
    "delay",
    "disarmed",
    "east",
    "east_wall",
    "enabled",
    "face",
    "facing",
    "half",
    "hanging",
    "hinge",
    "honey_level",
    "in_wall",
    "instrument",
    "layers",
    "level",
    "lit",
    "locked",
    "mode",
    "moisture",
    "north",
    "north_wall",
    "occupied",
    "open",
    "orientation",
    "part",
    "pickles",
    "powered",
    "rotation",
    "shape",
    "short",
    "signal_fire",
    "snowy",
    "stage",
    "triggered",
    "unstable",
    "up",
    "vertical_direction",
    "vine_end",
    "wall",
    "waterlogged",
    "west",
    "west_wall",
];

pub fn is_visible_block_property(name: &str) -> bool {
    INITIAL_VISIBLE_BLOCK_PROPERTY_NAMES.contains(&name)
}

/// 将 Azalea 的 property-map 文本恢复为模型可读的 JSON 标量类型。
pub fn parse_block_property_value(raw: &str) -> BlockPropertyValue {
    if raw.eq_ignore_ascii_case("true") {
        return BlockPropertyValue::Boolean(true);
    }
    if raw.eq_ignore_ascii_case("false") {
        return BlockPropertyValue::Boolean(false);
    }
    if let Ok(value) = raw.parse::<i64>() {
        return BlockPropertyValue::Integer(value);
    }
    if let Ok(value) = raw.parse::<f64>() {
        if value.is_finite() {
            return BlockPropertyValue::Number(value);
        }
    }
    BlockPropertyValue::String(raw.to_owned())
}

/// 按方块名扩展呈现的注册接口。
///
/// 初版 registry 为空；没有注册 hook 时属性会原样透传。hook 只能在唯一
/// `BlockInfo` 构造路径上改写已筛出的白名单属性，不能打开隐藏协议状态。
#[derive(Clone, Default)]
pub struct BlockInfoPresenterRegistry {
    presenters: BTreeMap<
        String,
        Arc<dyn Fn(&str, &mut BTreeMap<String, BlockPropertyValue>) + Send + Sync>,
    >,
}

impl BlockInfoPresenterRegistry {
    pub fn register<F>(&mut self, block_name: impl Into<String>, presenter: F)
    where
        F: Fn(&str, &mut BTreeMap<String, BlockPropertyValue>) + Send + Sync + 'static,
    {
        self.presenters
            .insert(block_name.into(), Arc::new(presenter));
    }

    pub fn is_empty(&self) -> bool {
        self.presenters.is_empty()
    }

    pub fn apply(&self, block_name: &str, properties: &mut BTreeMap<String, BlockPropertyValue>) {
        if let Some(presenter) = self.presenters.get(block_name) {
            presenter(block_name, properties);
        }
        properties.retain(|name, _| is_visible_block_property(name));
    }
}

/// 唯一的模型可见方块表示。
///
/// 序列化时没有白名单属性就是名称字符串；有属性时属性扁平放在对象中，形如
/// `{ "name": "furnace", "facing": "north", "lit": false }`。
#[derive(Clone, Debug, PartialEq)]
pub struct BlockInfo {
    pub name: String,
    pub properties: BTreeMap<String, BlockPropertyValue>,
}

impl BlockInfo {
    pub fn bare(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            properties: BTreeMap::new(),
        }
    }

    pub fn from_raw_properties(
        name: impl Into<String>,
        properties: &BTreeMap<String, String>,
    ) -> Self {
        Self::from_raw_properties_with_registry(
            name,
            properties,
            &BlockInfoPresenterRegistry::default(),
        )
    }

    pub fn from_raw_properties_with_registry(
        name: impl Into<String>,
        properties: &BTreeMap<String, String>,
        registry: &BlockInfoPresenterRegistry,
    ) -> Self {
        let name = name.into();
        let mut visible = properties
            .iter()
            .filter(|(property, _)| is_visible_block_property(property))
            .map(|(property, value)| (property.clone(), parse_block_property_value(value)))
            .collect();
        registry.apply(&name, &mut visible);
        Self {
            name,
            properties: visible,
        }
    }

    pub fn from_property_values(
        name: impl Into<String>,
        properties: BTreeMap<String, BlockPropertyValue>,
    ) -> Self {
        Self::from_property_values_with_registry(
            name,
            properties,
            &BlockInfoPresenterRegistry::default(),
        )
    }

    pub fn from_property_values_with_registry(
        name: impl Into<String>,
        mut properties: BTreeMap<String, BlockPropertyValue>,
        registry: &BlockInfoPresenterRegistry,
    ) -> Self {
        let name = name.into();
        properties.retain(|property, _| is_visible_block_property(property));
        registry.apply(&name, &mut properties);
        Self { name, properties }
    }

    fn visible_properties(&self) -> BTreeMap<String, BlockPropertyValue> {
        self.properties
            .iter()
            .filter(|(property, _)| is_visible_block_property(property))
            .map(|(property, value)| (property.clone(), value.clone()))
            .collect()
    }

    fn validate_name(name: &str) -> Result<(), String> {
        if name.is_empty() {
            Err("block name must not be empty".to_owned())
        } else {
            Ok(())
        }
    }
}

impl Serialize for BlockInfo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Self::validate_name(&self.name).map_err(serde::ser::Error::custom)?;
        let properties = self.visible_properties();
        if properties.is_empty() {
            return serializer.serialize_str(&self.name);
        }

        let mut map = serializer.serialize_map(Some(properties.len() + 1))?;
        map.serialize_entry("name", &self.name)?;
        for (property, value) in properties {
            map.serialize_entry(&property, &value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for BlockInfo {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BlockInfoVisitor;

        impl<'de> de::Visitor<'de> for BlockInfoVisitor {
            type Value = BlockInfo;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a block name string or a block info object")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                BlockInfo::validate_name(value).map_err(E::custom)?;
                Ok(BlockInfo::bare(value))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                self.visit_str(&value)
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: de::MapAccess<'de>,
            {
                let mut name = None;
                let mut properties = BTreeMap::new();
                while let Some(key) = map.next_key::<String>()? {
                    if key == "name" {
                        if name.is_some() {
                            return Err(de::Error::duplicate_field("name"));
                        }
                        name = Some(map.next_value::<String>()?);
                    } else {
                        if !is_visible_block_property(&key) {
                            return Err(de::Error::custom(format!(
                                "unknown block visual property: {key}"
                            )));
                        }
                        let value = map.next_value::<BlockPropertyValue>()?;
                        if properties.insert(key.clone(), value).is_some() {
                            return Err(de::Error::custom(format!(
                                "duplicate block visual property: {key}"
                            )));
                        }
                    }
                }

                let name = name.ok_or_else(|| de::Error::missing_field("name"))?;
                BlockInfo::validate_name(&name).map_err(de::Error::custom)?;
                if properties.is_empty() {
                    return Err(de::Error::custom(
                        "block info object must include at least one visual property",
                    ));
                }
                Ok(BlockInfo { name, properties })
            }
        }

        deserializer.deserialize_any(BlockInfoVisitor)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewportCoordinateSystem {
    #[default]
    #[serde(rename = "minecraft_world_absolute")]
    MinecraftWorldAbsolute,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportFrame {
    pub coordinates: ViewportCoordinateSystem,
    #[serde(rename = "self")]
    pub self_pose: ViewportSelfPose,
    pub legend: ViewportLegend,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportSelfPose {
    pub position: WorldPosition,
    /// Degrees, unlike snapshot and observation pose angles.
    pub yaw_degrees: f64,
    /// Degrees, unlike snapshot and observation pose angles.
    pub pitch_degrees: f64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportLegend {
    pub visible_entities: String,
    pub visible_blocks: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportBlock {
    pub block: BlockInfo,
    pub position: WorldPosition,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisibleEntityView {
    #[serde(rename = "type")]
    pub entity_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player: Option<String>,
    pub position: WorldPosition,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisibleEntitiesView {
    pub items: Vec<VisibleEntityView>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisibleBlocksView {
    pub blocks: Vec<VisibleBlockTuple>,
    pub truncated: bool,
}

/// backend 唯一 viewport kernel 的完整、一次性投影结果。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportProjection {
    pub frame: ViewportFrame,
    pub standing_on_block: Option<ViewportBlock>,
    pub looked_at_block: Option<ViewportBlock>,
    pub visible_entities: VisibleEntitiesView,
    pub visible_blocks: VisibleBlocksView,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectedWhy {
    OutsideFov,
    TooFar,
    Occluded,
    ChunkNotLoaded,
    OutOfWorld,
}

impl DirectedWhy {
    fn rank(self) -> u8 {
        match self {
            Self::OutsideFov => 0,
            Self::TooFar => 1,
            Self::Occluded => 2,
            Self::ChunkNotLoaded => 3,
            Self::OutOfWorld => 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectedOccluder {
    pub at: [i32; 3],
    pub block: BlockInfo,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectedSeenBlock {
    pub at: [i32; 3],
    pub block: BlockInfo,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DirectedUnseenBlock {
    pub at: [i32; 3],
    pub why: Vec<DirectedWhy>,
    pub distance: Option<f64>,
    pub max: Option<f64>,
    pub by: Option<DirectedOccluder>,
}

impl DirectedUnseenBlock {
    pub fn validate(&self) -> Result<(), String> {
        if self.why.is_empty() {
            return Err("directed unseen why must not be empty".to_owned());
        }
        for pair in self.why.windows(2) {
            if pair[0].rank() >= pair[1].rank() {
                return Err("directed unseen why must be unique and in canonical order".to_owned());
            }
        }
        let has_too_far = self.why.contains(&DirectedWhy::TooFar);
        match (has_too_far, self.distance, self.max) {
            (true, Some(distance), Some(max)) if distance.is_finite() && max.is_finite() => {
                if max <= 0.0 || distance <= max {
                    return Err(
                        "directed too_far requires finite distance greater than max".to_owned()
                    );
                }
            }
            (true, _, _) => {
                return Err("directed too_far requires distance and max".to_owned());
            }
            (false, None, None) => {}
            (false, _, _) => {
                return Err("distance and max are only valid with too_far".to_owned());
            }
        }
        if !self.why.contains(&DirectedWhy::Occluded) && self.by.is_some() {
            return Err("by is only valid with occluded".to_owned());
        }
        if self.why.contains(&DirectedWhy::OutOfWorld) && self.by.is_some() {
            return Err("out_of_world rows must not contain by".to_owned());
        }
        Ok(())
    }
}

impl Serialize for DirectedUnseenBlock {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        let mut fields = 2;
        if self.distance.is_some() {
            fields += 1;
        }
        if self.max.is_some() {
            fields += 1;
        }
        if self.by.is_some() {
            fields += 1;
        }
        let mut map = serializer.serialize_map(Some(fields))?;
        map.serialize_entry("at", &self.at)?;
        map.serialize_entry("why", &self.why)?;
        if let Some(distance) = self.distance {
            map.serialize_entry("distance", &distance)?;
        }
        if let Some(max) = self.max {
            map.serialize_entry("max", &max)?;
        }
        if let Some(by) = &self.by {
            map.serialize_entry("by", by)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for DirectedUnseenBlock {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            at: [i32; 3],
            why: Vec<DirectedWhy>,
            #[serde(default)]
            distance: Option<f64>,
            #[serde(default)]
            max: Option<f64>,
            #[serde(default)]
            by: Option<DirectedOccluder>,
        }
        let raw = Raw::deserialize(deserializer)?;
        let result = Self {
            at: raw.at,
            why: raw.why,
            distance: raw.distance,
            max: raw.max,
            by: raw.by,
        };
        result.validate().map_err(de::Error::custom)?;
        Ok(result)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DirectedViewportProjection {
    pub seen: Vec<DirectedSeenBlock>,
    pub unseen: Vec<DirectedUnseenBlock>,
}

impl DirectedViewportProjection {
    pub fn validate(&self) -> Result<(), String> {
        if self.seen.len().saturating_add(self.unseen.len()) > MAX_DIRECTED_VIEW_POSITIONS {
            return Err(format!(
                "directed output accepts at most {MAX_DIRECTED_VIEW_POSITIONS} positions"
            ));
        }
        let mut coordinates = std::collections::HashSet::new();
        for item in &self.seen {
            if !coordinates.insert(item.at) {
                return Err("directed output contains duplicate coordinates".to_owned());
            }
        }
        for item in &self.unseen {
            item.validate()?;
            if !coordinates.insert(item.at) {
                return Err("directed output contains duplicate coordinates".to_owned());
            }
        }
        Ok(())
    }
}

impl Serialize for DirectedViewportProjection {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("seen", &self.seen)?;
        map.serialize_entry("unseen", &self.unseen)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for DirectedViewportProjection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            seen: Vec<DirectedSeenBlock>,
            unseen: Vec<DirectedUnseenBlock>,
        }
        let raw = Raw::deserialize(deserializer)?;
        let result = Self {
            seen: raw.seen,
            unseen: raw.unseen,
        };
        result.validate().map_err(de::Error::custom)?;
        Ok(result)
    }
}

/// Internal failure boundary for a non-target viewport read; queried targets use the
/// model-visible `DirectedWhy::OutOfWorld` row instead of this error.
#[derive(Clone, Debug, PartialEq)]
pub enum DirectedViewportError {
    Backend(BackendError),
    OutOfWorld { position: BlockPosition },
}

impl From<BackendError> for DirectedViewportError {
    fn from(error: BackendError) -> Self {
        Self::Backend(error)
    }
}

impl fmt::Display for DirectedViewportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(error) => error.fmt(formatter),
            Self::OutOfWorld { position } => write!(
                formatter,
                "directed viewport position is outside the world: ({}, {}, {})",
                position.x, position.y, position.z
            ),
        }
    }
}

impl std::error::Error for DirectedViewportError {}

/// 原子读取：三项必须来自同一次 backend capture，不允许 middle 分别读取。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportRead {
    pub projection: ViewportProjection,
    pub source: super::FactSource,
    pub revision: u64,
}

/// 模型可见视口的独立 schema 锚点。
///
/// `ViewportProjection` 仍然是 backend observation source 的旧 kernel DTO；它不能
/// 直接穿过 middle 的模型边界。模型只接收下面的 v2 presenter 结果。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum ViewportProtocolV2 {
    #[default]
    #[serde(rename = "mineintent.viewport.v2")]
    V2,
}

impl ViewportProtocolV2 {
    pub const WIRE: &'static str = "mineintent.viewport.v2";
}

/// `view(mode="full")` 与轮末 viewport frame 共用的模型可见 payload。
///
/// 这是有意与 backend 的 `ViewportProjection` 分开的 DTO：`standingOnBlock` 已从
/// 模型通道删除，但 backend 内部仍可以保留它作为 kernel/旧回放数据。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportFullV2 {
    pub protocol: ViewportProtocolV2,
    pub frame: ViewportFrame,
    #[serde(deserialize_with = "deserialize_required_nullable")]
    pub looked_at_block: Option<ViewportBlock>,
    pub visible_entities: VisibleEntitiesView,
    pub visible_blocks: VisibleBlocksView,
}

fn deserialize_required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// `view(mode="directed")` 共用同一个模型可见 schema 锚点。
#[derive(Clone, Debug, PartialEq)]
pub struct ViewportDirectedV2 {
    pub protocol: ViewportProtocolV2,
    pub seen: Vec<DirectedSeenBlock>,
    pub unseen: Vec<DirectedUnseenBlock>,
}

impl ViewportDirectedV2 {
    pub fn validate(&self) -> Result<(), String> {
        DirectedViewportProjection {
            seen: self.seen.clone(),
            unseen: self.unseen.clone(),
        }
        .validate()
    }
}

impl Serialize for ViewportDirectedV2 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(serde::ser::Error::custom)?;
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("protocol", &self.protocol)?;
        map.serialize_entry("seen", &self.seen)?;
        map.serialize_entry("unseen", &self.unseen)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for ViewportDirectedV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Raw {
            protocol: ViewportProtocolV2,
            seen: Vec<DirectedSeenBlock>,
            unseen: Vec<DirectedUnseenBlock>,
        }

        let raw = Raw::deserialize(deserializer)?;
        let value = Self {
            protocol: raw.protocol,
            seen: raw.seen,
            unseen: raw.unseen,
        };
        value.validate().map_err(de::Error::custom)?;
        Ok(value)
    }
}

pub type ViewportProjectionV2 = ViewportFullV2;
pub type DirectedViewportProjectionV2 = ViewportDirectedV2;

/// The two model-visible forms share the same nested protocol anchor without adding a
/// model-visible `mode` field to either frozen payload.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewportV2 {
    Full(ViewportFullV2),
    Directed(ViewportDirectedV2),
}

impl Serialize for ViewportV2 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Full(value) => value.serialize(serializer),
            Self::Directed(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ViewportV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value
            .as_object()
            .is_some_and(|object| object.contains_key("seen") || object.contains_key("unseen"))
        {
            serde_json::from_value(value)
                .map(Self::Directed)
                .map_err(de::Error::custom)
        } else {
            serde_json::from_value(value)
                .map(Self::Full)
                .map_err(de::Error::custom)
        }
    }
}

/// 唯一的 kernel → model presenter。所有模型可见 full viewport 消费方都必须经过它。
pub fn present_viewport_v2(projection: &ViewportProjection) -> ViewportFullV2 {
    ViewportFullV2 {
        protocol: ViewportProtocolV2::V2,
        frame: projection.frame.clone(),
        looked_at_block: projection.looked_at_block.clone(),
        visible_entities: projection.visible_entities.clone(),
        visible_blocks: projection.visible_blocks.clone(),
    }
}

/// 唯一的 kernel directed DTO → model presenter；几何和 directed 五值纪律仍由 kernel
/// 的 `DirectedViewportProjection` 负责。
pub fn present_directed_viewport_v2(
    projection: &DirectedViewportProjection,
) -> Result<ViewportDirectedV2, String> {
    let value = ViewportDirectedV2 {
        protocol: ViewportProtocolV2::V2,
        seen: projection.seen.clone(),
        unseen: projection.unseen.clone(),
    };
    value.validate()?;
    Ok(value)
}
