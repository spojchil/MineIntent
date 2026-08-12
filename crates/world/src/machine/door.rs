//! 写口：动词的类型与它们在 tick 回调内的执行。
//!
//! 中间层各门（motion/hand/screens）把意图汇成 [`DoorCommand`] 排进队列，
//! 由连接机器在客户端事件回调里执行——ECS 只在那里被触碰，不跨线程直写。
//!
//! [`run_command`] 的 `Err` 是机器的**如实拒绝**（原版规则不允许、目标不在
//! 追踪范围内等），原文回到工具面转达给模型，不在这里改写成好听的说法。

use std::sync::atomic::Ordering;

use azalea::entity::{LoadedBy, LookDirection, Position};
use azalea::pathfinder::goals::BlockPosGoal;
use azalea::pathfinder::PathfinderClientExt;
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
            bot.start_goto(BlockPosGoal(BlockPos::new(
                destination[0],
                destination[1],
                destination[2],
            )));
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
            bot.start_goto(BlockPosGoal(target));
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
    }
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
