use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use mineintent_contracts::agent::{
    AgentError, JsonObject, ModelName, ModelUsage, RunId, WireToolDefinition,
};
use serde::Serialize;

pub const TRANSCRIPT_PROTOCOL: &str = "mineintent.agent-transcript.v1";
pub const TRANSCRIPT_FILE_NAME: &str = "agent-transcripts.jsonl";
pub const MAX_TRANSCRIPT_CHARS: usize = 262_144;
pub const MAX_TRANSCRIPT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ERROR_CHARS: usize = 300;

/// The usage shape used by the v1 transcript wire, which intentionally keeps
/// the provider-facing counters in the old snake_case names.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TranscriptUsage {
    #[serde(rename = "prompt_tokens", skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(rename = "completion_tokens", skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
}

impl TranscriptUsage {
    fn from_model_usage(usage: &ModelUsage) -> Option<Self> {
        let usage = Self {
            prompt_tokens: usage.input_tokens,
            completion_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
        };
        (usage.prompt_tokens.is_some()
            || usage.completion_tokens.is_some()
            || usage.cache_read_tokens.is_some()
            || usage.cache_write_tokens.is_some())
        .then_some(usage)
    }
}

/// One replayable v1 record before compact JSON serialization.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AgentTranscriptRecord {
    pub protocol: &'static str,
    #[serde(rename = "runId")]
    pub run_id: RunId,
    pub model: ModelName,
    #[serde(rename = "endedAt")]
    pub ended_at: String,
    pub tools: Vec<String>,
    #[serde(rename = "toolSchemas")]
    pub tool_schemas: Option<Vec<WireToolDefinition>>,
    pub closing: Option<String>,
    pub usage: Option<TranscriptUsage>,
    pub error: Option<String>,
    pub messages: Option<Vec<JsonObject>>,
    #[serde(skip_serializing_if = "is_false")]
    pub truncated: bool,
}

impl AgentTranscriptRecord {
    pub fn new(
        run_id: RunId,
        model: ModelName,
        tools: &[WireToolDefinition],
        messages: Vec<JsonObject>,
        closing: Option<String>,
        usage: Option<ModelUsage>,
        error: Option<String>,
    ) -> Self {
        Self::at(
            run_id,
            model,
            tools,
            messages,
            closing,
            usage,
            error,
            utc_timestamp_now(),
        )
    }

    /// Deterministic constructor useful for serialization tests and offline
    /// callers that already own their UTC second string.
    pub fn at(
        run_id: RunId,
        model: ModelName,
        tools: &[WireToolDefinition],
        messages: Vec<JsonObject>,
        closing: Option<String>,
        usage: Option<ModelUsage>,
        error: Option<String>,
        ended_at: impl Into<String>,
    ) -> Self {
        Self {
            protocol: TRANSCRIPT_PROTOCOL,
            run_id,
            model,
            ended_at: ended_at.into(),
            tools: tools
                .iter()
                .map(|tool| tool.function.name.as_str().to_owned())
                .collect(),
            tool_schemas: Some(tools.to_vec()),
            closing: closing.map(|value| value.trim().to_owned()),
            usage: usage.as_ref().and_then(TranscriptUsage::from_model_usage),
            error: error.map(|value| truncate_chars(&value, MAX_ERROR_CHARS)),
            messages: Some(messages),
            truncated: false,
        }
    }

    /// Serializes one compact, non-ASCII-escaped line without its newline.
    ///
    /// The size guard deliberately counts Unicode scalar values rather than
    /// UTF-8 bytes to preserve the Python oracle's `len(str)` boundary.
    pub fn to_json_line(&self) -> Result<String, serde_json::Error> {
        let mut line = serde_json::to_string(self)?;
        if line.chars().count() > MAX_TRANSCRIPT_CHARS {
            let mut truncated = self.clone();
            truncated.messages = None;
            truncated.tool_schemas = None;
            truncated.truncated = true;
            line = serde_json::to_string(&truncated)?;
        }
        Ok(line)
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Formats the stable error summary retained by a failed transcript.
pub fn summarize_error(error: &AgentError) -> String {
    let prefix = format!("{}: ", error.code);
    let available = MAX_ERROR_CHARS.saturating_sub(prefix.chars().count());
    let mut summary = prefix;
    summary.extend(error.summary.chars().take(available));
    summary
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

/// Resolves the transcript path without reading or changing process state.
/// `None`, an empty string, and a whitespace-only string use the private
/// `.mineintent` fallback directory.
pub fn transcript_path(data_dir: Option<&str>) -> PathBuf {
    let directory = data_dir.unwrap_or_default().trim();
    if directory.is_empty() {
        PathBuf::from(".mineintent").join(TRANSCRIPT_FILE_NAME)
    } else {
        Path::new(directory).join(TRANSCRIPT_FILE_NAME)
    }
}

/// Resolves the default path from the environment once, for explicit assembly
/// of a file store. The ordinary `ConcreteAgentRunner::new` never calls this.
pub fn transcript_path_from_env() -> PathBuf {
    let data_dir = std::env::var("MINEINTENT_DATA_DIR").ok();
    transcript_path(data_dir.as_deref())
}

/// A fail-able sink for already serialized transcript lines.
pub trait TranscriptSink: Send + Sync {
    fn append_line(&self, line: &str) -> io::Result<()>;

    fn append_record(&self, record: &AgentTranscriptRecord) -> io::Result<()> {
        let line = record
            .to_json_line()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        self.append_line(&line)
    }
}

#[derive(Debug)]
struct FileTranscriptStoreInner {
    path: PathBuf,
    append_lock: Mutex<()>,
}

/// A bounded, process-local serialized JSONL transcript store.
///
/// Clones share the same lock, so concurrent appends through one store (or any
/// clone of it) cannot race a rotation or interleave records.
#[derive(Clone, Debug)]
pub struct FileTranscriptStore {
    inner: Arc<FileTranscriptStoreInner>,
}

impl FileTranscriptStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            inner: Arc::new(FileTranscriptStoreInner {
                path: path.into(),
                append_lock: Mutex::new(()),
            }),
        }
    }

    pub fn from_environment() -> Self {
        Self::new(transcript_path_from_env())
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn append(&self, record: &AgentTranscriptRecord) -> io::Result<()> {
        let line = record
            .to_json_line()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        self.append_line(&line)
    }

    fn append_line_locked(&self, line: &str) -> io::Result<()> {
        let parent = self.inner.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;

        let existing_size = match fs::metadata(&self.inner.path) {
            Ok(metadata) => Some(metadata.len()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let line_chars = line.chars().count() as u64;
        if existing_size
            .map(|size| size.saturating_add(line_chars) > MAX_TRANSCRIPT_BYTES)
            .unwrap_or(false)
        {
            rotate(&self.inner.path)?;
        }

        let mut options = OpenOptions::new();
        options.write(true).create(true).append(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&self.inner.path)?;

        let mut encoded = Vec::with_capacity(line.len() + 1);
        encoded.extend_from_slice(line.as_bytes());
        encoded.push(b'\n');
        file.write_all(&encoded)
    }
}

impl Default for FileTranscriptStore {
    fn default() -> Self {
        Self::from_environment()
    }
}

impl TranscriptSink for FileTranscriptStore {
    fn append_line(&self, line: &str) -> io::Result<()> {
        let _guard = self
            .inner
            .append_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.append_line_locked(line)
    }
}

fn rotate(path: &Path) -> io::Result<()> {
    let rotated = rotated_path(path);
    match fs::remove_file(&rotated) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::rename(path, rotated)
}

fn rotated_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map(|value| value.to_os_string())
        .unwrap_or_default();
    file_name.push(".1");
    path.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(file_name)
}

fn utc_timestamp_now() -> String {
    utc_timestamp(SystemTime::now())
}

fn utc_timestamp(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = seconds / 86_400;
    let seconds_in_day = seconds % 86_400;
    let (year, month, day) = civil_date_from_days(days);
    let hour = seconds_in_day / 3_600;
    let minute = (seconds_in_day % 3_600) / 60;
    let second = seconds_in_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

// Howard Hinnant's civil-from-days calculation, with days counted from the
// Unix epoch. It avoids adding a time crate solely for second-precision UTC.
fn civil_date_from_days(days_since_epoch: u64) -> (i64, u32, u32) {
    let z = days_since_epoch as i64 + 719_468;
    let era = if z >= 0 {
        z / 146_097
    } else {
        (z - 146_096) / 146_097
    };
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = if month_part < 10 {
        month_part + 3
    } else {
        month_part - 9
    };
    let year = year + i64::from(month <= 2);
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::{civil_date_from_days, utc_timestamp};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn utc_format_is_second_precision_and_uses_gregorian_utc() {
        assert_eq!(
            utc_timestamp(UNIX_EPOCH + Duration::from_secs(0)),
            "1970-01-01T00:00:00Z"
        );
        assert_eq!(
            utc_timestamp(UNIX_EPOCH + Duration::from_secs(1_704_067_200)),
            "2024-01-01T00:00:00Z"
        );
    }

    #[test]
    fn civil_date_handles_leap_year_boundary() {
        assert_eq!(civil_date_from_days(18_262), (2020, 1, 1));
        assert_eq!(civil_date_from_days(18_321), (2020, 2, 29));
    }
}
