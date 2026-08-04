use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use super::{
    contracts::{
        InformationGrant, InformationInterfaceId, InformationInvalidationEvent,
        InformationReferenceIssueError, InformationReferenceIssueRequest,
        InformationReferenceIssuer, InformationScopeSnapshot, InformationSelectorRef,
        InformationSelectorRefProtocol,
    },
    support::{clone_bounded_json, format_utc_millis, is_expired, parse_javascript_date_millis},
    InformationClock, SystemInformationClock,
};

pub const DEFAULT_MAX_REFERENCE_ENTRIES: usize = 2_048;
pub const DEFAULT_MAX_REFERENCE_ENTRIES_PER_PRINCIPAL: usize = 512;
pub const DEFAULT_MAX_REFERENCE_ENTRIES_PER_INTERFACE: usize = 256;
pub const DEFAULT_MAX_REFERENCE_PAYLOAD_BYTES: usize = 8_192;
pub const DEFAULT_MAX_REFERENCE_ISSUES_PER_ISSUER: usize = 32;
pub const DEFAULT_REFERENCE_TTL_MS: u64 = 60_000;

/// Compatibility aliases for callers compiled against the original ref-store-specific names.
pub use super::{
    InformationClock as InformationRefClock, SystemInformationClock as SystemInformationRefClock,
};

#[derive(Clone)]
pub struct InformationRefStoreOptions {
    pub max_entries: usize,
    pub max_entries_per_principal: usize,
    pub max_entries_per_interface: usize,
    pub max_payload_bytes: usize,
    pub max_issues_per_issuer: usize,
    pub ttl_ms: u64,
    pub clock: Arc<dyn InformationClock>,
}

impl Default for InformationRefStoreOptions {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_MAX_REFERENCE_ENTRIES,
            max_entries_per_principal: DEFAULT_MAX_REFERENCE_ENTRIES_PER_PRINCIPAL,
            max_entries_per_interface: DEFAULT_MAX_REFERENCE_ENTRIES_PER_INTERFACE,
            max_payload_bytes: DEFAULT_MAX_REFERENCE_PAYLOAD_BYTES,
            max_issues_per_issuer: DEFAULT_MAX_REFERENCE_ISSUES_PER_ISSUER,
            ttl_ms: DEFAULT_REFERENCE_TTL_MS,
            clock: Arc::new(SystemInformationClock),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum InformationRefStoreError {
    #[error("information reference limits must be positive integers")]
    InvalidLimits,
    #[error("information reference store lock was poisoned during {operation}")]
    LockPoisoned { operation: &'static str },
}

#[derive(Clone)]
pub struct InformationRefIssuerInput {
    pub interface_id: InformationInterfaceId,
    pub principal_id: String,
    pub grant: InformationGrant,
    pub scope: InformationScopeSnapshot,
}

#[derive(Clone)]
pub struct InformationRefResolveInput {
    pub reference: InformationSelectorRef,
    pub target_interface: InformationInterfaceId,
    pub principal_id: String,
    pub grant: InformationGrant,
    pub scope: InformationScopeSnapshot,
    pub accepted_kinds: Option<Vec<String>>,
}

#[derive(Clone)]
pub struct InformationRefStore {
    inner: Arc<InformationRefStoreInner>,
}

struct InformationRefStoreInner {
    options: InformationRefStoreOptions,
    entries: Mutex<HashMap<String, StoredReference>>,
}

#[derive(Clone)]
struct StoredReference {
    reference: InformationSelectorRef,
    kind: String,
    payload: Value,
    principal_id: String,
    grant_id: String,
    audience: super::contracts::InformationAudience,
    allowed_interfaces: Vec<InformationInterfaceId>,
    dimension: Option<String>,
    screen_revision: Option<u64>,
}

pub struct InformationRefIssuer {
    store: InformationRefStore,
    input: InformationRefIssuerInput,
    issued: AtomicUsize,
}

impl InformationRefStore {
    pub fn new(options: InformationRefStoreOptions) -> Result<Self, InformationRefStoreError> {
        validate_options(&options)?;
        Ok(Self {
            inner: Arc::new(InformationRefStoreInner {
                options,
                entries: Mutex::new(HashMap::new()),
            }),
        })
    }

    pub fn issuer(&self, input: InformationRefIssuerInput) -> InformationRefIssuer {
        InformationRefIssuer {
            store: self.clone(),
            input,
            issued: AtomicUsize::new(0),
        }
    }

    pub fn resolve(
        &self,
        input: InformationRefResolveInput,
    ) -> Result<Option<Value>, InformationRefStoreError> {
        let now = self.inner.options.clock.now_millis();
        let mut entries =
            self.inner
                .entries
                .lock()
                .map_err(|_| InformationRefStoreError::LockPoisoned {
                    operation: "resolve",
                })?;
        let Some(stored) = entries.get(&input.reference.id) else {
            return Ok(None);
        };
        if stored.reference != input.reference {
            return Ok(None);
        }
        if is_expired(stored.reference.valid_until.as_deref(), now) {
            entries.remove(&input.reference.id);
            return Ok(None);
        }
        if stored.principal_id != input.principal_id
            || stored.grant_id != input.grant.id
            || stored.audience != input.grant.audience
            || !stored.allowed_interfaces.contains(&input.target_interface)
            || stored.reference.connection_epoch != input.scope.connection_epoch
            || stored.reference.world_id != input.scope.world_id
            || stored.dimension != input.scope.dimension
        {
            return Ok(None);
        }
        if stored.reference.screen_instance_id.is_some()
            && (stored.reference.screen_instance_id != input.scope.screen_instance_id
                || stored.screen_revision != input.scope.screen_revision)
        {
            return Ok(None);
        }
        if input
            .accepted_kinds
            .as_ref()
            .is_some_and(|kinds| !kinds.contains(&stored.kind))
        {
            return Ok(None);
        }
        Ok(Some(stored.payload.clone()))
    }

    pub fn invalidate(
        &self,
        event: &InformationInvalidationEvent,
    ) -> Result<(), InformationRefStoreError> {
        let mut entries =
            self.inner
                .entries
                .lock()
                .map_err(|_| InformationRefStoreError::LockPoisoned {
                    operation: "invalidate",
                })?;
        entries.retain(|_, stored| {
            let remove = match event {
                InformationInvalidationEvent::GrantEnded { grant_id } => {
                    stored.grant_id == *grant_id
                }
                InformationInvalidationEvent::ConnectionChanged { connection_epoch } => {
                    stored.reference.connection_epoch != *connection_epoch
                }
                InformationInvalidationEvent::WorldChanged {
                    world_id,
                    dimension,
                } => stored.reference.world_id != *world_id || stored.dimension != *dimension,
                InformationInvalidationEvent::ScreenChanged {
                    screen_instance_id,
                    screen_revision,
                } => {
                    stored.reference.screen_instance_id.is_some()
                        && (stored.reference.screen_instance_id != *screen_instance_id
                            || stored.screen_revision != *screen_revision)
                }
            };
            !remove
        });
        Ok(())
    }

    pub fn clear(&self) -> Result<(), InformationRefStoreError> {
        let mut entries = self
            .inner
            .entries
            .lock()
            .map_err(|_| InformationRefStoreError::LockPoisoned { operation: "clear" })?;
        entries.clear();
        Ok(())
    }

    pub fn size(&self) -> Result<usize, InformationRefStoreError> {
        let entries =
            self.inner
                .entries
                .lock()
                .map_err(|_| InformationRefStoreError::LockPoisoned {
                    operation: "read size",
                })?;
        Ok(entries.len())
    }

    fn issue(
        &self,
        input: &InformationRefIssuerInput,
        request: InformationReferenceIssueRequest,
    ) -> Result<InformationSelectorRef, InformationReferenceIssueError> {
        if request.allowed_interfaces.is_empty() {
            return Err(InformationReferenceIssueError::AllowedTargetRequired);
        }
        if request.kind.trim().is_empty()
            || request
                .valid_until
                .as_deref()
                .is_some_and(|timestamp| parse_javascript_date_millis(timestamp).is_none())
        {
            return Err(InformationReferenceIssueError::InvalidMetadata);
        }
        if request.bind_to_screen == Some(true)
            && (input
                .scope
                .screen_instance_id
                .as_deref()
                .is_none_or(str::is_empty)
                || input.scope.screen_revision.is_none())
        {
            return Err(InformationReferenceIssueError::ActiveScreenRevisionRequired);
        }

        let now = self.inner.options.clock.now_millis();
        let mut entries = self
            .inner
            .entries
            .lock()
            .map_err(|_| InformationReferenceIssueError::StoreUnavailable)?;
        entries.retain(|_, stored| !is_expired(stored.reference.valid_until.as_deref(), now));
        if entries.len() >= self.inner.options.max_entries
            || count_entries(&entries, |stored| stored.principal_id == input.principal_id)
                >= self.inner.options.max_entries_per_principal
            || count_entries(&entries, |stored| {
                stored.reference.interface_id == input.interface_id
            }) >= self.inner.options.max_entries_per_interface
        {
            return Err(InformationReferenceIssueError::CapacityExceeded);
        }

        let payload = clone_bounded_json(
            &request.payload,
            self.inner.options.max_payload_bytes,
            || InformationReferenceIssueError::PayloadNotJsonSerializable,
            |actual, maximum| InformationReferenceIssueError::PayloadByteLimitExceeded {
                actual,
                maximum,
            },
        )?;
        let maximum_valid_until = now
            .checked_add(
                i64::try_from(self.inner.options.ttl_ms)
                    .map_err(|_| InformationReferenceIssueError::TimestampOutOfRange)?,
            )
            .ok_or(InformationReferenceIssueError::TimestampOutOfRange)?;
        if request
            .valid_until
            .as_deref()
            .and_then(parse_javascript_date_millis)
            .is_some_and(|valid_until| valid_until > maximum_valid_until)
        {
            return Err(InformationReferenceIssueError::LifetimeExceeded);
        }
        let valid_until = match request.valid_until {
            Some(valid_until) => valid_until,
            None => format_utc_millis(maximum_valid_until, || {
                InformationReferenceIssueError::TimestampOutOfRange
            })?,
        };
        let id = next_unique_reference_id(&entries);
        let screen_bound = request.bind_to_screen == Some(true);
        let reference = InformationSelectorRef {
            protocol: InformationSelectorRefProtocol::V1,
            id: id.clone(),
            interface_id: input.interface_id,
            connection_epoch: input.scope.connection_epoch,
            world_id: input
                .scope
                .world_id
                .clone()
                .filter(|world| !world.is_empty()),
            screen_instance_id: if screen_bound {
                input.scope.screen_instance_id.clone()
            } else {
                None
            },
            based_on_information_revision: request.based_on_information_revision,
            valid_until: Some(valid_until),
        };
        entries.insert(
            id,
            StoredReference {
                reference: reference.clone(),
                kind: request.kind,
                payload,
                principal_id: input.principal_id.clone(),
                grant_id: input.grant.id.clone(),
                audience: input.grant.audience,
                allowed_interfaces: request.allowed_interfaces,
                dimension: input
                    .scope
                    .dimension
                    .clone()
                    .filter(|dimension| !dimension.is_empty()),
                screen_revision: if screen_bound {
                    input.scope.screen_revision
                } else {
                    None
                },
            },
        );
        Ok(reference)
    }
}

impl Default for InformationRefStore {
    fn default() -> Self {
        Self {
            inner: Arc::new(InformationRefStoreInner {
                options: InformationRefStoreOptions::default(),
                entries: Mutex::new(HashMap::new()),
            }),
        }
    }
}

impl InformationRefIssuer {
    pub fn issue(
        &self,
        request: InformationReferenceIssueRequest,
    ) -> Result<InformationSelectorRef, InformationReferenceIssueError> {
        let previous = match self
            .issued
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |issued| {
                Some(issued.saturating_add(1))
            }) {
            Ok(previous) | Err(previous) => previous,
        };
        if previous >= self.store.inner.options.max_issues_per_issuer {
            return Err(InformationReferenceIssueError::PerIssuerLimitExceeded);
        }
        self.store.issue(&self.input, request)
    }
}

impl InformationReferenceIssuer for InformationRefIssuer {
    fn issue(
        &self,
        request: InformationReferenceIssueRequest,
    ) -> Result<InformationSelectorRef, InformationReferenceIssueError> {
        InformationRefIssuer::issue(self, request)
    }
}

fn validate_options(options: &InformationRefStoreOptions) -> Result<(), InformationRefStoreError> {
    if options.max_entries < 1
        || options.max_entries_per_principal < 1
        || options.max_entries_per_interface < 1
        || options.max_payload_bytes < 1
        || options.max_issues_per_issuer < 1
        || options.ttl_ms < 1
    {
        return Err(InformationRefStoreError::InvalidLimits);
    }
    Ok(())
}

fn count_entries(
    entries: &HashMap<String, StoredReference>,
    predicate: impl Fn(&StoredReference) -> bool,
) -> usize {
    entries.values().filter(|stored| predicate(stored)).count()
}

fn next_unique_reference_id(entries: &HashMap<String, StoredReference>) -> String {
    loop {
        let id = format!("iref_{}", Uuid::new_v4());
        if !entries.contains_key(&id) {
            return id;
        }
    }
}
