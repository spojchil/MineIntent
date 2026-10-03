//! 官方 rmcp 承担 MCP 消息、版本协商与请求生命周期；本层只连接常驻身体。
//!
//! 仅承诺已经覆盖的 2025-11-25。恢复连接只恢复工具发现，不重放工具调用。
//!
//! # 唤醒
//!
//! 标准 MCP 没有「让模型开始新一轮」的原语：通用客户端靠每次回执带回期间信息，
//! 以及可提前返回的 `wait`。Claude Code 另有 channel 扩展
//! （`notifications/claude/channel`），会话空闲时也能把事送进去；声明它对别的
//! 客户端无害，所以总是声明。敲门只说有几件，不带内容：内容由下一次回执带回。
//! 客户端没把本服务当 channel 加载时会静默丢弃通知——只敲门不送信，丢了也不丢事。

use std::borrow::Cow;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent::{ContentPart, ImageSource, ToolResult, ToolResultStatus};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, CustomNotification,
    Implementation, ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities,
    ServerConfig, ServerNotification, Tool,
};
use rmcp::service::{Peer, RequestContext};
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, Semaphore};
use tokio::task::JoinHandle;

use crate::protocol::{encode, Message, Request};
use crate::CancellationToken;

const SUPPORTED_VERSIONS: &[ProtocolVersion] = &[ProtocolVersion::V_2025_11_25];
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);
const MAX_IN_FLIGHT: usize = 64;
const INSTRUCTIONS: &str = "这组工具操作 Minecraft 世界里的一名玩家。\
每次调用工具，回执末尾都可能附一段「——期间——」：周围情况的变化，以及上一次调用以来发生的事。\
刚接入时第一次回执会带完整的处境（在哪、对着什么、身上有什么）；想看周围就用 view，\
这是唯一看得见世界的方式。";

/// 敲门通知的方法名（Claude Code channel 扩展）。
const CHANNEL_METHOD: &str = "notifications/claude/channel";

/// MCP 服务的一次 stdio 接入；它不拥有身体进程。
pub struct Shim {
    addr: SocketAddr,
    link: tokio::sync::Mutex<Option<Arc<Link>>>,
    shutdown: CancellationToken,
    tools_listed: AtomicBool,
    refresh_pending: AtomicBool,
    /// 身体的敲门经此交给持有客户端 peer 的监视任务。
    nudges: mpsc::UnboundedSender<u64>,
    nudge_inbox: Mutex<Option<mpsc::UnboundedReceiver<u64>>>,
}

/// IO 任务只持有这份状态，不持有 Link 或 requests sender，避免互相等待的环。
struct LinkState {
    waiting: Mutex<HashMap<u64, oneshot::Sender<Message>>>,
    closed: CancellationToken,
    reason: Mutex<String>,
}

impl LinkState {
    fn fail(&self, reason: &str) {
        let mut current = self.reason.lock().expect("连接锁中毒");
        if !self.closed.is_cancelled() {
            *current = reason.to_owned();
            self.closed.cancel();
            self.waiting.lock().expect("连接锁中毒").clear();
        }
    }

    fn down_reason(&self) -> String {
        self.reason.lock().expect("连接锁中毒").clone()
    }
}

struct Link {
    state: Arc<LinkState>,
    requests: mpsc::UnboundedSender<Request>,
    next_id: AtomicU64,
    slots: Semaphore,
    tasks: tokio::sync::Mutex<Vec<JoinHandle<()>>>,
}

impl Drop for Link {
    fn drop(&mut self) {
        // 显式 close 会 await IO 任务；意外丢弃也不能留下仍占用身体的 TCP。
        self.state.fail("接入已结束");
    }
}

/// future 被 SDK 取消、被关闭路径丢弃或正常返回，都移除其等待记录。
struct PendingRequest {
    id: u64,
    state: Arc<LinkState>,
    requests: mpsc::UnboundedSender<Request>,
    completed: bool,
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        self.state
            .waiting
            .lock()
            .expect("连接锁中毒")
            .remove(&self.id);
        if !self.completed && !self.state.closed.is_cancelled() {
            let _ = self.requests.send(Request::Cancel { id: self.id });
        }
    }
}

impl Link {
    async fn connect(
        addr: SocketAddr,
        nudges: mpsc::UnboundedSender<u64>,
    ) -> Result<Arc<Self>, String> {
        let connect = async {
            let stream = TcpStream::connect(addr).await.map_err(|error| {
                format!("连不上身体（{addr}）：{error}；请先启动 companion 的 mcp 入口")
            })?;
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let greeting = lines
                .next_line()
                .await
                .map_err(|error| format!("读取身体接入结果失败：{error}"))?
                .ok_or_else(|| "身体在确认接入前断开了连接".to_owned())?;
            match serde_json::from_str::<Message>(&greeting) {
                Ok(Message::Ready) => {}
                Ok(Message::Occupied { reason }) => {
                    return Err(format!("身体拒绝了这次接入：{reason}"));
                }
                _ => return Err("身体返回了无法识别的接入结果".to_owned()),
            }
            let state = Arc::new(LinkState {
                waiting: Mutex::new(HashMap::new()),
                closed: CancellationToken::new(),
                reason: Mutex::new(
                    "与身体的连接断开，调用的执行结果无法确认；不会自动重发".to_owned(),
                ),
            });
            let (requests, mut outgoing) = mpsc::unbounded_channel::<Request>();
            let writer_state = state.clone();
            let writer = tokio::spawn(async move {
                loop {
                    let request = tokio::select! {
                        biased;
                        _ = writer_state.closed.cancelled() => break,
                        request = outgoing.recv() => match request {
                            Some(request) => request,
                            None => break,
                        },
                    };
                    let bytes = encode(&request);
                    let sent = tokio::select! {
                        biased;
                        _ = writer_state.closed.cancelled() => break,
                        sent = write.write_all(&bytes) => sent,
                    };
                    if sent.is_err() {
                        break;
                    }
                }
                writer_state.fail("向身体的连接已断开，调用的执行结果无法确认；不会自动重发");
            });
            let reader_state = state.clone();
            let reader = tokio::spawn(async move {
                loop {
                    let line = tokio::select! {
                        biased;
                        _ = reader_state.closed.cancelled() => break,
                        line = lines.next_line() => match line {
                            Ok(Some(line)) => line,
                            Ok(None) | Err(_) => break,
                        },
                    };
                    let message = match serde_json::from_str::<Message>(&line) {
                        Ok(message) => message,
                        Err(_) => break,
                    };
                    let id = match &message {
                        Message::Nudge { pending } => {
                            let _ = nudges.send(*pending);
                            continue;
                        }
                        Message::Tools { id, .. }
                        | Message::Result { id, .. }
                        | Message::UnknownTool { id, .. }
                        | Message::Failed { id, .. } => *id,
                        Message::Ready | Message::Occupied { .. } => break,
                    };
                    if let Some(waiter) =
                        reader_state.waiting.lock().expect("连接锁中毒").remove(&id)
                    {
                        let _ = waiter.send(message);
                    }
                }
                reader_state.fail("与身体的连接断开，调用的执行结果无法确认；不会自动重发");
            });
            Ok(Arc::new(Self {
                state,
                requests,
                next_id: AtomicU64::new(1),
                slots: Semaphore::new(MAX_IN_FLIGHT),
                tasks: tokio::sync::Mutex::new(vec![writer, reader]),
            }))
        };
        tokio::time::timeout(CONNECT_TIMEOUT, connect)
            .await
            .map_err(|_| "等待身体确认接入超时；请确认身体已经就绪".to_owned())?
    }

    fn alive(&self) -> bool {
        !self.state.closed.is_cancelled()
    }

    async fn ask(
        &self,
        cancelled: &CancellationToken,
        request: impl FnOnce(u64) -> Request,
    ) -> Result<Message, String> {
        let _slot = tokio::select! {
            biased;
            _ = cancelled.cancelled() => return Err("调用已取消".to_owned()),
            _ = self.state.closed.cancelled() => return Err(self.state.down_reason()),
            slot = self.slots.acquire() => slot.map_err(|_| self.state.down_reason())?,
        };
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        self.state
            .waiting
            .lock()
            .expect("连接锁中毒")
            .insert(id, sender);
        let mut pending = PendingRequest {
            id,
            state: self.state.clone(),
            requests: self.requests.clone(),
            completed: false,
        };
        // 先登记再检查关闭：与 IO 任务的「先取消再清空」配对，不漏掉等待者。
        if !self.alive() || self.requests.send(request(id)).is_err() {
            return Err(self.state.down_reason());
        }
        let reply = tokio::select! {
            biased;
            _ = cancelled.cancelled() => return Err("调用已取消".to_owned()),
            reply = receiver => reply.map_err(|_| self.state.down_reason())?,
        };
        pending.completed = true;
        Ok(reply)
    }

    async fn close(&self) {
        self.state.fail("接入已结束");
        let tasks = std::mem::take(&mut *self.tasks.lock().await);
        for task in tasks {
            let _ = task.await;
        }
    }
}

impl Shim {
    pub fn new(addr: SocketAddr) -> Arc<Self> {
        let (nudges, nudge_inbox) = mpsc::unbounded_channel();
        Arc::new(Self {
            nudges,
            nudge_inbox: Mutex::new(Some(nudge_inbox)),
            addr,
            link: tokio::sync::Mutex::new(None),
            shutdown: CancellationToken::new(),
            tools_listed: AtomicBool::new(false),
            refresh_pending: AtomicBool::new(false),
        })
    }

    async fn link(&self) -> Result<Arc<Link>, String> {
        let mut slot = self.link.lock().await;
        if self.shutdown.is_cancelled() {
            return Err("MCP 接入已经结束".to_owned());
        }
        if !self.addr.ip().is_loopback() {
            return Err("身体的私有接入只允许本机回环地址".to_owned());
        }
        if let Some(link) = slot.as_ref() {
            if link.alive() {
                return Ok(link.clone());
            }
        }
        if let Some(previous) = slot.take() {
            previous.close().await;
        }
        let link = tokio::select! {
            biased;
            _ = self.shutdown.cancelled() => return Err("MCP 接入已经结束".to_owned()),
            link = Link::connect(self.addr, self.nudges.clone()) => link?,
        };
        *slot = Some(link.clone());
        if self.tools_listed.load(Ordering::SeqCst) {
            self.refresh_pending.store(true, Ordering::SeqCst);
        }
        Ok(link)
    }

    /// 可被 stdio 服务的显式停机路径调用；不会停止身体进程。
    pub async fn close(&self) {
        self.shutdown.cancel();
        if let Some(link) = self.link.lock().await.take() {
            link.close().await;
        }
    }

    async fn watch(self: Arc<Self>, peer: Peer<RoleServer>) {
        let mut interval = tokio::time::interval(RECONNECT_INTERVAL);
        let mut nudges = self.nudge_inbox.lock().expect("敲门锁中毒").take();
        loop {
            tokio::select! {
                biased;
                _ = self.shutdown.cancelled() => break,
                Some(pending) = async {
                    match nudges.as_mut() {
                        Some(nudges) => nudges.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if peer.send_notification(channel_nudge(pending)).await.is_err() {
                        break;
                    }
                    continue;
                }
                _ = interval.tick() => {}
            }
            // 只做连接探测，不重发失败的 Call。
            let _ = self.link().await;
            if self.refresh_pending.swap(false, Ordering::SeqCst) {
                tokio::select! {
                    biased;
                    _ = self.shutdown.cancelled() => break,
                    result = peer.notify_tool_list_changed() => {
                        if result.is_err() {
                            break;
                        }
                    }
                }
            }
        }
        // run future 被丢弃时，也回收持有 TCP 的任务；外部可能仍持有 Shim。
        self.close().await;
    }

    async fn ask(
        &self,
        cancelled: &CancellationToken,
        request: impl FnOnce(u64) -> Request,
    ) -> Result<Message, String> {
        let link = tokio::select! {
            biased;
            _ = cancelled.cancelled() => return Err("调用已取消".to_owned()),
            link = self.link() => link?,
        };
        link.ask(cancelled, request).await
    }
}

impl ServerHandler for Shim {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_experimental_with(
                    [("claude/channel".to_owned(), serde_json::Map::new())].into(),
                )
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        )
        .with_protocol_version(ProtocolVersion::V_2025_11_25)
        .with_server_info(Implementation::new("mineintent", env!("CARGO_PKG_VERSION")))
        .with_instructions(INSTRUCTIONS)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(SUPPORTED_VERSIONS)
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if request.is_some_and(|request| request.cursor.is_some()) {
            return Err(ErrorData::invalid_params("工具表没有后续分页", None));
        }
        let reply = self.ask(&context.ct, |id| Request::Tools { id }).await;
        // 取过之后才算：这次请求自己建立的连接不该再触发一次「工具表已变」。
        self.tools_listed.store(true, Ordering::SeqCst);
        let reply = reply.map_err(|error| ErrorData::internal_error(error, None))?;
        let Message::Tools { tools, .. } = reply else {
            return Err(ErrorData::internal_error("身体未返回工具表", None));
        };
        let tools = tools
            .into_iter()
            .map(|tool| {
                let Value::Object(schema) = tool.input_schema else {
                    return Err(ErrorData::internal_error(
                        "工具的 inputSchema 必须是对象",
                        None,
                    ));
                };
                Ok(Tool::new_with_raw(
                    tool.name.as_str().to_owned(),
                    tool.description.map(Cow::Owned),
                    schema,
                ))
            })
            .collect::<Result<Vec<_>, ErrorData>>()?;
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = request.name.into_owned();
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        let reply = self
            .ask(&context.ct, move |id| Request::Call {
                id,
                name,
                arguments,
            })
            .await;
        match reply {
            Ok(Message::Result { result, .. }) => Ok(tool_result(result).into()),
            Ok(Message::UnknownTool { name, .. }) => Err(ErrorData::invalid_params(
                format!("没有名为 {name} 的工具；请改用工具列表中的名字"),
                None,
            )),
            Ok(Message::Failed { reason, .. }) | Err(reason) => {
                Ok(CallToolResult::error(vec![ContentBlock::text(reason)]).into())
            }
            Ok(_) => Err(ErrorData::internal_error("身体未返回工具结果", None)),
        }
    }
}

/// 内核回执转换；Base64 图片保留为原生图片块。
pub fn tool_result(result: ToolResult) -> CallToolResult {
    let content = result
        .content
        .into_iter()
        .map(|part| match part {
            ContentPart::Text { text } => ContentBlock::text(text),
            ContentPart::Image {
                source: ImageSource::Base64 { media_type, data },
            } => ContentBlock::image(data, media_type),
            ContentPart::Image {
                source: ImageSource::Url { url },
            } => ContentBlock::text(format!("[图片] {url}")),
            ContentPart::Json { value } => ContentBlock::text(value.to_string()),
            other => ContentBlock::text(serde_json::to_string(&other).unwrap_or_default()),
        })
        .collect();
    if result.status == ToolResultStatus::Error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    }
}

/// SDK 管理 stdio 生命周期；返回前显式结束私有 TCP 及连接监视任务。
pub async fn run<R, W>(input: R, mut output: W, shim: Arc<Shim>) -> Result<(), String>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let _stop_on_drop = shim.shutdown.clone().drop_guard();
    let input = answer_discover_probes(input, &mut output).await?;
    let (input, output) = crate::io_guard::guard(input, output, shim.shutdown.clone());
    let service = match shim.clone().serve((input, output)).await {
        Ok(service) => service,
        Err(error) => {
            shim.close().await;
            return Err(format!("MCP 初始化失败：{error}"));
        }
    };
    let service_cancel = service.cancellation_token();
    let watcher = tokio::spawn(shim.clone().watch(service.peer().clone()));
    let finishing = service.waiting();
    tokio::pin!(finishing);
    let result = tokio::select! {
        biased;
        _ = shim.shutdown.cancelled() => {
            // EOF/error 先断身体，不能等 SDK 的 handler drain 后才取消排队动作。
            shim.close().await;
            service_cancel.cancel();
            match tokio::time::timeout(Duration::from_secs(3), &mut finishing).await {
                Ok(result) => result.map(|_| ()).map_err(|error| format!("MCP 服务异常退出：{error}")),
                Err(_) => Err("身体连接已关闭，MCP 服务收尾超时".to_owned()),
            }
        }
        result = &mut finishing => {
            result.map(|_| ()).map_err(|error| format!("MCP 服务异常退出：{error}"))
        }
    };
    shim.close().await;
    let _ = watcher.await;
    result
}

/// 最多替 rmcp 回答几次探测；再多就交给它，不在这里无限读。
const MAX_DISCOVER_PROBES: usize = 8;

/// 在 rmcp 之前回答 `server/discover` 探测，让 rmcp 收到的第一条是 `initialize`。
///
/// rmcp 3.5.0 只要首条消息不是 `initialize`，就把整个会话永久切成「每个请求必须带
/// 2026-07-28 的 `_meta`」（`service/server.rs` 里的 `require_request_metadata`）。
/// Claude Code 2.1.287 起先发声明 2026-07-28 的 `server/discover`，被拒后按规范退回
/// `initialize`；会话却已切不回来，随后不带 `_meta` 的 `tools/list` 全被拒，客户端
/// 一件工具也拿不到。这里按 rmcp 自己会回的同一个错误（-32022）回答探测。
async fn answer_discover_probes<R, W>(
    input: R,
    output: &mut W,
) -> Result<impl AsyncRead + Unpin + Send + 'static, String>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(input);
    // 读到的第一条非探测消息：原样放回 rmcp 输入的最前面。
    let mut first = Vec::new();
    for _ in 0..MAX_DISCOVER_PROBES {
        let mut line = Vec::new();
        let read = reader
            .read_until(b'\n', &mut line)
            .await
            .map_err(|error| format!("读取 MCP 输入失败：{error}"))?;
        if read == 0 {
            break;
        }
        let Some(reply) = discover_refusal(&line) else {
            first = line;
            break;
        };
        let mut bytes = serde_json::to_vec(&reply).expect("JSON 必定可序列化");
        bytes.push(b'\n');
        output
            .write_all(&bytes)
            .await
            .and(output.flush().await)
            .map_err(|error| format!("回答 MCP 探测失败：{error}"))?;
    }
    Ok(std::io::Cursor::new(first).chain(reader))
}

/// 是 `server/discover` 请求就给出拒绝回复，否则 `None`（原样交给 rmcp）。
fn discover_refusal(line: &[u8]) -> Option<Value> {
    let message: Value = serde_json::from_slice(line).ok()?;
    if message.get("method")?.as_str()? != "server/discover" {
        return None;
    }
    let id = message.get("id")?.clone();
    let requested = message
        .pointer("/params/_meta/io.modelcontextprotocol~1protocolVersion")
        .cloned()
        .unwrap_or(Value::Null);
    let supported: Vec<&str> = SUPPORTED_VERSIONS.iter().map(|v| v.as_str()).collect();
    Some(serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32022,
            "message": "Unsupported protocol version",
            "data": { "requested": requested, "supported": supported },
        },
    }))
}

fn channel_nudge(pending: u64) -> ServerNotification {
    ServerNotification::CustomNotification(CustomNotification::new(
        CHANNEL_METHOD,
        Some(serde_json::json!({
            "content": format!("游戏里有 {pending} 件你还没看见的事，调用任意 mineintent 工具就能看到。"),
            "meta": { "pending": pending.to_string() },
        })),
    ))
}
