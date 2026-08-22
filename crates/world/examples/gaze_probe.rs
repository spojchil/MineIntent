//! 视向地面真值探针（自编排）：用 Face 动词自转五向，各向扫描并打印
//! 准星落点与可见标志方块；随后面北做一次 Forward，打印位移方向。
//! 方向真值由运行者预先搭好的四色墙提供（北钻石/南金/东绿宝石/西铁），
//! 服务端 Rotation/Pos 查询作旁证由运行者另行采集。
//!
//! 用法（需要 `--features azalea`）：
//! `cargo run -p world --features azalea --example gaze_probe -- <host> <port> <名字>`

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
    let username = args.next().unwrap_or_else(|| "gazer".to_owned());

    let module = Arc::new(
        Module::start(ConnectionConfig {
            host,
            port,
            username,
        })
        .map_err(|error| format!("启动失败：{error}"))?,
    );
    module.wait_ready(Duration::from_secs(60)).await?;
    // 给区块加载与世界模型就绪留时间。
    tokio::time::sleep(Duration::from_secs(5)).await;
    println!("[探针] 已进入世界，开始五向自转");

    for (yaw, pitch, expected) in [
        (0.0, 0.0, "南→gold"),
        (90.0, 0.0, "西→iron"),
        (180.0, 0.0, "北→diamond"),
        (-90.0, 0.0, "东→emerald"),
        (0.0, -90.0, "上→天(空气)"),
    ] {
        module
            .execute(DoorCommand::Face { yaw, pitch })
            .await
            .map_err(|error| format!("face 被拒：{error}"))?;
        for _ in 0..20 {
            module.ticked().await;
        }
        let snapshot = module.latest();
        match module.scan(&world::ViewportOptions::default()) {
            Ok(projection) => {
                let looked = projection
                    .looked_at_block
                    .as_ref()
                    .map(|block| {
                        format!(
                            "{} @({},{},{})",
                            block.name, block.position[0], block.position[1], block.position[2]
                        )
                    })
                    .unwrap_or_else(|| "无落点".to_owned());
                let mut names: Vec<&str> = projection
                    .visible_blocks
                    .blocks
                    .iter()
                    .map(|block| block.name.as_str())
                    .filter(|name| name.ends_with("_block"))
                    .collect();
                names.sort_unstable();
                names.dedup();
                println!(
                    "[探针] 期望[{expected}] 实际 yaw={:.0} pitch={:.0} 准星={} 标志墙={:?}",
                    snapshot.self_state.yaw, snapshot.self_state.pitch, looked, names
                );
            }
            Err(reason) => println!("[探针] 期望[{expected}] 扫描被拒：{reason}"),
        }
    }

    // Forward 方向真值：面北直走，位移应朝 −z。
    module
        .execute(DoorCommand::Face {
            yaw: 180.0,
            pitch: 0.0,
        })
        .await
        .map_err(|error| format!("face 被拒：{error}"))?;
    for _ in 0..10 {
        module.ticked().await;
    }
    let before = module.latest().self_state.position;
    println!(
        "[探针] Forward(4) 前 pos=({:.1},{:.1},{:.1}) yaw=180（北）",
        before.x, before.y, before.z
    );
    module
        .execute(DoorCommand::Forward(4.0))
        .await
        .map_err(|error| format!("forward 被拒：{error}"))?;
    tokio::time::sleep(Duration::from_secs(6)).await;
    let after = module.latest().self_state.position;
    println!(
        "[探针] Forward(4) 后 pos=({:.1},{:.1},{:.1}) Δ=({:+.1},{:+.1},{:+.1})（期望 Δz≈-4）",
        after.x,
        after.y,
        after.z,
        after.x - before.x,
        after.y - before.y,
        after.z - before.z
    );
    // 收尾前停 10 秒，给运行者留 rcon 旁证窗口（查 Rotation/Pos）。
    tokio::time::sleep(Duration::from_secs(10)).await;
    let _ = module.stop("探针结束").await;
    println!("[探针] 完成");
    Ok(())
}
