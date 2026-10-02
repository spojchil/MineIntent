//! 官方 rmcp 客户端 + 真实本机 TCP；只替换身体，不依赖 Minecraft 或模型。

use std::future::Future;
use std::io::Cursor;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use agent::{ContentPart, ImageSource, PortFuture, ToolCall, ToolDefinition, ToolResult};
use base64::Engine;
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use rmcp::model::{
    CallToolRequest, CallToolRequestParams, ClientCapabilities, ClientConfig, ClientRequest,
    ErrorCode, Implementation, ProtocolVersion,
};
use rmcp::service::{NotificationContext, PeerRequestOptions, RunningService, ServiceError};
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::body::{serve, Body};
use crate::protocol::parse_addr;
use crate::shim::{run, Shim};
use crate::CancellationToken;

async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(12), future)
        .await
        .expect("测试中的异步操作未在十二秒内结束")
}

struct FakeBody {
    attached: AtomicUsize,
    detached: AtomicUsize,
    active: AtomicUsize,
    effects: AtomicUsize,
    started: Notify,
    ended: Notify,
    released: Notify,
    produced: Notify,
    knock: Notify,
    png: String,
}

impl FakeBody {
    fn new() -> Arc<Self> {
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 3, Rgba([17, 33, 99, 255])))
            .write_to(&mut bytes, ImageFormat::Png)
            .unwrap();
        Arc::new(Self {
            attached: AtomicUsize::new(0),
            detached: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            effects: AtomicUsize::new(0),
            started: Notify::new(),
            ended: Notify::new(),
            released: Notify::new(),
            produced: Notify::new(),
            knock: Notify::new(),
            png: base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()),
        })
    }
}

struct ActiveCall<'a>(&'a FakeBody);

impl Drop for ActiveCall<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        self.0.ended.notify_one();
    }
}

impl Body for FakeBody {
    fn tools(&self) -> Vec<ToolDefinition> {
        ["echo", "slow", "fail", "large"]
            .into_iter()
            .map(|name| {
                let mut tool = ToolDefinition::new(name, json!({"type": "object"}));
                tool.description = Some(format!("测试工具 {name}"));
                tool
            })
            .collect()
    }

    fn call(&self, call: ToolCall) -> PortFuture<'_, ToolResult> {
        Box::pin(async move {
            match call.name.as_str() {
                "echo" => ToolResult::success(
                    call.id,
                    vec![
                        ContentPart::text(call.arguments.to_string()),
                        ContentPart::Image {
                            source: ImageSource::Base64 {
                                media_type: "image/png".to_owned(),
                                data: self.png.clone(),
                            },
                        },
                    ],
                ),
                "slow" => {
                    self.active.fetch_add(1, Ordering::SeqCst);
                    let _active = ActiveCall(self);
                    // 模拟动作已经提交、回执仍在等待；断线后重放会重复该效果。
                    self.effects.fetch_add(1, Ordering::SeqCst);
                    self.started.notify_one();
                    std::future::pending::<()>().await;
                    ToolResult::success(call.id, vec![])
                }
                "large" => {
                    self.produced.notify_one();
                    ToolResult::success(call.id, vec![ContentPart::text("x".repeat(1 << 20))])
                }
                _ => ToolResult::failure(call.id, "工具执行失败"),
            }
        })
    }

    fn attached(&self) {
        self.attached.fetch_add(1, Ordering::SeqCst);
    }

    fn next_nudge(&self) -> PortFuture<'_, u64> {
        Box::pin(async move {
            self.knock.notified().await;
            3
        })
    }

    fn detached(&self) {
        assert_eq!(self.active.load(Ordering::SeqCst), 0);
        self.detached.fetch_add(1, Ordering::SeqCst);
        self.released.notify_one();
    }
}

struct BodyServer {
    body: Arc<FakeBody>,
    addr: SocketAddr,
    stop: CancellationToken,
    task: JoinHandle<std::io::Result<()>>,
}

impl BodyServer {
    async fn start() -> Self {
        Self::at(
            TcpListener::bind("127.0.0.1:0").await.unwrap(),
            FakeBody::new(),
        )
        .await
    }

    async fn at(listener: TcpListener, body: Arc<FakeBody>) -> Self {
        let addr = listener.local_addr().unwrap();
        let stop = CancellationToken::new();
        let task = tokio::spawn(serve(listener, body.clone(), stop.clone()));
        Self {
            body,
            addr,
            stop,
            task,
        }
    }

    async fn shutdown(self) {
        self.stop.cancel();
        within(self.task).await.unwrap().unwrap();
    }
}

struct TestClient {
    changed: Arc<Notify>,
}

impl ClientHandler for TestClient {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("test", "1"),
        )
        .with_protocol_version(ProtocolVersion::V_2025_11_25)
    }

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.changed.notify_one();
    }
}

struct Client {
    service: RunningService<RoleClient, TestClient>,
    changed: Arc<Notify>,
    shim: Arc<Shim>,
    task: JoinHandle<Result<(), String>>,
}

impl Client {
    async fn connect(addr: SocketAddr) -> Self {
        let (client_io, server_io) = tokio::io::duplex(1 << 16);
        let (input, output) = tokio::io::split(server_io);
        let shim = Shim::new(addr);
        let task = tokio::spawn(run(input, output, shim.clone()));
        let changed = Arc::new(Notify::new());
        let service = within(
            TestClient {
                changed: changed.clone(),
            }
            .serve(client_io),
        )
        .await
        .unwrap();
        Self {
            service,
            changed,
            shim,
            task,
        }
    }

    async fn close(mut self) {
        within(self.service.close()).await.unwrap();
        within(self.task).await.unwrap().unwrap();
    }
}

fn params(name: &'static str) -> CallToolRequestParams {
    CallToolRequestParams::new(name)
}

#[test]
fn private_address_requires_numeric_loopback() {
    assert!(parse_addr("127.0.0.1:25580").is_ok());
    assert!(parse_addr("[::1]:25580").is_ok());
    for invalid in [
        "0.0.0.0:25580",
        "[::]:25580",
        "192.168.1.2:25580",
        "localhost:25580",
    ] {
        assert!(parse_addr(invalid).is_err(), "{invalid}");
    }
}

#[tokio::test]
async fn official_client_discovers_tools_and_decodes_a_real_png() {
    let server = BodyServer::start().await;
    let client = Client::connect(server.addr).await;
    let info = client.service.peer_info().unwrap();
    assert_eq!(info.protocol_version, ProtocolVersion::V_2025_11_25);
    assert!(info
        .capabilities
        .experimental
        .as_ref()
        .is_some_and(|experimental| experimental.contains_key("claude/channel")));
    let list = within(client.service.list_tools(None)).await.unwrap();
    assert_eq!(list.tools[0].name, "echo");
    assert_eq!(list.tools[0].description.as_deref(), Some("测试工具 echo"));
    assert_eq!(
        list.tools[0].input_schema.get("type"),
        Some(&json!("object"))
    );
    let reply =
        within(client.service.call_tool(
            params("echo").with_arguments(json!({"x": 1}).as_object().unwrap().clone()),
        ))
        .await
        .unwrap();
    assert_eq!(reply.is_error, Some(false));
    assert_eq!(reply.content[0].as_text().unwrap().text, "{\"x\":1}");
    let image = reply.content[1].as_image().unwrap();
    assert_eq!(image.mime_type, "image/png");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&image.data)
        .unwrap();
    let decoded = image::load_from_memory_with_format(&bytes, ImageFormat::Png)
        .unwrap()
        .to_rgba8();
    assert_eq!(decoded.dimensions(), (2, 3));
    assert_eq!(*decoded.get_pixel(1, 2), Rgba([17, 33, 99, 255]));

    let failed = within(client.service.call_tool(params("fail")))
        .await
        .unwrap();
    assert_eq!(failed.is_error, Some(true));
    let unknown = within(client.service.call_tool(params("missing")))
        .await
        .unwrap_err();
    assert!(
        matches!(unknown, ServiceError::McpError(error) if error.code == ErrorCode::INVALID_PARAMS)
    );
    client.close().await;
    within(server.body.released.notified()).await;
    server.shutdown().await;
}

#[tokio::test]
async fn cancellation_reaches_body_without_blocking_other_calls() {
    let server = BodyServer::start().await;
    let client = Client::connect(server.addr).await;
    let slow = within(client.service.send_cancellable_request(
        ClientRequest::CallToolRequest(CallToolRequest::new(params("slow"))),
        PeerRequestOptions::default(),
    ))
    .await
    .unwrap();
    within(server.body.started.notified()).await;
    let echo = within(client.service.call_tool(params("echo")))
        .await
        .unwrap();
    assert_eq!(echo.is_error, Some(false));
    assert_eq!(server.body.active.load(Ordering::SeqCst), 1);
    within(slow.cancel(Some("测试取消".to_owned())))
        .await
        .unwrap();
    within(server.body.ended.notified()).await;
    assert_eq!(server.body.active.load(Ordering::SeqCst), 0);
    assert_eq!(
        within(client.service.call_tool(params("echo")))
            .await
            .unwrap()
            .is_error,
        Some(false)
    );
    client.close().await;
    within(server.body.released.notified()).await;
    let next = Client::connect(server.addr).await;
    within(next.service.list_tools(None)).await.unwrap();
    assert_eq!(server.body.attached.load(Ordering::SeqCst), 2);
    next.close().await;
    server.shutdown().await;
}

#[tokio::test]
async fn a_late_body_announces_tools_without_another_client_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let client = Client::connect(addr).await;
    assert!(within(client.service.list_tools(None)).await.is_err());
    let failed = within(client.service.call_tool(params("echo")))
        .await
        .unwrap();
    assert_eq!(failed.is_error, Some(true));
    assert!(failed.content[0]
        .as_text()
        .unwrap()
        .text
        .contains("连不上身体"));
    let server = BodyServer::at(TcpListener::bind(addr).await.unwrap(), FakeBody::new()).await;
    within(client.changed.notified()).await;
    assert_eq!(
        within(client.service.list_tools(None))
            .await
            .unwrap()
            .tools
            .len(),
        4
    );
    client.close().await;
    server.shutdown().await;
}

#[tokio::test]
async fn occupied_body_can_be_taken_over_after_the_first_client_closes() {
    let server = BodyServer::start().await;
    let first = Client::connect(server.addr).await;
    within(first.service.list_tools(None)).await.unwrap();
    let second = Client::connect(server.addr).await;
    assert!(within(second.service.list_tools(None)).await.is_err());
    let refused = within(second.service.call_tool(params("echo")))
        .await
        .unwrap();
    assert_eq!(refused.is_error, Some(true));
    assert!(refused.content[0]
        .as_text()
        .unwrap()
        .text
        .contains("只认一个"));
    first.close().await;
    within(server.body.released.notified()).await;
    within(second.changed.notified()).await;
    within(second.service.list_tools(None)).await.unwrap();
    assert_eq!(server.body.attached.load(Ordering::SeqCst), 2);
    second.close().await;
    server.shutdown().await;
}

#[tokio::test]
async fn disconnect_does_not_replay_an_already_submitted_effect() {
    let server = BodyServer::start().await;
    let body = server.body.clone();
    let addr = server.addr;
    let client = Client::connect(addr).await;
    let peer = client.service.peer().clone();
    let call = tokio::spawn(async move { peer.call_tool(params("slow")).await });
    within(body.started.notified()).await;
    server.shutdown().await;
    let result = within(call).await.unwrap().unwrap();
    assert_eq!(result.is_error, Some(true));
    assert!(result.content[0]
        .as_text()
        .unwrap()
        .text
        .contains("不会自动重发"));
    assert_eq!(body.active.load(Ordering::SeqCst), 0);
    let restarted = BodyServer::at(TcpListener::bind(addr).await.unwrap(), body.clone()).await;
    within(client.service.list_tools(None)).await.unwrap();
    within(client.service.call_tool(params("echo")))
        .await
        .unwrap();
    assert_eq!(body.effects.load(Ordering::SeqCst), 1);
    client.close().await;
    restarted.shutdown().await;
}

#[tokio::test]
async fn aborting_run_releases_tcp_even_when_the_shim_arc_is_retained() {
    let server = BodyServer::start().await;
    let mut client = Client::connect(server.addr).await;
    within(client.service.list_tools(None)).await.unwrap();
    client.task.abort();
    assert!(within(&mut client.task).await.unwrap_err().is_cancelled());
    within(server.body.released.notified()).await;
    // 外部仍持有 Shim；清理不能依赖它的 Arc 计数变成零。
    assert!(Arc::strong_count(&client.shim) >= 1);
    let next = Client::connect(server.addr).await;
    within(next.service.list_tools(None)).await.unwrap();
    within(client.service.close()).await.unwrap();
    next.close().await;
    server.shutdown().await;
}

struct RawClient {
    input: DuplexStream,
    output: Lines<BufReader<DuplexStream>>,
    task: JoinHandle<Result<(), String>>,
}

impl RawClient {
    fn connect(addr: SocketAddr) -> Self {
        let (input, server_input) = tokio::io::duplex(1 << 16);
        let (server_output, output) = tokio::io::duplex(1 << 16);
        let task = tokio::spawn(run(server_input, server_output, Shim::new(addr)));
        Self {
            input,
            output: BufReader::new(output).lines(),
            task,
        }
    }

    async fn send(&mut self, message: Value) {
        let mut bytes = serde_json::to_vec(&message).unwrap();
        bytes.push(b'\n');
        within(self.input.write_all(&bytes)).await.unwrap();
    }

    async fn response(&mut self, id: u64) -> Value {
        loop {
            let line = within(self.output.next_line()).await.unwrap().unwrap();
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["id"] == id {
                return value;
            }
        }
    }

    async fn initialize(&mut self, version: &str) -> Value {
        self.send(json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
            "protocolVersion":version, "capabilities":{}, "clientInfo":{"name":"raw-test", "version":"1"}
        }})).await;
        let result = self.response(1).await;
        self.send(json!({"jsonrpc":"2.0", "method":"notifications/initialized"}))
            .await;
        result
    }
}

#[tokio::test]
async fn unsupported_versions_fall_back_to_the_only_supported_version() {
    let server = BodyServer::start().await;
    for version in ["2025-03-26", "2099-01-01"] {
        let mut client = RawClient::connect(server.addr);
        let reply = client.initialize(version).await;
        assert_eq!(reply["result"]["protocolVersion"], "2025-11-25");
        assert!(reply["result"]["capabilities"]["experimental"]["claude/channel"].is_object());
        drop(client.input);
        within(client.task).await.unwrap().unwrap();
    }
    server.shutdown().await;
}

/// Claude Code 2.1.287 实际的开场：先发 2026-07-28 的 `server/discover`，被拒后退回
/// `initialize`，随后的 `tools/list` 不带 `_meta`。探测不能把会话锁进新规范。
#[tokio::test]
async fn a_rejected_discover_probe_still_allows_the_legacy_session() {
    let server = BodyServer::start().await;
    let mut client = RawClient::connect(server.addr);
    client
        .send(
            json!({"jsonrpc":"2.0", "id":"server-discover-probe-1", "method":"server/discover",
            "params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}),
        )
        .await;
    let line = within(client.output.next_line()).await.unwrap().unwrap();
    let refusal: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(refusal["id"], "server-discover-probe-1");
    assert_eq!(refusal["error"]["code"], -32022);
    assert_eq!(refusal["error"]["data"]["requested"], "2026-07-28");

    let reply = client.initialize("2025-11-25").await;
    assert_eq!(reply["result"]["protocolVersion"], "2025-11-25");
    client
        .send(json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}))
        .await;
    let tools = client.response(2).await;
    assert!(tools.get("error").is_none(), "{tools}");
    assert!(!tools["result"]["tools"].as_array().unwrap().is_empty());
    drop(client.input);
    within(client.task).await.unwrap().unwrap();
    server.shutdown().await;
}

/// 身体敲门 → 客户端收到 Claude Code 的 channel 通知，只说件数不带内容。
#[tokio::test]
async fn a_body_nudge_becomes_a_channel_notification() {
    let server = BodyServer::start().await;
    let mut client = RawClient::connect(server.addr);
    client.initialize("2025-11-25").await;
    client
        .send(json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}))
        .await;
    client.response(2).await;
    // 身体那一侧每轮循环重建敲门 future；notify_one 留 permit，不怕错过。
    server.body.knock.notify_one();
    let notification = loop {
        let line = within(client.output.next_line()).await.unwrap().unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        if value["method"] == "notifications/claude/channel" {
            break value;
        }
    };
    assert_eq!(notification["params"]["meta"]["pending"], "3");
    assert!(notification["params"]["content"]
        .as_str()
        .unwrap()
        .contains("3 件"));
    drop(client.input);
    within(client.task).await.unwrap().unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn stdin_eof_cancels_active_calls_and_allows_a_new_attachment() {
    let server = BodyServer::start().await;
    let mut client = RawClient::connect(server.addr);
    client.initialize("2025-11-25").await;
    client
        .send(json!({"jsonrpc":"2.0", "id":2, "method":"tools/call", "params":{"name":"slow"}}))
        .await;
    within(server.body.started.notified()).await;
    drop(client.input);
    // 仍读得了 stdout，不能靠丢掉输出端或退出整个 runtime 才释放 TCP。
    within(client.task).await.unwrap().unwrap();
    within(server.body.released.notified()).await;
    assert_eq!(server.body.active.load(Ordering::SeqCst), 0);
    let next = Client::connect(server.addr).await;
    within(next.service.list_tools(None)).await.unwrap();
    next.close().await;
    server.shutdown().await;
}

#[tokio::test]
async fn stdin_eof_releases_body_while_stdout_is_backpressured() {
    let server = BodyServer::start().await;
    let mut client = RawClient::connect(server.addr);
    client.initialize("2025-11-25").await;
    client
        .send(json!({"jsonrpc":"2.0", "id":2, "method":"tools/call", "params":{"name":"slow"}}))
        .await;
    within(server.body.started.notified()).await;
    client
        .send(json!({"jsonrpc":"2.0", "id":3, "method":"tools/call", "params":{"name":"large"}}))
        .await;
    within(server.body.produced.notified()).await;
    // 见到回执的首字节后停止读取；1 MiB 回执填不进 64 KiB 的 stdout 管道。
    let mut first_byte = [0];
    within(client.output.get_mut().read_exact(&mut first_byte))
        .await
        .unwrap();
    drop(client.input);
    within(server.body.released.notified()).await;
    assert_eq!(server.body.active.load(Ordering::SeqCst), 0);
    within(client.task).await.unwrap().unwrap();
    let next = Client::connect(server.addr).await;
    within(next.service.list_tools(None)).await.unwrap();
    next.close().await;
    server.shutdown().await;
}
