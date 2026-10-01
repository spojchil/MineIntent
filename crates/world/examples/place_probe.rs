//! 放置冒烟探针（自编排）：在出生点旁的垫块场地放两块，并逐条试门层的如实
//! 拒绝。服务端方块查证（权威源）由运行者经 rcon 另行采集，本探针只打印门的
//! 受理/拒绝原文（旁证）。
//!
//! 前置（编排方经 rcon 完成）：垫块 stone@(-6,72,7)，探针上线后被 tp 到
//! (-8.5,73,7.5)，hotbar.0 放圆石。
//!
//! 用法（需要 `--features azalea`）：
//! `cargo run -p world --release --features azalea --example place_probe -- <host> <port> <名字>`

use std::sync::Arc;
use std::time::Duration;

use world::{ConnectionConfig, DoorCommand, Module};

async fn step(module: &Arc<Module>, label: &str, command: DoorCommand) {
    match module.execute(command).await {
        Ok(()) => println!("[探针] {label}：受理"),
        Err(reason) => println!("[探针] {label}：拒绝——{reason}"),
    }
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
    let username = args.next().unwrap_or_else(|| "placer".to_owned());

    let module = Arc::new(
        Module::start(ConnectionConfig {
            host,
            port,
            username,
        })
        .map_err(|error| format!("启动失败：{error}"))?,
    );
    module.wait_ready(Duration::from_secs(60)).await?;
    // 给编排方留 tp + 发物品的窗口，也给首登陆的区块生成留时间。
    tokio::time::sleep(Duration::from_secs(20)).await;
    println!("[探针] 就位，开始放置序列");

    // 手持圆石，贴垫块放第一块，再往上摞第二块。
    step(&module, "选中 0 号格（圆石）", DoorCommand::SelectSlot(0)).await;
    step(
        &module,
        "放 A (-6,73,7)",
        DoorCommand::PlaceBlock([-6, 73, 7]),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    step(
        &module,
        "放 B (-6,74,7)",
        DoorCommand::PlaceBlock([-6, 74, 7]),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // 如实拒绝三连：已占用、悬空、太远。
    step(
        &module,
        "重放 A（应拒：已有方块）",
        DoorCommand::PlaceBlock([-6, 73, 7]),
    )
    .await;
    step(
        &module,
        "悬空 (-8,78,7)（应拒：无依附）",
        DoorCommand::PlaceBlock([-8, 78, 7]),
    )
    .await;
    step(
        &module,
        "远处 (-6,85,20)（应拒：太远）",
        DoorCommand::PlaceBlock([-6, 85, 20]),
    )
    .await;
    // 收尾前停 20 秒，给运行者留 rcon 查证窗口。
    tokio::time::sleep(Duration::from_secs(20)).await;
    let _ = module.stop("探针结束").await;
    println!("[探针] 完成");
    Ok(())
}
