//! 增量差异的构成探针：一路走，统计 `BlockChange` 三种变体各占多少。
//!
//! 为什么要单独量：轮末帧日志只记 `changes.len()`，而渲染出来的**行数**不等于
//! 条数——`Changed` 是两行（先 `-` 后 `+`），`Appeared` / `Vanished` 各一行。
//! 上下文账要按行算，所以得知道构成。
//!
//! 用法（需要 `--features azalea`）：
//! `cargo run -p world --release --features azalea --example diff_mix_probe -- <host> <port> <名字> [秒数]`

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use world::{
    BlockChange, BlockMemory, ConnectionConfig, DoorCommand, Module, SnapshotSource,
    ViewportOptions,
};

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "127.0.0.1".to_owned());
    let port: u16 = args
        .next()
        .unwrap_or_else(|| "25565".to_owned())
        .parse()
        .map_err(|error| format!("端口无效：{error}"))?;
    let username = args.next().unwrap_or_else(|| "diffmix".to_owned());
    let seconds: u64 = args
        .next()
        .unwrap_or_else(|| "120".to_owned())
        .parse()
        .unwrap_or(120);

    let module = Arc::new(
        Module::start(ConnectionConfig {
            host,
            port,
            username,
        })
        .map_err(|error| format!("启动失败：{error}"))?,
    );
    module.wait_ready(Duration::from_secs(60)).await?;
    tokio::time::sleep(Duration::from_secs(15)).await;

    let memory = Arc::new(Mutex::new(BlockMemory::new()));
    let start = module.latest();
    let (x, y, z) = (
        start.self_state.position.x.floor() as i32,
        start.self_state.position.y.floor() as i32,
        start.self_state.position.z.floor() as i32,
    );
    println!("[探针] 起点 ({x}, {y}, {z})，走 {seconds} 秒，按轮末帧的节律采样");

    let (mut appeared, mut changed, mut vanished) = (0_usize, 0_usize, 0_usize);
    let mut frames = 0_usize;
    let mut empty = 0_usize;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut leg = 0_i32;

    while Instant::now() < deadline {
        // 每 80 帧（约 20 秒）换一个方向：走到一处停下就没有差异可看，
        // 而我们要量的正是**走动时**的构成。
        if frames.is_multiple_of(80) {
            let (dx, dz) = match leg % 4 {
                0 => (40, 0),
                1 => (0, 40),
                2 => (-40, 0),
                _ => (0, -40),
            };
            let _ = module
                .execute(DoorCommand::GoTo([
                    f64::from(x + dx),
                    f64::from(y),
                    f64::from(z + dz),
                ]))
                .await;
            leg += 1;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        match module.scan_changes(&memory, &ViewportOptions::default()) {
            Ok(changes) => {
                frames += 1;
                if changes.is_empty() {
                    empty += 1;
                    continue;
                }
                for change in &changes {
                    match change {
                        BlockChange::Appeared { .. } => appeared += 1,
                        BlockChange::Changed { .. } => changed += 1,
                        BlockChange::Vanished { .. } => vanished += 1,
                    }
                }
            }
            Err(_) => continue,
        }
    }

    let total = appeared + changed + vanished;
    let lines = appeared + changed * 2 + vanished;
    let pct = |n: usize| {
        if total == 0 {
            0.0
        } else {
            n as f64 * 100.0 / total as f64
        }
    };
    println!();
    println!(
        "采样 {frames} 帧（其中空 diff {empty} 帧），记忆里 {} 格",
        memory.lock().map(|m| m.len()).unwrap_or(0)
    );
    println!("条数合计 {total}");
    println!("  Appeared（+ 一行）  {appeared:>6}  {:.1}%", pct(appeared));
    println!(
        "  Changed （- 与 + 两行）{changed:>6}  {:.1}%",
        pct(changed)
    );
    println!("  Vanished（- 一行）  {vanished:>6}  {:.1}%", pct(vanished));
    println!();
    println!(
        "渲染行数 {lines}（= A + 2C + V），行/条 = {:.2}",
        if total == 0 {
            0.0
        } else {
            lines as f64 / total as f64
        }
    );
    println!(
        "其中 + 行 {}，- 行 {}",
        appeared + changed,
        changed + vanished
    );

    if let Err(reason) = module.stop("探针结束").await {
        eprintln!("[探针] 停机未合流（{reason}）");
    }
    Ok(())
}
