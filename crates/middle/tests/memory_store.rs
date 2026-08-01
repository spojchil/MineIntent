//! Issue #127 单文本 memory 的 characterization/contract tests。

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use mineintent_middle::memory::{MemoryError, MemoryStore};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[tokio::test]
async fn append_replace_and_rewrite_read_the_current_full_text() {
    let (directory, path) = temp_path("memory-ops");
    let store = MemoryStore::new(&path);

    assert_eq!(store.read_full().await.unwrap(), "");
    assert_eq!(store.append("第一段").await.unwrap(), "第一段");
    assert_eq!(store.append("第二段").await.unwrap(), "第一段第二段");
    assert_eq!(store.replace("第一段", "首段").await.unwrap(), "首段第二段");
    assert_eq!(store.rewrite("").await.unwrap(), "");
    assert_eq!(store.read_full().await.unwrap(), "");

    cleanup(directory);
}

#[tokio::test]
async fn anchored_replace_rejects_empty_missing_and_duplicate_anchors_without_writing() {
    let (directory, path) = temp_path("memory-anchor");
    let store = MemoryStore::new(&path);
    store.rewrite("same\nother\nsame").await.unwrap();

    assert!(matches!(
        store.replace("", "x").await,
        Err(MemoryError::EmptyAnchor)
    ));
    assert!(matches!(
        store.replace("missing", "x").await,
        Err(MemoryError::AnchorNotUnique { count: 0 })
    ));
    assert!(matches!(
        store.replace("same", "x").await,
        Err(MemoryError::AnchorNotUnique { count: 2 })
    ));
    assert_eq!(store.read_full().await.unwrap(), "same\nother\nsame");

    store.rewrite("aaa").await.unwrap();
    assert!(matches!(
        store.replace("aa", "x").await,
        Err(MemoryError::AnchorNotUnique { count: 2 })
    ));
    assert_eq!(store.read_full().await.unwrap(), "aaa");

    cleanup(directory);
}

#[tokio::test]
async fn failed_anchor_edits_do_not_create_or_rotate_a_backup() {
    let (directory, path) = temp_path("memory-anchor-backup");
    let store = MemoryStore::new(&path);
    store.rewrite("same\nother\nsame").await.unwrap();

    for (old_text, new_text) in [("", "x"), ("missing", "x"), ("same", "x")] {
        assert!(store.replace(old_text, new_text).await.is_err());
        assert!(!backup_path(&path).exists());
        assert!(!previous_backup_path(&path).exists());
    }
    assert_eq!(store.replace("other", "").await.unwrap(), "same\n\nsame");
    assert_eq!(
        fs::read_to_string(backup_path(&path)).unwrap(),
        "same\nother\nsame"
    );
    cleanup(directory);
}

#[tokio::test]
async fn legacy_schema_rejects_unknown_fields_and_invalid_created_at() {
    let (directory, path) = temp_path("memory-legacy-strict");
    let legacy = path.parent().unwrap().join("memories.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let record = r#"{
      "protocol":"mineintent.memory.v1", "id":"00000000-0000-4000-8000-000000000001",
      "worldId":"w", "kind":"episode", "summary":"ok", "keywords":["ok"],
      "evidence":[{"kind":"event","id":"e1"}],
      "createdAt":"2026-08-01T00:00:00Z", "status":"active"
    }"#;
    fs::write(
        &legacy,
        format!(r#"{{"protocol":"mineintent.memory-file.v1","records":[{record}]}}"#),
    )
    .unwrap();
    let unknown_record =
        record.replace(r#""status":"active""#, r#""status":"active","future":true"#);
    let unknown =
        format!(r#"{{"protocol":"mineintent.memory-file.v1","records":[{unknown_record}]}}"#);
    fs::write(&legacy, unknown).unwrap();
    let store = MemoryStore::new(&path);
    assert!(store.read_full().await.is_err());
    assert!(!path.exists());

    let invalid_record = record.replace("2026-08-01T00:00:00Z", "not-a-date");
    let invalid_date =
        format!(r#"{{"protocol":"mineintent.memory-file.v1","records":[{invalid_record}]}}"#);
    fs::write(&legacy, invalid_date).unwrap();
    assert!(store.read_full().await.is_err());
    assert!(!path.exists());
    cleanup(directory);
}

#[tokio::test]
async fn legacy_schema_matches_ts_zod_uuid_datetime_and_utf16_rules() {
    let (directory, path) = temp_path("memory-legacy-zod-valid");
    let legacy = path.parent().unwrap().join("memories.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let valid_record = serde_json::json!({
        "protocol": "mineintent.memory.v1",
        "id": "00000000-0000-4000-8000-000000000001",
        "worldId": "w",
        "kind": "episode",
        "summary": "ok",
        "keywords": [],
        "evidence": [{"kind": "event", "id": "e1"}],
        "createdAt": "2024-02-29T12:34Z",
        "status": "active"
    });
    fs::write(
        &legacy,
        serde_json::to_vec(&serde_json::json!({
            "protocol": "mineintent.memory-file.v1",
            "records": [valid_record.clone()]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        MemoryStore::new(&path).read_full().await.unwrap(),
        "ok (2024-02-29T12:34Z)"
    );
    cleanup(directory);

    for (label, field, value) in [
        (
            "memory-legacy-zod-offset",
            "createdAt",
            serde_json::json!("2024-02-29T12:34:00+00:00"),
        ),
        (
            "memory-legacy-zod-calendar",
            "createdAt",
            serde_json::json!("2026-02-31T12:34:00Z"),
        ),
        (
            "memory-legacy-zod-uuid",
            "id",
            serde_json::json!("00000000-0000-0000-8000-000000000001"),
        ),
        (
            "memory-legacy-zod-summary-utf16",
            "summary",
            serde_json::json!("😀".repeat(501)),
        ),
        (
            "memory-legacy-zod-keyword-utf16",
            "keywords",
            serde_json::json!(["😀".repeat(33)]),
        ),
    ] {
        let (directory, path) = temp_path(label);
        let legacy = path.parent().unwrap().join("memories.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut invalid_record = valid_record.clone();
        invalid_record[field] = value;
        fs::write(
            &legacy,
            serde_json::to_vec(&serde_json::json!({
                "protocol": "mineintent.memory-file.v1",
                "records": [invalid_record]
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(
            MemoryStore::new(&path).read_full().await.is_err(),
            "{label}"
        );
        assert!(!path.exists(), "{label}");
        cleanup(directory);
    }
}

#[tokio::test]
async fn writes_rotate_backup_before_each_change_and_external_edits_are_seen() {
    let (directory, path) = temp_path("memory-backup");
    let store = MemoryStore::new(&path);
    store.rewrite("one").await.unwrap();
    assert!(!backup_path(&path).exists());
    store.rewrite("two").await.unwrap();
    assert_eq!(fs::read_to_string(backup_path(&path)).unwrap(), "one");
    store.rewrite("three").await.unwrap();
    assert_eq!(fs::read_to_string(backup_path(&path)).unwrap(), "two");
    assert_eq!(
        fs::read_to_string(previous_backup_path(&path)).unwrap(),
        "one"
    );

    fs::write(&path, "outside").unwrap();
    assert_eq!(store.read_full().await.unwrap(), "outside");
    cleanup(directory);
}

#[tokio::test]
async fn legacy_memories_json_migrates_once_in_deterministic_time_order_and_is_backed_up() {
    let (directory, path) = temp_path("memory-migrate");
    let legacy = path.parent().unwrap().join("memories.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &legacy,
        r#"{
          "protocol":"mineintent.memory-file.v1",
          "records":[
            {
              "protocol":"mineintent.memory.v1", "id":"00000000-0000-4000-8000-000000000002",
              "worldId":"w", "kind":"episode", "summary":"later", "keywords":["later"],
              "evidence":[{"kind":"event","id":"e2"}],
              "createdAt":"2026-08-02T00:00:00Z", "status":"active"
            },
            {
              "protocol":"mineintent.memory.v1", "id":"00000000-0000-4000-8000-000000000001",
              "worldId":"w", "kind":"place", "summary":"earlier", "keywords":["earlier"],
              "evidence":[{"kind":"action_result","id":"e1"}],
              "createdAt":"2026-08-01T00:00:00Z", "status":"active"
            }
          ]
        }"#,
    )
    .unwrap();

    let store = MemoryStore::new(&path);
    assert_eq!(
        store.read_full().await.unwrap(),
        "earlier (2026-08-01T00:00:00Z)\nlater (2026-08-02T00:00:00Z)"
    );
    assert!(path.exists());
    assert_eq!(
        fs::read_to_string(backup_path(&legacy)).unwrap(),
        fs::read_to_string(&legacy).unwrap()
    );

    fs::write(&legacy, "external legacy edit").unwrap();
    assert!(store.read_full().await.unwrap().starts_with("earlier"));
    cleanup(directory);
}

#[tokio::test]
async fn legacy_migration_orders_optional_seconds_and_fractions_by_time_then_uuid() {
    let (directory, path) = temp_path("memory-migrate-time-order");
    let legacy = path.parent().unwrap().join("memories.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let records = [
        (
            "00000000-0000-4000-8000-000000000002",
            "tenth",
            "2026-08-01T00:00:00.1Z",
        ),
        (
            "00000000-0000-4000-8000-000000000003",
            "minute",
            "2026-08-01T00:00Z",
        ),
        (
            "00000000-0000-4000-8000-000000000001",
            "exact",
            "2026-08-01T00:00:00Z",
        ),
        (
            "00000000-0000-4000-8000-000000000004",
            "hundredth",
            "2026-08-01T00:00:00.01Z",
        ),
    ]
    .map(|(id, summary, created_at)| {
        serde_json::json!({
            "protocol": "mineintent.memory.v1",
            "id": id,
            "worldId": "w",
            "kind": "episode",
            "summary": summary,
            "keywords": [],
            "evidence": [{"kind": "event", "id": "e1"}],
            "createdAt": created_at,
            "status": "active"
        })
    });
    fs::write(
        &legacy,
        serde_json::to_vec(&serde_json::json!({
            "protocol": "mineintent.memory-file.v1",
            "records": records
        }))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(
        MemoryStore::new(&path).read_full().await.unwrap(),
        "exact (2026-08-01T00:00:00Z)\nminute (2026-08-01T00:00Z)\nhundredth (2026-08-01T00:00:00.01Z)\ntenth (2026-08-01T00:00:00.1Z)"
    );
    cleanup(directory);
}

#[tokio::test]
async fn concurrent_appends_are_serialized_without_losing_full_text() {
    let (directory, path) = temp_path("memory-concurrent");
    let store = MemoryStore::new(&path);
    let mut tasks = Vec::new();
    for index in 0..8 {
        let store = store.clone();
        tasks.push(tokio::spawn(async move {
            store.append(format!("entry-{index}\n")).await.unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }

    let text = store.read_full().await.unwrap();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 8);
    for index in 0..8 {
        let expected = format!("entry-{index}");
        assert!(lines.iter().any(|line| *line == expected));
    }
    cleanup(directory);
}

#[tokio::test]
async fn independently_constructed_stores_share_a_path_lock() {
    let (directory, path) = temp_path("memory-path-lock");
    let first = MemoryStore::new(&path);
    let second = MemoryStore::new(&path);
    let mut tasks = Vec::new();
    for index in 0..8 {
        let store = if index % 2 == 0 {
            first.clone()
        } else {
            second.clone()
        };
        tasks.push(tokio::spawn(async move {
            store.append(format!("entry-{index}\n")).await.unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let text = first.read_full().await.unwrap();
    assert_eq!(text.lines().count(), 8);
    for index in 0..8 {
        let expected = format!("entry-{index}");
        assert!(text.lines().any(|line| line == expected));
    }
    cleanup(directory);
}

#[tokio::test]
async fn empty_append_is_a_no_op_and_rewrite_empty_creates_a_private_file() {
    let (directory, path) = temp_path("memory-empty");
    let store = MemoryStore::new(&path);
    assert_eq!(store.append("").await.unwrap(), "");
    assert!(!path.exists());
    assert_eq!(store.rewrite("").await.unwrap(), "");
    assert!(path.exists());
    cleanup(directory);
}

#[cfg(unix)]
#[tokio::test]
async fn memory_and_backup_files_use_private_mode() {
    use std::os::unix::fs::PermissionsExt;

    let (directory, path) = temp_path("memory-mode");
    let store = MemoryStore::new(&path);
    store.rewrite("one").await.unwrap();
    store.rewrite("two").await.unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(backup_path(&path))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    cleanup(directory);
}

fn temp_path(label: &str) -> (PathBuf, PathBuf) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!("mineintent-{label}-{nonce}-{id}"));
    let path = directory.join("memory.md");
    (directory, path)
}

fn backup_path(path: &PathBuf) -> PathBuf {
    PathBuf::from(format!("{}.bak", path.display()))
}

fn previous_backup_path(path: &PathBuf) -> PathBuf {
    PathBuf::from(format!("{}.bak.1", path.display()))
}

fn cleanup(directory: PathBuf) {
    let _ = fs::remove_dir_all(directory);
}
