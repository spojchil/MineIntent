//! 键鼠输入的时序：起手转向 → 下一 tick 按键 → 按满时长（或提前结束）→ 全部松开。
//!
//! 为什么按键晚一 tick：准星（`HitResultComponent`）由 azalea 在自己的 tick 里
//! 按当前朝向重算。转向和按鼠标若在同一刻，按下去的会是转之前指着的东西。
//!
//! 同一时刻只有一次输入在按；新输入顶替旧输入（旧的以 [`InputEnd::Replaced`] 回执）。
//! 判定是纯函数（[`decide_end`]、[`walk_direction`]），副作用只在 [`poll_input`] 与
//! [`release`] 里落。每次输入恰好一个回执：正常结束走 [`finish`]，连接结束时
//! 丢掉发送端，等待方收到「连接已结束」。

use azalea::core::hit_result::HitResult;
use azalea::entity::metadata::{AbstractLivingUsingItem, Health};
use azalea::entity::Position;
use azalea::protocol::packets::game::s_player_action;
use azalea::{BlockPos, Client, SprintDirection, WalkDirection};
use tokio::sync::oneshot;

use super::state::Inner;
use crate::{HeldKeys, InputEnd, InputOutcome, InputSpec, LookingAt, MouseButton};

/// 按住右键时重复使用的间隔。原版 `Minecraft.rightClickDelay` 每次使用后置 4，
/// 按住期间不在使用物品（不是在吃、拉弓）就每 4 tick 再按一次——连续放方块就靠它。
const RIGHT_CLICK_REPEAT_TICKS: u64 = 4;

/// 一次输入的回执通道。`DoorCommand` 要 `Clone`，发送端只能被取走一次，所以包一层。
#[derive(Clone)]
pub struct InputCompletion(
    std::sync::Arc<parking_lot::Mutex<Option<oneshot::Sender<InputOutcome>>>>,
);

impl InputCompletion {
    pub(super) fn new() -> (Self, oneshot::Receiver<InputOutcome>) {
        let (sender, receiver) = oneshot::channel();
        (
            Self(std::sync::Arc::new(parking_lot::Mutex::new(Some(sender)))),
            receiver,
        )
    }

    fn take(&self) -> Option<oneshot::Sender<InputOutcome>> {
        self.0.lock().take()
    }
}

impl std::fmt::Debug for InputCompletion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("InputCompletion")
    }
}

/// 在按的那次输入。
pub(super) struct HeldInput {
    spec: InputSpec,
    queued_tick: u64,
    /// 真正按下的 tick；`None` = 已转向、等准星刷新。
    pressed_tick: Option<u64>,
    from: [f64; 3],
    pressed_on: Option<LookingAt>,
    broken: Vec<String>,
    /// 上一 tick 准星下的方块。按着左键时它下一 tick 变成空气，就是挖碎了。
    crosshair_block: Option<([i32; 3], String)>,
    last_use_tick: u64,
    done: oneshot::Sender<InputOutcome>,
}

/// 接受一次新输入：顶替旧的、转向、记下起点。按键留到下一 tick。
pub(super) fn begin(
    inner: &Inner,
    bot: &Client,
    spec: InputSpec,
    completion: InputCompletion,
) -> Result<(), String> {
    let done = completion
        .take()
        .ok_or_else(|| "这次输入已经提交过".to_owned())?;
    let from = position(bot)?;
    // 旧的坐标式任务（寻路、坐标挖掘）与键鼠抢同一副身体：先收掉，免得松开后它接着走。
    inner.movement_job.cancel(inner);
    bot.force_retire_pathfinding();
    inner.mining_job.cancel(inner);

    let previous = inner.held_input.lock().take();
    if let Some(previous) = previous {
        release(bot, &previous.spec);
        finish(bot, previous, InputEnd::Replaced, inner.now_tick());
    }

    if let Some(turn) = spec.turn {
        let look = bot.direction();
        // 原版 pitch 夹在 ±90；yaw 不夹，azalea 自己归一。
        let pitch = (look.x_rot() + turn.pitch).clamp(-90.0, 90.0);
        bot.set_direction(look.y_rot() + turn.yaw, pitch);
    }
    *inner.held_input.lock() = Some(HeldInput {
        spec,
        queued_tick: inner.now_tick(),
        pressed_tick: None,
        from,
        pressed_on: None,
        broken: Vec::new(),
        crosshair_block: None,
        last_use_tick: 0,
        done,
    });
    Ok(())
}

/// 每 tick 推进在按的输入。
pub(super) fn poll_input(inner: &Inner, bot: &Client) {
    let now = inner.now_tick();
    let mut slot = inner.held_input.lock();
    let Some(held) = slot.as_mut() else {
        return;
    };

    let Some(pressed_tick) = held.pressed_tick else {
        if now > held.queued_tick {
            press(inner, bot, held, now);
        }
        return;
    };

    let mut block_broke = false;
    if held.spec.mouse == Some(MouseButton::Left) {
        if let Some((at, name)) = held.crosshair_block.take() {
            if super::door::block_is_air(inner, at) == Ok(true) {
                held.broken.push(name);
                block_broke = true;
            }
        }
        held.crosshair_block = crosshair_block(inner, bot);
    }
    let observed = Observed {
        dead: is_dead(bot),
        screen_open: screen_open(bot),
        block_broke,
    };
    if let Some(end) = decide_end(pressed_tick, held.spec.ticks, now, observed) {
        let held = slot.take().expect("上面刚借到 Some");
        drop(slot);
        release(bot, &held.spec);
        finish(bot, held, end, now);
        return;
    }

    // 重复右键只发生在还按着的 tick。松开那一刻若再补一次，使用请求排在
    // Azalea 下一 GameTick 才发出，晚于立即写出的松手包——实服里这让松开后
    // 又整整吃掉一个金苹果。
    if held.spec.mouse == Some(MouseButton::Right)
        && now.saturating_sub(held.last_use_tick) >= RIGHT_CLICK_REPEAT_TICKS
        && !using_item(bot)
    {
        bot.start_use_item();
        held.last_use_tick = now;
    }
}

/// 连接结束：松开并丢掉发送端，等待方如实收到「连接已结束」。
pub(super) fn connection_ended(inner: &Inner) {
    inner.held_input.lock().take();
}

fn press(inner: &Inner, bot: &Client, held: &mut HeldInput, now: u64) {
    press_keys(bot, held.spec.keys);
    match held.spec.mouse {
        Some(MouseButton::Left) => {
            held.pressed_on = super::capture::capture_looking_at(bot);
            // 原版 startAttack：准星在实体上就打一下；按住不会连打，
            // 之后的 continueAttack 只挖方块——LeftClickMine 恰好只挖方块。
            if let Some(HitResult::Entity(hit)) = hit_result(bot) {
                bot.attack(hit.entity);
            }
            bot.left_click_mine(true);
            held.crosshair_block = crosshair_block(inner, bot);
        }
        Some(MouseButton::Right) => {
            held.pressed_on = super::capture::capture_looking_at(bot);
            bot.start_use_item();
            held.last_use_tick = now;
        }
        None => {}
    }
    held.pressed_tick = Some(now);
}

fn press_keys(bot: &Client, keys: HeldKeys) {
    let direction = walk_direction(keys);
    match sprint_direction(keys, direction) {
        Some(sprint) => bot.sprint(mirror_sprint(sprint)),
        None => bot.walk(mirror_walk(direction)),
    }
    bot.set_jumping(keys.jump);
    bot.set_crouching(keys.sneak);
}

/// 松开这次输入按下的一切。没按的也一并归零：身体上不该留着任何按键。
fn release(bot: &Client, spec: &InputSpec) {
    bot.walk(WalkDirection::None);
    bot.set_jumping(false);
    bot.set_crouching(false);
    match spec.mouse {
        Some(MouseButton::Left) => {
            bot.left_click_mine(false);
            super::mining::retire_mining(bot);
        }
        Some(MouseButton::Right) => {
            // 松右键总发：点按吃东西时服务端的「在用」回声可能还没到，
            // 等回声再决定会让一次点按变成吃完一整块。没在用时服务端忽略它。
            bot.write_packet(s_player_action::ServerboundPlayerAction {
                action: s_player_action::Action::ReleaseUseItem,
                pos: BlockPos::new(0, 0, 0),
                direction: Default::default(),
                seq: 0,
            });
        }
        None => {}
    }
}

fn finish(bot: &Client, held: HeldInput, ended: InputEnd, now: u64) {
    let look = bot.direction();
    let outcome = InputOutcome {
        ticks: held
            .pressed_tick
            .map(|pressed| now.saturating_sub(pressed) as u32)
            .unwrap_or(0),
        ended,
        from: held.from,
        to: position(bot).unwrap_or(held.from),
        yaw: look.y_rot(),
        pitch: look.x_rot(),
        pressed_on: held.pressed_on,
        broken: held.broken,
    };
    let _ = held.done.send(outcome);
}

#[derive(Clone, Copy, Debug, Default)]
struct Observed {
    dead: bool,
    screen_open: bool,
    block_broke: bool,
}

/// 这一 tick 该不该松开，为什么。先到先报：死亡与开界面压过其他原因。
fn decide_end(pressed_tick: u64, ticks: u32, now: u64, observed: Observed) -> Option<InputEnd> {
    if observed.dead {
        Some(InputEnd::Died)
    } else if observed.screen_open {
        Some(InputEnd::ScreenOpened)
    } else if observed.block_broke {
        Some(InputEnd::BlockBroken)
    } else if now.saturating_sub(pressed_tick) >= u64::from(ticks) {
        Some(InputEnd::Elapsed)
    } else {
        None
    }
}

/// WASD 合成走向。对向键互相抵消。
fn walk_direction(keys: HeldKeys) -> WalkDirection {
    let forward = i8::from(keys.forward) - i8::from(keys.back);
    let left = i8::from(keys.left) - i8::from(keys.right);
    match (forward, left) {
        (1, 0) => WalkDirection::Forward,
        (1, 1) => WalkDirection::ForwardLeft,
        (1, -1) => WalkDirection::ForwardRight,
        (-1, 0) => WalkDirection::Backward,
        (-1, 1) => WalkDirection::BackwardLeft,
        (-1, -1) => WalkDirection::BackwardRight,
        (0, 1) => WalkDirection::Left,
        (0, -1) => WalkDirection::Right,
        _ => WalkDirection::None,
    }
}

/// Azalea（fork `cce19df`）`tick_controls` 的左右号是反的：`Right` 给 `left_impulse`
/// 加 1，原版 `KeyboardInput` 是**左**键加 1。实服验证过：按 A 身体向右平移。
/// 应当修在 fork 源头；修好并升级 rev 之前，在交给 Azalea 的这一处镜像左右，
/// 本层其余地方的「左/右」都按原版语义。
fn mirror_walk(direction: WalkDirection) -> WalkDirection {
    match direction {
        WalkDirection::Left => WalkDirection::Right,
        WalkDirection::Right => WalkDirection::Left,
        WalkDirection::ForwardLeft => WalkDirection::ForwardRight,
        WalkDirection::ForwardRight => WalkDirection::ForwardLeft,
        WalkDirection::BackwardLeft => WalkDirection::BackwardRight,
        WalkDirection::BackwardRight => WalkDirection::BackwardLeft,
        other => other,
    }
}

/// 同 [`mirror_walk`]：疾跑的走向经同一个 `tick_controls` 落地。
fn mirror_sprint(direction: SprintDirection) -> SprintDirection {
    match direction {
        SprintDirection::ForwardLeft => SprintDirection::ForwardRight,
        SprintDirection::ForwardRight => SprintDirection::ForwardLeft,
        SprintDirection::Forward => SprintDirection::Forward,
    }
}

/// 疾跑只在带「前」的走向上成立（原版 `canStartSprinting` 要求向前输入）。
fn sprint_direction(keys: HeldKeys, direction: WalkDirection) -> Option<SprintDirection> {
    if !keys.sprint {
        return None;
    }
    match direction {
        WalkDirection::Forward => Some(SprintDirection::Forward),
        WalkDirection::ForwardLeft => Some(SprintDirection::ForwardLeft),
        WalkDirection::ForwardRight => Some(SprintDirection::ForwardRight),
        _ => None,
    }
}

fn hit_result(bot: &Client) -> Option<HitResult> {
    bot.get_component::<azalea::interact::pick::HitResultComponent>()
        .map(|hit| (**hit).clone())
}

/// 准星下的方块（没指着方块时为 None）。
fn crosshair_block(inner: &Inner, bot: &Client) -> Option<([i32; 3], String)> {
    let HitResult::Block(block) = hit_result(bot)? else {
        return None;
    };
    if block.miss {
        return None;
    }
    let at = [block.block_pos.x, block.block_pos.y, block.block_pos.z];
    let (name, _) = super::door::read_target_block(inner, at).ok()?;
    Some((at, super::capture::canonical_registry_name(&name)))
}

fn position(bot: &Client) -> Result<[f64; 3], String> {
    bot.try_query_self::<&Position, _>(|position| [position.x, position.y, position.z])
        .map_err(|_| "读不到自身位置".to_owned())
}

fn is_dead(bot: &Client) -> bool {
    bot.get_component::<Health>()
        .is_some_and(|health| health.0 <= 0.0)
}

fn screen_open(bot: &Client) -> bool {
    bot.try_query_self::<&azalea::entity::inventory::Inventory, _>(|inventory| {
        inventory.container_menu.is_some()
    })
    .unwrap_or(false)
}

/// 服务端回声的「正在使用物品」（吃、拉弓、举盾）。
fn using_item(bot: &Client) -> bool {
    bot.get_component::<AbstractLivingUsingItem>()
        .is_some_and(|using| using.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(forward: bool, back: bool, left: bool, right: bool) -> HeldKeys {
        HeldKeys {
            forward,
            back,
            left,
            right,
            ..HeldKeys::default()
        }
    }

    #[test]
    fn opposite_keys_cancel_and_diagonals_combine() {
        assert_eq!(
            walk_direction(keys(true, true, false, false)),
            WalkDirection::None
        );
        assert_eq!(
            walk_direction(keys(true, false, true, true)),
            WalkDirection::Forward
        );
        assert_eq!(
            walk_direction(keys(true, false, false, true)),
            WalkDirection::ForwardRight
        );
        assert_eq!(
            walk_direction(keys(false, true, true, false)),
            WalkDirection::BackwardLeft
        );
        assert_eq!(walk_direction(HeldKeys::default()), WalkDirection::None);
    }

    #[test]
    fn sprint_needs_a_forward_component() {
        let sprinting = |keys: HeldKeys| {
            sprint_direction(
                HeldKeys {
                    sprint: true,
                    ..keys
                },
                walk_direction(keys),
            )
        };
        assert!(matches!(
            sprinting(keys(true, false, true, false)),
            Some(SprintDirection::ForwardLeft)
        ));
        assert!(sprinting(keys(false, true, false, false)).is_none());
        assert!(sprinting(keys(false, false, false, true)).is_none());
        assert!(
            sprint_direction(keys(true, false, false, false), WalkDirection::Forward).is_none(),
            "没按 Ctrl 不疾跑"
        );
    }

    #[test]
    fn ends_when_the_hold_is_full_and_not_before() {
        let quiet = Observed::default();
        assert_eq!(decide_end(10, 1, 10, quiet), None);
        assert_eq!(decide_end(10, 1, 11, quiet), Some(InputEnd::Elapsed));
        assert_eq!(decide_end(10, 40, 49, quiet), None);
        assert_eq!(decide_end(10, 40, 50, quiet), Some(InputEnd::Elapsed));
    }

    #[test]
    fn death_and_screens_outrank_breaking_and_time() {
        let all = Observed {
            dead: true,
            screen_open: true,
            block_broke: true,
        };
        assert_eq!(decide_end(0, 1, 99, all), Some(InputEnd::Died));
        let screen_and_break = Observed { dead: false, ..all };
        assert_eq!(
            decide_end(0, 1, 99, screen_and_break),
            Some(InputEnd::ScreenOpened)
        );
        let broke = Observed {
            block_broke: true,
            ..Observed::default()
        };
        assert_eq!(decide_end(0, 40, 5, broke), Some(InputEnd::BlockBroken));
    }
}
