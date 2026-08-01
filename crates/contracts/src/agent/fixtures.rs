use serde_json::{json, Map, Value};

use super::{
    AgentContextProtocol, AgentDecisionContext, AgentEvent, AgentFrame, AgentRunRequest, AgentSelf,
    AgentWorld, FunctionToolDefinition, JsonAgentDecisionContext, JsonAgentFrame, ModelName,
    ModelRunResult, ModelUsage, PlayerMessage, PromptTemplateKey, PromptTemplateRef,
    PromptTemplateVersion, RequiredNullable, RunId, StableContext, ToolCallId, ToolDefinitionName,
    ToolDefinitionType, ToolExecution, ToolInvocation, ToolName, ToolResponseProtocol,
    WireToolDefinition,
};

pub const FIXTURE_AT: &str = "2026-07-25T00:00:00Z";

pub fn agent_frame() -> JsonAgentFrame {
    AgentFrame {
        at: FIXTURE_AT.to_owned(),
        player: Some(PlayerMessage {
            username: "Alex".to_owned(),
            text: "看看羊".to_owned(),
        }),
        world: AgentWorld {
            dimension: "overworld".to_owned(),
            time_of_day: None,
        },
        self_state: Some(AgentSelf {
            position: [0.0, 64.0, 0.0],
            yaw_degrees: 0.0,
            pitch_degrees: 0.0,
        }),
        status: None,
        inventory: None,
        sound: None,
        events: Vec::<AgentEvent>::new(),
        omitted_events: None,
        omissions: Vec::<Value>::new(),
    }
}

pub fn agent_context() -> JsonAgentDecisionContext {
    AgentDecisionContext {
        protocol: AgentContextProtocol::V3,
        stable: StableContext {
            memories: json!([{
                "kind": "note",
                "summary": "玩家怕高",
                "createdAt": "2026-07-01T00:00:00Z"
            }]),
        },
        frame: agent_frame(),
    }
}

pub fn tool_definition() -> WireToolDefinition {
    WireToolDefinition {
        r#type: ToolDefinitionType::Function,
        function: FunctionToolDefinition {
            name: ToolDefinitionName::new("look_relative")
                .expect("fixture advertised tool name is valid"),
            description: "转头".to_owned(),
            parameters: object(json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })),
        },
    }
}

pub fn tool_invocation() -> ToolInvocation {
    ToolInvocation {
        run_id: RunId::new("run-1").expect("fixture run id is valid"),
        tool_call_id: ToolCallId::new("call-1").expect("fixture tool call id is valid"),
        name: ToolName::new("look_relative").expect("fixture tool name is valid"),
        arguments: object(json!({"yaw_degrees": 10, "pitch_degrees": 0})),
    }
}

pub fn tool_execution() -> ToolExecution<JsonAgentFrame> {
    ToolExecution {
        protocol: ToolResponseProtocol::V2,
        result: json!({"status": "completed"}),
        observation_after: RequiredNullable::new(Some(agent_frame())),
    }
}

pub fn prompt_template() -> PromptTemplateRef {
    PromptTemplateRef {
        key: PromptTemplateKey::new("participant-system")
            .expect("fixture prompt template key is valid"),
        version: PromptTemplateVersion::new("v1")
            .expect("fixture prompt template version is valid"),
    }
}

pub fn agent_run_request() -> AgentRunRequest<JsonAgentDecisionContext> {
    AgentRunRequest {
        run_id: RunId::new("run-1").expect("fixture run id is valid"),
        context: agent_context(),
        tools: vec![tool_definition()],
        prompt_template: prompt_template(),
    }
}

pub fn model_run_result() -> ModelRunResult {
    ModelRunResult {
        protocol: super::AgentRunProtocol::V1,
        model: ModelName::new("deepseek-chat").expect("fixture model name is valid"),
        usage: Some(ModelUsage {
            input_tokens: Some(12),
            output_tokens: Some(8),
            cache_read_tokens: Some(0),
            cache_write_tokens: None,
        }),
    }
}

fn object(value: Value) -> Map<String, Value> {
    value
        .as_object()
        .cloned()
        .expect("fixture JSON value is an object")
}
