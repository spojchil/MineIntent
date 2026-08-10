//! 组合根：第一次真实对话的纵切装配。
//!
//! 接线：接入模块（azalea）→ 门适配器 → screens/memory 注册进 dispatch →
//! ContextStrategy 填内核提示与压缩端口 → openai 兼容适配器连模型 →
//! 最小唤醒脚手架（聊天→件投递）。脚手架不是己：唤醒判据（AttentionSpec）
//! 未裁前，这里只做"别人对我说话就醒"这一条最朴素的规则。
//!
//! 配置全走环境变量；API key 只从文件读，不进命令行与日志。

use std::sync::Arc;
use std::time::Duration;

use agent::adapters::http::{HttpModel, HttpModelConfig, Protocol};
use agent::{AgentSession, InputMessage, MailboxInput, SessionConfig};
use context::ContextStrategy;
use dispatch::{Dispatcher, Occupancy, ToolProvider};
use memory::{MemoryFile, MemoryTools};
use screens::{ChatBox, ChatDoor, ChatHistory, ChatReadMark};
use world::{ConnectionConfig, Module, SnapshotSource};

/// 人设占位（Q01 未裁；正式文本由维护者给出后替换）。
const PLACEHOLDER_PERSONA: &str = "\
你是这个 Minecraft 世界里的一位同伴，说中文。你通过工具行动：想说话就用 chat_box\
（say 发言，history 翻记录），想记住什么就用 remember 改写你的记忆。\
别人对你说的话会传到你这里；安静时什么都不做也可以。";

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn read_api_key() -> Result<String, String> {
    if let Ok(path) = std::env::var("MINEINTENT_MODEL_API_KEY_FILE") {
        return std::fs::read_to_string(&path)
            .map(|key| key.trim().to_owned())
            .map_err(|error| format!("读取 API key 文件失败（{path}）：{error}"));
    }
    std::env::var("MODEL_API_KEY")
        .map_err(|_| "缺少 MINEINTENT_MODEL_API_KEY_FILE（推荐）或 MODEL_API_KEY".to_owned())
}

/// 接入模块 → screens 门的窄化适配。
struct ModuleChatDoor(Arc<Module>);

impl ChatDoor for ModuleChatDoor {
    fn send_chat<'a>(&'a self, line: &'a str) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.send_chat_line(line).await })
    }
}

struct ModuleChatHistory(Arc<Module>);

impl ChatHistory for ModuleChatHistory {
    fn recent(&self, count: usize) -> Vec<String> {
        self.0.recent_chat(count)
    }
}

#[tokio::main]
async fn main() -> Result<(), String> {
    // ---- 配置 ----
    let host = env_or("MINEINTENT_HOST", "127.0.0.1");
    let port: u16 = env_or("MINEINTENT_PORT", "25565")
        .parse()
        .map_err(|error| format!("MINEINTENT_PORT 无效：{error}"))?;
    let username = env_or("MINEINTENT_USERNAME", "companion");
    let endpoint = env_or("MODEL_ENDPOINT", "https://api.deepseek.com/chat/completions");
    let model_name = env_or("MODEL_NAME", "deepseek-chat");
    let memory_path = env_or("MINEINTENT_MEMORY_FILE", "companion-memory.md");
    let persona = match std::env::var("MINEINTENT_PERSONA_FILE") {
        Ok(path) => std::fs::read_to_string(&path)
            .map_err(|error| format!("读取人设文件失败（{path}）：{error}"))?,
        Err(_) => PLACEHOLDER_PERSONA.to_owned(),
    };
    let api_key = read_api_key()?;

    // ---- 接入世界 ----
    println!("[组合根] 连接 {host}:{port}，用户名 {username}");
    let module = Arc::new(
        Module::start(ConnectionConfig {
            host,
            port,
            username: username.clone(),
        })
        .map_err(|error| format!("接入模块启动失败：{error}"))?,
    );
    module
        .wait_ready(Duration::from_secs(60))
        .await
        .map_err(|error| format!("进入世界失败：{error}"))?;
    println!("[组合根] 已进入世界");

    // ---- 中间层装配 ----
    let occupancy = Arc::new(Occupancy::new());
    let read_mark = Arc::new(ChatReadMark::new());
    let memory_file = Arc::new(MemoryFile::new(memory_path));
    let snapshots: Arc<dyn SnapshotSource> = module.clone();

    let providers: Vec<Arc<dyn ToolProvider>> = vec![
        Arc::new(ChatBox::new(
            occupancy.clone(),
            Arc::new(ModuleChatDoor(module.clone())),
            Arc::new(ModuleChatHistory(module.clone())),
            read_mark.clone(),
            snapshots.clone(),
        )),
        Arc::new(MemoryTools::new(memory_file.clone())),
    ];
    let dispatcher =
        Arc::new(Dispatcher::new(providers, occupancy).map_err(|error| error.to_string())?);

    let model = Arc::new(
        HttpModel::new(HttpModelConfig::new(
            endpoint,
            api_key,
            model_name,
            Protocol::openai_chat(),
        ))
        .map_err(|error| format!("模型适配器构造失败：{error}"))?,
    );

    let strategy = Arc::new(
        ContextStrategy::new(persona, memory_file)
            .with_situation(snapshots.clone(), read_mark.clone())
            .with_model(model.clone()),
    );
    let session = Arc::new(AgentSession::new(
        strategy.clone(),
        dispatcher,
        strategy,
        model,
        SessionConfig::default(),
    ));

    // ---- 最小唤醒脚手架：别人对我说话就醒 ----
    // 游标按 tick 前进；同 tick 的迟到消息可能漏（脚手架级简化，己落位时消除）。
    let mut chat_cursor: u64 = snapshots.latest().tick;
    println!("[组合根] 开始倾听聊天（Ctrl+C 停机）");
    loop {
        tokio::select! {
            _ = module.ticked() => {
                let snapshot = snapshots.latest();
                let fresh: Vec<String> = snapshot
                    .chat
                    .entries
                    .iter()
                    .filter(|entry| entry.tick > chat_cursor)
                    .filter_map(|entry| {
                        let sender = entry.sender.as_ref()?;
                        if sender.username == username {
                            return None; // 自己的话不是唤醒理由，防自激。
                        }
                        Some(format!("{}: {}", sender.username, entry.content.plain_text))
                    })
                    .collect();
                chat_cursor = chat_cursor.max(
                    snapshot.chat.entries.iter().map(|entry| entry.tick).max().unwrap_or(chat_cursor),
                );
                if fresh.is_empty() {
                    continue;
                }
                let items: Vec<agent::TranscriptItem> = fresh
                    .into_iter()
                    .map(|line| InputMessage::text("user", line).into())
                    .collect();
                match session.enqueue_if_running(MailboxInput::next_model_request(items)).await {
                    Ok(()) => {}
                    Err(rejected) => {
                        // 没有运行在跑：起一轮。轮在后台驱动，脚手架继续听。
                        let session = session.clone();
                        let items = rejected.input.items;
                        tokio::spawn(async move {
                            if let Err(rejected) = session.start_if_idle(items).await {
                                eprintln!("[组合根] 唤醒被拒：{:?}", rejected.reason);
                            }
                        });
                    }
                }
            }
            result = tokio::signal::ctrl_c() => {
                result.map_err(|error| format!("信号监听失败：{error}"))?;
                println!("[组合根] 收到停机信号");
                break;
            }
        }
    }

    session.stop().await;
    module.stop("维护者停机").await?;
    println!("[组合根] 已停机");
    Ok(())
}
