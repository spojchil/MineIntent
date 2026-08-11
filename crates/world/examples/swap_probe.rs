//! 交换探针：验证"同一 tick 内三连 SWAP 点击"能否完成任意两格交换。
//!
//! 期望（外部先用 /item 摆好道具）：菜单格 10 ↔ 菜单格 20 互换，
//! 中转的快捷栏 0（菜单格 36，**非空**）三步轮换后原样复位。
//!
//! 用法：`cargo run -p world --features azalea --example swap_probe -- <host> <port> <名字>`
//! 进服后每 100 tick 打印一次三格内容；第 300 tick 在单个 Tick 回调内
//! 连发三包，随后继续打印，观察服务器是否全部接受（以及有无回滚纠正）。

use azalea::container::ContainerHandleRef;
use azalea::inventory::operations::{ClickOperation, SwapClick};
use azalea::prelude::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Clone, Component)]
struct ProbeState {
    ticks: Arc<AtomicU64>,
    /// 0=未武装；非 0=计划点击的 tick。
    clicked_at: Arc<AtomicU64>,
}

impl Default for ProbeState {
    fn default() -> Self {
        Self {
            ticks: Arc::new(AtomicU64::new(0)),
            clicked_at: Arc::new(AtomicU64::new(0)),
        }
    }
}

fn describe(menu: &azalea::inventory::Menu, index: usize) -> String {
    let slots = menu.slots();
    match slots.get(index) {
        Some(slot) if !slot.is_empty() => format!("{slot:?}"),
        Some(_) => "空".to_owned(),
        None => "越界".to_owned(),
    }
}

async fn handle(bot: Client, event: Event, state: ProbeState) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Event::Tick = event {
        let tick = state.ticks.fetch_add(1, Ordering::AcqRel) + 1;
        let armed_now = state.clicked_at.load(Ordering::Acquire);
        if tick % 100 == 0 || (armed_now != 0 && tick + 10 >= armed_now && tick <= armed_now + 10) {
            let menu = bot.menu();
            println!(
                "[tick {tick}] 格10={} | 格20={} | 格36(快捷0)={}",
                describe(&menu, 10),
                describe(&menu, 20),
                describe(&menu, 36),
            );
        }
        // 自触发：两格道具都就位后再等 40 tick（让状态安定），同 tick 连发三包。
        let armed = state.clicked_at.load(Ordering::Acquire);
        if armed == 0 {
            let menu = bot.menu();
            let slots = menu.slots();
            let ready = [10usize, 20].iter().all(|&i| !slots[i].is_empty());
            if ready {
                state.clicked_at.store(tick + 40, Ordering::Release);
                println!("[tick {tick}] 道具就位，{} tick 后同 tick 连发三包", 40);
            }
        } else if tick == armed {
            println!("[tick {tick}] 同 tick 连发三包：10↔快捷0、20↔快捷0、10↔快捷0");
            let handle = ContainerHandleRef::new(0, bot.clone());
            for source_slot in [10u16, 20, 10] {
                handle.click(ClickOperation::Swap(SwapClick {
                    source_slot,
                    target_slot: 0,
                }));
            }
        } else if tick == armed + 100 {
            println!("[探针] 观察结束");
            std::process::exit(0);
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "127.0.0.1".to_owned());
    let port: u16 = args.next().unwrap_or_else(|| "25565".to_owned()).parse()?;
    let username = args.next().unwrap_or_else(|| "swapprobe".to_owned());

    let address = format!("{host}:{port}");
    ClientBuilder::new()
        .set_handler(handle)
        .start(Account::offline(&username), address.as_str())
        .await;
    Ok(())
}
