//! 视口投影内核：把"客户端缓存里有一个方块"与"观察者确实能看到这个方块"
//! 分开。读取器只提供绝对坐标上的观察原语，本层做视锥、暴露面和遮挡射线。
//!
//! 迁自旧 backend 的同名机器（几何与优化原样保留，行为测试同迁）；
//! 呈现外壳（BlockInfo 属性白名单、legend 文案、线格式）未迁——
//! 本层交出事实，措辞归渲染层。

use std::{cmp::Ordering, collections::HashMap, f64::consts::PI};

use crate::block::{is_air_name, BlockPosition, BlockProbe, BlockReadResult};
use crate::{wrap_degrees, EntitySnapshot, Vec3Value};

mod geometry;
mod incremental;
mod observed_space;

pub use incremental::{BlockFact, BlockMemory, Known};
pub use observed_space::ObservedSpace;

use geometry::{
    add, box_intersects_frustum, box_visibility_samples, compare_candidate, distance_to_box, dot,
    inside_frustum, length, normalize, point_inside_box, round_one, round_position, same_voxel,
    scale, section_of, subtract, view_axes, AxisAlignedBox, Point3, ViewAxes, FACE_NORMALS,
};

/// 投影期间读世界的两条通道。
///
/// 分成两层是因为量出来的成本分布：一次全量投影要问十几万次「这一格挡不挡
/// 视线」，而真正需要方块名字与属性的最多 `block_limit` 个。让热路径搬完整
/// DTO，等于为两位信息付三次堆分配。
///
/// - `probe`：可 `Copy` 的两位事实，**带缓存**，扫描与射线全走它；
/// - `full`：完整 DTO，**不带缓存**，只在需要把方块交给读方时调用。
pub struct WorldReader<P, F> {
    probe: P,
    full: F,
}

impl<P, F> WorldReader<P, F>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
{
    pub fn new(probe: P, full: F) -> Self {
        Self { probe, full }
    }

    fn probe(&mut self, position: BlockPosition) -> BlockProbe {
        (self.probe)(position)
    }

    fn full(&mut self, position: BlockPosition) -> BlockReadResult {
        (self.full)(position)
    }
}

/// 第一人称眼睛高度。
pub const EYE_HEIGHT: f64 = 1.62;
pub(crate) const SECTION_SIZE: i32 = 16;
const RAY_STEP: f64 = 0.25;
const FACE_EPSILON: f64 = 0.01;
const DEFAULT_VERTICAL_HALF_ANGLE: f64 = 35.0 * PI / 180.0;
const DEFAULT_ASPECT_RATIO: f64 = 16.0 / 9.0;

/// 视口失败：参数非法或检查点（取消/超时）触发。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewportError {
    pub field: String,
    pub message: String,
}

impl ViewportError {
    fn new(field: &str, message: impl Into<String>) -> Self {
        Self {
            field: field.to_owned(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ViewportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for ViewportError {}

/// 可见方块候选的几何谓词。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VisibilityPredicate {
    /// 旧的中心射线基线；保留用于对照，不作为生产默认值。
    BlockCentre,
    /// 至少一个朝向观察者的暴露面可被射线到达。
    #[default]
    ExposedFace,
}

/// 投影参数。
#[derive(Clone, Debug, PartialEq)]
pub struct ViewportOptions {
    pub horizontal_radius: i32,
    pub vertical_radius: i32,
    pub max_distance: f64,
    pub vertical_half_angle: f64,
    pub horizontal_half_angle: f64,
    pub block_limit: usize,
    pub entity_limit: usize,
    pub predicate: VisibilityPredicate,
}

impl ViewportOptions {
    /// 给**记忆**用的参数：判据不变，预算放开。
    ///
    /// `block_limit` 是**呈现**预算——模型读不了一万行，所以默认只留最近的 256 格。
    /// 记忆没有这个问题：它是给机器读的（寻路），一次吸多少只影响内存与 CPU，
    /// 不影响可读性。此前眼睛走的是默认参数，于是**远处看得见的
    /// 方块从来没被记住过**——那是纯粹的损失。
    ///
    /// 视锥角度与遮挡判据**照旧不动**：那是合法性本身，放开它就等于让同伴看见它没
    /// 看的地方。放开的只有「一次记多少」。
    pub fn for_memory() -> Self {
        Self {
            block_limit: 65_536,
            ..Self::default()
        }
    }
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
        }
    }
}

impl ViewportOptions {
    /// 检查投影参数，避免无界扫描或无效三角函数。
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
        ] {
            if !value.is_finite() || value <= 0.0 {
                return Err(format!("viewport {name} 必须是正的有限数"));
            }
        }
        if self.vertical_half_angle >= PI / 2.0 || self.horizontal_half_angle >= PI / 2.0 {
            return Err("viewport 视锥半角必须小于 90 度".to_owned());
        }
        // 上限是防误用的护栏（模型不直接传视口参数，所以只挡内部误用）。记忆那条路要把整个可见集收进来，4 096 挡得住它——
        // 抬到 64 Ki，够一次 160 格视野的量级，同时仍然拦得住离谱值。
        if self.block_limit > 65_536 || self.entity_limit > 256 {
            return Err("viewport 结果上限过大".to_owned());
        }
        Ok(())
    }
}

/// 投影用的观察者姿态。角度制；yaw 未归一化也可以（三角函数是周期的）。
#[derive(Clone, Debug, PartialEq)]
pub struct Pose {
    pub position: Vec3Value,
    pub yaw: f64,
    pub pitch: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ViewportPose {
    pub position: [f64; 3],
    /// 已归一化到 [−180, 180)：给读方看的角度在此处一次归一。
    pub yaw_degrees: f64,
    pub pitch_degrees: f64,
}

/// 交给读方的方块事实。`state_id` 是这次观察到的注册表状态；名字与属性负责
/// 呈现，它负责让动作规划忠于最后所见的碰撞体，而不是离屏偷读 live world。
#[derive(Clone, Debug, PartialEq)]
pub struct ViewportBlock {
    pub name: String,
    pub state_id: u32,
    pub properties: std::collections::BTreeMap<String, String>,
    pub position: [i32; 3],
}

#[derive(Clone, Debug, PartialEq)]
pub struct VisibleEntity {
    pub entity_type: String,
    pub player: Option<String>,
    pub position: [f64; 3],
}

#[derive(Clone, Debug, PartialEq)]
pub struct VisibleEntitiesResult {
    pub items: Vec<VisibleEntity>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VisibleBlocksResult {
    /// 按距离从近到远；`truncated` 为真表示更远处还有可见方块没列出。
    pub blocks: Vec<ViewportBlock>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ViewportProjection {
    pub pose: ViewportPose,
    pub standing_on_block: Option<ViewportBlock>,
    pub looked_at_block: Option<ViewportBlock>,
    pub visible_entities: VisibleEntitiesResult,
    pub visible_blocks: VisibleBlocksResult,
}

#[derive(Clone, Copy, Debug)]
enum BlockCell {
    Loaded,
    Empty,
    Unloaded,
}

#[derive(Clone, Copy, Debug)]
enum RayProperty {
    Occludes,
}

/// 全量投影内部射线的结果。只带命中坐标，不带方块身份：遮挡测试在碎地形里
/// 每次投影要跑上万条，需要身份的只有注视射线，它拿到坐标后自己读一次。
enum RayOutcome {
    Hit(BlockPosition),
    Clear,
    Unloaded,
}

/// 单读取器便捷入口：探针由完整 DTO 折出。
///
/// 只适合测试与合成读取器——生产走 [`project_with_reader`] 并自带廉价探针。
pub fn project<F>(
    pose: &Pose,
    entities: &[EntitySnapshot],
    read_block: F,
    options: &ViewportOptions,
) -> Result<ViewportProjection, String>
where
    F: Fn(BlockPosition) -> BlockReadResult,
{
    project_with_reader(
        pose,
        entities,
        WorldReader::new(
            |position| BlockProbe::from_read(&read_block(position)),
            &read_block,
        ),
        options,
        || Ok(()),
    )
    .map_err(|error| error.message)
}

/// 完整投影入口。检查点在每个昂贵几何阶段与每次方块/射线读取前运行，
/// 出错立即退出扫描（取消/超时的挂点）。
pub fn project_with_reader<P, F, C>(
    pose: &Pose,
    entities: &[EntitySnapshot],
    reader: WorldReader<P, F>,
    options: &ViewportOptions,
    checkpoint: C,
) -> Result<ViewportProjection, ViewportError>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
{
    project_inner(pose, entities, reader, options, checkpoint, None)
}

/// 投影，并把射线走过的空格记进 [`ObservedSpace`]。
///
/// 与 [`project_with_reader`] 同一次投影、同一份探针缓存：候选扫描已经把视锥里
/// 每一格都探过了，标记这一趟几乎全是缓存命中。判据见 [`observe_free_space`]。
pub fn project_observing<P, F, C>(
    pose: &Pose,
    entities: &[EntitySnapshot],
    reader: WorldReader<P, F>,
    options: &ViewportOptions,
    checkpoint: C,
    space: &mut ObservedSpace,
) -> Result<ViewportProjection, ViewportError>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
{
    project_inner(pose, entities, reader, options, checkpoint, Some(space))
}

fn project_inner<P, F, C>(
    pose: &Pose,
    entities: &[EntitySnapshot],
    reader: WorldReader<P, F>,
    options: &ViewportOptions,
    mut checkpoint: C,
    space: Option<&mut ObservedSpace>,
) -> Result<ViewportProjection, ViewportError>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
{
    options
        .validate()
        .map_err(|message| ViewportError::new("viewport", message))?;
    checkpoint()?;
    // 一个投影会反复读取同一体素：候选扫描、暴露面邻居和多条射线都会经过它。
    // 缓存的是**探针**而不是完整 DTO——探针是 `Copy` 的，命中不分配。
    let mut probe_cache = HashMap::<(i32, i32, i32), BlockProbe>::new();
    let WorldReader { probe, full } = reader;
    let mut probe = probe;
    let mut reader = WorldReader::new(
        move |position: BlockPosition| {
            let key = (position.x, position.y, position.z);
            if let Some(probe) = probe_cache.get(&key) {
                return *probe;
            }
            let result = probe(position);
            probe_cache.insert(key, result);
            result
        },
        full,
    );
    let eye = Point3 {
        x: pose.position.x,
        y: pose.position.y + EYE_HEIGHT,
        z: pose.position.z,
    };
    let axes = view_axes(pose.yaw, pose.pitch);
    let standing_on_block = standing_on_block(&mut reader, pose, &mut checkpoint)?;
    let looked_at_block =
        raycast_looked_at_block(&mut reader, eye, pose, options, &mut checkpoint)?;
    let visible_entities =
        visible_entities(&mut reader, entities, eye, axes, options, &mut checkpoint)?;
    let visible_blocks = visible_blocks(&mut reader, pose, eye, axes, options, &mut checkpoint)?;
    if let Some(space) = space {
        observe_free_space(
            &mut reader,
            eye,
            &visible_blocks.blocks,
            looked_at_block.as_ref(),
            space,
            &mut checkpoint,
        )?;
    }

    Ok(ViewportProjection {
        pose: ViewportPose {
            position: round_position(Point3 {
                x: pose.position.x,
                y: pose.position.y,
                z: pose.position.z,
            }),
            // 归一化只在给读方看的这一处做：视锥/遮挡的三角函数是周期的，
            // 不在乎转了多少圈；读方在乎。
            yaw_degrees: round_one(wrap_degrees(pose.yaw)),
            pitch_degrees: round_one(pose.pitch),
        },
        standing_on_block,
        looked_at_block,
        visible_entities,
        visible_blocks,
    })
}

fn standing_on_block<P, F, C>(
    reader: &mut WorldReader<P, F>,
    pose: &Pose,
    checkpoint: &mut C,
) -> Result<Option<ViewportBlock>, ViewportError>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
{
    checkpoint()?;
    let position = BlockPosition {
        x: pose.position.x.floor() as i32,
        y: pose.position.y.floor() as i32 - 1,
        z: pose.position.z.floor() as i32,
    };
    match read_cell(reader, position.clone(), checkpoint)? {
        BlockCell::Loaded => read_loaded_snapshot(reader, position, checkpoint),
        BlockCell::Empty | BlockCell::Unloaded => Ok(None),
    }
}

fn read_loaded_snapshot<P, F, C>(
    reader: &mut WorldReader<P, F>,
    position: BlockPosition,
    checkpoint: &mut C,
) -> Result<Option<ViewportBlock>, ViewportError>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
{
    checkpoint()?;
    // 这一格是要交给读方的（脚下 / 注视），必须走完整 DTO。
    Ok(match reader.full(position.clone()) {
        BlockReadResult::Loaded { block } if !is_air_name(&block.name) => Some(ViewportBlock {
            name: block.name,
            state_id: block.state_id,
            properties: block.properties,
            position: [position.x, position.y, position.z],
        }),
        BlockReadResult::Loaded { .. }
        | BlockReadResult::Unloaded
        | BlockReadResult::OutOfWorld => None,
    })
}

/// 准星落点：视线方向第一个非空气方块；一路空气则
/// 报**视线尽头的那格空气**——穿出世界高度（看天）= 最高一格在界内的空气，
/// 走到扫描盒边界 = 边界上的那格空气，撞上未加载区 = 已知边界的最后一格
/// 空气（认知边界如实呈现）。准星因此几乎总有落点；仅当第一步就出界/未加载
/// （异常姿态）才为 None。
fn raycast_looked_at_block<P, F, C>(
    reader: &mut WorldReader<P, F>,
    eye: Point3,
    pose: &Pose,
    options: &ViewportOptions,
    checkpoint: &mut C,
) -> Result<Option<ViewportBlock>, ViewportError>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
{
    checkpoint()?;
    let direction = view_axes(pose.yaw, pose.pitch).forward;
    let self_voxel = BlockPosition {
        x: pose.position.x.floor() as i32,
        y: pose.position.y.floor() as i32,
        z: pose.position.z.floor() as i32,
    };
    let in_box = |voxel: &BlockPosition| -> bool {
        (voxel.x - self_voxel.x).abs() <= options.horizontal_radius
            && (voxel.y - self_voxel.y).abs() <= options.vertical_radius
            && (voxel.z - self_voxel.z).abs() <= options.horizontal_radius
    };
    // 对角线穿盒的最长路径；逐步走，出盒即止。
    let max_distance = f64::from(options.horizontal_radius.max(options.vertical_radius)) * 2.0;
    let steps = (max_distance / RAY_STEP).ceil() as i32;
    let mut last_air: Option<BlockPosition> = None;
    let mut terminal: Option<BlockPosition> = None;
    for step in 1..=steps {
        checkpoint()?;
        let distance = f64::from(step) * RAY_STEP;
        let voxel = BlockPosition {
            x: (eye.x + direction.x * distance).floor() as i32,
            y: (eye.y + direction.y * distance).floor() as i32,
            z: (eye.z + direction.z * distance).floor() as i32,
        };
        if !in_box(&voxel) {
            break; // 扫描盒边界：落点=最后一格空气。
        }
        match reader.probe(voxel.clone()) {
            BlockProbe::OutOfWorld => break, // 穿出世界高度（看天/看虚空）。
            BlockProbe::Unloaded => break,   // 认知边界。
            BlockProbe::Loaded { visible: true, .. } => {
                terminal = Some(voxel);
                break; // 第一个非空气方块。
            }
            BlockProbe::Loaded { visible: false, .. } => {
                last_air = Some(voxel);
            }
        }
    }
    let landing = terminal.or(last_air);
    Ok(landing.and_then(|voxel| match reader.full(voxel.clone()) {
        // 注视的方块要交给读方，这里才付完整 DTO 的钱——一次投影一次。
        BlockReadResult::Loaded { block } => Some(ViewportBlock {
            name: block.name,
            state_id: block.state_id,
            properties: block.properties,
            position: [voxel.x, voxel.y, voxel.z],
        }),
        // 探针可读、完整读拿不到：两次读之间世界变了。不报读不出来的方块。
        BlockReadResult::Unloaded | BlockReadResult::OutOfWorld => None,
    }))
}

fn visible_blocks<P, F, C>(
    reader: &mut WorldReader<P, F>,
    pose: &Pose,
    eye: Point3,
    axes: ViewAxes,
    options: &ViewportOptions,
    checkpoint: &mut C,
) -> Result<VisibleBlocksResult, ViewportError>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
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

    // 先用 section AABB 做保守剔除，避免对背后的方块执行体素级射线。
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
                            let BlockProbe::Loaded { visible: true, .. } =
                                reader.probe(position.clone())
                            else {
                                continue;
                            };
                            if !is_visible_candidate(
                                reader,
                                eye,
                                &position,
                                distance,
                                options.predicate,
                                checkpoint,
                            )? {
                                continue;
                            }
                            candidates.push((distance, position));
                        }
                    }
                }
            }
        }
    }

    candidates.sort_by(compare_candidate);
    let truncated = candidates.len() > options.block_limit;
    // 排序截断之后才付完整 DTO 的钱：可见候选可以远多于 `block_limit`，
    // 先建再扔等于为扔掉的方块付全价。排序只用距离与坐标，延后不改次序。
    let mut blocks = Vec::with_capacity(candidates.len().min(options.block_limit));
    for (_, position) in candidates.into_iter().take(options.block_limit) {
        checkpoint()?;
        let BlockReadResult::Loaded { block } = reader.full(position.clone()) else {
            // 探针判定可见、完整读却拿不到：只可能是两次读之间世界变了。
            // 略过它，不把读不出来的方块报给读方。
            continue;
        };
        blocks.push(ViewportBlock {
            name: block.name,
            state_id: block.state_id,
            properties: block.properties,
            position: [position.x, position.y, position.z],
        });
    }
    Ok(VisibleBlocksResult { blocks, truncated })
}

fn visible_entities<P, F, C>(
    reader: &mut WorldReader<P, F>,
    entities: &[EntitySnapshot],
    eye: Point3,
    axes: ViewAxes,
    options: &ViewportOptions,
    checkpoint: &mut C,
) -> Result<VisibleEntitiesResult, ViewportError>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
{
    let mut candidates = Vec::new();
    for entity in entities.iter().filter(|entity| entity.valid) {
        checkpoint()?;
        let width = entity.width.max(0.01);
        let height = entity.height.max(0.01);
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
                    && line_is_clear(reader, eye, point, checkpoint)?
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

fn is_visible_candidate<P, F, C>(
    reader: &mut WorldReader<P, F>,
    eye: Point3,
    voxel: &BlockPosition,
    distance: f64,
    predicate: VisibilityPredicate,
    checkpoint: &mut C,
) -> Result<bool, ViewportError>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
{
    checkpoint()?;
    Ok(match predicate {
        VisibilityPredicate::ExposedFace => {
            exposed_face_reaches_eye(reader, eye, voxel, checkpoint)?
        }
        VisibilityPredicate::BlockCentre => {
            has_exposed_face(reader, voxel, checkpoint)?
                && line_reaches_voxel(reader, eye, voxel, distance, checkpoint)?
        }
    })
}

fn exposed_face_reaches_eye<P, F, C>(
    reader: &mut WorldReader<P, F>,
    eye: Point3,
    voxel: &BlockPosition,
    checkpoint: &mut C,
) -> Result<bool, ViewportError>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
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
        match read_cell(reader, neighbor.clone(), checkpoint)? {
            BlockCell::Unloaded => continue,
            BlockCell::Loaded if cell_occludes(reader, neighbor, checkpoint)? => continue,
            BlockCell::Loaded | BlockCell::Empty => {}
        }
        candidates.push((squareness, add(face, scale(normal, FACE_EPSILON))));
    }
    candidates.sort_by(|left, right| right.0.partial_cmp(&left.0).unwrap_or(Ordering::Equal));
    for (_, target) in candidates {
        checkpoint()?;
        if line_is_clear(reader, eye, target, checkpoint)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn has_exposed_face<P, F, C>(
    reader: &mut WorldReader<P, F>,
    voxel: &BlockPosition,
    checkpoint: &mut C,
) -> Result<bool, ViewportError>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
{
    for normal in FACE_NORMALS {
        checkpoint()?;
        let neighbor = BlockPosition {
            x: voxel.x + normal.x as i32,
            y: voxel.y + normal.y as i32,
            z: voxel.z + normal.z as i32,
        };
        let exposed = match read_cell(reader, neighbor.clone(), checkpoint)? {
            BlockCell::Unloaded => false,
            BlockCell::Loaded => !cell_occludes(reader, neighbor, checkpoint)?,
            BlockCell::Empty => true,
        };
        if exposed {
            return Ok(true);
        }
    }
    Ok(false)
}

fn line_reaches_voxel<P, F, C>(
    reader: &mut WorldReader<P, F>,
    eye: Point3,
    voxel: &BlockPosition,
    distance: f64,
    checkpoint: &mut C,
) -> Result<bool, ViewportError>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
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
            reader,
            eye,
            normalize(subtract(center, eye), distance),
            distance + RAY_STEP,
            RayProperty::Occludes,
            checkpoint,
        )? {
            RayOutcome::Clear => true,
            RayOutcome::Hit(hit) => same_voxel(&hit, voxel),
            RayOutcome::Unloaded => false,
        },
    )
}

fn line_is_clear<P, F, C>(
    reader: &mut WorldReader<P, F>,
    origin: Point3,
    target: Point3,
    checkpoint: &mut C,
) -> Result<bool, ViewportError>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
{
    checkpoint()?;
    let delta = subtract(target, origin);
    let distance = length(delta);
    if distance == 0.0 {
        return Ok(true);
    }
    Ok(matches!(
        first_hit(
            reader,
            origin,
            normalize(delta, distance),
            (distance - RAY_STEP).max(0.0),
            RayProperty::Occludes,
            checkpoint,
        )?,
        RayOutcome::Clear
    ))
}

/// 把「通向已见方块的射线上走过的空格」记进 [`ObservedSpace`]。
///
/// 独立一趟，不挂在热路径上：`visible_blocks` 一次投影要问十几万次探针，
/// 而本函数是**每个已见方块一条射线**（几百条量级），加起来只有百分之几。
/// 换来的是热路径那五层泛型一行不用改。
///
/// 判据：从眼睛朝方块中心步进，**撞到第一个非空气格就停**，只标记它之前的空格。
/// 于是标记的每一格都满足「眼睛到它之间全空」——这正是看得见的判据本身，
/// 因此永远不会多标。可见性本身是由暴露面射线定的（射向面，不是射向中心），
/// 中心线可能被挡；被挡就在挡住的地方停下，这一支自然什么都不标。
///
/// 开放方向由准星的 AIR 终点补一条射线，因此看天或空走廊也能学习自由空间。
/// 仍有意保守的一处是玻璃、水、树叶这类**透光但非空气**的格会让步进停下，
/// 它们身后的空不被标记。少标的后果是「还没看过」，多标的后果是让同伴知道
/// 它没看过的事——两者不对等。
fn observe_free_space<P, F, C>(
    reader: &mut WorldReader<P, F>,
    eye: Point3,
    blocks: &[ViewportBlock],
    looked_at: Option<&ViewportBlock>,
    space: &mut ObservedSpace,
    checkpoint: &mut C,
) -> Result<(), ViewportError>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
{
    // 准星射线也必须上账。开放方向的终点常是 AIR，不会出现在 visible_blocks；
    // 没有这条射线，“看向空地/天空”反而一格自由空间都学不到。
    for block in blocks.iter().chain(looked_at) {
        checkpoint()?;
        let center = Point3 {
            x: f64::from(block.position[0]) + 0.5,
            y: f64::from(block.position[1]) + 0.5,
            z: f64::from(block.position[2]) + 0.5,
        };
        let delta = subtract(center, eye);
        let distance = length(delta);
        if distance == 0.0 {
            continue;
        }
        let direction = normalize(delta, distance);
        let steps = (distance / RAY_STEP).floor() as i32;
        for step in 1..=steps {
            checkpoint()?;
            let along = f64::from(step) * RAY_STEP;
            let voxel = BlockPosition {
                x: (eye.x + direction.x * along).floor() as i32,
                y: (eye.y + direction.y * along).floor() as i32,
                z: (eye.z + direction.z * along).floor() as i32,
            };
            match reader.probe(voxel.clone()) {
                // 空气：这一格看过了，而且是空的。
                BlockProbe::Loaded { visible: false, .. } => {
                    space.mark([voxel.x, voxel.y, voxel.z]);
                }
                // 有东西：射线到此为止，身后的空一律不标。
                BlockProbe::Loaded { visible: true, .. } => break,
                // 读不到就停：没加载的地方不能声称看过。
                BlockProbe::Unloaded => break,
                BlockProbe::OutOfWorld => continue,
            }
        }
    }
    Ok(())
}

fn first_hit<P, F, C>(
    reader: &mut WorldReader<P, F>,
    origin: Point3,
    direction: Point3,
    max_distance: f64,
    property: RayProperty,
    checkpoint: &mut C,
) -> Result<RayOutcome, ViewportError>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
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
        let (visible, transparent_hint) = match reader.probe(voxel.clone()) {
            BlockProbe::Loaded {
                visible,
                transparent_hint,
            } => (visible, transparent_hint),
            BlockProbe::OutOfWorld => continue,
            BlockProbe::Unloaded => return Ok(RayOutcome::Unloaded),
        };
        let hits = match property {
            RayProperty::Occludes => visible && !transparent_hint,
        };
        if hits {
            return Ok(RayOutcome::Hit(voxel));
        }
    }
    Ok(RayOutcome::Clear)
}

fn read_cell<P, F, C>(
    reader: &mut WorldReader<P, F>,
    position: BlockPosition,
    checkpoint: &mut C,
) -> Result<BlockCell, ViewportError>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
{
    checkpoint()?;
    Ok(match reader.probe(position) {
        BlockProbe::Loaded { visible: true, .. } => BlockCell::Loaded,
        BlockProbe::Loaded { visible: false, .. } => BlockCell::Empty,
        BlockProbe::OutOfWorld => BlockCell::Empty,
        BlockProbe::Unloaded => BlockCell::Unloaded,
    })
}

fn cell_occludes<P, F, C>(
    reader: &mut WorldReader<P, F>,
    position: BlockPosition,
    checkpoint: &mut C,
) -> Result<bool, ViewportError>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
{
    checkpoint()?;
    Ok(match reader.probe(position) {
        BlockProbe::Loaded {
            visible,
            transparent_hint,
        } => visible && !transparent_hint,
        BlockProbe::Unloaded => true,
        BlockProbe::OutOfWorld => false,
    })
}

#[cfg(test)]
#[path = "../viewport_tests.rs"]
mod tests;
