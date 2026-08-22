//! 上下文策略：内核提示（`PromptSource`）与压缩（`Compaction`）端口的实现方。
//!
//! 受保护上下文**两段**：人设（系统提示词）、记忆全文。两段都不逐轮变，
//! 所以放在最前面吃满前缀缓存；它们也永不参与压缩——位置本身就是豁免。
//!
//! 处境（渲染后的世界快照）**不在这里**。它每轮都变，放前缀等于每轮把整条
//! 对话赶出缓存；它随帧追加在对话末尾，见 `companion::situation`。
//!
//! 压缩当前是**空实现**（原样交回）——做过的那一版形态错了，摘掉再重设计。
//! 理由与重设计待决项见 `Compaction` 的实现注释与 issue #136。

use std::sync::Arc;

use agent::{AgentError, Compaction, InputMessage, PortFuture, PromptSource, TranscriptItem};
use memory::MemoryFile;

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
    /// 只放**不逐轮变**的东西。
    ///
    /// 内核把本方法的返回值放在每次请求最前面，而前缀缓存按最长公共前缀命中——
    /// 这里放一样逐轮变的东西，缓存就在它那一项断掉，整条对话每轮全额重算。
    ///
    /// 处境走信箱，随帧追加在对话末尾（`companion::situation`）。分工是：
    /// **追加一次是免费的，同一段东西每轮重渲染是致命的**。
    fn base_context(&self) -> Result<Vec<TranscriptItem>, AgentError> {
        Ok(vec![
            InputMessage::text("system", self.persona.clone()).into(),
            self.memory_message().into(),
        ])
    }
}

impl Compaction for ContextStrategy {
    /// **空实现：原样交回，不压。**
    ///
    /// 不是「还没做」，是**做过的那一版形态错了**，先摘掉再重设计。原来那版每次
    /// 压缩打一次模型、要它交回 `{memory_full_text, summary}`，把值得留的经历并进
    /// 长期记忆、再用一段摘要替换整条对话。三个问题：
    ///
    /// 1. **压缩与长期记忆无关**。把「这一跑发生了什么」写进长期
    ///    记忆，是拿会话噪音污染一份该长期稳定的文件——实盘一跑写三次。
    /// 2. **它自己就是缓存杀手**。内核在 `Compaction` 的文档里写着：压缩改写对话，
    ///    服务商前缀缓存整体失效，下一次请求全额重算。省下的上下文要值回这笔钱，
    ///    而当前形态没算过这笔账。
    /// 3. **帧的段结构还没立回来**。压缩该保留什么、能丢什么，取决于哪些事实
    ///    可重导出；段结构没定之前定压缩规则，是在流沙上盖房子。
    ///
    /// 空实现期间压缩线设在**服务商上下文窗口的 95%**（组合根 `session_config`）。
    /// 那条线现在只是一个观察点：越过它什么也不会发生，上下文继续长，最终由服务商
    /// 的长度限制兜底。这是刻意的——先让「到底能撑多久」变成可观测的事实。
    ///
    /// 重设计的未决项在 issue #136。
    fn compact<'a>(
        &'a self,
        conversation: &'a [TranscriptItem],
    ) -> PortFuture<'a, Result<Vec<TranscriptItem>, AgentError>> {
        Box::pin(async move { Ok(conversation.to_vec()) })
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

        let base = strategy.base_context().unwrap();
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
        assert!(text_of(&strategy.base_context().unwrap()[1])
            .1
            .contains("第一轮的记忆"));

        std::fs::write(dir.join("memory.md"), "维护者手改的记忆").unwrap();
        assert!(text_of(&strategy.base_context().unwrap()[1])
            .1
            .contains("维护者手改的记忆"));
    }

    #[test]
    fn empty_memory_is_stated_not_omitted() {
        let strategy = ContextStrategy::new(
            "人设",
            Arc::new(MemoryFile::new(scratch_dir().join("memory.md"))),
        );

        let base = strategy.base_context().unwrap();
        assert_eq!(base.len(), 2);
        assert!(text_of(&base[1]).1.contains("还没有记忆"));
    }

    #[test]
    fn read_failure_warns_against_overwriting_instead_of_playing_empty() {
        // 把记忆路径指向目录本身制造真实读取错误（非 NotFound）。
        let strategy = ContextStrategy::new("人设", Arc::new(MemoryFile::new(scratch_dir())));

        let (_, memory_text) = text_of(&strategy.base_context().unwrap()[1]);
        assert!(memory_text.contains("读取失败"));
        assert!(memory_text.contains("不要用 remember 改写"));
        assert!(!memory_text.contains("还没有记忆"));
    }

    /// 前缀里**没有**逐轮变的东西。
    ///
    /// 这条测试是反过来写的：它的前身叫 `situation_is_appended_after_the_stable_prefix`，
    /// 断言 `base.len() == 3` 且第三项是「【当前处境】」——名字表达的意图（追加在
    /// 稳定前缀之后就不搅缓存）本身是错的：追到 `base_context` 的尾巴，仍然在**整条
    /// 对话之前**，缓存照断。那条测试一直是绿的，守卫的是一个错误理解。
    ///
    /// 现在守卫的是结论：前缀只有两项，处境不在其中。
    #[test]
    fn base_context_carries_nothing_that_changes_every_turn() {
        let strategy = ContextStrategy::new(
            "人设",
            Arc::new(MemoryFile::new(scratch_dir().join("memory.md"))),
        );

        let base = strategy.base_context().unwrap();
        assert_eq!(base.len(), 2, "前缀只该有人设与记忆");
        for item in &base {
            let (role, text) = text_of(item);
            assert_eq!(role, "system");
            assert!(
                !text.contains("【当前处境】"),
                "处境不能回到前缀里：它每轮都变，会把整条对话赶出缓存"
            );
        }
    }
}
