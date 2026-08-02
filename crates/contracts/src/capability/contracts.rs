use std::{collections::BTreeMap, sync::Arc, time::Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::{
    AgentError, AgentErrorCode, ContractFuture, ExecutionControl, JsonObject, RunId, ToolCallId,
    ToolExecution, ToolInvocation, WireToolDefinition,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionResource {
    Body,
    Chat,
    Memory,
    Viewport,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ToolResultProtocol {
    #[default]
    #[serde(rename = "mineintent.tool-result.v1")]
    V1,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CapabilityInvocation {
    pub run_id: RunId,
    pub tool_call_id: ToolCallId,
    pub arguments: JsonObject,
    pub action_id: String,
    pub started_at: String,
}

pub trait ScopeGuard: Send + Sync {
    fn check_current(&self) -> Result<(), AgentError>;
    fn is_current(&self) -> bool;
}

#[derive(Clone, Copy)]
pub struct CapabilityExecutionContext<'a> {
    world_id: &'a str,
    chat_event_id: &'a str,
    control: ExecutionControl<'a>,
    scope_guard: &'a dyn ScopeGuard,
}

impl<'a> CapabilityExecutionContext<'a> {
    pub fn new(
        world_id: &'a str,
        chat_event_id: &'a str,
        control: ExecutionControl<'a>,
        scope_guard: &'a dyn ScopeGuard,
    ) -> Self {
        Self {
            world_id,
            chat_event_id,
            control,
            scope_guard,
        }
    }

    pub fn world_id(self) -> &'a str {
        self.world_id
    }

    pub fn chat_event_id(self) -> &'a str {
        self.chat_event_id
    }

    pub fn control(self) -> ExecutionControl<'a> {
        self.control
    }

    pub fn check_at(self, now: Instant) -> Result<(), AgentError> {
        self.control.check_at(now)?;
        self.scope_guard.check_current()
    }

    pub fn is_current(self) -> bool {
        self.scope_guard.is_current()
    }
}

pub trait ToolCapability: Send + Sync {
    fn definition(&self) -> &WireToolDefinition;
    fn resource(&self) -> Option<ExecutionResource>;

    fn execute<'a>(
        &'a self,
        invocation: CapabilityInvocation,
        context: CapabilityExecutionContext<'a>,
    ) -> ContractFuture<'a, Result<Value, AgentError>>;
}

pub struct ToolCapabilityRegistry {
    capabilities: Vec<Arc<dyn ToolCapability>>,
    dispatch: BTreeMap<String, Arc<dyn ToolCapability>>,
}

impl ToolCapabilityRegistry {
    pub fn new(capabilities: Vec<Arc<dyn ToolCapability>>) -> Result<Self, AgentError> {
        let mut dispatch = BTreeMap::new();
        for capability in &capabilities {
            let name = capability.definition().function.name.as_str().to_owned();
            if dispatch
                .insert(name.clone(), Arc::clone(capability))
                .is_some()
            {
                return Err(AgentError::new(
                    AgentErrorCode::DuplicateToolCapability,
                    format!("duplicate_tool_capability:{name}"),
                ));
            }
        }

        Ok(Self {
            capabilities,
            dispatch,
        })
    }

    pub fn definitions(&self) -> Vec<WireToolDefinition> {
        self.capabilities
            .iter()
            .map(|capability| capability.definition().clone())
            .collect()
    }

    pub fn resolve(&self, name: &str) -> Option<Arc<dyn ToolCapability>> {
        self.dispatch.get(name).cloned()
    }

    pub fn len(&self) -> usize {
        self.capabilities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.capabilities.is_empty()
    }
}

pub trait ToolDispatcher: Send + Sync {
    type Observation: Send;

    /// Classifies a parsed invocation using the capability/resource registry.
    ///
    /// Implementations should return `None` when the invocation cannot be
    /// resolved.  The agent driver never infers this from a tool name or JSON
    /// arguments.  The driver isolates a classifier panic as the paired
    /// `tool_dispatch_panicked` result without guessing a resource.  The
    /// default keeps existing dispatchers source-compatible until the
    /// production dispatcher is assembled.
    fn resource(&self, _invocation: &ToolInvocation) -> Option<ExecutionResource> {
        None
    }

    fn dispatch<'a>(
        &'a self,
        invocation: ToolInvocation,
        control: ExecutionControl<'a>,
    ) -> ContractFuture<'a, Result<ToolExecution<Self::Observation>, AgentError>>;
}
