//! Read-only Information providers backed by injected source ports.

mod current_status;
mod inventory;
mod schema;

use std::sync::Mutex;

pub use current_status::CurrentStatusProvider;
pub use inventory::InventoryProvider;

struct RevisionTracker<Snapshot> {
    state: Mutex<RevisionState<Snapshot>>,
}

struct RevisionState<Snapshot> {
    revision: u64,
    last_snapshot: Option<Snapshot>,
}

impl<Snapshot> Default for RevisionTracker<Snapshot> {
    fn default() -> Self {
        Self {
            state: Mutex::new(RevisionState {
                revision: 0,
                last_snapshot: None,
            }),
        }
    }
}

impl<Snapshot> RevisionTracker<Snapshot>
where
    Snapshot: Clone + PartialEq,
{
    fn revision_for(&self, snapshot: &Snapshot) -> u64 {
        // Source-port callbacks happen before this method. Only owned snapshot comparison and
        // cloning occur under the lock. `f64` equality deliberately treats -0 and 0 as equal,
        // matching JSON.stringify's revision signature for that edge case.
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if state.last_snapshot.as_ref() != Some(snapshot) {
            state.last_snapshot = Some(snapshot.clone());
            state.revision = state.revision.saturating_add(1);
        }
        state.revision
    }
}
