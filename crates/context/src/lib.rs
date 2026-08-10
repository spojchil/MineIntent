//! 上下文策略：内核提示端口（`PromptSource`）的实现方。
//!
//! 受保护上下文放两样：人设（系统提示词）与记忆全文。两者每轮现拉、
//! 永不参与压缩——压缩只碰会话区，位置本身就是豁免。
//!
//! 留位（不发明临时形状）：处境（渲染后的世界快照）等丙与模块一的
//! `TickSnapshot` 落位后追加在 `base_context` 末尾（稳定前缀之后）；
//! 对话压缩（`Compaction`）等压缩政策裁定后在本 crate 补上，届时自持
//! 模型依赖并在摘要前把值得留的经历经记忆落盘。

use std::sync::Arc;

use agent::{InputMessage, PromptSource, TranscriptItem};
use memory::MemoryFile;

/// 人设与记忆分作两条消息：人设固定在最前，记忆紧随其后，
/// 变动的部分不搅动稳定前缀。
pub struct ContextStrategy {
    persona: String,
    memory: Arc<MemoryFile>,
}

impl ContextStrategy {
    pub fn new(persona: impl Into<String>, memory: Arc<MemoryFile>) -> Self {
        Self {
            persona: persona.into(),
            memory,
        }
    }

    fn memory_message(&self) -> InputMessage {
        let text = match self.memory.read() {
            Ok(text) if text.trim().is_empty() => "【长期记忆】\n（还没有记忆。）".to_owned(),
            Ok(text) => format!("【长期记忆】\n{text}"),
            // 读取失败不能装作记忆为空：模型看着空记忆整文改写会把真实记忆冲掉。
            Err(error) => format!(
                "【长期记忆】\n读取失败：{error}。当前看不到已有记忆，恢复前不要用 remember 改写。"
            ),
        };
        InputMessage::text("system", text)
    }
}

impl PromptSource for ContextStrategy {
    fn base_context(&self) -> Vec<TranscriptItem> {
        vec![
            InputMessage::text("system", self.persona.clone()).into(),
            self.memory_message().into(),
        ]
    }

}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn scratch_dir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mineintent-context-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("建测试目录");
        dir
    }

    fn text_of(item: &TranscriptItem) -> (&str, String) {
        match item {
            TranscriptItem::Input(message) => (
                message.role.as_str(),
                message
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        agent::ContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect(),
            ),
            other => panic!("期望输入消息，得到 {other:?}"),
        }
    }

    #[test]
    fn base_context_is_persona_then_memory_both_system() {
        let memory = Arc::new(MemoryFile::new(scratch_dir().join("memory.md")));
        memory.write("我在山坡的木屋住下了。").unwrap();
        let strategy = ContextStrategy::new("你是小明。", memory);

        let base = strategy.base_context();
        assert_eq!(base.len(), 2);
        let (persona_role, persona_text) = text_of(&base[0]);
        assert_eq!(persona_role, "system");
        assert_eq!(persona_text, "你是小明。");
        let (memory_role, memory_text) = text_of(&base[1]);
        assert_eq!(memory_role, "system");
        assert!(memory_text.contains("我在山坡的木屋住下了。"));
    }

    #[test]
    fn memory_is_read_fresh_each_round() {
        let dir = scratch_dir();
        let memory = Arc::new(MemoryFile::new(dir.join("memory.md")));
        let strategy = ContextStrategy::new("人设", memory.clone());

        memory.write("第一轮的记忆").unwrap();
        assert!(text_of(&strategy.base_context()[1]).1.contains("第一轮的记忆"));

        std::fs::write(dir.join("memory.md"), "维护者手改的记忆").unwrap();
        assert!(text_of(&strategy.base_context()[1]).1.contains("维护者手改的记忆"));
    }

    #[test]
    fn empty_memory_is_stated_not_omitted() {
        let strategy = ContextStrategy::new(
            "人设",
            Arc::new(MemoryFile::new(scratch_dir().join("memory.md"))),
        );

        let base = strategy.base_context();
        assert_eq!(base.len(), 2);
        assert!(text_of(&base[1]).1.contains("还没有记忆"));
    }

    #[test]
    fn read_failure_warns_against_overwriting_instead_of_playing_empty() {
        // 把记忆路径指向目录本身制造真实读取错误（非 NotFound）。
        let strategy = ContextStrategy::new("人设", Arc::new(MemoryFile::new(scratch_dir())));

        let (_, memory_text) = text_of(&strategy.base_context()[1]);
        assert!(memory_text.contains("读取失败"));
        assert!(memory_text.contains("不要用 remember 改写"));
        assert!(!memory_text.contains("还没有记忆"));
    }

}
