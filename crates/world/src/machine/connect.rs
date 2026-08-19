//! azalea 接入与连接生命周期。
//!
//! 机器独占一个线程（tokio current_thread + LocalSet，azalea 需要），
//! 对外全部经 [`Inner`] 交流。客户端事件回调是唯一触碰 ECS 的地方：
//! 每个 Tick 里排空写口队列、轮询移动 job、组装并发布快照。
//!
//! 停机路径有一个上游隐患：azalea 在 AppExit 清空 ECS 之后，残留事件处理
//! 可能在持 ECS 写锁时重入读锁而自死锁。此时本线程卡死，`Module::stop`
//! 的合流超时会如实报错，进程退出时由操作系统回收。

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use azalea::accept_resource_packs::AcceptResourcePacksPlugin;
use azalea::app::{App, AppExit, Plugin, PluginGroup, Update};
use azalea::auto_reconnect::AutoReconnectPlugin;
use azalea::auto_respawn::AutoRespawnPlugin;
use azalea::bot::DefaultBotPlugins;
use azalea::ecs::message::MessageWriter;
use azalea::ecs::system::Res;
use azalea::prelude::{bevy_ecs, Account, Component, Resource};
use azalea::protocol::address::{ResolvedAddr, ServerAddr};
use azalea::protocol::packets::game::ClientboundGamePacket;
use azalea::swarm::{DefaultSwarmPlugins, Swarm, SwarmBuilder, SwarmEvent};
use azalea::world::WorldName;
use azalea::{Client, DefaultPlugins, Event};

use super::capture::assemble_snapshot;
use super::door::{run_command, PendingCommand};
use super::movement::poll_movement_job;
use super::state::Inner;
use super::ConnectionConfig;
use crate::ConnectionPhase;

#[derive(Clone, Component)]
struct BotState {
    inner: Arc<Inner>,
}

impl Default for BotState {
    fn default() -> Self {
        Self {
            inner: Arc::new(Inner::new()),
        }
    }
}

#[derive(Clone, Resource)]
struct SwarmState {
    inner: Arc<Inner>,
}

impl Default for SwarmState {
    fn default() -> Self {
        Self {
            inner: Arc::new(Inner::new()),
        }
    }
}

/// 在 azalea 自己的 schedule 内发送退出消息，避免跨任务写消息与
/// Bevy 双缓冲时序竞争。
struct MachineShutdownPlugin;

impl Plugin for MachineShutdownPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, emit_app_exit_when_stopping);
    }
}

fn emit_app_exit_when_stopping(mut app_exit: MessageWriter<AppExit>, state: Res<SwarmState>) {
    if state.inner.stopping.load(Ordering::Acquire) {
        app_exit.write(AppExit::Success);
    }
}

pub(super) async fn run_swarm(inner: Arc<Inner>, config: ConnectionConfig) {
    let socket: SocketAddr = match format!("{}:{}", config.host, config.port).parse() {
        Ok(socket) => socket,
        Err(error) => {
            inner.publish_phase(ConnectionPhase::Disconnected {
                reason: format!("服务器地址无效：{error}"),
            });
            return;
        }
    };
    let address = ResolvedAddr {
        server: ServerAddr::from(socket),
        socket,
    };
    let account = Account::offline(&config.username);
    let bot_state = BotState {
        inner: inner.clone(),
    };
    let swarm_state = SwarmState {
        inner: inner.clone(),
    };
    let plugins = (
        DefaultPlugins.build(),
        // 2026-08-17 关掉自动重生（此前为「没有复活路径时死亡即永久」而保留）：
        // 复活成了模型自己的决定（presence 工具），自动重生会把那个决定抢走。
        //
        // 附带效果值得记一笔：自动重生在时，死亡瞬间的 14→0→20 压在一个 tick
        // 里，每 tick 采样的 `SelfState.alive` 几乎必然错过 false（state.rs 的
        // track_health 为此把伤害改成了包驱动）。关掉之后死亡是**持续状态**，
        // alive 采得到，"你已经死亡。"这行体征才真正到得了模型面前。
        DefaultBotPlugins
            .build()
            .disable::<AcceptResourcePacksPlugin>()
            .disable::<AutoReconnectPlugin>()
            .disable::<AutoRespawnPlugin>(),
        MachineShutdownPlugin,
        DefaultSwarmPlugins,
    );
    let shutdown = inner.clone();
    let start = SwarmBuilder::new_without_plugins()
        .add_plugins(plugins)
        .set_handler(handle_client)
        .set_swarm_handler(handle_swarm)
        .set_swarm_state(swarm_state)
        .add_account_with_state(account, bot_state)
        .reconnect_after(None)
        .start(&address);
    tokio::select! {
        _ = start => {}
        _ = shutdown.shutdown.notified() => {
            // 先让 SwarmBuilder 的 AppExit 路径清理；select 丢弃 start future 后
            // 由本线程 runtime 回收剩余任务。
            // 已知风险（上游未修）：azalea 在 AppExit 清空 ECS 后，残留事件
            // 处理可在持 ECS 写锁时重入读锁（username 路径）自死锁。此时本
            // 线程卡死，Module::stop 的 10 秒合流超时会如实报错；companion
            // 进程退出时由操作系统回收该线程。嵌入式使用者需自担此泄漏。
        }
    }
}

async fn handle_swarm(_swarm: Swarm, event: SwarmEvent, state: SwarmState) {
    if let SwarmEvent::Disconnect(_account, _join_opts, _token) = event {
        // 重连政策 Never：断线是要保持的事实，不是要修复的故障。
        if !state.inner.stopping.load(Ordering::Acquire) {
            state.inner.publish_phase(ConnectionPhase::Disconnected {
                reason: "与服务器的连接已断开".to_owned(),
            });
        }
        state.inner.fail_all_pending_chat("连接已断开");
    }
}

async fn handle_client(bot: Client, event: Event, state: BotState) {
    let inner = &state.inner;
    if inner.stopping.load(Ordering::Acquire) {
        return;
    }
    match event {
        Event::Spawn => {
            let dimension = bot
                .try_query_self::<Option<&WorldName>, _>(|world_name| {
                    world_name.map(ToString::to_string)
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| "minecraft:overworld".to_owned());
            *inner.dimension.lock() = dimension;
            *inner.world_handle.lock() = Some(bot.world());
            if let Some(snapshot) = assemble_snapshot(inner, &bot) {
                inner.publish(snapshot);
            }
        }
        Event::Chat(packet) => {
            let sender = packet
                .sender()
                .map(|username| (username, packet.sender_uuid().map(|uuid| uuid.to_string())));
            inner.push_chat(sender, packet.content());
        }
        Event::Packet(packet) => match &*packet {
            ClientboundGamePacket::SetTime(set_time) => {
                // 时钟表的键是服务端注册表动态 id；原版主世界只有一个昼夜钟，
                // 取首个条目。多时钟服务器的按键解析随 registry 读取落位。
                if let Some(clock) = set_time.clock_updates.values().next() {
                    inner.apply_set_time(clock.total_ticks);
                }
            }
            ClientboundGamePacket::GameEvent(game_event) => {
                inner.apply_game_event(game_event.event, game_event.param);
            }
            ClientboundGamePacket::SetHealth(set_health) => {
                inner.track_health(f64::from(set_health.health));
            }
            // 容器 0 = 玩家物品栏；非 0 = 当时开着的服务端容器（工作台等）。
            // 格位变化直译入窗；是回声还是意外由预期标记判定（state.rs），
            // 投不投由消费方裁。不追踪的容器 id（陈旧包）直接丢。
            ClientboundGamePacket::ContainerSetSlot(set_slot) => {
                let container_id = set_slot.container_id;
                let tracked = container_id == 0
                    || inner
                        .open_screen
                        .lock()
                        .as_ref()
                        .is_some_and(|screen| screen.container_id == container_id);
                if tracked {
                    let (item_name, count) = if set_slot.item_stack.is_empty() {
                        (None, 0)
                    } else {
                        (
                            Some(super::capture::canonical_registry_name(
                                &set_slot.item_stack.kind().to_string(),
                            )),
                            set_slot.item_stack.count() as u32,
                        )
                    };
                    inner.push_inventory_change(container_id, set_slot.slot, item_name, count);
                }
            }
            // /give、拾取等对玩家背包的更新走专用包（不经容器 0 的 SetSlot）。
            // 它的 slot 是背包索引（0-8 快捷、9-35 主背包、36-39 盔甲脚→头、
            // 40 副手），换算成菜单协议号后同窗入账。
            ClientboundGamePacket::SetPlayerInventory(set_inventory) => {
                if let Some(menu_slot) = inventory_index_to_menu_slot(set_inventory.slot) {
                    let (item_name, count) = if set_inventory.contents.is_empty() {
                        (None, 0)
                    } else {
                        (
                            Some(super::capture::canonical_registry_name(
                                &set_inventory.contents.kind().to_string(),
                            )),
                            set_inventory.contents.count() as u32,
                        )
                    };
                    inner.push_inventory_change(0, menu_slot, item_name, count);
                }
            }
            _ => {}
        },
        Event::Disconnect(reason) => {
            inner.publish_phase(ConnectionPhase::Disconnected {
                reason: reason
                    .map(|text| text.to_string())
                    .unwrap_or_else(|| "与服务器的连接已断开".to_owned()),
            });
            inner.fail_all_pending_chat("连接已断开");
        }
        Event::Tick => {
            inner.tick.fetch_add(1, Ordering::AcqRel);
            // 一次性跳跃：跳跃布尔保持了一整 tick，现在放开。
            if inner.jump_reset.swap(false, Ordering::AcqRel) {
                bot.set_jumping(false);
            }
            // 写口动词在 ECS 回调里执行：不跨线程触碰客户端。
            let pending: Vec<PendingCommand> = inner.pending.lock().drain(..).collect();
            for pending_command in pending {
                let outcome = run_command(inner, &bot, pending_command.command);
                let _ = pending_command.ack.send(outcome);
            }
            super::observed::tick(inner, &bot);
            poll_movement_job(inner, &bot);
            super::mining::poll_mining_job(inner, &bot);
            super::pillar::poll_pillar_job(inner, &bot);
            // 与容器组件对账：开/关变迁产屏事实（use_on 触发的服务端开屏
            // 也从这里被看见）。
            let open_screen = bot
                .try_query_self::<&azalea::entity::inventory::Inventory, _>(|inventory| {
                    inventory
                        .container_menu
                        .as_ref()
                        .map(|menu| crate::OpenScreenState {
                            kind: super::capture::menu_kind_name(menu).to_owned(),
                            container_id: inventory.id,
                            title: inventory
                                .container_menu_title
                                .as_ref()
                                .map(ToString::to_string),
                        })
                })
                .ok()
                .flatten();
            inner.track_open_screen(open_screen);
            if let Some(snapshot) = assemble_snapshot(inner, &bot) {
                inner.publish(snapshot);
            }
        }
        _ => {}
    }
}

/// 背包索引 → 玩家物品栏屏的菜单协议号。
/// 背包索引是 `SetPlayerInventory` 与 `/item container.N` 的编号空间。
fn inventory_index_to_menu_slot(index: u32) -> Option<u16> {
    match index {
        0..=8 => Some(36 + index as u16),         // 快捷栏
        9..=35 => Some(index as u16),             // 主背包（两个编号空间重合）
        36..=39 => Some(8 - (index as u16 - 36)), // 盔甲：36脚→8 … 39头→5
        40 => Some(45),                           // 副手
        _ => None,
    }
}
