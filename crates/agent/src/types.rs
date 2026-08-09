//! 内核端口之间交换的服务商无关数据。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 由适配器或应用管理的可扩展 JSON 字段。
pub type JsonObject = serde_json::Map<String, Value>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentErrorKind {
    /// 状态机方法的调用顺序错误。
    InvalidState,
    /// 模型适配器无法生成可用的规范化响应。
    Model,
    /// 无法可靠地分发或完成整个工具批次。
    ToolDispatch,
    /// 调用与结果的 ID 未能一一对应。
    InvalidToolBatch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentError {
    pub kind: AgentErrorKind,
    pub summary: String,
}

impl AgentError {
    pub fn new(kind: AgentErrorKind, summary: impl Into<String>) -> Self {
        Self {
            kind,
            summary: summary.into(),
        }
    }
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.summary)
    }
}

impl std::error::Error for AgentError {}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ModelUsage {
    /// 服务商报告的输入 token；缓存 token 是其子集还是额外项由具体协议定义。
    pub input_tokens: Option<u64>,
    /// 模型生成的全部输出 token。
    pub output_tokens: Option<u64>,
    /// 服务商提供或 adapter 规范化后的总 token。
    pub total_tokens: Option<u64>,
    /// 从 prompt cache 读取的输入 token。
    pub cached_input_tokens: Option<u64>,
    /// 本次写入 prompt cache 的输入 token。
    pub cache_write_input_tokens: Option<u64>,
    /// 输出 token 中用于 reasoning/thinking 的子集。
    pub reasoning_output_tokens: Option<u64>,
}

impl ModelUsage {
    pub(crate) fn merge(&mut self, next: &Self) {
        fn add(total: &mut Option<u64>, value: Option<u64>) {
            if let Some(value) = value {
                *total = Some(total.unwrap_or(0).saturating_add(value));
            }
        }

        add(&mut self.input_tokens, next.input_tokens);
        add(&mut self.output_tokens, next.output_tokens);
        add(&mut self.total_tokens, next.total_tokens);
        add(&mut self.cached_input_tokens, next.cached_input_tokens);
        add(
            &mut self.cache_write_input_tokens,
            next.cache_write_input_tokens,
        );
        add(
            &mut self.reasoning_output_tokens,
            next.reasoning_output_tokens,
        );
    }
}

macro_rules! string_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

string_id!(
    /// 一次逻辑运行的稳定 ID。
    RunId
);
string_id!(
    /// 本地分发调用的服务商无关关联 ID。
    ///
    /// 适配器通常在此保留服务商签发的关联 ID。仅当服务商未提供可用 ID 时才生成 ID；
    /// 其他传输 ID 和签名应放入 `ToolCall::provider_data`。
    ToolCallId
);
string_id!(
    /// 模型和工具运行时使用的名称。
    ToolName
);
string_id!(
    /// 一次运行中由模型生成的一个工具批次的稳定 ID。
    ToolBatchId
);
string_id!(
    /// 一次模型流中候选工具批次的稳定 ID。
    ///
    /// 候选批次可能最终提交，也可能因模型流中断而终止；两种情况下都使用同一个 ID。
    /// 当前生成方式只保证单个 `AgentSession` 生命周期内唯一，不能直接充当跨进程或跨会话
    /// 的持久幂等键；远程 runtime 必须再加入自己的会话命名空间或执行账本键。
    ToolBatchAttemptId
);

impl From<ToolBatchAttemptId> for ToolBatchId {
    fn from(value: ToolBatchAttemptId) -> Self {
        Self::new(value.into_inner())
    }
}

impl From<&ToolBatchAttemptId> for ToolBatchId {
    fn from(value: &ToolBatchAttemptId) -> Self {
        Self::new(value.as_str())
    }
}

/// 工具调用在一个候选批次中的规范化序号。
///
/// 适配器负责把服务商的块索引或数组索引归一化为从零开始的工具序号。不同调用可以乱序
/// 完成，但在批次封口时必须恰好覆盖 `0..call_count`。
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ToolCallSlot(u32);

impl ToolCallSlot {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

/// 刻意保持精简的内容中间表示。未知的多模态或服务商原生内容块通过 `Opaque`
/// 无损保留，并且仅由模型适配器解释。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    Json { value: Value },
    Opaque { kind: String, data: Value },
}

impl ContentPart {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn json(value: impl Into<Value>) -> Self {
        Self::Json {
            value: value.into(),
        }
    }
}

/// 模型输入消息。`role` 特意采用开放字符串：信箱不假定其为 `user`，适配器可支持
/// 系统、开发者、应用自定义或未来新增的角色。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InputMessage {
    pub role: String,
    pub content: Vec<ContentPart>,
    #[serde(default, skip_serializing_if = "JsonObject::is_empty")]
    pub provider_data: JsonObject,
}

impl InputMessage {
    pub fn new(role: impl Into<String>, content: Vec<ContentPart>) -> Self {
        Self {
            role: role.into(),
            content,
            provider_data: JsonObject::new(),
        }
    }

    pub fn text(role: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(role, vec![ContentPart::text(text)])
    }
}

/// 可移植的本地分发函数工具定义。
///
/// 核心不会添加 OpenAI 的 `type: function` 外层结构或服务商托管工具。模型适配器负责将
/// 这一通用 JSON Schema 子集映射为相应的传输格式。`metadata` 对内核不透明，可携带供应用
/// 或适配器参考的扩展信息。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolDefinition {
    pub name: ToolName,
    pub description: Option<String>,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "JsonObject::is_empty")]
    pub metadata: JsonObject,
}

impl ToolDefinition {
    pub fn new(name: impl Into<String>, input_schema: Value) -> Self {
        Self {
            name: ToolName::new(name),
            description: None,
            input_schema,
            output_schema: None,
            metadata: JsonObject::new(),
        }
    }
}

/// 模型适配器生成的一个规范化本地函数调用。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolCall {
    pub id: ToolCallId,
    pub name: ToolName,
    /// 服务商提供 JSON 时保存解析结果；否则适配器可将原始输入保留为字符串或不透明的
    /// JSON 值，交由工具运行时校验。
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "JsonObject::is_empty")]
    pub provider_data: JsonObject,
}

impl ToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: Value) -> Self {
        Self {
            id: ToolCallId::new(id),
            name: ToolName::new(name),
            arguments,
            provider_data: JsonObject::new(),
        }
    }
}

/// 一次调用中传给工具运行时的完整有序批次。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolCallBatch {
    pub run_id: RunId,
    pub batch_id: ToolBatchId,
    pub calls: Vec<ToolCall>,
}

/// 增量工具运行时接收候选批次时的稳定上下文。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ToolBatchStart {
    pub run_id: RunId,
    pub batch_attempt_id: ToolBatchAttemptId,
}

/// 一个已完整解析、可交给增量工具运行时接管的调用。
///
/// 此结构不表示调用已经执行，也不表示它是批次的最后一项。批次是否已经传输完整由随后
/// 独立的 `calls_sealed` 操作声明。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct IncrementalToolCall {
    pub run_id: RunId,
    pub batch_attempt_id: ToolBatchAttemptId,
    pub slot: ToolCallSlot,
    pub call: ToolCall,
}

/// 候选工具批次终止的原因。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolBatchAbortReason {
    /// 模型传输失败、返回 `incomplete`，或在成功终态前结束。
    ModelStreamInterrupted,
    /// 流事件与最终聚合响应不一致，不能安全提交。
    ModelOutputMismatch,
    /// 工具运行时在接管调用或批次封口时返回错误。
    ToolRuntimeRejected,
}

/// 批次中断后，工具运行时对一个已经传给 `submit` 的调用给出的冻结结论。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "status", content = "value", rename_all = "snake_case")]
pub enum AbortedToolCallOutcome {
    /// 已经获得可信的普通工具结果。
    Settled(ToolResult),
    /// 运行时确认该调用尚未开始且以后也不会开始。
    CancelledBeforeStart,
    /// 调用可能已经产生影响，但运行时无法确定最终结果。
    OutcomeUnknown { summary: String },
}

/// 中断批次中一个已经尝试提交给 runtime 的调用及其冻结结论。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AbortedToolCall {
    pub slot: ToolCallSlot,
    pub call_id: ToolCallId,
    pub outcome: AbortedToolCallOutcome,
}

/// 增量工具运行时在 `abort` 时返回的完整冻结报告。
///
/// `calls` 必须对每个已经传给 `submit` 的 slot 恰好给出一个结论，包括 `submit` 因确认
/// 不确定而返回 `Err` 的当前调用；返回后不得再执行该批次中的任何工作，也不得再产生
/// 迟到结果。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AbortedToolBatch {
    pub batch_attempt_id: ToolBatchAttemptId,
    pub calls: Vec<AbortedToolCall>,
}

/// 写入下一次正常模型输入的中断批次执行事实。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InterruptedToolBatchReceipt {
    pub batch_attempt_id: ToolBatchAttemptId,
    pub calls: Vec<InterruptedToolCallReceipt>,
}

/// 一个需要在恢复消息中告知模型的已发生或结果不确定调用。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InterruptedToolCallReceipt {
    pub slot: ToolCallSlot,
    pub call_id: ToolCallId,
    pub name: ToolName,
    pub arguments: Value,
    pub outcome: InterruptedToolCallOutcome,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "status", content = "value", rename_all = "snake_case")]
pub enum InterruptedToolCallOutcome {
    Settled(ToolResult),
    OutcomeUnknown { summary: String },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultStatus {
    Success,
    Error,
}

/// 一个已完成的调用结果。单次调用失败属于普通结果；仅在无法获得可信的完整批次时，
/// 才使用顶层分发错误。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolResult {
    pub call_id: ToolCallId,
    pub status: ToolResultStatus,
    pub content: Vec<ContentPart>,
    #[serde(default, skip_serializing_if = "JsonObject::is_empty")]
    pub metadata: JsonObject,
}

impl ToolResult {
    pub fn success(call_id: ToolCallId, content: Vec<ContentPart>) -> Self {
        Self {
            call_id,
            status: ToolResultStatus::Success,
            content,
            metadata: JsonObject::new(),
        }
    }

    pub fn success_json(call_id: ToolCallId, value: Value) -> Self {
        Self::success(call_id, vec![ContentPart::json(value)])
    }

    pub fn failure(call_id: ToolCallId, summary: impl Into<String>) -> Self {
        Self {
            call_id,
            status: ToolResultStatus::Error,
            content: vec![ContentPart::text(summary)],
            metadata: JsonObject::new(),
        }
    }
}

/// 一个已分发批次的完整结果。实现可以按任意顺序返回；状态机在提交对话记录前校验 ID，
/// 并恢复模型生成调用时的顺序。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ToolResultBatch {
    pub results: Vec<ToolResult>,
}

/// 一个规范化模型输出。本地函数调用采用强类型表示；精确回放服务商响应所需的全部信息
///（推理 ID、签名、响应 ID 等）保留在服务商数据或不透明内容中。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ModelOutput {
    pub content: Vec<ContentPart>,
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "JsonObject::is_empty")]
    pub provider_data: JsonObject,
}

impl ModelOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentPart::text(text)],
            ..Self::default()
        }
    }

    pub fn calls(calls: Vec<ToolCall>) -> Self {
        Self {
            tool_calls: calls,
            ..Self::default()
        }
    }

    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                ContentPart::Json { .. } | ContentPart::Opaque { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

/// 会话保存的规范对话记录，并非任何服务商的请求传输格式。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum TranscriptItem {
    Input(InputMessage),
    ModelOutput(ModelOutput),
    ToolResults(ToolResultBatch),
}

impl From<InputMessage> for TranscriptItem {
    fn from(message: InputMessage) -> Self {
        Self::Input(message)
    }
}

/// 验证对话记录中的每个工具轮都已闭合。含调用的模型输出必须与紧随其后的一个完整、
/// 同序结果批次成对出现；调用 ID 只在各自批内唯一，因此两个已闭合分段可以安全拼接。
pub(crate) fn validate_closed_transcript(transcript: &[TranscriptItem]) -> Result<(), AgentError> {
    let mut index = 0;

    while index < transcript.len() {
        match &transcript[index] {
            TranscriptItem::Input(_) => index += 1,
            TranscriptItem::ModelOutput(output) if output.tool_calls.is_empty() => index += 1,
            TranscriptItem::ModelOutput(output) => {
                let mut pending = Vec::with_capacity(output.tool_calls.len());
                let mut batch_call_ids = HashSet::with_capacity(output.tool_calls.len());
                for call in &output.tool_calls {
                    if call.id.as_str().is_empty() {
                        return Err(AgentError::new(
                            AgentErrorKind::InvalidToolBatch,
                            "empty_tool_call_id",
                        ));
                    }
                    if !batch_call_ids.insert(call.id.clone()) {
                        return Err(AgentError::new(
                            AgentErrorKind::InvalidToolBatch,
                            "duplicate_tool_call_id",
                        ));
                    }
                    pending.push(call.id.clone());
                }

                let Some(next) = transcript.get(index + 1) else {
                    return Err(AgentError::new(
                        AgentErrorKind::InvalidToolBatch,
                        "dangling_tool_calls",
                    ));
                };
                let TranscriptItem::ToolResults(results) = next else {
                    return Err(AgentError::new(
                        AgentErrorKind::InvalidToolBatch,
                        "tool_results_not_immediately_after_tool_calls",
                    ));
                };

                let ordered = order_tool_results(&pending, results.results.clone())?;
                if ordered
                    .iter()
                    .zip(&results.results)
                    .any(|(expected, actual)| expected.call_id != actual.call_id)
                {
                    return Err(AgentError::new(
                        AgentErrorKind::InvalidToolBatch,
                        "tool_result_order_mismatch",
                    ));
                }
                index += 2;
            }
            TranscriptItem::ToolResults(_) => {
                return Err(AgentError::new(
                    AgentErrorKind::InvalidToolBatch,
                    "orphan_tool_results",
                ));
            }
        }
    }

    Ok(())
}

/// 在严格保证 ID 一一对应的同时恢复结果顺序。
pub(crate) fn order_tool_results(
    pending: &[ToolCallId],
    results: Vec<ToolResult>,
) -> Result<Vec<ToolResult>, AgentError> {
    if pending.len() != results.len() {
        return Err(AgentError::new(
            AgentErrorKind::InvalidToolBatch,
            "tool_result_count_mismatch",
        ));
    }

    let mut by_id = HashMap::with_capacity(results.len());
    for result in results {
        if by_id.insert(result.call_id.clone(), result).is_some() {
            return Err(AgentError::new(
                AgentErrorKind::InvalidToolBatch,
                "duplicate_tool_result_id",
            ));
        }
    }

    let ordered = pending
        .iter()
        .map(|id| {
            by_id.remove(id).ok_or_else(|| {
                AgentError::new(AgentErrorKind::InvalidToolBatch, "missing_tool_result_id")
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    debug_assert!(by_id.is_empty());
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calls(ids: &[&str]) -> TranscriptItem {
        TranscriptItem::ModelOutput(ModelOutput::calls(
            ids.iter()
                .map(|id| ToolCall::new(*id, "test", Value::Null))
                .collect(),
        ))
    }

    fn results(ids: &[&str]) -> TranscriptItem {
        TranscriptItem::ToolResults(ToolResultBatch {
            results: ids
                .iter()
                .map(|id| ToolResult::success_json(ToolCallId::new(*id), Value::Null))
                .collect(),
        })
    }

    #[test]
    fn result_batch_is_validated_and_restored_to_call_order() {
        let pending = vec![ToolCallId::new("a"), ToolCallId::new("b")];
        let reversed = vec![
            ToolResult::success_json(ToolCallId::new("b"), Value::from(2)),
            ToolResult::success_json(ToolCallId::new("a"), Value::from(1)),
        ];

        let ordered = order_tool_results(&pending, reversed).unwrap();
        assert_eq!(ordered[0].call_id.as_str(), "a");
        assert_eq!(ordered[1].call_id.as_str(), "b");
    }

    #[test]
    fn result_batch_rejects_count_duplicate_and_mismatched_ids() {
        let pending = vec![ToolCallId::new("a"), ToolCallId::new("b")];
        let count_error = order_tool_results(
            &pending,
            vec![ToolResult::success_json(ToolCallId::new("a"), Value::Null)],
        )
        .unwrap_err();
        assert_eq!(count_error.summary, "tool_result_count_mismatch");

        let duplicate_error = order_tool_results(
            &pending,
            vec![
                ToolResult::success_json(ToolCallId::new("a"), Value::Null),
                ToolResult::success_json(ToolCallId::new("a"), Value::Null),
            ],
        )
        .unwrap_err();
        assert_eq!(duplicate_error.summary, "duplicate_tool_result_id");

        let mismatch_error = order_tool_results(
            &pending,
            vec![
                ToolResult::success_json(ToolCallId::new("a"), Value::Null),
                ToolResult::success_json(ToolCallId::new("unknown"), Value::Null),
            ],
        )
        .unwrap_err();
        assert_eq!(mismatch_error.summary, "missing_tool_result_id");
    }

    #[test]
    fn closed_transcript_accepts_independent_items_and_complete_tool_rounds() {
        let transcript = vec![
            InputMessage::text("user", "start").into(),
            TranscriptItem::ModelOutput(ModelOutput::text("answer")),
            calls(&["a", "b"]),
            results(&["a", "b"]),
            InputMessage::text("developer", "continue").into(),
        ];

        validate_closed_transcript(&transcript).unwrap();
    }

    #[test]
    fn closed_transcript_rejects_unpaired_tool_items() {
        let dangling = validate_closed_transcript(&[calls(&["a"])]).unwrap_err();
        assert_eq!(dangling.kind, AgentErrorKind::InvalidToolBatch);
        assert_eq!(dangling.summary, "dangling_tool_calls");

        let interleaved = validate_closed_transcript(&[
            calls(&["a"]),
            InputMessage::text("user", "interrupt").into(),
            results(&["a"]),
        ])
        .unwrap_err();
        assert_eq!(interleaved.kind, AgentErrorKind::InvalidToolBatch);
        assert_eq!(
            interleaved.summary,
            "tool_results_not_immediately_after_tool_calls"
        );

        let orphan = validate_closed_transcript(&[results(&["a"])]).unwrap_err();
        assert_eq!(orphan.kind, AgentErrorKind::InvalidToolBatch);
        assert_eq!(orphan.summary, "orphan_tool_results");
    }

    #[test]
    fn closed_transcript_rejects_invalid_call_and_result_ids() {
        let empty = validate_closed_transcript(&[calls(&[""]), results(&[""])]).unwrap_err();
        assert_eq!(empty.summary, "empty_tool_call_id");

        let duplicate =
            validate_closed_transcript(&[calls(&["a", "a"]), results(&["a", "a"])]).unwrap_err();
        assert_eq!(duplicate.summary, "duplicate_tool_call_id");

        let incomplete =
            validate_closed_transcript(&[calls(&["a", "b"]), results(&["a"])]).unwrap_err();
        assert_eq!(incomplete.summary, "tool_result_count_mismatch");

        let duplicate_result =
            validate_closed_transcript(&[calls(&["a", "b"]), results(&["a", "a"])]).unwrap_err();
        assert_eq!(duplicate_result.summary, "duplicate_tool_result_id");

        let unknown = validate_closed_transcript(&[calls(&["a", "b"]), results(&["a", "unknown"])])
            .unwrap_err();
        assert_eq!(unknown.summary, "missing_tool_result_id");

        let reversed =
            validate_closed_transcript(&[calls(&["a", "b"]), results(&["b", "a"])]).unwrap_err();
        assert_eq!(reversed.kind, AgentErrorKind::InvalidToolBatch);
        assert_eq!(reversed.summary, "tool_result_order_mismatch");
    }

    #[test]
    fn closed_transcript_segments_compose_when_call_ids_are_reused() {
        let first = vec![calls(&["same"]), results(&["same"])];
        let second = vec![calls(&["same"]), results(&["same"])];
        validate_closed_transcript(&first).unwrap();
        validate_closed_transcript(&second).unwrap();

        let combined = first.into_iter().chain(second).collect::<Vec<_>>();
        validate_closed_transcript(&combined).unwrap();
    }
}
