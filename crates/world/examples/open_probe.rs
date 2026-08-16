//! 开容器探针（直连 azalea，不经本仓门层）：进服后用 azalea 自家的
//! `open_container_at` 打开指定坐标的容器，打印结果。
//!
//! 判别用途：它成功 = 本仓 UseOnBlock 门路径有缺陷；它也失败 = 上游或
//! 服务器侧问题。
//!
//! 用法：`cargo run -p world --features azalea --example open_probe -- <host> <port> <名字> <x> <y> <z>`

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use azalea::prelude::*;
use azalea::BlockPos;

#[derive(Clone, Component)]
struct ProbeState {
    ticks: Arc<AtomicU64>,
    busy: Arc<AtomicBool>,
}

impl Default for ProbeState {
    fn default() -> Self {
        Self {
            ticks: Arc::new(AtomicU64::new(0)),
            busy: Arc::new(AtomicBool::new(false)),
        }
    }
}

async fn handle(
    bot: Client,
    event: Event,
    state: ProbeState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Event::Packet(packet) = &event {
        use azalea::protocol::packets::game::ClientboundGamePacket;
        match &**packet {
            ClientboundGamePacket::OpenScreen(p) => {
                println!("[开探针] 包级：收到 OpenScreen {p:?}");
            }
            ClientboundGamePacket::ContainerSetContent(p) => {
                println!(
                    "[开探针] 包级：收到 ContainerSetContent id={:?}（{} 格）",
                    p.container_id,
                    p.items.len()
                );
            }
            _ => {}
        }
    }
    if let Event::Tick = event {
        let tick = state.ticks.fetch_add(1, Ordering::AcqRel) + 1;
        // 200 tick（10 秒）后触发一次：给运行者留 tp/给道具时间。
        if tick == 200 && !state.busy.swap(true, Ordering::AcqRel) {
            let args: Vec<String> = std::env::args().collect();
            let x: i32 = args.get(4).map_or(32, |v| v.parse().unwrap_or(32));
            let y: i32 = args.get(5).map_or(73, |v| v.parse().unwrap_or(73));
            let z: i32 = args.get(6).map_or(1, |v| v.parse().unwrap_or(1));
            let bot = bot.clone();
            tokio::spawn(async move {
                println!("[开探针] open_container_at({x}, {y}, {z}) ……");
                match bot.open_container_at(BlockPos::new(x, y, z)).await {
                    Some(handle) => {
                        println!(
                            "[开探针] ✓ 打开成功：容器 id={}，菜单={:?}，格数={}",
                            handle.id(),
                            handle.menu().map(|menu| format!("{:?}", std::mem::discriminant(&menu))),
                            handle.menu().map_or(0, |menu| menu.len()),
                        );
                        handle.close();
                        println!("[开探针] 已关闭");
                    }
                    None => println!("[开探针] ✗ open_container_at 返回 None（10 tick 内菜单未出现）"),
                }
                std::process::exit(0);
            });
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let host = args.get(1).cloned().unwrap_or_else(|| "127.0.0.1".to_owned());
    let port: u16 = args.get(2).map_or(25565, |v| v.parse().unwrap_or(25565));
    let username = args.get(3).cloned().unwrap_or_else(|| "prober".to_owned());
    let address = format!("{host}:{port}");
    let _ = ClientBuilder::new()
        .set_handler(handle)
        .start(Account::offline(&username), address.as_str())
        .await;
}
