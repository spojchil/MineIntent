//! 全量视口基准（机器级，无模型）：默认参数下一次 `scan` 到底多少毫秒。
//!
//! 背靠背测两件事，差值即记忆那层的代价：
//!   `scan`         —— 纯投影（视锥 + 遮挡 + 聚合）
//!   `scan_changes` —— 投影 + 与方块记忆逐格 diff + apply
//!
//! 缘起：实盘轮末帧量到 180ms，而维护者记得旧线把全量压到过 10ms 量级。
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

    let mut pure = Vec::with_capacity(rounds);
    let mut with_memory = Vec::with_capacity(rounds);
    let memory = Mutex::new(BlockMemory::new());

    for round in 0..rounds {
        let started = Instant::now();
        let projection = module.scan(&ViewportOptions::default());
        pure.push(started.elapsed());
        if round == 0 {
            match &projection {
                Ok(view) => println!(
                    "[基准] 首帧可见方块 {} 个（截断：{}），实体 {} 个",
                    view.visible_blocks.blocks.len(),
                    view.visible_blocks.truncated,
                    view.visible_entities.items.len()
                ),
                Err(reason) => println!("[基准] 首帧失败：{reason}"),
            }
        }

        let started = Instant::now();
        let _ = module.scan_changes(&memory, &ViewportOptions::default());
        with_memory.push(started.elapsed());
    }

    println!();
    report("纯全量 scan            ", pure.clone());
    report("全量 + 记忆 diff/apply ", with_memory.clone());
    let avg = |v: &[Duration]| v.iter().sum::<Duration>().as_secs_f64() * 1000.0 / v.len() as f64;
    println!(
        "\n记忆那层的代价：{:.1}ms（{:.0}%）",
        avg(&with_memory) - avg(&pure),
        (avg(&with_memory) / avg(&pure) - 1.0) * 100.0
    );

    if let Err(reason) = module.stop("基准结束").await {
        eprintln!("[基准] 停机未合流（{reason}）");
    }
    Ok(())
}
