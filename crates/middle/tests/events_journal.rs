//! `events/journal.ts` 没有独立 TS 测试；本文件是持久格式与 I/O 边界的
//! characterization/contract tests，不计作 TS 测试的一一迁移。

use std::{collections::BTreeMap, path::PathBuf};

#[cfg(unix)]
use mineintent_middle::events::JOURNAL_FILE_MODE;
use mineintent_middle::events::{JournalEvent, JournalEventProtocol, JsonlEventJournal};
use serde_json::{json, Value};
use uuid::Uuid;

#[test]
fn event_v1_envelope_is_strict_and_distinct_from_backend_event_v2() {
    let fixture = json!({
        "protocol": "mineintent.event.v1",
        "id": "00000000-0000-4000-8000-000000000001",
        "type": "participant.started",
        "occurredAt": "2026-08-01T00:00:00.000Z",
        "worldId": "world-1",
        "sessionId": "session-1",
        "payload": {"ready": true}
    });
    let event: JournalEvent =
        serde_json::from_value(fixture.clone()).expect("event.v1 fixture is valid");
    assert_eq!(event.protocol, JournalEventProtocol::V1);
    assert_eq!(serde_json::to_value(event).unwrap(), fixture);

    let mut unknown = fixture.clone();
    unknown["source"] = json!("server_observed");
    assert!(serde_json::from_value::<JournalEvent>(unknown).is_err());

    let mut backend_protocol = fixture;
    backend_protocol["protocol"] = json!("mineintent.minecraft.backend-event.v2");
    assert!(serde_json::from_value::<JournalEvent>(backend_protocol).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_appends_are_complete_serial_lines_and_flush_is_visible() {
    let (root, file) = journal_path("serialized");
    let journal = JsonlEventJournal::new(&file, "world-1", "session-1").unwrap();

    let first = journal
        .append("ordered.first", json!({"index": 0}))
        .await
        .unwrap();
    let mut tasks = Vec::new();
    for index in 1..=24_u64 {
        let journal = journal.clone();
        tasks.push(tokio::spawn(async move {
            journal
                .append("concurrent", json!({"index": index}))
                .await
                .unwrap()
        }));
    }

    let mut returned = BTreeMap::new();
    returned.insert(first.id, serde_json::to_value(first).unwrap());
    for task in tasks {
        let event = task.await.unwrap();
        returned.insert(event.id, serde_json::to_value(event).unwrap());
    }
    journal.flush().await.unwrap();

    let contents = tokio::fs::read_to_string(&file).await.unwrap();
    assert!(contents.ends_with('\n'));
    let lines: Vec<_> = contents.lines().collect();
    assert_eq!(lines.len(), 25);

    let mut written_ids = Vec::new();
    for line in &lines {
        assert!(!line.contains('\n'));
        let value: Value =
            serde_json::from_str(line).expect("every physical line is complete JSON");
        let event: JournalEvent = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(event.world_id, "world-1");
        assert_eq!(event.session_id, "session-1");
        assert!(looks_like_utc_millis(&event.occurred_at));
        assert_eq!(event.id.get_version_num(), 4);
        assert_eq!(returned.get(&event.id), Some(&value));
        written_ids.push(event.id);
    }
    assert!(lines[0].contains("ordered.first"));
    written_ids.sort_unstable();
    let mut returned_ids: Vec<_> = returned.keys().copied().collect();
    returned_ids.sort_unstable();
    assert_eq!(written_ids, returned_ids);

    drop(journal);
    tokio::fs::remove_dir_all(root).await.unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn journal_file_permissions_are_owner_read_write_only() {
    use std::os::unix::fs::PermissionsExt;

    let (root, file) = journal_path("mode");
    let journal = JsonlEventJournal::new(&file, "world", "session").unwrap();
    journal.append("mode.checked", json!({})).await.unwrap();
    journal.flush().await.unwrap();

    let mode = tokio::fs::metadata(&file)
        .await
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, JOURNAL_FILE_MODE);

    drop(journal);
    tokio::fs::remove_dir_all(root).await.unwrap();
}

fn journal_path(label: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("mineintent-events-{label}-{}", Uuid::new_v4()));
    let file = root.join("nested").join("events.jsonl");
    (root, file)
}

fn looks_like_utc_millis(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 24
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'.'
        && bytes[23] == b'Z'
        && bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 4 | 7 | 10 | 13 | 16 | 19 | 23) || byte.is_ascii_digit()
        })
}
