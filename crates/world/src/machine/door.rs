//! 写口：动词的类型与它们在 tick 回调内的执行。
//!
//! 中间层各门（motion/hand/screens）把意图汇成 [`DoorCommand`] 排进队列，
//! 由连接机器在客户端事件回调里执行——ECS 只在那里被触碰，不跨线程直写。
//!
//! [`run_command`] 的 `Err` 是机器的**如实拒绝**（原版规则不允许、目标不在
//! 追踪范围内等），原文回到工具面转达给模型，不在这里改写成好听的说法。

use std::sync::atomic::Ordering;

use azalea::container::ContainerHandleRef;
use azalea::entity::{LoadedBy, LookDirection, Position};
use azalea::inventory::operations::{ClickOperation, PickupClick, SwapClick, ThrowClick};
use azalea::pathfinder::goals::BlockPosGoal;
use azalea::pathfinder::{PathfinderClientExt, PathfinderOpts};
use azalea::protocol::packets::game::s_player_action;
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
    Mine([i32; 3]),
    UseOnBlock([i32; 3]),
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
    /// 交换当前界面两格（菜单协议号，格空间随开着的界面）。任意两格经
    /// 快捷栏中转三包同 tick 完成（实测原子，见 swap_probe）；一侧在
    /// 快捷栏/副手则原生一包。
    ///
    /// `count`：仅当两格恰有一格为空时有效——从非空格挪这么多个到空格
    /// （拆栈原语：拿起整组→右键放 n 个→余量放回，同 tick 多包，
    /// 动词始末指针为空）。两格都有物品时给 count 是如实拒绝。
    SwapSlots {
        a: u16,
        b: u16,
        count: Option<u32>,
    },
    /// 丢弃整格（屏内 Ctrl+Q 语义）。
    ThrowSlot(u16),
    /// 关闭当前开着的服务端容器（发 ContainerClose 并清本地菜单）。
    CloseContainer,
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
            bot.start_goto_with_opts(
                BlockPosGoal(BlockPos::new(
                    destination[0],
                    destination[1],
                    destination[2],
                )),
                PathfinderOpts::new().allow_mining(false),
            );
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
            let target = BlockPos::new(
                (position.0 + (-yaw.sin()) * blocks).floor() as i32,
                position.1.floor() as i32,
                (position.2 + (-yaw.cos()) * blocks).floor() as i32,
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
        DoorCommand::Mine([x, y, z]) => {
            bot.start_mining(BlockPos::new(x, y, z));
            Ok(())
        }
        DoorCommand::UseOnBlock([x, y, z]) => {
            bot.block_interact(BlockPos::new(x, y, z));
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
            bot.left_click_mine(false);
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
        DoorCommand::SwapSlots { a, b, count: None } => {
            let geometry = active_menu_geometry(bot);
            let clicks = plan_swap(a, b, &geometry)?;
            let mut touched = vec![a, b];
            // 三连包经首个快捷格中转——它也会短暂变动再复位。
            if clicks.len() > 1 {
                touched.push(geometry.hotbar_start);
            }
            inner.mark_expected_slots(&touched);
            let handle = ContainerHandleRef::new(geometry.container_id, bot.clone());
            for click in clicks {
                handle.click(ClickOperation::Swap(click));
            }
            Ok(())
        }
        DoorCommand::SwapSlots {
            a,
            b,
            count: Some(count),
        } => {
            let geometry = active_menu_geometry(bot);
            if a > geometry.max_slot || b > geometry.max_slot {
                return Err(format!(
                    "格号超出当前界面范围（0-{}）：{a}、{b}",
                    geometry.max_slot
                ));
            }
            if a == b {
                return Err("两个格号相同，没有可挪的".to_owned());
            }
            // 挪个数需要知道两格现状：从活动菜单读（服务器已确认的本地镜像）。
            use azalea::entity::inventory::Inventory as InventoryComponent;
            let (count_a, count_b) = bot
                .try_query_self::<&InventoryComponent, _>(|inventory| {
                    let slots = inventory.menu().slots();
                    let at = |slot: u16| {
                        slots
                            .get(usize::from(slot))
                            .map_or(0, |item| item.count().max(0) as u32)
                    };
                    (at(a), at(b))
                })
                .map_err(|_| "读不到物品栏".to_owned())?;
            let (source, target, available) = match (count_a, count_b) {
                (0, 0) => return Err("两格都是空的，没有可挪的".to_owned()),
                (_, 0) => (a, b, count_a),
                (0, _) => (b, a, count_b),
                _ => {
                    return Err(
                        "count 只在一方为空格时可用；两格都有物品时只能整组交换".to_owned()
                    )
                }
            };
            let clicks = plan_count_move(source, target, available, count)?;
            inner.mark_expected_slots(&[a, b]);
            let handle = ContainerHandleRef::new(geometry.container_id, bot.clone());
            for click in clicks {
                handle.click(click);
            }
            Ok(())
        }
        DoorCommand::ThrowSlot(slot) => {
            let geometry = active_menu_geometry(bot);
            if slot > geometry.max_slot {
                return Err(format!(
                    "格号 {slot} 超出当前界面范围（0-{}）",
                    geometry.max_slot
                ));
            }
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

/// 把「从 source 挪 count 个到空格 target」翻译成点击序列（纯函数，可单测）。
///
/// 全挪 = 左键拿起 + 左键放下（两包）；部分挪 = 左键拿起整组 →
/// 右键点 target n 次（每次放一个）→ 左键把余量放回 source。
/// 同 tick 发出，点击序列始末指针都为空。
pub(super) fn plan_count_move(
    source: u16,
    target: u16,
    available: u32,
    count: u32,
) -> Result<Vec<ClickOperation>, String> {
    if count == 0 {
        return Err("count 必须大于 0".to_owned());
    }
    if count > available {
        return Err(format!("格 {source} 只有 {available} 个，挪不了 {count} 个"));
    }
    let left = |slot: u16| ClickOperation::Pickup(PickupClick::Left { slot: Some(slot) });
    let right = |slot: u16| ClickOperation::Pickup(PickupClick::Right { slot: Some(slot) });
    if count == available {
        return Ok(vec![left(source), left(target)]);
    }
    let mut clicks = vec![left(source)];
    clicks.extend((0..count).map(|_| right(target)));
    clicks.push(left(source));
    Ok(clicks)
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

    #[test]
    fn count_moves_are_pickup_place_sequences_with_empty_cursor_at_both_ends() {
        // 全挪：拿起 + 放下两包。
        let plan = plan_count_move(37, 2, 8, 8).unwrap();
        assert_eq!(plan.len(), 2);
        // 部分挪：拿起 + n 次右键放一 + 余量放回。
        let plan = plan_count_move(37, 2, 8, 3).unwrap();
        assert_eq!(plan.len(), 5);
        assert!(matches!(
            plan[0],
            ClickOperation::Pickup(PickupClick::Left { slot: Some(37) })
        ));
        assert!(matches!(
            plan[1],
            ClickOperation::Pickup(PickupClick::Right { slot: Some(2) })
        ));
        assert!(matches!(
            plan[4],
            ClickOperation::Pickup(PickupClick::Left { slot: Some(37) })
        ));
        // 越量与零个如实拒绝。
        assert!(plan_count_move(37, 2, 8, 9).is_err());
        assert!(plan_count_move(37, 2, 8, 0).is_err());
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
