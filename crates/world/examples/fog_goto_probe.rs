//! 战争迷雾 `go_to` 实盘探针。
//!
//! 从当前真实视野吸收一帧（不是全量区块），随后把相对坐标交给生产移动状态机。
//! 探针只观察公开的身体快照、知识版本与 job 事实；终局后额外留场，交叉检查任务槽
//! 已空且身体格不再移动。Azalea 内部组件的严格 retirement 由 fork 单测证明。
//!
//! ```text
//! cargo run --release -p world --features azalea --example fog_goto_probe -- \
//!   <host> <port> <username> <dx> <dy> <dz> [timeout_secs]
//! ```

use std::sync::{Arc, Mutex};
use std::time::Duration;

use azalea::pathfinder::player_pos_to_block_pos;
use world::{
    BlockMemory, ConnectionConfig, DoorCommand, JobFact, Module, MoveEvent, SnapshotSource,
    TickSnapshot, ViewportOptions,
};

fn body_block_pos(snapshot: &TickSnapshot) -> [i32; 3] {
    let at = player_pos_to_block_pos(azalea::Vec3::new(
        snapshot.self_state.position.x,
        snapshot.self_state.position.y,
        snapshot.self_state.position.z,
    ));
    [at.x, at.y, at.z]
}

fn parse<T: std::str::FromStr>(raw: Option<String>, fallback: T, label: &str) -> Result<T, String>
where
    T::Err: std::fmt::Display,
{
    raw.map_or(Ok(fallback), |value| {
        value
            .parse()
            .map_err(|error| format!("{label} 无效：{error}"))
    })
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "127.0.0.1".to_owned());
    let port = parse(args.next(), 25_565_u16, "端口")?;
    let username = args.next().unwrap_or_else(|| "fogprobe".to_owned());
    let dx = parse(args.next(), 16_i32, "dx")?;
    let dy = parse(args.next(), 0_i32, "dy")?;
    let dz = parse(args.next(), 0_i32, "dz")?;
    let timeout_secs = parse(args.next(), 180_u64, "timeout_secs")?;

    let module = Arc::new(Module::start(ConnectionConfig {
        host,
        port,
        username,
    })?);
    module.wait_ready(Duration::from_secs(60)).await?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    let memory = Arc::new(Mutex::new(BlockMemory::new()));
    module.use_observed_pathfinding(memory.clone());
    let visible = module.absorb(&memory, &ViewportOptions::for_memory())?;
    let before = module.latest();
    let here = body_block_pos(&before);
    let destination = [here[0] + dx, here[1] + dy, here[2] + dz];
    let (known_blocks, known_empty, revision) = memory
        .lock()
        .map(|memory| (memory.len(), memory.known_empty_len(), memory.revision()))
        .map_err(|_| "方块记忆锁中毒".to_owned())?;
    println!(
        "[探针] 起点 ({}, {}, {})，目标 {destination:?}；首帧可见 {visible}，已知方块 {known_blocks}，已知空格 {known_empty}，地图版本 {revision}",
        here[0], here[1], here[2]
    );

    let baseline_seq = before.jobs.entries.last().map_or(0, |entry| entry.seq);
    module
        .execute(DoorCommand::GoTo([
            f64::from(destination[0]),
            f64::from(destination[1]),
            f64::from(destination[2]),
        ]))
        .await?;
    println!("[探针] GO_TO_ACCEPTED {destination:?}");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let mut last_position = here;
    let mut last_revision = revision;
    let terminal = loop {
        if tokio::time::Instant::now() >= deadline {
            module.execute(DoorCommand::StopMoving).await.ok();
            module.stop("战争迷雾探针超时").await.ok();
            return Err(format!(
                "{timeout_secs} 秒内没有终局；最后位置 {last_position:?}、地图版本 {last_revision}"
            ));
        }
        module.ticked().await;
        let snapshot = module.latest();
        let position = body_block_pos(&snapshot);
        let revision = memory
            .lock()
            .map(|memory| memory.revision())
            .map_err(|_| "方块记忆锁中毒".to_owned())?;
        if position != last_position || revision != last_revision {
            println!(
                "[探针] PROGRESS 位置 {last_position:?} -> {position:?}，地图版本 {last_revision} -> {revision}"
            );
            last_position = position;
            last_revision = revision;
        }
        if let Some(event) = snapshot.jobs.entries.iter().find_map(|entry| {
            if entry.seq <= baseline_seq {
                return None;
            }
            let JobFact::Move {
                destination: event_destination,
                event,
            } = &entry.fact
            else {
                return None;
            };
            (*event_destination == destination && event.is_terminal()).then_some(*event)
        }) {
            break event;
        }
    };

    println!("[探针] TERMINAL {terminal:?}；位置 {last_position:?}，地图版本 {last_revision}");
    let quiet_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let terminal_position = last_position;
    let mut quiet_position = terminal_position;
    let mut moved_after_terminal = false;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(quiet_deadline) => break,
            _ = module.ticked() => {
                let position = body_block_pos(&module.latest());
                if position != quiet_position {
                    println!("[探针] POST_TERMINAL_MOVE {quiet_position:?} -> {position:?}");
                    quiet_position = position;
                    moved_after_terminal = true;
                }
            }
        }
    }
    let jobs_after = module.jobs_in_flight();
    println!(
        "[探针] QUIESCENT_CHECK in_flight={}，body_stationary={}（终局后 5 秒）",
        jobs_after.len(),
        !moved_after_terminal
    );
    if !jobs_after.is_empty() {
        module.execute(DoorCommand::StopMoving).await.ok();
        module.stop("战争迷雾探针发现残留任务").await.ok();
        return Err("终局后仍有移动任务在途".to_owned());
    }
    if moved_after_terminal {
        module.execute(DoorCommand::StopMoving).await.ok();
        module.stop("战争迷雾探针发现终局后位移").await.ok();
        return Err(format!(
            "终局后身体仍从 {terminal_position:?} 移到 {quiet_position:?}；可能是残留导航或外部物理，需检查"
        ));
    }

    // 把枚举显式用在探针里，新增终局时编译器会提醒我们重新检查成功口径。
    match terminal {
        MoveEvent::Arrived
        | MoveEvent::PathEnded
        | MoveEvent::DestinationRejected { .. }
        | MoveEvent::NavigationLimitReached { .. }
        | MoveEvent::DispatchNotObserved { .. }
        | MoveEvent::NoBodyProgressLimitReached { .. }
        | MoveEvent::Replaced
        | MoveEvent::Cancelled
        | MoveEvent::ConnectionEnded => {}
        MoveEvent::Leg { .. } | MoveEvent::Stalled => unreachable!("上面只接受终局事件"),
    }
    module.stop("战争迷雾探针结束").await?;
    Ok(())
}
