//! 真实二进制的入口隔离：在绑定本机端口时结束，不连接游戏或模型。

use std::net::TcpListener;
use std::process::Command;

#[test]
fn mcp_checks_its_listener_without_loading_model_configuration() {
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_companion"))
        .env_clear()
        .env("MINEINTENT_ENTRY", "mcp")
        .env(
            "MINEINTENT_BODY_ADDR",
            occupied.local_addr().unwrap().to_string(),
        )
        .env("MODEL_PROTOCOL", "this-protocol-does-not-exist")
        .env("MINEINTENT_MODEL_API_KEY_FILE", "/nonexistent/mcp-test-key")
        .env("MINEINTENT_PERSONA_FILE", "/nonexistent/mcp-test-persona")
        .env("MINEINTENT_MEMORY_FILE", "/nonexistent/mcp-test-memory")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("外接入口监听"), "{error}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("[组合根] 连接"), "{stdout}");
}

#[test]
fn invalid_entry_stops_before_world_startup() {
    let output = Command::new(env!("CARGO_BIN_EXE_companion"))
        .env_clear()
        .env("MINEINTENT_ENTRY", "unknown")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("MINEINTENT_ENTRY"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("[组合根] 连接"));
}
