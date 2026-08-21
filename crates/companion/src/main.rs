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
use dispatch::{Dispatcher, LifeGate, Occupancy, ToolProvider};
use hand::{HandDoor, HandTools, MiningStatus};
use memory::{MemoryFile, MemoryTools};
use motion::{MotionDoor, MotionTools};
use perception::{PerceptionTools, ViewportDoor};
use presence::{PresenceDoor, PresenceTools};
use screens::{
    ChatBox, ChatDoor, ChatHistory, ChatReadMark, ContainerScreen, InventoryDoor, InventoryScreen,
    ScreenKind, ScreenState,
};
use world::{ConnectionConfig, DoorCommand, Module, SnapshotSource};

mod situation;
mod wake;

use situation::SituationTracker;
use wake::{ScreenDirective, SelfIdentity, WakeCursors};

/// 人设占位（Q01 未裁；正式文本由维护者给出后替换）。
///
/// 末尾那句「开发中，欢迎反馈」是**开发阶段专用**，正式上线前删掉。它存在的理由：
/// 实机测试的三种信息来源里，「问模型原因」是唯一能解释「为什么它那样做」的一种，
/// 而它此前只能靠我们事后翻转录去猜。让它自己说「这里反直觉」，比我们猜准得多。
const PLACEHOLDER_PERSONA: &str = "\
你是这个 Minecraft 世界里的一位同伴，说中文。\
重要：你直接写出的文字只是内心独白，世界里没有任何人能看到——写\"我告诉了他\"\
并不会真的告诉任何人。要开口，必须调用工具 chat_box，例如\
{\"action\":\"say\",\"text\":\"你好\"}；不调用它就等于保持沉默。\
想记住什么就用 remember 改写你的记忆。别人对你说的话会传到你这里；\
真的想安静时，不调用任何工具即可。\
另外：这套身体和工具还在开发中。哪里用起来别扭、和你的直觉相反、\
回执说的和实际发生的对不上，或者你觉得换个做法会更顺手——都直接讲出来。\
维护者读得到你的内心独白，这类反馈比我们自己猜有用得多。";

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
    fn mine<'a>(&'a self, blocks: Vec<[i32; 3]>) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::Mine(blocks)).await })
    }
    fn mining_status<'a>(&'a self) -> agent::PortFuture<'a, Option<MiningStatus>> {
        Box::pin(async move {
            self.0
                .mining_status()
                .map(|(done, total, current)| MiningStatus {
                    done,
                    total,
                    current,
                })
        })
    }
    fn place<'a>(&'a self, block: [i32; 3]) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::PlaceBlock(block)).await })
    }

    fn pillar_up<'a>(&'a self, count: usize) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::PillarUp { count }).await })
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

/// 生死去留门：当前只有复活。
struct ModulePresenceDoor(Arc<Module>);

impl PresenceDoor for ModulePresenceDoor {
    fn respawn<'a>(&'a self) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::Respawn).await })
    }
}

/// 生命闸门的判据来源：快照里的 `alive`。
///
/// dispatch 不认识 world，所以这条窄化在组合根落地。取的是**最新快照**而
/// 不是缓存的标志——工具执行与 tick 采样是两条线，缓存会让「刚死就还能挥手」
/// 这类窗口打开。
struct SnapshotLifeGate(Arc<dyn SnapshotSource>);

impl LifeGate for SnapshotLifeGate {
    fn alive(&self) -> bool {
        self.0.latest().self_state.alive
    }
}

/// 物品栏门：交换与丢弃直译为写口命令。
struct ModuleInventoryDoor(Arc<Module>);

impl InventoryDoor for ModuleInventoryDoor {
    fn move_slots<'a>(
        &'a self,
        from: String,
        to: String,
        count: Option<u32>,
    ) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.0
                .execute(DoorCommand::MoveSlots { from, to, count })
                .await
        })
    }
    fn throw_slot<'a>(&'a self, slot: String) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::ThrowSlot(slot)).await })
    }
    fn close_container<'a>(&'a self) -> agent::PortFuture<'a, Result<(), String>> {
        Box::pin(async move { self.0.execute(DoorCommand::CloseContainer).await })
    }
}

/// 视口门：投影是纯 CPU 重活，放阻塞池，不占用异步线程。
struct ModuleViewportDoor {
    module: Arc<Module>,
    /// 增量模式的对比基线：与 perception 吸收共用同一本方块记忆。
    block_memory: Arc<std::sync::Mutex<world::BlockMemory>>,
}

impl ViewportDoor for ModuleViewportDoor {
    fn scan<'a>(
        &'a self,
        options: world::ViewportOptions,
    ) -> agent::PortFuture<'a, Result<world::ViewportProjection, String>> {
        let module = self.module.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || module.scan(&options))
                .await
                .map_err(|error| format!("视口投影任务失败：{error}"))?
        })
    }

    fn scan_directed<'a>(
        &'a self,
        positions: Vec<[i32; 3]>,
    ) -> agent::PortFuture<'a, Result<world::DirectedProjection, String>> {
        let module = self.module.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                module.scan_directed(&positions, &world::ViewportOptions::default())
            })
            .await
            .map_err(|error| format!("视口投影任务失败：{error}"))?
        })
    }

    fn scan_changes<'a>(
        &'a self,
    ) -> agent::PortFuture<'a, Result<Vec<world::BlockChange>, String>> {
        let module = self.module.clone();
        let memory = self.block_memory.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                module.scan_changes(&memory, &world::ViewportOptions::default())
            })
            .await
            .map_err(|error| format!("视口投影任务失败：{error}"))?
        })
    }
}

/// 诊断轨迹：把每一轮模型说了什么、调了哪些工具、工具回了什么写进一个文件。
///
/// 只在 `MINEINTENT_TRACE_FILE` 给了路径时装配。默认不开的理由与内核 wire 日志
/// 同款：内容**未经脱敏**（聊天原文都在里面），落盘或外传前要自己看一眼。
///
/// 不记 `ModelRequestTranscript`——那是每次请求的整份上下文，量级完全不同，
/// 要看那个另说。这里只回答「它做了什么」。
/// 压缩线设在**服务商上下文窗口的 95%**。
///
/// 压缩本身当前是空实现（`context::ContextStrategy` 的 `Compaction`），所以这条线现在
/// 只是一个观察点：越过它什么也不会发生，上下文继续长，最终由服务商的长度限制兜底。
/// 这是刻意的——先让「到底能撑多久」变成可观测的事实，再谈压缩该长什么样。
///
/// 用 `compact_above_tokens` 而不是字节：它读的是**服务商上报的输入 token**，比字节
/// 估算准得多（实测约 3.6 字节/token，估算误差足以差出一整轮）。只有发过至少一次
/// 请求、且服务商报了 usage 才有值，没有时内核回退到字节估算——所以字节线也留着，
/// 当第一次请求之前的兜底。
///
/// ⚠ `MINEINTENT_MODEL_CONTEXT_TOKENS` 是**唯一需要跟着模型手工改的数**：服务商的
/// `/models` 不给这个值（DeepSeek 实测只返回 `id` / `object` / `owned_by`）。启动时
/// 打印出来，好当场看出配错。
///
/// # 换模型时去哪查
///
/// **先看文档**：<https://api-docs.deepseek.com/quick_start/pricing> 直接列了每个模型的
/// Context Length 与 Max Output。文档没写时才用报错法——服务商在越界时会说出这个数，
/// 两步都不计费：`max_tokens: 99999999` 问出输出上限，超量 messages 问出上下文窗口。
///
/// 2026-08-19 查证（文档与报错互相印证）：
///
/// - `MODEL_NAME=deepseek-chat` 实际解析到 **`deepseek-v4-flash`**（回包的 `model` 字段
///   自己说的）——配置里写的名字和真正在跑的模型不是同一个；
/// - 窗口 **1 048 576**（2^20），输出上限 **393 216**；
/// - 输入命中 **$0.007/M**、未命中 **$0.22/M**——**31 倍**。这个比值才是前缀缓存那笔账
///   的真实分量：省下的不是 token 数，是 token 数乘以 31。
fn session_config() -> SessionConfig {
    fn env_number(key: &str, fallback: u64) -> u64 {
        std::env::var(key)
            .ok()
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(fallback)
    }

    // 实测 3.6 字节/token（30 分钟长跑的转录）。取 4 是**故意往高了取**：字节线只在
    // 第一次请求之前有效，宁可晚一点触发，也不要抢在 token 线前面把它架空。
    const BYTES_PER_TOKEN: u64 = 4;

    let window = env_number("MINEINTENT_MODEL_CONTEXT_TOKENS", 1_048_576);
    let tokens = window * 95 / 100;
    let mut config = SessionConfig::default();
    config.budget.context.compact_above_tokens = Some(tokens);
    // 字节线跟着 token 线算，不单独配：两条线各配各的必然会错位——
    // 先前 token 线 996 147、字节线 1 MiB（≈29 万 token），字节线永远先撞上，
    // token 线等于不存在。
    config.budget.context.compact_above_bytes =
        env_number("MINEINTENT_COMPACT_ABOVE_BYTES", tokens * BYTES_PER_TOKEN) as usize;
    println!(
        "[组合根] 压缩线：输入 token 超过 {tokens} 时压缩（窗口 {window} 的 95%）；\
         首次请求前按字节兜底 {} MiB。压缩当前是空实现，越线只是观察点。",
        config.budget.context.compact_above_bytes / (1024 * 1024)
    );
    config
}

/// 把事件分发给多个观察者。内核只留一个观察者槽位，而组合根有两件事要看。
struct FanOut(Vec<Arc<dyn agent::Observer>>);

impl agent::Observer for FanOut {
    fn observe(&self, event: &agent::AgentEvent) {
        for observer in &self.0 {
            observer.observe(event);
        }
    }
}

/// 只看一件事：压缩完成了没有。
struct CompactionFlag(Arc<std::sync::atomic::AtomicBool>);

impl agent::Observer for CompactionFlag {
    fn observe(&self, event: &agent::AgentEvent) {
        if matches!(event, agent::AgentEvent::CompactionFinished { .. }) {
            self.0.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

struct TraceObserver(std::sync::Mutex<std::fs::File>);

impl TraceObserver {
    fn open(path: &str) -> Result<Self, String> {
        std::fs::File::create(path)
            .map(|file| Self(std::sync::Mutex::new(file)))
            .map_err(|error| format!("诊断轨迹文件打不开（{path}）：{error}"))
    }

    fn write(&self, line: &str) {
        use std::io::Write;
        // 诊断出口失败不该拖垮同伴：写不进去就算了，别 panic 进观察端旁路。
        if let Ok(mut file) = self.0.lock() {
            let _ = writeln!(file, "{line}");
            let _ = file.flush();
        }
    }
}

/// 每次模型请求一行：这一份上下文有多大、命中了多少、花了多久。
/// 每一轮的起止各一行：它怎么开始的、怎么结束的。
///
/// 轮级的 `ModelUsage` 是**累加**的（midturn `types.rs` 的 merge），回答不了
/// 「单次请求的上下文多大」——那正是评估上下文时唯一要看的数。逐请求的
/// `ModelRequestFinished` 才带真实数字。
///
/// **轮的结局此前一条都没记。** 2026-08-20 那次永久停机之所以只能靠「投影统计
/// 还在滚」来判断进程没死，就是因为这里只接了两个请求级事件；内核明明发着
/// `RunStarted` / `RunCompleted` / `RunStopped` / `RunFailed`（`RunFailed` 还是
/// `EventLevel::Error`），我们一个都没听。
///
/// 补它不改任何行为——观察端是旁路。这一点很要紧：僵尸轮的成因未明，维护者裁
/// 「不动」，而**多记一笔不是动**，少记一笔才是把证据丢了。
impl agent::Observer for TraceObserver {
    fn observe(&self, event: &agent::AgentEvent) {
        match event {
            agent::AgentEvent::ModelRequestStarted {
                request_index,
                transcript_items,
                function_tools,
                ..
            } => self.write(&format!(
                "[请求#{request_index}] 转录条目={transcript_items} 工具={function_tools}"
            )),
            agent::AgentEvent::ModelRequestFinished {
                request_index,
                duration_ms,
                usage,
                ..
            } => {
                let (input, cached, output) = usage
                    .as_ref()
                    .map(|u| {
                        (
                            u.input_tokens.unwrap_or(0),
                            u.cached_input_tokens.unwrap_or(0),
                            u.output_tokens.unwrap_or(0),
                        )
                    })
                    .unwrap_or((0, 0, 0));
                self.write(&format!(
                    "[用量#{request_index}] 输入={input} 命中={cached} 未命中={} 输出={output} 耗时={duration_ms}ms",
                    input.saturating_sub(cached)
                ));
            }
            agent::AgentEvent::RunStarted {
                run_id,
                prior_transcript_items,
                ..
            } => self.write(&format!(
                "[轮开始] id={run_id:?} 起始转录条目={prior_transcript_items}"
            )),
            agent::AgentEvent::RunCompleted {
                run_id,
                model_requests,
                tool_batches,
                ..
            } => self.write(&format!(
                "[轮结束] id={run_id:?} 结局=完成 请求数={model_requests} 工具批={tool_batches}"
            )),
            agent::AgentEvent::RunStopped { run_id, .. } => {
                self.write(&format!("[轮结束] id={run_id:?} 结局=被停止"))
            }
            agent::AgentEvent::RunFailed {
                run_id,
                stage,
                error_kind,
                ..
            } => self.write(&format!(
                "[轮结束] id={run_id:?} 结局=失败 阶段={stage:?} 错误类别={error_kind:?}"
            )),
            _ => {}
        }
    }
}

impl agent::ContentObserver for TraceObserver {
    fn observe(&self, event: &agent::ContentEvent) {
        match event {
            agent::ContentEvent::ModelResponseOutput { output, .. } => {
                for part in &output.content {
                    if let agent::ContentPart::Text { text } = part {
                        self.write(&format!("[说] {text}"));
                    }
                }
                for call in &output.tool_calls {
                    self.write(&format!("[调用] {} {}", call.name.as_str(), call.arguments));
                }
            }
            agent::ContentEvent::ToolBatchResults { results, .. } => {
                for result in &results.results {
                    let body: String = result
                        .content
                        .iter()
                        .map(|part| match part {
                            agent::ContentPart::Text { text } => text.clone(),
                            agent::ContentPart::Json { value } => value.to_string(),
                            other => format!("{other:?}"),
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    self.write(&format!("[回执/{:?}] {body}", result.status));
                }
            }
            _ => {}
        }
    }
}

/// 眼睛的自适应节律。
///
/// 一次投影就是一整幅视口——纯 CPU 重活。它现在**只为记忆服务**（方块不进会话区），
/// 但快慢仍然要按实测让路：拍一个常数在密林里会把 tick 处理拖垮，在旷野里又白等。
///
/// 一次增量就是一整幅视口投影——纯 CPU 重活，耗时随视野里方块多少浮动，
/// 拍一个常数（比如「每 5 tick」）在密林里会把 tick 处理拖垮，在旷野里又
/// 白等。所以按**实测**来：每次投影计时，取近几次均值，下一次间隔 = 均值 ×
/// 倍率，再夹到 [最短, 最长] 之间。
///
/// 倍率的含义是「投影占用的时间份额」：×4 即最多花 1/5 的时间在投影上，
/// 剩下留给 tick 处理与模型往返。
///
/// 三个数都能用环境变量调，好在实盘里对着日志找合适值：
/// `MINEINTENT_FRAME_MIN_MS`（默认 250）、`MINEINTENT_FRAME_MAX_MS`（默认 5000）、
/// `MINEINTENT_FRAME_FACTOR`（默认 4）。
struct FramePace {
    recent: std::collections::VecDeque<Duration>,
    min: Duration,
    max: Duration,
    factor: u32,
    /// 全程统计（含空 diff 的那些）。滑窗管节律，这几个管「到底多久」。
    total: u64,
    /// 投影本身的累计（不含排队）。
    sum: Duration,
    /// 含排队的累计。两者之差就是调度开销。
    round_sum: Duration,
    fastest: Duration,
    slowest: Duration,
}

impl FramePace {
    /// 均值取最近这么多次：够平滑，又能跟上场景切换（进洞、出林）。
    const WINDOW: usize = 8;

    fn new() -> Self {
        let ms = |name: &str, fallback: u64| -> u64 {
            std::env::var(name)
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(fallback)
        };
        Self {
            recent: std::collections::VecDeque::with_capacity(Self::WINDOW),
            min: Duration::from_millis(ms("MINEINTENT_FRAME_MIN_MS", 250)),
            max: Duration::from_millis(ms("MINEINTENT_FRAME_MAX_MS", 5_000)),
            factor: ms("MINEINTENT_FRAME_FACTOR", 4) as u32,
            total: 0,
            sum: Duration::ZERO,
            round_sum: Duration::ZERO,
            fastest: Duration::MAX,
            slowest: Duration::ZERO,
        }
    }

    /// `work` 是投影本身，`round` 含排队。节律按 round 退让（忙就让路），
    /// 分布报 work（那才是算法的成本）。
    fn record(&mut self, work: Duration, round: Duration) {
        if self.recent.len() == Self::WINDOW {
            self.recent.pop_front();
        }
        self.recent.push_back(round);
        self.total += 1;
        self.sum += work;
        self.round_sum += round;
        self.slowest = self.slowest.max(work);
        self.fastest = self.fastest.min(work);
    }

    /// 每这么多次投影汇报一次分布。**空 diff 的投影不投递也不打帧日志**，
    /// 只按非空帧统计会漏掉「无事发生时多久」那一半——那正是常态。
    const REPORT_EVERY: u64 = 40;

    fn due_report(&self) -> Option<String> {
        if self.total == 0 || !self.total.is_multiple_of(Self::REPORT_EVERY) {
            return None;
        }
        let n = self.total as u32;
        Some(format!(
            "[组合根] 投影分布：{} 次，最快 {}ms，均值 {}ms，最慢 {}ms；含排队均值 {}ms（排队开销 {}ms）",
            self.total,
            self.fastest.as_millis(),
            (self.sum / n).as_millis(),
            self.slowest.as_millis(),
            (self.round_sum / n).as_millis(),
            ((self.round_sum - self.sum) / n).as_millis()
        ))
    }

    fn average(&self) -> Duration {
        if self.recent.is_empty() {
            return Duration::ZERO;
        }
        self.recent.iter().sum::<Duration>() / self.recent.len() as u32
    }

    /// 下次定时触发的间隔。还没测过时先用最短间隔起步。
    fn next_interval(&self) -> Duration {
        (self.average() * self.factor).clamp(self.min, self.max)
    }
}

/// 眼睛的第二个触发源：一轮模型响应落定（AttemptCommitted）时发信号；
/// 收集与投递在旁路任务做——观察端契约要求快速返回。
struct RoundEndSignal(tokio::sync::mpsc::UnboundedSender<()>);

impl agent::StreamObserver for RoundEndSignal {
    fn observe(&self, event: &agent::ObservedModelStreamEvent) {
        if matches!(
            event.payload,
            agent::ModelStreamObservation::AttemptCommitted
        ) {
            let _ = self.0.send(());
        }
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
    // 方块记忆：同伴「已知道什么」的共享认知状态。当前由 scan 回执喂入；
    // 增量呈现与寻路合法域随后也读写这一本。
    let block_memory = Arc::new(std::sync::Mutex::new(world::BlockMemory::new()));
    // 合法寻路：寻路只按这本记忆里观察过的方块规划，不再读服务端推来的全量世界。
    // 必须是同一本——眼睛每 250ms 把看见的写进去，寻路要读的正是那一份。
    module.use_observed_pathfinding(block_memory.clone());
    println!("[组合根] 合法寻路：只按观察过的方块规划路线");
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
        Arc::new(ContainerScreen::new(
            occupancy.clone(),
            screen_state.clone(),
            Arc::new(ModuleInventoryDoor(module.clone())),
            snapshots.clone(),
        )),
        Arc::new(MemoryTools::new(memory_file.clone())),
        Arc::new(MotionTools::new(Arc::new(ModuleMotionDoor(module.clone())))),
        Arc::new(HandTools::new(Arc::new(ModuleHandDoor(module.clone())))),
        Arc::new(PerceptionTools::new(
            Arc::new(ModuleViewportDoor {
                module: module.clone(),
                block_memory: block_memory.clone(),
            }),
            block_memory.clone(),
            snapshots.clone(),
        )),
        Arc::new(PresenceTools::new(Arc::new(ModulePresenceDoor(
            module.clone(),
        )))),
    ];
    let life: Arc<dyn LifeGate> = Arc::new(SnapshotLifeGate(snapshots.clone()));
    let dispatcher = Arc::new(
        Dispatcher::new(providers, occupancy.clone(), life).map_err(|error| error.to_string())?,
    );
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

    // 前缀只有人设与记忆——两者都不逐轮变，吃满前缀缓存。处境不在这里：
    // 它每轮都变，随帧追加在对话末尾（见 `situation` 模块的账）。
    let strategy = Arc::new(ContextStrategy::new(persona, memory_file));
    // 眼睛：持续把**合法可见**的方块写进记忆。
    //
    // 这个循环以前叫「轮末帧」，任务是把方块差异送给模型。方块不再进会话区之后
    // （裁定五），它的身份变了——**它是眼睛，不是帧**：视口判据（视锥 + 遮挡 +
    // ExposedFace）决定什么算看过，看过的自动进 BlockMemory，模型既不花轮次、也
    // 看不见这个过程。就像人不「决定去看」，睁着眼东西就自己进了记忆。
    //
    // 视口因此仍然不可少，但角色是**合法性守门人**而非呈现：直接吸收服务端推来的
    // 已加载区块会让记忆退化成「服务端给了什么」，今天刚从寻路里赶出去的开挂就会
    // 从记忆的后门回来。
    //
    // 顺带还投递两样**非方块**的东西：处境变了的那几行、在途 job 的进展。它们便宜，
    // 且只在真有内容时才发。
    let (round_end_tx, mut round_end_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let mut assembled = AgentSession::new(
        strategy.clone(),
        dispatcher,
        strategy,
        model,
        session_config(),
    )
    .with_stream_observer(Arc::new(RoundEndSignal(round_end_tx)));
    // 压缩完成的旗子：压缩把对话换成摘要，先前追加的处境随之消失，
    // 下一帧要把处境从头说一遍。
    let compacted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut observers: Vec<Arc<dyn agent::Observer>> =
        vec![Arc::new(CompactionFlag(compacted.clone()))];
    if let Ok(path) = std::env::var("MINEINTENT_TRACE_FILE") {
        let trace = Arc::new(TraceObserver::open(&path)?);
        assembled = assembled.with_content_observer(trace.clone());
        observers.push(trace);
        println!("[组合根] 诊断轨迹：{path}（内容未脱敏）");
    }
    // 内核的观察者是单槽（装第二个会顶掉第一个），所以这里自己分发。
    assembled = assembled.with_observer(Arc::new(FanOut(observers)));
    let session = Arc::new(assembled);
    {
        let session = session.clone();
        let module = module.clone();
        let block_memory = block_memory.clone();
        let snapshots = snapshots.clone();
        let read_mark = read_mark.clone();
        let compacted = compacted.clone();
        tokio::spawn(async move {
            let mut pace = FramePace::new();
            let mut situation = SituationTracker::new();
            // job 进展的游标，与 wake 那条各走各的：进展是「还在走」，不该叫醒，
            // 由帧搭车呈现；终局才是事件，走 wake。
            let mut progress_seq: Option<u64> = None;
            // 拾取游标与攒下的句子。**每帧都排空，投递时才吐**——不排空的话，
            // 窗只有 64 条，挖得快就会在两次投递之间被挤掉。
            let mut pickup_seq: Option<u64> = None;
            let mut pickup_lines: Vec<String> = Vec::new();
            loop {
                // 两个触发源，谁先到算谁：
                //   一、模型响应落定（工具刚跑完，世界多半刚变）；
                //   二、定时——**间隔由上几次实测耗时自适应**，不是拍脑袋的常数。
                // 世界不变就没有 diff，也就不投递、不唤醒；固定心跳会为无事发生
                // 烧轮，这里不会。
                tokio::select! {
                    signal = round_end_rx.recv() => {
                        if signal.is_none() {
                            break;
                        }
                        // 合并积压：连续几轮落定只收集一次，diff 是累积的不丢事。
                        while round_end_rx.try_recv().is_ok() {}
                    }
                    _ = tokio::time::sleep(pace.next_interval()) => {}
                }

                let scan_module = module.clone();
                let scan_memory = block_memory.clone();
                // 两个时长，别混：
                //   work  —— 投影本身（闭包内计时）。
                //   round —— 派发 + 在阻塞池排队 + 执行 + join。节律按它退让。
                // 分开后实测排队开销 ≈0ms（1000 次采样，两者均值同为 26ms）；两个数
                // 留着，是因为阻塞池一旦真忙起来它们会分开，而节律要跟着退。
                let dispatched = std::time::Instant::now();
                let outcome = tokio::task::spawn_blocking(move || {
                    let at = std::time::Instant::now();
                    // 全量吸收，不算差异——差异没有消费者了（见 Module::absorb）。
                    // 参数用 for_memory：判据不动，只把「一次记多少」的呈现预算放开。
                    let absorbed =
                        scan_module.absorb(&scan_memory, &world::ViewportOptions::for_memory());
                    (absorbed, at.elapsed())
                })
                .await;
                let round = dispatched.elapsed();
                let (absorbed, work) = match outcome {
                    Ok((absorbed, work)) => (absorbed, work),
                    Err(_) => (Ok(0), std::time::Duration::ZERO),
                };
                pace.record(work, round);
                if let Some(report) = pace.due_report() {
                    println!("{report}");
                }

                // 未连接/世界未就绪等如实拒绝：静默跳过，不是错误。
                let Ok(absorbed) = absorbed else { continue };
                // 压缩旗子先收：`request_full_resend` 只置位，真正重投在下面的
                // `take` 里发生——本轮没进展而提前返回，位也留着不会丢。
                if compacted.swap(false, std::sync::atomic::Ordering::Relaxed) {
                    // 摘要按指令不含世界状态，先前追加的处境也随对话一起没了。
                    situation.request_full_resend();
                }
                // 在途 job 的进展：多段行走每一程的终点、挖掘每碎一块。
                //
                // **只有它触发投递。**处境里的位置走路时每帧都变，让它自己触发就等于
                // 每 250ms 投一帧——2026-08-20 实测：1,646 次投递对 294 次请求，其中
                // 1,323 次是单行的位置变化；模型忙着时它们全堆在信箱里，一次并进
                // 855 条、+98k token。
                let snapshot = snapshots.latest();

                // 拾取：世界事件，不受开屏与否管（裁定 2026-08-21）。
                //
                // **它搭车，不触发。** 挖矿时在途 job 的进展本来就在触发投递，
                // 自己捡的东西顺势就出去了；空闲时被人塞了东西则要等下一次
                // 投递——那属于「空闲唤醒源」，`wake-criterion-decision.md`
                // §9.4 未裁，这里不替它裁。
                {
                    let fresh: Vec<world::PickupEntry> = snapshot
                        .pickups
                        .entries
                        .iter()
                        .filter(|entry| !pickup_seq.is_some_and(|seen| entry.seq <= seen))
                        .cloned()
                        .collect();
                    if let Some(last) = fresh.last() {
                        pickup_seq = Some(last.seq);
                    }
                    pickup_lines.extend(render::render_pickups(&fresh));
                    // 攒太多就只留最近的：真到这一步说明久没投递，
                    // 旧的拾取对「我现在有什么」已经由快捷栏那行代答了。
                    const PICKUP_BACKLOG: usize = 32;
                    if pickup_lines.len() > PICKUP_BACKLOG {
                        let drop = pickup_lines.len() - PICKUP_BACKLOG;
                        pickup_lines.drain(..drop);
                    }
                }

                let mut progress = Vec::new();
                for entry in &snapshot.jobs.entries {
                    if progress_seq.is_some_and(|seen| entry.seq <= seen) {
                        continue;
                    }
                    progress_seq = Some(entry.seq);
                    if matches!(entry.event, world::JobEvent::Progress(_)) {
                        progress.push(render::render_job_entry(entry));
                    }
                }
                if progress.is_empty() {
                    continue;
                }

                // 处境搭这趟车。**取它必须在决定投递之后**——`take` 会立刻推进比对
                // 基准，先取后放弃就等于把那次差异吞掉，模型再也听不到。
                let mut sections = situation.take(render::render_situation_lines(
                    &snapshot,
                    read_mark.position(),
                ));
                sections.append(&mut pickup_lines);
                sections.extend(progress);
                // **方块信息不进会话区**（维护者裁定，2026-08-20）：diff 照常算、照常
                // 推进记忆——那是给机器用的（寻路读它，`blocks` 工具查它）——但一格
                // 都不推给模型。
                //
                // 账：3 分钟 164 帧、4,876 条差异，输入峰值 111,617；按 10.5 token/格
                // 折算，半个 1M 窗口只装得下约 4.8 万格，而站着转两分钟就攒 7,040 格。
                // 逐格进上下文在数学上就走不通，不是优化得好不好的问题。
                //
                // 模型要方块就自己 `scan`（睁眼，填记忆）再查记忆库——就像我改代码时
                // 从不加载整个仓库，而是 grep 到行号再读那几行。
                let text = sections.join("\n");
                println!(
                    "[组合根] 投递 {} 行（本次看进记忆 {absorbed} 格；投影 {}ms，含排队 {}ms，均值 {}ms，下次间隔 {}ms）",
                    sections.len(),
                    work.as_millis(),
                    round.as_millis(),
                    pace.average().as_millis(),
                    pace.next_interval().as_millis()
                );
                let item: agent::TranscriptItem = InputMessage::text("user", text).into();
                // WhenIdle 而非 Passive：世界真的变了就值得叫醒空闲的同伴——
                // 挖穿、别人动土、熔炉灭火都在这条通道上。忙时它排在轮末，
                // 天然与进行中的轮合并，不插队。
                if let Err(rejected) = session.enqueue(MailboxInput::when_idle(vec![item])).await {
                    eprintln!("[组合根] 投递被拒：{:?}", rejected.reason);
                }
            }
        });
    }

    // ---- 最小唤醒脚手架：别人对我说话、受伤、移动任务有果就醒 ----
    // 判据本身是纯函数，在 `wake` 里，带单测；这里只负责取快照与投递。
    let own_key = snapshots.latest().self_state.entity_key.clone();
    let mut cursors = WakeCursors::resume_from(&snapshots.latest());
    println!("[组合根] 开始倾听聊天与通知（Ctrl+C 停机）");
    loop {
        tokio::select! {
            _ = module.ticked() => {
                let snapshot = snapshots.latest();
                let wake = cursors.collect(
                    &snapshot,
                    SelfIdentity { entity_key: &own_key, username: &username },
                    // 格位变化只在格位类屏（物品栏/工作台）开着时投递（维护者裁定）。
                    matches!(
                        screen_state.current(),
                        Some(ScreenKind::Inventory | ScreenKind::Container)
                    ),
                );
                if wake.is_empty() {
                    continue;
                }
                let mut lines = wake.lines;
                // 屏事实的副作用：状态翻转 + 占域随服务端真相走。
                // 所有服务端容器共用一件 container 工具；种类差异只在
                // 通知携带的清单段表与用法补充（数据，不是分支）。
                for directive in wake.screens {
                    match directive {
                        ScreenDirective::Opened { kind } => {
                            let displaced = screen_state.server_open(ScreenKind::Container);
                            occupancy.occupy(dispatch::Domain::Screen);
                            // 措辞装配归 render（与 model_surface 导出共用一份）；
                            // 这里只做副作用：占域与屏状态翻转。
                            lines.push(render::render_container_opened(
                                &snapshot,
                                &kind,
                                displaced == Some(ScreenKind::Chat),
                            ));
                        }
                        ScreenDirective::Closed { kind, commanded } => {
                            screen_state.server_close(ScreenKind::Container);
                            if screen_state.current().is_none() {
                                occupancy.release(dispatch::Domain::Screen);
                            }
                            if !commanded {
                                lines.push(format!("容器界面被关闭了（{kind}）。"));
                            }
                        }
                    }
                }
                if lines.is_empty() {
                    // 只有副作用（如 close 回声）没有要说的话：不吵模型。
                    continue;
                }
                let items: Vec<agent::TranscriptItem> = lines
                    .into_iter()
                    .map(|line| InputMessage::text("user", line).into())
                    .collect();
                // 新内核的 enqueue 一步完成「并入进行中的轮」或「叫醒空闲会话」，
                // 旧的闲/忙两步竞态窗口已在内核里闭合，不再需要投递重试循环。
                println!("[组合根] 唤醒：{} 条新话", items.len());
                for item in &items {
                    if let agent::TranscriptItem::Input(message) = item {
                        for part in &message.content {
                            if let agent::ContentPart::Text { text } = part {
                                // 冒烟观测：投递原文（单行截断，长清单只看开头）。
                                let head: String = text.chars().take(120).collect();
                                println!("[组合根]   → {}", head.replace('\n', "⏎"));
                            }
                        }
                    }
                }
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
