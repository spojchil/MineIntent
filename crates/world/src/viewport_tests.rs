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
        looked_at_max_distance: 8.0,
        block_limit: 64,
        entity_limit: 8,
        ..ViewportOptions::default()
    }
}

fn world_bounds() -> WorldHeightBounds {
    WorldHeightBounds::new(-64, 384)
}

#[test]
fn world_height_bounds_uses_u32_without_upper_bound_overflow() {
    let bounds = WorldHeightBounds::new(i32::MIN, u32::MAX);
    assert!(bounds.contains_y(i32::MIN));
    assert!(bounds.contains_y(i32::MAX - 1));
    assert!(!bounds.contains_y(i32::MAX));
}

#[test]
fn projection_reports_pose_and_first_hit_in_absolute_coordinates() {
    let projection = project(&pose(0.0), &[], fixture_read, &options())
        .expect("fixture options should be valid");

    assert_eq!(projection.pose.position, [0.5, 1.0, 0.5]);
    assert_eq!(
        projection.looked_at_block,
        Some(ViewportBlock {
            name: "stone".to_owned(),
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
    let projection = project(&pose(0.0), &entities, fixture_read, &options())
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
    let result = project(&pose(0.0), &[], fixture_read, &options);
    assert!(result.is_err());
}

#[test]
fn directed_kernel_reports_seen_air_four_reasons_and_first_occluder() {
    let positions = [[1, 2, -1], [0, 2, -2], [3, 2, -5], [5, 2, 0]];
    let result = project_directed(
        &pose(0.0),
        &positions,
        |position| {
            if position.x == 3 && position.y == 2 && position.z == -5 {
                BlockReadResult::Unloaded
            } else {
                fixture_read(position)
            }
        },
        &options(),
        world_bounds(),
        || Ok(()),
    )
    .expect("directed fixture should be classifiable");

    assert_eq!(result.seen.len(), 1);
    assert_eq!(result.seen[0].at, [1, 2, -1]);
    assert_eq!(result.seen[0].name, "air");

    let occluded = result
        .unseen
        .iter()
        .find(|item| item.at == [0, 2, -2])
        .expect("被遮挡目标应在报告中");
    assert_eq!(occluded.why, [DirectedWhy::Occluded]);
    assert_eq!(occluded.distance, None);
    assert_eq!(occluded.max, None);
    let by = occluded.by.as_ref().expect("应报出第一个遮挡者");
    assert_eq!(by.at, [0, 2, -1]);
    assert_eq!(by.name, "stone");

    let unloaded = result
        .unseen
        .iter()
        .find(|item| item.at == [3, 2, -5])
        .expect("未加载目标应在报告中");
    assert_eq!(
        unloaded.why,
        [DirectedWhy::Occluded, DirectedWhy::ChunkNotLoaded]
    );
    assert_eq!(unloaded.by.as_ref().unwrap().at, [0, 2, -1]);
    assert!(unloaded.distance.is_none());
    assert!(unloaded.max.is_none());

    let outside = result
        .unseen
        .iter()
        .find(|item| item.at == [5, 2, 0])
        .expect("视锥外目标应在报告中");
    assert_eq!(outside.why, [DirectedWhy::OutsideFov]);
}

#[test]
fn changes_mode_reports_appearance_silence_vanish_and_ignores_whats_behind() {
    let all_air = |_position: BlockPosition| BlockReadResult::Loaded {
        block: block("air", true),
    };

    // 首看：空记忆 → 石头是新看到。
    let mut memory = BlockMemory::new();
    let first = project_changes(
        &pose(0.0),
        &memory,
        fixture_read,
        &options(),
        world_bounds(),
    )
    .expect("fixture options should be valid");
    assert_eq!(first.len(), 1);
    assert!(
        matches!(&first[0], BlockChange::Appeared { at, fact } if *at == [0, 2, -1] && fact.name == "stone"),
        "{first:?}"
    );

    // 推进后同景再看：无话可说。
    memory.apply(&first);
    let silent = project_changes(
        &pose(0.0),
        &memory,
        fixture_read,
        &options(),
        world_bounds(),
    )
    .expect("fixture options should be valid");
    assert!(silent.is_empty(), "{silent:?}");

    // 石头被移走（世界全空）：亲眼可证 → 没了。
    let vanish = project_changes(&pose(0.0), &memory, all_air, &options(), world_bounds())
        .expect("fixture options should be valid");
    assert_eq!(vanish.len(), 1);
    assert!(
        matches!(&vanish[0], BlockChange::Vanished { at, was } if *at == [0, 2, -1] && was.name == "stone"),
        "{vanish:?}"
    );

    // 背后的记忆（视锥外）：即使世界全空也保持沉默——看不到就不下结论。
    let mut behind = BlockMemory::new();
    behind.apply(&[BlockChange::Appeared {
        at: [0, 2, 3],
        fact: BlockFact {
            name: "stone".to_owned(),
            properties: BTreeMap::new(),
        },
    }]);
    let quiet = project_changes(&pose(0.0), &behind, all_air, &options(), world_bounds())
        .expect("fixture options should be valid");
    assert!(quiet.is_empty(), "{quiet:?}");
    assert_eq!(behind.len(), 1, "记忆原样保留");
}

#[test]
fn directed_geometry_short_circuits_extreme_coordinates_without_reading() {
    let positions = [
        [i32::MAX, i32::MAX, i32::MAX],
        [i32::MIN, i32::MIN, i32::MIN],
    ];
    let result = project_directed(
        &pose(0.0),
        &positions,
        |_position| panic!("被几何拒绝的定向目标不得读世界"),
        &options(),
        world_bounds(),
        || Ok(()),
    )
    .expect("极端定向坐标应零读取分类");

    assert!(result.seen.is_empty());
    assert_eq!(result.unseen.len(), positions.len());
    assert!(result.unseen.iter().all(|item| {
        item.why
            == [
                DirectedWhy::OutsideFov,
                DirectedWhy::TooFar,
                DirectedWhy::OutOfWorld,
            ]
            && item.distance.is_some()
            && item.max == Some(options().max_distance)
            && item.by.is_none()
    }));
}

#[test]
fn directed_clear_to_unloaded_target_reports_only_chunk_not_loaded() {
    let target = [0, 2, -2];
    let result = project_directed(
        &pose(0.0),
        &[target],
        |position| {
            if [position.x, position.y, position.z] == target {
                BlockReadResult::Unloaded
            } else {
                BlockReadResult::Loaded {
                    block: block("air", true),
                }
            }
        },
        &options(),
        world_bounds(),
        || Ok(()),
    )
    .expect("clear unloaded target should be classified");

    assert!(result.seen.is_empty());
    assert_eq!(result.unseen.len(), 1);
    assert_eq!(result.unseen[0].at, target);
    assert_eq!(result.unseen[0].why, [DirectedWhy::ChunkNotLoaded]);
    assert!(result.unseen[0].by.is_none());
}

#[test]
fn directed_exposed_face_matches_full_when_target_centre_is_blocked() {
    let target = BlockPosition { x: 1, y: 2, z: -2 };
    let wall = BlockPosition { x: 1, y: 2, z: -1 };
    let read = |position: BlockPosition| {
        if position == target || position == wall {
            BlockReadResult::Loaded {
                block: block("stone", false),
            }
        } else {
            BlockReadResult::Loaded {
                block: block("air", true),
            }
        }
    };

    let mut center_read =
        WorldReader::new(|position| BlockProbe::from_read(&read(position)), &read);
    let center_hit = first_occluder_before_target(
        &mut center_read,
        Point3 {
            x: 0.5,
            y: 2.62,
            z: 0.5,
        },
        Point3 {
            x: 1.5,
            y: 2.5,
            z: -1.5,
        },
        &target,
        &mut || Ok(()),
    )
    .expect("centre ray should be classifiable");
    assert!(matches!(
        center_hit,
        DirectedRayOutcome::Hit(BlockHit { ref voxel, .. }) if *voxel == wall
    ));

    let full = project(&pose(0.0), &[], read, &options())
        .expect("full projection should use the exposed-face predicate");
    assert!(full
        .visible_blocks
        .blocks
        .iter()
        .any(|item| item.position == [target.x, target.y, target.z]));

    let directed = project_directed(
        &pose(0.0),
        &[[target.x, target.y, target.z]],
        read,
        &options(),
        world_bounds(),
        || Ok(()),
    )
    .expect("directed projection should reuse the full visibility predicate");
    assert_eq!(directed.seen.len(), 1);
    assert_eq!(directed.seen[0].at, [target.x, target.y, target.z]);
    assert!(directed.unseen.is_empty());
}

#[test]
fn directed_kernel_reports_too_far_fields_and_stable_combined_reason_order() {
    let result = project_directed(
        &pose(0.0),
        &[[0, 2, -40], [40, 2, 0]],
        |_position| BlockReadResult::Loaded {
            block: block("air", true),
        },
        &options(),
        world_bounds(),
        || Ok(()),
    )
    .expect("far fixture should be classifiable");
    let too_far = result
        .unseen
        .iter()
        .find(|item| item.at == [0, 2, -40])
        .expect("超距目标应在报告中");
    assert_eq!(too_far.why, [DirectedWhy::TooFar]);
    assert!(too_far.distance.unwrap() > too_far.max.unwrap());
    let combined = result
        .unseen
        .iter()
        .find(|item| item.at == [40, 2, 0])
        .expect("组合原因目标应在报告中");
    assert_eq!(combined.why, [DirectedWhy::OutsideFov, DirectedWhy::TooFar]);
    assert!(combined.by.is_none());
}

#[test]
fn directed_kernel_rejects_duplicate_input_and_keeps_target_out_of_world_per_row() {
    let duplicate = project_directed(
        &pose(0.0),
        &[[1, 2, -1], [1, 2, -1]],
        fixture_read,
        &options(),
        world_bounds(),
        || Ok(()),
    );
    assert!(matches!(duplicate, Err(error) if error.field == "positions"));

    let out_of_world = project_directed(
        &pose(0.0),
        &[[0, 2, -1]],
        |_position| BlockReadResult::OutOfWorld,
        &options(),
        world_bounds(),
        || Ok(()),
    )
    .expect("目标读出 OutOfWorld 必须成为一行结果");
    assert!(out_of_world.seen.is_empty());
    assert_eq!(out_of_world.unseen.len(), 1);
    assert_eq!(out_of_world.unseen[0].why, [DirectedWhy::OutOfWorld]);
    assert!(out_of_world.unseen[0].by.is_none());
    assert!(out_of_world.unseen[0].distance.is_none());
    assert!(out_of_world.unseen[0].max.is_none());
}

#[test]
fn directed_world_height_bounds_lock_lower_and_upper_edges() {
    let mut lower_pose = pose(0.0);
    lower_pose.position.y = -64.0;
    lower_pose.pitch = -35.0;
    let lower = project_directed(
        &lower_pose,
        &[[0, -65, -3], [0, -64, -3]],
        |_position| BlockReadResult::Loaded {
            block: block("air", true),
        },
        &options(),
        world_bounds(),
        || Ok(()),
    )
    .expect("lower world-height edge should remain a row-wise result");
    assert_eq!(lower.unseen.len(), 1);
    assert_eq!(lower.unseen[0].at, [0, -65, -3]);
    assert_eq!(lower.unseen[0].why, [DirectedWhy::OutOfWorld]);
    assert!(lower.seen.iter().any(|item| item.at == [0, -64, -3]));

    let mut upper_pose = pose(0.0);
    upper_pose.position.y = 319.0;
    let upper = project_directed(
        &upper_pose,
        &[[0, 319, -3], [0, 320, -3]],
        |_position| BlockReadResult::Loaded {
            block: block("air", true),
        },
        &options(),
        world_bounds(),
        || Ok(()),
    )
    .expect("upper world-height edge should remain a row-wise result");
    assert_eq!(upper.unseen.len(), 1);
    assert_eq!(upper.unseen[0].at, [0, 320, -3]);
    assert_eq!(upper.unseen[0].why, [DirectedWhy::OutOfWorld]);
    assert!(upper.seen.iter().any(|item| item.at == [0, 319, -3]));
}

#[test]
fn directed_out_of_world_geometry_short_circuits_and_mixes_with_other_rows() {
    let mut observation_pose = pose(0.0);
    observation_pose.position.y = 0.5;
    let bounds = WorldHeightBounds::new(-64, 66);
    let result = project_directed(
        &observation_pose,
        &[[0, 1, -2], [0, 2, -4], [5, 1, 0], [40, 2, 0]],
        |position| {
            assert_ne!(
                [position.x, position.y, position.z],
                [0, 2, -4],
                "距离内的出界目标不得被读取"
            );
            assert_ne!(
                [position.x, position.y, position.z],
                [40, 2, 0],
                "被几何拒绝的出界目标不得被读取"
            );
            BlockReadResult::Loaded {
                block: block("air", true),
            }
        },
        &options(),
        bounds,
        || Ok(()),
    )
    .expect("混合定向行不得整批失败");

    assert!(result.seen.iter().any(|item| item.at == [0, 1, -2]));
    let out_of_world = result
        .unseen
        .iter()
        .find(|item| item.at == [0, 2, -4])
        .expect("距离内出界目标应在报告中");
    assert_eq!(out_of_world.why, [DirectedWhy::OutOfWorld]);
    assert!(out_of_world.by.is_none());

    let combined = result
        .unseen
        .iter()
        .find(|item| item.at == [40, 2, 0])
        .expect("被几何拒绝的出界目标应在报告中");
    assert_eq!(
        combined.why,
        [
            DirectedWhy::OutsideFov,
            DirectedWhy::TooFar,
            DirectedWhy::OutOfWorld
        ]
    );
    assert!(combined.distance.is_some());
    assert_eq!(combined.max, Some(options().max_distance));
    assert!(combined.by.is_none());

    let outside = result
        .unseen
        .iter()
        .find(|item| item.at == [5, 1, 0])
        .expect("普通不可见目标在混合批中应保留");
    assert_eq!(outside.why, [DirectedWhy::OutsideFov]);
}

#[test]
fn directed_target_out_of_world_wins_over_inconsistent_test_bounds() {
    let target = [0, 2, -1];
    let result = project_directed(
        &pose(0.0),
        &[target],
        |position| {
            assert_eq!([position.x, position.y, position.z], target);
            BlockReadResult::OutOfWorld
        },
        &options(),
        world_bounds(),
        || Ok(()),
    )
    .expect("target OutOfWorld must not become a batch error");
    assert_eq!(result.unseen[0].why, [DirectedWhy::OutOfWorld]);
    assert!(result.unseen[0].by.is_none());
}

// ---- perception oracle 几何断言（自主仓库 perception.ts 测试移植而来）----

fn pose_at(x: f64, y: f64, z: f64) -> Pose {
    Pose {
        position: Vec3Value { x, y, z },
        yaw: 0.0,
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
        looked_at_max_distance: 4.5,
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
