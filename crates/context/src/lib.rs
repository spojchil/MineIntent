//! 上下文策略：内核提示（`PromptSource`）与压缩（`Compaction`）端口的实现方。
//!
//! 受保护上下文**两段**：人设（系统提示词）、记忆全文。两段都不逐轮变，
//! 所以放在最前面吃满前缀缓存；它们也永不参与压缩——位置本身就是豁免。
//!
//! 处境（渲染后的世界快照）**不在这里**。它每轮都变，放前缀等于每轮把整条
//! 对话赶出缓存；它随帧追加在对话末尾，见 `companion::situation`。
//!
//! 压缩金律：可重导出的世界状态直接扔；值得留的经历
//! **先经记忆落盘再入摘要**——落盘失败就保持原对话，不丢没存下的东西。

use std::sync::Arc;

use agent::persistence::SessionId;
use agent::{
    AgentError, Compaction, InputMessage, Model, ModelRequest, PortFuture, PromptSource, RunId,
    TranscriptItem,
};
use memory::MemoryFile;

/// 压缩指令：要求模型同时交回记忆增补与对话摘要。公开以便模型可见面导出评审。
pub const COMPACTION_INSTRUCTIONS: &str = "\
你在为一个 Minecraft 世界里的同伴压缩对话历史。下面是它的长期记忆全文与将被
压缩的对话。请输出一个 JSON 对象，恰好两个字段：
{\"memory_full_text\": \"更新后的记忆完整全文\", \"summary\": \"对话摘要\"}
规则：
- memory_full_text 是记忆文件的完整新全文：保留原有内容，把对话里值得长期
  记住的经历（承诺、关系变化、重要事件与教训）以第一人称并入；没有就原样交回。
- summary 用第一人称、过去式，写清对话里发生了什么、说过什么重要的话、
  哪些事做到一半。工具调用的机械细节可以丢，正在进行的意图不能丢。
- 世界状态（位置、血量、天色等）不要写入摘要——压缩后会重新投一份处境给你。
只输出这个 JSON 对象，不要其他文字。";

/// 压缩摘要在新对话里的包裹头。公开以便模型可见面导出评审。
pub const SUMMARY_PREFIX: &str = "【我此前的经历记述（同伴第一人称，压缩自更早的对话）】";

pub struct ContextStrategy {
    persona: String,
    memory: Arc<MemoryFile>,
    /// 摘要请求自持的模型依赖；缺席时压缩退化为"不压"（原样交回）。
    model: Option<Arc<dyn Model>>,
}

impl ContextStrategy {
    pub fn new(persona: impl Into<String>, memory: Arc<MemoryFile>) -> Self {
        Self {
            persona: persona.into(),
            memory,
            model: None,
        }
    }

    /// 接上压缩用的模型。
    pub fn with_model(mut self, model: Arc<dyn Model>) -> Self {
        self.model = Some(model);
        self
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

    /// 从模型回复里取出压缩结论。返回 None = 这次压缩作废（原样保留对话）。
    ///
    /// `memory_full_text` 是必填：缺了它无法证明"值得留的已落盘"，宁可不压
    /// 也不拿摘要替换没备份过的对话（金律）。无须改动时模型按指令原样交回。
    fn parse_compaction_reply(reply: &str) -> Option<(String, String)> {
        let start = reply.find('{')?;
        let end = reply.rfind('}')?;
        let value: serde_json::Value = serde_json::from_str(&reply[start..=end]).ok()?;
        let summary = value.get("summary")?.as_str()?.to_owned();
        if summary.trim().is_empty() {
            return None;
        }
        let memory_full_text = value.get("memory_full_text")?.as_str()?.to_owned();
        Some((memory_full_text, summary))
    }
}

impl PromptSource for ContextStrategy {
    /// 只放**不逐轮变**的东西。
    ///
    /// 内核把本方法的返回值放在每次请求最前面，而前缀缓存按最长公共前缀命中——
    /// 这里放一样逐轮变的东西，缓存就在它那一项断掉，整条对话每轮全额重算。
    ///
    /// 2026-08-11 到 08-18 这里第三项是「当前处境」（位置、附近实体、天色……），
    /// 每轮现拉。实盘 30 分钟 456 次请求的账：13 次轮首未命中 367,984 token，
    /// 占全跑未命中的 75%，而轮内 443 次平均只有 272。命中值是 `0 / 256 / 640`
    /// ——不是没命中，是只命中了人设加半截记忆就断在处境开头。
    ///
    /// 处境现在走信箱，随帧追加在对话末尾（`companion::situation`）。这正是旧栈
    /// PR #93「稳定前缀 + 追加帧」立下、迁移时丢掉的分工：**追加一次是免费的，
    /// 同一段东西每轮重渲染是致命的**。
    fn base_context(&self) -> Result<Vec<TranscriptItem>, AgentError> {
        Ok(vec![
            InputMessage::text("system", self.persona.clone()).into(),
            self.memory_message().into(),
        ])
    }
}

impl Compaction for ContextStrategy {
    fn compact<'a>(
        &'a self,
        conversation: &'a [TranscriptItem],
    ) -> PortFuture<'a, Result<Vec<TranscriptItem>, AgentError>> {
        Box::pin(async move {
            // 压缩失败一律降级为「不压」：返回 Err 会终止运行，而保持原对话总是安全的。
            let unchanged = || Ok(conversation.to_vec());
            let Some(model) = &self.model else {
                return unchanged();
            };
            let Ok(memory_text) = self.memory.read() else {
                // 读不到记忆就无法安全并入经历；保持原对话，下个边界再试。
                return unchanged();
            };

            let mut transcript: Vec<TranscriptItem> = vec![InputMessage::text(
                "system",
                format!("{COMPACTION_INSTRUCTIONS}\n\n【长期记忆现文】\n{memory_text}"),
            )
            .into()];
            transcript.extend(conversation.iter().cloned());
            transcript.push(InputMessage::text("user", "请按上面的规则输出压缩 JSON。").into());

            let session_id = match SessionId::new("compaction") {
                Ok(id) => id,
                Err(_) => return unchanged(),
            };
            let Ok(response) = model
                .complete(ModelRequest {
                    session_id,
                    run_id: RunId::new("compaction"),
                    request_index: 1,
                    transcript,
                    function_tools: Vec::new(),
                })
                .await
            else {
                return unchanged();
            };
            let Some((memory_full_text, summary)) =
                Self::parse_compaction_reply(&response.output.text_content())
            else {
                return unchanged();
            };
            // 模型调用期间记忆可能被别的写入方（remember、维护者手改）更新；
            // 压缩结论基于旧文，覆盖会吃掉新写入——检测到变化就放弃本次压缩。
            match self.memory.read() {
                Ok(current) if current == memory_text => {}
                _ => return unchanged(),
            }
            if self.memory.write(&memory_full_text).is_err() {
                // 落盘失败就不丢对话：金律是"落盘否则就丢"，反之亦然。
                return unchanged();
            }
            // 耐久事实（中断回执、effect 对账等）说的是「外界真的发生过什么」，
            // 内核要求逐项原样保留并校验序列一致，否则整个压缩结果被丢弃。
            // 摘要在前，耐久事实按原相对顺序跟在后面。
            let mut replaced: Vec<TranscriptItem> =
                vec![InputMessage::text("user", format!("{SUMMARY_PREFIX}\n{summary}")).into()];
            replaced.extend(
                conversation
                    .iter()
                    .filter(|item| agent::durable_fact_kind(item).is_some())
                    .cloned(),
            );
            Ok(replaced)
        })
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

    struct CannedModel(String);

    impl Model for CannedModel {
        fn complete<'a>(
            &'a self,
            _request: ModelRequest,
        ) -> PortFuture<'a, Result<agent::ModelResponse, agent::AgentError>> {
            Box::pin(async move {
                Ok(agent::ModelResponse {
                    output: agent::ModelOutput::text(self.0.clone()),
                    ..agent::ModelResponse::default()
                })
            })
        }
    }

    fn conversation() -> Vec<TranscriptItem> {
        vec![
            InputMessage::text("user", "alice: 你叫什么").into(),
            TranscriptItem::ModelOutput(agent::ModelOutput::text("我叫小明。")),
        ]
    }

    #[tokio::test]
    async fn compaction_writes_memory_then_replaces_conversation_with_summary() {
        let memory = Arc::new(MemoryFile::new(scratch_dir().join("memory.md")));
        memory.write("旧记忆。").unwrap();
        let strategy = ContextStrategy::new("人设", memory.clone()).with_model(Arc::new(
            CannedModel(
                "{\"memory_full_text\": \"旧记忆。\\n认识了 alice。\", \"summary\": \"我和 alice 互相认识了。\"}"
                    .to_owned(),
            ),
        ));

        let compacted = strategy.compact(&conversation()).await.unwrap();
        assert_eq!(compacted.len(), 1);
        let (role, text) = text_of(&compacted[0]);
        assert_eq!(role, "user");
        assert!(text.contains("我和 alice 互相认识了。"));
        assert!(memory.read().unwrap().contains("认识了 alice。"));
    }

    #[tokio::test]
    async fn unparseable_compaction_reply_keeps_the_conversation_untouched() {
        let memory = Arc::new(MemoryFile::new(scratch_dir().join("memory.md")));
        memory.write("旧记忆。").unwrap();
        let strategy = ContextStrategy::new("人设", memory.clone())
            .with_model(Arc::new(CannedModel("我不想输出 JSON。".to_owned())));

        let original = conversation();
        let compacted = strategy.compact(&original).await.unwrap();
        assert_eq!(compacted, original);
        assert_eq!(memory.read().unwrap(), "旧记忆。");
    }

    #[tokio::test]
    async fn summary_without_memory_field_does_not_destroy_the_conversation() {
        // 缺 memory_full_text 无法证明经历已落盘——金律要求放弃本次压缩。
        let memory = Arc::new(MemoryFile::new(scratch_dir().join("memory.md")));
        memory.write("旧记忆。").unwrap();
        let strategy = ContextStrategy::new("人设", memory.clone()).with_model(Arc::new(
            CannedModel("{\"summary\": \"只有摘要没有记忆。\"}".to_owned()),
        ));

        let original = conversation();
        assert_eq!(strategy.compact(&original).await.unwrap(), original);
        assert_eq!(memory.read().unwrap(), "旧记忆。");
    }

    #[tokio::test]
    async fn compaction_preserves_durable_facts_verbatim() {
        let memory = Arc::new(MemoryFile::new(scratch_dir().join("memory.md")));
        memory.write("旧记忆。").unwrap();
        let strategy = ContextStrategy::new("人设", memory).with_model(Arc::new(CannedModel(
            "{\"memory_full_text\": \"旧记忆。\", \"summary\": \"摘要。\"}".to_owned(),
        )));

        // 中断回执是耐久事实：不保留它，内核会丢弃整个压缩结果。
        let receipt: TranscriptItem = InputMessage::new(
            "user",
            vec![agent::ContentPart::json(serde_json::json!({
                "kind": agent::DurableFactKind::InterruptedToolBatch.as_wire(),
            }))],
        )
        .into();
        let mut with_receipt = conversation();
        with_receipt.push(receipt.clone());

        let compacted = strategy.compact(&with_receipt).await.unwrap();
        assert_eq!(compacted.len(), 2, "摘要一条 + 回执一条：{compacted:?}");
        assert_eq!(compacted[1], receipt);
    }

    #[tokio::test]
    async fn compaction_without_a_model_returns_the_conversation_as_is() {
        let strategy = ContextStrategy::new(
            "人设",
            Arc::new(MemoryFile::new(scratch_dir().join("memory.md"))),
        );
        let original = conversation();
        assert_eq!(strategy.compact(&original).await.unwrap(), original);
    }
}
