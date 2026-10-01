//! Independent, on-demand image rendering. No game simulation or window loop.
//! Geometry follows Minecraft's model resource conventions; original code and
//! assets are not embedded. See README for the deliberately incomplete effects.

mod assets;
mod biome;
mod block_entity;
mod daylight;
mod entity;
mod geometry;
mod light;
mod random;
mod raster;
mod sky;
#[cfg(test)]
mod trace;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

use image::{DynamicImage, ImageFormat, RgbaImage};
use serde::{Deserialize, Serialize};

pub use assets::Resources;
pub use entity::Entity;
pub use geometry::fixture;
pub use raster::render;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    pub position: [i32; 3],
    pub name: String,
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
    /// Only true for a full opaque block; used to omit neighbour-facing quads.
    #[serde(default)]
    pub opaque: bool,
    /// 采集方已算好的六面遮挡位（下、上、北、南、西、东）。给了就按它剔面，
    /// 不再查邻格——采集只交表面方块，被埋住的邻格不在场景里，查邻格会把贴着它们的面误画出来。
    #[serde(default)]
    pub covered: Option<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Camera {
    /// Eye position, not feet position. Minecraft yaw: 0 = south, 90 = west.
    pub eye: [f64; 3],
    pub yaw: f64,
    pub pitch: f64,
    pub vertical_fov: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scene {
    pub game_version: String,
    pub camera: Camera,
    pub blocks: Vec<Block>,
    /// 视野里的实体（不含观察者自己）。
    #[serde(default)]
    pub entities: Vec<Entity>,
    /// 视距与天空。没有时按旧口径：固定背景色、只按 `Options::far` 截断、不加雾、不算光照。
    #[serde(default)]
    pub environment: Option<Environment>,
    /// 光照格：原版平滑光照与环境光遮蔽要查的格子。查不到的格按露天空气。
    #[serde(default)]
    pub cells: Vec<Cell>,
    /// 生物群系注册表名（如 `minecraft:plains`），[`BiomeCell::biome`] 是它的下标。
    #[serde(default)]
    pub biome_names: Vec<String>,
    /// 染色与环境属性要查的 4×4×4 生物群系格。查不到的格按原版空区块，即平原。
    #[serde(default)]
    pub biomes: Vec<BiomeCell>,
}

/// 一个 4×4×4 生物群系格（原版 quart 坐标 = 方块坐标 >> 2）。
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct BiomeCell {
    pub quart: [i32; 3],
    pub biome: u16,
}

/// 一格的服务端光照与原版方块渲染属性。
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Cell {
    pub position: [i32; 3],
    /// 天空光、方块光，0..=15。
    pub sky_light: u8,
    pub block_light: u8,
    /// 本格方块的发光值与透光度，0..=15。
    pub emission: u8,
    pub dampening: u8,
    /// 原版 `isViewBlocking`、`isSolidRender`、`emissiveRendering`、`isCollisionShapeFullBlock`。
    pub view_blocking: bool,
    pub solid_render: bool,
    pub emissive: bool,
    pub full_collision: bool,
}

/// 原版的视距雾、天空与光照所需的世界状态。颜色与光照参数按主世界维度属性
/// （`dimension_type/overworld`）、相机处的生物群系、日时间轴（`timeline/day`）依次求值。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Environment {
    /// 实际视距（区块）。
    pub view_distance: u32,
    /// 原版地平线高度：眼睛低于它时，天空下半画黑盘。
    pub horizon_height: f64,
    /// 主世界时钟的累计 tick；各时间轴按自己的周期取模。
    pub clock_ticks: u64,
    /// 生物群系格的 y 范围（含两端）：原版查格时把 y 夹进去。
    #[serde(default = "full_quart_range")]
    pub biome_quart_y: [i32; 2],
    /// 登录/重生包的生物群系缩放种子（原版 `BiomeManager` 的模糊缩放用）。
    #[serde(default)]
    pub biome_zoom_seed: i64,
}

fn full_quart_range() -> [i32; 2] {
    [i32::MIN, i32::MAX]
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub width: u32,
    pub height: u32,
    pub far: f64,
    /// 画准星（画面正中、对底色取反）。几何对照测试关掉它，以免中心像素被覆盖。
    pub crosshair: bool,
    /// 只交出整屏里的这一块（左上 x、y 与宽高，像素）。投影与准星仍按整屏，
    /// 所以局部的每个像素和整屏图里同位置的像素相同。
    pub crop: Option<[u32; 4]>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            far: 48.0,
            crosshair: true,
            crop: None,
        }
    }
}

/// Machine-readable limitations accompany the image; unsupported blocks are
/// conspicuous checkerboard cubes, never silently omitted.
#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub blocks: usize,
    pub triangles: usize,
    pub warnings: BTreeSet<String>,
}

pub struct Frame {
    pub image: RgbaImage,
    pub report: Report,
}

impl Frame {
    /// Ready for a future image tool result, without a temporary file.
    pub fn png(&self) -> Result<Vec<u8>, String> {
        let mut output = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(self.image.clone())
            .write_to(&mut output, ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        Ok(output.into_inner())
    }
}

#[cfg(test)]
mod tests;
