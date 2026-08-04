//! 纵向实测用假人：以测试身份进服，spawn 后发一句指名聊天，驻留记录聊天。
//!
//! 26.1.2（协议 775）下 mineflayer 假人无法进服，本假人用同一份 vendored
//! Azalea，协议一致性由构造保证。仅供本地纵向验收，不进产品面。
//!
//! 环境变量：FAKE_HOST/FAKE_PORT/FAKE_USERNAME/FAKE_MESSAGE/FAKE_SECONDS。

use std::time::Duration;

use azalea::prelude::*;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_owned())
}

#[derive(Clone, Component, Default)]
struct FakeState;

async fn handle(bot: Client, event: Event, _state: FakeState) {
    match event {
        Event::Login => {
            println!("[fake] 已登录：{}", bot.username());
        }
        Event::Spawn => {
            println!("[fake] 已出生，2 秒后发话");
            tokio::time::sleep(Duration::from_secs(2)).await;
            let message = env_or("FAKE_MESSAGE", "MineIntentBot 你好，帮我看看周围");
            println!("[fake] 发送聊天：{message}");
            bot.chat(&message);
        }
        Event::Chat(packet) => {
            println!("[fake] CHAT {}", packet.message().to_ansi());
        }
        _ => {}
    }
}

#[tokio::main]
async fn main() {
    let host = env_or("FAKE_HOST", "127.0.0.1");
    let port = env_or("FAKE_PORT", "25565");
    let seconds: u64 = env_or("FAKE_SECONDS", "90").parse().unwrap_or(90);
    let username = env_or("FAKE_USERNAME", "Alice");
    let account = Account::offline(&username);
    let address = format!("{host}:{port}");
    println!("[fake] 连接 {address}，身份 {username}，驻留 {seconds}s");
    let run = ClientBuilder::new()
        .set_handler(handle)
        .set_state(FakeState)
        .start(account, address.as_str());
    match tokio::time::timeout(Duration::from_secs(seconds), run).await {
        Ok(exit) => println!("[fake] 客户端自行退出：{exit:?}"),
        Err(_) => println!("[fake] 驻留期满，退出"),
    }
}
