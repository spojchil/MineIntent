//! 键鼠输入的时序：起手转向 → 下一 tick 按键 → 按满时长（或提前结束）→ 全部松开。
//!
//! 为什么按键要晚：准星（`HitResultComponent`）由 azalea 在自己的调度里按当前朝向
//! 重算。转向和按鼠标若在同一刻，按下去的会是转之前指着的东西。我们的 tick 是
//! azalea 经通道异步送来的 `Event::Tick`，处理慢了会积压，连着处理的两个 tick 之间
//! azalea 未必跑过调度——所以不按我们的 tick 数算，等 azalea 自己的 `TicksConnected`
//! 走过转向那一刻再按：那一轮调度跑完，准星已按新朝向重算。
//!
//! 同一时刻只有一次输入在按；新输入顶替旧输入（旧的以 [`InputEnd::Replaced`] 回执）。
//! 判定是纯函数（[`decide_end`]、[`walk_direction`]），副作用只在 [`poll_input`] 与
//! [`release`] 里落。每次输入恰好一个回执：正常结束走 [`finish`]，连接结束时
//! 丢掉发送端，等待方收到「连接已结束」。
//!
//! 回执在本 tick 的快照发布**之后**才交出（[`Finished::send`] 由 tick 收尾调用）：
//! 收到回执的一方紧接着读最新快照拼处境，先交回执会让它读到上一 tick 的世界。
//! 方块碎掉时先松手、下一 tick 再交回执：准星由 Azalea 在下一 tick 才对着
//! 碎后的世界重算，早一 tick 交出，处境里的准星还是刚挖掉的那块。

use azalea::block::{BlockState, BlockTrait};
use azalea::core::hit_result::HitResult;
use azalea::entity::metadata::{AbstractLivingUsingItem, Health};
use azalea::entity::Position;
use azalea::protocol::packets::game::s_player_action;
use azalea::{BlockPos, Client, SprintDirection, WalkDirection};
use tokio::sync::oneshot;

use super::state::Inner;
use crate::{
    BlockUsed, HeldKeys, InputEnd, InputOutcome, InputSpec, LookingAt, MouseButton, Unconfirmed,
};

/// 右键点在方块上之后，等服务端回声最多等到按下后的第几 tick。
///
/// 开不开界面（箱子、工作台……）、拉杆拨没拨过去是服务端说了算，客户端判断不了；
/// 高延迟的服务器上回声可能晚到好几 tick。等，但不无限等：超时如实说没等到，
/// 界面后来开了会作为事件另行送到，方块后来变了画面上看得到。
const SERVER_WAIT_TICKS: u64 = 5;

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
    /// 转向时 azalea 的 `TicksConnected`；它变大之前不按键。
    turned_at: u64,
    /// 真正按下的 tick；`None` = 已转向、等准星刷新。
    pressed_tick: Option<u64>,
    from: [f64; 3],
    pressed_on: Option<LookingAt>,
    broken: Vec<String>,
    /// 上一 tick 准星下的方块。按着左键时它下一 tick 变成空气，就是挖碎了。
    crosshair_block: Option<([i32; 3], String)>,
    placed: Vec<(String, [i32; 3])>,
    /// 已经记进回执的放置序号；按下右键时取当时的，之后序号变大就是又放下了一块。
    placement_seq: u32,
    last_use_tick: u64,
    /// 已松手、等下一 tick 交回执：为什么结束、在哪一 tick 结束。
    ending: Option<(InputEnd, u64)>,
    /// 按下右键时准星下的方块和它当时的状态：用过之后拿来比变了什么。
    pressed_block: Option<(BlockPos, BlockState)>,
    /// 已经记进回执的方块使用序号，规则同 `placement_seq`。
    block_use_seq: u32,
    /// 对 `pressed_block` 用了几次；最近一次是不是只有服务端会改它。
    uses: u32,
    server_use: bool,
    /// 已松手，等服务端的回声。
    awaiting: Option<Awaiting>,
    unconfirmed: Option<Unconfirmed>,
    done: oneshot::Sender<InputOutcome>,
}

/// 松手之后在等的服务端回声：等什么、在哪一 tick 松的、最晚等到哪一 tick。
#[derive(Clone, Copy, Debug)]
struct Awaiting {
    what: Unconfirmed,
    at: u64,
    deadline: u64,
}

/// 已结束、待交出的回执。
pub(super) struct Finished {
    done: oneshot::Sender<InputOutcome>,
    outcome: InputOutcome,
}

impl Finished {
    pub(super) fn send(self) {
        let _ = self.done.send(self.outcome);
    }
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

    let previous = inner.held_input.lock().take();
    if let Some(previous) = previous {
        release(bot, &previous.spec);
        // 已经结束、只差交回执的那次照它自己的结局回执，不算被顶替。
        let (end, at) = previous
            .ending
            .or(previous
                .awaiting
                .map(|awaiting| (InputEnd::Elapsed, awaiting.at)))
            .unwrap_or((InputEnd::Replaced, inner.now_tick()));
        finish(bot, previous, end, at).send();
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
        turned_at: azalea_ticks(bot),
        pressed_tick: None,
        from,
        pressed_on: None,
        broken: Vec::new(),
        crosshair_block: None,
        placed: Vec::new(),
        placement_seq: 0,
        last_use_tick: 0,
        ending: None,
        pressed_block: None,
        block_use_seq: 0,
        uses: 0,
        server_use: false,
        awaiting: None,
        unconfirmed: None,
        done,
    });
    Ok(())
}

/// 每 tick 推进在按的输入。结束了就交出待发的回执，由调用方在发布快照后发送。
pub(super) fn poll_input(inner: &Inner, bot: &Client) -> Option<Finished> {
    let now = inner.now_tick();
    let mut slot = inner.held_input.lock();
    let held = slot.as_mut()?;

    if let Some((end, at)) = held.ending {
        let held = slot.take().expect("上面刚借到 Some");
        return Some(finish(bot, held, end, at));
    }
    if let Some(Awaiting { what, at, deadline }) = held.awaiting {
        let answered = match what {
            Unconfirmed::Screen => screen_open(bot),
            Unconfirmed::BlockUse => block_changed(bot, held),
        };
        let end = if answered && what == Unconfirmed::Screen {
            InputEnd::ScreenOpened
        } else if answered {
            InputEnd::Elapsed
        } else if is_dead(bot) {
            InputEnd::Died
        } else if now >= deadline {
            InputEnd::Elapsed
        } else {
            return None;
        };
        let mut held = slot.take().expect("上面刚借到 Some");
        if !answered && end == InputEnd::Elapsed {
            held.unconfirmed = Some(what);
        }
        return Some(finish(bot, held, end, at));
    }

    let Some(pressed_tick) = held.pressed_tick else {
        if now > held.queued_tick && azalea_ticks(bot) > held.turned_at {
            press(inner, bot, held, now);
        }
        return None;
    };
    collect_placements(bot, held);
    collect_block_uses(bot, held);

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
        release(bot, &held.spec);
        if end == InputEnd::BlockBroken {
            held.ending = Some((end, now));
            return None;
        }
        let changed = block_changed(bot, held);
        if let Some(what) = server_wait(held, end, pressed_tick, now, changed) {
            held.awaiting = Some(Awaiting {
                what,
                at: now,
                deadline: pressed_tick + SERVER_WAIT_TICKS,
            });
            return None;
        }
        let held = slot.take().expect("上面刚借到 Some");
        return Some(finish(bot, held, end, now));
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
    None
}

/// 按满时长松手时，回执要不要再等服务端的回声、等的是什么。
///
/// 只在按下还不到 [`SERVER_WAIT_TICKS`] 时等；按得更久的，服务端早就来得及回了。
/// `changed`：按下时准星下的方块现在变没变。
fn server_wait(
    held: &HeldInput,
    end: InputEnd,
    pressed_tick: u64,
    now: u64,
    changed: bool,
) -> Option<Unconfirmed> {
    if end != InputEnd::Elapsed || now >= pressed_tick + SERVER_WAIT_TICKS {
        None
    } else if awaits_screen(held) {
        Some(Unconfirmed::Screen)
    } else if awaits_block_use(held) && !changed {
        Some(Unconfirmed::BlockUse)
    } else {
        None
    }
}

/// 右键点在会开界面的方块上、什么也没放下：服务端可能正要开界面，回执要等它。
fn awaits_screen(held: &HeldInput) -> bool {
    let Some(LookingAt::Block { name, .. }) = &held.pressed_on else {
        return false;
    };
    held.spec.mouse == Some(MouseButton::Right)
        // 潜行时原版不碰方块的交互，直接用手里的东西（手里有东西时）；保守起见潜行就不等。
        && !held.spec.keys.sneak
        && opens_menu(name)
        && held.placed.is_empty()
}

/// 只用了一次、只有服务端会改的方块（拉杆、音符盒……）：等它的方块更新。
/// 用了好几次的不等——来回拨几下之后「没变」说明不了服务端回没回。
fn awaits_block_use(held: &HeldInput) -> bool {
    held.server_use && held.uses == 1
}

/// 右键会让服务端开界面的方块（注册名，不带命名空间）。
///
/// 按 26.1.2 反编译：`world/level/block` 下 `useWithoutItem` 里 `openMenu` 的方块类——
/// 铁砧、木桶、信标、高炉、酿造台、制图台、箱子（含陷阱箱）、合成器、工作台、发射器
/// （含投掷器）、附魔台、末影箱、熔炉、砂轮、漏斗、讲台、织布机、潜影盒、锻造台、
/// 烟熏炉、切石机。箱子上方被挡、潜影盒开口被挡时服务端也不开，那时就是等到超时。
fn opens_menu(name: &str) -> bool {
    const MENU_BLOCKS: &[&str] = &[
        "anvil",
        "chipped_anvil",
        "damaged_anvil",
        "barrel",
        "beacon",
        "blast_furnace",
        "brewing_stand",
        "cartography_table",
        "chest",
        "trapped_chest",
        "crafter",
        "crafting_table",
        "dispenser",
        "dropper",
        "enchanting_table",
        "ender_chest",
        "furnace",
        "grindstone",
        "hopper",
        "lectern",
        "loom",
        "smithing_table",
        "smoker",
        "stonecutter",
    ];
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    MENU_BLOCKS.contains(&name) || name.ends_with("shulker_box")
}

/// 连接结束：松开并丢掉发送端，等待方如实收到「连接已结束」。
pub(super) fn connection_ended(inner: &Inner) {
    inner.held_input.lock().take();
}

/// azalea 自己跑过的游戏 tick 数（准星在它的调度里重算）。
fn azalea_ticks(bot: &Client) -> u64 {
    bot.get_component::<azalea::tick_counter::TicksConnected>()
        .map_or(0, |ticks| ticks.0)
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
            held.placement_seq = latest_placement(bot).map_or(0, |last| last.seq);
            held.block_use_seq = latest_block_use(bot).map_or(0, |last| last.seq);
            held.pressed_block = match hit_result(bot) {
                Some(HitResult::Block(hit)) if !hit.miss => {
                    block_state(bot, hit.block_pos).map(|state| (hit.block_pos, state))
                }
                _ => None,
            };
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
        Some(sprint) => bot.sprint(sprint),
        None => bot.walk(direction),
    }
    bot.set_jumping(keys.jump);
    bot.set_crouching(keys.sneak);
}

/// 收掉 Azalea 的活跃/排队挖掘状态：只摘「自动挖准星」组件不够，正在挖的那一块
/// 还会接着挖。停挖要发真事件（`AbortDestroyBlock` 由它触发），并在同一把锁里摘掉
/// `MiningQueued`，否则已经取消的请求下一 tick 仍会开始挖。
pub(super) fn stop_mining(bot: &Client) {
    let mut ecs = bot.ecs.write();
    let is_active = ecs.get::<azalea::mining::Mining>(bot.entity).is_some();
    ecs.entity_mut(bot.entity)
        .remove::<azalea::mining::MiningQueued>();
    // 守卫：Azalea 的停挖处理器在没挖时会 panic（`MineBlockPos` 内层 expect），只有真在挖才发。
    if is_active {
        ecs.write_message(azalea::mining::StopMiningBlockEvent { entity: bot.entity });
    }
}

/// 松开这次输入按下的一切。没按的也一并归零：身体上不该留着任何按键。
fn release(bot: &Client, spec: &InputSpec) {
    bot.walk(WalkDirection::None);
    bot.set_jumping(false);
    bot.set_crouching(false);
    match spec.mouse {
        Some(MouseButton::Left) => {
            bot.left_click_mine(false);
            stop_mining(bot);
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

/// 右键预测出的放置（Azalea 按原版客户端逻辑判断；原版这时已经把方块放进了
/// 自己的世界）。序号比上次记的大就是新放下的。
fn collect_placements(bot: &Client, held: &mut HeldInput) {
    if held.spec.mouse != Some(MouseButton::Right) {
        return;
    }
    let Some(latest) = latest_placement(bot) else {
        return;
    };
    if latest.seq > held.placement_seq {
        held.placement_seq = latest.seq;
        let pos = latest.placement.pos;
        held.placed.push((
            super::capture::canonical_registry_name(&latest.placement.block.to_string()),
            [pos.x, pos.y, pos.z],
        ));
    }
}

fn latest_placement(bot: &Client) -> Option<azalea::interact::PredictedPlacement> {
    bot.get_component::<azalea::interact::PredictedPlacement>()
        .map(|placement| *placement)
}

/// 右键预测出的方块使用（Azalea 按原版客户端逻辑判断；门这类原版客户端当场就改的，
/// Azalea 也已经改进了自己的世界）。只记按下时准星下那一块的。
fn collect_block_uses(bot: &Client, held: &mut HeldInput) {
    if held.spec.mouse != Some(MouseButton::Right) {
        return;
    }
    let Some(latest) = latest_block_use(bot) else {
        return;
    };
    if latest.seq > held.block_use_seq {
        held.block_use_seq = latest.seq;
        if held.pressed_block.is_some_and(|(pos, _)| pos == latest.pos) {
            held.uses += 1;
            held.server_use = latest.block_use == azalea::interact::predict::BlockUse::Server;
        }
    }
}

fn latest_block_use(bot: &Client) -> Option<azalea::interact::PredictedBlockUse> {
    bot.get_component::<azalea::interact::PredictedBlockUse>()
        .map(|block_use| (*block_use).clone())
}

fn block_state(bot: &Client, pos: BlockPos) -> Option<BlockState> {
    bot.world().read().get_block_state(pos)
}

/// 按下时准星下那一块现在和按下时不一样了。
fn block_changed(bot: &Client, held: &HeldInput) -> bool {
    held.pressed_block
        .is_some_and(|(pos, before)| block_state(bot, pos).is_some_and(|now| now != before))
}

/// 两个方块状态差在哪：（属性、之前、现在）。整个方块换了时只报 `block`。
fn state_changes(before: BlockState, after: BlockState) -> Vec<(String, String, String)> {
    let before: Box<dyn BlockTrait> = Box::from(before);
    let after: Box<dyn BlockTrait> = Box::from(after);
    if before.id() != after.id() {
        return vec![(
            "block".to_owned(),
            super::capture::canonical_registry_name(before.id()),
            super::capture::canonical_registry_name(after.id()),
        )];
    }
    let old = before.property_map();
    let mut changes: Vec<(String, String, String)> = after
        .property_map()
        .into_iter()
        .filter(|(name, value)| old.get(name) != Some(value))
        .map(|(name, value)| {
            (
                name.to_owned(),
                old.get(name).copied().unwrap_or_default().to_owned(),
                value.to_owned(),
            )
        })
        .collect();
    changes.sort();
    changes
}

fn finish(bot: &Client, mut held: HeldInput, ended: InputEnd, now: u64) -> Finished {
    collect_placements(bot, &mut held);
    collect_block_uses(bot, &mut held);
    let used = match held.pressed_block {
        Some((pos, before)) if held.uses > 0 => {
            let after = block_state(bot, pos).unwrap_or(before);
            let named: Box<dyn BlockTrait> = Box::from(before);
            Some(BlockUsed {
                block: super::capture::canonical_registry_name(named.id()),
                position: [pos.x, pos.y, pos.z],
                times: held.uses,
                changes: state_changes(before, after),
            })
        }
        _ => None,
    };
    // 按得久、松手时已过了等待期：服务端该回早回了，没变就是没等到。
    if held.unconfirmed.is_none()
        && ended == InputEnd::Elapsed
        && awaits_block_use(&held)
        && used.as_ref().is_some_and(|used| used.changes.is_empty())
    {
        held.unconfirmed = Some(Unconfirmed::BlockUse);
    }
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
        mouse: held.spec.mouse,
        pressed_on: held.pressed_on,
        broken: held.broken,
        placed: held.placed,
        used,
        unconfirmed: held.unconfirmed,
    };
    Finished {
        done: held.done,
        outcome,
    }
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

    fn right_click_on(target: Option<LookingAt>) -> HeldInput {
        let (done, _) = oneshot::channel();
        HeldInput {
            spec: InputSpec {
                keys: HeldKeys::default(),
                mouse: Some(MouseButton::Right),
                turn: None,
                ticks: 1,
            },
            queued_tick: 0,
            turned_at: 0,
            pressed_tick: Some(10),
            from: [0.0; 3],
            pressed_on: target,
            broken: Vec::new(),
            crosshair_block: None,
            placed: Vec::new(),
            placement_seq: 0,
            last_use_tick: 10,
            ending: None,
            pressed_block: None,
            block_use_seq: 0,
            uses: 0,
            server_use: false,
            awaiting: None,
            unconfirmed: None,
            done,
        }
    }

    fn waits(held: &HeldInput, now: u64) -> Option<Unconfirmed> {
        server_wait(held, InputEnd::Elapsed, 10, now, false)
    }

    /// 只有右键点在方块上、没放下东西、按下不到 5 tick 时才等服务端开界面。
    #[test]
    fn waits_for_a_screen_only_after_a_short_right_click_on_a_block() {
        let chest = Some(LookingAt::Block {
            name: "chest".to_owned(),
            position: [0, 64, 0],
            face: "north".to_owned(),
        });
        let mut held = right_click_on(chest.clone());
        assert_eq!(waits(&held, 11), Some(Unconfirmed::Screen));
        assert_eq!(waits(&held, 15), None, "按满 5 tick 就不再等");
        assert_eq!(
            server_wait(&held, InputEnd::Died, 10, 11, false),
            None,
            "死了就不等"
        );

        held.placed.push(("cobblestone".to_owned(), [0, 65, 0]));
        assert_eq!(waits(&held, 11), None, "放下了方块就是客户端已有结果");

        assert_eq!(waits(&right_click_on(None), 11), None, "没对着方块");

        let mut left = right_click_on(chest.clone());
        left.spec.mouse = Some(MouseButton::Left);
        assert_eq!(waits(&left, 11), None);

        let mut sneaking = right_click_on(chest);
        sneaking.spec.keys.sneak = true;
        assert_eq!(waits(&sneaking, 11), None, "潜行右键不开界面");

        let door = Some(LookingAt::Block {
            name: "oak_door".to_owned(),
            position: [0, 64, 0],
            face: "north".to_owned(),
        });
        assert_eq!(waits(&right_click_on(door), 11), None, "门不开界面，不等");
        assert!(opens_menu("minecraft:lime_shulker_box"));
    }

    /// 只由服务端改的方块用了一次、还没变：等它的方块更新；变了或用了多次就不等。
    #[test]
    fn waits_for_a_server_block_use_only_while_unchanged() {
        let lever = Some(LookingAt::Block {
            name: "lever".to_owned(),
            position: [0, 64, 0],
            face: "north".to_owned(),
        });
        let mut held = right_click_on(lever);
        assert_eq!(waits(&held, 11), None, "预测没说用到了方块");

        held.uses = 1;
        held.server_use = true;
        assert_eq!(waits(&held, 11), Some(Unconfirmed::BlockUse));
        assert_eq!(
            server_wait(&held, InputEnd::Elapsed, 10, 11, true),
            None,
            "已经变了"
        );
        assert_eq!(waits(&held, 15), None, "按满 5 tick 就不再等");

        held.uses = 2;
        assert_eq!(waits(&held, 11), None, "来回拨了几下，看不出服务端回没回");

        held.uses = 1;
        held.server_use = false;
        assert_eq!(waits(&held, 11), None, "客户端当场改的不等");
    }

    #[test]
    fn state_changes_name_the_changed_properties() {
        use azalea::block::BlockTrait as _;
        let closed = azalea::block::blocks::OakDoor {
            open: false,
            ..Default::default()
        };
        let mut opened = closed;
        opened.open = true;
        assert_eq!(
            state_changes(closed.as_block_state(), opened.as_block_state()),
            vec![("open".to_owned(), "false".to_owned(), "true".to_owned())]
        );
        assert_eq!(
            state_changes(closed.as_block_state(), azalea::block::BlockState::AIR),
            vec![("block".to_owned(), "oak_door".to_owned(), "air".to_owned())]
        );
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
