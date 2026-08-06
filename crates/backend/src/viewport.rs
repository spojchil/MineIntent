//! MineIntent viewport 的 Rust 投影层。
//!
//! 这里把“客户端缓存里有一个方块”与“观察者确实能看到这个方块”分开：
//! `BlockReadResult` 只提供绝对坐标上的观察原语，投影层再做视锥、暴露面和
//! 遮挡射线判断。算法参数与主仓库 `source-ports/perception.ts` 对齐。

use std::{cmp::Ordering, collections::HashMap, f64::consts::PI};

use mineintent_contracts::capability::validate_directed_positions;
use mineintent_contracts::minecraft::{
    wrap_degrees, BackendError, BlockInfo, BlockInfoPresenterRegistry, DirectedOccluder,
    DirectedSeenBlock, DirectedUnseenBlock, DirectedViewportError, DirectedViewportProjection,
    DirectedWhy,
};
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
const DIRECTED_MAX_DISTANCE: f64 = 32.0;

/// The dimension height metadata captured alongside the world read guard.
///
/// Directed geometry may use this value before touching a target. Keeping the
/// upper-bound calculation in `i64` makes arbitrary `i32` query coordinates
/// safe even when a test seam supplies extreme values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldHeightBounds {
    pub min_y: i32,
    pub height: u32,
}

impl WorldHeightBounds {
    pub const fn new(min_y: i32, height: u32) -> Self {
        Self { min_y, height }
    }

    fn contains_y(self, y: i32) -> bool {
        let y = i64::from(y);
        y >= i64::from(self.min_y) && y < i64::from(self.min_y) + i64::from(self.height)
    }
}

/// 可见方块候选的几何谓词。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum VisibilityPredicate {
    /// 旧的中心射线基线；保留用于对照，不作为生产默认值。
    BlockCentre,
    /// 至少一个朝向观察者的暴露面可被射线到达。
    #[default]
    ExposedFace,
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
    pub block: BlockInfo,
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
    /// `[BlockInfo, x, y, z]`，所有方块表示共用 `BlockInfo` serializer。
    pub blocks: Vec<(BlockInfo, i32, i32, i32)>,
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
    block: BlockInfo,
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
    let presenters = BlockInfoPresenterRegistry::default();
    project_with_checkpoint_and_presenters(pose, entities, read_block, options, &presenters, || {
        Ok(())
    })
    .map_err(|error| match error {
        BackendError::InvalidCommand { message, .. } => message,
        other => other.to_string(),
    })
}

/// Generate a projection while exposing real cancellation/deadline checkpoints
/// to the caller. The callback runs before each expensive geometry phase and
/// before every block/ray read; an error exits the scan immediately.
pub fn project_with_checkpoint<F, C>(
    pose: &PoseSnapshot,
    entities: &[ProtocolEntitySnapshot],
    read_block: F,
    options: &ViewportOptions,
    checkpoint: C,
) -> Result<ViewportProjection, BackendError>
where
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), BackendError>,
{
    let presenters = BlockInfoPresenterRegistry::default();
    project_with_checkpoint_and_presenters(
        pose,
        entities,
        read_block,
        options,
        &presenters,
        checkpoint,
    )
}

/// Full projection variant with an explicit presenter registry. All block slots and ray hits
/// use this same registry and the same `BlockInfo` serializer.
pub fn project_with_checkpoint_and_presenters<F, C>(
    pose: &PoseSnapshot,
    entities: &[ProtocolEntitySnapshot],
    mut read_block: F,
    options: &ViewportOptions,
    presenters: &BlockInfoPresenterRegistry,
    mut checkpoint: C,
) -> Result<ViewportProjection, BackendError>
where
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), BackendError>,
{
    options
        .validate()
        .map_err(|message| BackendError::InvalidCommand {
            field: "viewport".to_owned(),
            message,
        })?;
    checkpoint()?;
    // 一个投影会反复读取同一体素：候选扫描、暴露面邻居和多条射线都会经过
    // 它。RuntimeObservationSource 的单次 read_block 还会创建完整 DTO，局部
    // 缓存既减少世界锁竞争，也避免同一块状态被重复转换。
    let mut block_cache = HashMap::<(i32, i32, i32), BlockReadResult>::new();
    let mut read_cached = |position: BlockPosition| {
        let key = (position.x, position.y, position.z);
        if let Some(result) = block_cache.get(&key).cloned() {
            return result;
        }
        let result = read_block(position);
        block_cache.insert(key, result.clone());
        result
    };
    let eye = Point3 {
        x: pose.position.x,
        y: pose.position.y + EYE_HEIGHT,
        z: pose.position.z,
    };
    let axes = view_axes(pose.yaw, pose.pitch);
    let standing_on_block = standing_on_block(&mut read_cached, pose, presenters, &mut checkpoint)?;
    let looked_at_block = raycast_looked_at_block(
        &mut read_cached,
        eye,
        pose,
        options,
        presenters,
        &mut checkpoint,
    )?;
    let visible_entities = visible_entities(
        &mut read_cached,
        entities,
        eye,
        axes,
        options,
        presenters,
        &mut checkpoint,
    )?;
    let visible_blocks = visible_blocks(
        &mut read_cached,
        pose,
        eye,
        axes,
        options,
        presenters,
        &mut checkpoint,
    )?;

    Ok(ViewportProjection {
        frame: ViewportFrame {
            coordinates: "minecraft_world_absolute".to_owned(),
            self_pose: ViewportSelf {
                position: round_position(Point3 {
                    x: pose.position.x,
                    y: pose.position.y,
                    z: pose.position.z,
                }),
                // 归一化只在给模型看的这一处做：视锥/遮挡的三角函数是周期的，
                // 不在乎转了多少圈；模型在乎。
                yaw_degrees: round_one(wrap_degrees(pose.yaw as f64)),
                pitch_degrees: round_one(pose.pitch as f64),
            },
            legend: ViewportLegend {
                visible_entities: "items 每项为 {type, player?, position}：type 是原版实体类型（玩家为 player），player 只有玩家才有，position 是 Minecraft 世界绝对坐标；按距离从近到远，truncated 为真表示更远处还有实体没列出".to_owned(),
                visible_blocks: "[BlockInfo, x, y, z]；BlockInfo 无视觉属性时为方块名称字符串，有属性时为 name 加全部白名单属性的对象；同一坐标系的整数体素，按距离从近到远，可能截断".to_owned(),
            },
        },
        standing_on_block,
        looked_at_block,
        visible_entities,
        visible_blocks,
    })
}

/// Directed projection in the same backend viewport kernel as full projection.
///
/// The captured height bounds classify out-of-world targets without reads. A target read that
/// independently returns `OutOfWorld` becomes the model-visible reason; neighbor reads follow
/// the full kernel's conservative out-of-world boundary handling.
pub fn project_directed<F, C>(
    pose: &PoseSnapshot,
    positions: &[[i32; 3]],
    read_block: F,
    options: &ViewportOptions,
    world_bounds: WorldHeightBounds,
    checkpoint: C,
) -> Result<DirectedViewportProjection, DirectedViewportError>
where
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), BackendError>,
{
    let presenters = BlockInfoPresenterRegistry::default();
    project_directed_with_presenters(
        pose,
        positions,
        read_block,
        options,
        world_bounds,
        &presenters,
        checkpoint,
    )
}

pub fn project_directed_with_presenters<F, C>(
    pose: &PoseSnapshot,
    positions: &[[i32; 3]],
    mut read_block: F,
    options: &ViewportOptions,
    world_bounds: WorldHeightBounds,
    presenters: &BlockInfoPresenterRegistry,
    mut checkpoint: C,
) -> Result<DirectedViewportProjection, DirectedViewportError>
where
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), BackendError>,
{
    let tuple_positions = positions
        .iter()
        .map(|[x, y, z]| (*x, *y, *z))
        .collect::<Vec<_>>();
    validate_directed_positions(&tuple_positions).map_err(|message| {
        DirectedViewportError::Backend(BackendError::InvalidCommand {
            field: "positions".to_owned(),
            message,
        })
    })?;
    options.validate().map_err(|message| {
        DirectedViewportError::Backend(BackendError::InvalidCommand {
            field: "viewport".to_owned(),
            message,
        })
    })?;
    checkpoint().map_err(DirectedViewportError::Backend)?;

    let eye = Point3 {
        x: pose.position.x,
        y: pose.position.y + EYE_HEIGHT,
        z: pose.position.z,
    };
    let axes = view_axes(pose.yaw, pose.pitch);
    let mut block_cache = HashMap::<(i32, i32, i32), BlockReadResult>::new();
    let mut read_cached = |position: BlockPosition| {
        let key = (position.x, position.y, position.z);
        if let Some(result) = block_cache.get(&key).cloned() {
            return result;
        }
        let result = read_block(position);
        block_cache.insert(key, result.clone());
        result
    };
    let directed_max_distance = options.max_distance.min(DIRECTED_MAX_DISTANCE);
    let mut seen = Vec::new();
    let mut unseen = Vec::new();

    for [x, y, z] in positions.iter().copied() {
        checkpoint().map_err(DirectedViewportError::Backend)?;
        let target = BlockPosition { x, y, z };
        let center = Point3 {
            x: f64::from(x) + 0.5,
            y: f64::from(y) + 0.5,
            z: f64::from(z) + 0.5,
        };
        let delta = subtract(center, eye);
        let distance = length(delta);
        let outside_fov = !inside_frustum(axes, delta, options);
        let too_far = distance > directed_max_distance;
        let out_of_world = !world_bounds.contains_y(y);

        // Geometry is a hard privacy and work boundary. A target outside the view cone or
        // beyond the directed distance limit is classified without consulting the world or
        // tracing a ray, so arbitrary i32 coordinates remain O(1) work.
        if outside_fov || too_far || out_of_world {
            let mut why = Vec::new();
            if outside_fov {
                why.push(DirectedWhy::OutsideFov);
            }
            if too_far {
                why.push(DirectedWhy::TooFar);
            }
            if out_of_world {
                why.push(DirectedWhy::OutOfWorld);
            }
            unseen.push(DirectedUnseenBlock {
                at: [x, y, z],
                why,
                distance: too_far.then_some(distance),
                max: too_far.then_some(directed_max_distance),
                by: None,
            });
            continue;
        }

        let target_result = read_cached(target.clone());
        // 一次 match 同时定出「这块方块的身份」与「看不看得见」。拆成两次会让
        // 第二次的 OutOfWorld 分支不可达，只能靠 unreachable! 兜住——而那个
        // 不变量（同一个不可变值在两次 match 之间不变）编译器无法传播，只能
        // 靠人读。合起来之后它不再需要被相信。
        let (target_info, target_visible) = match &target_result {
            BlockReadResult::Loaded { block } => {
                let info = BlockInfo::from_raw_properties_with_registry(
                    block.name.clone(),
                    &block.properties,
                    presenters,
                );
                let visible = is_visible_candidate(
                    &mut read_cached,
                    eye,
                    &target,
                    distance,
                    options.predicate,
                    presenters,
                    &mut checkpoint,
                )
                .map_err(DirectedViewportError::Backend)?;
                (Some(info), visible)
            }
            BlockReadResult::Unloaded => (None, false),
            BlockReadResult::OutOfWorld => {
                unseen.push(DirectedUnseenBlock {
                    at: [x, y, z],
                    why: vec![DirectedWhy::OutOfWorld],
                    distance: None,
                    max: None,
                    by: None,
                });
                continue;
            }
        };

        if target_visible {
            seen.push(DirectedSeenBlock {
                at: [x, y, z],
                block: target_info.expect("loaded target has block info"),
            });
            continue;
        }

        let ray = first_occluder_before_target(
            &mut read_cached,
            eye,
            center,
            &target,
            presenters,
            &mut checkpoint,
        )?;
        // outside_fov / too_far / out_of_world 三者中任意一个成立，上面的几何闸门
        // 已经 continue 掉了；到这里它们必为 false。原先这里还各有一次 push，
        // 是死代码——留着会让人误以为 why 可能因视锥或距离而非空，从而看不清
        // 「why 为空 ⟹ target_info 是 Some」这条推理。
        let mut why = Vec::new();
        let mut by = None;
        match ray {
            DirectedRayOutcome::Hit(hit) => {
                why.push(DirectedWhy::Occluded);
                by = Some(DirectedOccluder {
                    at: [hit.voxel.x, hit.voxel.y, hit.voxel.z],
                    block: hit.block,
                });
            }
            DirectedRayOutcome::Unloaded => why.push(DirectedWhy::ChunkNotLoaded),
            DirectedRayOutcome::Clear if target_info.is_some() => why.push(DirectedWhy::Occluded),
            DirectedRayOutcome::Clear => {}
        }
        if target_info.is_none() && !why.contains(&DirectedWhy::ChunkNotLoaded) {
            why.push(DirectedWhy::ChunkNotLoaded);
        }

        if why.is_empty() {
            seen.push(DirectedSeenBlock {
                at: [x, y, z],
                block: target_info.expect("loaded target has block info"),
            });
        } else {
            unseen.push(DirectedUnseenBlock {
                at: [x, y, z],
                why,
                distance: None,
                max: None,
                by,
            });
        }
    }

    let projection = DirectedViewportProjection { seen, unseen };
    projection.validate().map_err(|message| {
        DirectedViewportError::Backend(BackendError::InvalidCommand {
            field: "directed".to_owned(),
            message,
        })
    })?;
    Ok(projection)
}

enum DirectedRayOutcome {
    Hit(BlockHit),
    Clear,
    Unloaded,
}

fn first_occluder_before_target<F, C>(
    read_block: &mut F,
    origin: Point3,
    target: Point3,
    target_voxel: &BlockPosition,
    presenters: &BlockInfoPresenterRegistry,
    checkpoint: &mut C,
) -> Result<DirectedRayOutcome, DirectedViewportError>
where
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), BackendError>,
{
    checkpoint().map_err(DirectedViewportError::Backend)?;
    let delta = subtract(target, origin);
    let distance = length(delta);
    if distance == 0.0 {
        return Ok(DirectedRayOutcome::Clear);
    }
    let direction = normalize(delta, distance);
    let steps = (distance / RAY_STEP).ceil() as i32;
    for step in 1..=steps {
        checkpoint().map_err(DirectedViewportError::Backend)?;
        let travelled = f64::from(step) * RAY_STEP;
        if travelled >= distance {
            break;
        }
        let voxel = BlockPosition {
            x: (origin.x + direction.x * travelled).floor() as i32,
            y: (origin.y + direction.y * travelled).floor() as i32,
            z: (origin.z + direction.z * travelled).floor() as i32,
        };
        if same_voxel(&voxel, target_voxel) {
            break;
        }
        match read_block(voxel.clone()) {
            BlockReadResult::Loaded { block } if is_occluding_block(&block) => {
                return Ok(DirectedRayOutcome::Hit(BlockHit {
                    voxel,
                    block: BlockInfo::from_raw_properties_with_registry(
                        block.name,
                        &block.properties,
                        presenters,
                    ),
                }));
            }
            BlockReadResult::Loaded { .. } => {}
            BlockReadResult::Unloaded => return Ok(DirectedRayOutcome::Unloaded),
            // Match the full kernel's conservative ray boundary: an out-of-world
            // neighbor is not evidence about the queried target and is skipped.
            BlockReadResult::OutOfWorld => {}
        }
    }
    Ok(DirectedRayOutcome::Clear)
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

fn standing_on_block<F, C>(
    read_block: &mut F,
    pose: &PoseSnapshot,
    presenters: &BlockInfoPresenterRegistry,
    checkpoint: &mut C,
) -> Result<Option<ViewportBlock>, BackendError>
where
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), BackendError>,
{
    checkpoint()?;
    let position = BlockPosition {
        x: pose.position.x.floor() as i32,
        y: pose.position.y.floor() as i32 - 1,
        z: pose.position.z.floor() as i32,
    };
    match read_cell(read_block, position.clone(), checkpoint)? {
        BlockCell::Loaded => read_loaded_snapshot(read_block, position, presenters, checkpoint),
        BlockCell::Empty | BlockCell::Unloaded => Ok(None),
    }
}

fn read_loaded_snapshot<F, C>(
    read_block: &mut F,
    position: BlockPosition,
    presenters: &BlockInfoPresenterRegistry,
    checkpoint: &mut C,
) -> Result<Option<ViewportBlock>, BackendError>
where
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), BackendError>,
{
    checkpoint()?;
    Ok(match read_block(position.clone()) {
        BlockReadResult::Loaded { block } if is_visible_block(&block) => Some(ViewportBlock {
            block: BlockInfo::from_raw_properties_with_registry(
                block.name,
                &block.properties,
                presenters,
            ),
            position: [position.x, position.y, position.z],
        }),
        BlockReadResult::Loaded { .. }
        | BlockReadResult::Unloaded
        | BlockReadResult::OutOfWorld => None,
    })
}

fn raycast_looked_at_block<F, C>(
    read_block: &mut F,
    eye: Point3,
    pose: &PoseSnapshot,
    options: &ViewportOptions,
    presenters: &BlockInfoPresenterRegistry,
    checkpoint: &mut C,
) -> Result<Option<ViewportBlock>, BackendError>
where
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), BackendError>,
{
    checkpoint()?;
    let direction = view_axes(pose.yaw, pose.pitch).forward;
    Ok(
        match first_hit(
            read_block,
            eye,
            direction,
            options.looked_at_max_distance,
            RayProperty::Visible,
            presenters,
            checkpoint,
        )? {
            RayOutcome::Hit(BlockHit { voxel, block }) => Some(ViewportBlock {
                block,
                position: [voxel.x, voxel.y, voxel.z],
            }),
            RayOutcome::Clear | RayOutcome::Unloaded => None,
        },
    )
}

fn visible_blocks<F, C>(
    read_block: &mut F,
    pose: &PoseSnapshot,
    eye: Point3,
    axes: ViewAxes,
    options: &ViewportOptions,
    presenters: &BlockInfoPresenterRegistry,
    checkpoint: &mut C,
) -> Result<VisibleBlocksResult, BackendError>
where
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), BackendError>,
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
        checkpoint()?;
        for sz in section_of(lowest.z)..=section_of(highest.z) {
            checkpoint()?;
            for sy in section_of(lowest.y)..=section_of(highest.y) {
                checkpoint()?;
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
                            checkpoint()?;
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
                                    presenters,
                                    checkpoint,
                                )?
                            {
                                continue;
                            }
                            candidates.push((
                                distance,
                                position,
                                BlockInfo::from_raw_properties_with_registry(
                                    block.name,
                                    &block.properties,
                                    presenters,
                                ),
                            ));
                        }
                    }
                }
            }
        }
    }

    candidates.sort_by(compare_candidate);
    let truncated = candidates.len() > options.block_limit;
    let blocks = candidates
        .into_iter()
        .take(options.block_limit)
        .map(|(_, position, block)| (block, position.x, position.y, position.z))
        .collect();
    Ok(VisibleBlocksResult { blocks, truncated })
}

fn visible_entities<F, C>(
    read_block: &mut F,
    entities: &[ProtocolEntitySnapshot],
    eye: Point3,
    axes: ViewAxes,
    options: &ViewportOptions,
    presenters: &BlockInfoPresenterRegistry,
    checkpoint: &mut C,
) -> Result<VisibleEntitiesResult, BackendError>
where
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), BackendError>,
{
    let mut candidates = Vec::new();
    for entity in entities.iter().filter(|entity| entity.valid) {
        checkpoint()?;
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
                checkpoint()?;
                let point_delta = subtract(point, eye);
                if inside_frustum(axes, point_delta, options)
                    && line_is_clear(read_block, eye, point, presenters, checkpoint)?
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
    Ok(VisibleEntitiesResult { items, truncated })
}

fn is_visible_candidate<F, C>(
    read_block: &mut F,
    eye: Point3,
    voxel: &BlockPosition,
    distance: f64,
    predicate: VisibilityPredicate,
    presenters: &BlockInfoPresenterRegistry,
    checkpoint: &mut C,
) -> Result<bool, BackendError>
where
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), BackendError>,
{
    checkpoint()?;
    Ok(match predicate {
        VisibilityPredicate::ExposedFace => {
            exposed_face_reaches_eye(read_block, eye, voxel, presenters, checkpoint)?
        }
        VisibilityPredicate::BlockCentre => {
            has_exposed_face(read_block, voxel, checkpoint)?
                && line_reaches_voxel(read_block, eye, voxel, distance, presenters, checkpoint)?
        }
    })
}

fn exposed_face_reaches_eye<F, C>(
    read_block: &mut F,
    eye: Point3,
    voxel: &BlockPosition,
    presenters: &BlockInfoPresenterRegistry,
    checkpoint: &mut C,
) -> Result<bool, BackendError>
where
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), BackendError>,
{
    let center = Point3 {
        x: f64::from(voxel.x) + 0.5,
        y: f64::from(voxel.y) + 0.5,
        z: f64::from(voxel.z) + 0.5,
    };
    let mut candidates = Vec::new();
    for normal in FACE_NORMALS {
        checkpoint()?;
        let face = add(center, scale(normal, 0.5));
        let to_eye = subtract(eye, face);
        let reach = length(to_eye);
        if reach == 0.0 {
            return Ok(true);
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
        match read_cell(read_block, neighbor.clone(), checkpoint)? {
            BlockCell::Unloaded => continue,
            BlockCell::Loaded if cell_occludes(read_block, neighbor, checkpoint)? => continue,
            BlockCell::Loaded | BlockCell::Empty => {}
        }
        candidates.push((squareness, add(face, scale(normal, FACE_EPSILON))));
    }
    candidates.sort_by(|left, right| right.0.partial_cmp(&left.0).unwrap_or(Ordering::Equal));
    for (_, target) in candidates {
        checkpoint()?;
        if line_is_clear(read_block, eye, target, presenters, checkpoint)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn has_exposed_face<F, C>(
    read_block: &mut F,
    voxel: &BlockPosition,
    checkpoint: &mut C,
) -> Result<bool, BackendError>
where
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), BackendError>,
{
    for normal in FACE_NORMALS {
        checkpoint()?;
        let neighbor = BlockPosition {
            x: voxel.x + normal.x as i32,
            y: voxel.y + normal.y as i32,
            z: voxel.z + normal.z as i32,
        };
        let exposed = match read_cell(read_block, neighbor.clone(), checkpoint)? {
            BlockCell::Unloaded => false,
            BlockCell::Loaded => !cell_occludes(read_block, neighbor, checkpoint)?,
            BlockCell::Empty => true,
        };
        if exposed {
            return Ok(true);
        }
    }
    Ok(false)
}

fn line_reaches_voxel<F, C>(
    read_block: &mut F,
    eye: Point3,
    voxel: &BlockPosition,
    distance: f64,
    presenters: &BlockInfoPresenterRegistry,
    checkpoint: &mut C,
) -> Result<bool, BackendError>
where
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), BackendError>,
{
    checkpoint()?;
    if distance == 0.0 {
        return Ok(true);
    }
    let center = Point3 {
        x: f64::from(voxel.x) + 0.5,
        y: f64::from(voxel.y) + 0.5,
        z: f64::from(voxel.z) + 0.5,
    };
    Ok(
        match first_hit(
            read_block,
            eye,
            normalize(subtract(center, eye), distance),
            distance + RAY_STEP,
            RayProperty::Occludes,
            presenters,
            checkpoint,
        )? {
            RayOutcome::Clear => true,
            RayOutcome::Hit(hit) => same_voxel(&hit.voxel, voxel),
            RayOutcome::Unloaded => false,
        },
    )
}

fn line_is_clear<F, C>(
    read_block: &mut F,
    origin: Point3,
    target: Point3,
    presenters: &BlockInfoPresenterRegistry,
    checkpoint: &mut C,
) -> Result<bool, BackendError>
where
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), BackendError>,
{
    checkpoint()?;
    let delta = subtract(target, origin);
    let distance = length(delta);
    if distance == 0.0 {
        return Ok(true);
    }
    Ok(matches!(
        first_hit(
            read_block,
            origin,
            normalize(delta, distance),
            (distance - RAY_STEP).max(0.0),
            RayProperty::Occludes,
            presenters,
            checkpoint,
        )?,
        RayOutcome::Clear
    ))
}

fn first_hit<F, C>(
    read_block: &mut F,
    origin: Point3,
    direction: Point3,
    max_distance: f64,
    property: RayProperty,
    presenters: &BlockInfoPresenterRegistry,
    checkpoint: &mut C,
) -> Result<RayOutcome, BackendError>
where
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), BackendError>,
{
    let steps = (max_distance / RAY_STEP).floor() as i32;
    for step in 1..=steps {
        checkpoint()?;
        let distance = f64::from(step) * RAY_STEP;
        let voxel = BlockPosition {
            x: (origin.x + direction.x * distance).floor() as i32,
            y: (origin.y + direction.y * distance).floor() as i32,
            z: (origin.z + direction.z * distance).floor() as i32,
        };
        let block = match read_block(voxel.clone()) {
            BlockReadResult::Loaded { block } => block,
            BlockReadResult::OutOfWorld => continue,
            BlockReadResult::Unloaded => return Ok(RayOutcome::Unloaded),
        };
        let hits = match property {
            RayProperty::Visible => is_visible_block(&block),
            RayProperty::Occludes => is_occluding_block(&block),
        };
        if hits {
            return Ok(RayOutcome::Hit(BlockHit {
                voxel,
                block: BlockInfo::from_raw_properties_with_registry(
                    block.name,
                    &block.properties,
                    presenters,
                ),
            }));
        }
    }
    Ok(RayOutcome::Clear)
}

fn read_cell<F, C>(
    read_block: &mut F,
    position: BlockPosition,
    checkpoint: &mut C,
) -> Result<BlockCell, BackendError>
where
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), BackendError>,
{
    checkpoint()?;
    Ok(match read_block(position) {
        BlockReadResult::Loaded { block } => {
            if is_visible_block(&block) {
                BlockCell::Loaded
            } else {
                BlockCell::Empty
            }
        }
        BlockReadResult::OutOfWorld => BlockCell::Empty,
        BlockReadResult::Unloaded => BlockCell::Unloaded,
    })
}

fn cell_occludes<F, C>(
    read_block: &mut F,
    position: BlockPosition,
    checkpoint: &mut C,
) -> Result<bool, BackendError>
where
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), BackendError>,
{
    checkpoint()?;
    Ok(match read_block(position) {
        BlockReadResult::Loaded { block } => is_occluding_block(&block),
        BlockReadResult::Unloaded => true,
        BlockReadResult::OutOfWorld => false,
    })
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
    left: &(f64, BlockPosition, BlockInfo),
    right: &(f64, BlockPosition, BlockInfo),
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
#[path = "viewport_tests.rs"]
mod tests;
