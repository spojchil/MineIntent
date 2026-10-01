//! 方块观察词汇：拉路径的读取原语。
//!
//! 方块不进 tick 快照——最深最重的嵌套留在 azalea 世界模型原地，读方按
//! 绝对坐标拉取。`BlockReadResult` 只回答"这一格是什么"，看不看得见由
//! 视口层从观察者位置做视锥与遮挡判断。

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

#[derive(Clone, Debug, PartialEq)]
pub enum BlockReadResult {
    Loaded { block: BlockSnapshot },
    Unloaded,
    OutOfWorld,
}

/// 原版客户端的默认视距（区块）。登录前随客户端信息上报；实际视距取它与服务端给的较小者。
pub const CLIENT_VIEW_DISTANCE: u8 = 12;

/// 供成像用的一份当前已加载方块拷贝：视距内全高度，只含**表面**方块——
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

/// 视口扫描热路径上唯一用得到的事实。
///
/// 一次全量投影要问十几万次「这一格挡不挡视线」，而每次问的都只有两位：
/// 是不是空气、透不透光。完整 DTO 为回答这两位携带三个持堆字段；
/// 这个类型是 `Copy` 的，缓存命中不分配。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockProbe {
    Loaded {
        /// 不是三种空气之一——也就是这一格"有东西"。
        visible: bool,
        transparent_hint: bool,
    },
    Unloaded,
    OutOfWorld,
}

impl BlockProbe {
    /// 从完整 DTO 折出探针。给测试与合成读取器用；生产读取器应当
    /// 直接从注册表状态取探针，不为热路径建 DTO。
    pub fn from_read(result: &BlockReadResult) -> Self {
        match result {
            BlockReadResult::Loaded { block } => Self::Loaded {
                visible: !is_air_name(&block.name),
                transparent_hint: block.transparent_hint,
            },
            BlockReadResult::Unloaded => Self::Unloaded,
            BlockReadResult::OutOfWorld => Self::OutOfWorld,
        }
    }
}

/// 三种空气的注册名。视口把它们当作"这一格没有东西"。
pub fn is_air_name(name: &str) -> bool {
    matches!(name, "air" | "cave_air" | "void_air")
}
