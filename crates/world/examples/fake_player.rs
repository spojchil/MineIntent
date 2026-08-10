//! 冒烟用假玩家：进服、说一句话、把随后听到的聊天打印出来。
//!
//! 用法（需要 `--features azalea`）：
//! `cargo run -p world --features azalea --example fake_player -- <host> <port> <名字> <要说的话> [倾听秒数]`

use std::sync::Arc;
use std::time::Duration;

use world::{ConnectionConfig, Module, SnapshotSource};

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let host = args.next().unwrap_or_else(|| "127.0.0.1".to_owned());
    let port: u16 = args
        .next()
        .unwrap_or_else(|| "25565".to_owned())
        .parse()
        .map_err(|error| format!("端口无效：{error}"))?;
    let username = args.next().unwrap_or_else(|| "tester".to_owned());
    let message = args.next().unwrap_or_else(|| "你好".to_owned());
    let listen_secs: u64 = args
        .next()
        .unwrap_or_else(|| "60".to_owned())
        .parse()
        .map_err(|error| format!("倾听秒数无效：{error}"))?;

    let module = Arc::new(
        Module::start(ConnectionConfig {
            host,
            port,
            username: username.clone(),
        })
        .map_err(|error| format!("启动失败：{error}"))?,
    );
    module.wait_ready(Duration::from_secs(60)).await?;
    println!("[假玩家] 已进入世界，说：{message}");
    module.send_chat_line(&message).await?;

    let mut cursor: Option<u64> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(listen_secs);
    loop {
        tokio::select! {
            _ = module.ticked() => {
                let snapshot = module.latest();
                for entry in &snapshot.chat.entries {
                    if cursor.is_none_or(|seen| entry.seq > seen) {
                        let sender = entry
                            .sender
                            .as_ref()
                            .map(|player| player.username.as_str())
                            .unwrap_or("[系统]");
                        println!("[聊天] {sender}: {}", entry.content.plain_text);
                        cursor = Some(entry.seq);
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline) => break,
        }
    }
    if let Err(reason) = module.stop("冒烟结束").await {
        eprintln!("[假玩家] 停机未合流（{reason}），交由进程退出回收");
    }
    println!("[假玩家] 已退出");
    Ok(())
}
