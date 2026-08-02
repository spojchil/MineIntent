use std::{
    sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

use super::contracts::{
    DebugStateInput, DebugStateUpdate, ParticipantDebugState, MAX_RECENT_FAILURES,
};

/// An owned, immutable-by-convention snapshot. Cloning the `Arc` never exposes
/// the store's mutable state; callers that need to edit a value must first
/// make their own owned copy.
pub type DebugSnapshot = Arc<ParticipantDebugState>;

#[derive(Clone)]
pub struct DebugStateStore {
    inner: Arc<RwLock<DebugStateInner>>,
}

struct DebugStateInner {
    revision: u64,
    input: DebugStateInput,
}

impl Default for DebugStateStore {
    fn default() -> Self {
        Self::new()
    }
}

impl DebugStateStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(DebugStateInner {
                revision: 0,
                input: DebugStateInput::default(),
            })),
        }
    }

    /// Apply a top-level shallow patch and advance the revision exactly once.
    /// The patch is consumed and every stored field is owned by the store.
    pub fn update<U>(&self, update: U)
    where
        U: Into<DebugStateUpdate>,
    {
        let update = update.into();
        let mut inner = write_recover(&self.inner);
        apply_update(&mut inner.input, update);
        inner.revision = inner.revision.saturating_add(1);
    }

    /// Add one failure, retaining the ten most recent entries in insertion
    /// order, and advance the revision exactly once.
    pub fn failure(&self, failure: super::contracts::DebugFailureSummary) {
        let mut inner = write_recover(&self.inner);
        inner.input.recent_failures.push(failure);
        if inner.input.recent_failures.len() > MAX_RECENT_FAILURES {
            let excess = inner.input.recent_failures.len() - MAX_RECENT_FAILURES;
            inner.input.recent_failures.drain(..excess);
        }
        inner.revision = inner.revision.saturating_add(1);
    }

    /// Capture a fresh UTC timestamp and a redacted, deeply owned state.
    pub fn snapshot(&self) -> DebugSnapshot {
        let (revision, input) = {
            let inner = read_recover(&self.inner);
            (inner.revision, inner.input.clone())
        };
        let state = ParticipantDebugState::from_input(revision, current_timestamp(), input);
        Arc::new(redact_sensitive(&state))
    }
}

fn apply_update(input: &mut DebugStateInput, update: DebugStateUpdate) {
    if let Some(connection) = update.connection {
        input.connection = connection;
    }
    if let Some(body) = update.body {
        input.body = body;
    }
    if let Some(current_body_tool) = update.current_body_tool {
        input.current_body_tool = current_body_tool;
    }
    if let Some(recent_failures) = update.recent_failures {
        input.recent_failures = recent_failures;
    }
    if let Some(observations) = update.observations {
        input.observations = observations;
    }
    if let Some(decision) = update.decision {
        input.decision = decision;
    }
}

/// Redact a serializable value without mutating the input. The JSON round trip
/// is deliberate: it gives arrays and arbitrary nested objects the same walk
/// semantics as the TypeScript `structuredClone` implementation.
pub fn redact_sensitive<T>(input: &T) -> T
where
    T: Serialize + DeserializeOwned,
{
    let value = serde_json::to_value(input).expect("telemetry values must be JSON serializable");
    serde_json::from_value(redact_sensitive_value(&value))
        .expect("redaction must preserve the telemetry value shape")
}

/// Value-level form useful for redacting diagnostic payloads that do not have
/// a statically declared Rust DTO.
pub fn redact_sensitive_value(input: &Value) -> Value {
    redact_value(input, None)
}

fn redact_value(value: &Value, key: Option<&str>) -> Value {
    if key.is_some_and(is_sensitive_key) || key.is_some_and(is_private_raw_key) {
        return Value::String("[REDACTED]".to_owned());
    }

    match value {
        Value::String(text) => Value::String(redact_string(text)),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| redact_value(item, None)).collect())
        }
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(child_key, child)| {
                    (
                        child_key.clone(),
                        redact_value(child, Some(child_key.as_str())),
                    )
                })
                .collect(),
        ),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let key = javascript_case_fold(key);
    [
        "apikey",
        "api-key",
        "api_key",
        "authorization",
        "cookie",
        "credential",
        "password",
        "profilefolder",
        "profilesfolder",
        "secret",
        "token",
    ]
    .into_iter()
    .any(|suffix| key.ends_with(suffix))
}

fn is_private_raw_key(key: &str) -> bool {
    matches!(
        javascript_case_fold(key).as_str(),
        "content" | "message" | "messages" | "prompt" | "raw" | "transcript"
    )
}

/// Match ECMAScript `/iu` for ASCII literals and `[A-Za-z]`: besides ASCII
/// case pairs, Unicode simple case folding makes LONG S equivalent to `s` and
/// KELVIN SIGN equivalent to `k`.
fn javascript_case_fold(value: &str) -> String {
    value.chars().map(javascript_case_fold_char).collect()
}

fn javascript_case_fold_char(character: char) -> char {
    match character {
        'A'..='Z' => character.to_ascii_lowercase(),
        '\u{017f}' => 's',
        '\u{212a}' => 'k',
        _ => character,
    }
}

fn redact_string(input: &str) -> String {
    let characters: Vec<char> = input.chars().collect();
    let mut output = String::with_capacity(input.len());
    let mut index = 0;

    while index < characters.len() {
        if starts_with_javascript_case_insensitive(&characters, index, "bearer") {
            let mut secret_start = index + "bearer".chars().count();
            if characters
                .get(secret_start)
                .is_some_and(|character| is_javascript_whitespace(*character))
            {
                while characters
                    .get(secret_start)
                    .is_some_and(|character| is_javascript_whitespace(*character))
                {
                    secret_start += 1;
                }
                let secret_end = consume_secret(&characters, secret_start, is_bearer_char);
                if secret_end - secret_start >= 12 {
                    output.push_str("[REDACTED]");
                    index = secret_end;
                    continue;
                }
            }
        }

        if starts_with_javascript_case_insensitive(&characters, index, "sk-") {
            let secret_start = index + 3;
            let secret_end = consume_secret(&characters, secret_start, is_sk_char);
            if secret_end - secret_start >= 12 {
                output.push_str("[REDACTED]");
                index = secret_end;
                continue;
            }
        }

        output.push(characters[index]);
        index += 1;
    }

    output
}

fn starts_with_javascript_case_insensitive(
    characters: &[char],
    index: usize,
    needle: &str,
) -> bool {
    needle.chars().enumerate().all(|(offset, expected)| {
        characters.get(index + offset).is_some_and(|actual| {
            javascript_case_fold_char(*actual) == expected.to_ascii_lowercase()
        })
    })
}

fn consume_secret(characters: &[char], start: usize, allowed: fn(char) -> bool) -> usize {
    let mut end = start;
    while characters
        .get(end)
        .is_some_and(|character| allowed(*character))
    {
        end += 1;
    }
    end
}

fn is_bearer_char(character: char) -> bool {
    is_javascript_ascii_alphanumeric(character)
        || matches!(character, '.' | '_' | '~' | '+' | '/' | '-')
}

fn is_sk_char(character: char) -> bool {
    is_javascript_ascii_alphanumeric(character) || matches!(character, '_' | '-')
}

fn is_javascript_ascii_alphanumeric(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '\u{017f}' | '\u{212a}')
}

fn is_javascript_whitespace(character: char) -> bool {
    // Rust's Unicode White_Space includes NEXT LINE (U+0085), while
    // ECMAScript `\\s` deliberately does not; ECMAScript does include BOM.
    (character.is_whitespace() && character != '\u{0085}') || character == '\u{feff}'
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

fn read_recover<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    match lock.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn write_recover<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    match lock.write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
