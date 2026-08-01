use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use super::{
    contracts::{
        InformationAudience, InformationGrant, InformationInterfaceId,
        InformationInvalidationEvent, InformationScopeSnapshot, InformationSelectorRef,
    },
    ref_store::{
        clone_bounded_json, format_utc_millis, is_expired, InformationRefClock,
        SystemInformationRefClock,
    },
};

pub const DEFAULT_MAX_CURSOR_ENTRIES: usize = 2_048;
pub const DEFAULT_MAX_CURSOR_ENTRIES_PER_PRINCIPAL: usize = 512;
pub const DEFAULT_MAX_CURSOR_ENTRIES_PER_INTERFACE: usize = 256;
pub const DEFAULT_MAX_CURSOR_PAGE_STATE_BYTES: usize = 8_192;
pub const DEFAULT_CURSOR_TTL_MS: u64 = 60_000;

#[derive(Clone)]
pub struct InformationCursorStoreOptions {
    pub max_entries: usize,
    pub max_entries_per_principal: usize,
    pub max_entries_per_interface: usize,
    pub max_page_state_bytes: usize,
    pub ttl_ms: u64,
    pub clock: Arc<dyn InformationRefClock>,
}

impl Default for InformationCursorStoreOptions {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_MAX_CURSOR_ENTRIES,
            max_entries_per_principal: DEFAULT_MAX_CURSOR_ENTRIES_PER_PRINCIPAL,
            max_entries_per_interface: DEFAULT_MAX_CURSOR_ENTRIES_PER_INTERFACE,
            max_page_state_bytes: DEFAULT_MAX_CURSOR_PAGE_STATE_BYTES,
            ttl_ms: DEFAULT_CURSOR_TTL_MS,
            clock: Arc::new(SystemInformationRefClock),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum InformationCursorStoreError {
    #[error("information cursor limits must be positive integers")]
    InvalidLimits,
    #[error("information cursor metadata is invalid")]
    InvalidMetadata,
    #[error("information cursor capacity exceeded")]
    CapacityExceeded,
    #[error("information cursor page state must be JSON serializable")]
    PageStateNotJsonSerializable,
    #[error("information cursor page state exceeds its byte limit ({actual} > {maximum})")]
    PageStateByteLimitExceeded { actual: usize, maximum: usize },
    #[error("information cursor timestamp is outside the supported UTC range")]
    TimestampOutOfRange,
    #[error("information cursor store lock was poisoned during {operation}")]
    LockPoisoned { operation: &'static str },
}

#[derive(Clone, Debug, PartialEq)]
pub struct InformationCursorIssueInput {
    pub interface_id: InformationInterfaceId,
    pub fields: Vec<String>,
    pub selector: Option<InformationSelectorRef>,
    pub information_revision: u64,
    pub limit: u64,
    pub page_state: Value,
    pub principal_id: String,
    pub grant: InformationGrant,
    pub scope: InformationScopeSnapshot,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InformationCursorResolveInput {
    pub cursor: String,
    pub interface_id: InformationInterfaceId,
    pub fields: Vec<String>,
    pub selector: Option<InformationSelectorRef>,
    pub limit: u64,
    pub principal_id: String,
    pub grant: InformationGrant,
    pub scope: InformationScopeSnapshot,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InformationCursorResolution {
    pub state: Value,
    pub information_revision: u64,
}

#[derive(Clone)]
pub struct InformationCursorStore {
    inner: Arc<InformationCursorStoreInner>,
}

struct InformationCursorStoreInner {
    options: InformationCursorStoreOptions,
    entries: Mutex<HashMap<String, StoredCursor>>,
}

struct StoredCursor {
    interface_id: InformationInterfaceId,
    fields: Vec<String>,
    selector_id: Option<String>,
    information_revision: u64,
    limit: u64,
    page_state: Value,
    principal_id: String,
    grant_id: String,
    audience: InformationAudience,
    connection_epoch: u64,
    world_id: Option<String>,
    dimension: Option<String>,
    screen_instance_id: Option<String>,
    screen_revision: Option<u64>,
    valid_until: String,
}

impl InformationCursorStore {
    pub fn new(
        options: InformationCursorStoreOptions,
    ) -> Result<Self, InformationCursorStoreError> {
        validate_options(&options)?;
        Ok(Self {
            inner: Arc::new(InformationCursorStoreInner {
                options,
                entries: Mutex::new(HashMap::new()),
            }),
        })
    }

    pub fn issue(
        &self,
        input: InformationCursorIssueInput,
    ) -> Result<String, InformationCursorStoreError> {
        if input.limit < 1 {
            return Err(InformationCursorStoreError::InvalidMetadata);
        }

        let now = self.inner.options.clock.now_millis();
        let mut entries = self
            .inner
            .entries
            .lock()
            .map_err(|_| InformationCursorStoreError::LockPoisoned { operation: "issue" })?;
        entries.retain(|_, stored| !is_expired(Some(&stored.valid_until), now));
        if entries.len() >= self.inner.options.max_entries
            || count_entries(&entries, |stored| stored.principal_id == input.principal_id)
                >= self.inner.options.max_entries_per_principal
            || count_entries(&entries, |stored| stored.interface_id == input.interface_id)
                >= self.inner.options.max_entries_per_interface
        {
            return Err(InformationCursorStoreError::CapacityExceeded);
        }

        let page_state = clone_bounded_json(
            &input.page_state,
            self.inner.options.max_page_state_bytes,
            || InformationCursorStoreError::PageStateNotJsonSerializable,
            |actual, maximum| InformationCursorStoreError::PageStateByteLimitExceeded {
                actual,
                maximum,
            },
        )?;
        let valid_until_millis = self
            .inner
            .options
            .clock
            .now_millis()
            .checked_add(
                i64::try_from(self.inner.options.ttl_ms)
                    .map_err(|_| InformationCursorStoreError::TimestampOutOfRange)?,
            )
            .ok_or(InformationCursorStoreError::TimestampOutOfRange)?;
        let valid_until = format_utc_millis(valid_until_millis, || {
            InformationCursorStoreError::TimestampOutOfRange
        })?;
        let id = next_unique_cursor_id(&entries);
        let screen_instance_id = input
            .scope
            .screen_instance_id
            .clone()
            .filter(|screen| !screen.is_empty());
        entries.insert(
            id.clone(),
            StoredCursor {
                interface_id: input.interface_id,
                fields: input.fields,
                selector_id: input.selector.map(|selector| selector.id),
                information_revision: input.information_revision,
                limit: input.limit,
                page_state,
                principal_id: input.principal_id,
                grant_id: input.grant.id,
                audience: input.grant.audience,
                connection_epoch: input.scope.connection_epoch,
                world_id: input.scope.world_id.filter(|world| !world.is_empty()),
                dimension: input
                    .scope
                    .dimension
                    .filter(|dimension| !dimension.is_empty()),
                screen_revision: screen_instance_id.as_ref().and(input.scope.screen_revision),
                screen_instance_id,
                valid_until,
            },
        );
        Ok(id)
    }

    pub fn resolve(
        &self,
        input: InformationCursorResolveInput,
    ) -> Result<Option<InformationCursorResolution>, InformationCursorStoreError> {
        let now = self.inner.options.clock.now_millis();
        let mut entries =
            self.inner
                .entries
                .lock()
                .map_err(|_| InformationCursorStoreError::LockPoisoned {
                    operation: "resolve",
                })?;
        let Some(stored) = entries.get(&input.cursor) else {
            return Ok(None);
        };
        if is_expired(Some(&stored.valid_until), now) {
            entries.remove(&input.cursor);
            return Ok(None);
        }
        if stored.interface_id != input.interface_id
            || stored.principal_id != input.principal_id
            || stored.grant_id != input.grant.id
            || stored.audience != input.grant.audience
            || stored.limit != input.limit
            || stored.selector_id.as_deref()
                != input.selector.as_ref().map(|selector| selector.id.as_str())
            || stored.connection_epoch != input.scope.connection_epoch
            || stored.world_id != input.scope.world_id
            || stored.dimension != input.scope.dimension
            || stored.screen_instance_id != input.scope.screen_instance_id
            || stored.screen_revision != input.scope.screen_revision
            || stored.fields != input.fields
        {
            return Ok(None);
        }
        let Some(stored) = entries.remove(&input.cursor) else {
            return Ok(None);
        };
        Ok(Some(InformationCursorResolution {
            state: stored.page_state,
            information_revision: stored.information_revision,
        }))
    }

    pub fn invalidate(
        &self,
        event: &InformationInvalidationEvent,
    ) -> Result<(), InformationCursorStoreError> {
        let mut entries =
            self.inner
                .entries
                .lock()
                .map_err(|_| InformationCursorStoreError::LockPoisoned {
                    operation: "invalidate",
                })?;
        entries.retain(|_, stored| {
            let remove = match event {
                InformationInvalidationEvent::GrantEnded { grant_id } => {
                    stored.grant_id == *grant_id
                }
                InformationInvalidationEvent::ConnectionChanged { connection_epoch } => {
                    stored.connection_epoch != *connection_epoch
                }
                InformationInvalidationEvent::WorldChanged {
                    world_id,
                    dimension,
                } => stored.world_id != *world_id || stored.dimension != *dimension,
                InformationInvalidationEvent::ScreenChanged {
                    screen_instance_id,
                    screen_revision,
                } => {
                    stored.screen_instance_id != *screen_instance_id
                        || stored.screen_revision != *screen_revision
                }
            };
            !remove
        });
        Ok(())
    }

    pub fn size(&self) -> Result<usize, InformationCursorStoreError> {
        let entries =
            self.inner
                .entries
                .lock()
                .map_err(|_| InformationCursorStoreError::LockPoisoned {
                    operation: "read size",
                })?;
        Ok(entries.len())
    }
}

impl Default for InformationCursorStore {
    fn default() -> Self {
        Self {
            inner: Arc::new(InformationCursorStoreInner {
                options: InformationCursorStoreOptions::default(),
                entries: Mutex::new(HashMap::new()),
            }),
        }
    }
}

fn validate_options(
    options: &InformationCursorStoreOptions,
) -> Result<(), InformationCursorStoreError> {
    if options.max_entries < 1
        || options.max_entries_per_principal < 1
        || options.max_entries_per_interface < 1
        || options.max_page_state_bytes < 1
        || options.ttl_ms < 1
    {
        return Err(InformationCursorStoreError::InvalidLimits);
    }
    Ok(())
}

fn count_entries(
    entries: &HashMap<String, StoredCursor>,
    predicate: impl Fn(&StoredCursor) -> bool,
) -> usize {
    entries.values().filter(|stored| predicate(stored)).count()
}

fn next_unique_cursor_id(entries: &HashMap<String, StoredCursor>) -> String {
    loop {
        let id = format!("icur_{}", Uuid::new_v4());
        if !entries.contains_key(&id) {
            return id;
        }
    }
}
