use std::collections::BTreeMap;

use mineintent_middle::telemetry::{
    redact_sensitive, BackendState, DebugBodyState, DebugBodyTool, DebugDecision,
    DebugDecisionStatus, DebugFailureSource, DebugFailureSummary, DebugInventoryItem,
    DebugStateInput, DebugStateProtocol, DebugStateStore, DebugStateUpdate, LocalDebugServer,
    PassiveObservations, Vec3Value,
};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

#[test]
fn debug_state_is_immutable_bounded_and_redacts_sensitive_values() {
    let store = DebugStateStore::new();
    for index in 0..12 {
        store.failure(DebugFailureSummary {
            at: format!("2026-08-01T00:00:{index:02}.000Z"),
            source: DebugFailureSource::Runtime,
            code: format!("E{index}"),
            summary: "safe synthetic failure".to_owned(),
        });
    }

    let snapshot = store.snapshot();
    assert_eq!(snapshot.recent_failures.len(), 10);
    assert_eq!(snapshot.recent_failures[0].code, "E2");

    let redacted = redact_sensitive(&json!({
        "apiKey": "sk-testvalue0000",
        "nested": {
            "authorization": "Bearer abcdefghijklmnop",
            "raw": "synthetic private content",
        },
    }));
    assert_eq!(
        redacted,
        json!({
            "apiKey": "[REDACTED]",
            "nested": {
                "authorization": "[REDACTED]",
                "raw": "[REDACTED]",
            },
        })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn local_debug_server_only_permits_read_only_get_routes() {
    let store = DebugStateStore::new();
    store.update(DebugStateUpdate {
        current_body_tool: Some(Some(DebugBodyTool {
            id: "body-1".to_owned(),
            tool: "move_input".to_owned(),
            purpose: "synthetic agent tool".to_owned(),
            started_at: "1970-01-01T00:00:00.000Z".to_owned(),
        })),
        ..DebugStateUpdate::default()
    });
    let server = LocalDebugServer::new(store, 0).unwrap();
    let address = server.start().await.unwrap();

    let state = http_request(&address, "GET", "/v1/state", "").await;
    assert_eq!(state.status, 200);
    assert_eq!(state.json()["currentBodyTool"]["tool"], "move_input");

    let rejected = http_request(&address, "POST", "/v1/state", "{}").await;
    assert_eq!(rejected.status, 405);
    assert_eq!(rejected.json(), json!({"error": "read_only"}));

    server.stop().await.unwrap();
}

#[test]
fn debug_state_defaults_revision_and_input_snapshot_ownership_are_isolated() {
    let store = DebugStateStore::new();
    let initial = store.snapshot();
    assert_eq!(initial.protocol, DebugStateProtocol::V1);
    assert_eq!(initial.revision, 0);
    assert_eq!(initial.connection, BackendState::Idle);
    assert!(initial.recent_failures.is_empty());
    let decision = initial.decision.as_ref().expect("default decision");
    assert_eq!(decision.status, DebugDecisionStatus::Idle);
    assert!(decision.context_sources.is_empty());
    assert!(decision.retrieved_memory_ids.is_empty());
    assert!(looks_like_utc_millis(&initial.captured_at));

    let mut body = body_state();
    let mut patch = DebugStateUpdate {
        body: Some(Some(body.clone())),
        current_body_tool: Some(Some(body_tool())),
        ..DebugStateUpdate::default()
    };
    store.update(patch.clone());

    body.health = 1.0;
    patch
        .body
        .as_mut()
        .and_then(Option::as_mut)
        .expect("body patch")
        .position
        .x = 999.0;
    let stored = store.snapshot();
    assert_eq!(stored.revision, 1);
    assert_eq!(stored.body.as_ref().unwrap().health, 20.0);
    assert_eq!(stored.body.as_ref().unwrap().position.x, 1.0);

    let mut caller_owned_snapshot = (*stored).clone();
    caller_owned_snapshot.body.as_mut().unwrap().health = 2.0;
    assert_eq!(store.snapshot().body.as_ref().unwrap().health, 20.0);

    store.update(DebugStateUpdate {
        connection: Some(BackendState::Idle),
        ..DebugStateUpdate::default()
    });
    let shallow = store.snapshot();
    assert_eq!(shallow.revision, 2);
    assert_eq!(shallow.body.as_ref().unwrap().health, 20.0);
    assert_eq!(
        shallow.current_body_tool.as_ref().unwrap().tool,
        "move_input"
    );
}

#[test]
fn debug_state_optional_patch_fields_can_be_set_and_cleared() {
    let store = DebugStateStore::new();
    store.update(DebugStateUpdate {
        body: Some(Some(body_state())),
        current_body_tool: Some(Some(body_tool())),
        observations: Some(Some(PassiveObservations::default())),
        decision: Some(Some(DebugDecision::idle())),
        ..DebugStateUpdate::default()
    });

    let set = store.snapshot();
    assert_eq!(set.revision, 1);
    assert!(set.body.is_some());
    assert!(set.current_body_tool.is_some());
    assert!(set.observations.is_some());
    assert!(set.decision.is_some());

    store.update(DebugStateUpdate {
        body: Some(None),
        current_body_tool: Some(None),
        observations: Some(None),
        decision: Some(None),
        ..DebugStateUpdate::default()
    });

    let cleared = store.snapshot();
    assert_eq!(cleared.revision, 2);
    assert!(cleared.body.is_none());
    assert!(cleared.current_body_tool.is_none());
    assert!(cleared.observations.is_none());
    assert!(cleared.decision.is_none());
}

#[test]
fn debug_state_input_update_clears_optional_none_fields() {
    let store = DebugStateStore::new();
    store.update(DebugStateUpdate {
        body: Some(Some(body_state())),
        current_body_tool: Some(Some(body_tool())),
        observations: Some(Some(PassiveObservations::default())),
        decision: Some(Some(DebugDecision::idle())),
        ..DebugStateUpdate::default()
    });

    store.update(DebugStateInput {
        connection: BackendState::Idle,
        body: None,
        current_body_tool: None,
        recent_failures: Vec::new(),
        observations: None,
        decision: None,
    });

    let cleared = store.snapshot();
    assert_eq!(cleared.revision, 2);
    assert!(cleared.body.is_none());
    assert!(cleared.current_body_tool.is_none());
    assert!(cleared.observations.is_none());
    assert!(cleared.decision.is_none());
}

#[test]
fn debug_state_update_serde_distinguishes_absent_and_null_optional_fields() {
    let absent: DebugStateUpdate = serde_json::from_value(json!({})).unwrap();
    assert_eq!(absent.body, None);
    assert_eq!(absent.current_body_tool, None);
    assert_eq!(absent.observations, None);
    assert_eq!(absent.decision, None);

    let clear: DebugStateUpdate = serde_json::from_value(json!({
        "body": null,
        "currentBodyTool": null,
        "observations": null,
        "decision": null,
    }))
    .unwrap();
    assert_eq!(clear.body, Some(None));
    assert_eq!(clear.current_body_tool, Some(None));
    assert_eq!(clear.observations, Some(None));
    assert_eq!(clear.decision, Some(None));

    let encoded = serde_json::to_value(clear).unwrap();
    assert_eq!(encoded["body"], Value::Null);
    assert_eq!(encoded["currentBodyTool"], Value::Null);
    assert_eq!(encoded["observations"], Value::Null);
    assert_eq!(encoded["decision"], Value::Null);
}

#[test]
fn redaction_recurses_through_arrays_unicode_keys_and_string_values_without_mutation() {
    let input = json!({
        "items": [
            {"API_KEY": "synthetic api value"},
            {"profilesFolder": "synthetic profile path"},
            {"secretToken": "synthetic token value"},
            {"nested": [{"RAW": "synthetic raw text"}]},
            {"safe": "prefix Bearer abcdefghijklmnop suffix sk-qrstuvwxyz1234"},
            {"unicode": "prefix Bearer abcdefghijKl suffix sk-abcdefghijkſ"},
            {"ſecret": "synthetic long-s key"},
            {"apiKey": "synthetic kelvin-key value"},
        ],
        "short": "Bearer brief",
        "nel": "Bearer\u{0085}abcdefghijkl",
        "bom": "Bearer\u{feff}abcdefghijkl",
    });
    let original = input.clone();
    let redacted = redact_sensitive(&input);

    assert_eq!(input, original);
    assert_eq!(redacted["items"][0]["API_KEY"], "[REDACTED]");
    assert_eq!(redacted["items"][1]["profilesFolder"], "[REDACTED]");
    assert_eq!(redacted["items"][2]["secretToken"], "[REDACTED]");
    assert_eq!(redacted["items"][3]["nested"][0]["RAW"], "[REDACTED]");
    assert_eq!(
        redacted["items"][4]["safe"],
        "prefix [REDACTED] suffix [REDACTED]"
    );
    assert_eq!(
        redacted["items"][5]["unicode"],
        "prefix [REDACTED] suffix [REDACTED]"
    );
    assert_eq!(redacted["items"][6]["ſecret"], "[REDACTED]");
    assert_eq!(redacted["items"][7]["apiKey"], "[REDACTED]");
    assert_eq!(redacted["short"], "Bearer brief");
    assert_eq!(redacted["nel"], "Bearer\u{0085}abcdefghijkl");
    assert_eq!(redacted["bom"], "[REDACTED]");
}

#[tokio::test(flavor = "current_thread")]
async fn debug_server_headers_health_404_and_every_non_get_are_exact() {
    let server = LocalDebugServer::new(DebugStateStore::new(), 0).unwrap();
    let address = server.start().await.unwrap();

    let health = http_request(&address, "GET", "/health", "").await;
    assert_json_response(&health, 200, json!({"status": "ok"}));

    let missing = http_request(&address, "GET", "/missing", "").await;
    assert_json_response(&missing, 404, json!({"error": "not_found"}));

    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let rejected = http_request(&address, method, "/anywhere", "{}").await;
        assert_eq!(rejected.status, 405, "{method}");
        assert_eq!(rejected.json(), json!({"error": "read_only"}));
        assert_eq!(rejected.headers.get("allow"), Some(&"GET".to_owned()));
        assert_common_headers(&rejected);
    }

    assert_common_headers(&health);
    assert_common_headers(&missing);
    server.stop().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn debug_server_start_stop_is_idempotent_and_address_errors_are_structured() {
    let store = DebugStateStore::new();
    let invalid = match LocalDebugServer::new(store.clone(), 65_536) {
        Err(error) => error,
        Ok(_) => panic!("65536 must be rejected before bind"),
    };
    assert_eq!(invalid.code(), "invalid_port");

    let server = LocalDebugServer::new(store, 0).unwrap();
    assert_eq!(server.address().unwrap_err().code(), "not_listening");

    let first = server.start().await.unwrap();
    let second = server.start().await.unwrap();
    assert_eq!(first, second);
    assert_eq!(server.address().unwrap(), first);

    server.stop().await.unwrap();
    server.stop().await.unwrap();
    assert_eq!(server.address().unwrap_err().code(), "not_listening");

    let restarted = server.start().await.unwrap();
    assert_eq!(restarted.host, "127.0.0.1");
    server.stop().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_snapshot_reads_and_http_reads_do_not_panic() {
    let store = DebugStateStore::new();
    store.update(DebugStateUpdate {
        body: Some(Some(body_state())),
        ..DebugStateUpdate::default()
    });
    let server = LocalDebugServer::new(store.clone(), 0).unwrap();
    let address = server.start().await.unwrap();

    let mut tasks = Vec::new();
    for _ in 0..16 {
        let reader = store.clone();
        let address = address.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..16 {
                let snapshot = reader.snapshot();
                assert_eq!(snapshot.protocol, DebugStateProtocol::V1);
                let response = http_request(&address, "GET", "/v1/state", "").await;
                assert_eq!(response.status, 200);
                assert_eq!(response.json()["protocol"], "mineintent.debug-state.v1");
            }
        }));
    }

    for index in 0..32 {
        store.update(DebugStateUpdate {
            decision: Some(Some(DebugDecision {
                status: if index % 2 == 0 {
                    DebugDecisionStatus::Running
                } else {
                    DebugDecisionStatus::Idle
                },
                run_id: Some(format!("run-{index}")),
                model: None,
                started_at: None,
                context_sources: Vec::new(),
                retrieved_memory_ids: Vec::new(),
            })),
            ..DebugStateUpdate::default()
        });
    }

    for task in tasks {
        task.await.unwrap();
    }
    server.stop().await.unwrap();
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("HTTP body should be JSON")
    }
}

async fn http_request(
    address: &mineintent_middle::telemetry::DebugServerAddress,
    method: &str,
    path: &str,
    body: &str,
) -> HttpResponse {
    let mut stream = TcpStream::connect((address.host, address.port))
        .await
        .expect("debug server should accept loopback connections");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        address.host,
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("request should be written");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("response should be readable");
    parse_response(&raw)
}

fn parse_response(raw: &[u8]) -> HttpResponse {
    let separator = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP response should contain headers");
    let header_text = String::from_utf8_lossy(&raw[..separator]);
    let mut lines = header_text.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
        .expect("HTTP response should contain a status");
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    HttpResponse {
        status,
        headers,
        body: raw[separator + 4..].to_vec(),
    }
}

fn assert_json_response(response: &HttpResponse, status: u16, body: Value) {
    assert_eq!(response.status, status);
    assert_eq!(response.json(), body);
    assert_common_headers(response);
}

fn assert_common_headers(response: &HttpResponse) {
    assert_eq!(
        response.headers.get("content-type"),
        Some(&"application/json; charset=utf-8".to_owned())
    );
    assert_eq!(
        response.headers.get("cache-control"),
        Some(&"no-store".to_owned())
    );
}

fn body_state() -> DebugBodyState {
    DebugBodyState {
        position: Vec3Value {
            x: 1.0,
            y: 64.0,
            z: -2.0,
        },
        health: 20.0,
        food: 20.0,
        inventory: vec![DebugInventoryItem {
            item_name: "synthetic_item".to_owned(),
            count: 3,
        }],
    }
}

fn body_tool() -> DebugBodyTool {
    DebugBodyTool {
        id: "body-1".to_owned(),
        tool: "move_input".to_owned(),
        purpose: "synthetic movement".to_owned(),
        started_at: "2026-08-01T00:00:00.000Z".to_owned(),
    }
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
