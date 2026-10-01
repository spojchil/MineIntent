//! Protocol world -> bounded scene -> PNG; expensive work stays off async workers.
use std::sync::{Arc, Mutex};

use perception::{Picture, PictureDoor, Region};
use world::Module;

pub struct ModulePictureDoor {
    module: Arc<Module>,
    resources: Arc<Mutex<vision::Resources>>,
    /// 屏幕像素尺寸（宽, 高），即这具身体的「显示器」。
    screen: [u32; 2],
}

impl ModulePictureDoor {
    pub fn new(module: Arc<Module>, resources: vision::Resources, screen: [u32; 2]) -> Self {
        Self {
            module,
            resources: Arc::new(Mutex::new(resources)),
            screen,
        }
    }
}

/// 屏幕比例换成像素矩形 `[x, y, 宽, 高]`：左上向下取整、右下向上取整，至少 1 像素。
fn crop_pixels(region: Region, [width, height]: [u32; 2]) -> [u32; 4] {
    let span = |start: f64, size: f64, total: u32| {
        let total_f = f64::from(total);
        let from = ((start * total_f).floor() as u32).min(total - 1);
        let to = (((start + size) * total_f).ceil() as u32).clamp(from + 1, total);
        (from, to - from)
    };
    let (x, w) = span(region.x, region.width, width);
    let (y, h) = span(region.y, region.height, height);
    [x, y, w, h]
}

/// 屏幕尺寸配置 `宽x高`（如 `1280x720`），每边 1–2048。
pub fn parse_screen(text: &str) -> Result<[u32; 2], String> {
    let invalid = || format!("屏幕尺寸要写成 宽x高（如 1280x720，每边 1–2048），收到 {text:?}");
    let (width, height) = text.split_once(['x', 'X']).ok_or_else(invalid)?;
    let side = |value: &str| {
        value
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|n| (1..=2048).contains(n))
            .ok_or_else(invalid)
    };
    Ok([side(width)?, side(height)?])
}

impl PictureDoor for ModulePictureDoor {
    fn capture<'a>(
        &'a self,
        region: Option<Region>,
    ) -> agent::PortFuture<'a, Result<Picture, String>> {
        let module = self.module.clone();
        let resources = self.resources.clone();
        let [width, height] = self.screen;
        let crop = region.map(|region| crop_pixels(region, self.screen));
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let mut resources = resources.lock().map_err(|_| "图片资源暂不可用")?;
                // One bounded copy, no polling: let the caller choose when to retry.
                let region = module.capture_view()?;
                let view_distance = region.view_distance;
                let scene = scene_from_region(region)?;
                let options = vision::Options {
                    width,
                    height,
                    crop,
                    ..vision::Options::default()
                };
                let frame = vision::render(&scene, &mut resources, options)?;
                let framing = match crop {
                    None => format!("整个屏幕 {width}×{height}，正中是准星"),
                    Some([x, y, w, h]) => format!(
                        "屏幕（{width}×{height}）上的一块：左上 ({x}, {y}) 起 {w}×{h} 像素；\
准星在整屏正中 ({}, {})",
                        width / 2,
                        height / 2
                    ),
                };
                Ok(Picture {
                    png: frame.png()?,
                    // Reports describe the entire captured region and can name occluded
                    // blocks. Do not send that inventory or raw scene to the model.
                    description: format!(
                        "当前朝向的第一人称画面，{framing}。视距 {view_distance} 区块，远处渐隐入雾。\
画出方块、玩家、掉落物和常见生物（猪、牛、羊、鸡、苦力怕、蜘蛛、僵尸、骷髅）；\
生物是静止姿态、默认花色，不画装备、手持物、界面、天气和粒子；\
明暗按服务端光照与当前时刻，天空有昼夜与日月星辰；不画云；植物与水按生物群系染色，水面是平的。\
紫黑块表示模型或资源尚未支持的方块或实体（大小即其碰撞箱）；未绘制及视距外内容不代表不存在。\
视点采用站姿眼高 1.62 格，姿态与方块采集并非同一服务端 tick 的原子快照。"
                    ),
                })
            })
            .await
            .map_err(|_| "图片生成任务失败，请稍后再看".to_owned())?
        })
    }
}

fn scene_from_region(region: world::BlockRegion) -> Result<vision::Scene, String> {
    if region.unloaded != 0 {
        return Err("周围区块尚未加载完整，暂时无法生成可靠图片，请稍后再看".to_owned());
    }
    let pose = &region.snapshot.self_state;
    Ok(vision::Scene {
        game_version: "26.1.2".to_owned(),
        camera: vision::Camera {
            eye: [
                pose.position.x,
                pose.position.y + world::EYE_HEIGHT,
                pose.position.z,
            ],
            yaw: pose.yaw,
            pitch: pose.pitch,
            vertical_fov: 70.0,
        },
        blocks: region
            .blocks
            .into_iter()
            .map(|block| vision::Block {
                position: [
                    block.block.position.x,
                    block.block.position.y,
                    block.block.position.z,
                ],
                name: block.block.name,
                properties: block.block.properties,
                opaque: !block.block.transparent_hint,
                covered: Some(block.covered),
            })
            .collect(),
        entities: region
            .snapshot
            .entities
            .iter()
            .filter(|entity| entity.valid)
            .map(|entity| vision::Entity {
                kind: entity.entity_type.clone(),
                position: [entity.position.x, entity.position.y, entity.position.z],
                body_yaw: entity.yaw,
                head_yaw: entity.head_yaw,
                pitch: entity.pitch,
                width: entity.width,
                height: entity.height,
                uuid: entity.uuid.clone(),
                item: entity.item_name.clone(),
            })
            .collect(),
        cells: region
            .cells
            .iter()
            .map(|cell| vision::Cell {
                position: cell.position,
                sky_light: cell.sky_light,
                block_light: cell.block_light,
                emission: cell.emission,
                dampening: cell.dampening,
                view_blocking: cell.view_blocking,
                solid_render: cell.solid_render,
                emissive: cell.emissive,
                full_collision: cell.full_collision,
            })
            .collect(),
        biome_names: region.biome_names.clone(),
        biomes: region
            .biomes
            .iter()
            .map(|cell| vision::BiomeCell {
                quart: cell.quart,
                biome: cell.biome,
            })
            .collect(),
        environment: Some(vision::Environment {
            view_distance: region.view_distance,
            horizon_height: region.horizon_height,
            clock_ticks: region.clock_ticks,
            biome_quart_y: region.biome_quart_y,
            biome_zoom_seed: region.biome_zoom_seed,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_uses_body_pose_and_rejects_unloaded_regions() {
        let mut snapshot =
            world::TickSnapshot::empty(world::Epoch(1), 23, world::ConnectionPhase::Ready);
        snapshot.self_state.position = world::Vec3Value {
            x: 12.5,
            y: 64.0,
            z: -3.0,
        };
        snapshot.self_state.yaw = 91.0;
        snapshot.self_state.pitch = -17.0;
        let mut region = world::BlockRegion {
            snapshot: Arc::new(snapshot),
            blocks: vec![],
            cells: vec![],
            biome_names: vec![],
            biomes: vec![],
            biome_quart_y: [-16, 79],
            biome_zoom_seed: 0,
            unloaded: 1,
            view_distance: 6,
            horizon_height: 63.0,
            clock_ticks: 0,
        };
        assert!(scene_from_region(region.clone()).is_err());
        region.unloaded = 0;
        let scene = scene_from_region(region).unwrap();
        assert_eq!(scene.camera.eye, [12.5, 65.62, -3.0]);
        assert_eq!(scene.camera.yaw, 91.0);
        assert_eq!(scene.camera.pitch, -17.0);
    }

    #[test]
    fn region_fractions_become_pixel_rectangles_that_cover_the_request() {
        let screen = [1280, 720];
        let region = |x, y, width, height| Region {
            x,
            y,
            width,
            height,
        };
        assert_eq!(
            crop_pixels(region(0.0, 0.0, 1.0, 1.0), screen),
            [0, 0, 1280, 720]
        );
        assert_eq!(
            crop_pixels(region(0.0, 0.66, 1.0, 0.34), screen),
            [0, 475, 1280, 245]
        );
        assert_eq!(
            crop_pixels(region(0.5, 0.5, 1e-9, 1e-9), screen),
            [640, 360, 1, 1]
        );
        // 容差内略超出屏幕的右下边缘截在屏幕边上。
        assert_eq!(
            crop_pixels(region(0.0, 0.66, 1.0, 0.340_000_5), screen),
            [0, 475, 1280, 245]
        );
    }

    #[test]
    fn screen_size_parses_width_by_height() {
        assert_eq!(parse_screen("1280x720"), Ok([1280, 720]));
        assert_eq!(parse_screen("640X360"), Ok([640, 360]));
        for bad in ["1280", "0x720", "4096x720", "ax720", ""] {
            assert!(parse_screen(bad).is_err(), "{bad}");
        }
    }
}
