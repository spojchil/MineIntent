//! 请求边界信箱。两类条目，两种语义：
//!
//! - **件**：积攒，按到达序；轮末剩件留箱，供会话续轮。
//! - **景**：顶替，箱内至多一条；轮末清除。

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

    /// 全量排空：件按到达序在前，景（若有）最后。
    pub(crate) fn drain(&mut self) -> Vec<Message> {
        let mut drained = std::mem::take(&mut self.pieces);
        if let Some(scene) = self.scene.take() {
            drained.push(scene);
        }
        drained
    }

    /// 轮末：清除景。件留在箱里。
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
