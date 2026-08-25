//! 全量视口基准（机器级，无模型）：默认参数下一次 `scan` 到底多少毫秒。
//!
//! 背靠背测两件事，差值即记忆那层的代价：
//!   `scan`         —— 纯投影（视锥 + 遮挡 + 聚合）
//!   `scan_changes` —— 投影 + 与方块记忆逐格 diff + apply
//!
//! 缘起：实盘眼睛那一趟曾量到 180ms，需要知道慢在哪一层。
//!
//! 本探针给出的答案（默认参数、空闲站立）：纯 scan 中位 8.8ms，scan_changes
//! 13.3ms——记忆那层 +3.7ms（35%）。三项旧优化（section 剔除、ExposedFace
//! 判据、零分配探针表）都还在。180ms 是 debug 构建的产物（release 26ms），
//! 剩下的 26 与 13 之差不是排队（实测排队 0ms），是场景：实盘边走边挖，
//! 投影成本在 7~122ms 之间随视野内容浮动，静止基准只采到分布的一端。
//!
//! 所以这个探针的用法是「定一端」：它给的是静止基线，不是实盘代价。
//!
//! 用法（需要 `--features azalea`）：
//! `cargo run --release -p world --features azalea --example scan_bench_probe -- <host> <port> <名字> [轮数]`

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use world::{BlockMemory, ConnectionConfig, Module, SnapshotSource, ViewportOptions};

fn report(label: &str, mut samples: Vec<Duration>) {
    if samples.is_empty() {
        println!("{label}：无样本");
        return;
    }
    samples.sort();
    let n = samples.len();
    let sum: Duration = samples.iter().sum();
    println!(
        "{label}：{n} 次｜最快 {:.1}ms｜中位 {:.1}ms｜均值 {:.1}ms｜最慢 {:.1}ms",
        samples[0].as_secs_f64() * 1000.0,
        samples[n / 2].as_secs_f64() * 1000.0,
        sum.as_secs_f64() * 1000.0 / n as f64,
        samples[n - 1].as_secs_f64() * 1000.0,
    );
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "127.0.0.1".to_owned());
    let port: u16 = args
        .next()
        .unwrap_or_else(|| "25565".to_owned())
        .parse()
        .map_err(|error| format!("端口无效：{error}"))?;
    let username = args.next().unwrap_or_else(|| "bench".to_owned());
    let rounds: usize = args
        .next()
        .unwrap_or_else(|| "30".to_owned())
        .parse()
        .unwrap_or(30);

    let module = Arc::new(
        Module::start(ConnectionConfig {
            host,
            port,
            username,
        })
        .map_err(|error| format!("启动失败：{error}"))?,
    );
    module.wait_ready(Duration::from_secs(60)).await?;
    // 等区块装够：未加载的格子会走保守失败分支，测出来的不是稳态。
    println!("[基准] 已进入世界，等 15 秒让区块装载");
    tokio::time::sleep(Duration::from_secs(15)).await;

    let snapshot = module.latest();
    println!(
        "[基准] 位置 ({:.0}, {:.0}, {:.0})，默认参数：{}° 横 × {}° 纵，{} 格",
        snapshot.self_state.position.x,
        snapshot.self_state.position.y,
        snapshot.self_state.position.z,
        ViewportOptions::default()
            .horizontal_half_angle
            .to_degrees()
            * 2.0,
        ViewportOptions::default().vertical_half_angle.to_degrees() * 2.0,
        ViewportOptions::default().max_distance,
    );

    // 预热：首次会触发探针查表初始化（一次性 64 KiB），不计入。
    let _ = module.scan(&ViewportOptions::default());

    // 几组参数放一起量。问题很具体：**去掉方块数上限贵多少，放远又贵多少**。
    //   block_limit 是**呈现**预算（模型读不了一万行）；眼睛只往记忆里塞，不受这个限制。
    //   角度与遮挡判据全组不动——那是合法性本身。
    let configs: Vec<(String, ViewportOptions)> = vec![
        (
            "默认（32 格，上限 256）".to_owned(),
            ViewportOptions::default(),
        ),
        (
            "记忆（32 格，不限）".to_owned(),
            ViewportOptions::for_memory(),
        ),
        (
            "48 格，不限".to_owned(),
            ViewportOptions {
                max_distance: 48.0,
                horizontal_radius: 48,
                vertical_radius: 32,
                ..ViewportOptions::for_memory()
            },
        ),
        (
            "64 格，不限".to_owned(),
            ViewportOptions {
                max_distance: 64.0,
                horizontal_radius: 64,
                vertical_radius: 40,
                ..ViewportOptions::for_memory()
            },
        ),
        (
            "160 格＝10 区块，不限".to_owned(),
            ViewportOptions {
                max_distance: 160.0,
                horizontal_radius: 160,
                vertical_radius: 64,
                ..ViewportOptions::for_memory()
            },
        ),
    ];

    for (label, options) in &configs {
        let mut samples = Vec::with_capacity(rounds);
        let mut blocks = 0usize;
        let mut truncated = false;
        for _ in 0..rounds {
            let started = Instant::now();
            let projection = module.scan(options);
            samples.push(started.elapsed());
            match &projection {
                Ok(view) => {
                    blocks = view.visible_blocks.blocks.len();
                    truncated = view.visible_blocks.truncated;
                }
                // 吞掉错误会把「跑不成」读成「0 格、0ms」——上一版就是这么骗了自己。
                Err(reason) => {
                    println!("{label:<24}｜跑不成：{reason}");
                    break;
                }
            }
        }
        samples.sort();
        let n = samples.len();
        let sum: Duration = samples.iter().sum();
        println!(
            "{label:<24}｜可见 {blocks:>6} 格{}｜最快 {:>6.1}ms｜中位 {:>6.1}ms｜均值 {:>6.1}ms｜最慢 {:>6.1}ms",
            if truncated { "（截断）" } else { "　　　　" },
            samples[0].as_secs_f64() * 1000.0,
            samples[n / 2].as_secs_f64() * 1000.0,
            sum.as_secs_f64() * 1000.0 / n as f64,
            samples[n - 1].as_secs_f64() * 1000.0,
        );
    }

    // 记忆那一层单独量一次：眼睛现在走全量吸收，不算 diff。
    let memory = Mutex::new(BlockMemory::new());
    let mut absorb = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let started = Instant::now();
        let _ = module.absorb(&memory, &ViewportOptions::for_memory());
        absorb.push(started.elapsed());
    }
    report("吸收进记忆 + 标记已观察空间（32 格，不限）", absorb);
    println!(
        "记忆里现在 {} 格",
        memory.lock().map(|m| m.len()).unwrap_or(0)
    );
    // 三态里「确认为空」那一位的产量与占用：位图是定长的，区段数乘 512 字节
    // 就是全部开销，与看了多少次无关。
    if let Ok(memory) = memory.lock() {
        println!(
            "已观察为空 {} 格，占 {} 个区段 = {} KB",
            memory.known_empty_len(),
            memory.section_count(),
            memory.section_count() * 512 / 1024
        );
    }

    if let Err(reason) = module.stop("基准结束").await {
        eprintln!("[基准] 停机未合流（{reason}）");
    }
    Ok(())
}
