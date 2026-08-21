//! 垫柱探针：跳起来能不能在脚下放方块，以及**要隔多久才行**。
//!
//! `pillar_up` 那个临时工具已经拆掉（维护者 2026-08-21 裁：回到放置与挖掘两个
//! 工具上）。本探针**留着**——它测的不是那个工具，是「跳跃与放置之间的时序」
//! 这个事实，而放置那条线重做时还要靠它。它直接驱动 `DoorCommand::Jump` 与
//! `PlaceBlock`，不依赖任何已删的东西。
//!
//! 2026-08-19 实测（这台机器）：
//!
//! | jump→place 间隔 | 结果 |
//! | --- | --- |
//! | 0ms（同批相邻） | ✗ 发出时还在地上 |
//! | 50ms | ✗ 已离地但没升过那格 |
//! | **100~300ms** | **✓ 连垫 5 格** |
//! | 400ms 以上 | ✗ 已在下落 |
//!
//! **但这几个毫秒数只是表象。**真正的条件是几何的：脚底高过目标格顶面
//! （`feet_y >= target_y + 1`），服务端才不会判成「方块与玩家重叠」。谁来重做
//! 放置，判据都该取这条，不该取计时器——上一版 `pillar_up` 的卡住判据取了
//! 计时器（60 tick），而模型一轮 1~2 秒 < 3 秒，每次重发都把时钟按回零，
//! 于是那条正确的措辞一次都没送到过。
//!
//! 问题来自模型可见面：`motion.jump` 与 `hand.place` 是两个工具调用，同一批里相邻。
//! 派发是**串行**的（`dispatch::Dispatcher::dispatch` 逐条 `await`），两条之间几乎没有
//! 间隔——而跳跃要下一 tick 才起效，起跳瞬间人还占着脚下那格。所以「同批相邻」到底成
//! 不成，取决于放置发出时人离地了没有。本探针把间隔扫一遍，让它变成一个数。
//!
//! 判据用**方块本身**：放完等一拍再读脚下那格。服务端拒绝的放置会被同步回来纠正，
//! 所以读到非空气就是真放上了，不是客户端的幻影。
//!
//! 前置（编排方经 rcon 完成）：给探针一叠可放置的方块并选中 0 号格，场地上方留空。
//!
//! 用法（需要 `--features azalea`）：
//! `cargo run -p world --release --features azalea --example pillar_probe -- <host> <port> <名字>`

use std::sync::Arc;
use std::time::Duration;

use world::{ConnectionConfig, DoorCommand, Module, SnapshotSource, ViewportOptions};

/// 站立时脚下那格的整数坐标——垫柱要放的就是这一格。
fn feet_block(module: &Module) -> [i32; 3] {
    let snapshot = module.latest();
    let position = &snapshot.self_state.position;
    [
        position.x.floor() as i32,
        position.y.floor() as i32,
        position.z.floor() as i32,
    ]
}

fn on_ground(module: &Module) -> bool {
    module.latest().self_state.on_ground
}

/// 脚下那格现在是什么。定向观察只对**看得见**的格位有答案，所以这只是旁证；
/// 主判据是人有没有站上去（`feet_block` 的 y 抬高一格）——那个不依赖视线。
fn block_name(module: &Arc<Module>, at: [i32; 3]) -> String {
    match module.scan_directed(&[at], &ViewportOptions::default()) {
        Ok(view) => match view.seen.first() {
            Some(block) => block.name.clone(),
            None => "看不见".to_owned(),
        },
        Err(reason) => format!("读不到（{reason}）"),
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
    let username = args.next().unwrap_or_else(|| "pillar".to_owned());

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

    let _ = module.execute(DoorCommand::SelectSlot(0)).await;
    // 朝下看：`place` 自己会挑依附面，但视线决定 eye 位置进而决定触及判定。
    let _ = module
        .execute(DoorCommand::Face {
            yaw: 0.0,
            pitch: 85.0,
        })
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    println!("[探针] 就位于 {:?}，开始扫间隔", feet_block(&module));

    // 0 = 同批相邻的真实情形（派发串行，两条之间只隔一次 await）。
    for gap_ms in [0_u64, 50, 100, 150, 200, 250, 300, 400, 500] {
        // 每轮都要从站在地上开始，否则量的是上一轮的余波。
        for _ in 0..40 {
            if on_ground(&module) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let before = feet_block(&module);

        if let Err(reason) = module.execute(DoorCommand::Jump).await {
            println!("[探针] 间隔 {gap_ms}ms：jump 被拒——{reason}");
            continue;
        }
        if gap_ms > 0 {
            tokio::time::sleep(Duration::from_millis(gap_ms)).await;
        }
        let airborne = !on_ground(&module);
        let placed = module.execute(DoorCommand::PlaceBlock(before)).await;

        // 放置是瞬时动词，受理不等于生效；等落地，让服务端的结论同步回来。
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let after = feet_block(&module);
        // 主判据：人站高了一格 = 方块真的垫上了。它不依赖视线，也不依赖客户端幻影。
        let lifted = after[1] == before[1] + 1;

        println!(
            "[探针] 间隔 {gap_ms:>3}ms｜发出时{}｜门层：{}｜脚下 {before:?} 现在是 {}｜人{}",
            if airborne {
                "已离地"
            } else {
                "还在地上"
            },
            match &placed {
                Ok(()) => "受理".to_owned(),
                Err(reason) => format!("拒绝——{reason}"),
            },
            block_name(&module, before),
            if lifted {
                format!("升到 {after:?} ✓")
            } else {
                format!("仍在 {after:?} ✗")
            }
        );
        tokio::time::sleep(Duration::from_millis(400)).await;
    }

    if let Err(reason) = module.stop("探针结束").await {
        eprintln!("[探针] 停机未合流（{reason}）");
    }
    Ok(())
}
