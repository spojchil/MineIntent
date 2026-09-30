//! Independent, on-demand image rendering. No game simulation or window loop.
//! Geometry follows Minecraft's model resource conventions; original code and
//! assets are not embedded. See README for the deliberately incomplete effects.

mod assets;
mod entity;
mod geometry;
mod raster;
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
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub width: u32,
    pub height: u32,
    pub far: f64,
    /// 画准星（画面正中、对底色取反）。几何对照测试关掉它，以免中心像素被覆盖。
    pub crosshair: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            width: 640,
            height: 360,
            far: 48.0,
            crosshair: true,
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
