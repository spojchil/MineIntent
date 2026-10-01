//! 方块词汇：成像用的方块拷贝与光照格。
//!
//! 方块不进 tick 快照——最深最重的嵌套留在 azalea 世界模型原地，成像时由
//! `Module::capture_view` 按视距与调用方给的区块段判据拷贝一份。

use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockPosition {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockBoundingBox {
    Block,
    Empty,
}

/// 完整方块 DTO：azalea 注册表状态的直译。
/// `transparent_hint` 是观察层的保守提示，不是服务端的可见性结论。
#[derive(Clone, Debug, PartialEq)]
pub struct BlockSnapshot {
    pub position: BlockPosition,
    /// registry 本地名（如 `stone`、`air`），不带 `minecraft:` 前缀。
    pub name: String,
    pub state_id: u32,
    pub properties: BTreeMap<String, String>,
    pub collision_shapes: Vec<[f64; 6]>,
    pub transparent_hint: bool,
    pub bounding_box: BlockBoundingBox,
}

/// 站姿第一人称眼睛高度（格）。
pub const EYE_HEIGHT: f64 = 1.62;

/// 原版客户端的默认视距（区块）。登录前随客户端信息上报；实际视距取它与服务端给的较小者。
pub const CLIENT_VIEW_DISTANCE: u8 = 12;

/// 供成像用的一份当前已加载方块拷贝：视距内全高度、调用方判据认可的区块段，只含**表面**方块——
/// 至少有一面没被挡住的方块。六面都贴着完整不透明方块的方块看不见，不拷贝。
/// 这不是观察记忆，也不判断哪些面最终可见（那是成像的事）。
/// 位姿取最新 tick 快照，方块在同一把世界读锁下拷贝；两者不是同一服务端 tick 的原子事务。
#[derive(Clone, Debug)]
pub struct BlockRegion {
    pub snapshot: std::sync::Arc<crate::TickSnapshot>,
    pub blocks: Vec<RegionBlock>,
    /// 身边（[`NEAR_LOADED_RADIUS`] 格内）还没加载的格数。非 0 时不该出图：
    /// 未加载会被画成空气。更远处没加载的区块和原版一样直接不画。
    pub unloaded: usize,
    /// 实际视距（区块）：客户端视距与服务端视距的较小者。
    pub view_distance: u32,
    /// 原版地平线高度：超平坦是世界底，其余是 63。眼睛低于它时天空下半是黑的。
    pub horizon_height: f64,
    /// 原版平滑光照要查的格子：每个表面方块周围一圈，加上露出面前方第二层的四个侧格。
    /// 视距外、没加载的格不在里面。
    pub cells: Vec<LightCell>,
    /// 生物群系注册表名（服务端同步的协议序，如 `minecraft:plains`）；
    /// [`BiomeCell::biome`] 是这里的下标。
    pub biome_names: Vec<String>,
    /// 成像要查的生物群系格：表面方块的 5×5 混色与模糊缩放邻域、相机周围 6³。
    pub biomes: Vec<BiomeCell>,
    /// 生物群系格的 y 范围（含两端，单位 4 格）：原版查格时把 y 夹进这个范围。
    pub biome_quart_y: [i32; 2],
    /// 登录/重生包的生物群系缩放种子。
    pub biome_zoom_seed: i64,
    /// 主世界时钟的累计 tick（服务端时间包）。原版各时间轴按各自周期取模：
    /// 昼夜 `timeline/day` 24000，月相 `timeline/moon` 192000。
    pub clock_ticks: u64,
}

/// 一格的服务端光照与原版方块渲染属性（光照与环境光遮蔽用）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LightCell {
    pub position: [i32; 3],
    /// 天空光、方块光，0..=15。
    pub sky_light: u8,
    pub block_light: u8,
    /// 方块自身发光值（`getLightEmission`）与透光度（`getLightDampening`），0..=15。
    pub emission: u8,
    pub dampening: u8,
    /// `isViewBlocking`、`isSolidRender`、`emissiveRendering`、`isCollisionShapeFullBlock`。
    pub view_blocking: bool,
    pub solid_render: bool,
    pub emissive: bool,
    pub full_collision: bool,
}

/// 一个 4×4×4 生物群系格（原版 quart 坐标 = 方块坐标 >> 2）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BiomeCell {
    pub quart: [i32; 3],
    /// [`BlockRegion::biome_names`] 的下标。
    pub biome: u16,
}

/// 身边必须加载完整的半径（格）。
pub const NEAR_LOADED_RADIUS: i32 = 16;

#[derive(Clone, Debug, PartialEq)]
pub struct RegionBlock {
    pub block: BlockSnapshot,
    /// 六个方向上这一面是否被邻格挡住，位序：下、上、北（-z）、南（+z）、西（-x）、东（+x）。
    /// 挡住 = 邻格是完整不透明方块；流体另算同种流体相邻（原版 `FluidRenderer` 不画这种面）。
    /// 视距边界外与没加载的邻格按挡住算，世界顶之上按空气、世界底之下按挡住算。
    pub covered: u8,
}

/// 三种空气的注册名：这一格"没有东西"。
pub fn is_air_name(name: &str) -> bool {
    matches!(name, "air" | "cave_air" | "void_air")
}
