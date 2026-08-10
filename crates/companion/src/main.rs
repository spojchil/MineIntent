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
你是这个 Minecraft 世界里的一位同伴，说中文。\
重要：你直接写出的文字只是内心独白，世界里没有任何人能看到——写\"我告诉了他\"\
并不会真的告诉任何人。要开口，必须调用工具 chat_box，例如\
{\"action\":\"say\",\"text\":\"你好\"}；不调用它就等于保持沉默。\
想记住什么就用 remember 改写你的记忆。别人对你说的话会传到你这里；\
真的想安静时，不调用任何工具即可。";

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
    {
        use agent::ToolRuntime;
        let names: Vec<String> = dispatcher
            .definitions()
            .into_iter()
            .map(|definition| definition.name.as_str().to_owned())
            .collect();
        println!("[组合根] 工具表：{names:?}");
    }

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
    // 游标用聊天 seq（单调、同 tick 多条也不漏）；启动前的旧聊天不消费。
    let own_key = snapshots.latest().self_state.entity_key.clone();
    let mut chat_cursor: Option<u64> = snapshots.latest().chat.entries.last().map(|entry| entry.seq);
    println!("[组合根] 开始倾听聊天（Ctrl+C 停机）");
    loop {
        tokio::select! {
            _ = module.ticked() => {
                let snapshot = snapshots.latest();
                let mut fresh = Vec::new();
                for entry in &snapshot.chat.entries {
                    if chat_cursor.is_some_and(|seen| entry.seq <= seen) {
                        continue;
                    }
                    chat_cursor = Some(entry.seq);
                    let Some(sender) = entry.sender.as_ref() else { continue };
                    // 防自激：优先比对稳定 UUID（自身 entity_key 即 UUID），
                    // 服务器不给 UUID 时退回用户名比较。
                    let is_self = match &sender.uuid {
                        Some(uuid) => *uuid == own_key,
                        None => sender.username == username,
                    };
                    if is_self {
                        continue;
                    }
                    fresh.push(format!("{}: {}", sender.username, entry.content.plain_text));
                }
                if fresh.is_empty() {
                    continue;
                }
                let items: Vec<agent::TranscriptItem> = fresh
                    .into_iter()
                    .map(|line| InputMessage::text("user", line).into())
                    .collect();
                // 投递到接受为止：闲/忙状态在投递间隙可能翻转（起轮竞态、
                // 轮刚收尾），单次尝试会把消息丢在地上。
                println!("[组合根] 唤醒：{} 条新话", items.len());
                let session = session.clone();
                tokio::spawn(async move {
                    let mut items = items;
                    loop {
                        match session
                            .enqueue_if_running(MailboxInput::next_model_request(items))
                            .await
                        {
                            Ok(()) => {
                                println!("[组合根] 已并入进行中的轮");
                                return;
                            }
                            Err(rejected) => items = rejected.input.items,
                        }
                        match session.start_if_idle(items).await {
                            Ok(outcome) => {
                                println!("[组合根] 轮结束:{outcome:?}");
                                return;
                            }
                            Err(rejected) => {
                                items = rejected.initial_items;
                                match rejected.reason {
                                    agent::StartRejectedReason::Busy => {
                                        // 另一轮刚接手：回到 enqueue 路径重试。
                                        tokio::task::yield_now().await;
                                    }
                                    _ => {
                                        eprintln!("[组合根] 唤醒被弃：{:?}", rejected.reason);
                                        return;
                                    }
                                }
                            }
                        }
                    }
                });
            }
            result = tokio::signal::ctrl_c() => {
                result.map_err(|error| format!("信号监听失败：{error}"))?;
                println!("[组合根] 收到停机信号");
                break;
            }
        }
    }

    // 停机有界：会话没在限时内收尾也要走世界停机，不让一次悬挂的模型
    // 请求挡住整个进程退出。
    if tokio::time::timeout(Duration::from_secs(30), session.stop())
        .await
        .is_err()
    {
        eprintln!("[组合根] 会话 30 秒内未收尾，继续停机");
    }
    if let Err(reason) = module.stop("维护者停机").await {
        // 已知上游隐患：azalea 停机路径可能卡死机器线程；进程退出由
        // 操作系统回收，不把它当成组合根自己的失败。
        eprintln!("[组合根] 世界停机未合流（{reason}），交由进程退出回收");
    }
    println!("[组合根] 已停机");
    Ok(())
}
