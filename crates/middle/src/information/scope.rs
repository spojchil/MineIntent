use std::sync::RwLock;

use super::contracts::{InformationScopeDependency, InformationScopeSnapshot};

pub trait InformationScopeSource: Send + Sync {
    fn capture(&self) -> InformationScopeSnapshot;
}

pub struct MutableInformationScopeSource {
    snapshot: RwLock<InformationScopeSnapshot>,
}

impl MutableInformationScopeSource {
    pub fn new(initial: InformationScopeSnapshot) -> Self {
        Self {
            snapshot: RwLock::new(initial),
        }
    }

    pub fn update(&self, next: InformationScopeSnapshot) {
        match self.snapshot.write() {
            Ok(mut snapshot) => *snapshot = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
    }
}

impl InformationScopeSource for MutableInformationScopeSource {
    fn capture(&self) -> InformationScopeSnapshot {
        match self.snapshot.read() {
            Ok(snapshot) => snapshot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

pub fn scope_changed(
    before: &InformationScopeSnapshot,
    after: &InformationScopeSnapshot,
    dependencies: &[InformationScopeDependency],
) -> bool {
    if before.process_session_id != after.process_session_id {
        return true;
    }
    for dependency in dependencies {
        match dependency {
            InformationScopeDependency::Connection => {
                if before.connection_epoch != after.connection_epoch
                    || before.connection_state != after.connection_state
                {
                    return true;
                }
            }
            InformationScopeDependency::World => {
                if before.world_id != after.world_id {
                    return true;
                }
            }
            InformationScopeDependency::Dimension => {
                if before.dimension != after.dimension {
                    return true;
                }
            }
            InformationScopeDependency::Ui => {
                if before.ui_revision != after.ui_revision {
                    return true;
                }
            }
            InformationScopeDependency::Screen => {
                if before.screen_instance_id != after.screen_instance_id
                    || before.screen_revision != after.screen_revision
                {
                    return true;
                }
            }
        }
    }
    false
}
