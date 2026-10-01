//! Repeatable synthetic workloads; separates JAR opening, rendering and PNG.
use std::time::Instant;

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let jar = args.first().ok_or("usage: benchmark <client.jar>")?;
    let options = vision::Options::default();
    let fixture = vision::fixture();
    let mut extended = fixture.clone();
    extended.blocks.clear();
    for x in -1..=1 {
        for z in -1..=1 {
            extended
                .blocks
                .extend(fixture.blocks.iter().cloned().map(|mut b| {
                    b.position[0] += x * 17;
                    b.position[2] += z * 18;
                    b
                }));
        }
    }
    for (name, scene) in [("fixture", fixture), ("extended", extended)] {
        let start = Instant::now();
        let mut resources = vision::Resources::open(jar)?;
        let open_ms = start.elapsed().as_secs_f64() * 1000.0;
        let mut render_ms = Vec::new();
        let mut png_ms = Vec::new();
        let mut triangles = 0;
        for _ in 0..6 {
            let start = Instant::now();
            let frame = vision::render(&scene, &mut resources, options)?;
            render_ms.push(start.elapsed().as_secs_f64() * 1000.0);
            triangles = frame.report.triangles;
            let start = Instant::now();
            std::hint::black_box(frame.png()?);
            png_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        println!(
            "{}",
            serde_json::json!({
                "scene":name, "width":options.width, "height":options.height, "blocks":scene.blocks.len(),
                "triangles":triangles, "jar_open_ms":open_ms,
                "render_ms_cold_then_warm":render_ms, "png_ms":png_ms,
                "debug_assertions":cfg!(debug_assertions)
            })
        );
    }
    Ok(())
}
