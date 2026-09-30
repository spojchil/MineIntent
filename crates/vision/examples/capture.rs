//! Live protocol-client image probe. No model/provider, commands or world writes.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 5 {
        return Err(
            "usage: capture <client-26.1.2.jar> <host> <port> <offline-name> <output.png>"
                .to_owned(),
        );
    }
    let mut resources = vision::Resources::open(&args[0])?;
    if resources.version() != "26.1.2" {
        return Err("this protocol probe requires 26.1.2 resources".to_owned());
    }
    let module = Arc::new(world::Module::start(world::ConnectionConfig {
        host: args[1].clone(),
        port: args[2].parse().map_err(|_| "invalid port")?,
        username: args[3].clone(),
    })?);
    let result = capture(module.clone(), &mut resources, &args[4]).await;
    let _ = module.stop("图片采集完成").await;
    result
}

async fn capture(
    module: Arc<world::Module>,
    resources: &mut vision::Resources,
    output: &str,
) -> Result<(), String> {
    module.wait_ready(Duration::from_secs(45)).await?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let region = loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let source = module.clone();
        let region = tokio::task::spawn_blocking(move || source.capture_blocks(16))
            .await
            .map_err(|e| e.to_string())??;
        if region.unloaded == 0 {
            break region;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{} cells are not loaded; refusing to render them as air",
                region.unloaded
            ));
        }
    };
    let pose = &region.snapshot.self_state;
    let scene = vision::Scene {
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
            .map(|b| vision::Block {
                position: [b.position.x, b.position.y, b.position.z],
                name: b.name,
                properties: b.properties,
                opaque: !b.transparent_hint,
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
    };
    let started = Instant::now();
    let mut frame = vision::render(
        &scene,
        resources,
        vision::Options {
            far: 16.0,
            ..Default::default()
        },
    )?;
    frame.report.warnings.insert("capture uses latest tick pose and current block-region copy, not an atomic server tick; standing eye height 1.62".to_owned());
    std::fs::write(output, frame.png()?).map_err(|e| e.to_string())?;
    std::fs::write(
        PathBuf::from(output).with_extension("report.json"),
        serde_json::to_vec_pretty(&frame.report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!(
        "captured {} loaded blocks, rendered {} triangles in {:.2}s; limitations in report",
        frame.report.blocks,
        frame.report.triangles,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
