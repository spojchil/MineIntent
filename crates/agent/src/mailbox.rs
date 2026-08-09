//! 请求边界信箱：新架构里唯一合法的队列。
//!
//! 两类条目，两种语义（`TEMP_顶层模块接口.md` §2/§3）：
//! - **件**（玩家的话、疼、job 终了）：积攒——三条就是三条，按到达序，
//!   不可互相替代；轮末剩货晋升为下一轮触发。
//! - **景**（处境更新/轮末帧）：顶替——新的替换未投递的旧的；轮末蒸发
//!   （不起轮：下一轮的 situation 由①现拉，天然更新）。
//!
//! 有界性来自现实（一个请求周期里玩家打不了几句话），不设容量参数。

use crate::ports::Message;

#[derive(Default)]
pub(crate) struct Mailbox {
    pieces: Vec<Message>,
    scene: Option<Message>,
}

impl Mailbox {
    /// 件：积攒。
    pub(crate) fn post_pieces(&mut self, pieces: Vec<Message>) {
        self.pieces.extend(pieces);
    }

    /// 景：顶替。
    pub(crate) fn post_scene(&mut self, scene: Message) {
        self.scene = Some(scene);
    }

    /// 请求边界全量排空：件按到达序在前，景（若有）最后——处境是最新鲜的收尾。
    pub(crate) fn drain(&mut self) -> Vec<Message> {
        let mut drained = std::mem::take(&mut self.pieces);
        if let Some(scene) = self.scene.take() {
            drained.push(scene);
        }
        drained
    }

    /// 轮末：景蒸发。件留在箱里（晋升为下一轮触发由会话层根据非空判断）。
    pub(crate) fn end_of_turn(&mut self) {
        self.scene = None;
    }

    pub(crate) fn has_pieces(&self) -> bool {
        !self.pieces.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn msg(text: &str) -> Message {
        let mut m = Message::new();
        m.insert("role".to_owned(), Value::String("user".to_owned()));
        m.insert("content".to_owned(), Value::String(text.to_owned()));
        m
    }

    #[test]
    fn pieces_accumulate_in_order_and_scene_replaces() {
        let mut mailbox = Mailbox::default();
        mailbox.post_pieces(vec![msg("a")]);
        mailbox.post_pieces(vec![msg("b")]);
        mailbox.post_scene(msg("scene-1"));
        mailbox.post_scene(msg("scene-2"));

        let drained = mailbox.drain();
        let contents: Vec<&str> = drained
            .iter()
            .map(|m| m.get("content").and_then(Value::as_str).unwrap())
            .collect();
        // 件按到达序，景顶替后只剩最新一条、排最后。
        assert_eq!(contents, vec!["a", "b", "scene-2"]);
        assert!(mailbox.drain().is_empty());
    }

    #[test]
    fn end_of_turn_evaporates_scene_but_keeps_pieces() {
        let mut mailbox = Mailbox::default();
        mailbox.post_pieces(vec![msg("leftover")]);
        mailbox.post_scene(msg("stale-scene"));
        mailbox.end_of_turn();

        assert!(mailbox.has_pieces());
        let drained = mailbox.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(
            drained[0].get("content").and_then(Value::as_str),
            Some("leftover")
        );
    }
}
