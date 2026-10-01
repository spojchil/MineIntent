//! Explicit test scene or caller-provided snapshot; never opens a game window.
use std::path::PathBuf;

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() < 2 || args.len() > 3 {
        return Err("usage: render_scene <client.jar> <output.png> [scene.json]; without scene.json renders a synthetic fixture".to_owned());
    }
    let mut resources = vision::Resources::open(&args[0])?;
    let scene = if let Some(path) = args.get(2) {
        serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?
    } else {
        vision::fixture()
    };
    // RENDER_REPEAT=n 时同一场景连渲 n 次（资源复用，与常驻工具一致），报最后一次的分段耗时。
    let repeat: usize = std::env::var("RENDER_REPEAT")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(1)
        .max(1);
    for _ in 1..repeat {
        vision::render(&scene, &mut resources, vision::Options::default())?;
    }
    let started = std::time::Instant::now();
    let frame = vision::render(&scene, &mut resources, vision::Options::default())?;
    std::fs::write(&args[1], frame.png()?).map_err(|e| e.to_string())?;
    let report_path = PathBuf::from(&args[1]).with_extension("report.json");
    std::fs::write(
        report_path,
        serde_json::to_vec_pretty(&frame.report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!(
        "rendered {} blocks / {} triangles in {:.2}s; {} limitation(s), see report",
        frame.report.blocks,
        frame.report.triangles,
        started.elapsed().as_secs_f64(),
        frame.report.warnings.len()
    );
    if repeat > 1 {
        for (stage, ms) in &frame.report.stage_ms {
            println!("  {stage}: {ms:.0} ms");
        }
    }
    Ok(())
}
