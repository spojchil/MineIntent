//! 唤醒判据的纯函数部分：从一帧快照里挑出「该把同伴叫起来」的事实。
//!
//! **这是脚手架，不是判据。** 正式的关注清单未裁（见
//! `docs/wake-criterion-decision.md`），这里只做最朴素的三条：别人对我说话、
//! 我受伤了、我下的移动任务有果了。
//!
//! 抽成纯函数的理由：它是组合根里唯一有分支的逻辑，而组合根跑起来要一台
//! Minecraft 服务器和一个模型端点。分支判断不该只能靠实盘验证。
//!
//! 三个窗共用一条单调 `seq`，所以三个游标互不干扰，且「同一 tick 内多条」
//! 不会漏——tick 会重复，seq 不会。

use world::{DamageEntry, JobEntry, JobOutcome, TickSnapshot};

/// 三个窗各自的消费位置。启动时置于窗尾，不消费启动前的存量。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WakeCursors {
    chat: Option<u64>,
    damage: Option<u64>,
    jobs: Option<u64>,
}

impl WakeCursors {
    /// 从一帧快照建立游标：停在各窗当前末尾。
    pub fn resume_from(snapshot: &TickSnapshot) -> Self {
        Self {
            chat: snapshot.chat.entries.last().map(|entry| entry.seq),
            damage: snapshot.damage.entries.last().map(|entry| entry.seq),
            jobs: snapshot.jobs.entries.last().map(|entry| entry.seq),
        }
    }

    /// 自身身份，用于防自激。
    ///
    /// `entity_key` 是自身 UUID；服务端不给发言者 UUID 时退回用户名比较。
    pub fn collect(&mut self, snapshot: &TickSnapshot, own: SelfIdentity<'_>) -> Vec<String> {
        let mut lines = Vec::new();

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
            if advance(&mut self.jobs, entry.seq) && wakes_on(entry.outcome) {
                lines.push(render_job(entry));
            }
        }

        lines
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
fn wakes_on(outcome: JobOutcome) -> bool {
    matches!(
        outcome,
        JobOutcome::Arrived | JobOutcome::PathEnded | JobOutcome::Stalled
    )
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
        ChatContent, ChatEntry, ChatPosition, ConnectionPhase, Epoch, FactSource, JobKind,
        PlayerRef, Window,
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

    fn job(seq: u64, outcome: JobOutcome) -> JobEntry {
        JobEntry {
            seq,
            tick: seq,
            occurred_at: std::time::SystemTime::UNIX_EPOCH,
            job: JobKind::MoveTo {
                destination: [1, 2, 3],
            },
            outcome,
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

        assert!(cursors.collect(&snap, identity()).is_empty());
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

        let lines = cursors.collect(&snap, identity());

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

        let lines = cursors.collect(&snap, identity());

        assert_eq!(lines, vec!["alice: 别人说的".to_owned()]);
    }

    #[test]
    fn system_broadcasts_do_not_wake_us() {
        let mut snap = snapshot();
        let mut cursors = WakeCursors::default();
        snap.chat = Window {
            entries: vec![system_chat(1, "服务器将在 5 分钟后重启")],
        };

        assert!(cursors.collect(&snap, identity()).is_empty());
    }

    #[test]
    fn a_filtered_entry_still_advances_the_cursor() {
        let mut snap = snapshot();
        let mut cursors = WakeCursors::default();
        snap.chat = Window {
            entries: vec![chat(1, "companion", Some(OWN_UUID), "我说的")],
        };

        assert!(cursors.collect(&snap, identity()).is_empty());
        // 再来一帧：同一条不该被重新检查——否则它会在整个窗口存活期里
        // 每 tick 复检一次。
        snap.chat.entries.push(chat(2, "alice", None, "后来的"));
        assert_eq!(
            cursors.collect(&snap, identity()),
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
        assert_eq!(cursors.collect(&snap, identity()).len(), 1);
        assert!(cursors.collect(&snap, identity()).is_empty());

        snap.chat.entries.push(chat(2, "alice", None, "第二句"));
        assert_eq!(cursors.collect(&snap, identity()).len(), 1);
        assert!(cursors.collect(&snap, identity()).is_empty());
    }

    #[test]
    fn only_outcomes_we_did_not_command_wake_us() {
        let mut snap = snapshot();
        let mut cursors = WakeCursors::default();
        snap.jobs = Window {
            entries: vec![
                job(1, JobOutcome::Arrived),
                job(2, JobOutcome::Replaced),
                job(3, JobOutcome::Stopped),
                job(4, JobOutcome::PathEnded),
                job(5, JobOutcome::Stalled),
            ],
        };

        // 顶替与停止是模型自己下的令的回声，不该把它自己吵醒。
        assert_eq!(cursors.collect(&snap, identity()).len(), 3);
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
            entries: vec![job(3, JobOutcome::Arrived)],
        };

        let lines = cursors.collect(&snap, identity());

        assert_eq!(lines.len(), 3, "三个窗各出一条：{lines:?}");
        assert!(cursors.collect(&snap, identity()).is_empty());
    }
}
