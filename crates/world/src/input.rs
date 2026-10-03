//! 键鼠输入的类型层：一次输入 = 一组按住的键 + 至多一个鼠标键 + 起手的鼠标相对移动，
//! 保持若干 tick 后全部松开。
//!
//! 纯数据，不碰 azalea；按键时序与准星判定在连接机器的 `machine::input` 里。
//! 语义逐项对齐原版默认键位：WASD 走、空格跳、Shift 潜行、Ctrl 疾跑、
//! 左键攻击/破坏、右键使用/放置——都作用于准星所指，不收坐标。

use crate::LookingAt;

/// 一次最多按住多少 tick。按住期间模型看不见世界、也不能改主意，
/// 所以给一个上限：10 秒够挖穿一块黑曜石以外的绝大多数方块、跑一段路。
pub const MAX_INPUT_TICKS: u32 = 200;

/// 按住的键。对向键同时按下互相抵消，与原版 `KeyboardInput` 一致。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeldKeys {
    /// W
    pub forward: bool,
    /// S
    pub back: bool,
    /// A
    pub left: bool,
    /// D
    pub right: bool,
    /// 空格
    pub jump: bool,
    /// Shift
    pub sneak: bool,
    /// Ctrl：只在向前走时生效，与原版疾跑键一致。
    pub sprint: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MouseButton {
    /// 攻击/破坏：准星在实体上是一次攻击；在方块上按住就一直挖。
    Left,
    /// 使用/放置：准星在方块上是对它使用（放置、开门、开箱）；
    /// 在实体上是交互；都不是就用手里的物品（吃、拉弓、举盾）。
    Right,
}

/// 鼠标相对移动，单位是度。**右、下为正**——和挪鼠标的直觉一致，
/// 也恰好是原版 yaw/pitch 的增长方向（yaw 向右转增大，pitch 向下看增大）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Turn {
    pub yaw: f32,
    pub pitch: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputSpec {
    pub keys: HeldKeys,
    pub mouse: Option<MouseButton>,
    /// 起手时先转过去，再按键。准星判定用转完之后的朝向。
    pub turn: Option<Turn>,
    /// 按住几 tick（1..=[`MAX_INPUT_TICKS`]）。1 就是点按。
    pub ticks: u32,
}

/// 输入为什么结束。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputEnd {
    /// 按满了时长。
    Elapsed,
    /// 按着左键时准星下的方块碎了——像人看见方块碎掉就松手，
    /// 不会继续挖到后面那块。
    BlockBroken,
    /// 界面打开了（右键开了箱子、被别人推进了界面）。原版开界面时松开所有按键。
    ScreenOpened,
    /// 死了。
    Died,
    /// 被下一次输入顶替。
    Replaced,
}

/// 一次输入的结果：松开时的如实回执。
#[derive(Clone, Debug, PartialEq)]
pub struct InputOutcome {
    /// 实际按住的 tick 数。
    pub ticks: u32,
    pub ended: InputEnd,
    /// 按下前与松开后的脚底位置（F3 可见的坐标）。
    pub from: [f64; 3],
    pub to: [f64; 3],
    /// 松开时的朝向（度）。
    pub yaw: f32,
    pub pitch: f32,
    /// 按的是哪个鼠标键；没按鼠标为 None。
    pub mouse: Option<MouseButton>,
    /// 按下鼠标那一刻准星所指；没按鼠标或准星没指着东西时为 None。
    pub pressed_on: Option<LookingAt>,
    /// 按住左键期间在准星下碎掉的方块（注册名，按碎的先后）。
    /// 这是客户端看到的碎裂，和玩家屏幕上看到的一样；服务端若回滚，下一张画面会显示出来。
    pub broken: Vec<String>,
    /// 按住右键期间放下的方块（注册名、位置，按先后）。
    /// 和原版一样按客户端判断算放下，不等服务端；服务端若拒绝，下一张画面会显示出来。
    pub placed: Vec<(String, [i32; 3])>,
    /// 右键直接作用在准星下的方块上（开门、拨拉杆、调中继器……）的结果。
    pub used: Option<BlockUsed>,
    /// 要等服务端回声的结果，限时内没等到：结果未知，不是「没发生」。
    pub unconfirmed: Option<Unconfirmed>,
}

/// 右键对方块本身的使用。
///
/// 门、活板门、栅栏门、按钮、中继器、比较器、花盆原版客户端当场就改，回执照
/// 客户端算；拉杆、音符盒、蛋糕这类只有服务端改，回执等服务端的方块更新。
#[derive(Clone, Debug, PartialEq)]
pub struct BlockUsed {
    /// 按下时的方块注册名与位置。
    pub block: String,
    pub position: [i32; 3],
    /// 按住期间用了几次（按住右键每 4 tick 再用一次）。
    pub times: u32,
    /// 和按下前比变了的状态：（属性、之前、现在）。方块整个换了时属性名是 `block`。
    pub changes: Vec<(String, String, String)>,
}

/// 等服务端确认、但限时内没等到的那件事。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unconfirmed {
    /// 右键点了会开界面的方块，界面没在限时内打开。
    Screen,
    /// 右键用了只由服务端改动的方块（拉杆等），方块没在限时内变。
    BlockUse,
}

/// 手里的一件东西：注册名与数量。
pub type HeldStack = Option<(String, u32)>;

/// 手上瞬时键（数字键、Q、F）的结果。
#[derive(Clone, Debug, PartialEq)]
pub enum HandOutcome {
    /// 换到了快捷栏第几格（0 起），现在手里是什么。客户端当场生效。
    Selected { slot: u8, held: HeldStack },
    /// 丢出了手里的东西。和原版 `LocalPlayer.drop` 一样客户端当场从手里拿走，
    /// 不等服务端；服务端若不认，物品会回到手里，画面上看得到。
    Dropped { item: String, count: u32 },
    /// 手里是空的，没有可丢的。
    NothingToDrop,
    /// 主副手对调了（服务端已回声）：对调后的主手、副手。
    Swapped { main: HeldStack, offhand: HeldStack },
    /// 两手拿的一样（或都空），对调了也看不出区别。
    SwapNoChange { both: HeldStack },
    /// 对调要由服务端做，限时内没等到它的回声：两手还是按之前的样子。
    SwapUnconfirmed { main: HeldStack, offhand: HeldStack },
}

impl HeldKeys {
    pub fn any(&self) -> bool {
        self.forward
            || self.back
            || self.left
            || self.right
            || self.jump
            || self.sneak
            || self.sprint
    }
}
