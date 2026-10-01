//! 生物群系染色：原版 26.1.2 客户端的 `BlockColors`、`BiomeColors`、
//! `ClientLevel.calculateBlockTint`、`BiomeManager.getBiome` 与 `Biome` 的取色转录。
//!
//! 场景给出生物群系格（4×4×4，原版 quart）与缩放种子；生物群系的气候与特效从客户端
//! jar 的 `data/minecraft/worldgen/biome/*.json` 读，草与树叶色图取 `textures/colormap`。
//! 每格的生物群系经模糊缩放从相邻 quart 里挑，染色取同一 y 上 5×5 格的整数平均
//! （`biomeBlendRadius` 默认 2）。

use std::collections::HashMap;
use std::sync::Arc;

use image::RgbaImage;
use serde_json::Value;

use crate::geometry::V3;
use crate::random::{JavaRandom, SimplexNoise};
use crate::{Report, Resources, Scene};

/// 查不到的格：原版客户端对没加载的区块返回平原。
const FALLBACK: &str = "minecraft:plains";
/// 原版 `biomeBlendRadius` 默认值。
const BLEND_RADIUS: i32 = 2;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Resolver {
    Grass,
    Foliage,
    DryFoliage,
    Water,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GrassModifier {
    None,
    DarkForest,
    Swamp,
}

#[derive(Clone, Debug)]
struct Definition {
    /// 原版存为 float，取色时先夹再升成 double；照样保留 f32 精度。
    temperature: f32,
    downfall: f32,
    grass: Option<u32>,
    foliage: Option<u32>,
    dry_foliage: Option<u32>,
    water: u32,
    modifier: GrassModifier,
    attributes: Value,
}

impl Definition {
    fn load(resources: &mut Resources, name: &str) -> Result<Self, String> {
        let path = name.strip_prefix("minecraft:").unwrap_or(name);
        let json = resources.json(&format!("data/minecraft/worldgen/biome/{path}.json"))?;
        let effects = &json["effects"];
        let color = |key: &str| effects.get(key).and_then(parse_rgb);
        Ok(Self {
            temperature: json["temperature"].as_f64().unwrap_or(0.5) as f32,
            downfall: json["downfall"].as_f64().unwrap_or(0.5) as f32,
            grass: color("grass_color"),
            foliage: color("foliage_color"),
            dry_foliage: color("dry_foliage_color"),
            water: color("water_color").unwrap_or(0x3F76E4),
            modifier: match effects["grass_color_modifier"].as_str() {
                Some("dark_forest") => GrassModifier::DarkForest,
                Some("swamp") => GrassModifier::Swamp,
                _ => GrassModifier::None,
            },
            attributes: json["attributes"].clone(),
        })
    }
}

/// `"#rrggbb"` 或整数 → 0xRRGGBB。
fn parse_rgb(value: &Value) -> Option<u32> {
    if let Some(i) = value.as_i64() {
        return Some(i as u32 & 0xFF_FFFF);
    }
    u32::from_str_radix(value.as_str()?.strip_prefix('#')?, 16)
        .ok()
        .map(|c| c & 0xFF_FFFF)
}

/// 草、树叶、落叶三张 256×256 色图（原版 `GrassColor` / `FoliageColor` / `DryFoliageColor`）。
struct ColorMaps {
    grass: Arc<RgbaImage>,
    foliage: Arc<RgbaImage>,
    dry_foliage: Arc<RgbaImage>,
}

impl ColorMaps {
    fn load(resources: &mut Resources) -> Result<Self, String> {
        Ok(Self {
            grass: resources.texture("minecraft:colormap/grass")?,
            foliage: resources.texture("minecraft:colormap/foliage")?,
            dry_foliage: resources.texture("minecraft:colormap/dry_foliage")?,
        })
    }
}

/// `ColorMapColorUtil.get`：湿度先乘温度，再按 (1-温度, 1-湿度) 查表。
fn colormap(map: &RgbaImage, temperature: f32, downfall: f32) -> u32 {
    let temp = f64::from(temperature.clamp(0.0, 1.0));
    let rain = f64::from(downfall.clamp(0.0, 1.0)) * temp;
    let x = ((1.0 - temp) * 255.0) as u32;
    let y = ((1.0 - rain) * 255.0) as u32;
    if x >= map.width() || y >= map.height() {
        return 0xFF00FF;
    }
    let p = map.get_pixel(x, y).0;
    u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2])
}

/// 混色缓存的键：方块位置与取色方式。
type TintKey = ([i32; 3], Resolver);

/// 每个出几何的线程一份（原版 `ClientLevel.tintCaches` 也按线程分）：混好的颜色，以及
/// 各位置经模糊缩放选中的生物群系——相邻方块的 5×5 窗口大面积重叠，同一位置会被反复
/// 取样；[`Biomes::biome_at`] 是位置的纯函数，缓存不改结果。
#[derive(Default)]
pub(crate) struct TintCache {
    blends: HashMap<TintKey, [u8; 3]>,
    biomes: crate::geometry::PositionMap<usize>,
}

impl TintCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

pub(crate) struct Biomes {
    definitions: Vec<Definition>,
    fallback: usize,
    cells: crate::geometry::PositionMap<usize>,
    quart_y: [i32; 2],
    seed: i64,
    maps: ColorMaps,
    swamp: SimplexNoise,
}

impl Biomes {
    /// 场景没有生物群系格时（模型夹具、测试）一律按平原。
    pub(crate) fn new(
        scene: &Scene,
        resources: &mut Resources,
        report: &mut Report,
    ) -> Result<Self, String> {
        let maps = ColorMaps::load(resources)?;
        let mut definitions = Vec::new();
        let mut by_name: HashMap<&str, usize> = HashMap::new();
        let mut load = |name: &str, definitions: &mut Vec<Definition>| -> Option<usize> {
            match Definition::load(resources, name) {
                Ok(definition) => {
                    definitions.push(definition);
                    Some(definitions.len() - 1)
                }
                Err(error) => {
                    report
                        .warnings
                        .insert(format!("biome {name}: {error}; drawn as plains"));
                    None
                }
            }
        };
        let fallback = load(FALLBACK, &mut definitions).ok_or("plains biome definition missing")?;
        by_name.insert(FALLBACK, fallback);
        let mut index_of_name = Vec::with_capacity(scene.biome_names.len());
        for name in &scene.biome_names {
            let index = match by_name.get(name.as_str()) {
                Some(&index) => index,
                None => {
                    let index = load(name, &mut definitions).unwrap_or(fallback);
                    by_name.insert(name, index);
                    index
                }
            };
            index_of_name.push(index);
        }
        let cells = scene
            .biomes
            .iter()
            .map(|cell| {
                let index = index_of_name
                    .get(usize::from(cell.biome))
                    .copied()
                    .unwrap_or(fallback);
                (cell.quart, index)
            })
            .collect();
        let (quart_y, seed) = scene
            .environment
            .as_ref()
            .map_or(([i32::MIN, i32::MAX], 0), |e| {
                (e.biome_quart_y, e.biome_zoom_seed)
            });
        Ok(Self {
            definitions,
            fallback,
            cells,
            quart_y,
            seed,
            maps,
            // 原版 `Biome.BIOME_INFO_NOISE`：种子 2345、只有第 0 八度的 PerlinSimplexNoise。
            swamp: SimplexNoise::new(&mut JavaRandom::new(2345)),
        })
    }

    /// `getNoiseBiomeAtQuart`：y 夹进范围，查不到按平原。
    fn noise_biome(&self, [x, y, z]: [i32; 3]) -> usize {
        let y = y.clamp(self.quart_y[0], self.quart_y[1]);
        self.cells.get(&[x, y, z]).copied().unwrap_or(self.fallback)
    }

    /// `BiomeManager.getBiome`：在相邻 8 个 quart 里挑抖动距离最近的那个。
    fn biome_at(&self, position: [i32; 3]) -> usize {
        crate::counters::add(crate::counters::Counter::BiomeSamples, 1);
        let absolute = position.map(|v| v - 2);
        let parent = absolute.map(|v| v >> 2);
        let fraction = absolute.map(|v| f64::from(v & 3) / 4.0);
        let mut best = 0;
        let mut best_distance = f64::INFINITY;
        for i in 0..8 {
            let corner: [i32; 3] =
                std::array::from_fn(|axis| parent[axis] + i32::from(i & (4 >> axis) != 0));
            let distance: [f64; 3] = std::array::from_fn(|axis| {
                fraction[axis] - f64::from(u8::from(i & (4 >> axis) != 0))
            });
            let next = fiddled_distance(self.seed, corner, distance);
            if best_distance > next {
                best = i;
                best_distance = next;
            }
        }
        self.noise_biome(std::array::from_fn(|axis| {
            parent[axis] + i32::from(best & (4 >> axis) != 0)
        }))
    }

    /// 单个生物群系在 (x, z) 的颜色（0xRRGGBB），原版 `ColorResolver`。
    fn resolve(&self, biome: usize, resolver: Resolver, x: i32, z: i32) -> u32 {
        let definition = &self.definitions[biome];
        let from_map = |map: &RgbaImage| colormap(map, definition.temperature, definition.downfall);
        match resolver {
            Resolver::Grass => {
                let base = definition
                    .grass
                    .unwrap_or_else(|| from_map(&self.maps.grass));
                match definition.modifier {
                    GrassModifier::None => base,
                    GrassModifier::DarkForest => ((base & 0xFE_FEFE) + 0x28_340A) >> 1,
                    GrassModifier::Swamp => {
                        let value = self
                            .swamp
                            .value(f64::from(x) * 0.0225, f64::from(z) * 0.0225);
                        if value < -0.1 {
                            0x4C_763C
                        } else {
                            0x6A_7039
                        }
                    }
                }
            }
            Resolver::Foliage => definition
                .foliage
                .unwrap_or_else(|| from_map(&self.maps.foliage)),
            Resolver::DryFoliage => definition
                .dry_foliage
                .unwrap_or_else(|| from_map(&self.maps.dry_foliage)),
            Resolver::Water => definition.water,
        }
    }

    /// `ClientLevel.calculateBlockTint`：同一 y 上 5×5 格逐通道整数平均。
    pub(crate) fn blended(
        &self,
        position: [i32; 3],
        resolver: Resolver,
        cache: &mut TintCache,
    ) -> V3 {
        crate::counters::add(crate::counters::Counter::TintLookups, 1);
        if let Some(rgb) = cache.blends.get(&(position, resolver)) {
            return rgb.map(|c| f64::from(c) / 255.0);
        }
        crate::counters::add(crate::counters::Counter::TintBlends, 1);
        let [x, y, z] = position;
        let mut total = [0u32; 3];
        for dz in -BLEND_RADIUS..=BLEND_RADIUS {
            for dx in -BLEND_RADIUS..=BLEND_RADIUS {
                let at = [x + dx, y, z + dz];
                let biome = *cache.biomes.entry(at).or_insert_with(|| self.biome_at(at));
                let color = self.resolve(biome, resolver, x + dx, z + dz);
                total[0] += color >> 16 & 0xFF;
                total[1] += color >> 8 & 0xFF;
                total[2] += color & 0xFF;
            }
        }
        let count = ((BLEND_RADIUS * 2 + 1) * (BLEND_RADIUS * 2 + 1)) as u32;
        let rgb = total.map(|c| (c / count) as u8);
        cache.blends.insert((position, resolver), rgb);
        rgb.map(|c| f64::from(c) / 255.0)
    }

    /// 一个模型面的染色（`BlockColors` 的 `colorInWorld`）。`index` 是模型的 `tintindex`。
    pub(crate) fn block_tint(
        &self,
        name: &str,
        properties: &std::collections::BTreeMap<String, String>,
        index: usize,
        position: [i32; 3],
        cache: &mut TintCache,
    ) -> V3 {
        match source(name, index) {
            Source::Biome(resolver) => self.blended(position, resolver, cache),
            Source::UpperHalfBelow => {
                let below = if properties.get("half").map(String::as_str) == Some("upper") {
                    [position[0], position[1] - 1, position[2]]
                } else {
                    position
                };
                self.blended(below, Resolver::Grass, cache)
            }
            Source::Fixed(color) => rgb(color),
            Source::InHandOnly { world, .. } => rgb(world),
            Source::Redstone => redstone(properties),
            Source::Stem => stem(properties),
            Source::None => [1.0; 3],
        }
    }

    /// 相机处的生物群系权重：`GaussianSampler` 在位置 ×0.25 处取 6³ 个 quart，同一生物群系
    /// 的权重相加，顺序为首次出现的顺序（原版 `SpatialAttributeInterpolator` 的累积顺序）。
    pub(crate) fn camera_weights(&self, eye: V3) -> Vec<(&Value, f64)> {
        const KERNEL: [f64; 7] = [0.0, 1.0, 4.0, 6.0, 4.0, 1.0, 0.0];
        let position = eye.map(|v| v * 0.25 - 0.5);
        let integral = position.map(|v| v.floor());
        let relative: [f64; 3] = std::array::from_fn(|i| position[i] - integral[i]);
        let base = integral.map(|v| v as i32);
        let weight = |axis: usize, step: usize| {
            KERNEL[step + 1] + relative[axis] * (KERNEL[step] - KERNEL[step + 1])
        };
        let mut weights: Vec<(usize, f64)> = Vec::new();
        for z in 0..6 {
            for x in 0..6 {
                for y in 0..6 {
                    let w = weight(0, x) * weight(1, y) * weight(2, z);
                    let biome = self.noise_biome([
                        base[0] - 2 + x as i32,
                        base[1] - 2 + y as i32,
                        base[2] - 2 + z as i32,
                    ]);
                    match weights.iter_mut().find(|(b, _)| *b == biome) {
                        Some(entry) => entry.1 += w,
                        None => weights.push((biome, w)),
                    }
                }
            }
        }
        weights
            .into_iter()
            .map(|(biome, w)| (&self.definitions[biome].attributes, w))
            .collect()
    }
}

/// `BiomeManager.getFiddledDistance`。
fn fiddled_distance(seed: i64, corner: [i32; 3], distance: [f64; 3]) -> f64 {
    let next = |rval: i64, c: i64| {
        rval.wrapping_mul(
            rval.wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407),
        )
        .wrapping_add(c)
    };
    let mut rval = seed;
    for c in [
        corner[0], corner[1], corner[2], corner[0], corner[1], corner[2],
    ] {
        rval = next(rval, i64::from(c));
    }
    let fiddle = |r: i64| ((r >> 24).rem_euclid(1024) as f64 / 1024.0 - 0.5) * 0.9;
    let fx = fiddle(rval);
    rval = next(rval, seed);
    let fy = fiddle(rval);
    rval = next(rval, seed);
    let fz = fiddle(rval);
    (distance[2] + fz).powi(2) + (distance[1] + fy).powi(2) + (distance[0] + fx).powi(2)
}

fn rgb(color: u32) -> V3 {
    [16, 8, 0].map(|shift| f64::from(color >> shift & 0xFF) / 255.0)
}

/// `BlockColors.createDefault` 的各层来源。
enum Source {
    Biome(Resolver),
    /// 高草、大型蕨：上半取下面一格的草色。
    UpperHalfBelow,
    Fixed(u32),
    /// 手持与世界里颜色不同（睡莲）。
    InHandOnly {
        hand: u32,
        world: u32,
    },
    Redstone,
    Stem,
    None,
}

fn source(name: &str, index: usize) -> Source {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    match (name, index) {
        ("large_fern" | "tall_grass", 0) => Source::UpperHalfBelow,
        ("fern" | "short_grass" | "potted_fern" | "bush" | "grass_block" | "sugar_cane", 0) => {
            Source::Biome(Resolver::Grass)
        }
        ("pink_petals" | "wildflowers", 1) => Source::Biome(Resolver::Grass),
        ("spruce_leaves", 0) => Source::Fixed(0x61_9961),
        ("birch_leaves", 0) => Source::Fixed(0x80_A755),
        (
            "oak_leaves" | "jungle_leaves" | "acacia_leaves" | "dark_oak_leaves" | "vine"
            | "mangrove_leaves",
            0,
        ) => Source::Biome(Resolver::Foliage),
        ("leaf_litter", 0) => Source::Biome(Resolver::DryFoliage),
        // 水方块的 tint 源只给粒子用；流体渲染另走 `FluidStateModelSet` 的水色。
        ("water" | "water_cauldron", 0) => Source::Biome(Resolver::Water),
        ("redstone_wire", 0) => Source::Redstone,
        ("attached_melon_stem" | "attached_pumpkin_stem", 0) => Source::Fixed(0xE0_C71C),
        ("melon_stem" | "pumpkin_stem", 0) => Source::Stem,
        ("lily_pad", 0) => Source::InHandOnly {
            hand: 0x71_C35C,
            world: 0x20_8030,
        },
        _ => Source::None,
    }
}

/// 手持与掉落物的染色（`BlockTintSource.color`，不看生物群系）。
pub(crate) fn item_tint(
    name: &str,
    properties: &std::collections::BTreeMap<String, String>,
    index: usize,
    resources: &mut Resources,
) -> V3 {
    let mut default_grass = || {
        resources
            .texture("minecraft:colormap/grass")
            .map(|map| colormap(&map, 0.5, 1.0))
            .unwrap_or(0x91_BD59)
    };
    match source(name, index) {
        Source::Biome(Resolver::Grass) | Source::UpperHalfBelow => {
            if name.ends_with("sugar_cane") {
                [1.0; 3]
            } else {
                rgb(default_grass())
            }
        }
        Source::Biome(Resolver::Foliage) => rgb(0x48_B518),
        Source::Biome(Resolver::DryFoliage) => rgb(0x5C_3B32),
        Source::Biome(Resolver::Water) => [1.0; 3],
        Source::Fixed(color) => rgb(color),
        Source::InHandOnly { hand, .. } => rgb(hand),
        Source::Redstone => redstone(properties),
        Source::Stem => stem(properties),
        Source::None => [1.0; 3],
    }
}

/// `RedStoneWireBlock.getColorForPower`。
fn redstone(properties: &std::collections::BTreeMap<String, String>) -> V3 {
    let power = properties
        .get("power")
        .and_then(|p| p.parse::<u8>().ok())
        .unwrap_or(0);
    let f = f32::from(power) / 15.0;
    let red = f * 0.6 + if f > 0.0 { 0.4 } else { 0.3 };
    let green = (f * f * 0.7 - 0.5).clamp(0.0, 1.0);
    let blue = (f * f * 0.6 - 0.7).clamp(0.0, 1.0);
    [red, green, blue].map(f64::from)
}

/// `BlockTintSources.stem`：按生长阶段由绿变黄。
fn stem(properties: &std::collections::BTreeMap<String, String>) -> V3 {
    let age = properties
        .get("age")
        .and_then(|p| p.parse::<u32>().ok())
        .unwrap_or(0);
    rgb((age * 32) << 16 | (255 - age * 8) << 8 | (age * 4))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colormap_lookup_keeps_float_precision() {
        // 温度 0.8f 升成 double 是 0.800000011920929：(1 - t) * 255 = 50.99… → 50；
        // 若按 0.8 的 double 算会得 51。
        // 湿度 0：y = (1 - 0) * 255 = 255。
        let mut map = RgbaImage::new(256, 256);
        map.put_pixel(50, 255, image::Rgba([1, 2, 3, 255]));
        map.put_pixel(51, 255, image::Rgba([9, 9, 9, 255]));
        assert_eq!(colormap(&map, 0.8, 0.0), 0x01_0203);
    }

    #[test]
    fn dark_forest_halves_toward_a_fixed_green() {
        let base = 0x79_C05A;
        assert_eq!(((base & 0xFE_FEFE) + 0x28_340A) >> 1, 0x50_7A32);
    }
}
