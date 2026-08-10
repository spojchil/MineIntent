//! 模块一·原版连接与感知——数据出口的类型层。
//!
//! 唯一数据出口是 tick 快照：latest-wins，外部持旧 `Arc` 用多久都行，
//! 模块换新指针永不等它。本 crate 先定型快照与读侧接口；连接机器
//! （start/stop/写口/视口）随后落位于同一 crate。
//!
//! 直译无损、政策外置：这里的每个字段都是 azalea ECS 组件或原版协议
//! 事实的直译，不过滤、不判重要性、不做丢弃决策。世界方块不在快照里
//! ——最深最重的嵌套留在 azalea 世界模型原地，走拉的路。

use std::sync::Arc;
use std::time::SystemTime;

mod block;
#[cfg(feature = "azalea")]
mod machine;
mod viewport;

pub use block::*;
#[cfg(feature = "azalea")]
pub use machine::{ConnectionConfig, DoorCommand, Module};
pub use viewport::*;

/// 挂钟时刻，取证用。
pub type Timestamp = SystemTime;

/// 把角度收进 [−180, 180)，等价于原版 `Mth.wrapDegrees`。
///
/// 原版和 azalea 都不归一化 yaw：一直往同一个方向转会累加。内部三角函数
/// 是周期的无所谓；给人或模型看的角度必须先过这里。非有限数原样返回，
/// 缺陷不伪装成一个像样的角度。
pub fn wrap_degrees(value: f64) -> f64 {
    if !value.is_finite() {
        return value;
    }
    let mut wrapped = value % 360.0;
    if wrapped >= 180.0 {
        wrapped -= 360.0;
    }
    if wrapped < -180.0 {
        wrapped += 360.0;
    }
    // −0.0 与 0.0 是同一个朝向，显示出来却像有区别。
    if wrapped == 0.0 {
        0.0
    } else {
        wrapped
    }
}

/// 连接纪元。重连必换纪元；`(epoch, tick)` 成对单调。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Epoch(pub u64);

/// 快照读侧：任何时刻取最新快照，不阻塞、不排队。
/// 该不该因新快照开一轮，是读方的判断，本接口无意见。
pub trait SnapshotSource: Send + Sync {
    fn latest(&self) -> Arc<TickSnapshot>;
}

/// 聊天窗容量：原版客户端聊天留存行数，焊死。
pub const CHAT_WINDOW_LINES: usize = 100;
/// 声音窗时长：原版客户端配音留存 tick 数，焊死。
pub const SOUND_WINDOW_TICKS: u64 = 60;

/// 一个 tick 的世界快照。"快照"是语义不是深拷贝义务，实现可结构共享。
impl TickSnapshot {
    /// 尚未进入世界时的空快照：连接机器的初始状态，测试夹具同用。
    pub fn empty(epoch: Epoch, tick: u64, phase: ConnectionPhase) -> Self {
        Self {
            epoch,
            tick,
            captured_at: SystemTime::now(),
            phase,
            world_meta: WorldMeta::default(),
            self_state: SelfState::default(),
            entities: Vec::new(),
            players: Vec::new(),
            chat: Window::default(),
            sounds: Window::default(),
            damage: Window::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TickSnapshot {
    pub epoch: Epoch,
    /// 自本连接 login 起的 tick 计数（azalea `TicksConnected`）。
    pub tick: u64,
    pub captured_at: Timestamp,

    /// 连接状态是状态，活在快照里。
    pub phase: ConnectionPhase,

    /// 环境事实里的热门部分：维度、天色、天气每轮都与行为相关，进轮末帧。
    /// 冷门档案数（Day #、群系）不在这里，走感知的信息工具按需查。
    pub world_meta: WorldMeta,

    pub self_state: SelfState,
    pub entities: Vec<EntitySnapshot>,
    /// tab 表的状态形式。
    pub players: Vec<PlayerListEntry>,

    // ── 时间窗：原版客户端本来就养、azalea 不给的 ──────────────
    pub chat: Window<ChatEntry>,
    pub sounds: Window<SoundEntry>,
    /// ⚠ 拟含，随关注清单（唤醒判据）裁定收尾。
    /// 尺寸约束与 chat 同理：关注类窗口 ≥ 最长一轮时长，不得抄声音的 60 tick。
    pub damage: Window<DamageEntry>,
}

/// 世界环境的直译。呈现（时段文案、"下大雨"）归渲染层。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorldMeta {
    /// 维度注册名（如 `minecraft:overworld`）。
    pub dimension: String,
    /// 世界时钟原始值：`% 24000` 是一天内时刻（轮末帧的时段），
    /// `/ 24000` 是天数（信息工具的 Day #）——一个事实，两种呈现。
    pub day_time: u64,
    /// 雨强度 0..1（原版客户端的过渡值直译）。
    pub rain_level: f32,
    /// 雷暴强度 0..1。
    pub thunder_level: f32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionPhase {
    Connecting,
    Ready,
    /// 断线如实：原因原文保留。重连成功 = 新纪元的新快照，不是本状态消失。
    Disconnected { reason: String },
    /// stop 之后的终态。
    Stopped { reason: String },
}

/// 时间窗。条目按 tick 升序；逐出 = 各自的原版常量，无其他政策。
/// 条目自带 tick，"取某 tick 之后的条目"是读方一行过滤，不需要队列。
#[derive(Clone, Debug, PartialEq)]
pub struct Window<T> {
    pub entries: Vec<T>,
}

impl<T> Default for Window<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

/// 事实来源。沿用现契约三分法。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FactSource {
    /// 我们下令产生的。
    Commanded,
    /// 客户端本地推导/配音。
    ClientPredicted,
    /// 服务端明示。
    ServerObserved,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlayerRef {
    pub username: String,
    pub uuid: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChatEntry {
    /// 本连接内单调递增的到达序号。tick 在同一游戏刻内可重复，
    /// 读方要"恰好一次"地消费聊天必须用 seq 做游标，不能用 tick。
    pub seq: u64,
    pub tick: u64,
    pub occurred_at: Timestamp,
    pub source: FactSource,
    /// 系统消息为 None。
    pub sender: Option<PlayerRef>,
    pub content: ChatContent,
}

/// azalea ChatPacket 的保真直译，不做文案化。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatContent {
    pub plain_text: String,
    pub position: Option<ChatPosition>,
    /// 签名校验结论；服务端不给则 None。
    pub verified: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChatPosition {
    Chat,
    System,
    GameInfo,
}

/// 声音注册名（如 `entity.zombie.ambient`）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SoundId(pub String);

#[derive(Clone, Debug, PartialEq)]
pub struct SoundEntry {
    pub tick: u64,
    pub occurred_at: Timestamp,
    /// 服务端明示（ClientboundSound）/ 客户端配音（LevelEvent 类）/ 纯本地推导。
    pub source: FactSource,
    pub sound: SoundId,
    pub position: Option<[f64; 3]>,
}

/// 伤害类型注册名的直译；服务端给因就直译，不给不编。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DamageCause(pub String);

#[derive(Clone, Debug, PartialEq)]
pub struct DamageEntry {
    pub tick: u64,
    pub occurred_at: Timestamp,
    pub health_before: f32,
    pub health_after: f32,
    pub cause: Option<DamageCause>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3Value {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// 自身状态：玩家实体 ECS 组件的直译，含背包（背包是玩家实体的组件）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SelfState {
    pub entity_key: String,
    pub username: String,
    pub position: Vec3Value,
    pub velocity: Vec3Value,
    /// 角度制，未归一化：连续转身会累加出 −10313 这样的值。
    /// 给人或模型看时必须归一化到 [−180, 180)（渲染层的义务）。
    pub yaw: f64,
    /// 角度制，原版钳在 ±90。
    pub pitch: f64,
    pub on_ground: bool,
    pub alive: bool,
    pub health: f64,
    pub food: f64,
    pub food_saturation: f64,
    /// 只在水下等耗氧场景有意义时为 Some。
    pub oxygen: Option<f64>,
    pub experience: Option<ExperienceState>,
    pub effects: Vec<StatusEffect>,
    pub inventory: Inventory,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExperienceState {
    pub level: u32,
    pub progress: f64,
    pub total: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StatusEffect {
    pub name: String,
    pub amplifier: i32,
    pub duration_ticks: Option<i32>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Inventory {
    pub selected_hotbar_slot: u8,
    pub slots: Vec<InventorySlot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InventorySlot {
    pub slot: u32,
    pub item_name: String,
    pub count: u32,
    pub metadata: Option<i32>,
    pub durability_used: Option<u32>,
}

/// 周围实体：azalea 实体 ECS 组件的直译。
#[derive(Clone, Debug, PartialEq)]
pub struct EntitySnapshot {
    pub entity_key: String,
    pub protocol_entity_id: i32,
    pub entity_type: String,
    pub name: Option<String>,
    pub username: Option<String>,
    pub uuid: Option<String>,
    pub position: Vec3Value,
    pub velocity: Vec3Value,
    /// 角度制，未归一化。见 [`SelfState::yaw`]。
    pub yaw: f64,
    /// 角度制，钳在 ±90。
    pub pitch: f64,
    /// 角度制，未归一化。
    pub head_yaw: Option<f64>,
    pub width: f64,
    pub height: f64,
    pub on_ground: bool,
    pub pose: Option<String>,
    pub held_item_name: Option<String>,
    pub equipment: Vec<EntityEquipment>,
    pub valid: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntityEquipment {
    pub slot: u32,
    pub item_name: String,
    pub count: u32,
}

/// tab 表条目；实体在追踪范围内时附带实体侧观测。
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerListEntry {
    pub player_key: String,
    pub uuid: Option<String>,
    pub username: String,
    pub listed: bool,
    pub entity_tracked: bool,
    pub position: Option<Vec3Value>,
    pub yaw: Option<f64>,
    pub pitch: Option<f64>,
    pub held_item_name: Option<String>,
}
