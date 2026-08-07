//! `view` 全量投影的耗时测量。
//!
//! 存在的理由：判断「模型给的工具数组要在 1 tick（50ms）内处理完」这个目标成不成立。
//! 持续动作改成后台 job 之后只是入队、必然很快，全量投影是唯一可能超预算的工具。
//!
//! **与生产路径的差别只有一处**：生产的 `read_block` 还要走
//! `world.get_block_state()`（区块调色板查表），这里直接给状态。
//! DTO 构造走的是**生产的同一个 `block_snapshot`**，投影走的是
//! 生产的同一个 `project_with_checkpoint` + `ViewportOptions::default()`
//! （`runtime/observation.rs:420` 用的就是这两个）。
//!
//! 所以读数是**生产耗时的下界**，缺的那部分是区块查表。
//!
//! 跑法（必须 release，生产用的是 lto=fat / opt-level=3）：
//! ```text
//! cargo run --release -p mineintent-backend --example viewport_cost
//! ```

use std::time::{Duration, Instant};

use azalea::block::BlockState;
use mineintent_backend::{
    snapshot::{block_snapshot, BlockPosition, BlockReadResult, PoseSnapshot, Vec3Value},
    viewport::{project_with_checkpoint, ViewportOptions},
};

const WARMUP: usize = 3;
const SAMPLES: usize = 20;
/// 一个游戏刻。目标就是拿测得的耗时和它比。
const TICK: Duration = Duration::from_millis(50);

fn stone() -> BlockState {
    BlockState::from(azalea::registry::builtin::BlockKind::Stone)
}

/// 微基准的取样方块。石头是**最便宜的非空气方块**——原版石头没有 blockstate
/// 属性，`properties` 是空 BTreeMap，不分配。真实地形里到处是带属性的方块
/// （楼梯有 facing/half/shape/waterlogged，树叶有 distance/persistent/waterlogged），
/// 每个键和值都是一次独立的 String 分配。拿石头当代表会低估。
const SAMPLE_BLOCKS: [(&str, fn() -> BlockState); 5] = [
    ("air", || BlockState::AIR),
    ("stone", stone),
    ("grass_block", || {
        BlockState::from(azalea::registry::builtin::BlockKind::GrassBlock)
    }),
    ("oak_leaves", || {
        BlockState::from(azalea::registry::builtin::BlockKind::OakLeaves)
    }),
    ("oak_stairs", || {
        BlockState::from(azalea::registry::builtin::BlockKind::OakStairs)
    }),
];

/// 便宜的整数散列，用来造「碎」地形；不引随机数依赖，保证可复现。
fn hashed(x: i32, y: i32, z: i32) -> u32 {
    let mut h = 2_166_136_261u32;
    for value in [x, y, z] {
        h ^= value as u32;
        h = h.wrapping_mul(16_777_619);
    }
    h ^= h >> 15;
    h
}

struct Scenario {
    name: &'static str,
    note: &'static str,
    solid: fn(i32, i32, i32) -> bool,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "open_air",
        note: "全空气：只剩视锥/距离剔除，下界",
        solid: |_, _, _| false,
    },
    Scenario {
        name: "flat_ground",
        note: "y<=0 石头：典型露天站立",
        solid: |_, y, _| y <= 0,
    },
    Scenario {
        name: "cave_pocket",
        note: "半径 6 空气球，其余石头：洞穴/矿道",
        solid: |x, y, z| {
            let dy = y - 1;
            x * x + dy * dy + z * z > 36
        },
    },
    Scenario {
        name: "noise_30",
        note: "30% 伪随机石头：最碎，暴露面最多",
        solid: |x, y, z| hashed(x, y, z) % 100 < 30,
    },
    Scenario {
        name: "solid",
        note: "全石头：眼在石头里",
        solid: |_, _, _| true,
    },
];

struct Measurement {
    elapsed: Vec<Duration>,
    reads: usize,
    blocks: usize,
    truncated: bool,
}

fn measure(scenario: &Scenario) -> Measurement {
    let pose = PoseSnapshot {
        position: Vec3Value {
            x: 0.5,
            y: 1.0,
            z: 0.5,
        },
        velocity: Vec3Value {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
        yaw: 0.0,
        pitch: 0.0,
        on_ground: true,
    };
    let options = ViewportOptions::default();
    let solid = scenario.solid;

    let mut reads = 0usize;
    let mut blocks = 0usize;
    let mut truncated = false;
    let mut elapsed = Vec::with_capacity(SAMPLES);

    for iteration in 0..(WARMUP + SAMPLES) {
        let mut counter = 0usize;
        let read_block = |position: BlockPosition| {
            counter += 1;
            let state = if solid(position.x, position.y, position.z) {
                stone()
            } else {
                BlockState::AIR
            };
            BlockReadResult::Loaded {
                block: block_snapshot(position, state),
            }
        };

        let started = Instant::now();
        let projection = project_with_checkpoint(&pose, &[], read_block, &options, || Ok(()))
            .expect("projection must succeed");
        let took = started.elapsed();

        if iteration >= WARMUP {
            elapsed.push(took);
        }
        reads = counter;
        blocks = projection.visible_blocks.blocks.len();
        truncated = projection.visible_blocks.truncated;
    }

    elapsed.sort();
    Measurement {
        elapsed,
        reads,
        blocks,
        truncated,
    }
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

/// 把「建 DTO」从「做几何」里拆出来。
///
/// 投影每读一个体素就调一次 `block_snapshot`，而它每次都要三次堆分配
/// （`Box::from(state)`、`to_aabbs()` 收 Vec、`property_map()` 收 BTreeMap，
/// 见 `snapshot.rs:218-233`）。但绝大多数体素只是被问「挡不挡光」这个布尔问题，
/// 真正进输出的最多 256 个。这一段量的就是那笔冤枉钱有多大。
fn dto_share() {
    /// 与上表的唯一体素数同量级，便于直接对照。
    const CALLS: usize = 21_000;

    println!();
    println!("DTO 构造成本（block_snapshot × {CALLS}，与上表唯一体素数同量级）");
    println!("{:<10} {:>10} {:>12} {:>12}", "方块", "总计ms", "每次ns", "占一刻");

    for (label, state) in SAMPLE_BLOCKS.map(|(label, kind)| (label, kind())) {
        let mut best = Duration::MAX;
        for _ in 0..5 {
            let started = Instant::now();
            for index in 0..CALLS {
                let position = BlockPosition {
                    x: index as i32,
                    y: 0,
                    z: 0,
                };
                std::hint::black_box(block_snapshot(position, state));
            }
            best = best.min(started.elapsed());
        }
        println!(
            "{:<10} {:>10.2} {:>12.0} {:>11.0}%",
            label,
            millis(best),
            best.as_nanos() as f64 / CALLS as f64,
            millis(best) / millis(TICK) * 100.0
        );
    }

    clone_share();
}

/// 量投影层那个块缓存**每次命中**的代价。
///
/// `viewport.rs:352` 写的是 `block_cache.get(&key).cloned()`——命中也要把整个
/// `BlockReadResult` 深拷一份，而它里面带 `Vec<[f64;6]>` 和 `BTreeMap`。
/// 暴露面判据每个候选要探 6 个邻居，邻居基本都是已读过的候选，
/// 所以命中次数大约是候选数的 6 倍。这一段量的就是那 6 倍要多少钱。
fn clone_share() {
    const CANDIDATES: usize = 21_000;
    /// 暴露面判据每个候选最多探 6 个邻居。
    const NEIGHBOURS: usize = 6;
    const HITS: usize = CANDIDATES * NEIGHBOURS;

    println!();
    println!("缓存命中成本（BlockReadResult::clone × {HITS} = 候选 {CANDIDATES} × 邻居 {NEIGHBOURS}）");
    println!("{:<10} {:>10} {:>12} {:>12}", "方块", "总计ms", "每次ns", "占一刻");

    for (label, state) in SAMPLE_BLOCKS.map(|(label, kind)| (label, kind())) {
        let cached = BlockReadResult::Loaded {
            block: block_snapshot(BlockPosition { x: 0, y: 0, z: 0 }, state),
        };
        let mut best = Duration::MAX;
        for _ in 0..5 {
            let started = Instant::now();
            for _ in 0..HITS {
                std::hint::black_box(cached.clone());
            }
            best = best.min(started.elapsed());
        }
        println!(
            "{:<10} {:>10.2} {:>12.0} {:>11.0}%",
            label,
            millis(best),
            best.as_nanos() as f64 / HITS as f64,
            millis(best) / millis(TICK) * 100.0
        );
    }
}

fn main() {
    let options = ViewportOptions::default();
    let box_voxels =
        (2 * options.horizontal_radius as i64 + 1).pow(2) * (2 * options.vertical_radius as i64 + 1);

    println!("view 全量投影耗时测量");
    println!(
        "参数（生产默认值 ViewportOptions::default）：水平半径 {}、垂直半径 {}、最远 {} 格、\
         上限 {} 块、判据 {:?}",
        options.horizontal_radius,
        options.vertical_radius,
        options.max_distance,
        options.block_limit,
        options.predicate
    );
    println!(
        "扫描盒 {}x{}x{} = {} 体素；预热 {} 次，取样 {} 次；一刻 = {} ms",
        2 * options.horizontal_radius + 1,
        2 * options.horizontal_radius + 1,
        2 * options.vertical_radius + 1,
        box_voxels,
        WARMUP,
        SAMPLES,
        millis(TICK)
    );
    println!();
    println!(
        "{:<14} {:>9} {:>9} {:>9} {:>10} {:>8} {:>7}  {}",
        "场景", "最快ms", "中位ms", "最慢ms", "唯一体素", "可见块", "占一刻", "说明"
    );

    for scenario in SCENARIOS {
        let measurement = measure(scenario);
        let median = measurement.elapsed[measurement.elapsed.len() / 2];
        println!(
            "{:<14} {:>9.2} {:>9.2} {:>9.2} {:>10} {:>8} {:>6.0}%  {}{}",
            scenario.name,
            millis(measurement.elapsed[0]),
            millis(median),
            millis(measurement.elapsed[measurement.elapsed.len() - 1]),
            measurement.reads,
            measurement.blocks,
            millis(median) / millis(TICK) * 100.0,
            scenario.note,
            if measurement.truncated {
                "（已截断）"
            } else {
                ""
            }
        );
    }

    dto_share();

    println!();
    println!("读法：");
    println!("- 「唯一体素」是本 harness 的 read_block 被调用的次数。投影层在它外面套了一层");
    println!("  HashMap 缓存（viewport.rs:349-358），只有未命中才落到这里，所以这个数是");
    println!("  **实际读过的不重复体素**，不是总请求数。生产里每次未命中还要多一次区块");
    println!("  调色板查表，本测量没算进去。");
    println!("- 「占一刻」超过 100% 就意味着单次 view 已经吃掉整个 tick 预算。");
}
