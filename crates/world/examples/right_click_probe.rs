//! 右键链实服探针：按原版 `startUseItem` 主手→副手、实体/方块→用物品的顺序，
//! 逐个场景按一次右键，打印回执（旁证）。服务端物品数、饱食度、方块查证（权威源）
//! 由编排方经 rcon 在每步前后采集。
//!
//! 每步先打印 `[探针] 准备 N`，留 5 秒给编排方布场（发物品、tp 到固定眼位与朝向、
//! 放方块）；再按右键并打印 `[探针] 步骤 N 回执`，留 3 秒给编排方查询。
//!
//! 场景（编排方搭的悬空草方块平台）：
//! 1. 主手金苹果（饱着也能吃），低头看地，按住右键 2 秒——看着方块也该吃（方块不吃
//!    这次右键，原版退到「用物品」）。
//! 2. 主手金苹果，看着面前的箱子，按住右键 2 秒——只开箱子，不该吃。
//! 3. 主手空、副手火把，低头看地，点右键——主手一路 PASS，副手放下火把。
//! 4. 主手圆石，低头看地，点右键——照旧放方块。
//! 5. 主手雪球，低头看地，点右键——看着方块也该扔出去。
//!
//! 用法（需要 `--features azalea`）：
//! `cargo run -p world --release --features azalea --example right_click_probe -- <host> <port> <名字> <就位秒数>`

use std::sync::Arc;
use std::time::Duration;

use world::{ConnectionConfig, DoorCommand, HeldKeys, InputSpec, Module, MouseButton};

async fn right_click(module: &Arc<Module>, step: u32, label: &str, ticks: u32) {
    println!("[探针] 准备 {step}：{label}");
    tokio::time::sleep(Duration::from_secs(5)).await;
    let spec = InputSpec {
        keys: HeldKeys::default(),
        mouse: Some(MouseButton::Right),
        turn: None,
        ticks,
    };
    match module.input(spec).await {
        Ok(outcome) => println!("[探针] 步骤 {step} 回执：{outcome:?}"),
        Err(reason) => println!("[探针] 步骤 {step} 拒绝：{reason}"),
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
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
    let username = args.next().unwrap_or_else(|| "clicker".to_owned());
    let settle: u64 = args
        .next()
        .unwrap_or_else(|| "20".to_owned())
        .parse()
        .map_err(|error| format!("就位秒数无效：{error}"))?;

    let module = Arc::new(
        Module::start(ConnectionConfig {
            host,
            port,
            username,
        })
        .map_err(|error| format!("启动失败：{error}"))?,
    );
    module.wait_ready(Duration::from_secs(60)).await?;
    // 上次可能死在场地里（自动复活已关）；活着时门会如实拒绝，忽略即可。
    let _ = module.execute(DoorCommand::Respawn).await;
    tokio::time::sleep(Duration::from_secs(settle)).await;
    println!("[探针] 就位");

    right_click(&module, 1, "金苹果，看地，按住 2 秒（该吃）", 40).await;
    right_click(&module, 2, "金苹果，看箱子，按住 2 秒（只开箱）", 40).await;
    right_click(&module, 3, "主手空、副手火把，看地，点按（副手放火把）", 1).await;
    right_click(&module, 4, "圆石，看地，点按（放方块）", 1).await;
    right_click(&module, 5, "雪球，看地，点按（扔出去）", 1).await;

    println!("[探针] 结束");
    module.stop("探针结束").await
}
