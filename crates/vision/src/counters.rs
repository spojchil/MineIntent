//! 工作量计数：与耗时无关、换机器也不变的性能指标——做了多少次覆盖测试、查了多少次
//! 光照格、染色缓存没命中多少次。优化前后比它们，能看出是真的少做了事，还是只换了
//! 分配方式或碰上了机器负载。
//!
//! 热路径只碰线程局部计数；工作线程收尾时 [`flush`] 汇到全局，[`take`] 在出图结束时
//! 收进报告。全局计数是进程级的：同时出两张图会混在一起（工具出图有资源锁，本来就串行）。

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

macro_rules! counters {
    ($($name:ident => $label:literal,)*) => {
        #[derive(Clone, Copy, Debug)]
        pub(crate) enum Counter {
            $($name,)*
        }

        const LABELS: &[&str] = &[$($label,)*];
    };
}

counters! {
    FacesConsidered => "geometry: model faces considered",
    FacesCovered => "geometry: faces hidden by neighbours",
    FacesBackfacing => "geometry: faces facing away",
    FacesOutside => "geometry: faces outside the view frustum",
    FacesEmitted => "geometry: faces emitted",
    TintLookups => "tint: lookups",
    TintBlends => "tint: 5x5 blends computed (cache misses)",
    BiomeSamples => "tint: biome zoom samples",
    AoFaces => "light: smooth-lit faces",
    CellLookups => "light: cell lookups",
    TrianglesProjected => "project: triangles in",
    TrianglesDropped => "project: triangles fully clipped away",
    TrianglesOut => "project: screen triangles out",
    CoverageTests => "raster: coverage tests (bounding-box pixels)",
    SpanProbes => "raster: span end probes",
    Covered => "raster: pixels inside triangles",
    DepthPassed => "raster: fragments passing depth and far",
    Transparent => "raster: transparent texels discarded",
    FogApplied => "raster: fog evaluations",
    SkyPixels => "sky: pixels painted",
}

const COUNT: usize = LABELS.len();

thread_local! {
    static LOCAL: [Cell<u64>; COUNT] = const { [const { Cell::new(0) }; COUNT] };
}

static GLOBAL: [AtomicU64; COUNT] = [const { AtomicU64::new(0) }; COUNT];

/// 本线程的计数加 `n`。
#[inline]
pub(crate) fn add(counter: Counter, n: u64) {
    LOCAL.with(|local| {
        let cell = &local[counter as usize];
        cell.set(cell.get() + n);
    });
}

/// 把本线程攒下的计数汇到全局。工作线程结束前调一次。
pub(crate) fn flush() {
    LOCAL.with(|local| {
        for (cell, global) in local.iter().zip(&GLOBAL) {
            global.fetch_add(cell.take(), Ordering::Relaxed);
        }
    });
}

/// 取走全局计数（含本线程的）并清零。
pub(crate) fn take() -> Vec<(&'static str, u64)> {
    flush();
    LABELS
        .iter()
        .zip(&GLOBAL)
        .map(|(label, global)| (*label, global.swap(0, Ordering::Relaxed)))
        .collect()
}
