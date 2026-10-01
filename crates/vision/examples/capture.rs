//! Live protocol-client image probe. No model/provider, commands or world writes.
//!
//! 输出路径给 `-` 时常驻在线：从标准输入逐行读 `输出路径 [宽x高]`，每行按当前位姿出一张图
//! （不给尺寸用默认 640x360），便于让人旁观同一个视点做对照。
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 5 {
        return Err(
            "usage: capture <client-26.1.2.jar> <host> <port> <offline-name> <output.png | ->"
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
    let result = if args[4] == "-" {
        resident(module.clone(), &mut resources).await
    } else {
        capture(module.clone(), &mut resources, &args[4], None).await
    };
    let _ = module.stop("图片采集完成").await;
    result
}

async fn resident(
    module: Arc<world::Module>,
    resources: &mut vision::Resources,
) -> Result<(), String> {
    module.wait_ready(Duration::from_secs(45)).await?;
    println!("ready");
    for line in std::io::stdin().lines() {
        let line = line.map_err(|e| e.to_string())?;
        let mut words = line.split_whitespace();
        let Some(output) = words.next() else {
            continue;
        };
        let size = words.next().and_then(|size| {
            let (width, height) = size.split_once('x')?;
            Some([width.parse().ok()?, height.parse().ok()?])
        });
        match capture(module.clone(), resources, output, size).await {
            Ok(()) => println!("done {output}"),
            Err(error) => println!("failed {output}: {error}"),
        }
    }
    Ok(())
}

async fn capture(
    module: Arc<world::Module>,
    resources: &mut vision::Resources,
    output: &str,
    size: Option<[u32; 2]>,
) -> Result<(), String> {
    module.wait_ready(Duration::from_secs(45)).await?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let region = loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let source = module.clone();
        let region = tokio::task::spawn_blocking(move || source.capture_view())
            .await
            .map_err(|e| e.to_string())?;
        // Ready 可能先于出生事件发布，那一刻世界句柄还没装上；截止前一律重试。
        let pending = match region {
            Ok(region) if region.unloaded == 0 => break region,
            Ok(region) => format!(
                "{} cells are not loaded; refusing to render them as air",
                region.unloaded
            ),
            Err(error) => error,
        };
        if Instant::now() >= deadline {
            return Err(pending);
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
                position: [b.block.position.x, b.block.position.y, b.block.position.z],
                name: b.block.name,
                properties: b.block.properties,
                opaque: !b.block.transparent_hint,
                covered: Some(b.covered),
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
    };
    let started = Instant::now();
    let mut options = vision::Options::default();
    if let Some([width, height]) = size {
        options.width = width;
        options.height = height;
    }
    let mut frame = vision::render(&scene, resources, options)?;
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
