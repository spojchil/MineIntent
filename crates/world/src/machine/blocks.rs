//! azalea 世界模型上的方块读取原语——「拉」那条路的底层。
//!
//! 方块不进 tick 快照（最深最重的嵌套留在原地），视口按绝对坐标来这里拉。
//! 本模块只回答「这一格是什么」；看不看得见由视口层做视锥与遮挡。
//!
//! 类型词汇在 [`crate::block`]，那边不依赖 azalea；这里是它在 azalea
//! 世界模型上的实现。

use azalea::physics::collision::BlockWithShape;
use azalea::BlockPos;

use crate::{BlockPosition, BlockProbe, BlockReadResult};

/// `BlockProbe::Loaded` 的两位，按 `state_id` 预先算好。整张表首次使用时
/// 算一遍（`BlockStateIntegerRepr` 是 u16，表最大 64 KiB），之后每次探测
/// 是一次数组下标，零分配。
fn probe_table() -> &'static [(bool, bool)] {
    static TABLE: std::sync::OnceLock<Box<[(bool, bool)]>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        (0..=azalea::block::BlockState::MAX_STATE)
            .map(|state_id| {
                let Ok(state) = azalea::block::BlockState::try_from(state_id) else {
                    // 不该发生：迭代范围就是合法区间。按最保守的"有东西且
                    // 不透光"处理，宁可少报可见块也不误报。
                    return (true, false);
                };
                let block: Box<dyn azalea::block::BlockTrait> = Box::from(state);
                let name = block.id();
                (
                    !crate::is_air_name(name),
                    transparent_hint(name, state.outline_shape()),
                )
            })
            .collect()
    })
}

fn transparent_hint(name: &str, outline_shape: &azalea::physics::collision::VoxelShape) -> bool {
    // 26.1 的方块注册表没有暴露 transparent 布尔；对常见全体积透明块按名
    // 提示，其余非完整轮廓按保守的"可能透光"处理。
    let named_transparent = crate::is_air_name(name)
        || name.contains("glass")
        || name.ends_with("leaves")
        || name == "water"
        || name == "lava"
        || name == "powder_snow";
    named_transparent || !is_full_cube(outline_shape)
}

fn is_full_cube(shape: &azalea::physics::collision::VoxelShape) -> bool {
    let boxes = shape.to_aabbs();
    boxes.len() == 1
        && boxes[0].min.x == 0.0
        && boxes[0].min.y == 0.0
        && boxes[0].min.z == 0.0
        && boxes[0].max.x == 1.0
        && boxes[0].max.y == 1.0
        && boxes[0].max.z == 1.0
}

/// 热路径探针：不建 DTO，一次表下标。
pub(super) fn probe_block_from_world(
    world: &azalea::world::World,
    position: BlockPosition,
) -> BlockProbe {
    let block_position = BlockPos::new(position.x, position.y, position.z);
    let y = i64::from(position.y);
    let min_y = i64::from(world.chunks.min_y());
    let max_y_exclusive = min_y + i64::from(world.chunks.height());
    if y < min_y || y >= max_y_exclusive {
        return BlockProbe::OutOfWorld;
    }
    let Some(state) = world.get_block_state(block_position) else {
        return BlockProbe::Unloaded;
    };
    let (visible, transparent_hint) = probe_table()[usize::from(state.id())];
    BlockProbe::Loaded {
        visible,
        transparent_hint,
    }
}

/// 完整 DTO 读取：只在要把方块交给读方时调用。
pub(super) fn read_block_from_world(
    world: &azalea::world::World,
    position: BlockPosition,
) -> BlockReadResult {
    let block_position = BlockPos::new(position.x, position.y, position.z);
    let y = i64::from(position.y);
    let min_y = i64::from(world.chunks.min_y());
    let max_y_exclusive = min_y + i64::from(world.chunks.height());
    if y < min_y || y >= max_y_exclusive {
        return BlockReadResult::OutOfWorld;
    }
    let Some(state) = world.get_block_state(block_position) else {
        return BlockReadResult::Unloaded;
    };
    let block: Box<dyn azalea::block::BlockTrait> = Box::from(state);
    let collision_shape = state.collision_shape();
    let collision_shapes: Vec<[f64; 6]> = collision_shape
        .to_aabbs()
        .into_iter()
        .map(|aabb| {
            [
                aabb.min.x, aabb.min.y, aabb.min.z, aabb.max.x, aabb.max.y, aabb.max.z,
            ]
        })
        .collect();
    let properties = block
        .property_map()
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
    let bounding_box = if collision_shapes.is_empty() {
        crate::BlockBoundingBox::Empty
    } else {
        crate::BlockBoundingBox::Block
    };
    BlockReadResult::Loaded {
        block: crate::BlockSnapshot {
            position,
            name: block.id().to_owned(),
            state_id: u32::from(state.id()),
            properties,
            collision_shapes,
            transparent_hint: transparent_hint(block.id(), state.outline_shape()),
            bounding_box,
        },
    }
}
