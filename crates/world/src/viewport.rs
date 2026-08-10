//! 视口投影内核：把"客户端缓存里有一个方块"与"观察者确实能看到这个方块"
//! 分开。读取器只提供绝对坐标上的观察原语，本层做视锥、暴露面和遮挡射线。
//!
//! 迁自旧 backend 的同名机器（几何与优化原样保留，行为测试同迁）；
//! 呈现外壳（BlockInfo 属性白名单、legend 文案、线格式）未迁——
//! 本层交出事实，措辞归渲染层。

use std::{cmp::Ordering, collections::HashMap, f64::consts::PI};

use crate::block::{is_air_name, BlockPosition, BlockProbe, BlockReadResult};
use crate::{wrap_degrees, EntitySnapshot, Vec3Value};

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
const SECTION_SIZE: i32 = 16;
const RAY_STEP: f64 = 0.25;
const FACE_EPSILON: f64 = 0.01;
const DEFAULT_VERTICAL_HALF_ANGLE: f64 = 35.0 * PI / 180.0;
const DEFAULT_ASPECT_RATIO: f64 = 16.0 / 9.0;
const DEFAULT_LOOKED_AT_DISTANCE: f64 = 4.5;
const DIRECTED_MAX_DISTANCE: f64 = 32.0;
pub const MAX_DIRECTED_VIEW_POSITIONS: usize = 16;

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

/// 世界高度边界。定向几何在读取世界前用它分类出界目标；
/// 上界用 `i64` 计算，任意 `i32` 查询坐标都安全。
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

/// 交给读方的方块事实：名字 + 全部原始属性。挑哪些属性示人归渲染层。
#[derive(Clone, Debug, PartialEq)]
pub struct ViewportBlock {
    pub name: String,
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
    properties: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug)]
enum RayProperty {
    Visible,
    Occludes,
}

/// 全量投影内部射线的结果。只带命中坐标，不带方块身份：遮挡测试在碎地形里
/// 每次投影要跑上万条，需要身份的只有注视射线，它拿到坐标后自己读一次。
enum RayOutcome {
    Hit(BlockPosition),
    Clear,
    Unloaded,
}

const FACE_NORMALS: [Point3; 6] = [
    Point3 { x: 1.0, y: 0.0, z: 0.0 },
    Point3 { x: -1.0, y: 0.0, z: 0.0 },
    Point3 { x: 0.0, y: 1.0, z: 0.0 },
    Point3 { x: 0.0, y: -1.0, z: 0.0 },
    Point3 { x: 0.0, y: 0.0, z: 1.0 },
    Point3 { x: 0.0, y: 0.0, z: -1.0 },
];

// ---- 定向投影结果 ----

/// 目标不可见的原因，报告固定按此规范顺序排列。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

#[derive(Clone, Debug, PartialEq)]
pub struct DirectedOccluder {
    pub at: [i32; 3],
    pub name: String,
    pub properties: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DirectedSeenBlock {
    pub at: [i32; 3],
    pub name: String,
    pub properties: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DirectedUnseenBlock {
    pub at: [i32; 3],
    pub why: Vec<DirectedWhy>,
    /// 仅与 TooFar 同现。
    pub distance: Option<f64>,
    pub max: Option<f64>,
    /// 仅与 Occluded 同现；OutOfWorld 行必无。
    pub by: Option<DirectedOccluder>,
}

impl DirectedUnseenBlock {
    pub fn validate(&self) -> Result<(), String> {
        if self.why.is_empty() {
            return Err("directed unseen why 不得为空".to_owned());
        }
        for pair in self.why.windows(2) {
            if pair[0].rank() >= pair[1].rank() {
                return Err("directed unseen why 必须去重并按规范顺序".to_owned());
            }
        }
        let has_too_far = self.why.contains(&DirectedWhy::TooFar);
        match (has_too_far, self.distance, self.max) {
            (true, Some(distance), Some(max)) if distance.is_finite() && max.is_finite() => {
                if max <= 0.0 || distance <= max {
                    return Err("too_far 要求有限 distance 大于 max".to_owned());
                }
            }
            (true, _, _) => {
                return Err("too_far 要求同时给出 distance 与 max".to_owned());
            }
            (false, None, None) => {}
            (false, _, _) => {
                return Err("distance 与 max 只在 too_far 时有效".to_owned());
            }
        }
        if !self.why.contains(&DirectedWhy::Occluded) && self.by.is_some() {
            return Err("by 只在 occluded 时有效".to_owned());
        }
        if self.why.contains(&DirectedWhy::OutOfWorld) && self.by.is_some() {
            return Err("out_of_world 行不得携带 by".to_owned());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DirectedProjection {
    pub seen: Vec<DirectedSeenBlock>,
    pub unseen: Vec<DirectedUnseenBlock>,
}

impl DirectedProjection {
    pub fn validate(&self) -> Result<(), String> {
        if self.seen.len().saturating_add(self.unseen.len()) > MAX_DIRECTED_VIEW_POSITIONS {
            return Err(format!(
                "定向结果最多 {MAX_DIRECTED_VIEW_POSITIONS} 个位置"
            ));
        }
        let mut coordinates = std::collections::HashSet::new();
        for item in &self.seen {
            if !coordinates.insert(item.at) {
                return Err("定向结果含重复坐标".to_owned());
            }
        }
        for item in &self.unseen {
            item.validate()?;
            if !coordinates.insert(item.at) {
                return Err("定向结果含重复坐标".to_owned());
            }
        }
        Ok(())
    }
}

/// 定向输入边界：非空、至多 16 个、无重复。
pub fn validate_directed_positions(positions: &[[i32; 3]]) -> Result<(), String> {
    if positions.is_empty() {
        return Err("定向观察至少需要一个位置".to_owned());
    }
    if positions.len() > MAX_DIRECTED_VIEW_POSITIONS {
        return Err(format!(
            "定向观察最多接受 {MAX_DIRECTED_VIEW_POSITIONS} 个位置"
        ));
    }
    let mut unique = std::collections::HashSet::with_capacity(positions.len());
    if positions.iter().copied().all(|position| unique.insert(position)) {
        Ok(())
    } else {
        Err("定向观察位置不得重复".to_owned())
    }
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
    mut checkpoint: C,
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
    let looked_at_block = raycast_looked_at_block(&mut reader, eye, pose, options, &mut checkpoint)?;
    let visible_entities = visible_entities(&mut reader, entities, eye, axes, options, &mut checkpoint)?;
    let visible_blocks = visible_blocks(&mut reader, pose, eye, axes, options, &mut checkpoint)?;

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

/// 定向投影便捷入口，约束同 [`project`]。
pub fn project_directed<F, C>(
    pose: &Pose,
    positions: &[[i32; 3]],
    read_block: F,
    options: &ViewportOptions,
    world_bounds: WorldHeightBounds,
    checkpoint: C,
) -> Result<DirectedProjection, ViewportError>
where
    F: Fn(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
{
    project_directed_with_reader(
        pose,
        positions,
        WorldReader::new(
            |position| BlockProbe::from_read(&read_block(position)),
            &read_block,
        ),
        options,
        world_bounds,
        checkpoint,
    )
}

/// 定向投影：与全量投影同一台内核。捕获的高度边界让出界目标零读取分类；
/// 目标读取独立返回 `OutOfWorld` 时成为模型可见原因。
pub fn project_directed_with_reader<P, F, C>(
    pose: &Pose,
    positions: &[[i32; 3]],
    reader: WorldReader<P, F>,
    options: &ViewportOptions,
    world_bounds: WorldHeightBounds,
    mut checkpoint: C,
) -> Result<DirectedProjection, ViewportError>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
{
    validate_directed_positions(positions)
        .map_err(|message| ViewportError::new("positions", message))?;
    options
        .validate()
        .map_err(|message| ViewportError::new("viewport", message))?;
    checkpoint()?;

    let eye = Point3 {
        x: pose.position.x,
        y: pose.position.y + EYE_HEIGHT,
        z: pose.position.z,
    };
    let axes = view_axes(pose.yaw, pose.pitch);
    // 缓存探针而非完整 DTO，理由同全量投影。
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
    let directed_max_distance = options.max_distance.min(DIRECTED_MAX_DISTANCE);
    let mut seen = Vec::new();
    let mut unseen = Vec::new();

    for [x, y, z] in positions.iter().copied() {
        checkpoint()?;
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

        // 几何是硬的隐私与工作量边界：视锥外或超距的目标不读世界、不追射线
        // 就分类完毕，任意 i32 坐标都是 O(1) 工作量。
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

        // 定向投影每个目标都要报出方块身份，所以这里必须走完整 DTO；
        // 目标个数已由 `validate_directed_positions` 限住，不是热路径。
        let target_result = reader.full(target.clone());
        // 一次 match 同时定出「这块方块的身份」与「看不看得见」，
        // 使「同一个不可变值在两次 match 之间不变」的不变量无需被相信。
        let (target_identity, target_visible) = match &target_result {
            BlockReadResult::Loaded { block } => {
                let identity = (block.name.clone(), block.properties.clone());
                let visible = is_visible_candidate(
                    &mut reader,
                    eye,
                    &target,
                    distance,
                    options.predicate,
                    &mut checkpoint,
                )?;
                (Some(identity), visible)
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
            let (name, properties) = target_identity.expect("已加载目标必有方块身份");
            seen.push(DirectedSeenBlock {
                at: [x, y, z],
                name,
                properties,
            });
            continue;
        }

        let ray = first_occluder_before_target(&mut reader, eye, center, &target, &mut checkpoint)?;
        // 几何闸门已经 continue 掉三种原因；到这里它们必为 false。
        let mut why = Vec::new();
        let mut by = None;
        match ray {
            DirectedRayOutcome::Hit(hit) => {
                why.push(DirectedWhy::Occluded);
                by = Some(DirectedOccluder {
                    at: [hit.voxel.x, hit.voxel.y, hit.voxel.z],
                    name: hit.name,
                    properties: hit.properties,
                });
            }
            DirectedRayOutcome::Unloaded => why.push(DirectedWhy::ChunkNotLoaded),
            DirectedRayOutcome::Clear if target_identity.is_some() => {
                why.push(DirectedWhy::Occluded)
            }
            DirectedRayOutcome::Clear => {}
        }
        if target_identity.is_none() && !why.contains(&DirectedWhy::ChunkNotLoaded) {
            why.push(DirectedWhy::ChunkNotLoaded);
        }

        if why.is_empty() {
            let (name, properties) = target_identity.expect("已加载目标必有方块身份");
            seen.push(DirectedSeenBlock {
                at: [x, y, z],
                name,
                properties,
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

    let projection = DirectedProjection { seen, unseen };
    projection
        .validate()
        .map_err(|message| ViewportError::new("directed", message))?;
    Ok(projection)
}

enum DirectedRayOutcome {
    Hit(BlockHit),
    Clear,
    Unloaded,
}

fn first_occluder_before_target<P, F, C>(
    reader: &mut WorldReader<P, F>,
    origin: Point3,
    target: Point3,
    target_voxel: &BlockPosition,
    checkpoint: &mut C,
) -> Result<DirectedRayOutcome, ViewportError>
where
    P: FnMut(BlockPosition) -> BlockProbe,
    F: FnMut(BlockPosition) -> BlockReadResult,
    C: FnMut() -> Result<(), ViewportError>,
{
    checkpoint()?;
    let delta = subtract(target, origin);
    let distance = length(delta);
    if distance == 0.0 {
        return Ok(DirectedRayOutcome::Clear);
    }
    let direction = normalize(delta, distance);
    let steps = (distance / RAY_STEP).ceil() as i32;
    for step in 1..=steps {
        checkpoint()?;
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
        match reader.probe(voxel.clone()) {
            BlockProbe::Loaded {
                visible: true,
                transparent_hint: false,
            } => {
                // 射线每一步都要问「挡不挡」，但只有命中的那一步会被报出去。
                // 所以只在这里才付完整 DTO 的钱。
                let BlockReadResult::Loaded { block } = reader.full(voxel.clone()) else {
                    // 探针说这里有不透光方块，完整读却拿不到——只可能是两次读
                    // 之间世界变了。按"没挡住"继续，宁可少报遮挡也不报一个读不
                    // 出来的方块。
                    continue;
                };
                return Ok(DirectedRayOutcome::Hit(BlockHit {
                    voxel,
                    name: block.name,
                    properties: block.properties,
                }));
            }
            BlockProbe::Loaded { .. } => {}
            BlockProbe::Unloaded => return Ok(DirectedRayOutcome::Unloaded),
            // 与全量内核一致的保守射线边界：出界邻居不是被查目标的证据，跳过。
            BlockProbe::OutOfWorld => {}
        }
    }
    Ok(DirectedRayOutcome::Clear)
}

fn view_axes(yaw_degrees: f64, pitch_degrees: f64) -> ViewAxes {
    // 角度制输入（azalea LookDirection 同单位），内部转弧度。
    let yaw = yaw_degrees.to_radians();
    let pitch = pitch_degrees.to_radians();
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
            properties: block.properties,
            position: [position.x, position.y, position.z],
        }),
        BlockReadResult::Loaded { .. }
        | BlockReadResult::Unloaded
        | BlockReadResult::OutOfWorld => None,
    })
}

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
    Ok(
        match first_hit(
            reader,
            eye,
            direction,
            options.looked_at_max_distance,
            RayProperty::Visible,
            checkpoint,
        )? {
            // 注视的方块要交给读方，这里才付完整 DTO 的钱——一次投影一次。
            RayOutcome::Hit(voxel) => match reader.full(voxel.clone()) {
                BlockReadResult::Loaded { block } => Some(ViewportBlock {
                    name: block.name,
                    properties: block.properties,
                    position: [voxel.x, voxel.y, voxel.z],
                }),
                // 探针命中、完整读拿不到：只可能是两次读之间世界变了。
                // 不报一个读不出来的方块。
                BlockReadResult::Unloaded | BlockReadResult::OutOfWorld => None,
            },
            RayOutcome::Clear | RayOutcome::Unloaded => None,
        },
    )
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
            RayProperty::Visible => visible,
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
    let AxisAlignedBox { min, max } = bounds;
    [
        Point3 { x: min.x, y: min.y, z: min.z },
        Point3 { x: min.x, y: min.y, z: max.z },
        Point3 { x: min.x, y: max.y, z: min.z },
        Point3 { x: min.x, y: max.y, z: max.z },
        Point3 { x: max.x, y: min.y, z: min.z },
        Point3 { x: max.x, y: min.y, z: max.z },
        Point3 { x: max.x, y: max.y, z: min.z },
        Point3 { x: max.x, y: max.y, z: max.z },
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

fn compare_candidate(left: &(f64, BlockPosition), right: &(f64, BlockPosition)) -> Ordering {
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
