use serde_json::{json, Map, Value};

use crate::agent::{RunId, ToolCallId};

use super::{CapabilityInvocation, MoveDirection, MoveInputArguments};

pub const FIXTURE_STARTED_AT: &str = "2026-07-27T00:00:00.000Z";

pub fn capability_invocation() -> CapabilityInvocation {
    CapabilityInvocation {
        run_id: RunId::new("run-1").expect("fixture run id is valid"),
        tool_call_id: ToolCallId::new("call-1").expect("fixture tool call id is valid"),
        arguments: object(json!({"directions": ["forward"], "duration_ms": 250})),
        action_id: "action-1".to_owned(),
        started_at: FIXTURE_STARTED_AT.to_owned(),
    }
}

pub fn move_input_arguments() -> MoveInputArguments {
    MoveInputArguments {
        directions: vec![MoveDirection::Forward, MoveDirection::Right],
        duration_ms: 250,
        sprint: Some(false),
    }
}

fn object(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .cloned()
        .expect("fixture JSON value is an object")
}
