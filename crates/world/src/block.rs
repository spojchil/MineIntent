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
