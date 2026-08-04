//! Budgeted Information catalog/tool session adapters.

use std::sync::{Arc, Mutex};

use mineintent_contracts::minecraft::{BoxFuture, OperationControl};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use tokio::time::{sleep, Duration};

use super::{
    contracts::{
        InformationCatalogResult, InformationErrorCode, InformationErrorProtocol,
        InformationGrantPurpose, InformationRequestError, InformationToolResult,
        InformationToolSessionBudget, InformationToolSessionContext, TrustedInformationCaller,
    },
    control::{child_operation_control, pending_unit},
    support::{javascript_json_bytes, parse_javascript_date_millis},
    InformationClock, SystemInformationClock,
};

/// Runtime-facing port kept deliberately smaller than the concrete runtime, matching the TS
/// adapter boundary and making the session independently testable.
pub trait InformationRuntimePort: Send + Sync {
    fn catalog(
        &self,
        caller: &TrustedInformationCaller,
        request: &str,
    ) -> Result<InformationCatalogResult, InformationRequestError>;

    fn query<'a>(
        &'a self,
        caller: &'a TrustedInformationCaller,
        request: &'a str,
        control: OperationControl,
    ) -> BoxFuture<'a, InformationToolResult>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InformationToolCallKind {
    Catalog,
    Help,
    Read,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum InformationToolSessionInitError {
    #[error("invalid information tool session budget")]
    InvalidBudget,
    #[error("invalid information tool session deadline")]
    InvalidDeadline,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InformationToolSessionUsage {
    pub calls: u64,
    pub read_calls: u64,
    pub returned_bytes: u64,
}

#[derive(Default)]
struct UsageState {
    usage: InformationToolSessionUsage,
}

pub struct InformationToolSession {
    context: InformationToolSessionContext,
    deadline_millis: i64,
    clock: Arc<dyn InformationClock>,
    usage: Arc<Mutex<UsageState>>,
}

impl InformationToolSession {
    pub fn new(
        context: InformationToolSessionContext,
    ) -> Result<Self, InformationToolSessionInitError> {
        Self::with_clock(context, Arc::new(SystemInformationClock))
    }

    pub fn with_clock(
        context: InformationToolSessionContext,
        clock: Arc<dyn InformationClock>,
    ) -> Result<Self, InformationToolSessionInitError> {
        validate_budget(&context.budget)?;
        let deadline_millis = parse_javascript_date_millis(&context.budget.deadline_at)
            .ok_or(InformationToolSessionInitError::InvalidDeadline)?;
        Ok(Self {
            context,
            deadline_millis,
            clock,
            usage: Arc::new(Mutex::new(UsageState::default())),
        })
    }

    pub fn context(&self) -> &InformationToolSessionContext {
        &self.context
    }

    pub fn reserve(&self, kind: InformationToolCallKind) -> Option<InformationRequestError> {
        if self.clock.now_millis() >= self.deadline_millis {
            return Some(session_error(
                InformationErrorCode::DeadlineExceeded,
                "The information tool session deadline elapsed.",
            ));
        }
        let mut state = match self.usage.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if state.usage.calls >= self.context.budget.max_calls {
            return Some(session_error(
                InformationErrorCode::BudgetExceeded,
                "The information tool call budget is exhausted.",
            ));
        }
        if kind == InformationToolCallKind::Read
            && state.usage.read_calls >= self.context.budget.max_read_calls
        {
            return Some(session_error(
                InformationErrorCode::BudgetExceeded,
                "The information read budget is exhausted.",
            ));
        }
        state.usage.calls = state.usage.calls.saturating_add(1);
        if kind == InformationToolCallKind::Read {
            state.usage.read_calls = state.usage.read_calls.saturating_add(1);
        }
        None
    }

    pub fn record<T: Serialize>(&self, result: &T) -> Option<InformationRequestError> {
        let bytes = javascript_json_bytes(result)
            .map(|bytes| bytes.len() as u64)
            .unwrap_or(u64::MAX);
        let mut state = match self.usage.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.usage.returned_bytes = state.usage.returned_bytes.saturating_add(bytes);
        if state.usage.returned_bytes > self.context.budget.max_returned_bytes {
            Some(session_error(
                InformationErrorCode::BudgetExceeded,
                "The information result byte budget is exhausted.",
            ))
        } else {
            None
        }
    }

    pub fn usage(&self) -> InformationToolSessionUsage {
        match self.usage.lock() {
            Ok(state) => state.usage,
            Err(poisoned) => poisoned.into_inner().usage,
        }
    }

    pub fn caller(&self) -> TrustedInformationCaller {
        TrustedInformationCaller {
            principal_id: self.context.principal_id.clone(),
            grant_id: self.context.grant_id.clone(),
            purpose: InformationGrantPurpose::ModelTool,
            correlation_id: self.context.correlation_id.clone(),
            decision_run_id: Some(self.context.decision_run_id.clone()),
            controller_lease_id: None,
        }
    }

    /// Runs one operation with a child control.  Upstream cancellation/deadline and the session
    /// wall-clock deadline are forwarded to that child before the operation is awaited further,
    /// allowing a well-behaved provider to observe the same boundary.
    pub async fn run_operation<'a, Result, Operation>(
        &'a self,
        upstream: OperationControl,
        operation: Operation,
    ) -> Result
    where
        Operation: FnOnce(OperationControl) -> BoxFuture<'a, Result>,
    {
        let (child, child_cancellation, child_deadline) = child_operation_control();
        let remaining = self.deadline_millis.saturating_sub(self.clock.now_millis());
        if upstream.cancellation().is_cancelled() {
            child_cancellation.trigger();
        }
        if upstream
            .deadline()
            .is_some_and(|deadline| deadline.has_elapsed())
            || remaining <= 0
        {
            child_deadline.trigger();
        }

        let operation_future = operation(child);
        let mut upstream_cancelled = upstream.cancelled();
        let mut upstream_deadline = upstream.deadline_elapsed().unwrap_or_else(pending_unit);
        let mut session_timer = Box::pin(if remaining > 0 {
            sleep(Duration::from_millis(
                remaining.min(2_147_483_647_i64) as u64
            ))
        } else {
            sleep(Duration::from_millis(0))
        });
        tokio::pin!(operation_future);
        tokio::select! {
            result = &mut operation_future => result,
            _ = &mut upstream_cancelled => {
                child_cancellation.trigger();
                operation_future.await
            }
            _ = &mut upstream_deadline => {
                child_deadline.trigger();
                operation_future.await
            }
            _ = &mut session_timer => {
                child_deadline.trigger();
                operation_future.await
            }
        }
    }
}

pub struct InformationCatalogTool<'a> {
    name: &'static str,
    runtime: &'a dyn InformationRuntimePort,
}

impl<'a> InformationCatalogTool<'a> {
    pub fn new(runtime: &'a dyn InformationRuntimePort) -> Self {
        Self {
            name: "information_catalog",
            runtime,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn invoke(
        &self,
        input: &str,
        session: &InformationToolSession,
    ) -> Result<InformationCatalogResult, InformationRequestError> {
        if let Some(error) = session.reserve(InformationToolCallKind::Catalog) {
            return Err(error);
        }
        let result = self.runtime.catalog(&session.caller(), input);
        let record_error = match &result {
            Ok(result) => session.record(result),
            Err(error) => session.record(error),
        };
        if let Some(error) = record_error {
            return Err(error);
        }
        result
    }
}

pub struct InformationTool<'a> {
    name: &'static str,
    runtime: &'a dyn InformationRuntimePort,
}

impl<'a> InformationTool<'a> {
    pub fn new(runtime: &'a dyn InformationRuntimePort) -> Self {
        Self {
            name: "information",
            runtime,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub async fn invoke(
        &self,
        input: &str,
        session: &InformationToolSession,
        upstream: OperationControl,
    ) -> InformationToolResult {
        let kind = if is_read_request(input) {
            InformationToolCallKind::Read
        } else {
            InformationToolCallKind::Help
        };
        if let Some(error) = session.reserve(kind) {
            return InformationToolResult::Error(error);
        }
        let caller = session.caller();
        let runtime = self.runtime;
        let result = session
            .run_operation(upstream, move |control| {
                Box::pin(async move { runtime.query(&caller, input, control).await })
            })
            .await;
        if let Some(error) = session.record(&result) {
            return InformationToolResult::Error(error);
        }
        result
    }
}

fn validate_budget(
    budget: &InformationToolSessionBudget,
) -> Result<(), InformationToolSessionInitError> {
    if budget.max_calls < 1 || budget.max_returned_bytes < 1 {
        return Err(InformationToolSessionInitError::InvalidBudget);
    }
    if budget.deadline_at.trim().is_empty() {
        return Err(InformationToolSessionInitError::InvalidDeadline);
    }
    Ok(())
}

fn is_read_request(input: &str) -> bool {
    serde_json::from_str::<Value>(input)
        .ok()
        .and_then(|value| {
            value
                .get("operation")
                .and_then(Value::as_str)
                .map(|op| op == "read")
        })
        .unwrap_or(false)
}

fn session_error(code: InformationErrorCode, message: &str) -> InformationRequestError {
    InformationRequestError {
        protocol: InformationErrorProtocol::V1,
        interface_id: None,
        code,
        message: message.to_owned(),
        current_catalog_revision: None,
        current_schema_revision: None,
        rejected_fields: None,
    }
}
