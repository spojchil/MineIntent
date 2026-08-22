//! 合成时序探针（机器级，无模型）：摆一块料、等一等、再摆一块，每一步都同时打印
//!
//! - **本地活动菜单**里成品格现在是什么（写口判据读的就是这一份）；
//! - **格位变化窗**这一步新增了哪些条目（模型收到的通知就是从这里出的）。
//!
//! 要回答的问题只有一个：通知与引起它的动作是不是对得上。实盘里模型把
//! 「按钮」归因给了竖排两块木板、把「木棍」归因给了横排两块，据此建立了一套
//! 错误的格位几何——怀疑通知落后一步，但那是从模型行为反推的，需要机器级坐实。
//!
//! 服务端权威由运行脚本在每步之间用 `data get entity <名> Inventory` 取，
//! 本探针只负责把客户端两侧的时间点打出来。
//!
//! 前置：探针名下有橡木木板，`<x> <y> <z>` 处有工作台且在可及范围内。
//!
//! 用法（需要 `--features azalea`）：
//! `cargo run -p world --features azalea --example craft_timing_probe -- <host> <port> <名字> <x> <y> <z>`

use std::sync::Arc;
use std::time::Duration;

use world::{ConnectionConfig, DoorCommand, Module, SnapshotSource};

fn arg<T: std::str::FromStr>(args: &mut impl Iterator<Item = String>, fallback: T) -> T {
    args.next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(fallback)
}

/// 成品格（0）在本地活动菜单里的现状，取自快照的格位清单。
fn result_slot(snapshot: &world::TickSnapshot) -> String {
    snapshot
        .self_state
        .inventory
        .slots
        .iter()
        .find(|entry| entry.slot == 0)
        .map(|entry| format!("{} ×{}", entry.item_name, entry.count))
        .unwrap_or_else(|| "空".to_owned())
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "127.0.0.1".to_owned());
    let port: u16 = arg(&mut args, 25565);
    let username = args.next().unwrap_or_else(|| "timing".to_owned());
    let x: i32 = arg(&mut args, 0);
    let y: i32 = arg(&mut args, 64);
    let z: i32 = arg(&mut args, 0);

    let module = Arc::new(
        Module::start(ConnectionConfig {
            host,
            port,
            username,
        })
        .map_err(|error| format!("启动失败：{error}"))?,
    );
    module.wait_ready(Duration::from_secs(60)).await?;
    println!("[探针] 已进入世界；10 秒后开工（留出摆料时间）");
    tokio::time::sleep(Duration::from_secs(10)).await;

    module
        .execute(DoorCommand::LookAt([
            f64::from(x) + 0.5,
            f64::from(y) + 0.5,
            f64::from(z) + 0.5,
        ]))
        .await?;
    module.execute(DoorCommand::UseOnBlock([x, y, z])).await?;
    println!("[探针] use_on 已发出，等开屏……");

    let mut opened = false;
    for _ in 0..60 {
        module.ticked().await;
        if module.latest().open_screen.is_some() {
            opened = true;
            break;
        }
    }
    if !opened {
        return Err("60 tick 内没等到开屏".to_owned());
    }
    let snapshot = module.latest();
    println!(
        "[探针] ✓ 开屏 tick={} 种类={}",
        snapshot.tick,
        snapshot
            .open_screen
            .as_ref()
            .map(|screen| screen.kind.as_str())
            .unwrap_or("?")
    );

    // 每步之前重新找木板：格号会随点击变（服务端可能把余量落到别处），
    // 缓存第一步之前的值会让第二步对着空格发命令。
    let find_planks = |module: &Module| {
        module
            .latest()
            .self_state
            .inventory
            .slots
            .iter()
            .find(|entry| entry.item_name.contains("planks"))
            .map(|entry| entry.slot as u16)
    };

    let mut seen_changes = 0usize;
    // 每一步：发命令 → 立刻读一次 → 之后连续 10 tick 逐 tick 读。
    // 「立刻」那次就是工具回执能看到的时刻；后面几次显示真相什么时候到。
    for (step, target) in [(1u32, 2u16), (2, 5)] {
        let Some(source) = find_planks(&module) else {
            println!("\n===== 第 {step} 步：身上已经找不到木板，停 =====");
            break;
        };
        println!("\n===== 第 {step} 步：格 {source} → 格 {target}（各放 1 块）=====");
        module
            .execute(DoorCommand::MoveSlots {
                from: world::slots::SlotSpace::player().describe(source),
                to: world::slots::SlotSpace::player().describe(target),
                count: Some(1),
            })
            .await?;
        let immediate = module.latest();
        println!(
            "  写口回执当下：tick={} 成品格={} 变化窗条目数={}",
            immediate.tick,
            result_slot(&immediate),
            immediate.inventory_changes.entries.len()
        );
        for round in 1..=10 {
            module.ticked().await;
            let now = module.latest();
            let total = now.inventory_changes.entries.len();
            let fresh: Vec<String> = now
                .inventory_changes
                .entries
                .iter()
                .skip(seen_changes)
                .map(|entry| {
                    format!(
                        "格{}={}×{}({:?})",
                        entry.slot,
                        entry.item_name.as_deref().unwrap_or("空"),
                        entry.count,
                        entry.source
                    )
                })
                .collect();
            if !fresh.is_empty() || round == 10 {
                println!(
                    "  +{round} tick：tick={} 成品格={} 新增变化 [{}]",
                    now.tick,
                    result_slot(&now),
                    fresh.join(", ")
                );
            }
            seen_changes = total;
        }
    }

    println!("\n[探针] 保持开屏 10 秒供服务端查询，然后关屏退出");
    tokio::time::sleep(Duration::from_secs(10)).await;
    module.execute(DoorCommand::CloseContainer).await.ok();
    if let Err(reason) = module.stop("时序探针结束").await {
        eprintln!("[探针] 停机未合流（{reason}）");
    }
    Ok(())
}
