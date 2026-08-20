//! 写口：动词的类型与它们在 tick 回调内的执行。
//!
//! 中间层各门（motion/hand/screens）把意图汇成 [`DoorCommand`] 排进队列，
//! 由连接机器在客户端事件回调里执行——ECS 只在那里被触碰，不跨线程直写。
//!
//! [`run_command`] 的 `Err` 是机器的**如实拒绝**（原版规则不允许、目标不在
//! 追踪范围内等），原文回到工具面转达给模型，不在这里改写成好听的说法。

use std::sync::atomic::Ordering;

use azalea::container::ContainerHandleRef;
use azalea::core::direction::Direction;
use azalea::entity::{LoadedBy, LookDirection, Position};
use azalea::inventory::operations::{ClickOperation, PickupClick, SwapClick, ThrowClick};
use azalea::pathfinder::goals::BlockPosGoal;
use azalea::pathfinder::{PathfinderClientExt, PathfinderOpts};
use azalea::protocol::packets::game::s_interact::InteractionHand;
use azalea::protocol::packets::game::s_player_action;
use azalea::protocol::packets::game::s_use_item_on::{BlockHit, ServerboundUseItemOn};
use azalea::respawn::PerformRespawnEvent;
use azalea::{BlockPos, Client, SprintDirection, WalkDirection};
use tokio::sync::oneshot;

use super::state::Inner;

/// 写口动词：中间层各门的意图汇成一条队列，tick 回调内执行
/// （ECS 只在客户端事件回调里触碰，不跨线程直写）。
#[derive(Clone, Debug)]
pub enum DoorCommand {
    /// 一行聊天；`/` 开头由 azalea 按原版语义路由为命令。
    Chat(String),
    GoTo([f64; 3]),
    /// 朝当前面向直走 N 格（化归为寻路目标，机械终止交给寻路器）。
    Forward(f64),
    StopMoving,
    Jump,
    Sneak(bool),
    Sprint(bool),
    LookAt([f64; 3]),
    Face {
        yaw: f64,
        pitch: f64,
    },
    Attack {
        entity_key: String,
    },
    /// 挖掉一格方块（异步持续：挖穿与否由世界变化通知证实）。
    /// 按顺序挖一串方块。**队列**：`start_mining` 是单目标槽，一次只收一块
    /// 会让模型以为在排队、实则每发一次就掐断上一块（2026-08-18 长跑实证）。
    /// 新队列顶替旧队列（与移动的单意图槽同款）。
    Mine(Vec<[i32; 3]>),
    UseOnBlock([i32; 3]),
    /// 把手持方块放到目标空位（目标须紧挨已有方块，被点的是共享面）。
    PlaceBlock([i32; 3]),
    /// 垫柱：跳起来在脚下放方块，站上去，重复 `count` 次。
    PillarUp {
        count: usize,
    },
    UseOnEntity {
        entity_key: String,
    },
    UseItem,
    /// 松手：停止挖掘并松开使用中的物品。
    ReleaseHand,
    DropItem {
        whole_stack: bool,
    },
    SwapOffhand,
    SelectSlot(u8),
    /// 把 `from` 格的东西弄到 `to` 格（菜单协议号，格空间随开着的界面）。
    /// 语义按 `to` 格现状分派：空=移动（count 可拆栈）；同种物品=倒入
    /// 合堆（count 可只倒几个，溢出留原格）；不同物品=整组对调
    /// （count 不适用）。只出格（成品格）只能整组取走、不能倒入。
    /// 全部编排为同 tick 多包点击，动词始末指针为空。
    /// 挪格子。`from`/`to` 是[格位地址](crate::slots)（`hotbar 3`、`pack 0`、
    /// `result`…），不是协议号——协议号只活在这一层以内。
    MoveSlots {
        from: String,
        to: String,
        count: Option<u32>,
    },
    /// 丢弃整格（屏内 Ctrl+Q 语义）。
    ThrowSlot(String),
    /// 关闭当前开着的服务端容器（发 ContainerClose 并清本地菜单）。
    CloseContainer,
    /// 复活。自动重生已关（connect.rs），死亡是持续状态，由模型自己决定何时起来。
    Respawn,
}

pub(super) struct PendingCommand {
    pub(super) command: DoorCommand,
    pub(super) ack: oneshot::Sender<Result<(), String>>,
}

/// 在 tick 回调内执行一个写口动词。Err 是机器的如实拒绝，原文回到工具面。
pub(super) fn run_command(inner: &Inner, bot: &Client, command: DoorCommand) -> Result<(), String> {
    match command {
        DoorCommand::Chat(line) => {
            bot.chat(&line);
            Ok(())
        }
        DoorCommand::GoTo([x, y, z]) => {
            let destination = [x.floor() as i32, y.floor() as i32, z.floor() as i32];
            inner.begin_movement_job(destination);
            // 禁止寻路器隐式挖方块：挖掘是模型的显式动作（hand mine），
            // 不是移动的副作用——实测它会把作为目的地的工作台整个挖掉。
            begin_goto(bot, destination);
            Ok(())
        }
        DoorCommand::Forward(blocks) => {
            // 直走化归为寻路目标：终止条件（到达/受阻）交给寻路器。
            let (position, yaw) = bot
                .try_query_self::<(&Position, &LookDirection), _>(|(position, look)| {
                    (
                        (position.x, position.y, position.z),
                        f64::from(look.y_rot()),
                    )
                })
                .map_err(|_| "读不到自身位置".to_owned())?;
            let yaw = yaw.to_radians();
            // 原版视向量水平分量：x=−sin(yaw)、z=+cos(yaw)（yaw 0=南+z）。
            // 2026-08-17 修正：z 此前取反，「前进」会走成后退。
            let target = BlockPos::new(
                (position.0 + (-yaw.sin()) * blocks).floor() as i32,
                position.1.floor() as i32,
                (position.2 + yaw.cos() * blocks).floor() as i32,
            );
            inner.begin_movement_job([target.x, target.y, target.z]);
            bot.start_goto_with_opts(
                BlockPosGoal(target),
                PathfinderOpts::new().allow_mining(false),
            );
            Ok(())
        }
        DoorCommand::StopMoving => {
            inner.end_movement_job_stopped();
            bot.stop_pathfinding();
            bot.walk(WalkDirection::None);
            Ok(())
        }
        DoorCommand::Jump => {
            bot.set_jumping(true);
            inner.jump_reset.store(true, Ordering::Release);
            Ok(())
        }
        DoorCommand::Sneak(on) => {
            bot.set_crouching(on);
            Ok(())
        }
        DoorCommand::Sprint(on) => {
            if on {
                bot.sprint(SprintDirection::Forward);
            } else {
                // v1 简化：停疾跑=停下。原版疾跑是移动修饰符，细化随运动打磨。
                bot.walk(WalkDirection::None);
            }
            Ok(())
        }
        DoorCommand::LookAt([x, y, z]) => {
            bot.look_at(azalea::Vec3 { x, y, z });
            Ok(())
        }
        DoorCommand::Face { yaw, pitch } => {
            bot.set_direction(yaw as f32, pitch as f32);
            Ok(())
        }
        DoorCommand::Attack { entity_key } => {
            let entity = find_entity_by_key(bot, &entity_key)
                .ok_or_else(|| format!("附近没有 {entity_key} 这个实体"))?;
            bot.attack(entity);
            Ok(())
        }
        DoorCommand::Mine(targets) => {
            if targets.is_empty() {
                return Err("没给要挖的坐标".to_owned());
            }
            // 只校验第一块：后面的等轮到它时再看。中途世界会变（自己挖塌、
            // 别人动土），提前校验全部等于拿过期事实拒绝一个还没发生的动作。
            let [x, y, z] = targets[0];
            let (name, _) = read_target_block(inner, [x, y, z])?;
            if crate::is_air_name(&name) {
                return Err(format!("({x},{y},{z}) 没有方块，是空气"));
            }
            check_reach(bot, BlockPos::new(x, y, z).center())?;
            inner.begin_mining_job(targets.clone());
            begin_mining(bot, targets[0]);
            Ok(())
        }
        DoorCommand::UseOnBlock([x, y, z]) => {
            bot.block_interact(BlockPos::new(x, y, z));
            Ok(())
        }
        DoorCommand::PlaceBlock(at) => place_block(inner, bot, at),
        DoorCommand::PillarUp { count } => {
            use azalea::entity::inventory::Inventory as InventoryComponent;
            if count == 0 {
                return Err("要垫几格？给个大于 0 的数".to_owned());
            }
            // 手里没东西就当场拒绝：在途失败没有措辞的位置，能提前说的就提前说。
            let empty_handed = bot
                .try_query_self::<&InventoryComponent, _>(|inventory| {
                    inventory.held_item().is_empty()
                })
                .map_err(|_| "读不到物品栏".to_owned())?;
            if empty_handed {
                return Err("手里没拿东西，先用 select_slot 选中要垫的方块".to_owned());
            }
            let on_ground = bot
                .try_query_self::<&azalea::entity::Physics, _>(|physics| physics.on_ground())
                .map_err(|_| "读不到自身状态".to_owned())?;
            if !on_ground {
                return Err("人还在空中，落地再垫".to_owned());
            }
            let target = bot
                .try_query_self::<&azalea::entity::Position, _>(|position| {
                    [
                        position.x.floor() as i32,
                        position.y.floor() as i32,
                        position.z.floor() as i32,
                    ]
                })
                .map_err(|_| "读不到自身位置".to_owned())?;
            inner.begin_pillar_job(count, target);
            bot.set_jumping(true);
            inner
                .jump_reset
                .store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        }
        DoorCommand::UseOnEntity { entity_key } => {
            let entity = find_entity_by_key(bot, &entity_key)
                .ok_or_else(|| format!("附近没有 {entity_key} 这个实体"))?;
            bot.entity_interact(entity);
            Ok(())
        }
        DoorCommand::UseItem => {
            bot.start_use_item();
            Ok(())
        }
        DoorCommand::ReleaseHand => {
            // 停挖要发真事件（AbortDestroyBlock 由它触发）。旧实现调的
            // left_click_mine(false) 只摘「自动挖准星」组件——从未装过，
            // 等于停不掉挖掘。守卫：azalea 的停挖处理器在没挖时会 panic
            // （MineBlockPos 内层 expect），只有真在挖才发。
            if bot.get_component::<azalea::mining::Mining>().is_some() {
                bot.ecs
                    .write()
                    .write_message(azalea::mining::StopMiningBlockEvent { entity: bot.entity });
            }
            // 队列也要收：不收的话轮询会不停重发 start_mining，release 等于没停。
            inner.end_mining_job_stopped();
            bot.write_packet(s_player_action::ServerboundPlayerAction {
                action: s_player_action::Action::ReleaseUseItem,
                pos: BlockPos::new(0, 0, 0),
                direction: Default::default(),
                seq: 0,
            });
            Ok(())
        }
        DoorCommand::DropItem { whole_stack } => {
            bot.write_packet(s_player_action::ServerboundPlayerAction {
                action: if whole_stack {
                    s_player_action::Action::DropAllItems
                } else {
                    s_player_action::Action::DropItem
                },
                pos: BlockPos::new(0, 0, 0),
                direction: Default::default(),
                seq: 0,
            });
            Ok(())
        }
        DoorCommand::SwapOffhand => {
            bot.write_packet(s_player_action::ServerboundPlayerAction {
                action: s_player_action::Action::SwapItemWithOffhand,
                pos: BlockPos::new(0, 0, 0),
                direction: Default::default(),
                seq: 0,
            });
            Ok(())
        }
        DoorCommand::SelectSlot(slot) => {
            bot.set_selected_hotbar_slot(slot);
            Ok(())
        }
        DoorCommand::MoveSlots { from, to, count } => {
            let space = active_slot_space(inner, bot);
            let geometry = active_menu_geometry(bot);
            let from = space.resolve(&from)?;
            let to = space.resolve(&to)?;
            if from == to {
                return Err("两个格子是同一个，没有可挪的".to_owned());
            }
            // 语义按两格现状分派：从活动菜单读（服务器已确认的本地镜像）。
            use azalea::entity::inventory::Inventory as InventoryComponent;
            use azalea::inventory::item::MaxStackSizeExt;
            let (from_stack, to_count, same_item) = bot
                .try_query_self::<&InventoryComponent, _>(|inventory| {
                    let slots = inventory.menu().slots();
                    let stack = |slot: u16| {
                        slots.get(usize::from(slot)).and_then(|item| {
                            if item.is_empty() {
                                None
                            } else {
                                Some((
                                    item.kind(),
                                    item.count().max(0) as u32,
                                    item.kind().max_stack_size().max(1) as u32,
                                ))
                            }
                        })
                    };
                    let from_stack = stack(from);
                    let to_stack = stack(to);
                    let same_item = matches!(
                        (&from_stack, &to_stack),
                        (Some((a, _, _)), Some((b, _, _))) if a == b
                    );
                    (
                        from_stack.map(|(_, count, cap)| (count, cap)),
                        to_stack.map(|(_, count, _)| count),
                        same_item,
                    )
                })
                .map_err(|_| "读不到物品栏".to_owned())?;
            let clicks = plan_move(
                MoveEnds {
                    from,
                    to,
                    from_stack,
                    to_count,
                    same_item,
                    from_take_only: slot_is_take_only(bot, from),
                    to_take_only: slot_is_take_only(bot, to),
                },
                count,
                &geometry,
            )?;
            let mut touched = vec![from, to];
            // SWAP 三连包经首个快捷格中转——它也会短暂变动再复位。
            if clicks
                .iter()
                .filter(|click| matches!(click, ClickOperation::Swap(_)))
                .count()
                > 1
            {
                touched.push(geometry.hotbar_start);
            }
            inner.mark_expected_slots(&touched);
            let handle = ContainerHandleRef::new(geometry.container_id, bot.clone());
            for click in clicks {
                handle.click(click);
            }
            Ok(())
        }
        DoorCommand::ThrowSlot(slot) => {
            let geometry = active_menu_geometry(bot);
            let slot = active_slot_space(inner, bot).resolve(&slot)?;
            inner.mark_expected_slots(&[slot]);
            ContainerHandleRef::new(geometry.container_id, bot.clone())
                .click(ClickOperation::Throw(ThrowClick::All { slot }));
            Ok(())
        }
        DoorCommand::CloseContainer => {
            let geometry = active_menu_geometry(bot);
            if geometry.container_id == 0 {
                return Err("没有开着的容器界面".to_owned());
            }
            inner.mark_expected_close();
            ContainerHandleRef::new(geometry.container_id, bot.clone()).close();
            Ok(())
        }
        DoorCommand::Respawn => {
            // 活着时发这个包，服务端 handleClientCommand 见 getHealth() > 0
            // 直接 return——一次静默空操作。与其让工具面收到"成功"，不如
            // 在这里如实拒绝：机器知道自己是死是活。
            let dead = bot
                .get_component::<azalea::entity::metadata::Health>()
                .is_some_and(|health| health.0 <= 0.0);
            if !dead {
                return Err("你还活着，没有可复活的".to_owned());
            }
            bot.ecs
                .write()
                .write_message(PerformRespawnEvent { entity: bot.entity });
            Ok(())
        }
    }
}

/// 三连包中转用的快捷栏按钮（0-8 任选；轮换必复位，不要求为空）。
const TEMP_HOTBAR_BUTTON: u8 = 0;

/// 当前界面的交换几何：容器 id、格号上限、快捷栏起点、副手格（仅玩家屏有）。
pub(super) struct MenuGeometry {
    pub(super) container_id: i32,
    pub(super) max_slot: u16,
    /// 快捷栏首格的菜单号（玩家屏 36；容器屏 = 总格数 − 9）。
    pub(super) hotbar_start: u16,
    /// 副手格的菜单号。只有玩家物品栏屏有（45）。
    pub(super) offhand_slot: Option<u16>,
}

/// 当前屏的格位地址空间。
///
/// 协议号只活在这一层以内——可见面一律用 [`crate::slots`] 那套区名 + 序号。
/// 自有区怎么叫要看容器种类，所以要读 `open_screen`（没开容器就是玩家屏）。
pub(super) fn active_slot_space(inner: &Inner, bot: &Client) -> crate::slots::SlotSpace {
    let geometry = active_menu_geometry(bot);
    let own = match inner.open_screen.lock().as_ref() {
        None => crate::slots::OwnArea::Player,
        Some(open) => match open.kind.as_str() {
            "crafting" => crate::slots::OwnArea::Crafting,
            "furnace" | "blast_furnace" | "smoker" => crate::slots::OwnArea::Furnace,
            // 其余按容器名整片编号（维护者裁定：映射按容器命名）。
            other => crate::slots::OwnArea::Named(other.to_owned()),
        },
    };
    crate::slots::SlotSpace::new(
        geometry.hotbar_start,
        geometry.max_slot,
        geometry.offhand_slot,
        own,
    )
}

/// 玩家物品栏屏的几何（容器读不到时的兜底，也是无容器时的常态）。
fn player_menu_geometry() -> MenuGeometry {
    MenuGeometry {
        container_id: 0,
        max_slot: 45,
        hotbar_start: 36,
        offhand_slot: Some(45),
    }
}

/// 从 ECS 读当前活动菜单的几何。容器开着时按容器格空间，否则玩家屏。
fn active_menu_geometry(bot: &Client) -> MenuGeometry {
    use azalea::entity::inventory::Inventory as InventoryComponent;
    bot.try_query_self::<&InventoryComponent, _>(|inventory| match &inventory.container_menu {
        Some(menu) => {
            let hotbar = menu.hotbar_slots_range();
            MenuGeometry {
                container_id: inventory.id,
                max_slot: (menu.len() - 1) as u16,
                hotbar_start: *hotbar.start() as u16,
                offhand_slot: None,
            }
        }
        None => player_menu_geometry(),
    })
    .unwrap_or_else(|_| player_menu_geometry())
}

/// 把「交换菜单格 a、b」翻译成 SWAP 点击序列（纯函数，可单测）。
///
/// 协议的 SWAP 模式只能拿任意格对快捷栏（按钮 0-8）或副手（按钮 40）换：
/// - 一侧是快捷栏或副手 → 原生一包；
/// - 两侧都不是 → 经首个快捷格三步轮换：a↔中转、b↔中转、a↔中转，
///   同 tick 三包（实测原子且中转格必复位，见 examples/swap_probe.rs）。
pub(super) fn plan_swap(a: u16, b: u16, geometry: &MenuGeometry) -> Result<Vec<SwapClick>, String> {
    let max = geometry.max_slot;
    if a > max || b > max {
        return Err(format!("格号超出当前界面范围（0-{max}）：{a}、{b}"));
    }
    if a == b {
        return Err("两个格号相同，没有可交换的".to_owned());
    }
    let swap_with = |source: u16, button: u8| SwapClick {
        source_slot: source,
        target_slot: button,
    };
    if let Some(button) = swap_button(b, geometry) {
        return Ok(vec![swap_with(a, button)]);
    }
    if let Some(button) = swap_button(a, geometry) {
        return Ok(vec![swap_with(b, button)]);
    }
    Ok(vec![
        swap_with(a, TEMP_HOTBAR_BUTTON),
        swap_with(b, TEMP_HOTBAR_BUTTON),
        swap_with(a, TEMP_HOTBAR_BUTTON),
    ])
}

/// `plan_move` 的两端现状（纯数据，可单测）。
pub(super) struct MoveEnds {
    pub(super) from: u16,
    pub(super) to: u16,
    /// from 格：Some((数量, 堆叠上限))；None=空。
    pub(super) from_stack: Option<(u32, u32)>,
    /// to 格现有数量；None=空。
    pub(super) to_count: Option<u32>,
    /// 两格是否同种物品（都非空时才可能为 true）。
    pub(super) same_item: bool,
    /// 只出不进的格（成品格）。
    pub(super) from_take_only: bool,
    pub(super) to_take_only: bool,
}

/// 把「from 格的东西弄到 to 格」翻译成点击序列（纯函数，可单测）。
///
/// 语义按 to 格现状分派：
/// - **空**：整组走 SWAP（原生/三包中转）；给 count 走拆栈
///   （左键拿起 → 右键放 n → 余量放回）；
/// - **同种物品**：倒入合堆——左键拿起、左键倒入（顶到堆叠上限），
///   溢出/余量左键放回；给 count 用右键逐个倒；
/// - **不同物品**：整组对调（SWAP），count 不适用。
///
/// 只出格规则：成品格不能被倒入；作为来源时不能留余量（服务器会拒绝
/// 放回，指针会悬着）——部分挪/装不下都如实拒绝。
/// 全部同 tick 发出，点击序列始末指针为空。
pub(super) fn plan_move(
    ends: MoveEnds,
    count: Option<u32>,
    geometry: &MenuGeometry,
) -> Result<Vec<ClickOperation>, String> {
    let MoveEnds {
        from,
        to,
        from_stack,
        to_count,
        same_item,
        from_take_only,
        to_take_only,
    } = ends;
    let Some((available, cap)) = from_stack else {
        return Err(format!("格 {from} 是空的，没有可挪的"));
    };
    if let Some(count) = count {
        if count == 0 {
            return Err("count 必须大于 0".to_owned());
        }
        if count > available {
            return Err(format!("格 {from} 只有 {available} 个，挪不了 {count} 个"));
        }
    }
    let left = |slot: u16| ClickOperation::Pickup(PickupClick::Left { slot: Some(slot) });
    let right = |slot: u16| ClickOperation::Pickup(PickupClick::Right { slot: Some(slot) });
    match to_count {
        // 目标为空：整组对调（与空格对调=移动）或拆栈。
        None => match count {
            None => Ok(plan_swap(from, to, geometry)?
                .into_iter()
                .map(ClickOperation::Swap)
                .collect()),
            Some(count) => {
                if from_take_only && count < available {
                    return Err("成品格只能整组取走，不能留余量".to_owned());
                }
                if count == available {
                    return Ok(vec![left(from), left(to)]);
                }
                let mut clicks = vec![left(from)];
                clicks.extend((0..count).map(|_| right(to)));
                clicks.push(left(from));
                Ok(clicks)
            }
        },
        // 同种物品：倒入合堆。
        Some(existing) if same_item => {
            if to_take_only {
                return Err(format!("格 {to} 是成品格，只出不进"));
            }
            let space = cap.saturating_sub(existing);
            if space == 0 {
                return Err(format!("格 {to} 已经满了，倒不进去"));
            }
            let pour = count.unwrap_or_else(|| available.min(space));
            if pour > space {
                return Err(format!("格 {to} 只装得下 {space} 个"));
            }
            let remainder = available - pour;
            if from_take_only && remainder > 0 {
                return Err(format!("格 {to} 装不下全部，而成品格不能留余量"));
            }
            if pour == available {
                // 整组拿起倒入，装得下就没有余量要放回。
                return Ok(vec![left(from), left(to)]);
            }
            match count {
                // 未指定 count：倒到 to 满为止，余量放回。
                None => Ok(vec![left(from), left(to), left(from)]),
                // 指定 count：右键逐个倒，余量放回。
                Some(count) => {
                    let mut clicks = vec![left(from)];
                    clicks.extend((0..count).map(|_| right(to)));
                    clicks.push(left(from));
                    Ok(clicks)
                }
            }
        }
        // 不同物品：整组对调。
        Some(_) => {
            if count.is_some() {
                return Err("两格物品不同，只能整组对调；count 不适用".to_owned());
            }
            Ok(plan_swap(from, to, geometry)?
                .into_iter()
                .map(ClickOperation::Swap)
                .collect())
        }
    }
}

/// 只出不进的格（成品格）。数据随容器种类扩展：玩家物品栏与工作台的
/// 成品格都是 0 号；熔炉族（熔炉/高炉/烟熏炉）的成品格是 2 号；
/// 其余容器落地时在此补行。
fn slot_is_take_only(bot: &Client, slot: u16) -> bool {
    use azalea::entity::inventory::Inventory as InventoryComponent;
    use azalea::inventory::Menu;
    bot.try_query_self::<&InventoryComponent, _>(|inventory| match inventory.menu() {
        Menu::Player(_) | Menu::Crafting { .. } => slot == 0,
        Menu::Furnace { .. } | Menu::BlastFurnace { .. } | Menu::Smoker { .. } => slot == 2,
        _ => false,
    })
    .unwrap_or(false)
}

/// 菜单号能否直接充当 SWAP 的目标按钮：快捷栏 9 格 → 按钮 0-8，
/// 副手（仅玩家屏）→ 按钮 40。
fn swap_button(menu_slot: u16, geometry: &MenuGeometry) -> Option<u8> {
    if geometry.offhand_slot == Some(menu_slot) {
        return Some(40);
    }
    let hotbar = geometry.hotbar_start..=geometry.hotbar_start + 8;
    if hotbar.contains(&menu_slot) {
        return Some((menu_slot - geometry.hotbar_start) as u8);
    }
    None
}

/// 世界模型句柄（Spawn 时登记；未就绪即如实拒绝）。
fn world_handle(
    inner: &Inner,
) -> Result<std::sync::Arc<parking_lot::RwLock<azalea::world::World>>, String> {
    inner
        .world_handle
        .lock()
        .clone()
        .ok_or_else(|| "世界模型尚未就绪".to_owned())
}

/// 读目标格的（方块名，有无碰撞体）。未加载/超高度如实拒绝。
fn read_target_block(inner: &Inner, [x, y, z]: [i32; 3]) -> Result<(String, bool), String> {
    let world = world_handle(inner)?;
    let world = world.read();
    let y64 = i64::from(y);
    let min_y = i64::from(world.chunks.min_y());
    if y64 < min_y || y64 >= min_y + i64::from(world.chunks.height()) {
        return Err(format!("({x},{y},{z}) 在世界高度之外"));
    }
    let Some(state) = world.get_block_state(BlockPos::new(x, y, z)) else {
        return Err(format!("({x},{y},{z}) 还没加载进来，先走近或看向那里"));
    };
    let block: Box<dyn azalea::block::BlockTrait> = Box::from(state);
    Ok((block.id().to_owned(), state_has_collision(state)))
}

/// 这一格有没有碰撞体（可依附）。空气、水、火把、草都算没有。
fn block_has_collision(world: &azalea::world::World, [x, y, z]: [i32; 3]) -> bool {
    world
        .get_block_state(BlockPos::new(x, y, z))
        .is_some_and(state_has_collision)
}

fn state_has_collision(state: azalea::block::BlockState) -> bool {
    use azalea::physics::collision::BlockWithShape;
    !state.collision_shape().to_aabbs().is_empty()
}

/// 眼睛位置与方块触及上限（原版属性，生存默认 4.5 格）。
fn eye_and_reach(bot: &Client) -> Result<(azalea::Vec3, f64), String> {
    use azalea::entity::dimensions::EntityDimensions;
    bot.try_query_self::<(&Position, &EntityDimensions, &azalea::entity::Attributes), _>(
        |(position, dimensions, attributes)| {
            (
                azalea::Vec3 {
                    x: position.x,
                    y: position.y + f64::from(dimensions.eye_height),
                    z: position.z,
                },
                attributes.block_interaction_range.calculate(),
            )
        },
    )
    .map_err(|_| "读不到自身位置".to_owned())
}

/// 目标点在触及范围内吗（按眼到格中心；比服务端的宽容值略严）。
fn check_reach(bot: &Client, target: azalea::Vec3) -> Result<(), String> {
    let (eye, reach) = eye_and_reach(bot)?;
    let distance =
        ((eye.x - target.x).powi(2) + (eye.y - target.y).powi(2) + (eye.z - target.z).powi(2))
            .sqrt();
    if distance > reach {
        return Err(format!(
            "太远了：目标离眼 {distance:.1} 格，触及上限 {reach:.1} 格；先走近"
        ));
    }
    Ok(())
}

/// 依附面选择的结果：（邻块坐标、被点的面、命中点）。
pub(super) type SupportFace = ([i32; 3], Direction, [f64; 3]);

/// 放置的依附面选择（纯函数，可单测）。
///
/// 目标格 T 的六个邻格里找有碰撞体的方块 N，被点的是 N 朝向 T 的那面，
/// 命中点取共享面中心；多个候选时选面法向最朝向眼睛的——人放方块点的
/// 是自己看得见的面。
pub(super) fn pick_support_face(
    target: [i32; 3],
    eye: [f64; 3],
    mut solid_at: impl FnMut([i32; 3]) -> bool,
) -> Option<SupportFace> {
    // 面法向 = 从邻块指向目标格；原版语义：North=-z、South=+z、West=-x、East=+x。
    const FACES: [(Direction, [i32; 3]); 6] = [
        (Direction::Down, [0, -1, 0]),
        (Direction::Up, [0, 1, 0]),
        (Direction::North, [0, 0, -1]),
        (Direction::South, [0, 0, 1]),
        (Direction::West, [-1, 0, 0]),
        (Direction::East, [1, 0, 0]),
    ];
    let center = [
        f64::from(target[0]) + 0.5,
        f64::from(target[1]) + 0.5,
        f64::from(target[2]) + 0.5,
    ];
    let mut best: Option<(f64, SupportFace)> = None;
    for (direction, normal) in FACES {
        let neighbor = [
            target[0] - normal[0],
            target[1] - normal[1],
            target[2] - normal[2],
        ];
        if !solid_at(neighbor) {
            continue;
        }
        let hit = [
            center[0] - 0.5 * f64::from(normal[0]),
            center[1] - 0.5 * f64::from(normal[1]),
            center[2] - 0.5 * f64::from(normal[2]),
        ];
        let score = (eye[0] - hit[0]) * f64::from(normal[0])
            + (eye[1] - hit[1]) * f64::from(normal[1])
            + (eye[2] - hit[2]) * f64::from(normal[2]);
        if best.as_ref().is_none_or(|(top, _)| score > *top) {
            best = Some((score, (neighbor, direction, hit)));
        }
    }
    best.map(|(_, choice)| choice)
}

/// 按快照里的实体键（`{epoch}:{协议id}`）找回 ECS 实体。
pub(super) fn find_entity_by_key(
    bot: &Client,
    entity_key: &str,
) -> Option<azalea::ecs::entity::Entity> {
    let protocol_id: i32 = entity_key.strip_prefix("1:")?.parse().ok()?;
    let mut ecs = bot.ecs.write();
    let mut query = ecs.query::<(
        azalea::ecs::entity::Entity,
        &azalea::core::entity_id::MinecraftEntityId,
        &LoadedBy,
    )>();
    query
        .iter(&ecs)
        .find(|(_, id, loaded_by)| ***id == protocol_id && loaded_by.contains(&bot.entity))
        .map(|(entity, _, _)| entity)
}

/// 开挖一块：挖什么看什么（原版机制——azalea 在事件处理时若发现视线正落在
/// 目标上会用真实命中面，否则填 Down 兜底），然后交给 azalea 持续挖。
/// 把手持方块放到目标空位。
///
/// 抽出来是因为**垫柱要在机器持有的时序里调它**（跳起来、升过那一格、再放），
/// 那个时序模型表达不了。判定与发包口径只有这一处。
pub(super) fn place_block(inner: &Inner, bot: &Client, [x, y, z]: [i32; 3]) -> Result<(), String> {
    use azalea::entity::inventory::Inventory as InventoryComponent;
    let empty_handed = bot
        .try_query_self::<&InventoryComponent, _>(|inventory| inventory.held_item().is_empty())
        .map_err(|_| "读不到物品栏".to_owned())?;
    if empty_handed {
        return Err("手里没拿东西，先用 select_slot 选中要放的方块".to_owned());
    }
    let (name, _) = read_target_block(inner, [x, y, z])?;
    if !crate::is_air_name(&name) {
        return Err(format!("({x},{y},{z}) 已经有方块：{name}"));
    }
    let (eye, _) = eye_and_reach(bot)?;
    check_reach(bot, BlockPos::new(x, y, z).center())?;
    let support = {
        let world = world_handle(inner)?;
        let world = world.read();
        pick_support_face([x, y, z], [eye.x, eye.y, eye.z], |position| {
            block_has_collision(&world, position)
        })
    };
    let Some((neighbor, direction, hit)) = support else {
        return Err(format!(
            "({x},{y},{z}) 六面都没有可依附的方块，放不上去；先在旁边放好落脚块"
        ));
    };
    let location = azalea::Vec3 {
        x: hit[0],
        y: hit[1],
        z: hit[2],
    };
    // 人放方块是看着依附面点右键：看向命中点，然后自己构造真实命中
    // 发包。不走 block_interact——它在视线没落到目标时会伪造
    // 「中心点+Up 面」，放置会歪到依附块顶上。
    bot.look_at(location);
    let seq = bot
        .try_query_self::<&mut azalea::interact::BlockStatePredictionHandler, _>(|mut handler| {
            handler.start_predicting()
        })
        .unwrap_or_default();
    bot.write_packet(ServerboundUseItemOn {
        hand: InteractionHand::MainHand,
        block_hit: BlockHit {
            block_pos: BlockPos::new(neighbor[0], neighbor[1], neighbor[2]),
            direction,
            location,
            inside: false,
            world_border: false,
        },
        seq,
    });
    Ok(())
}

/// 下一个寻路目标。**禁止寻路器隐式挖方块**：挖掘是模型的显式动作（hand mine），
/// 不是移动的副作用——实测它会把作为目的地的工作台整个挖掉。
///
/// 多段行走每一程都经这里重发，所以口径只有一处。
pub(super) fn begin_goto(bot: &Client, [x, y, z]: [i32; 3]) {
    bot.start_goto_with_opts(
        BlockPosGoal(BlockPos::new(x, y, z)),
        PathfinderOpts::new().allow_mining(false),
    );
}

pub(super) fn begin_mining(bot: &Client, [x, y, z]: [i32; 3]) {
    let target = BlockPos::new(x, y, z);
    bot.look_at(target.center());
    bot.start_mining(target);
}

/// 某格现在是不是空气。挖碎的唯一判据——不按时间猜。
pub(super) fn block_is_air(inner: &Inner, at: [i32; 3]) -> Result<bool, String> {
    read_target_block(inner, at).map(|(name, _)| crate::is_air_name(&name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crafting_menu_geometry() -> MenuGeometry {
        // 工作台屏：0 成品、1-9 摆料、10-36 主背包、37-45 快捷栏、无副手。
        MenuGeometry {
            container_id: 7,
            max_slot: 45,
            hotbar_start: 37,
            offhand_slot: None,
        }
    }

    #[test]
    fn support_face_uses_the_only_solid_neighbor() {
        // 只有脚下有地面：依附块是下邻格，被点的是它的顶面，命中点在共享面中心。
        let picked = pick_support_face([0, 64, 0], [0.5, 65.6, 0.5], |position| {
            position == [0, 63, 0]
        });
        assert_eq!(picked, Some(([0, 63, 0], Direction::Up, [0.5, 64.0, 0.5])));
    }

    #[test]
    fn support_face_prefers_the_face_visible_from_the_eye() {
        let solid = |position: [i32; 3]| position == [0, 63, 0] || position == [1, 64, 0];
        // 眼在西侧：东邻墙的西面正对眼睛，胜过只擦边的顶面。
        let picked = pick_support_face([0, 64, 0], [-2.5, 64.5, 0.5], solid);
        assert_eq!(
            picked,
            Some(([1, 64, 0], Direction::West, [1.0, 64.5, 0.5]))
        );
        // 眼在东侧：看不见那面西墙（法向背对眼睛），退回选顶面。
        let picked = pick_support_face([0, 64, 0], [3.5, 64.5, 0.5], solid);
        assert_eq!(picked, Some(([0, 63, 0], Direction::Up, [0.5, 64.0, 0.5])));
    }

    #[test]
    fn support_face_is_none_when_floating() {
        assert_eq!(
            pick_support_face([0, 64, 0], [0.5, 65.6, 0.5], |_| false),
            None
        );
    }

    #[test]
    fn swap_plans_follow_the_protocol_swap_constraints() {
        let player = player_menu_geometry();
        // 一侧在快捷栏：原生一包，按钮 = 菜单号 − 36。
        let plan = plan_swap(10, 38, &player).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!((plan[0].source_slot, plan[0].target_slot), (10, 2));
        // 副手按钮 40；快捷栏在 a 侧同样一包。
        let plan = plan_swap(45, 20, &player).unwrap();
        assert_eq!((plan[0].source_slot, plan[0].target_slot), (20, 40));
        let plan = plan_swap(44, 5, &player).unwrap();
        assert_eq!((plan[0].source_slot, plan[0].target_slot), (5, 8));
        // 两侧都不在快捷栏/副手：经快捷格 0 三步轮换。
        let plan = plan_swap(10, 20, &player).unwrap();
        let steps: Vec<(u16, u8)> = plan
            .iter()
            .map(|click| (click.source_slot, click.target_slot))
            .collect();
        assert_eq!(steps, vec![(10, 0), (20, 0), (10, 0)]);
        // 越界与同格拒绝。
        assert!(plan_swap(46, 0, &player).is_err());
        assert!(plan_swap(0, 99, &player).is_err());
        assert!(plan_swap(7, 7, &player).is_err());
    }

    fn ends(
        from: u16,
        to: u16,
        from_stack: Option<(u32, u32)>,
        to_count: Option<u32>,
        same_item: bool,
    ) -> MoveEnds {
        MoveEnds {
            from,
            to,
            from_stack,
            to_count,
            same_item,
            from_take_only: false,
            to_take_only: false,
        }
    }

    #[test]
    fn move_to_empty_is_swap_or_split() {
        let geometry = player_menu_geometry();
        // 整组：走 SWAP（一侧快捷栏=原生一包）。
        let plan = plan_move(ends(10, 38, Some((8, 64)), None, false), None, &geometry).unwrap();
        assert_eq!(plan.len(), 1);
        assert!(matches!(plan[0], ClickOperation::Swap(_)));
        // 拆栈：拿起 + n 右键 + 余量放回。
        let plan = plan_move(ends(37, 2, Some((8, 64)), None, false), Some(3), &geometry).unwrap();
        assert_eq!(plan.len(), 5);
        assert!(matches!(
            plan[0],
            ClickOperation::Pickup(PickupClick::Left { slot: Some(37) })
        ));
        assert!(matches!(
            plan[4],
            ClickOperation::Pickup(PickupClick::Left { slot: Some(37) })
        ));
        // count=全部：拿起+放下两包。
        let plan = plan_move(ends(37, 2, Some((8, 64)), None, false), Some(8), &geometry).unwrap();
        assert_eq!(plan.len(), 2);
        // 越量/零个/空来源如实拒绝。
        assert!(plan_move(ends(37, 2, Some((8, 64)), None, false), Some(9), &geometry).is_err());
        assert!(plan_move(ends(37, 2, Some((8, 64)), None, false), Some(0), &geometry).is_err());
        assert!(plan_move(ends(37, 2, None, None, false), None, &geometry).is_err());
    }

    #[test]
    fn merge_pours_into_same_item_respecting_the_stack_cap() {
        let geometry = player_menu_geometry();
        // 装得下：拿起 + 倒入两包。
        let plan = plan_move(ends(10, 20, Some((8, 64)), Some(40), true), None, &geometry).unwrap();
        assert_eq!(plan.len(), 2);
        // 装不下全部：倒到满，余量放回（三包）。
        let plan = plan_move(
            ends(10, 20, Some((30, 64)), Some(50), true),
            None,
            &geometry,
        )
        .unwrap();
        assert_eq!(plan.len(), 3);
        // 指定 count：右键逐个倒。
        let plan = plan_move(
            ends(10, 20, Some((8, 64)), Some(40), true),
            Some(2),
            &geometry,
        )
        .unwrap();
        assert_eq!(plan.len(), 4);
        // 目标已满 / count 超过剩余空间：如实拒绝。
        assert!(plan_move(ends(10, 20, Some((8, 64)), Some(64), true), None, &geometry).is_err());
        assert!(plan_move(
            ends(10, 20, Some((30, 64)), Some(60), true),
            Some(5),
            &geometry
        )
        .is_err());
    }

    #[test]
    fn different_items_only_do_whole_exchange() {
        let geometry = player_menu_geometry();
        let plan = plan_move(ends(10, 20, Some((8, 64)), Some(3), false), None, &geometry).unwrap();
        assert_eq!(plan.len(), 3, "两侧都不在快捷栏：三包中转对调");
        assert!(plan_move(
            ends(10, 20, Some((8, 64)), Some(3), false),
            Some(2),
            &geometry
        )
        .is_err());
    }

    #[test]
    fn take_only_slots_never_keep_a_remainder_and_never_accept() {
        let geometry = crafting_menu_geometry();
        let take_only_source = |to_count: Option<u32>, same: bool| MoveEnds {
            from: 0,
            to: 40,
            from_stack: Some((4, 64)),
            to_count,
            same_item: same,
            from_take_only: true,
            to_take_only: false,
        };
        // 整组取走（目标空）：合法。
        assert!(plan_move(take_only_source(None, false), None, &geometry).is_ok());
        assert!(plan_move(take_only_source(None, false), Some(4), &geometry).is_ok());
        // 部分取走：拒绝（余量放不回成品格）。
        assert!(plan_move(take_only_source(None, false), Some(2), &geometry).is_err());
        // 倒入同种但装不下全部：拒绝。
        assert!(plan_move(take_only_source(Some(63), true), None, &geometry).is_err());
        // 往成品格里倒：拒绝。
        let into_result = MoveEnds {
            from: 40,
            to: 0,
            from_stack: Some((4, 64)),
            to_count: Some(4),
            same_item: true,
            from_take_only: false,
            to_take_only: true,
        };
        assert!(plan_move(into_result, None, &geometry).is_err());
    }

    #[test]
    fn crafting_menu_swaps_use_its_own_hotbar_and_have_no_offhand() {
        let crafting = crafting_menu_geometry();
        // 工作台屏快捷栏 37-45：按钮 = 菜单号 − 37。取成品 = swap(0, 快捷格)。
        let plan = plan_swap(0, 45, &crafting).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!((plan[0].source_slot, plan[0].target_slot), (0, 8));
        // 玩家屏里 45 是副手按钮 40；工作台屏里 45 是快捷栏末格——几何决定按钮。
        let plan = plan_swap(20, 37, &crafting).unwrap();
        assert_eq!((plan[0].source_slot, plan[0].target_slot), (20, 0));
        // 摆料 ↔ 主背包：两侧都不在快捷栏，经快捷首格（按钮 0）三步轮换。
        let plan = plan_swap(3, 15, &crafting).unwrap();
        assert_eq!(plan.len(), 3);
        assert!(plan.iter().all(|click| click.target_slot == 0));
    }
}
