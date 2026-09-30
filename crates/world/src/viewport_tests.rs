//! 视口内核的行为 oracle，随内核自旧 backend 迁入。
//! 呈现相关断言（BlockInfo 序列化、线格式 JSON、legend 文案）未迁——
//! 那些属于渲染层；几何与分类行为逐条保留。

use std::collections::BTreeMap;

use super::*;
use crate::block::{BlockBoundingBox, BlockSnapshot};

fn pose(yaw: f64) -> Pose {
    Pose {
        position: Vec3Value {
            x: 0.5,
            y: 1.0,
            z: 0.5,
        },
        yaw,
        pitch: 0.0,
    }
}

fn block(name: &str, transparent_hint: bool) -> BlockSnapshot {
    BlockSnapshot {
        position: BlockPosition { x: 0, y: 0, z: 0 },
        name: name.to_owned(),
        state_id: 1,
        properties: BTreeMap::new(),
        collision_shapes: Vec::new(),
        transparent_hint,
        bounding_box: BlockBoundingBox::Block,
    }
}

fn entity(key: &str, z: f64) -> EntitySnapshot {
    entity_at(key, "sheep", None, [0.5, 2.0, z], 0.9, 1.3)
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
        block_limit: 64,
        entity_limit: 8,
        ..ViewportOptions::default()
    }
}

#[test]
fn projection_reports_pose_and_first_hit_in_absolute_coordinates() {
    let projection = project(&pose(180.0), &[], fixture_read, &options())
        .expect("fixture options should be valid");

    assert_eq!(projection.pose.position, [0.5, 1.0, 0.5]);
    assert_eq!(
        projection.looked_at_block,
        Some(ViewportBlock {
            name: "stone".to_owned(),
            state_id: 1,
            properties: BTreeMap::new(),
            position: [0, 2, -1],
        })
    );
    assert_eq!(
        projection.standing_on_block, None,
        "fixture 地面是空气，不得捏造支撑方块"
    );
}

#[test]
fn opaque_wall_blocks_far_blocks_and_entities() {
    let entities = [entity("near", 0.5), entity("behind-wall", -2.0)];
    let projection = project(&pose(180.0), &entities, fixture_read, &options())
        .expect("fixture options should be valid");

    assert!(projection
        .visible_blocks
        .blocks
        .iter()
        .any(|block| block.name == "stone" && block.position[2] == -1));
    assert!(!projection
        .visible_blocks
        .blocks
        .iter()
        .any(|block| block.position[2] == -2));
    assert_eq!(projection.visible_entities.items.len(), 1);
    assert_eq!(projection.visible_entities.items[0].entity_type, "sheep");
}

#[test]
fn invalid_options_are_rejected_before_scanning() {
    let options = ViewportOptions {
        max_distance: f64::NAN,
        ..ViewportOptions::default()
    };
    let result = project(&pose(180.0), &[], fixture_read, &options);
    assert!(result.is_err());
}

#[test]
fn gaze_lands_on_terminal_air_at_sky_box_edge_and_loading_frontier() {
    // 看天：穿出世界高度前的最高一格空气。
    let sky_read = |position: BlockPosition| {
        if position.y > 5 {
            BlockReadResult::OutOfWorld
        } else {
            BlockReadResult::Loaded {
                block: block("air", true),
            }
        }
    };
    let up = Pose {
        position: Vec3Value {
            x: 0.5,
            y: 1.0,
            z: 0.5,
        },
        yaw: 0.0,
        // 原版俯仰约定：−90 = 向上（视向量 y=−sin(pitch)）。
        pitch: -90.0,
    };
    let projection = project(&up, &[], sky_read, &options()).expect("valid options");
    let landing = projection.looked_at_block.expect("看天也该有落点");
    assert_eq!(landing.name, "air");
    assert_eq!(landing.position, [0, 5, 0], "{landing:?}");

    // 一路空气：扫描盒边界上的那格空气（radius 4 → z 最远 -4）。
    let all_air = |_position: BlockPosition| BlockReadResult::Loaded {
        block: block("air", true),
    };
    let projection = project(&pose(180.0), &[], all_air, &options()).expect("valid options");
    let landing = projection.looked_at_block.expect("一路空气也该有落点");
    assert_eq!(landing.name, "air");
    assert_eq!(landing.position, [0, 2, -4], "{landing:?}");

    // 撞上未加载区：已知边界的最后一格空气。
    let frontier = |position: BlockPosition| {
        if position.z <= -3 {
            BlockReadResult::Unloaded
        } else {
            BlockReadResult::Loaded {
                block: block("air", true),
            }
        }
    };
    let projection = project(&pose(180.0), &[], frontier, &options()).expect("valid options");
    let landing = projection.looked_at_block.expect("认知边界也该有落点");
    assert_eq!(landing.name, "air");
    assert_eq!(landing.position, [0, 2, -2], "{landing:?}");
}

// ---- perception oracle 几何断言（自主仓库 perception.ts 测试移植而来）----

fn pose_at(x: f64, y: f64, z: f64) -> Pose {
    Pose {
        position: Vec3Value { x, y, z },
        // 夹具全部铺在 −z 侧；原版约定 yaw 180=北（−z），面朝夹具。
        yaw: 180.0,
        pitch: 0.0,
    }
}

fn entity_at(
    key: &str,
    entity_type: &str,
    username: Option<&str>,
    position: [f64; 3],
    width: f64,
    height: f64,
) -> EntitySnapshot {
    EntitySnapshot {
        entity_key: key.to_owned(),
        protocol_entity_id: 1,
        entity_type: entity_type.to_owned(),
        name: None,
        username: username.map(str::to_owned),
        uuid: None,
        position: Vec3Value {
            x: position[0],
            y: position[1],
            z: position[2],
        },
        velocity: Vec3Value::default(),
        yaw: 0.0,
        pitch: 0.0,
        head_yaw: None,
        width,
        height,
        on_ground: true,
        pose: None,
        held_item_name: None,
        equipment: Vec::new(),
        valid: true,
    }
}

fn map_read<'a>(
    blocks: &'a [(i32, i32, i32, &'a str, bool)],
) -> impl Fn(BlockPosition) -> BlockReadResult + 'a {
    move |position: BlockPosition| {
        for (x, y, z, name, transparent) in blocks {
            if position.x == *x && position.y == *y && position.z == *z {
                return BlockReadResult::Loaded {
                    block: block(name, *transparent),
                };
            }
        }
        BlockReadResult::Loaded {
            block: block("air", true),
        }
    }
}

fn wide_options() -> ViewportOptions {
    ViewportOptions {
        horizontal_radius: 12,
        vertical_radius: 10,
        max_distance: 20.0,
        block_limit: 64,
        entity_limit: 8,
        ..ViewportOptions::default()
    }
}

fn block_names(projection: &ViewportProjection) -> Vec<String> {
    projection
        .visible_blocks
        .blocks
        .iter()
        .map(|entry| entry.name.clone())
        .collect()
}

/// oracle：约 50.4° 侧偏在 16:9 水平半角内可见，约 39.7° 上仰在
/// 35° 垂直半角外不可见——视锥是矩形，不是各向同性圆锥。
#[test]
fn view_frustum_is_rectangular_not_an_isotropic_cone_for_blocks() {
    let blocks = [
        (11, 65, -10, "sideways", false),
        (0, 73, -10, "raised", false),
    ];
    let projection = project(
        &pose_at(0.0, 64.0, 0.0),
        &[],
        map_read(&blocks),
        &wide_options(),
    )
    .expect("options valid");
    let names = block_names(&projection);
    assert!(
        names.iter().any(|name| name == "sideways"),
        "50.4° 侧偏应在矩形视锥内: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name == "raised"),
        "39.7° 上仰应在垂直半角外: {names:?}"
    );
}

/// oracle：暴露在相机背后的方块不进可见集。
#[test]
fn blocks_behind_the_camera_are_excluded() {
    let blocks = [(0, 65, 6, "stone", false)];
    let projection = project(
        &pose_at(0.0, 64.0, 0.0),
        &[],
        map_read(&blocks),
        &wide_options(),
    )
    .expect("options valid");
    assert!(projection.visible_blocks.blocks.is_empty());
}

/// oracle：按距排序，截断永远弃最远、保最近。
#[test]
fn visible_blocks_sort_by_distance_and_limit_truncates_farthest_first() {
    let blocks = [
        (0, 65, -2, "nearest", false),
        (-3, 65, -6, "farther", false),
    ];
    let full = project(
        &pose_at(0.0, 64.0, 0.0),
        &[],
        map_read(&blocks),
        &wide_options(),
    )
    .expect("options valid");
    assert!(!full.visible_blocks.truncated);
    assert_eq!(block_names(&full), vec!["nearest", "farther"]);

    let mut limited_options = wide_options();
    limited_options.block_limit = 1;
    let limited = project(
        &pose_at(0.0, 64.0, 0.0),
        &[],
        map_read(&blocks),
        &limited_options,
    )
    .expect("options valid");
    assert!(limited.visible_blocks.truncated);
    assert_eq!(block_names(&limited), vec!["nearest"]);
}

/// oracle：可见的透光方块不遮挡其后的不透明方块；准星射线能命中透明方块。
#[test]
fn transparent_block_does_not_hide_opaque_behind_and_is_looked_at() {
    let blocks = [(0, 65, -2, "glass", true), (0, 65, -3, "stone", false)];
    let projection = project(
        &pose_at(0.5, 64.0, 0.5),
        &[],
        map_read(&blocks),
        &wide_options(),
    )
    .expect("options valid");
    let names = block_names(&projection);
    assert!(names.iter().any(|name| name == "glass"), "{names:?}");
    assert!(
        names.iter().any(|name| name == "stone"),
        "透明方块不得遮挡其后不透明方块: {names:?}"
    );
    let looked_at = projection.looked_at_block.expect("准星命中玻璃");
    assert_eq!(looked_at.name, "glass");
    assert_eq!(looked_at.position, [0, 65, -2]);
}

/// oracle：同距 5 格，48° 侧偏可见、约 44° 上仰不可见——实体视锥同为矩形。
#[test]
fn entity_frustum_is_rectangular_not_an_isotropic_cone() {
    let entities = [
        entity_at("side", "sheep", None, [5.55, 65.0, -5.0], 0.9, 1.3),
        entity_at("raised", "sheep", None, [0.0, 70.5, -5.0], 0.9, 1.3),
    ];
    let projection = project(
        &pose_at(0.0, 64.0, 0.0),
        &entities,
        map_read(&[]),
        &wide_options(),
    )
    .expect("options valid");
    let keys: Vec<[f64; 3]> = projection
        .visible_entities
        .items
        .iter()
        .map(|entity| entity.position)
        .collect();
    assert_eq!(projection.visible_entities.items.len(), 1, "{keys:?}");
    assert_eq!(projection.visible_entities.items[0].position[0], 5.6);
}

/// oracle：相机背后的实体不进可见集，视锥拒绝不算截断。
#[test]
fn entities_behind_the_camera_are_excluded_and_not_counted_as_truncated() {
    let entities = [
        entity_at("ahead", "sheep", None, [2.0, 64.0, -5.0], 0.9, 1.3),
        entity_at("behind", "cow", None, [0.0, 64.0, 5.0], 0.9, 1.4),
    ];
    let projection = project(
        &pose_at(0.0, 64.0, 0.0),
        &entities,
        map_read(&[]),
        &wide_options(),
    )
    .expect("options valid");
    assert_eq!(projection.visible_entities.items.len(), 1);
    assert_eq!(projection.visible_entities.items[0].entity_type, "sheep");
    assert!(!projection.visible_entities.truncated, "视锥拒绝不是截断");
}

/// oracle：中心在视锥外但 hitbox 与视锥相交的实体仍可见。
#[test]
fn entity_hitbox_intersecting_frustum_keeps_visibility() {
    let entities = [entity_at(
        "sheep",
        "sheep",
        None,
        [0.0, 64.0, -1.0],
        0.9,
        1.3,
    )];
    let projection = project(
        &pose_at(0.0, 64.0, 0.0),
        &entities,
        map_read(&[]),
        &wide_options(),
    )
    .expect("options valid");
    assert_eq!(
        projection
            .visible_entities
            .items
            .iter()
            .map(|entity| entity.entity_type.as_str())
            .collect::<Vec<_>>(),
        vec!["sheep"]
    );
}

/// oracle：实体上限截断报真，弃最远、永不弃最近。
#[test]
fn entity_cap_drops_farthest_never_nearest_and_reports_truncation() {
    let entities: Vec<EntitySnapshot> = (0..9)
        .map(|index| {
            entity_at(
                &format!("sheep-{index}"),
                "sheep",
                None,
                [0.0, 64.0, -2.0 - f64::from(index)],
                0.9,
                1.3,
            )
        })
        .collect();
    let projection = project(
        &pose_at(0.0, 64.0, 0.0),
        &entities,
        map_read(&[]),
        &wide_options(),
    )
    .expect("options valid");
    assert!(projection.visible_entities.truncated);
    assert_eq!(projection.visible_entities.items.len(), 8);
    let zs: Vec<f64> = projection
        .visible_entities
        .items
        .iter()
        .map(|entity| entity.position[2])
        .collect();
    assert!(zs.contains(&-2.0), "最近的必须保留: {zs:?}");
    assert!(!zs.contains(&-10.0), "最远的必须被弃: {zs:?}");
}

/// oracle：与怪同名的玩家保持可分辨——type 与 player 两列分开。
#[test]
fn player_named_after_a_mob_stays_distinguishable() {
    let entities = [
        entity_at("p", "player", Some("sheep"), [0.0, 64.0, -3.0], 0.6, 1.8),
        entity_at("m", "sheep", None, [1.0, 64.0, -5.0], 0.9, 1.3),
    ];
    let projection = project(
        &pose_at(0.0, 64.0, 0.0),
        &entities,
        map_read(&[]),
        &wide_options(),
    )
    .expect("options valid");
    let mut labels: Vec<(String, Option<String>)> = projection
        .visible_entities
        .items
        .iter()
        .map(|entity| (entity.entity_type.clone(), entity.player.clone()))
        .collect();
    labels.sort();
    assert_eq!(
        labels,
        vec![
            ("player".to_owned(), Some("sheep".to_owned())),
            ("sheep".to_owned(), None),
        ]
    );
}

// ---- 已观察空间：射线走过的空格 ----

/// 造一个只有指定坐标是石头、其余全空气的世界读取器。
fn stone_at(solid: Vec<BlockPosition>) -> impl Fn(BlockPosition) -> BlockReadResult {
    move |position: BlockPosition| {
        if solid.contains(&position) {
            BlockReadResult::Loaded {
                block: block("stone", false),
            }
        } else {
            BlockReadResult::Loaded {
                block: block("air", true),
            }
        }
    }
}

fn seen(position: [i32; 3]) -> ViewportBlock {
    ViewportBlock {
        name: "stone".to_owned(),
        state_id: 1,
        properties: BTreeMap::new(),
        position,
    }
}

/// 主线：眼睛到已见方块之间的空格全部记下，方块自己那一格不算空。
#[test]
fn free_space_along_the_ray_to_a_seen_block_is_recorded() {
    let target = BlockPosition { x: 0, y: 1, z: 5 };
    let read = stone_at(vec![target.clone()]);
    let mut reader = WorldReader::new(|position| BlockProbe::from_read(&read(position)), &read);
    let mut space = ObservedSpace::new();

    observe_free_space(
        &mut reader,
        Point3 {
            x: 0.5,
            y: 1.5,
            z: 0.5,
        },
        &[seen([0, 1, 5])],
        None,
        &mut space,
        &mut || Ok(()),
    )
    .expect("射线应当可判定");

    for z in 1..=4 {
        assert!(space.contains([0, 1, z]), "(0,1,{z}) 在射线上且是空的");
    }
    assert!(
        !space.contains([0, 1, 5]),
        "方块那一格不是空的，不该标成已观察为空"
    );
}

/// 开放方向没有可见方块可当射线终点；准星落在 AIR 时仍应把整条真实可读射线
/// 记成自由空间，否则看向空走廊反而学不到路。
#[test]
fn looked_at_air_records_the_open_gaze_ray() {
    let read = stone_at(Vec::new());
    let mut reader = WorldReader::new(|position| BlockProbe::from_read(&read(position)), &read);
    let mut space = ObservedSpace::new();
    let looked_at = ViewportBlock {
        name: "air".to_owned(),
        state_id: 0,
        properties: BTreeMap::new(),
        position: [0, 1, 5],
    };

    observe_free_space(
        &mut reader,
        Point3 {
            x: 0.5,
            y: 1.5,
            z: 0.5,
        },
        &[],
        Some(&looked_at),
        &mut space,
        &mut || Ok(()),
    )
    .expect("开放射线应当可判定");

    for z in 1..=5 {
        assert!(space.contains([0, 1, z]), "(0,1,{z}) 应被亲眼确认为空");
    }
}

/// **不多标**：中心线被墙挡住时，墙后面的空格一格都不许标。
/// 可见性由暴露面射线判定（射向面，不射向中心），所以这一支确实会发生。
#[test]
fn nothing_behind_an_occluder_is_recorded() {
    let wall = BlockPosition { x: 0, y: 1, z: 2 };
    let target = BlockPosition { x: 0, y: 1, z: 5 };
    let read = stone_at(vec![wall, target.clone()]);
    let mut reader = WorldReader::new(|position| BlockProbe::from_read(&read(position)), &read);
    let mut space = ObservedSpace::new();

    observe_free_space(
        &mut reader,
        Point3 {
            x: 0.5,
            y: 1.5,
            z: 0.5,
        },
        &[seen([0, 1, 5])],
        None,
        &mut space,
        &mut || Ok(()),
    )
    .expect("射线应当可判定");

    assert!(space.contains([0, 1, 1]), "墙之前的空格照记");
    for z in 2..=5 {
        assert!(!space.contains([0, 1, z]), "(0,1,{z}) 在墙之后，不得标记");
    }
}

/// 没加载的地方不能声称看过：探针答 Unloaded 就停，别把未知说成空。
#[test]
fn an_unloaded_cell_stops_the_walk_instead_of_being_called_empty() {
    let read = |position: BlockPosition| {
        if position.z >= 3 {
            BlockReadResult::Unloaded
        } else {
            BlockReadResult::Loaded {
                block: block("air", true),
            }
        }
    };
    let mut reader = WorldReader::new(|position| BlockProbe::from_read(&read(position)), &read);
    let mut space = ObservedSpace::new();

    observe_free_space(
        &mut reader,
        Point3 {
            x: 0.5,
            y: 1.5,
            z: 0.5,
        },
        &[seen([0, 1, 6])],
        None,
        &mut space,
        &mut || Ok(()),
    )
    .expect("射线应当可判定");

    assert!(space.contains([0, 1, 2]), "加载了的空格照记");
    for z in 3..=6 {
        assert!(!space.contains([0, 1, z]), "(0,1,{z}) 没加载，不得当成空");
    }
}

/// 三态判据合起来读：同一份记忆加同一份空间，答三种话。
#[test]
fn memory_and_space_together_answer_three_states() {
    let target = BlockPosition { x: 0, y: 1, z: 3 };
    let read = stone_at(vec![target.clone()]);
    let mut reader = WorldReader::new(|position| BlockProbe::from_read(&read(position)), &read);
    let mut space = ObservedSpace::new();
    let mut memory = BlockMemory::new();
    memory.absorb_visible(&[seen([0, 1, 3])], 0);

    observe_free_space(
        &mut reader,
        Point3 {
            x: 0.5,
            y: 1.5,
            z: 0.5,
        },
        &[seen([0, 1, 3])],
        None,
        &mut space,
        &mut || Ok(()),
    )
    .expect("射线应当可判定");

    // 暂存折进记忆之后，三态从同一个出口给出。
    memory.absorb_empty(&space, 0);
    assert!(
        matches!(memory.state_at([0, 1, 3]), Known::Block(_)),
        "有东西"
    );
    assert_eq!(memory.state_at([0, 1, 2]), Known::Empty, "确认为空");
    assert_eq!(memory.state_at([99, 1, 99]), Known::Unseen, "没看过");
}
