use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::sync::watch;
use uuid::Uuid;

use super::{
    AcquireDecision, ExecutionRefusal, ExecutionRefusalCode, ExecutionRequest, ExecutionResource,
    JobOutcome, JobState, ResourceLease, SettledJobState,
};

#[derive(Clone, Default)]
pub struct ExecutionArbiter {
    inner: Arc<ArbiterInner>,
}

#[derive(Default)]
struct ArbiterInner {
    state: Mutex<ArbiterState>,
}

#[derive(Default)]
struct ArbiterState {
    epoch: u64,
    next_lease_generation: u64,
    leases: HashMap<ExecutionResource, HeldLease>,
    jobs: Vec<Arc<JobShared>>,
}

struct HeldLease {
    lease: ResourceLease,
    generation: u64,
}

/// Owns the authority to release one exact lease generation.
///
/// Its private generation cannot be constructed by callers. A cloned or delayed handle therefore
/// cannot remove a replacement lease, even after scope invalidation.
#[derive(Clone)]
pub struct ResourceLeaseHandle {
    inner: Arc<ArbiterInner>,
    lease: ResourceLease,
    generation: u64,
}

impl ResourceLeaseHandle {
    pub fn lease(&self) -> &ResourceLease {
        &self.lease
    }

    /// Idempotently releases this generation if it is still current.
    pub fn release(&self) {
        let mut state = lock_recover(&self.inner.state);
        let is_current = state
            .leases
            .get(&self.lease.resource)
            .is_some_and(|held| held.generation == self.generation);
        if is_current {
            state.leases.remove(&self.lease.resource);
        }
    }
}

struct JobShared {
    job_id: Uuid,
    resource: ExecutionResource,
    run_id: String,
    tool_name: String,
    started_at: String,
    state: Mutex<JobState>,
    cancellation: watch::Sender<Option<String>>,
}

impl JobShared {
    fn state(&self) -> JobState {
        *lock_recover(&self.state)
    }

    fn settle(
        &self,
        desired: SettledJobState,
        cancellation_reason: &str,
        notify_already_settled: bool,
    ) -> JobState {
        let mut state = lock_recover(&self.state);
        let was_running = *state == JobState::Running;
        if was_running {
            *state = desired.into();
        }
        if desired == SettledJobState::Cancelled
            && (was_running || notify_already_settled)
            && self.cancellation.borrow().is_none()
        {
            self.cancellation
                .send_replace(Some(cancellation_reason.to_owned()));
        }
        *state
    }
}

#[derive(Clone)]
pub struct JobHandle {
    shared: Arc<JobShared>,
}

impl JobHandle {
    pub fn job_id(&self) -> Uuid {
        self.shared.job_id
    }

    pub fn resource(&self) -> ExecutionResource {
        self.shared.resource
    }

    pub fn run_id(&self) -> &str {
        &self.shared.run_id
    }

    pub fn tool_name(&self) -> &str {
        &self.shared.tool_name
    }

    pub fn started_at(&self) -> &str {
        &self.shared.started_at
    }

    pub fn state(&self) -> JobState {
        self.shared.state()
    }

    pub fn cancellation(&self) -> JobCancellation {
        JobCancellation {
            receiver: self.shared.cancellation.subscribe(),
            _shared: Arc::clone(&self.shared),
        }
    }
}

pub struct JobCancellation {
    receiver: watch::Receiver<Option<String>>,
    // Keeping the shared record alive also keeps the watch sender alive while this signal waits.
    _shared: Arc<JobShared>,
}

impl JobCancellation {
    pub fn is_cancelled(&self) -> bool {
        self.receiver.borrow().is_some()
    }

    pub fn reason(&self) -> Option<String> {
        self.receiver.borrow().clone()
    }

    /// Waits without holding arbiter or job locks.
    pub async fn cancelled(&mut self) -> Option<String> {
        loop {
            if let Some(reason) = self.reason() {
                return Some(reason);
            }
            if self.receiver.changed().await.is_err() {
                return None;
            }
        }
    }
}

impl ExecutionArbiter {
    pub fn epoch(&self) -> u64 {
        lock_recover(&self.inner.state).epoch
    }

    pub fn lease_for(&self, resource: ExecutionResource) -> Option<ResourceLease> {
        lock_recover(&self.inner.state)
            .leases
            .get(&resource)
            .map(|held| held.lease.clone())
    }

    /// A resource conflict is an ordinary refusal value, never a panic or error path.
    pub fn acquire(&self, input: ExecutionRequest) -> AcquireDecision {
        let mut state = lock_recover(&self.inner.state);
        if let Some(held) = state.leases.get(&input.resource) {
            return AcquireDecision::Refused(ExecutionRefusal {
                code: ExecutionRefusalCode::ResourceBusy,
                summary: format!(
                    "resource_busy:{} is held by {}",
                    input.resource.as_str(),
                    held.lease.tool_name
                ),
            });
        }

        state.next_lease_generation = state.next_lease_generation.wrapping_add(1);
        let generation = state.next_lease_generation;
        let lease = ResourceLease {
            resource: input.resource,
            action_id: Uuid::new_v4(),
            run_id: input.run_id,
            tool_name: input.tool_name,
            acquired_at: current_timestamp(),
        };
        state.leases.insert(
            lease.resource,
            HeldLease {
                lease: lease.clone(),
                generation,
            },
        );
        AcquireDecision::Granted(ResourceLeaseHandle {
            inner: Arc::clone(&self.inner),
            lease,
            generation,
        })
    }

    /// Registers background work and returns a shared handle immediately.
    pub fn start_job(&self, input: ExecutionRequest) -> JobHandle {
        let (cancellation, _) = watch::channel(None);
        let shared = Arc::new(JobShared {
            job_id: Uuid::new_v4(),
            resource: input.resource,
            run_id: input.run_id,
            tool_name: input.tool_name,
            started_at: current_timestamp(),
            state: Mutex::new(JobState::Running),
            cancellation,
        });
        lock_recover(&self.inner.state)
            .jobs
            .push(Arc::clone(&shared));
        JobHandle { shared }
    }

    pub fn settle_job(
        &self,
        job_id: Uuid,
        state: SettledJobState,
        summary: Option<String>,
    ) -> Option<JobOutcome> {
        let job = lock_recover(&self.inner.state)
            .jobs
            .iter()
            .find(|job| job.job_id == job_id)
            .cloned()?;
        let state = job.settle(state, "job_cancelled", true);
        Some(JobOutcome {
            job_id,
            state,
            summary,
        })
    }

    pub fn cancel_job(&self, job_id: Uuid) -> Option<JobOutcome> {
        self.settle_job(job_id, SettledJobState::Cancelled, None)
    }

    pub fn jobs_for(&self, run_id: &str) -> Vec<JobOutcome> {
        lock_recover(&self.inner.state)
            .jobs
            .iter()
            .filter(|job| job.run_id == run_id)
            .map(|job| JobOutcome {
                job_id: job.job_id,
                state: job.state(),
                summary: None,
            })
            .collect()
    }

    /// Drops settled jobs while preserving the insertion order of running jobs.
    pub fn prune_settled_jobs(&self) {
        lock_recover(&self.inner.state)
            .jobs
            .retain(|job| job.state() == JobState::Running);
    }

    /// Advances the scope epoch, clears all leases and cancels every running job atomically with
    /// respect to other arbiter operations.
    pub fn invalidate(&self, reason: impl Into<String>) {
        let reason = reason.into();
        let mut state = lock_recover(&self.inner.state);
        state.epoch = state.epoch.wrapping_add(1);
        state.leases.clear();
        for job in &state.jobs {
            job.settle(SettledJobState::Cancelled, &reason, false);
        }
    }
}

/// Critical sections contain only local state transitions: no await, user callback or fallible
/// serialization/I/O. If another thread panics while holding a lock, recover the inner state and
/// keep the poison marker from turning that unrelated panic into process-wide propagation.
fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn current_timestamp() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = elapsed.as_secs();
    let seconds_in_day = seconds % 86_400;
    let (year, month, day) = civil_date((seconds / 86_400) as i64);
    let hour = seconds_in_day / 3_600;
    let minute = seconds_in_day % 3_600 / 60;
    let second = seconds_in_day % 60;
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:03}Z",
        elapsed.subsec_millis()
    )
}

fn civil_date(days_since_epoch: i64) -> (i64, i64, i64) {
    let shifted = days_since_epoch + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}
