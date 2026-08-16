//! 工作台链路探针（机器级，无模型）：use_on 开屏 → 屏事实 → swap 摆料 →
//! 成品出现 → 取成品 → close。
//!
//! 前置（由运行者用服务器命令准备）：探针名下有橡木木板（快捷栏首格），
//! 且 `<x> <y> <z>` 处有一张工作台、探针在可及范围内。
//!
//! 用法（需要 `--features azalea`）：
//! `cargo run -p world --features azalea --example crafting_probe -- <host> <port> <名字> <x> <y> <z>`

use std::sync::Arc;
use std::time::Duration;

use world::{ConnectionConfig, DoorCommand, Module, ScreenEvent, SnapshotSource};

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "127.0.0.1".to_owned());
    let port: u16 = args
        .next()
        .unwrap_or_else(|| "25565".to_owned())
        .parse()
        .map_err(|error| format!("端口无效：{error}"))?;
    let username = args.next().unwrap_or_else(|| "prober".to_owned());
    let x: i32 = args.next().unwrap_or_else(|| "32".to_owned()).parse().unwrap();
    let y: i32 = args.next().unwrap_or_else(|| "73".to_owned()).parse().unwrap();
    let z: i32 = args.next().unwrap_or_else(|| "1".to_owned()).parse().unwrap();

    let module = Arc::new(
        Module::start(ConnectionConfig {
            host,
            port,
            username,
        })
        .map_err(|error| format!("启动失败：{error}"))?,
    );
    module.wait_ready(Duration::from_secs(60)).await?;
    println!("[探针] 已进入世界；10 秒后对 ({x}, {y}, {z}) 使用（给运行者留出摆道具时间）");
    tokio::time::sleep(Duration::from_secs(10)).await;

    // 判别实验：先看向方块中心再使用——命中结果为真实射线，
    // 不走 force_block 的伪造命中路径。
    module
        .execute(DoorCommand::LookAt([
            f64::from(x) + 0.5,
            f64::from(y) + 0.5,
            f64::from(z) + 0.5,
        ]))
        .await
        .map_err(|error| format!("look_at 被拒：{error}"))?;
    wait_ticks(&module, 5).await;
    module
        .execute(DoorCommand::UseOnBlock([x, y, z]))
        .await
        .map_err(|error| format!("use_on 被拒：{error}"))?;
    println!("[探针] look_at + use_on 已发出，等待屏事实……");

    // 等开屏事实。
    let mut screen_cursor: Option<u64> = None;
    let opened = wait_for_screen(&module, &mut screen_cursor, true, 100).await;
    let Some(kind) = opened else {
        let snapshot = module.latest();
        println!(
            "[探针] ✗ 100 tick 内没有开屏事实。open_screen={:?} screens={} 条",
            snapshot.open_screen,
            snapshot.screens.entries.len()
        );
        let _ = module.stop("探针结束").await;
        return Err("开屏失败".to_owned());
    };
    println!("[探针] ✓ 开屏：{kind}");
    print_slots(&module, "开屏时");

    // 找到木板所在格（工作台格空间），摆成木棍配方：上下两格（2 与 5）。
    let planks = find_slot(&module, "oak_planks").ok_or("背包里没有橡木木板")?;
    println!("[探针] 木板在格 {planks}；swap({planks}, 2) 入摆料格");
    module
        .execute(DoorCommand::SwapSlots { a: planks, b: 2 })
        .await
        .map_err(|error| format!("swap 入格 2 被拒：{error}"))?;
    wait_ticks(&module, 10).await;
    // 再找一格木板（如果原格还有剩就还是那格——swap 是整组，所以此时
    // 木板整组在格 2；把它对半分不可能，改用第二组来源：从格 2 拿回一部分
    // 做不到——直接用两组的前提不成立时，就用格 2 与格 5 之间倒手验证
    // swap 语义，然后靠服务器判空。简化：把格 2 整组换到 5，再从背包找
    // 第二组；找不到第二组就退化为验证「单格无成品」+ 取消。
    let second = find_slot(&module, "oak_planks");
    match second {
        Some(slot) if slot != 2 => {
            println!("[探针] 第二组木板在格 {slot}；swap({slot}, 5)");
            module
                .execute(DoorCommand::SwapSlots { a: slot, b: 5 })
                .await
                .map_err(|error| format!("swap 入格 5 被拒：{error}"))?;
        }
        _ => {
            println!("[探针] 只有一组木板：改验证 2→5 倒手后无成品路径");
        }
    }
    wait_ticks(&module, 20).await;
    print_slots(&module, "摆料后");

    let snapshot = module.latest();
    let result_filled = snapshot
        .self_state
        .inventory
        .slots
        .iter()
        .any(|slot| slot.slot == 0);
    println!(
        "[探针] 成品格：{}",
        if result_filled { "有内容 ✓" } else { "空 ✗" }
    );
    if result_filled {
        println!("[探针] swap(0, 44) 取成品到快捷栏末格");
        module
            .execute(DoorCommand::SwapSlots { a: 0, b: 44 })
            .await
            .map_err(|error| format!("取成品被拒：{error}"))?;
        wait_ticks(&module, 20).await;
        print_slots(&module, "取成品后");
    }

    println!("[探针] close");
    module
        .execute(DoorCommand::CloseContainer)
        .await
        .map_err(|error| format!("close 被拒：{error}"))?;
    let closed = wait_for_screen(&module, &mut screen_cursor, false, 100).await;
    println!(
        "[探针] 关屏事实：{}",
        closed.map_or("未见 ✗".to_owned(), |kind| format!("{kind} ✓"))
    );
    let _ = module.stop("探针结束").await;
    println!("[探针] 完成");
    Ok(())
}

async fn wait_ticks(module: &Module, count: u32) {
    for _ in 0..count {
        module.ticked().await;
    }
}

/// 等下一条开/关屏事实，返回种类名。`want_open`=true 等 Opened，否则 Closed。
async fn wait_for_screen(
    module: &Module,
    cursor: &mut Option<u64>,
    want_open: bool,
    max_ticks: u32,
) -> Option<String> {
    // 起点：跳过存量。
    if cursor.is_none() {
        *cursor = module.latest().screens.entries.last().map(|entry| entry.seq);
    }
    for _ in 0..max_ticks {
        module.ticked().await;
        let snapshot = module.latest();
        for entry in &snapshot.screens.entries {
            if cursor.is_some_and(|seen| entry.seq <= seen) {
                continue;
            }
            *cursor = Some(entry.seq);
            match (&entry.event, want_open) {
                (ScreenEvent::Opened { kind, .. }, true) => return Some(kind.clone()),
                (ScreenEvent::Closed { kind }, false) => return Some(kind.clone()),
                _ => {}
            }
        }
    }
    None
}

fn find_slot(module: &Module, item: &str) -> Option<u16> {
    module
        .latest()
        .self_state
        .inventory
        .slots
        .iter()
        .find(|slot| slot.item_name == item && slot.slot != 0)
        .map(|slot| slot.slot as u16)
}

fn print_slots(module: &Module, label: &str) {
    let snapshot = module.latest();
    let mut slots: Vec<String> = snapshot
        .self_state
        .inventory
        .slots
        .iter()
        .map(|slot| format!("{}={}×{}", slot.slot, slot.item_name, slot.count))
        .collect();
    slots.sort();
    println!(
        "[探针] {label} open_screen={:?} 非空格位：{}",
        snapshot.open_screen.as_ref().map(|screen| &screen.kind),
        slots.join("、")
    );
}
