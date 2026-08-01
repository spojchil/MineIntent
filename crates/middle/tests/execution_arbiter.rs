//! One-to-one behavior migration of the eight tests in `execution-arbiter.test.ts`.
//! The final serde test is an explicitly additional Rust contract test.

use mineintent_middle::execution::{
    AcquireDecision, ExecutionArbiter, ExecutionRefusal, ExecutionRefusalCode, ExecutionRequest,
    ExecutionResource, JobOutcome, JobState, ResourceLease, ResourceLeaseHandle, SettledJobState,
};
use serde_json::json;
use uuid::Uuid;

#[test]
fn leases_are_per_resource_so_chat_memory_and_viewport_stay_free_while_body_is_held() {
    let arbiter = ExecutionArbiter::default();
    let body = granted(arbiter.acquire(request(ExecutionResource::Body, "r", "move_input")));

    for resource in [
        ExecutionResource::Chat,
        ExecutionResource::Memory,
        ExecutionResource::Viewport,
    ] {
        granted(arbiter.acquire(request(resource, "r", "say")));
    }

    let refused = arbiter.acquire(request(ExecutionResource::Body, "r", "look_relative"));
    let AcquireDecision::Refused(refusal) = refused else {
        panic!("a second body lease must be refused");
    };
    assert_eq!(refusal.code, ExecutionRefusalCode::ResourceBusy);
    assert!(refusal.summary.contains("body is held by move_input"));
    assert_eq!(
        arbiter.lease_for(ExecutionResource::Body),
        Some(body.lease().clone())
    );
}

#[test]
fn a_refusal_is_returned_rather_than_panicking_so_one_conflict_cannot_kill_a_run() {
    let arbiter = ExecutionArbiter::default();
    granted(arbiter.acquire(request(ExecutionResource::Body, "r", "move_input")));

    assert!(matches!(
        arbiter.acquire(request(ExecutionResource::Body, "r", "move_input")),
        AcquireDecision::Refused(ExecutionRefusal {
            code: ExecutionRefusalCode::ResourceBusy,
            ..
        })
    ));
}

#[test]
fn releasing_is_idempotent_and_frees_the_resource_exactly_once() {
    let arbiter = ExecutionArbiter::default();
    let first = granted(arbiter.acquire(request(ExecutionResource::Body, "r", "move_input")));
    first.release();
    first.release();

    let second = granted(arbiter.acquire(request(ExecutionResource::Body, "r", "look_relative")));
    first.release();
    assert_eq!(
        arbiter
            .lease_for(ExecutionResource::Body)
            .map(|lease| lease.tool_name),
        Some("look_relative".to_owned())
    );
    assert_eq!(second.lease().tool_name, "look_relative");
}

#[test]
fn a_stale_release_after_invalidation_cannot_evict_the_replacement_lease() {
    let arbiter = ExecutionArbiter::default();
    let stale = granted(arbiter.acquire(request(ExecutionResource::Body, "old", "move_input")));

    arbiter.invalidate("world_scope_changed");
    let live = granted(arbiter.acquire(request(ExecutionResource::Body, "new", "look_relative")));
    stale.release();

    assert_eq!(
        arbiter.lease_for(ExecutionResource::Body),
        Some(live.lease().clone())
    );
    assert_eq!(live.lease().run_id, "new");
}

#[test]
fn a_job_returns_a_shared_handle_immediately_and_reports_its_outcome_later() {
    let arbiter = ExecutionArbiter::default();
    let job = arbiter.start_job(request(ExecutionResource::Body, "r", "move_input"));
    assert_eq!(job.state(), JobState::Running);
    assert_eq!(job.resource(), ExecutionResource::Body);
    assert_eq!(job.run_id(), "r");
    assert_eq!(job.tool_name(), "move_input");
    assert!(!job.started_at().is_empty());
    assert_eq!(
        arbiter.jobs_for("r"),
        vec![JobOutcome {
            job_id: job.job_id(),
            state: JobState::Running,
            summary: None,
        }]
    );

    let settled = arbiter
        .settle_job(
            job.job_id(),
            SettledJobState::Completed,
            Some("walked 3 blocks".to_owned()),
        )
        .unwrap();
    assert_eq!(settled.state, JobState::Completed);
    assert_eq!(settled.summary.as_deref(), Some("walked 3 blocks"));
    assert_eq!(job.state(), JobState::Completed);

    let late_cancellation = job.cancellation();
    assert_eq!(
        arbiter.cancel_job(job.job_id()).unwrap().state,
        JobState::Completed
    );
    assert_eq!(job.state(), JobState::Completed);
    assert!(late_cancellation.is_cancelled());
    assert_eq!(
        arbiter.settle_job(Uuid::new_v4(), SettledJobState::Completed, None),
        None
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_a_running_job_updates_shared_state_and_wakes_its_signal() {
    let arbiter = ExecutionArbiter::default();
    let job = arbiter.start_job(request(ExecutionResource::Body, "r", "move_input"));
    let mut cancellation = job.cancellation();
    assert!(!cancellation.is_cancelled());

    let waiter = tokio::spawn(async move { cancellation.cancelled().await });
    tokio::task::yield_now().await;
    assert!(
        !waiter.is_finished(),
        "active cancellation must remain pending"
    );

    let outcome = arbiter.cancel_job(job.job_id()).unwrap();
    assert_eq!(outcome.state, JobState::Cancelled);
    assert_eq!(job.state(), JobState::Cancelled);
    assert_eq!(waiter.await.unwrap().as_deref(), Some("job_cancelled"));
}

#[tokio::test(flavor = "current_thread")]
async fn scope_loss_voids_every_lease_and_running_job_in_one_step() {
    let arbiter = ExecutionArbiter::default();
    granted(arbiter.acquire(request(ExecutionResource::Body, "r", "move_input")));
    granted(arbiter.acquire(request(ExecutionResource::Chat, "r", "say")));
    let body_job = arbiter.start_job(request(ExecutionResource::Body, "r", "move_input"));
    let chat_job = arbiter.start_job(request(ExecutionResource::Chat, "r", "say"));
    let completed_job = arbiter.start_job(request(ExecutionResource::Memory, "r", "remember"));
    arbiter.settle_job(completed_job.job_id(), SettledJobState::Completed, None);
    let completed_signal = completed_job.cancellation();
    let body_signal = body_job.cancellation();
    let before = arbiter.epoch();

    arbiter.invalidate("world_scope_changed");

    assert_eq!(arbiter.epoch(), before + 1);
    assert_eq!(arbiter.lease_for(ExecutionResource::Body), None);
    assert_eq!(arbiter.lease_for(ExecutionResource::Chat), None);
    assert_eq!(body_job.state(), JobState::Cancelled);
    assert_eq!(chat_job.state(), JobState::Cancelled);
    assert_eq!(completed_job.state(), JobState::Completed);
    assert!(!completed_signal.is_cancelled());
    assert!(body_signal.is_cancelled());
    assert_eq!(body_signal.reason().as_deref(), Some("world_scope_changed"));
    assert_eq!(
        arbiter.cancel_job(body_job.job_id()).unwrap().state,
        JobState::Cancelled
    );
    assert_eq!(body_signal.reason().as_deref(), Some("world_scope_changed"));
}

#[test]
fn settled_jobs_are_pruned_while_running_jobs_survive_in_insertion_order() {
    let arbiter = ExecutionArbiter::default();
    let done = arbiter.start_job(request(ExecutionResource::Body, "r", "move_input"));
    let first_running = arbiter.start_job(request(ExecutionResource::Chat, "r", "say"));
    let other_run = arbiter.start_job(request(ExecutionResource::Memory, "other", "remember"));
    let second_running = arbiter.start_job(request(ExecutionResource::Viewport, "r", "view"));
    arbiter.settle_job(done.job_id(), SettledJobState::Completed, None);

    arbiter.prune_settled_jobs();

    let ids: Vec<_> = arbiter
        .jobs_for("r")
        .into_iter()
        .map(|outcome| outcome.job_id)
        .collect();
    assert_eq!(ids, vec![first_running.job_id(), second_running.job_id()]);
    assert_eq!(arbiter.jobs_for("other")[0].job_id, other_run.job_id());
    assert_eq!(
        arbiter.settle_job(done.job_id(), SettledJobState::Failed, None),
        None
    );
}

#[test]
fn additional_execution_contracts_are_strict_and_keep_outcomes_narrow() {
    for resource in ["body", "chat", "memory", "viewport"] {
        assert!(serde_json::from_value::<ExecutionResource>(json!(resource)).is_ok());
    }
    assert!(serde_json::from_value::<ExecutionResource>(json!("tool")).is_err());
    for code in ["resource_busy", "unknown_tool", "scope_invalid"] {
        assert!(serde_json::from_value::<ExecutionRefusalCode>(json!(code)).is_ok());
    }

    let lease = json!({
        "resource": "body",
        "actionId": "00000000-0000-4000-8000-000000000001",
        "runId": "run-1",
        "toolName": "move_input",
        "acquiredAt": "2026-08-01T00:00:00.000Z"
    });
    assert!(serde_json::from_value::<ResourceLease>(lease.clone()).is_ok());
    let mut lease_with_unknown = lease;
    lease_with_unknown["epoch"] = json!(1);
    assert!(serde_json::from_value::<ResourceLease>(lease_with_unknown).is_err());

    let request_with_transport = json!({
        "resource": "body",
        "runId": "run-1",
        "toolName": "move_input",
        "callbackUrl": "http://127.0.0.1"
    });
    assert!(serde_json::from_value::<ExecutionRequest>(request_with_transport).is_err());

    let outcome = JobOutcome {
        job_id: Uuid::parse_str("00000000-0000-4000-8000-000000000002").unwrap(),
        state: JobState::Failed,
        summary: None,
    };
    assert_eq!(
        serde_json::to_value(outcome).unwrap(),
        json!({
            "jobId": "00000000-0000-4000-8000-000000000002",
            "state": "failed"
        })
    );
    let omitted_summary: JobOutcome = serde_json::from_value(json!({
        "jobId": "00000000-0000-4000-8000-000000000002",
        "state": "failed"
    }))
    .unwrap();
    assert_eq!(omitted_summary.summary, None);
    assert!(serde_json::from_value::<JobOutcome>(json!({
        "jobId": "00000000-0000-4000-8000-000000000002",
        "state": "failed",
        "summary": null
    }))
    .is_err());
    assert!(serde_json::from_value::<JobOutcome>(json!({
        "jobId": "00000000-0000-4000-8000-000000000002",
        "state": "running",
        "resource": "body"
    }))
    .is_err());
}

fn request(resource: ExecutionResource, run_id: &str, tool_name: &str) -> ExecutionRequest {
    ExecutionRequest {
        resource,
        run_id: run_id.to_owned(),
        tool_name: tool_name.to_owned(),
    }
}

fn granted(decision: AcquireDecision) -> ResourceLeaseHandle {
    match decision {
        AcquireDecision::Granted(handle) => handle,
        AcquireDecision::Refused(refusal) => {
            panic!("expected lease, got refusal: {}", refusal.summary)
        }
    }
}
