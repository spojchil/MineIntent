use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use super::contracts::{
    InformationGrant, InformationInterfaceId, InformationInvalidationEvent,
    InformationReferenceIssueError, InformationReferenceIssueRequest, InformationReferenceIssuer,
    InformationScopeSnapshot, InformationSelectorRef, InformationSelectorRefProtocol,
};

pub const DEFAULT_MAX_REFERENCE_ENTRIES: usize = 2_048;
pub const DEFAULT_MAX_REFERENCE_ENTRIES_PER_PRINCIPAL: usize = 512;
pub const DEFAULT_MAX_REFERENCE_ENTRIES_PER_INTERFACE: usize = 256;
pub const DEFAULT_MAX_REFERENCE_PAYLOAD_BYTES: usize = 8_192;
pub const DEFAULT_MAX_REFERENCE_ISSUES_PER_ISSUER: usize = 32;
pub const DEFAULT_REFERENCE_TTL_MS: u64 = 60_000;

pub trait InformationRefClock: Send + Sync {
    fn now_millis(&self) -> i64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemInformationRefClock;

impl InformationRefClock for SystemInformationRefClock {
    fn now_millis(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
            Err(error) => {
                let millis = i64::try_from(error.duration().as_millis()).unwrap_or(i64::MAX);
                millis.saturating_neg()
            }
        }
    }
}

#[derive(Clone)]
pub struct InformationRefStoreOptions {
    pub max_entries: usize,
    pub max_entries_per_principal: usize,
    pub max_entries_per_interface: usize,
    pub max_payload_bytes: usize,
    pub max_issues_per_issuer: usize,
    pub ttl_ms: u64,
    pub clock: Arc<dyn InformationRefClock>,
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
            clock: Arc::new(SystemInformationRefClock),
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
                .is_some_and(|timestamp| parse_rfc3339_millis(timestamp).is_none())
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
            .and_then(parse_rfc3339_millis)
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

pub(crate) fn clone_bounded_json<E>(
    value: &Value,
    maximum: usize,
    not_serializable: impl Fn() -> E,
    too_large: impl Fn(usize, usize) -> E,
) -> Result<Value, E> {
    let serialized = serde_json::to_vec(value).map_err(|_| not_serializable())?;
    if serialized.len() > maximum {
        return Err(too_large(serialized.len(), maximum));
    }
    serde_json::from_slice(&serialized).map_err(|_| not_serializable())
}

pub(crate) fn is_expired(valid_until: Option<&str>, now: i64) -> bool {
    valid_until
        .and_then(parse_rfc3339_millis)
        .is_some_and(|valid_until| valid_until <= now)
}

pub(crate) fn parse_rfc3339_millis(timestamp: &str) -> Option<i64> {
    let (date, time_and_zone) = timestamp.split_once('T')?;
    let date_parts: Vec<_> = date.split('-').collect();
    if date_parts.len() != 3
        || date_parts[0].len() != 4
        || date_parts[1].len() != 2
        || date_parts[2].len() != 2
    {
        return None;
    }
    let year = parse_digits(date_parts[0])? as i64;
    let month = parse_digits(date_parts[1])? as u32;
    let day = parse_digits(date_parts[2])? as u32;
    if day < 1 || day > days_in_month(year, month)? {
        return None;
    }

    let (time, offset_minutes) = if let Some(time) = time_and_zone.strip_suffix('Z') {
        (time, 0_i64)
    } else {
        let zone_index = time_and_zone
            .char_indices()
            .skip(1)
            .filter(|(_, character)| *character == '+' || *character == '-')
            .map(|(index, _)| index)
            .last()?;
        let (time, zone) = time_and_zone.split_at(zone_index);
        if zone.len() != 6 || zone.as_bytes().get(3) != Some(&b':') {
            return None;
        }
        let hours = parse_digits(&zone[1..3])? as i64;
        let minutes = parse_digits(&zone[4..6])? as i64;
        if hours > 23 || minutes > 59 {
            return None;
        }
        let magnitude = hours.checked_mul(60)?.checked_add(minutes)?;
        let offset = if zone.starts_with('+') {
            magnitude
        } else {
            -magnitude
        };
        (time, offset)
    };

    let (whole_time, fraction) = match time.split_once('.') {
        Some((whole, fraction)) if !fraction.is_empty() => (whole, Some(fraction)),
        Some(_) => return None,
        None => (time, None),
    };
    let time_parts: Vec<_> = whole_time.split(':').collect();
    if time_parts.len() != 3 || time_parts.iter().any(|part| part.len() != 2) {
        return None;
    }
    let hour = parse_digits(time_parts[0])? as i64;
    let minute = parse_digits(time_parts[1])? as i64;
    let second = parse_digits(time_parts[2])? as i64;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let millis = match fraction {
        Some(fraction) if fraction.bytes().all(|byte| byte.is_ascii_digit()) => {
            let mut digits = fraction.bytes().take(3).collect::<Vec<_>>();
            while digits.len() < 3 {
                digits.push(b'0');
            }
            i64::from(digits[0] - b'0') * 100
                + i64::from(digits[1] - b'0') * 10
                + i64::from(digits[2] - b'0')
        }
        Some(_) => return None,
        None => 0,
    };
    let days = days_from_civil(year, month, day);
    let local_millis = i128::from(days)
        .checked_mul(86_400_000)?
        .checked_add(i128::from(
            hour * 3_600_000 + minute * 60_000 + second * 1_000 + millis,
        ))?;
    i64::try_from(local_millis - i128::from(offset_minutes * 60_000)).ok()
}

pub(crate) fn format_utc_millis<E>(
    timestamp: i64,
    out_of_range: impl Fn() -> E,
) -> Result<String, E> {
    let days = timestamp.div_euclid(86_400_000);
    let time = timestamp.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    if !(0..=9_999).contains(&year) {
        return Err(out_of_range());
    }
    let hour = time / 3_600_000;
    let minute = time % 3_600_000 / 60_000;
    let second = time % 60_000 / 1_000;
    let millis = time % 1_000;
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z"
    ))
}

fn parse_digits(value: &str) -> Option<u32> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn days_in_month(year: i64, month: u32) -> Option<u32> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => Some(29),
        2 => Some(28),
        _ => None,
    }
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
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
