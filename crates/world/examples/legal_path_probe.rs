//! 合法寻路探针：同一个目标，**全量世界** vs **只按观察过的地图**，各算一次路。
//!
//! 回答三个问题，按重要性排：
//!   一、合法之后**还找不找得到路**（这才是能不能用的分界）
//!   二、路**长多少**（合法的必然更绕，绕多少）
//!   三、**算多久**（记忆是 HashMap，取块比 section 直取慢；慢多少）
//!
//! 观察量按真实节律攒：反复 `scan_changes` 推进同一份记忆，与眼睛同一条路径。
//! 中途转头，因为视锥只覆盖面朝方向——不转头，「合法」会窄得没有代表性。
//!
//! 用法（需要 `--features azalea`）：
//! `cargo run -p world --release --features azalea --example legal_path_probe -- <host> <port> <名字>`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use world::{BlockMemory, ConnectionConfig, DoorCommand, Module, SnapshotSource, ViewportOptions};

async fn look_around(module: &Arc<Module>, memory: &Arc<Mutex<BlockMemory>>) {
    // 八向各看一眼，另加俯仰两档：视锥只覆盖面朝方向，不转头攒不出可用的地图。
    for pitch in [0.0_f64, 35.0, -20.0] {
        for step in 0..8 {
            let yaw = step as f64 * 45.0;
            let _ = module.execute(DoorCommand::Face { yaw, pitch }).await;
            tokio::time::sleep(Duration::from_millis(180)).await;
            let _ = module.scan_changes(memory, &ViewportOptions::default());
        }
    }
}

fn report(label: &str, attempt: &world::PathAttempt) {
    println!(
        "  {label:<10}｜{}｜节点 {:>4}｜{}｜{} ms",
        if attempt.found {
            "找到 ✓"
        } else {
            "没找到 ✗"
        },
        attempt.nodes,
        if attempt.partial { "部分" } else { "完整" },
        attempt.elapsed.as_millis()
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
    let username = args.next().unwrap_or_else(|| "legalpath".to_owned());
    // 距离表可从命令行给（逗号分隔），好在不重编的情况下试几千格。
    let distances: Vec<i32> = args
        .next()
        .map(|raw| {
            raw.split(',')
                .filter_map(|piece| piece.trim().parse().ok())
                .collect()
        })
        .filter(|list: &Vec<i32>| !list.is_empty())
        .unwrap_or_else(|| vec![4, 8, 16, 32, 64]);

    let module = Arc::new(
        Module::start(ConnectionConfig {
            host,
            port,
            username,
        })
        .map_err(|error| format!("启动失败：{error}"))?,
    );
    module.wait_ready(Duration::from_secs(60)).await?;
    println!("[探针] 进入世界，等 15 秒装区块");
    tokio::time::sleep(Duration::from_secs(15)).await;

    let memory = Arc::new(Mutex::new(BlockMemory::new()));
    let snapshot = module.latest();
    let here = &snapshot.self_state.position;
    let (x, y, z) = (
        here.x.floor() as i32,
        here.y.floor() as i32,
        here.z.floor() as i32,
    );
    println!("[探针] 起点 ({x}, {y}, {z})");

    // 先量一次「什么都没看过」的极端：记忆空的时候合法寻路应该寸步难行。
    println!("\n=== 记忆为空（刚进服，还没看过任何东西）===");
    match module.compare_paths(memory.clone(), [x + 8, y, z]) {
        Ok((full, legal)) => {
            report("全量世界", &full);
            report("合法地图", &legal);
        }
        Err(reason) => println!("  比不了：{reason}"),
    }

    look_around(&module, &memory).await;
    let known = memory.lock().map(|m| m.len()).unwrap_or(0);
    println!("\n=== 环视一圈之后：记忆里有 {known} 格 ===");

    for distance in distances {
        println!("\n目标 ({}, {y}, {z})　距离 {distance} 格", x + distance);
        match module.compare_paths(memory.clone(), [x + distance, y, z]) {
            Ok((full, legal)) => {
                report("全量世界", &full);
                report("合法地图", &legal);
            }
            Err(reason) => println!("  比不了：{reason}"),
        }
    }

    if let Err(reason) = module.stop("探针结束").await {
        eprintln!("[探针] 停机未合流（{reason}）");
    }
    Ok(())
}
