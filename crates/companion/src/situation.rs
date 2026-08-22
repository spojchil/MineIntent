//! 处境随帧追加：只说变了的那几行。
//!
//! # 为什么处境不在前缀里
//!
//! 内核把 `base_context` 放在每次请求最前面，而前缀缓存按最长公共前缀命中——处境若在
//! 前缀里，每轮开头缓存都断在它那一项，它后面的整条对话全额重算。长跑实测中，
//! 仅轮首未命中就能占全跑 token 的七成以上。
//!
//! 分工因此是：**追加一次是免费的，同一段东西每轮重渲染是致命的**——处境随帧
//! 追加到对话末尾，前缀只留不逐轮变的人设与记忆。
//!
//! # 只说变了的那几行
//!
//! 处境六行里逐轮都变的只有位置与附近实体；自称是静态的，环境、体征、未读数偶尔变。
//! 整段重发等于每帧几十 token 的重复，所以按行比对，只投变了的。
//!
//! 消失的行不点名——「未读数归零」「不再悬空」由后续事实自然覆盖，为每个消失的行
//! 补一句「xx 没有了」只会制造噪音。

use render::SituationLine;

/// 帧要说的处境行。持有上一次**投出去过**的那份，因此比对基准始终是模型真正见过的。
pub struct SituationTracker {
    last: Vec<(SituationLine, String)>,
    resend_all: bool,
}

impl SituationTracker {
    /// 开局第一帧投全量：模型此前没见过任何一行。
    pub fn new() -> Self {
        Self {
            last: Vec::new(),
            resend_all: true,
        }
    }

    /// 下一帧重投全量。
    ///
    /// 压缩把对话换成一段摘要，先前追加的处境随之消失，而摘要按指令不含世界状态。
    /// 不重投的话，模型要一直等到某一行**恰好又变了**才重新知道自己在哪。
    pub fn request_full_resend(&mut self) {
        self.resend_all = true;
    }

    /// 本帧该说的处境行。**只在真的要投递时调用**——它会推进比对基准，
    /// 算了却没投出去的差异就永远丢了。
    pub fn take(&mut self, current: Vec<(SituationLine, String)>) -> Vec<String> {
        let lines = if self.resend_all {
            current.iter().map(|(_, text)| text.clone()).collect()
        } else {
            changed_lines(&self.last, &current)
        };
        self.resend_all = false;
        self.last = current;
        lines
    }
}

/// 与上一份逐行比对，返回新增或改写过的行（保持当前顺序）。
fn changed_lines(
    previous: &[(SituationLine, String)],
    current: &[(SituationLine, String)],
) -> Vec<String> {
    current
        .iter()
        .filter(|(kind, text)| {
            !previous
                .iter()
                .any(|(seen_kind, seen_text)| seen_kind == kind && seen_text == text)
        })
        .map(|(_, text)| text.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: SituationLine, text: &str) -> (SituationLine, String) {
        (kind, text.to_owned())
    }

    fn full() -> Vec<(SituationLine, String)> {
        vec![
            line(SituationLine::Identity, "你叫 Bot。"),
            line(SituationLine::Environment, "雪原，正午前后。"),
            line(SituationLine::Position, "位置 (1, 64, 1)，面朝北。"),
        ]
    }

    #[test]
    fn first_frame_says_everything() {
        let mut tracker = SituationTracker::new();
        assert_eq!(tracker.take(full()).len(), 3);
    }

    #[test]
    fn unchanged_lines_are_not_repeated() {
        let mut tracker = SituationTracker::new();
        tracker.take(full());

        let mut moved = full();
        moved[2] = line(SituationLine::Position, "位置 (2, 64, 1)，面朝北。");
        assert_eq!(tracker.take(moved), vec!["位置 (2, 64, 1)，面朝北。"]);
    }

    #[test]
    fn nothing_changed_says_nothing() {
        let mut tracker = SituationTracker::new();
        tracker.take(full());
        assert!(tracker.take(full()).is_empty());
    }

    /// 同一行改回旧值也算变化——模型上一次听到的是中间那个值。
    #[test]
    fn returning_to_an_older_value_is_still_a_change() {
        let mut tracker = SituationTracker::new();
        tracker.take(full());

        let mut moved = full();
        moved[2] = line(SituationLine::Position, "位置 (2, 64, 1)，面朝北。");
        tracker.take(moved);

        assert_eq!(tracker.take(full()), vec!["位置 (1, 64, 1)，面朝北。"]);
    }

    /// 消失的行不点名，也不该让留下的行重复。
    #[test]
    fn vanished_lines_are_silent() {
        let mut tracker = SituationTracker::new();
        let mut with_unread = full();
        with_unread.push(line(SituationLine::Unread, "聊天有 2 条新消息。"));
        tracker.take(with_unread);

        assert!(tracker.take(full()).is_empty());
    }

    #[test]
    fn compaction_makes_the_next_frame_say_everything_again() {
        let mut tracker = SituationTracker::new();
        tracker.take(full());
        assert!(tracker.take(full()).is_empty());

        tracker.request_full_resend();
        assert_eq!(tracker.take(full()).len(), 3);
        // 只重投一次，之后回到只说差异。
        assert!(tracker.take(full()).is_empty());
    }
}
