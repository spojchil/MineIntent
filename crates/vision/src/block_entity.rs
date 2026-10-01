//! 方块实体成像：箱子、床、潜影盒、钟身、讲台与附魔台上的书。
//!
//! 几何不在这里转录：`data/BlockEntityModelDump.java` 在 26.1.2 客户端上用原版模型层、
//! 原版渲染器的摆放变换与 `ModelPart.render` 导出每个方块状态的四边形（方块内坐标、
//! 贴图比例 UV、法线），这里只按方块名与决定几何的属性查表、平移到方块位置。
//! 取静止状态：盖子关闭、钟不摇、附魔台的书合着朝向 0（原版随时间与附近玩家而动）。
//! 光照取方块所在格（原版 `getLightCoords(level, pos)`）；双箱两半各取各的格，
//! 原版取两格较亮者（`BrightnessCombiner`）。明暗沿用实体的近似，不是原版实体光照。
//! 依赖服务端方块实体数据的内容（告示牌文字、旗帜图案、头颅皮肤、陶罐纹饰、
//! 架上物品等）尚未接入；圣诞节期间原版箱子换皮，这里不换。

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::geometry::{Triangle, V3};
use crate::light::{Cells, FULL_BRIGHT};
use crate::{Block, Report, Resources};

#[derive(Deserialize)]
struct Data {
    blocks: HashMap<String, Family>,
    geometries: HashMap<String, Vec<Quad>>,
}

#[derive(Deserialize)]
struct Family {
    properties: Vec<String>,
    variants: HashMap<String, Variant>,
}

#[derive(Deserialize)]
struct Variant {
    texture: String,
    geometry: String,
}

/// 四个顶点 `[x, y, z, u, v]` 与法线。
type Quad = ([[f64; 5]; 4], V3);

fn data() -> &'static Data {
    static DATA: OnceLock<Data> = OnceLock::new();
    DATA.get_or_init(|| {
        serde_json::from_str(include_str!("../data/block_entity_models_26.1.2.json"))
            .expect("随源码入库的方块实体几何表可解析")
    })
}

fn short(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

/// 这个方块有没有方块实体几何（有就不该因为方块模型为空而画占位块）。
pub(crate) fn has(name: &str) -> bool {
    data().blocks.contains_key(short(name))
}

fn variant(block: &Block) -> Option<&'static Variant> {
    let family = data().blocks.get(short(&block.name))?;
    let key = family
        .properties
        .iter()
        .map(|name| {
            let value = block.properties.get(name).map_or("", String::as_str);
            format!("{name}={value}")
        })
        .collect::<Vec<_>>()
        .join(",");
    family.variants.get(&key)
}

/// 方块实体部分的三角形；这个状态不画方块实体时为空。
pub(crate) fn triangles(
    block: &Block,
    cells: Option<&Cells>,
    resources: &mut Resources,
    report: &mut Report,
) -> Vec<Triangle> {
    let Some(variant) = variant(block) else {
        return Vec::new();
    };
    let Some(quads) = data().geometries.get(&variant.geometry) else {
        return Vec::new();
    };
    report.warnings.insert(
        "block entities: static pose (lids closed, bell still, enchanting book closed); signs, banners, heads, pots and other server-data contents not drawn yet".to_owned(),
    );
    let texture = resources.texture(&variant.texture).unwrap_or_else(|error| {
        report.warnings.insert(format!(
            "{}: block entity texture unavailable ({error})",
            block.name
        ));
        crate::assets::missing_texture()
    });
    let light = cells.map_or(FULL_BRIGHT, |cells| {
        cells.coords(cells.get(block.position), block.position)
    });
    let offset = block.position.map(f64::from);
    let mut triangles = Vec::with_capacity(quads.len() * 2);
    for (vertices, normal) in quads {
        let shade = crate::entity::shade(*normal);
        let position = |i: usize| -> V3 { std::array::from_fn(|k| vertices[i][k] + offset[k]) };
        // 光栅器按「贴图宽 16 单位」采样。
        let uv = |i: usize| [vertices[i][3] * 16.0, vertices[i][4] * 16.0];
        for indices in [[0, 1, 2], [0, 2, 3]] {
            triangles.push(Triangle {
                vertices: indices.map(position),
                uv: indices.map(uv),
                texture: texture.clone(),
                color: [[shade; 3]; 3],
                light: [light; 3],
                alpha: 1.0,
            });
        }
    }
    triangles
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(name: &str, properties: &[(&str, &str)]) -> Block {
        Block {
            position: [10, 64, -3],
            name: name.to_owned(),
            properties: properties
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            opaque: false,
            covered: None,
        }
    }

    fn bounds(block: &Block) -> (V3, V3) {
        let variant = variant(block).expect("有这个状态");
        let quads = &data().geometries[&variant.geometry];
        let mut min = [f64::MAX; 3];
        let mut max = [f64::MIN; 3];
        for (vertices, _) in quads {
            for vertex in vertices {
                for k in 0..3 {
                    min[k] = min[k].min(vertex[k]);
                    max[k] = max[k].max(vertex[k]);
                }
            }
        }
        (min, max)
    }

    #[test]
    fn exported_geometry_has_vanilla_extents_and_textures() {
        // 单箱：14×14 像素见方、高 14；朝北时锁扣伸到 z=0。
        let chest = block(
            "minecraft:chest",
            &[
                ("facing", "north"),
                ("type", "single"),
                ("waterlogged", "false"),
            ],
        );
        let (min, max) = bounds(&chest);
        assert_eq!(
            (min[0], max[0], max[1]),
            (1.0 / 16.0, 15.0 / 16.0, 14.0 / 16.0)
        );
        assert_eq!(min[2], 0.0);
        assert_eq!(
            variant(&chest).unwrap().texture,
            "minecraft:entity/chest/normal"
        );
        let left = block("chest", &[("facing", "north"), ("type", "left")]);
        assert_eq!(
            variant(&left).unwrap().texture,
            "minecraft:entity/chest/normal_left"
        );
        let waxed = block(
            "waxed_oxidized_copper_chest",
            &[("facing", "east"), ("type", "single")],
        );
        assert_eq!(
            variant(&waxed).unwrap().texture,
            "minecraft:entity/chest/copper_oxidized"
        );
        // 床高 9 像素；潜影盒占满一格（原版缩到 0.9995 防 Z 冲突）。
        let bed = block(
            "red_bed",
            &[("facing", "south"), ("part", "foot"), ("occupied", "false")],
        );
        assert!((bounds(&bed).1[1] - 9.0 / 16.0).abs() < 1e-6);
        assert_eq!(variant(&bed).unwrap().texture, "minecraft:entity/bed/red");
        let box_ = block("shulker_box", &[("facing", "up")]);
        let (min, max) = bounds(&box_);
        assert!(min.iter().all(|c| c.abs() < 1e-3) && max.iter().all(|c| (c - 1.0).abs() < 1e-3));
        // 讲台只在有书时画书。
        assert!(variant(&block(
            "lectern",
            &[("facing", "north"), ("has_book", "false")]
        ))
        .is_none());
        assert!(variant(&block(
            "lectern",
            &[("facing", "north"), ("has_book", "true")]
        ))
        .is_some());
        assert!(has("bell") && has("minecraft:enchanting_table") && !has("stone"));
    }
}
