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
use hand::{HandDoor, HandTools};
use memory::{MemoryFile, MemoryTools};
use motion::{MotionDoor, MotionTools};
use perception::{PerceptionTools, ViewportDoor};
use screens::{ChatBox, ChatDoor, ChatHistory, ChatReadMark, InventoryDoor, InventoryScreen, ScreenKind, ScreenState};
use world::{ConnectionConfig, DoorCommand, Module, SnapshotSource};

mod wake;

use wake::{SelfIdentity, WakeCursors};

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

/// 运动门：动词直译为接入模块的写口命令。
struct ModuleMotionDoor(Arc<Module>);

impl MotionDoor for ModuleMotionDoor {
    fn go_to<'a>(&'a self, target: [f64; 3]) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::GoTo(target)).await })
    }
    fn forward<'a>(&'a self, blocks: f64) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::Forward(blocks)).await })
    }
    fn stop<'a>(&'a self) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::StopMoving).await })
    }
    fn jump<'a>(&'a self) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::Jump).await })
    }
    fn sneak<'a>(&'a self, on: bool) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::Sneak(on)).await })
    }
    fn sprint<'a>(&'a self, on: bool) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::Sprint(on)).await })
    }
    fn look_at<'a>(&'a self, target: [f64; 3]) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::LookAt(target)).await })
    }
    fn face<'a>(&'a self, yaw: f64, pitch: f64) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::Face { yaw, pitch }).await })
    }
}

/// 手门：同上。
struct ModuleHandDoor(Arc<Module>);

impl HandDoor for ModuleHandDoor {
    fn attack<'a>(&'a self, entity_key: &'a str) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.0
                .execute(DoorCommand::Attack {
                    entity_key: entity_key.to_owned(),
                })
                .await
        })
    }
    fn mine<'a>(&'a self, block: [i32; 3]) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::Mine(block)).await })
    }
    fn use_on_block<'a>(&'a self, block: [i32; 3]) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::UseOnBlock(block)).await })
    }
    fn use_on_entity<'a>(
        &'a self,
        entity_key: &'a str,
    ) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.0
                .execute(DoorCommand::UseOnEntity {
                    entity_key: entity_key.to_owned(),
                })
                .await
        })
    }
    fn use_item<'a>(&'a self) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::UseItem).await })
    }
    fn release<'a>(&'a self) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::ReleaseHand).await })
    }
    fn drop_item<'a>(&'a self, whole_stack: bool) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::DropItem { whole_stack }).await })
    }
    fn swap_offhand<'a>(&'a self) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::SwapOffhand).await })
    }
    fn select_slot<'a>(&'a self, slot: u8) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::SelectSlot(slot)).await })
    }
}

/// 物品栏门：交换与丢弃直译为写口命令。
struct ModuleInventoryDoor(Arc<Module>);

impl InventoryDoor for ModuleInventoryDoor {
    fn swap_slots<'a>(&'a self, a: u16, b: u16) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::SwapSlots { a, b }).await })
    }
    fn throw_slot<'a>(&'a self, slot: u16) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::ThrowSlot(slot)).await })
    }
}

/// 视口门：投影是纯 CPU 重活，放阻塞池，不占用异步线程。
struct ModuleViewportDoor(Arc<Module>);

impl ViewportDoor for ModuleViewportDoor {
    fn scan<'a>(&'a self) -> agent::PortFuture<'a, Result<world::ViewportProjection, String>> {
        let module = self.0.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || module.scan(&world::ViewportOptions::default()))
                .await
                .map_err(|error| format!("视口投影任务失败：{error}"))?
        })
    }

    fn scan_directed<'a>(
        &'a self,
        positions: Vec<[i32; 3]>,
    ) -> agent::PortFuture<'a, Result<world::DirectedProjection, String>> {
        let module = self.0.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                module.scan_directed(&positions, &world::ViewportOptions::default())
            })
            .await
            .map_err(|error| format!("视口投影任务失败：{error}"))?
        })
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
    let endpoint = env_or(
        "MODEL_ENDPOINT",
        "https://api.deepseek.com/chat/completions",
    );
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
    let screen_state = Arc::new(ScreenState::new());
    let read_mark = Arc::new(ChatReadMark::new());
    let memory_file = Arc::new(MemoryFile::new(memory_path));
    let snapshots: Arc<dyn SnapshotSource> = module.clone();

    let providers: Vec<Arc<dyn ToolProvider>> = vec![
        Arc::new(ChatBox::new(
            occupancy.clone(),
            screen_state.clone(),
            Arc::new(ModuleChatDoor(module.clone())),
            Arc::new(ModuleChatHistory(module.clone())),
            read_mark.clone(),
            snapshots.clone(),
        )),
        Arc::new(InventoryScreen::new(
            occupancy.clone(),
            screen_state.clone(),
            Arc::new(ModuleInventoryDoor(module.clone())),
            snapshots.clone(),
        )),
        Arc::new(MemoryTools::new(memory_file.clone())),
        Arc::new(MotionTools::new(Arc::new(ModuleMotionDoor(module.clone())))),
        Arc::new(HandTools::new(Arc::new(ModuleHandDoor(module.clone())))),
        Arc::new(PerceptionTools::new(Arc::new(ModuleViewportDoor(
            module.clone(),
        )))),
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

    // ---- 最小唤醒脚手架：别人对我说话、受伤、移动任务有果就醒 ----
    // 判据本身是纯函数，在 `wake` 里，带单测；这里只负责取快照与投递。
    let own_key = snapshots.latest().self_state.entity_key.clone();
    let mut cursors = WakeCursors::resume_from(&snapshots.latest());
    println!("[组合根] 开始倾听聊天与通知（Ctrl+C 停机）");
    loop {
        tokio::select! {
            _ = module.ticked() => {
                let snapshot = snapshots.latest();
                let fresh = cursors.collect(
                    &snapshot,
                    SelfIdentity { entity_key: &own_key, username: &username },
                    // 库存变化只在物品栏开着时投递（维护者裁定）。
                    screen_state.current() == Some(ScreenKind::Inventory),
                );
                if fresh.is_empty() {
                    continue;
                }
                let items: Vec<agent::TranscriptItem> = fresh
                    .into_iter()
                    .map(|line| InputMessage::text("user", line).into())
                    .collect();
                // 新内核的 enqueue 一步完成「并入进行中的轮」或「叫醒空闲会话」，
                // 旧的闲/忙两步竞态窗口已在内核里闭合，不再需要投递重试循环。
                println!("[组合根] 唤醒：{} 条新话", items.len());
                let session = session.clone();
                tokio::spawn(async move {
                    match session
                        .enqueue(MailboxInput::next_model_request(items))
                        .await
                    {
                        Ok(agent::Enqueued::Started(handle)) => {
                            println!("[组合根] 轮结束:{:?}", handle.join().await);
                        }
                        Ok(agent::Enqueued::Pending) => {
                            println!("[组合根] 已并入进行中的轮");
                        }
                        Ok(agent::Enqueued::Held(reason)) => {
                            // 收下了但暂时无人来取（例如需要对账或续跑）；内容留在信箱，
                            // 下一次运行的首个边界会排空。
                            eprintln!("[组合根] 唤醒被搁置：{reason:?}");
                        }
                        Ok(other) => eprintln!("[组合根] 未预期的投递结局：{other:?}"),
                        Err(rejected) => {
                            eprintln!("[组合根] 唤醒被拒：{:?}", rejected.reason);
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
    // 请求挡住整个进程退出。先关自动启动挡住新唤醒，再取消在跑的轮并等它收尾。
    session.set_auto_start(false).await;
    session.cancel_run().await;
    if tokio::time::timeout(Duration::from_secs(30), session.wait_until_idle())
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
