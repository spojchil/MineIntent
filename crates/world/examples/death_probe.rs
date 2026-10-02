//! 死亡期间位置探针：连上后每秒打印一次客户端自认的位置、速度、是否着地、是否活着
//! （旁证）；编排方经 rcon 把它放上平台、杀死，并同时取服务端坐标（权威源）。
//! 原版死后头 20 tick 只清输入、重力与碰撞照常，之后本地玩家移出世界不再模拟
//! （服务端同时收回全部区块）：应停在平台上，不会穿地往下掉，复活后恢复正常。
//!
//! 用法（需要 `--features azalea`）：
//! `cargo run -p world --release --features azalea --example death_probe -- <host> <port> <名字> <秒数>`

use std::sync::Arc;
use std::time::Duration;

use world::{ConnectionConfig, DoorCommand, Module, SnapshotSource};

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "127.0.0.1".to_owned());
    let port: u16 = args
        .next()
        .unwrap_or_else(|| "25565".to_owned())
        .parse()
        .map_err(|error| format!("端口无效：{error}"))?;
    let username = args.next().unwrap_or_else(|| "dier".to_owned());
    let seconds: u64 = args
        .next()
        .unwrap_or_else(|| "30".to_owned())
        .parse()
        .map_err(|error| format!("秒数无效：{error}"))?;

    let module = Arc::new(
        Module::start(ConnectionConfig {
            host,
            port,
            username,
        })
        .map_err(|error| format!("启动失败：{error}"))?,
    );
    module.wait_ready(Duration::from_secs(60)).await?;
    // 上次可能死着下线（自动复活已关）；活着时门会如实拒绝，忽略即可。
    let _ = module.execute(DoorCommand::Respawn).await;
    println!("[探针] 就位");

    for second in 0..seconds {
        let snapshot = module.latest();
        let me = &snapshot.self_state;
        println!(
            "[探针] t={second} 位置 ({:.2}, {:.2}, {:.2}) 速度 y {:.3} 着地 {} 活着 {} 生命 {}",
            me.position.x,
            me.position.y,
            me.position.z,
            me.velocity.y,
            me.on_ground,
            me.alive,
            me.health
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    println!("[探针] 结束");
    module.stop("探针结束").await
}
