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
                let scene = scene_from_region(module.capture_blocks(16)?)?;
                let frame = vision::render(
                    &scene,
                    &mut resources,
                    vision::Options {
                        far: 16.0,
                        ..Default::default()
                    },
                )?;
                Ok(Picture {
                    png: frame.png()?,
                    // Reports describe the entire captured region and can name occluded
                    // blocks. Do not send that inventory or raw scene to the model.
                    description: "当前朝向的第一人称方块地形图，范围 16 格，640×360。\
图片不含玩家、生物、掉落物、手持物、界面、天气和粒子；亮度固定，植物颜色和水面简化。\
紫黑格表示模型或资源未支持；未绘制及范围外内容不代表不存在。\
视点采用站姿眼高 1.62 格，姿态与方块采集并非同一服务端 tick 的原子快照。"
                        .to_owned(),
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
                position: [block.position.x, block.position.y, block.position.z],
                name: block.name,
                properties: block.properties,
                opaque: !block.transparent_hint,
            })
            .collect(),
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
            unloaded: 1,
        };
        assert!(scene_from_region(region.clone()).is_err());
        region.unloaded = 0;
        let scene = scene_from_region(region).unwrap();
        assert_eq!(scene.camera.eye, [12.5, 65.62, -3.0]);
        assert_eq!(scene.camera.yaw, 91.0);
        assert_eq!(scene.camera.pitch, -17.0);
    }
}
