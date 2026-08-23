//! 唤醒判据的纯函数部分：从一帧快照里挑出「该把同伴叫起来」的事实。
//!
//! **这是脚手架，不是判据。** 正式的关注清单未裁（issue #135），
//! 这里只做最朴素的三条：别人对我说话、我受伤了、我下的移动任务有果了。
//!
//! 抽成纯函数的理由：它是组合根里唯一有分支的逻辑，而组合根跑起来要一台
//! Minecraft 服务器和一个模型端点。分支判断不该只能靠实盘验证。
//!
//! 三个窗共用一条单调 `seq`，所以三个游标互不干扰，且「同一 tick 内多条」
//! 不会漏——tick 会重复，seq 不会。

use world::{
    DamageEntry, FactSource, InventoryChangeEntry, JobEntry, JobFact, MineEvent, MoveEvent,
    ScreenEvent, TickSnapshot,
};

/// 屏事实要组合根做的事：状态翻转与占域是副作用，出纯函数交给外面。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScreenDirective {
    /// 容器开了（工作台、箱子、熔炉……同一件 container 工具操作）：
    /// 登记屏状态、占域、投递格位清单与用法。
    Opened { kind: String },
    /// 容器关了。`commanded`=我们 close 动词的回声（工具已回执，不再吵）。
    Closed { kind: String, commanded: bool },
}

/// 一次收集的产出：要投递的行 + 要执行的屏副作用。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Wake {
    pub lines: Vec<String>,
    pub screens: Vec<ScreenDirective>,
}

impl Wake {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.screens.is_empty()
    }
}

/// 各窗各自的消费位置。启动时置于窗尾，不消费启动前的存量。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WakeCursors {
    chat: Option<u64>,
    damage: Option<u64>,
    jobs: Option<u64>,
    inventory: Option<u64>,
    screens: Option<u64>,
}

impl WakeCursors {
    /// 从一帧快照建立游标：停在各窗当前末尾。
    pub fn resume_from(snapshot: &TickSnapshot) -> Self {
        Self {
            chat: snapshot.chat.entries.last().map(|entry| entry.seq),
            damage: snapshot.damage.entries.last().map(|entry| entry.seq),
            jobs: snapshot.jobs.entries.last().map(|entry| entry.seq),
            inventory: snapshot
                .inventory_changes
                .entries
                .last()
                .map(|entry| entry.seq),
            screens: snapshot.screens.entries.last().map(|entry| entry.seq),
        }
    }

    /// 自身身份，用于防自激。
    ///
    /// `entity_key` 是自身 UUID；服务端不给发言者 UUID 时退回用户名比较。
    /// `item_screen_open`：格位类屏（物品栏/工作台）开着才投递格位变化
    /// （开着屏时有预期之外的格子变化也要通知；关着时不吵）。
    pub fn collect(
        &mut self,
        snapshot: &TickSnapshot,
        own: SelfIdentity<'_>,
        item_screen_open: bool,
    ) -> Wake {
        let mut lines = Vec::new();
        let mut screens = Vec::new();

        for entry in &snapshot.chat.entries {
            if !advance(&mut self.chat, entry.seq) {
                continue;
            }
            let Some(sender) = entry.sender.as_ref() else {
                // 系统广播没有发言者。当前不唤醒——它不是「有人对我说话」。
                continue;
            };
            let is_self = match &sender.uuid {
                Some(uuid) => uuid == own.entity_key,
                None => sender.username == own.username,
            };
            if is_self {
                continue;
            }
            lines.push(format!("{}: {}", sender.username, entry.content.plain_text));
        }

        for entry in &snapshot.damage.entries {
            if advance(&mut self.damage, entry.seq) {
                lines.push(render_damage(entry));
            }
        }

        for entry in &snapshot.jobs.entries {
            if !advance(&mut self.jobs, entry.seq) {
                continue;
            }
            // 进展不走这条通道：它是「还在走」，不是「出事了」，由轮末帧搭车呈现
            // （组合根另有一个游标）。这里只管终局。
            if entry.fact.is_terminal() && wakes_on(&entry.fact) {
                lines.push(render_job(entry));
            }
        }

        for entry in &snapshot.inventory_changes.entries {
            // 游标先推进（含屏关着时错过的条目——过了就是过了，不回放）。
            if advance(&mut self.inventory, entry.seq)
                && item_screen_open
                && wakes_on_inventory(entry)
            {
                lines.push(render_inventory_change(entry));
            }
        }

        for entry in &snapshot.screens.entries {
            if !advance(&mut self.screens, entry.seq) {
                continue;
            }
            let commanded = entry.source == FactSource::Commanded;
            screens.push(match &entry.event {
                ScreenEvent::Opened { kind, .. } => ScreenDirective::Opened { kind: kind.clone() },
                ScreenEvent::Closed { kind } => ScreenDirective::Closed {
                    kind: kind.clone(),
                    commanded,
                },
            });
        }

        Wake { lines, screens }
    }
}

/// 自身身份的借用视图，免得把两个 `String` 拷进每次轮询。
#[derive(Clone, Copy, Debug)]
pub struct SelfIdentity<'a> {
    pub entity_key: &'a str,
    pub username: &'a str,
}

/// 游标前进：已消费过返回 false。
///
/// **前进发生在过滤之前**——被判为自激或不值得唤醒的条目同样推进游标，
/// 否则它会在下一 tick 被重新检查，直到滑出窗口为止。
fn advance(cursor: &mut Option<u64>, seq: u64) -> bool {
    if cursor.is_some_and(|seen| seq <= seen) {
        return false;
    }
    *cursor = Some(seq);
    true
}

/// 哪些任务终局值得把同伴叫起来。
///
/// 顶替与停止是**模型自己下的令**的回声——为它醒等于自己吵自己。
/// 哪些终局值得叫醒。**自己干的不必回报**：顶替与取消都是模型刚下的命令，
/// 告诉它「你停下了」只是复述它自己的动作。看门狗超时要报——那正是它不知道
/// 的那种失败。
fn wakes_on(fact: &JobFact) -> bool {
    match fact {
        JobFact::Move { event, .. } => !matches!(
            event,
            MoveEvent::Replaced | MoveEvent::Cancelled | MoveEvent::Leg { .. } | MoveEvent::Stalled
        ),
        JobFact::Mine { event, .. } => !matches!(
            event,
            MineEvent::Replaced | MineEvent::Cancelled | MineEvent::Broke { .. }
        ),
    }
}

/// 哪些库存变化值得通知：预期之外的（ServerObserved）。
/// 自己 swap/丢弃的回声（Commanded）不吵——模型刚收到过工具回执。
fn wakes_on_inventory(entry: &InventoryChangeEntry) -> bool {
    entry.source == FactSource::ServerObserved
}

fn render_inventory_change(entry: &InventoryChangeEntry) -> String {
    render::render_inventory_change(entry)
}

fn render_damage(entry: &DamageEntry) -> String {
    render::render_damage_entry(entry)
}

fn render_job(entry: &JobEntry) -> String {
    render::render_job_entry(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use world::{
        ChatContent, ChatEntry, ChatPosition, ConnectionPhase, Epoch, FactSource, PlayerRef, Window,
    };

    const OWN_UUID: &str = "11111111-2222-3333-4444-555555555555";

    fn identity() -> SelfIdentity<'static> {
        SelfIdentity {
            entity_key: OWN_UUID,
            username: "companion",
        }
    }

    fn snapshot() -> TickSnapshot {
        TickSnapshot::empty(Epoch(1), 1, ConnectionPhase::Ready)
    }

    fn chat(seq: u64, username: &str, uuid: Option<&str>, text: &str) -> ChatEntry {
        ChatEntry {
            seq,
            tick: seq,
            occurred_at: std::time::SystemTime::UNIX_EPOCH,
            source: FactSource::ServerObserved,
            sender: Some(PlayerRef {
                username: username.to_owned(),
                uuid: uuid.map(str::to_owned),
            }),
            content: ChatContent {
                plain_text: text.to_owned(),
                position: Some(ChatPosition::Chat),
                verified: None,
            },
        }
    }

    fn system_chat(seq: u64, text: &str) -> ChatEntry {
        ChatEntry {
            sender: None,
            ..chat(seq, "", None, text)
        }
    }

    fn job(seq: u64, event: MoveEvent) -> JobEntry {
        JobEntry {
            seq,
            tick: seq,
            occurred_at: std::time::SystemTime::UNIX_EPOCH,
            id: world::JobId(seq),
            fact: JobFact::Move {
                destination: [1, 2, 3],
                event,
            },
        }
    }

    fn damage(seq: u64, before: f32, after: f32) -> DamageEntry {
        DamageEntry {
            seq,
            tick: seq,
            occurred_at: std::time::SystemTime::UNIX_EPOCH,
            health_before: before,
            health_after: after,
            cause: None,
        }
    }

    #[test]
    fn boot_cursors_skip_everything_already_in_the_windows() {
        let mut snap = snapshot();
        snap.chat = Window {
            entries: vec![chat(1, "alice", None, "启动前说的")],
        };
        let mut cursors = WakeCursors::resume_from(&snap);

        assert!(cursors.collect(&snap, identity(), false).is_empty());
    }

    #[test]
    fn own_chat_never_wakes_us_matched_by_uuid() {
        let mut snap = snapshot();
        let mut cursors = WakeCursors::default();
        // 服务端给了 UUID：按 UUID 比对，用户名被人冒用也不会误伤。
        snap.chat = Window {
            entries: vec![
                chat(1, "companion", Some(OWN_UUID), "我说的"),
                chat(
                    2,
                    "companion",
                    Some("99999999-0000-0000-0000-000000000000"),
                    "冒名者说的",
                ),
            ],
        };

        let lines = cursors.collect(&snap, identity(), false).lines;

        assert_eq!(lines, vec!["companion: 冒名者说的".to_owned()]);
    }

    #[test]
    fn own_chat_falls_back_to_username_when_the_server_gives_no_uuid() {
        let mut snap = snapshot();
        let mut cursors = WakeCursors::default();
        snap.chat = Window {
            entries: vec![
                chat(1, "companion", None, "我说的"),
                chat(2, "alice", None, "别人说的"),
            ],
        };

        let lines = cursors.collect(&snap, identity(), false).lines;

        assert_eq!(lines, vec!["alice: 别人说的".to_owned()]);
    }

    #[test]
    fn system_broadcasts_do_not_wake_us() {
        let mut snap = snapshot();
        let mut cursors = WakeCursors::default();
        snap.chat = Window {
            entries: vec![system_chat(1, "服务器将在 5 分钟后重启")],
        };

        assert!(cursors.collect(&snap, identity(), false).is_empty());
    }

    #[test]
    fn a_filtered_entry_still_advances_the_cursor() {
        let mut snap = snapshot();
        let mut cursors = WakeCursors::default();
        snap.chat = Window {
            entries: vec![chat(1, "companion", Some(OWN_UUID), "我说的")],
        };

        assert!(cursors.collect(&snap, identity(), false).is_empty());
        // 再来一帧：同一条不该被重新检查——否则它会在整个窗口存活期里
        // 每 tick 复检一次。
        snap.chat.entries.push(chat(2, "alice", None, "后来的"));
        assert_eq!(
            cursors.collect(&snap, identity(), false).lines,
            vec!["alice: 后来的".to_owned()]
        );
    }

    #[test]
    fn each_entry_is_delivered_exactly_once_across_ticks() {
        let mut snap = snapshot();
        let mut cursors = WakeCursors::default();
        snap.chat = Window {
            entries: vec![chat(1, "alice", None, "第一句")],
        };
        assert_eq!(cursors.collect(&snap, identity(), false).lines.len(), 1);
        assert!(cursors.collect(&snap, identity(), false).is_empty());

        snap.chat.entries.push(chat(2, "alice", None, "第二句"));
        assert_eq!(cursors.collect(&snap, identity(), false).lines.len(), 1);
        assert!(cursors.collect(&snap, identity(), false).is_empty());
    }

    #[test]
    fn only_terminals_we_did_not_command_wake_us() {
        let mut snap = snapshot();
        let mut cursors = WakeCursors::default();
        snap.jobs = Window {
            entries: vec![
                job(1, MoveEvent::Arrived),
                job(2, MoveEvent::Replaced),
                job(3, MoveEvent::Cancelled),
                job(4, MoveEvent::PathEnded),
                job(5, MoveEvent::Stalled),
            ],
        };

        // 三类被挡下，只剩到达与走不到：
        //   顶替、取消——模型自己下的令的回声，不该把它自己吵醒；
        //   卡住——它是**进展**不是终局（任务还在跑），由帧搭车呈现。
        assert_eq!(cursors.collect(&snap, identity(), false).lines.len(), 2);
    }

    #[test]
    fn three_windows_share_one_seq_but_keep_independent_cursors() {
        let mut snap = snapshot();
        let mut cursors = WakeCursors::default();
        // 单调 seq 跨窗交错：聊天 1、伤害 2、任务 3。
        snap.chat = Window {
            entries: vec![chat(1, "alice", None, "小心")],
        };
        snap.damage = Window {
            entries: vec![damage(2, 20.0, 14.0)],
        };
        snap.jobs = Window {
            entries: vec![job(3, MoveEvent::Arrived)],
        };

        let lines = cursors.collect(&snap, identity(), false).lines;

        assert_eq!(lines.len(), 3, "三个窗各出一条：{lines:?}");
        assert!(cursors.collect(&snap, identity(), false).is_empty());
    }

    fn inventory_change(seq: u64, slot: u16, source: FactSource) -> world::InventoryChangeEntry {
        world::InventoryChangeEntry {
            seq,
            tick: seq,
            occurred_at: std::time::SystemTime::UNIX_EPOCH,
            source,
            container_id: 0,
            slot,
            item_name: Some("oak_planks".to_owned()),
            count: 4,
        }
    }

    #[test]
    fn inventory_changes_wake_only_when_open_and_only_unexpected_ones() {
        let mut snap = snapshot();
        let mut cursors = WakeCursors::default();
        snap.inventory_changes = Window {
            entries: vec![inventory_change(1, 3, FactSource::ServerObserved)],
        };
        // 屏关着：不投递，但游标推进（过了就是过了，不回放）。
        assert!(cursors.collect(&snap, identity(), false).is_empty());
        assert!(cursors.collect(&snap, identity(), true).is_empty());

        // 屏开着：预期之外的投递，自己动作的回声（Commanded）不吵。
        snap.inventory_changes
            .entries
            .push(inventory_change(2, 10, FactSource::Commanded));
        snap.inventory_changes
            .entries
            .push(inventory_change(3, 0, FactSource::ServerObserved));
        let lines = cursors.collect(&snap, identity(), true).lines;
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("合成结果格"), "{lines:?}");
    }

    fn screen_entry(seq: u64, source: FactSource, event: ScreenEvent) -> world::ScreenEntry {
        world::ScreenEntry {
            seq,
            tick: seq,
            occurred_at: std::time::SystemTime::UNIX_EPOCH,
            source,
            event,
        }
    }

    #[test]
    fn screen_facts_become_directives_with_echo_marked() {
        let mut snap = snapshot();
        let mut cursors = WakeCursors::default();
        snap.screens = Window {
            entries: vec![
                screen_entry(
                    1,
                    FactSource::ServerObserved,
                    ScreenEvent::Opened {
                        kind: "crafting".to_owned(),
                        container_id: 3,
                        title: None,
                    },
                ),
                screen_entry(
                    2,
                    FactSource::Commanded,
                    ScreenEvent::Closed {
                        kind: "crafting".to_owned(),
                    },
                ),
                screen_entry(
                    3,
                    FactSource::ServerObserved,
                    ScreenEvent::Opened {
                        kind: "generic_9x3".to_owned(),
                        container_id: 4,
                        title: Some("box".to_owned()),
                    },
                ),
                screen_entry(
                    4,
                    FactSource::ServerObserved,
                    ScreenEvent::Closed {
                        kind: "generic_9x3".to_owned(),
                    },
                ),
            ],
        };

        let wake = cursors.collect(&snap, identity(), false);
        assert_eq!(
            wake.screens,
            vec![
                ScreenDirective::Opened {
                    kind: "crafting".to_owned(),
                },
                ScreenDirective::Closed {
                    kind: "crafting".to_owned(),
                    commanded: true,
                },
                ScreenDirective::Opened {
                    kind: "generic_9x3".to_owned(),
                },
                ScreenDirective::Closed {
                    kind: "generic_9x3".to_owned(),
                    commanded: false,
                },
            ]
        );
        // 屏事实恰好一次。
        assert!(cursors.collect(&snap, identity(), false).is_empty());
    }
}
