//! Protocol world -> bounded scene -> PNG; expensive work stays off async workers.
use std::sync::{Arc, Mutex};

use perception::{Picture, PictureDoor};
use world::Module;

pub struct ModulePictureDoor {
    module: Arc<Module>,
    resources: Arc<Mutex<vision::Resources>>,
}

impl ModulePictureDoor {
    pub fn new(module: Arc<Module>, resources: vision::Resources) -> Self {
        Self {
            module,
            resources: Arc::new(Mutex::new(resources)),
        }
    }
}

impl PictureDoor for ModulePictureDoor {
    fn capture<'a>(&'a self) -> agent::PortFuture<'a, Result<Picture, String>> {
        let module = self.module.clone();
        let resources = self.resources.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let mut resources = resources.lock().map_err(|_| "图片资源暂不可用")?;
                // One bounded copy, no polling: let the caller choose when to retry.
                let region = module.capture_view()?;
                let view_distance = region.view_distance;
                let scene = scene_from_region(region)?;
                let frame = vision::render(&scene, &mut resources, vision::Options::default())?;
                Ok(Picture {
                    png: frame.png()?,
                    // Reports describe the entire captured region and can name occluded
                    // blocks. Do not send that inventory or raw scene to the model.
                    description: format!(
                        "当前朝向的第一人称画面，视距 {view_distance} 区块，远处渐隐入雾，640×360，正中是准星。\
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
}
