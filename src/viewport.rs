//! MineIntent viewport 的 Rust 投影层。
//!
//! 这里把“客户端缓存里有一个方块”与“观察者确实能看到这个方块”分开：
//! `BlockReadResult` 只提供绝对坐标上的观察原语，投影层再做视锥、暴露面和
//! 遮挡射线判断。算法参数与主仓库 `source-ports/perception.ts` 对齐。

use std::{cell::RefCell, cmp::Ordering, collections::HashMap, f64::consts::PI};

use serde::{Deserialize, Serialize};

use crate::snapshot::{
    BlockPosition, BlockReadResult, PoseSnapshot, ProtocolBlockSnapshot, ProtocolEntitySnapshot,
};

/// MineIntent 视口使用的第一人称眼睛高度。
pub const EYE_HEIGHT: f64 = 1.62;
const SECTION_SIZE: i32 = 16;
const RAY_STEP: f64 = 0.25;
const FACE_EPSILON: f64 = 0.01;
const DEFAULT_VERTICAL_HALF_ANGLE: f64 = 35.0 * PI / 180.0;
const DEFAULT_ASPECT_RATIO: f64 = 16.0 / 9.0;
const DEFAULT_LOOKED_AT_DISTANCE: f64 = 4.5;

/// 可见方块候选的几何谓词。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VisibilityPredicate {
    /// 旧的中心射线基线；保留用于对照，不作为生产默认值。
    BlockCentre,
    /// 至少一个朝向观察者的暴露面可被射线到达。
    ExposedFace,
}

impl Default for VisibilityPredicate {
    fn default() -> Self {
        Self::ExposedFace
    }
}

/// 与主仓库 viewport provider 对齐的投影参数。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportOptions {
    pub horizontal_radius: i32,
    pub vertical_radius: i32,
    pub max_distance: f64,
    pub vertical_half_angle: f64,
    pub horizontal_half_angle: f64,
    pub block_limit: usize,
    pub entity_limit: usize,
    pub predicate: VisibilityPredicate,
    pub looked_at_max_distance: f64,
}

impl Default for ViewportOptions {
    fn default() -> Self {
        Self {
            horizontal_radius: 32,
            vertical_radius: 20,
            max_distance: 32.0,
            vertical_half_angle: DEFAULT_VERTICAL_HALF_ANGLE,
            horizontal_half_angle: (DEFAULT_VERTICAL_HALF_ANGLE.tan() * DEFAULT_ASPECT_RATIO)
                .atan(),
            block_limit: 256,
            entity_limit: 8,
            predicate: VisibilityPredicate::ExposedFace,
            looked_at_max_distance: DEFAULT_LOOKED_AT_DISTANCE,
        }
    }
}

impl ViewportOptions {
    /// 检查来自集成层的投影参数，避免无界扫描或无效三角函数。
    pub fn validate(&self) -> Result<(), String> {
        if !(0..=256).contains(&self.horizontal_radius) {
            return Err("viewport horizontal_radius 必须在 0..=256 内".to_owned());
        }
        if !(0..=256).contains(&self.vertical_radius) {
            return Err("viewport vertical_radius 必须在 0..=256 内".to_owned());
        }
        for (name, value) in [
            ("max_distance", self.max_distance),
            ("vertical_half_angle", self.vertical_half_angle),
            ("horizontal_half_angle", self.horizontal_half_angle),
            ("looked_at_max_distance", self.looked_at_max_distance),
        ] {
            if !value.is_finite() || value <= 0.0 {
                return Err(format!("viewport {name} 必须是正的有限数"));
            }
        }
        if self.vertical_half_angle >= PI / 2.0 || self.horizontal_half_angle >= PI / 2.0 {
            return Err("viewport 视锥半角必须小于 90 度".to_owned());
        }
        if self.block_limit > 4_096 || self.entity_limit > 256 {
            return Err("viewport 结果上限过大".to_owned());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportSelf {
    pub position: [f64; 3],
    pub yaw_degrees: f64,
    pub pitch_degrees: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportLegend {
    pub visible_entities: String,
    pub visible_blocks: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportFrame {
    pub coordinates: String,
    #[serde(rename = "self")]
    pub self_pose: ViewportSelf,
    pub legend: ViewportLegend,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportBlock {
    pub name: String,
    pub position: [i32; 3],
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisibleEntity {
    #[serde(rename = "type")]
    pub entity_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player: Option<String>,
    pub position: [f64; 3],
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisibleEntitiesResult {
    pub items: Vec<VisibleEntity>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisibleBlocksResult {
    /// `[block_name, x, y, z]`，与主仓库 viewport 的紧凑值对齐。
    pub blocks: Vec<(String, i32, i32, i32)>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewportProjection {
    pub frame: ViewportFrame,
    pub standing_on_block: Option<ViewportBlock>,
    pub looked_at_block: Option<ViewportBlock>,
    pub visible_entities: VisibleEntitiesResult,
    pub visible_blocks: VisibleBlocksResult,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Point3 {
    x: f64,
    y: f64,
    z: f64,
}

#[derive(Clone, Copy, Debug)]
struct ViewAxes {
    right: Point3,
    up: Point3,
    forward: Point3,
}

#[derive(Clone, Copy, Debug)]
struct AxisAlignedBox {
    min: Point3,
    max: Point3,
}

#[derive(Clone, Copy, Debug)]
struct CameraPoint {
    depth: f64,
    right: f64,
    up: f64,
}

#[derive(Clone, Copy, Debug)]
enum BlockCell {
    Loaded,
    Empty,
    Unloaded,
}

#[derive(Clone, Debug)]
struct BlockHit {
    voxel: BlockPosition,
    name: String,
}

#[derive(Clone, Copy, Debug)]
enum RayProperty {
    Visible,
    Occludes,
}

enum RayOutcome {
    Hit(BlockHit),
    Clear,
    Unloaded,
}

const FACE_NORMALS: [Point3; 6] = [
    Point3 {
        x: 1.0,
        y: 0.0,
        z: 0.0,
    },
    Point3 {
        x: -1.0,
        y: 0.0,
        z: 0.0,
    },
    Point3 {
        x: 0.0,
        y: 1.0,
        z: 0.0,
    },
    Point3 {
        x: 0.0,
        y: -1.0,
        z: 0.0,
    },
    Point3 {
        x: 0.0,
        y: 0.0,
        z: 1.0,
    },
    Point3 {
        x: 0.0,
        y: 0.0,
        z: -1.0,
    },
];

/// 在当前姿态和观察源上生成一次完整 viewport 投影。
///
/// `read_block` 必须返回绝对坐标上的原始观察结果；`Unloaded` 会让相关射线保守失败，
/// 不会把未知区误报成空气或可见空间。
pub fn project<F>(
    pose: &PoseSnapshot,
    entities: &[ProtocolEntitySnapshot],
    read_block: F,
    options: &ViewportOptions,
) -> Result<ViewportProjection, String>
where
    F: Fn(BlockPosition) -> BlockReadResult,
{
    options.validate()?;
    // 一个投影会反复读取同一体素：候选扫描、暴露面邻居和多条射线都会经过
    // 它。RuntimeObservationSource 的单次 read_block 还会创建完整 DTO，局部
    // 缓存既减少世界锁竞争，也避免同一块状态被重复转换。
    let block_cache = RefCell::new(HashMap::<(i32, i32, i32), BlockReadResult>::new());
    let read_cached = |position: BlockPosition| {
        let key = (position.x, position.y, position.z);
        if let Some(result) = block_cache.borrow().get(&key).cloned() {
            return result;
        }
        let result = read_block(position);
        block_cache.borrow_mut().insert(key, result.clone());
        result
    };
    let eye = Point3 {
        x: pose.position.x,
        y: pose.position.y + EYE_HEIGHT,
        z: pose.position.z,
    };
    let axes = view_axes(pose.yaw, pose.pitch);
    let standing_on_block = standing_on_block(&read_cached, pose);
    let looked_at_block = raycast_looked_at_block(&read_cached, eye, pose, options);
    let visible_entities = visible_entities(&read_cached, entities, eye, axes, options);
    let visible_blocks = visible_blocks(&read_cached, pose, eye, axes, options);

    Ok(ViewportProjection {
        frame: ViewportFrame {
            coordinates: "minecraft_world_absolute".to_owned(),
            self_pose: ViewportSelf {
                position: round_position(Point3 {
                    x: pose.position.x,
                    y: pose.position.y,
                    z: pose.position.z,
                }),
                yaw_degrees: round_one(pose.yaw as f64),
                pitch_degrees: round_one(pose.pitch as f64),
            },
            legend: ViewportLegend {
                visible_entities: "items 每项为 {type, player?, position}：type 是原版实体类型（玩家为 player），player 只有玩家才有，position 是 Minecraft 世界绝对坐标；按距离从近到远，truncated 为真表示更远处还有实体没列出".to_owned(),
                visible_blocks: "[block_name, x, y, z]，同一坐标系的整数体素，按距离从近到远，可能截断".to_owned(),
            },
        },
        standing_on_block,
        looked_at_block,
        visible_entities,
        visible_blocks,
    })
}

fn view_axes(yaw_degrees: f32, pitch_degrees: f32) -> ViewAxes {
    // Azalea 的 LookDirection 明确以度为单位；主仓库 geometry.ts 内部使用弧度。
    let yaw = f64::from(yaw_degrees).to_radians();
    let pitch = f64::from(pitch_degrees).to_radians();
    let forward = Point3 {
        x: -yaw.sin() * pitch.cos(),
        y: pitch.sin(),
        z: -yaw.cos() * pitch.cos(),
    };
    let level = Point3 {
        x: -yaw.sin(),
        y: 0.0,
        z: -yaw.cos(),
    };
    let right = Point3 {
        x: -level.z,
        y: 0.0,
        z: level.x,
    };
    ViewAxes {
        right,
        up: cross(right, forward),
        forward,
    }
}

fn standing_on_block<F>(read_block: &F, pose: &PoseSnapshot) -> Option<ViewportBlock>
where
    F: Fn(BlockPosition) -> BlockReadResult,
{
    let position = BlockPosition {
        x: pose.position.x.floor() as i32,
        y: pose.position.y.floor() as i32 - 1,
        z: pose.position.z.floor() as i32,
    };
    match read_cell(read_block, position.clone()) {
        BlockCell::Loaded => read_loaded_snapshot(read_block, position),
        BlockCell::Empty | BlockCell::Unloaded => None,
    }
}

fn read_loaded_snapshot<F>(read_block: &F, position: BlockPosition) -> Option<ViewportBlock>
where
    F: Fn(BlockPosition) -> BlockReadResult,
{
    match read_block(position.clone()) {
        BlockReadResult::Loaded { block } if is_visible_block(&block) => Some(ViewportBlock {
            name: block.name,
            position: [position.x, position.y, position.z],
        }),
        BlockReadResult::Loaded { .. }
        | BlockReadResult::Unloaded
        | BlockReadResult::OutOfWorld => None,
    }
}

fn raycast_looked_at_block<F>(
    read_block: &F,
    eye: Point3,
    pose: &PoseSnapshot,
    options: &ViewportOptions,
) -> Option<ViewportBlock>
where
    F: Fn(BlockPosition) -> BlockReadResult,
{
    let direction = view_axes(pose.yaw, pose.pitch).forward;
    match first_hit(
        read_block,
        eye,
        direction,
        options.looked_at_max_distance,
        RayProperty::Visible,
    ) {
        RayOutcome::Hit(BlockHit { voxel, name }) => Some(ViewportBlock {
            name,
            position: [voxel.x, voxel.y, voxel.z],
        }),
        RayOutcome::Clear | RayOutcome::Unloaded => None,
    }
}

fn visible_blocks<F>(
    read_block: &F,
    pose: &PoseSnapshot,
    eye: Point3,
    axes: ViewAxes,
    options: &ViewportOptions,
) -> VisibleBlocksResult
where
    F: Fn(BlockPosition) -> BlockReadResult,
{
    let self_voxel = BlockPosition {
        x: pose.position.x.floor() as i32,
        y: pose.position.y.floor() as i32,
        z: pose.position.z.floor() as i32,
    };
    let lowest = BlockPosition {
        x: self_voxel.x - options.horizontal_radius,
        y: self_voxel.y - options.vertical_radius,
        z: self_voxel.z - options.horizontal_radius,
    };
    let highest = BlockPosition {
        x: self_voxel.x + options.horizontal_radius,
        y: self_voxel.y + options.vertical_radius,
        z: self_voxel.z + options.horizontal_radius,
    };
    let mut candidates = Vec::new();

    // 和主仓库一样先用 section AABB 做保守剔除，避免对背后的方块执行体素级射线。
    for sx in section_of(lowest.x)..=section_of(highest.x) {
        for sz in section_of(lowest.z)..=section_of(highest.z) {
            for sy in section_of(lowest.y)..=section_of(highest.y) {
                let bounds = AxisAlignedBox {
                    min: Point3 {
                        x: f64::from(sx * SECTION_SIZE),
                        y: f64::from(sy * SECTION_SIZE),
                        z: f64::from(sz * SECTION_SIZE),
                    },
                    max: Point3 {
                        x: f64::from(sx * SECTION_SIZE + SECTION_SIZE),
                        y: f64::from(sy * SECTION_SIZE + SECTION_SIZE),
                        z: f64::from(sz * SECTION_SIZE + SECTION_SIZE),
                    },
                };
                if distance_to_box(eye, bounds) > options.max_distance
                    || !box_intersects_frustum(axes, eye, bounds, options)
                {
                    continue;
                }

                let x_start = (bounds.min.x as i32).max(lowest.x);
                let x_end = (bounds.max.x as i32 - 1).min(highest.x);
                let y_start = (bounds.min.y as i32).max(lowest.y);
                let y_end = (bounds.max.y as i32 - 1).min(highest.y);
                let z_start = (bounds.min.z as i32).max(lowest.z);
                let z_end = (bounds.max.z as i32 - 1).min(highest.z);
                for x in x_start..=x_end {
                    for z in z_start..=z_end {
                        for y in y_start..=y_end {
                            let position = BlockPosition { x, y, z };
                            let center = Point3 {
                                x: f64::from(x) + 0.5,
                                y: f64::from(y) + 0.5,
                                z: f64::from(z) + 0.5,
                            };
                            let delta = subtract(center, eye);
                            let distance = length(delta);
                            if distance > options.max_distance
                                || (distance > 0.0 && !inside_frustum(axes, delta, options))
                            {
                                continue;
                            }
                            let BlockReadResult::Loaded { block } = read_block(position.clone())
                            else {
                                continue;
                            };
                            if !is_visible_block(&block)
                                || !is_visible_candidate(
                                    read_block,
                                    eye,
                                    &position,
                                    distance,
                                    options.predicate,
                                )
                            {
                                continue;
                            }
                            candidates.push((distance, position, block.name));
                        }
                    }
                }
            }
        }
    }

    candidates.sort_by(|left, right| compare_candidate(left, right));
    let truncated = candidates.len() > options.block_limit;
    let blocks = candidates
        .into_iter()
        .take(options.block_limit)
        .map(|(_, position, name)| (name, position.x, position.y, position.z))
        .collect();
    VisibleBlocksResult { blocks, truncated }
}

fn visible_entities<F>(
    read_block: &F,
    entities: &[ProtocolEntitySnapshot],
    eye: Point3,
    axes: ViewAxes,
    options: &ViewportOptions,
) -> VisibleEntitiesResult
where
    F: Fn(BlockPosition) -> BlockReadResult,
{
    let mut candidates = Vec::new();
    for entity in entities.iter().filter(|entity| entity.valid) {
        let width = f64::from(entity.width.max(0.01));
        let height = f64::from(entity.height.max(0.01));
        let half_width = width / 2.0;
        let bounds = AxisAlignedBox {
            min: Point3 {
                x: entity.position.x - half_width,
                y: entity.position.y,
                z: entity.position.z - half_width,
            },
            max: Point3 {
                x: entity.position.x + half_width,
                y: entity.position.y + height,
                z: entity.position.z + half_width,
            },
        };
        let center = Point3 {
            x: entity.position.x,
            y: entity.position.y + height / 2.0,
            z: entity.position.z,
        };
        let distance = length(subtract(center, eye));
        if distance_to_box(eye, bounds) > options.max_distance
            || !box_intersects_frustum(axes, eye, bounds, options)
        {
            continue;
        }

        let mut visible = point_inside_box(eye, bounds);
        if !visible {
            for point in box_visibility_samples(bounds) {
                let point_delta = subtract(point, eye);
                if inside_frustum(axes, point_delta, options)
                    && line_is_clear(read_block, eye, point)
                {
                    visible = true;
                    break;
                }
            }
        }
        if !visible {
            continue;
        }
        candidates.push((
            distance,
            entity
                .name
                .clone()
                .unwrap_or_else(|| entity.entity_type.clone()),
            entity.username.clone(),
            round_position(Point3 {
                x: entity.position.x,
                y: entity.position.y,
                z: entity.position.z,
            }),
            entity.entity_key.clone(),
        ));
    }

    candidates.sort_by(|left, right| {
        left.0
            .partial_cmp(&right.0)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.4.cmp(&right.4))
    });
    let truncated = candidates.len() > options.entity_limit;
    let items = candidates
        .into_iter()
        .take(options.entity_limit)
        .map(
            |(_distance, entity_type, player, position, _)| VisibleEntity {
                entity_type,
                player,
                position,
            },
        )
        .collect();
    VisibleEntitiesResult { items, truncated }
}

fn is_visible_candidate<F>(
    read_block: &F,
    eye: Point3,
    voxel: &BlockPosition,
    distance: f64,
    predicate: VisibilityPredicate,
) -> bool
where
    F: Fn(BlockPosition) -> BlockReadResult,
{
    match predicate {
        VisibilityPredicate::ExposedFace => exposed_face_reaches_eye(read_block, eye, voxel),
        VisibilityPredicate::BlockCentre => {
            has_exposed_face(read_block, voxel)
                && line_reaches_voxel(read_block, eye, voxel, distance)
        }
    }
}

fn exposed_face_reaches_eye<F>(read_block: &F, eye: Point3, voxel: &BlockPosition) -> bool
where
    F: Fn(BlockPosition) -> BlockReadResult,
{
    let center = Point3 {
        x: f64::from(voxel.x) + 0.5,
        y: f64::from(voxel.y) + 0.5,
        z: f64::from(voxel.z) + 0.5,
    };
    let mut candidates = Vec::new();
    for normal in FACE_NORMALS {
        let face = add(center, scale(normal, 0.5));
        let to_eye = subtract(eye, face);
        let reach = length(to_eye);
        if reach == 0.0 {
            return true;
        }
        let squareness = dot(normal, to_eye) / reach;
        if squareness <= 0.0 {
            continue;
        }
        let neighbor = BlockPosition {
            x: voxel.x + normal.x as i32,
            y: voxel.y + normal.y as i32,
            z: voxel.z + normal.z as i32,
        };
        match read_cell(read_block, neighbor.clone()) {
            BlockCell::Unloaded => continue,
            BlockCell::Loaded if cell_occludes(read_block, neighbor) => continue,
            BlockCell::Loaded | BlockCell::Empty => {}
        }
        candidates.push((squareness, add(face, scale(normal, FACE_EPSILON))));
    }
    candidates.sort_by(|left, right| right.0.partial_cmp(&left.0).unwrap_or(Ordering::Equal));
    candidates
        .into_iter()
        .any(|(_, target)| line_is_clear(read_block, eye, target))
}

fn has_exposed_face<F>(read_block: &F, voxel: &BlockPosition) -> bool
where
    F: Fn(BlockPosition) -> BlockReadResult,
{
    FACE_NORMALS.iter().any(|normal| {
        let neighbor = BlockPosition {
            x: voxel.x + normal.x as i32,
            y: voxel.y + normal.y as i32,
            z: voxel.z + normal.z as i32,
        };
        match read_cell(read_block, neighbor.clone()) {
            BlockCell::Unloaded => false,
            BlockCell::Loaded => !cell_occludes(read_block, neighbor),
            BlockCell::Empty => true,
        }
    })
}

fn line_reaches_voxel<F>(read_block: &F, eye: Point3, voxel: &BlockPosition, distance: f64) -> bool
where
    F: Fn(BlockPosition) -> BlockReadResult,
{
    if distance == 0.0 {
        return true;
    }
    let center = Point3 {
        x: f64::from(voxel.x) + 0.5,
        y: f64::from(voxel.y) + 0.5,
        z: f64::from(voxel.z) + 0.5,
    };
    match first_hit(
        read_block,
        eye,
        normalize(subtract(center, eye), distance),
        distance + RAY_STEP,
        RayProperty::Occludes,
    ) {
        RayOutcome::Clear => true,
        RayOutcome::Hit(hit) => same_voxel(&hit.voxel, voxel),
        RayOutcome::Unloaded => false,
    }
}

fn line_is_clear<F>(read_block: &F, origin: Point3, target: Point3) -> bool
where
    F: Fn(BlockPosition) -> BlockReadResult,
{
    let delta = subtract(target, origin);
    let distance = length(delta);
    if distance == 0.0 {
        return true;
    }
    matches!(
        first_hit(
            read_block,
            origin,
            normalize(delta, distance),
            (distance - RAY_STEP).max(0.0),
            RayProperty::Occludes,
        ),
        RayOutcome::Clear
    )
}

fn first_hit<F>(
    read_block: &F,
    origin: Point3,
    direction: Point3,
    max_distance: f64,
    property: RayProperty,
) -> RayOutcome
where
    F: Fn(BlockPosition) -> BlockReadResult,
{
    let steps = (max_distance / RAY_STEP).floor() as i32;
    for step in 1..=steps {
        let distance = f64::from(step) * RAY_STEP;
        let voxel = BlockPosition {
            x: (origin.x + direction.x * distance).floor() as i32,
            y: (origin.y + direction.y * distance).floor() as i32,
            z: (origin.z + direction.z * distance).floor() as i32,
        };
        let block = match read_block(voxel.clone()) {
            BlockReadResult::Loaded { block } => block,
            BlockReadResult::OutOfWorld => continue,
            BlockReadResult::Unloaded => return RayOutcome::Unloaded,
        };
        let hits = match property {
            RayProperty::Visible => is_visible_block(&block),
            RayProperty::Occludes => is_occluding_block(&block),
        };
        if hits {
            return RayOutcome::Hit(BlockHit {
                voxel,
                name: block.name,
            });
        }
    }
    RayOutcome::Clear
}

fn read_cell<F>(read_block: &F, position: BlockPosition) -> BlockCell
where
    F: Fn(BlockPosition) -> BlockReadResult,
{
    match read_block(position) {
        BlockReadResult::Loaded { block } => {
            if is_visible_block(&block) {
                BlockCell::Loaded
            } else {
                BlockCell::Empty
            }
        }
        BlockReadResult::OutOfWorld => BlockCell::Empty,
        BlockReadResult::Unloaded => BlockCell::Unloaded,
    }
}

fn cell_occludes<F>(read_block: &F, position: BlockPosition) -> bool
where
    F: Fn(BlockPosition) -> BlockReadResult,
{
    match read_block(position) {
        BlockReadResult::Loaded { block } => is_occluding_block(&block),
        BlockReadResult::Unloaded => true,
        BlockReadResult::OutOfWorld => false,
    }
}

fn is_visible_block(block: &ProtocolBlockSnapshot) -> bool {
    !matches!(block.name.as_str(), "air" | "cave_air" | "void_air")
}

fn is_occluding_block(block: &ProtocolBlockSnapshot) -> bool {
    is_visible_block(block) && !block.transparent_hint
}

fn inside_frustum(axes: ViewAxes, delta: Point3, options: &ViewportOptions) -> bool {
    let depth = dot(delta, axes.forward);
    depth > 0.0
        && dot(delta, axes.right).abs() <= depth * options.horizontal_half_angle.tan()
        && dot(delta, axes.up).abs() <= depth * options.vertical_half_angle.tan()
}

fn box_intersects_frustum(
    axes: ViewAxes,
    eye: Point3,
    bounds: AxisAlignedBox,
    options: &ViewportOptions,
) -> bool {
    let tan_horizontal = options.horizontal_half_angle.tan();
    let tan_vertical = options.vertical_half_angle.tan();
    let corners = box_corners(bounds).map(|corner| {
        let delta = subtract(corner, eye);
        CameraPoint {
            depth: dot(delta, axes.forward),
            right: dot(delta, axes.right),
            up: dot(delta, axes.up),
        }
    });
    let outside_depth = corners.iter().all(|point| point.depth <= 0.0);
    let outside_left = corners
        .iter()
        .all(|point| point.right < -point.depth * tan_horizontal);
    let outside_right = corners
        .iter()
        .all(|point| point.right > point.depth * tan_horizontal);
    let outside_bottom = corners
        .iter()
        .all(|point| point.up < -point.depth * tan_vertical);
    let outside_top = corners
        .iter()
        .all(|point| point.up > point.depth * tan_vertical);
    !(outside_depth || outside_left || outside_right || outside_bottom || outside_top)
}

fn box_corners(bounds: AxisAlignedBox) -> [Point3; 8] {
    [
        Point3 {
            x: bounds.min.x,
            y: bounds.min.y,
            z: bounds.min.z,
        },
        Point3 {
            x: bounds.min.x,
            y: bounds.min.y,
            z: bounds.max.z,
        },
        Point3 {
            x: bounds.min.x,
            y: bounds.max.y,
            z: bounds.min.z,
        },
        Point3 {
            x: bounds.min.x,
            y: bounds.max.y,
            z: bounds.max.z,
        },
        Point3 {
            x: bounds.max.x,
            y: bounds.min.y,
            z: bounds.min.z,
        },
        Point3 {
            x: bounds.max.x,
            y: bounds.min.y,
            z: bounds.max.z,
        },
        Point3 {
            x: bounds.max.x,
            y: bounds.max.y,
            z: bounds.min.z,
        },
        Point3 {
            x: bounds.max.x,
            y: bounds.max.y,
            z: bounds.max.z,
        },
    ]
}

fn box_visibility_samples(bounds: AxisAlignedBox) -> Vec<Point3> {
    let xs = axis_samples(bounds.min.x, bounds.max.x, [0.05, 0.5, 0.95]);
    let ys = axis_samples(bounds.min.y, bounds.max.y, [0.15, 0.5, 0.85]);
    let zs = axis_samples(bounds.min.z, bounds.max.z, [0.05, 0.5, 0.95]);
    let mut points = Vec::with_capacity(27);
    for x in xs {
        for y in ys {
            for z in zs {
                points.push(Point3 { x, y, z });
            }
        }
    }
    points
}

fn axis_samples(minimum: f64, maximum: f64, fractions: [f64; 3]) -> [f64; 3] {
    fractions.map(|fraction| minimum + (maximum - minimum) * fraction)
}

fn point_inside_box(point: Point3, bounds: AxisAlignedBox) -> bool {
    point.x >= bounds.min.x
        && point.x <= bounds.max.x
        && point.y >= bounds.min.y
        && point.y <= bounds.max.y
        && point.z >= bounds.min.z
        && point.z <= bounds.max.z
}

fn distance_to_box(point: Point3, bounds: AxisAlignedBox) -> f64 {
    let dx = (bounds.min.x - point.x)
        .max(0.0)
        .max(point.x - bounds.max.x);
    let dy = (bounds.min.y - point.y)
        .max(0.0)
        .max(point.y - bounds.max.y);
    let dz = (bounds.min.z - point.z)
        .max(0.0)
        .max(point.z - bounds.max.z);
    (dx * dx + dy * dy + dz * dz).sqrt()
}

fn section_of(value: i32) -> i32 {
    value.div_euclid(SECTION_SIZE)
}

fn compare_candidate(
    left: &(f64, BlockPosition, String),
    right: &(f64, BlockPosition, String),
) -> Ordering {
    left.0
        .partial_cmp(&right.0)
        .unwrap_or(Ordering::Equal)
        .then_with(|| left.1.x.cmp(&right.1.x))
        .then_with(|| left.1.y.cmp(&right.1.y))
        .then_with(|| left.1.z.cmp(&right.1.z))
}

fn cross(left: Point3, right: Point3) -> Point3 {
    Point3 {
        x: left.y * right.z - left.z * right.y,
        y: left.z * right.x - left.x * right.z,
        z: left.x * right.y - left.y * right.x,
    }
}

fn add(left: Point3, right: Point3) -> Point3 {
    Point3 {
        x: left.x + right.x,
        y: left.y + right.y,
        z: left.z + right.z,
    }
}

fn scale(value: Point3, factor: f64) -> Point3 {
    Point3 {
        x: value.x * factor,
        y: value.y * factor,
        z: value.z * factor,
    }
}

fn subtract(left: Point3, right: Point3) -> Point3 {
    Point3 {
        x: left.x - right.x,
        y: left.y - right.y,
        z: left.z - right.z,
    }
}

fn dot(left: Point3, right: Point3) -> f64 {
    left.x * right.x + left.y * right.y + left.z * right.z
}

fn length(value: Point3) -> f64 {
    dot(value, value).sqrt()
}

fn normalize(value: Point3, magnitude: f64) -> Point3 {
    Point3 {
        x: value.x / magnitude,
        y: value.y / magnitude,
        z: value.z / magnitude,
    }
}

fn same_voxel(left: &BlockPosition, right: &BlockPosition) -> bool {
    left.x == right.x && left.y == right.y && left.z == right.z
}

fn round_position(position: Point3) -> [f64; 3] {
    [
        round_one(position.x),
        round_one(position.y),
        round_one(position.z),
    ]
}

fn round_one(value: f64) -> f64 {
    let rounded = (value * 10.0).round() / 10.0;
    if rounded == 0.0 {
        0.0
    } else {
        rounded
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::snapshot::Vec3Value;

    fn pose(yaw: f32) -> PoseSnapshot {
        PoseSnapshot {
            position: Vec3Value {
                x: 0.5,
                y: 1.0,
                z: 0.5,
            },
            velocity: Vec3Value {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            yaw,
            pitch: 0.0,
            on_ground: true,
        }
    }

    fn block(name: &str, transparent_hint: bool) -> ProtocolBlockSnapshot {
        ProtocolBlockSnapshot {
            position: BlockPosition { x: 0, y: 0, z: 0 },
            name: name.to_owned(),
            state_id: 1,
            properties: BTreeMap::new(),
            collision_shapes: Vec::new(),
            transparent_hint,
            bounding_box: crate::snapshot::BlockBoundingBox::Block,
        }
    }

    fn entity(key: &str, z: f64) -> ProtocolEntitySnapshot {
        ProtocolEntitySnapshot {
            entity_key: key.to_owned(),
            protocol_entity_id: 1,
            entity_type: "sheep".to_owned(),
            name: None,
            username: None,
            uuid: None,
            position: Vec3Value { x: 0.5, y: 2.0, z },
            velocity: Vec3Value {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            yaw: 0.0,
            pitch: 0.0,
            head_yaw: None,
            width: 0.9,
            height: 1.3,
            on_ground: true,
            pose: None,
            held_item_name: None,
            equipment: Vec::new(),
            valid: true,
        }
    }

    fn fixture_read(position: BlockPosition) -> BlockReadResult {
        if position.x == 0 && position.y == 2 && position.z == -1 {
            return BlockReadResult::Loaded {
                block: block("stone", false),
            };
        }
        BlockReadResult::Loaded {
            block: block("air", true),
        }
    }

    fn options() -> ViewportOptions {
        ViewportOptions {
            horizontal_radius: 4,
            vertical_radius: 4,
            max_distance: 8.0,
            looked_at_max_distance: 8.0,
            block_limit: 64,
            entity_limit: 8,
            ..ViewportOptions::default()
        }
    }

    #[test]
    fn projection_uses_absolute_coordinates_and_first_hit() {
        let projection = project(&pose(0.0), &[], fixture_read, &options())
            .expect("fixture options should be valid");

        assert_eq!(projection.frame.coordinates, "minecraft_world_absolute");
        assert_eq!(projection.frame.self_pose.position, [0.5, 1.0, 0.5]);
        assert_eq!(
            projection.looked_at_block,
            Some(ViewportBlock {
                name: "stone".to_owned(),
                position: [0, 2, -1],
            })
        );
        assert_eq!(
            projection.standing_on_block, None,
            "fixture ground is air, so no fabricated support block is allowed"
        );
    }

    #[test]
    fn opaque_wall_blocks_far_blocks_and_entities() {
        let entities = [entity("near", 0.5), entity("behind-wall", -2.0)];
        let projection = project(&pose(0.0), &entities, fixture_read, &options())
            .expect("fixture options should be valid");

        assert!(projection
            .visible_blocks
            .blocks
            .iter()
            .any(|block| block.0 == "stone" && block.3 == -1));
        assert!(!projection
            .visible_blocks
            .blocks
            .iter()
            .any(|block| block.3 == -2));
        assert_eq!(projection.visible_entities.items.len(), 1);
        assert_eq!(projection.visible_entities.items[0].entity_type, "sheep");
    }

    #[test]
    fn invalid_options_are_rejected_before_scanning() {
        let mut options = ViewportOptions::default();
        options.max_distance = f64::NAN;
        let result = project(&pose(0.0), &[], fixture_read, &options);
        assert!(result.is_err());
    }
}
