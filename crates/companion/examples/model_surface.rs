//! 模型可见面导出：把所有会进到模型眼前的文本保真打印成 Markdown。
//!
//! 不手抄——工具定义、基础上下文、措辞样例全部由真实代码路径产出，
//! 与 companion 实际发给模型的内容逐字一致。唯一的例外是人设：
//! 它是 companion/src/main.rs 的私有常量，这里复制了一份（改动时同步）。
//!
//! 用法：`cargo run -p companion --example model_surface > TEMP_模型可见面.md`

use std::sync::Arc;

use agent::{ContentPart, PortFuture, ToolCall, ToolResult};
use dispatch::{Occupancy, ToolClass, ToolProvider};
use memory::{MemoryFile, MemoryTools};
use screens::{
    container_usage, ChatBox, ChatDoor, ChatHistory, ChatReadMark, ContainerScreen, InventoryDoor,
    InventoryScreen, ScreenState,
};
use serde_json::json;
use world::SnapshotSource;

/// 与 companion/src/main.rs 的 PLACEHOLDER_PERSONA 保持一致（Q01 未裁的占位稿）。
const PLACEHOLDER_PERSONA: &str = "\
你是这个 Minecraft 世界里的一位同伴，说中文。\
重要：你直接写出的文字只是内心独白，世界里没有任何人能看到——写\"我告诉了他\"\
并不会真的告诉任何人。要开口，必须调用工具 chat_box，例如\
{\"action\":\"say\",\"text\":\"你好\"}；不调用它就等于保持沉默。\
想记住什么就用 remember 改写你的记忆。别人对你说的话会传到你这里；\
真的想安静时，不调用任何工具即可。";

// ---- 各门的哑实现：只为构造工具定义与调用样例，永不触碰世界 ----

struct NoDoor;

impl ChatDoor for NoDoor {
    fn send_chat<'a>(&'a self, _line: &'a str) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}
impl ChatHistory for NoDoor {
    fn recent(&self, _count: usize) -> Vec<String> {
        vec![
            "alice: 你好".to_owned(),
            "companion: 你好，alice！".to_owned(),
        ]
    }
}
impl InventoryDoor for NoDoor {
    fn move_slots<'a>(
        &'a self,
        _from: u16,
        _to: u16,
        _count: Option<u32>,
    ) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn throw_slot<'a>(&'a self, _slot: u16) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn close_container<'a>(&'a self) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}
impl motion::MotionDoor for NoDoor {
    fn go_to<'a>(&'a self, _t: [f64; 3]) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn forward<'a>(&'a self, _b: f64) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn stop<'a>(&'a self) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn jump<'a>(&'a self) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn sneak<'a>(&'a self, _on: bool) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn sprint<'a>(&'a self, _on: bool) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn look_at<'a>(&'a self, _t: [f64; 3]) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn face<'a>(&'a self, _y: f64, _p: f64) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}
impl hand::HandDoor for NoDoor {
    fn attack<'a>(&'a self, _k: &'a str) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn mine<'a>(&'a self, _b: [i32; 3]) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn use_on_block<'a>(&'a self, _b: [i32; 3]) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn use_on_entity<'a>(&'a self, _k: &'a str) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn use_item<'a>(&'a self) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn release<'a>(&'a self) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn drop_item<'a>(&'a self, _w: bool) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn swap_offhand<'a>(&'a self) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn select_slot<'a>(&'a self, _s: u8) -> PortFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}
impl perception::ViewportDoor for NoDoor {
    fn scan<'a>(
        &'a self,
        _options: world::ViewportOptions,
    ) -> PortFuture<'a, Result<world::ViewportProjection, String>> {
        Box::pin(async {
            Ok(world::ViewportProjection {
                pose: world::ViewportPose {
                    position: [10.5, 72.0, 3.5],
                    yaw_degrees: 90.0,
                    pitch_degrees: 10.0,
                },
                standing_on_block: Some(world::ViewportBlock {
                    name: "grass_block".to_owned(),
                    properties: Default::default(),
                    position: [10, 71, 3],
                }),
                looked_at_block: Some(world::ViewportBlock {
                    name: "oak_log".to_owned(),
                    properties: Default::default(),
                    position: [6, 72, 3],
                }),
                visible_entities: world::VisibleEntitiesResult {
                    items: vec![world::VisibleEntity {
                        entity_type: "sheep".to_owned(),
                        player: None,
                        position: [7.0, 72.0, 5.0],
                    }],
                    truncated: false,
                },
                visible_blocks: world::VisibleBlocksResult {
                    blocks: vec![
                        world::ViewportBlock {
                            name: "oak_log".to_owned(),
                            properties: Default::default(),
                            position: [6, 72, 3],
                        },
                        world::ViewportBlock {
                            name: "oak_log".to_owned(),
                            properties: Default::default(),
                            position: [6, 73, 3],
                        },
                    ],
                    truncated: true,
                },
            })
        })
    }
    fn scan_directed<'a>(
        &'a self,
        _p: Vec<[i32; 3]>,
    ) -> PortFuture<'a, Result<world::DirectedProjection, String>> {
        Box::pin(async {
            Ok(world::DirectedProjection {
                seen: vec![world::DirectedSeenBlock {
                    at: [6, 72, 3],
                    name: "oak_log".to_owned(),
                    properties: Default::default(),
                }],
                unseen: vec![world::DirectedUnseenBlock {
                    at: [0, 60, 0],
                    why: vec![world::DirectedWhy::Occluded],
                    by: Some(world::DirectedOccluder {
                        name: "stone".to_owned(),
                        properties: Default::default(),
                        at: [3, 65, 1],
                    }),
                    distance: None,
                    max: None,
                }],
            })
        })
    }
    fn scan_changes<'a>(&'a self) -> PortFuture<'a, Result<Vec<world::BlockChange>, String>> {
        Box::pin(async {
            Ok(vec![
                world::BlockChange::Appeared {
                    at: [8, 72, 4],
                    fact: world::BlockFact {
                        name: "chest".to_owned(),
                        properties: Default::default(),
                    },
                },
                world::BlockChange::Changed {
                    at: [6, 72, 3],
                    was: world::BlockFact {
                        name: "furnace".to_owned(),
                        properties: std::collections::BTreeMap::from([
                            ("facing".to_owned(), "north".to_owned()),
                            ("lit".to_owned(), "false".to_owned()),
                        ]),
                    },
                    now: world::BlockFact {
                        name: "furnace".to_owned(),
                        properties: std::collections::BTreeMap::from([
                            ("facing".to_owned(), "north".to_owned()),
                            ("lit".to_owned(), "true".to_owned()),
                        ]),
                    },
                },
                world::BlockChange::Vanished {
                    at: [6, 73, 3],
                    was: world::BlockFact {
                        name: "oak_log".to_owned(),
                        properties: Default::default(),
                    },
                },
            ])
        })
    }
}

/// 样例快照：白天主世界、半血、身边有玩家与两只僵尸、背包有几样东西、
/// 聊天窗两条未读。
fn sample_snapshot() -> world::TickSnapshot {
    let mut snap =
        world::TickSnapshot::empty(world::Epoch(1), 2_400, world::ConnectionPhase::Ready);
    snap.world_meta.dimension = "minecraft:overworld".to_owned();
    snap.world_meta.day_time = 3_000;
    snap.world_meta.rain_level = 1.0;
    snap.self_state.entity_key = "self-uuid".to_owned();
    snap.self_state.username = "companion".to_owned();
    snap.self_state.position = world::Vec3Value {
        x: 10.5,
        y: 72.0,
        z: 3.5,
    };
    snap.self_state.yaw = -90.0;
    snap.self_state.on_ground = true;
    snap.self_state.alive = true;
    snap.self_state.health = 14.0;
    snap.self_state.food = 17.0;
    snap.self_state.inventory.selected_hotbar_slot = 0;
    snap.self_state.inventory.slots = vec![
        world::InventorySlot {
            slot: 36,
            item_name: "iron_sword".to_owned(),
            count: 1,
            metadata: None,
            durability_used: None,
        },
        world::InventorySlot {
            slot: 10,
            item_name: "oak_planks".to_owned(),
            count: 12,
            metadata: None,
            durability_used: None,
        },
        world::InventorySlot {
            slot: 6,
            item_name: "iron_chestplate".to_owned(),
            count: 1,
            metadata: None,
            durability_used: None,
        },
    ];
    let entity =
        |key: &str, kind: &str, username: Option<&str>, x: f64, z: f64| world::EntitySnapshot {
            entity_key: key.to_owned(),
            protocol_entity_id: 7,
            entity_type: kind.to_owned(),
            name: None,
            username: username.map(str::to_owned),
            uuid: None,
            position: world::Vec3Value { x, y: 72.0, z },
            velocity: world::Vec3Value::default(),
            yaw: 0.0,
            pitch: 0.0,
            head_yaw: None,
            width: 0.6,
            height: 1.8,
            on_ground: true,
            pose: None,
            held_item_name: None,
            equipment: Vec::new(),
            valid: true,
        };
    snap.entities = vec![
        entity(
            "self-uuid",
            "minecraft:player",
            Some("companion"),
            10.5,
            3.5,
        ),
        entity("1:8", "minecraft:player", Some("alice"), 13.5, 3.5),
        entity("1:9", "minecraft:zombie", None, 18.5, 8.5),
        entity("1:10", "minecraft:zombie", None, 20.5, 9.5),
    ];
    let chat = |seq: u64, tick: u64, text: &str| world::ChatEntry {
        seq,
        tick,
        occurred_at: std::time::SystemTime::UNIX_EPOCH,
        source: world::FactSource::ServerObserved,
        sender: Some(world::PlayerRef {
            username: "alice".to_owned(),
            uuid: None,
        }),
        content: world::ChatContent {
            plain_text: text.to_owned(),
            position: Some(world::ChatPosition::Chat),
            verified: None,
        },
    };
    snap.chat.entries = vec![chat(1, 2_300, "你在哪"), chat(2, 2_350, "过来一下")];
    snap
}

struct FixedSnapshots(world::TickSnapshot);

impl SnapshotSource for FixedSnapshots {
    fn latest(&self) -> Arc<world::TickSnapshot> {
        Arc::new(self.0.clone())
    }
}

fn class_word(class: ToolClass) -> String {
    match class {
        ToolClass::Free => "Free（不占域、不受屏压制）".to_owned(),
        ToolClass::Body { domain } => format!("Body（域：{domain:?}，屏开时被压制）"),
    }
}

fn result_text(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => text.clone(),
            ContentPart::Json { value } => serde_json::to_string_pretty(value).unwrap_or_default(),
            other => format!("{other:?}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn call(provider: &dyn ToolProvider, name: &str, args: serde_json::Value) -> String {
    let result = provider.call(ToolCall::new("dump", name, args)).await;
    let status = match result.status {
        agent::ToolResultStatus::Success => "✅",
        _ => "❌",
    };
    format!("{status} {}", result_text(&result))
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let scratch = std::env::temp_dir().join(format!("model-surface-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("建临时目录");
    let snapshot = sample_snapshot();
    let snapshots: Arc<dyn SnapshotSource> = Arc::new(FixedSnapshots(snapshot.clone()));
    let door = Arc::new(NoDoor);

    println!("# 模型可见面全文（保真导出）\n");
    println!("> 由 `cargo run -p companion --example model_surface` 生成；除人设为复制件外，");
    println!("> 所有文本由真实代码路径产出，与运行时发给模型的内容逐字一致。\n");

    // ---- 一、基础上下文（每轮免压缩三段） ----
    println!("## 一、基础上下文（每轮开场，免压缩，全部 system 角色）\n");
    let memory_file = Arc::new(MemoryFile::new(scratch.join("memory.md")));
    memory_file
        .write("我叫 companion。tester 最喜欢的方块是青金石块。")
        .unwrap();
    let read_mark = Arc::new(ChatReadMark::new());
    read_mark.mark_read(1, 2_300); // 看过第一条，第二条未读
    let strategy = context::ContextStrategy::new(PLACEHOLDER_PERSONA, memory_file.clone())
        .with_situation(snapshots.clone(), read_mark.clone());
    for (index, item) in agent::PromptSource::base_context(&strategy)
        .unwrap()
        .iter()
        .enumerate()
    {
        if let agent::TranscriptItem::Input(message) = item {
            let text: String = message
                .content
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            println!(
                "### 第 {} 段（role={}）\n\n```text\n{}\n```\n",
                index + 1,
                message.role.as_str(),
                text
            );
        }
    }
    println!("记忆的另外两种状态：\n");
    let empty_memory = Arc::new(MemoryFile::new(scratch.join("empty.md")));
    let empty_strategy = context::ContextStrategy::new("（人设略）", empty_memory);
    if let agent::TranscriptItem::Input(message) =
        &agent::PromptSource::base_context(&empty_strategy).unwrap()[1]
    {
        let text: String = message
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        println!("- 空记忆时：`{text}`");
    }
    let broken_memory = Arc::new(MemoryFile::new(scratch.clone())); // 指向目录制造读取失败
    let broken_strategy = context::ContextStrategy::new("（人设略）", broken_memory);
    if let agent::TranscriptItem::Input(message) =
        &agent::PromptSource::base_context(&broken_strategy).unwrap()[1]
    {
        let text: String = message
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        println!("- 读取失败时：`{text}`\n");
    }
    println!("处境的其他相位：\n");
    for (name, phase) in [
        ("连接中", world::ConnectionPhase::Connecting),
        (
            "断线",
            world::ConnectionPhase::Disconnected {
                reason: "与服务器的连接已断开".to_owned(),
            },
        ),
        (
            "已停止",
            world::ConnectionPhase::Stopped {
                reason: "维护者停机".to_owned(),
            },
        ),
    ] {
        let mut other = snapshot.clone();
        other.phase = phase;
        println!(
            "- {name}：`{}`",
            render::render_situation(&other, read_mark.position())
        );
    }
    {
        let mut dead = snapshot.clone();
        dead.self_state.alive = false;
        println!("- 死亡时体征行：`{}`\n", render::render_vitals(&dead));
    }

    // ---- 二、工具表 ----
    println!("## 二、工具表（模型收到的定义原文）\n");
    let occupancy = Arc::new(Occupancy::new());
    let screen_state = Arc::new(ScreenState::new());
    let providers: Vec<(&str, Box<dyn ToolProvider>)> = vec![
        (
            "chat_box",
            Box::new(ChatBox::new(
                occupancy.clone(),
                screen_state.clone(),
                door.clone(),
                door.clone(),
                read_mark.clone(),
                snapshots.clone(),
            )),
        ),
        (
            "inventory",
            Box::new(InventoryScreen::new(
                occupancy.clone(),
                screen_state.clone(),
                door.clone(),
                snapshots.clone(),
            )),
        ),
        (
            "container",
            Box::new(ContainerScreen::new(
                occupancy.clone(),
                screen_state.clone(),
                door.clone(),
                snapshots.clone(),
            )),
        ),
        ("remember", Box::new(MemoryTools::new(memory_file.clone()))),
        (
            "motion/look",
            Box::new(motion::MotionTools::new(door.clone())),
        ),
        ("hand", Box::new(hand::HandTools::new(door.clone()))),
        (
            "scan",
            Box::new(perception::PerceptionTools::new(
                door.clone(),
                Arc::new(std::sync::Mutex::new(world::BlockMemory::new())),
            )),
        ),
    ];
    for (label, provider) in &providers {
        for (definition, class) in provider.tools() {
            let _ = label;
            println!(
                "### `{}` — {}\n",
                definition.name.as_str(),
                class_word(class)
            );
            if let Some(description) = &definition.description {
                println!("描述：{description}\n");
            }
            println!(
                "参数 schema：\n\n```json\n{}\n```\n",
                serde_json::to_string_pretty(&definition.input_schema).unwrap()
            );
        }
    }

    // ---- 三、屏内文本与工具回执样例 ----
    println!("## 三、工具回执与屏内文本（真实调用产出）\n");
    let chat_box = &providers[0].1;
    println!(
        "chat_box open(describe=true)（含用法全文）：\n\n```text\n{}\n```\n",
        call(
            chat_box.as_ref(),
            "chat_box",
            json!({"action":"open","describe":true})
        )
        .await
    );
    println!(
        "chat_box history：\n\n```text\n{}\n```\n",
        call(
            chat_box.as_ref(),
            "chat_box",
            json!({"action":"history","count":5})
        )
        .await
    );
    println!(
        "chat_box say：`{}`\n",
        call(
            chat_box.as_ref(),
            "chat_box",
            json!({"action":"say","text":"你好"})
        )
        .await
    );
    let inventory = &providers[1].1;
    println!(
        "inventory open（46 格清单+用法全文）：\n\n```text\n{}\n```\n",
        call(inventory.as_ref(), "inventory", json!({"action":"open"})).await
    );
    println!(
        "inventory move：`{}`",
        call(
            inventory.as_ref(),
            "inventory",
            json!({"action":"move","from":10,"to":38})
        )
        .await
    );
    println!(
        "inventory move 丢弃：`{}`",
        call(
            inventory.as_ref(),
            "inventory",
            json!({"action":"move","from":10,"to":99})
        )
        .await
    );
    println!(
        "inventory close：`{}`\n",
        call(inventory.as_ref(), "inventory", json!({"action":"close"})).await
    );
    let container = &providers[2].1;
    // 容器没有 open 动作：开屏由服务器发起，这里模拟组合根对开屏事实的
    // 处置（登记状态）后取回执。开屏通知全文见 §五。
    screen_state.server_open(screens::ScreenKind::Container);
    println!(
        "container move（取成品到快捷栏）：`{}`",
        call(
            container.as_ref(),
            "container",
            json!({"action":"move","from":0,"to":40})
        )
        .await
    );
    println!(
        "container move 拆栈：`{}`",
        call(
            container.as_ref(),
            "container",
            json!({"action":"move","from":37,"to":2,"count":1})
        )
        .await
    );
    println!(
        "container close：`{}`\n",
        call(container.as_ref(), "container", json!({"action":"close"})).await
    );
    let motion_tools = &providers[4].1;
    println!(
        "motion go_to：`{}`",
        call(
            motion_tools.as_ref(),
            "motion",
            json!({"action":"go_to","target":[35.0,72.0,3.0]})
        )
        .await
    );
    let scan = &providers[6].1;
    println!(
        "\nscan 环视（呈现样例）：\n\n```text\n{}\n```\n",
        call(scan.as_ref(), "scan", json!({})).await
    );
    println!(
        "scan 定向（呈现样例）：\n\n```text\n{}\n```\n",
        call(scan.as_ref(), "scan", json!({"at":[[6,72,3],[0,60,0]]})).await
    );
    println!(
        "scan 增量（呈现样例；与已见过的对比，git 式差异行）：\n\n```text\n{}\n```\n",
        call(scan.as_ref(), "scan", json!({"changes": true})).await
    );

    // ---- 四、拒绝与报错话术 ----
    println!("## 四、拒绝与报错话术（真实调用产出）\n");
    let _ = call(inventory.as_ref(), "inventory", json!({"action":"open"})).await; // 占屏
    println!(
        "- 物品栏开着时 chat_box say：`{}`",
        call(
            chat_box.as_ref(),
            "chat_box",
            json!({"action":"say","text":"你好"})
        )
        .await
    );
    println!("- 物品栏没开时 move：先 close 再试 → `{}`", {
        let _ = call(inventory.as_ref(), "inventory", json!({"action":"close"})).await;
        call(
            inventory.as_ref(),
            "inventory",
            json!({"action":"move","from":1,"to":2}),
        )
        .await
    });
    println!(
        "- 容器没开时 move：`{}`",
        call(
            container.as_ref(),
            "container",
            json!({"action":"move","from":1,"to":2})
        )
        .await
    );
    println!(
        "- 未知 action：`{}`",
        call(chat_box.as_ref(), "chat_box", json!({"action":"dance"})).await
    );
    println!(
        "- motion 缺参数：`{}`",
        call(motion_tools.as_ref(), "motion", json!({"action":"forward"})).await
    );
    println!(
        "- look 越界俯仰：`{}`",
        call(
            motion_tools.as_ref(),
            "look",
            json!({"action":"face","yaw":0.0,"pitch":120.0})
        )
        .await
    );
    let hand_tools = &providers[5].1;
    println!(
        "- hand use_on 参数二选一：`{}`",
        call(hand_tools.as_ref(), "hand", json!({"action":"use_on"})).await
    );
    println!(
        "- scan at 越界：`{}`",
        call(scan.as_ref(), "scan", json!({"at":[[1,2]]})).await
    );
    println!("\n编排层拒绝（dispatch，字面常量）：\n");
    println!("- 屏压制其他身体域：`有界面开着，无法移动或与世界交互；先关闭界面再行动`");
    println!("- 未知工具名：`没有名为 xx 的工具；请改用工具列表中的名字`\n");
    println!("接入机器的如实拒绝（字面常量，经门原文转达）：\n");
    println!("- `尚未连接到世界，无法行动` / `尚未连接到世界，无法观察` / `连接已结束`");
    println!("- `附近没有 {{entity_key}} 这个实体`（attack/use_on 实体找不到时）");
    println!("- `格号 {{slot}} 超出当前界面范围（0-{{max}}）` / `两个格号相同，没有可交换的`");
    println!("- `没有开着的容器界面`（没有容器时 container close）\n");

    // ---- 五、通知措辞（唤醒时以 user 角色投递） ----
    println!("## 五、唤醒投递的措辞（user 角色）\n");
    println!("聊天：`alice: 过来一下`（发言者名: 原文）\n");
    println!("任务通知（render_job_entry 全谱）：\n");
    for outcome in [
        world::JobOutcome::Arrived,
        world::JobOutcome::PathEnded,
        world::JobOutcome::Stalled,
        world::JobOutcome::Replaced,
        world::JobOutcome::Stopped,
    ] {
        let entry = world::JobEntry {
            seq: 1,
            tick: 100,
            occurred_at: std::time::SystemTime::UNIX_EPOCH,
            job: world::JobKind::MoveTo {
                destination: [35, 72, 3],
            },
            outcome,
        };
        let delivered = matches!(
            outcome,
            world::JobOutcome::Arrived | world::JobOutcome::PathEnded | world::JobOutcome::Stalled
        );
        println!(
            "- {}`{}`",
            if delivered {
                ""
            } else {
                "（入窗不投递）"
            },
            render::render_job_entry(&entry)
        );
    }
    println!("\n受伤通知：\n");
    for (before, after) in [(20.0_f32, 14.0_f32), (14.0, 0.0)] {
        let entry = world::DamageEntry {
            seq: 1,
            tick: 100,
            occurred_at: std::time::SystemTime::UNIX_EPOCH,
            health_before: before,
            health_after: after,
            cause: None,
        };
        println!("- `{}`", render::render_damage_entry(&entry));
    }
    println!("\n库存变化通知（格位类屏开着才投递；自己动作的回声不投递；容器 0=物品栏屏，非 0=当时开着的容器格空间）：\n");
    for (container_id, slot, item, count) in [
        (0_i32, 38_u16, Some("emerald"), 5_u32),
        (0, 0, Some("oak_button"), 1),
        (0, 10, None, 0),
        (3, 0, Some("oak_button"), 1),
        (3, 5, None, 0),
    ] {
        let entry = world::InventoryChangeEntry {
            seq: 1,
            tick: 100,
            occurred_at: std::time::SystemTime::UNIX_EPOCH,
            source: world::FactSource::ServerObserved,
            container_id,
            slot,
            item_name: item.map(str::to_owned),
            count,
        };
        println!("- `{}`", render::render_inventory_change(&entry));
    }
    println!("\n屏通知（容器真相在服务端，组合根随屏事实投递；种类差异=清单段表+用法补充）：\n");
    {
        let mut crafting_snap =
            world::TickSnapshot::empty(world::Epoch(1), 120, world::ConnectionPhase::Ready);
        crafting_snap.self_state.inventory.slots = vec![
            world::InventorySlot {
                slot: 5,
                item_name: "oak_planks".to_owned(),
                count: 2,
                metadata: None,
                durability_used: None,
            },
            world::InventorySlot {
                slot: 20,
                item_name: "stick".to_owned(),
                count: 4,
                metadata: None,
                durability_used: None,
            },
            world::InventorySlot {
                slot: 40,
                item_name: "bread".to_owned(),
                count: 7,
                metadata: None,
                durability_used: None,
            },
        ];
        println!("开屏（hand use_on 工作台后，服务器打开界面）：\n\n```text\n容器界面已打开（crafting）。\n{}\n\n{}\n```\n", render::render_container_menu(&crafting_snap, "crafting"), container_usage("crafting"));
        println!("开屏（箱子，无种类补充时只有通用用法）：\n\n```text\n容器界面已打开（generic_9x3「木箱」）。\n{}\n\n{}\n```\n", render::render_container_menu(&crafting_snap, "generic_9x3"), container_usage("generic_9x3"));

        let mut furnace_snap =
            world::TickSnapshot::empty(world::Epoch(1), 121, world::ConnectionPhase::Ready);
        furnace_snap.self_state.inventory.slots = vec![
            world::InventorySlot {
                slot: 4,
                item_name: "raw_iron".to_owned(),
                count: 3,
                metadata: None,
                durability_used: None,
            },
            world::InventorySlot {
                slot: 30,
                item_name: "coal".to_owned(),
                count: 2,
                metadata: None,
                durability_used: None,
            },
        ];
        println!("开屏（熔炉族有专属段表与补充；高炉/烟熏炉同形）：\n\n```text\n容器界面已打开（furnace「熔炉」）。\n{}\n\n{}\n```\n", render::render_container_menu(&furnace_snap, "furnace"), container_usage("furnace"));
    }
    println!("- 被服务器关闭（非自己 close 的回声）：`容器界面被关闭了（crafting）。`");

    // ---- 六、压缩（上下文满时的模型交互） ----
    println!("\n## 六、上下文压缩（满时对模型的指令与结果包裹）\n");
    println!(
        "压缩指令全文（system，后接【长期记忆现文】与被压缩对话）：\n\n```text\n{}\n```\n",
        context::COMPACTION_INSTRUCTIONS
    );
    println!("收尾催告（user）：`请按上面的规则输出压缩 JSON。`\n");
    println!(
        "压缩成功后新对话开头的摘要包裹（user）：\n\n```text\n{}\n（摘要正文）\n```",
        context::SUMMARY_PREFIX
    );

    let _ = std::fs::remove_dir_all(&scratch);
}
