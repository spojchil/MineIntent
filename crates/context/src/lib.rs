//! 上下文策略：内核提示（`PromptSource`）与压缩（`Compaction`）端口的实现方。
//!
//! 受保护上下文三段：人设（系统提示词）、记忆全文、处境（渲染后的世界快照）。
//! 全部每轮现拉、永不参与压缩——压缩只碰会话区，位置本身就是豁免。
//! 稳定前缀在前（人设、记忆），易变处境在尾，不搅动提示缓存。
//!
//! 压缩金律：可重导出的世界状态直接扔（处境下轮现拉）；值得留的经历
//! **先经记忆落盘再入摘要**——落盘失败就保持原对话，不丢没存下的东西。

use std::sync::Arc;

use agent::{
    Compaction, InputMessage, Model, ModelRequest, PortFuture, PromptSource, TranscriptItem,
};
use memory::MemoryFile;
use screens::ChatReadMark;
use world::SnapshotSource;

/// 压缩指令：要求模型同时交回记忆增补与对话摘要。
const COMPACTION_INSTRUCTIONS: &str = "\
你在为一个 Minecraft 世界里的同伴压缩对话历史。下面是它的长期记忆全文与将被
压缩的对话。请输出一个 JSON 对象，恰好两个字段：
{\"memory_full_text\": \"更新后的记忆完整全文\", \"summary\": \"对话摘要\"}
规则：
- memory_full_text 是记忆文件的完整新全文：保留原有内容，把对话里值得长期
  记住的经历（承诺、关系变化、重要事件与教训）以第一人称并入；没有就原样交回。
- summary 用第一人称、过去式，写清对话里发生了什么、说过什么重要的话、
  哪些事做到一半。工具调用的机械细节可以丢，正在进行的意图不能丢。
- 世界状态（位置、血量、天色等）不要写入摘要——下一轮会重新看到。
只输出这个 JSON 对象，不要其他文字。";

pub struct ContextStrategy {
    persona: String,
    memory: Arc<MemoryFile>,
    /// 处境来源；组合根接线前可缺席（此时 base_context 只有人设+记忆）。
    situation: Option<SituationInputs>,
    /// 摘要请求自持的模型依赖；缺席时压缩退化为"不压"（原样交回）。
    model: Option<Arc<dyn Model>>,
}

struct SituationInputs {
    snapshots: Arc<dyn SnapshotSource>,
    chat_read: Arc<ChatReadMark>,
}

impl ContextStrategy {
    pub fn new(persona: impl Into<String>, memory: Arc<MemoryFile>) -> Self {
        Self {
            persona: persona.into(),
            memory,
            situation: None,
            model: None,
        }
    }

    /// 接上处境来源：快照 + 聊天已读水位（未读数渲染进开场处境）。
    pub fn with_situation(
        mut self,
        snapshots: Arc<dyn SnapshotSource>,
        chat_read: Arc<ChatReadMark>,
    ) -> Self {
        self.situation = Some(SituationInputs {
            snapshots,
            chat_read,
        });
        self
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

    fn situation_message(&self) -> Option<InputMessage> {
        let situation = self.situation.as_ref()?;
        let snapshot = situation.snapshots.latest();
        let text = render::render_situation(&snapshot, situation.chat_read.position());
        Some(InputMessage::text("system", format!("【当前处境】\n{text}")))
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
    fn base_context(&self) -> Vec<TranscriptItem> {
        let mut items = vec![
            InputMessage::text("system", self.persona.clone()).into(),
            self.memory_message().into(),
        ];
        if let Some(situation) = self.situation_message() {
            items.push(situation.into());
        }
        items
    }
}

impl Compaction for ContextStrategy {
    fn compact<'a>(
        &'a self,
        conversation: &'a [TranscriptItem],
    ) -> PortFuture<'a, Vec<TranscriptItem>> {
        Box::pin(async move {
            let unchanged = || conversation.to_vec();
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
            transcript.push(
                InputMessage::text("user", "请按上面的规则输出压缩 JSON。").into(),
            );

            let Ok(response) = model
                .complete(ModelRequest {
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
            vec![InputMessage::text(
                "user",
                format!("【我此前的经历记述（同伴第一人称，压缩自更早的对话）】\n{summary}"),
            )
            .into()]
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

    struct FixedSnapshots(world::TickSnapshot);

    impl world::SnapshotSource for FixedSnapshots {
        fn latest(&self) -> Arc<world::TickSnapshot> {
            Arc::new(self.0.clone())
        }
    }

    fn ready_snapshot() -> world::TickSnapshot {
        let mut snap =
            world::TickSnapshot::empty(world::Epoch(1), 40, world::ConnectionPhase::Ready);
        snap.world_meta.dimension = "minecraft:overworld".to_owned();
        snap.world_meta.day_time = 6_000;
        snap.self_state.alive = true;
        snap.self_state.health = 20.0;
        snap.self_state.food = 20.0;
        snap
    }

    #[test]
    fn situation_is_appended_after_the_stable_prefix() {
        let strategy = ContextStrategy::new(
            "人设",
            Arc::new(MemoryFile::new(scratch_dir().join("memory.md"))),
        )
        .with_situation(
            Arc::new(FixedSnapshots(ready_snapshot())),
            Arc::new(ChatReadMark::new()),
        );

        let base = strategy.base_context();
        assert_eq!(base.len(), 3);
        let (role, text) = text_of(&base[2]);
        assert_eq!(role, "system");
        assert!(text.contains("【当前处境】"));
        assert!(text.contains("正午前后"), "{text}");
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

        let compacted = strategy.compact(&conversation()).await;
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
        let compacted = strategy.compact(&original).await;
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
        assert_eq!(strategy.compact(&original).await, original);
        assert_eq!(memory.read().unwrap(), "旧记忆。");
    }

    #[tokio::test]
    async fn compaction_without_a_model_returns_the_conversation_as_is() {
        let strategy = ContextStrategy::new(
            "人设",
            Arc::new(MemoryFile::new(scratch_dir().join("memory.md"))),
        );
        let original = conversation();
        assert_eq!(strategy.compact(&original).await, original);
    }
}
