use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::information::{RelativeDirection, SoundObservation, SoundValues};

use super::{
    AgentChatItemV5, AgentChatMessageV5, AgentChatMovedMarkerV5, AgentChatMovedV5, AgentChatV5,
    AgentContextProtocolV3, AgentContextProtocolV4, AgentContextProtocolV5, AgentDecisionContextV3,
    AgentDecisionContextV4, AgentDecisionContextV5, AgentEvent, AgentEventV5, AgentFrame,
    AgentFrameV5, AgentHotbarV5, AgentItemStackV5, AgentPoseV5, AgentRunRequest, AgentSelf,
    AgentStatusV5, AgentWorld, AgentWorldV5, FunctionToolDefinition, JsonAgentDecisionContext,
    JsonAgentDecisionContextV4, JsonAgentDecisionContextV5, JsonAgentFrame, ModelName,
    ModelRunResult, ModelUsage, PlayerMessage, PromptTemplateKey, PromptTemplateRef,
    PromptTemplateVersion, RequiredNullable, RunId, StableContextV3, StableContextV4, ToolCallId,
    ToolDefinitionName, ToolDefinitionType, ToolExecution, ToolInvocation, ToolName,
    ToolResponseProtocol, WireToolDefinition,
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
    AgentDecisionContextV3 {
        protocol: AgentContextProtocolV3,
        stable: StableContextV3 {
            memories: json!([{
                "kind": "note",
                "summary": "玩家怕高",
                "createdAt": "2026-07-01T00:00:00Z"
            }]),
        },
        frame: agent_frame(),
    }
}

pub fn agent_context_v4() -> JsonAgentDecisionContextV4 {
    AgentDecisionContextV4 {
        protocol: AgentContextProtocolV4,
        stable: StableContextV4 {
            memory: "玩家怕高".to_owned(),
        },
        frame: agent_frame(),
    }
}

pub fn agent_context_v5() -> JsonAgentDecisionContextV5 {
    let bob = AgentChatMessageV5 {
        username: "bob".to_owned(),
        text: "早".to_owned(),
        at: FIXTURE_AT.to_owned(),
    };
    let alice = AgentChatMessageV5 {
        username: "alice".to_owned(),
        text: "帮我看看农田".to_owned(),
        at: FIXTURE_AT.to_owned(),
    };
    let mut slots = BTreeMap::new();
    slots.insert(
        0,
        AgentItemStackV5::new("oak_log", 12).expect("fixture item is valid"),
    );
    slots.insert(
        2,
        AgentItemStackV5::new("iron_sword", 1).expect("fixture item is valid"),
    );
    AgentDecisionContextV5 {
        protocol: AgentContextProtocolV5,
        stable: StableContextV4 {
            memory: "玩家怕高".to_owned(),
        },
        frame: AgentFrameV5 {
            at: FIXTURE_AT.to_owned(),
            world: AgentWorldV5 {
                dimension: "minecraft:overworld".to_owned(),
            },
            pose: AgentPoseV5 {
                position: [0.5, 64.0, -7.5],
                yaw_degrees: 0.0,
                pitch_degrees: 0.0,
            },
            status: Some(AgentStatusV5 {
                health: 20.0,
                food: 20.0,
                armor: Some(15),
            }),
            hotbar: AgentHotbarV5 {
                selected: 2,
                slots,
                off_hand: Some(AgentItemStackV5::new("shield", 1).expect("fixture item is valid")),
            },
            chat: Some(AgentChatV5 {
                items: vec![
                    AgentChatItemV5::Message(bob),
                    AgentChatItemV5::Moved(AgentChatMovedV5 {
                        username: alice.username.clone(),
                        at: alice.at.clone(),
                        moved: AgentChatMovedMarkerV5::Events,
                    }),
                ],
                omitted: 0,
            }),
            sound: Some(SoundValues {
                recent_sounds: Some(vec![SoundObservation {
                    sound_name: Some("block.note_block.harp".to_owned()),
                    category: Some("record".to_owned()),
                    distance: 4.5,
                    direction: RelativeDirection::Ahead,
                    volume: 1.0,
                    pitch: 0.8,
                    observed_at: "2026-08-01T00:00:20Z".to_owned(),
                }]),
            }),
            light: Some(12),
            events: Some(vec![AgentEventV5::player_chat(alice)]),
            omissions: None,
        },
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

pub fn agent_run_request_v4() -> AgentRunRequest<JsonAgentDecisionContextV4> {
    AgentRunRequest {
        run_id: RunId::new("run-1").expect("fixture run id is valid"),
        context: agent_context_v4(),
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
