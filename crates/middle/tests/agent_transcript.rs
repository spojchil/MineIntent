use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
};

use mineintent_contracts::agent::{
    fixtures, JsonObject, ModelName, ModelUsage, RunId, ToolDefinitionName, WireToolDefinition,
};
use mineintent_middle::agent::{
    transcript_path, AgentTranscriptRecord, FileTranscriptStore, TranscriptSink,
    MAX_TRANSCRIPT_BYTES, MAX_TRANSCRIPT_CHARS,
};
use serde_json::{json, Value};
use uuid::Uuid;

struct TempDirectory {
    path: PathBuf,
}

impl TempDirectory {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("mineintent-transcript-{label}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("temporary directory");
        Self { path }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn object(value: Value) -> JsonObject {
    value.as_object().expect("object fixture").clone()
}

fn second_tool() -> WireToolDefinition {
    let mut tool = fixtures::tool_definition();
    tool.function.name = ToolDefinitionName::new("say").expect("valid tool name");
    tool.function.description = "说话".to_owned();
    tool
}

fn record_with_content(content: &str) -> AgentTranscriptRecord {
    let tools = Vec::new();
    AgentTranscriptRecord::at(
        RunId::new("transcript-run").expect("valid run id"),
        ModelName::new("model-v1").expect("valid model name"),
        &tools,
        vec![object(json!({"role": "user", "content": content}))],
        Some("  完成  ".to_owned()),
        Some(ModelUsage {
            input_tokens: Some(0),
            output_tokens: Some(7),
            cache_read_tokens: None,
            cache_write_tokens: Some(0),
        }),
        None,
        "2026-08-02T12:34:56Z",
    )
}

fn read_json_lines(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .expect("transcript file")
        .lines()
        .map(|line| serde_json::from_str(line).expect("complete JSON line"))
        .collect()
}

#[test]
fn success_record_has_exact_wire_fields_usage_and_replay_boundary() {
    let tools = vec![fixtures::tool_definition(), second_tool()];
    let messages = vec![
        object(json!({"role": "system", "content": "稳定"})),
        object(json!({"role": "user", "content": "帧"})),
        object(json!({
            "role": "assistant",
            "reasoning_content": "观察",
            "tool_calls": [{"id": "call-1", "function": {"name": "look_relative", "arguments": "{}"}}]
        })),
        object(json!({
            "role": "tool",
            "tool_call_id": "call-1",
            "content": "{\"result\":{\"status\":\"completed\"}}"
        })),
    ];
    let record = AgentTranscriptRecord::at(
        RunId::new("run-精确").expect("valid unicode run id"),
        ModelName::new("explicit-model").expect("valid model name"),
        &tools,
        messages.clone(),
        Some("  最终 closing  ".to_owned()),
        Some(ModelUsage {
            input_tokens: Some(12),
            output_tokens: Some(8),
            cache_read_tokens: Some(0),
            cache_write_tokens: Some(4),
        }),
        None,
        "2026-08-02T12:34:56Z",
    );

    let line = record.to_json_line().expect("compact transcript JSON");
    assert!(line.contains("最终 closing"), "non-ASCII must remain UTF-8");
    assert!(line.starts_with("{\"protocol\":"));
    let fields = [
        "protocol",
        "runId",
        "model",
        "endedAt",
        "tools",
        "toolSchemas",
        "closing",
        "usage",
        "error",
        "messages",
    ];
    let positions = fields
        .iter()
        .map(|field| line.find(&format!("\"{field}\":")).expect("wire field"))
        .collect::<Vec<_>>();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(!line.contains("truncated"));

    let value: Value = serde_json::from_str(&line).expect("record JSON");
    assert_eq!(value["protocol"], "mineintent.agent-transcript.v1");
    assert_eq!(value["runId"], "run-精确");
    assert_eq!(value["model"], "explicit-model");
    assert_eq!(value["endedAt"], "2026-08-02T12:34:56Z");
    assert_eq!(value["tools"], json!(["look_relative", "say"]));
    assert_eq!(value["toolSchemas"], serde_json::to_value(&tools).unwrap());
    assert_eq!(value["closing"], "最终 closing");
    assert_eq!(
        value["usage"],
        json!({
            "prompt_tokens": 12,
            "completion_tokens": 8,
            "cache_read_tokens": 0,
            "cache_write_tokens": 4,
        })
    );
    assert!(value["error"].is_null());
    assert_eq!(value["messages"], serde_json::to_value(messages).unwrap());
}

#[test]
fn empty_usage_is_null_and_error_is_truncated_by_unicode_characters() {
    let error = format!("provider_failed: {}", "界".repeat(400));
    let record = AgentTranscriptRecord::at(
        RunId::new("run-failure").unwrap(),
        ModelName::new("model-v1").unwrap(),
        &[],
        Vec::new(),
        None,
        Some(ModelUsage::default()),
        Some(error),
        "2026-08-02T12:34:56Z",
    );

    let value: Value = serde_json::from_str(&record.to_json_line().unwrap()).unwrap();
    assert!(value["usage"].is_null());
    let error = value["error"].as_str().unwrap();
    assert!(error.starts_with("provider_failed: "));
    assert_eq!(error.chars().count(), 300);
}

#[test]
fn transcript_path_is_pure_and_trims_data_directory_input() {
    assert_eq!(
        transcript_path(None),
        PathBuf::from(".mineintent").join("agent-transcripts.jsonl")
    );
    assert_eq!(
        transcript_path(Some("   ")),
        PathBuf::from(".mineintent").join("agent-transcripts.jsonl")
    );
    assert_eq!(
        transcript_path(Some("  custom-data  ")),
        PathBuf::from("custom-data").join("agent-transcripts.jsonl")
    );
}

#[test]
fn truncation_uses_strict_unicode_scalar_boundary_and_preserves_multibyte_lines() {
    let base = record_with_content("").to_json_line().unwrap();
    let base_chars = base.chars().count();
    let exact_ascii = "a".repeat(MAX_TRANSCRIPT_CHARS - base_chars);
    let exact_ascii_line = record_with_content(&exact_ascii).to_json_line().unwrap();
    assert_eq!(exact_ascii_line.chars().count(), MAX_TRANSCRIPT_CHARS);
    assert!(!exact_ascii_line.contains("\"truncated\":true"));

    let exact_unicode = "界".repeat(MAX_TRANSCRIPT_CHARS - base_chars);
    let exact_unicode_line = record_with_content(&exact_unicode).to_json_line().unwrap();
    assert_eq!(exact_unicode_line.chars().count(), MAX_TRANSCRIPT_CHARS);
    assert!(exact_unicode_line.len() > MAX_TRANSCRIPT_CHARS);
    assert!(!exact_unicode_line.contains("\"truncated\":true"));

    let over = "界".repeat(MAX_TRANSCRIPT_CHARS - base_chars + 1);
    let over_line = record_with_content(&over).to_json_line().unwrap();
    let over_value: Value = serde_json::from_str(&over_line).unwrap();
    assert_eq!(over_value["messages"], Value::Null);
    assert_eq!(over_value["toolSchemas"], Value::Null);
    assert_eq!(over_value["truncated"], true);
}

#[test]
fn rotation_is_strictly_greater_and_replaces_old_backup() {
    let temp = TempDirectory::new("rotation");
    let path = temp.path("agent-transcripts.jsonl");
    let backup = temp.path("agent-transcripts.jsonl.1");
    let line = "{\"value\":\"界\"}";
    let line_chars = line.chars().count() as u64;

    set_file_size(&path, MAX_TRANSCRIPT_BYTES - line_chars);
    let store = FileTranscriptStore::new(&path);
    store.append_line(line).expect("exact-boundary append");
    assert!(!backup.exists(), "equal boundary must not rotate");

    fs::write(&backup, b"old backup").unwrap();
    set_file_size(&path, MAX_TRANSCRIPT_BYTES - line_chars + 1);
    store.append_line(line).expect("over-boundary append");
    assert_eq!(
        fs::metadata(&backup).unwrap().len(),
        MAX_TRANSCRIPT_BYTES - line_chars + 1
    );
    let current = fs::read_to_string(&path).unwrap();
    assert_eq!(current, format!("{line}\n"));
}

#[test]
fn concurrent_appends_from_one_store_are_complete_parseable_records() {
    let temp = TempDirectory::new("concurrent");
    let path = temp.path("nested").join("agent-transcripts.jsonl");
    let store = Arc::new(FileTranscriptStore::new(path.clone()));
    let mut threads = Vec::new();
    for index in 0..64 {
        let store = Arc::clone(&store);
        threads.push(thread::spawn(move || {
            let mut record = record_with_content(&format!("record-{index}"));
            record.run_id = RunId::new(format!("run-{index}")).unwrap();
            store.append(&record).expect("serialized append");
        }));
    }
    for thread in threads {
        thread.join().expect("append thread");
    }

    let records = read_json_lines(&path);
    assert_eq!(records.len(), 64);
    assert!(records
        .iter()
        .all(|record| record["protocol"] == "mineintent.agent-transcript.v1"));
}

#[cfg(unix)]
#[test]
fn newly_created_transcript_file_is_private() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDirectory::new("permissions");
    let path = temp.path("agent-transcripts.jsonl");
    FileTranscriptStore::new(&path)
        .append(&record_with_content("权限"))
        .expect("file append");
    let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn file_store_reports_write_failures_without_faking_a_successful_write() {
    let temp = TempDirectory::new("write-failure");
    let parent_file = temp.path("not-a-directory");
    fs::write(&parent_file, b"blocker").unwrap();
    let path = parent_file.join("agent-transcripts.jsonl");
    let result = FileTranscriptStore::new(path).append(&record_with_content("失败"));
    assert!(result.is_err());
}

fn set_file_size(path: &Path, size: u64) {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(path)
        .expect("size fixture");
    file.set_len(size).expect("sparse size fixture");
}
