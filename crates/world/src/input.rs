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
