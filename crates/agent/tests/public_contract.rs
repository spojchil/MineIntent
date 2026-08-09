//! 从外部使用者视角验证最重要的公开状态机契约。

use agent::{
    InputMessage, ModelOutput, ModelResponse, RequestBoundaryKind, RunId, ToolCall, ToolCallId,
    ToolResult, ToolResultBatch, TranscriptItem, Turn, TurnStep,
};
use serde_json::json;

#[test]
fn public_api_exposes_one_complete_tool_batch_and_ordered_results() {
    let mut turn = Turn::new(RunId::new("public-run"), Vec::new());
    assert_eq!(
        turn.next_step().unwrap(),
        TurnStep::RequestBoundary {
            kind: RequestBoundaryKind::BeforeModelRequest,
        }
    );
    turn.resume_boundary(vec![InputMessage::text("operator", "开始").into()])
        .unwrap();
    assert!(matches!(
        turn.next_step().unwrap(),
        TurnStep::CallModel { .. }
    ));

    turn.model_response(ModelResponse {
        output: ModelOutput::calls(vec![
            ToolCall::new("call-a", "read", json!({"path": "a"})),
            ToolCall::new("call-b", "read", json!({"path": "b"})),
        ]),
        ..ModelResponse::default()
    })
    .unwrap();

    let TurnStep::DispatchTools { batch } = turn.next_step().unwrap() else {
        panic!("应一次公开完整工具批");
    };
    assert_eq!(batch.calls.len(), 2);

    turn.tool_results(ToolResultBatch {
        results: vec![
            ToolResult::success_json(ToolCallId::new("call-b"), json!(2)),
            ToolResult::success_json(ToolCallId::new("call-a"), json!(1)),
        ],
    })
    .unwrap();

    let TranscriptItem::ToolResults(results) = turn.transcript().last().unwrap() else {
        panic!("结果批应写入公开 transcript");
    };
    assert_eq!(results.results[0].call_id.as_str(), "call-a");
    assert_eq!(results.results[1].call_id.as_str(), "call-b");
}
