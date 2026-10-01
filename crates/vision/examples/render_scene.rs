//! Explicit test scene or caller-provided snapshot; never opens a game window.
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// 计数分配器：最后一次出图的分配次数、分配字节与峰值在用内存。与耗时无关的性能指标，
/// 只装在这个探针里，库本身不受影响。
struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicI64 = AtomicI64::new(0);
static PEAK: AtomicI64 = AtomicI64::new(0);

impl Counting {
    fn grow(size: usize) {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED.fetch_add(size as u64, Ordering::Relaxed);
        let live = LIVE.fetch_add(size as i64, Ordering::Relaxed) + size as i64;
        PEAK.fetch_max(live, Ordering::Relaxed);
    }

    fn shrink(size: usize) {
        LIVE.fetch_sub(size as i64, Ordering::Relaxed);
    }
}

// 豁免 unsafe：GlobalAlloc 是 unsafe trait，实现它是计数分配器的唯一入口；这里只记数，
// 指针与布局原样转发给 System，不读写任何内存。
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::grow(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::grow(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        Self::shrink(layout.size());
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::shrink(layout.size());
        Self::grow(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

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
    // RENDER_REPEAT=n 时同一场景连渲 n 次（资源复用，与常驻工具一致），报最后一次的分段耗时、
    // 工作量计数与内存分配。
    let repeat: usize = std::env::var("RENDER_REPEAT")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(1)
        .max(1);
    for _ in 1..repeat {
        vision::render(&scene, &mut resources, vision::Options::default())?;
    }
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED.store(0, Ordering::Relaxed);
    let baseline = LIVE.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);
    let started = std::time::Instant::now();
    let frame = vision::render(&scene, &mut resources, vision::Options::default())?;
    let elapsed = started.elapsed();
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let allocated = ALLOCATED.load(Ordering::Relaxed);
    let peak = PEAK.load(Ordering::Relaxed) - baseline;
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
        elapsed.as_secs_f64(),
        frame.report.warnings.len()
    );
    if repeat > 1 {
        for (stage, ms) in &frame.report.stage_ms {
            println!("  {stage}: {ms:.0} ms");
        }
        println!(
            "  alloc: {allocations} allocations, {:.1} MiB allocated, peak +{:.1} MiB live",
            allocated as f64 / 1048576.0,
            peak as f64 / 1048576.0
        );
        for (counter, count) in &frame.report.counters {
            println!("  # {counter}: {count}");
        }
    }
    Ok(())
}
