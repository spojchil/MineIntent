//! azalea 世界模型上的方块解码：成像拷贝出的注册表状态在这里变成 DTO、
//! 渲染分类与原版光照属性。
//!
//! 类型词汇在 [`crate::block`]，那边不依赖 azalea；这里是它在 azalea
//! 世界模型上的实现。

use azalea::physics::collision::BlockWithShape;

use crate::BlockPosition;

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

/// 锁外解码一份拷贝出的注册表状态。
pub(super) fn snapshot_from_state(
    state: azalea::block::BlockState,
    position: BlockPosition,
) -> crate::BlockSnapshot {
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
    crate::BlockSnapshot {
        position,
        name: block.id().to_owned(),
        state_id: u32::from(state.id()),
        properties,
        collision_shapes,
        transparent_hint: transparent_hint(block.id(), state.outline_shape()),
        bounding_box,
    }
}

/// 成像用的方块分类：只回答「这一格挡不挡住邻格的面」与「是哪种流体」。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RenderClass {
    Air,
    /// 完整不透明方块：贴着它的面原版不画。
    Opaque,
    Water,
    Lava,
    /// 其余看得见但不挡面的方块（半砖、树叶、玻璃、花草……）。
    Other,
}

/// 按 `state_id` 预先算好的 [`RenderClass`]，与 [`probe_table`] 一样首次使用时建表。
pub(super) fn render_class(state_id: u16) -> RenderClass {
    static TABLE: std::sync::OnceLock<Box<[RenderClass]>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        (0..=azalea::block::BlockState::MAX_STATE)
            .map(|state_id| {
                let Ok(state) = azalea::block::BlockState::try_from(state_id) else {
                    return RenderClass::Opaque;
                };
                let block: Box<dyn azalea::block::BlockTrait> = Box::from(state);
                let name = block.id();
                if crate::is_air_name(name) {
                    RenderClass::Air
                } else if name == "water" {
                    RenderClass::Water
                } else if name == "lava" {
                    RenderClass::Lava
                } else if transparent_hint(name, state.outline_shape()) || renders_through(name) {
                    RenderClass::Other
                } else {
                    RenderClass::Opaque
                }
            })
            .collect()
    })[usize::from(state_id)]
}

/// 轮廓是完整方块、但原版渲染不遮挡邻面的方块（半透明或镂空材质、不可见方块）。
fn renders_through(name: &str) -> bool {
    matches!(
        name,
        "ice"
            | "frosted_ice"
            | "slime_block"
            | "honey_block"
            | "barrier"
            | "spawner"
            | "beacon"
            | "trial_spawner"
            | "vault"
    )
}

/// 原版方块渲染属性表（`data/BlockRenderDump.java` 从 26.1.2 服务端导出），每个状态 2 字节。
static RENDER_TABLE: &[u8] = include_bytes!("../../data/block_render_26.1.2.bin");

/// 一个方块状态在原版光照与环境光遮蔽里用到的属性。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct RenderProps {
    pub emission: u8,
    pub dampening: u8,
    pub view_blocking: bool,
    pub solid_render: bool,
    pub emissive: bool,
    pub full_collision: bool,
}

pub(super) fn render_props(state_id: u16) -> RenderProps {
    let index = usize::from(state_id) * 2;
    let (Some(&light), Some(&flags)) = (RENDER_TABLE.get(index), RENDER_TABLE.get(index + 1))
    else {
        return RenderProps::default();
    };
    RenderProps {
        emission: light >> 4,
        dampening: light & 0xF,
        view_blocking: flags & 1 != 0,
        solid_render: flags & 2 != 0,
        emissive: flags & 4 != 0,
        full_collision: flags & 8 != 0,
    }
}

#[cfg(test)]
mod render_table_tests {
    use super::*;

    fn props_of(name: &str) -> RenderProps {
        (0..=azalea::block::BlockState::MAX_STATE)
            .find(|&id| {
                azalea::block::BlockState::try_from(id).is_ok_and(|state| {
                    let block: Box<dyn azalea::block::BlockTrait> = Box::from(state);
                    block.id() == name
                })
            })
            .map(render_props)
            .unwrap()
    }

    #[test]
    fn table_lines_up_with_azalea_state_ids() {
        assert_eq!(
            RENDER_TABLE.len(),
            (usize::from(azalea::block::BlockState::MAX_STATE) + 1) * 2
        );
        let glowstone = props_of("glowstone");
        assert_eq!((glowstone.emission, glowstone.dampening), (15, 15));
        let stone = props_of("stone");
        assert!(stone.view_blocking && stone.solid_render && stone.full_collision);
        assert_eq!((stone.emission, stone.dampening), (0, 15));
        let glass = props_of("glass");
        assert!(!glass.view_blocking && !glass.solid_render && glass.full_collision);
        assert_eq!(glass.dampening, 0);
        assert!(props_of("magma_block").emissive);
        assert_eq!(props_of("torch").emission, 14);
    }
}
