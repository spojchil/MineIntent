//! 常驻身体的本机接入。连接与调用任务均由监听器拥有，关闭时回收。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use agent::{PortFuture, ToolCall, ToolCallId, ToolDefinition, ToolResult};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};

use crate::protocol::{encode, Message, Request};
use crate::CancellationToken;

const MAX_IN_FLIGHT: usize = 64;

/// 同一具身体由不同接入先后使用。取消调用会丢弃其 future，实现须取消安全。
/// 连接关闭只结束接入，不代表取消已经提交到游戏的动作。
pub trait Body: Send + Sync + 'static {
    fn tools(&self) -> Vec<ToolDefinition>;

    fn call(&self, call: ToolCall) -> PortFuture<'_, ToolResult>;

    fn attached(&self);

    /// 正常关闭时在回收全部调用后触发；任务异常退出时也会通知接入释放。
    fn detached(&self) {}

    /// 等到「有未取走的事件、且此刻没有调用在途」，返回待取条数。
    ///
    /// future 可能在任意点被丢弃（每轮循环重建），实现须取消安全。默认永不敲门。
    fn next_nudge(&self) -> PortFuture<'_, u64> {
        Box::pin(std::future::pending())
    }
}

struct Attachment {
    body: Arc<dyn Body>,
    occupied: Arc<AtomicBool>,
}

impl Drop for Attachment {
    fn drop(&mut self) {
        self.body.detached();
        self.occupied.store(false, Ordering::SeqCst);
    }
}

/// 只监听回环地址。停机或监听错误时，先结束全部接入再返回。
pub async fn serve(
    listener: TcpListener,
    body: Arc<dyn Body>,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    if !listener.local_addr()?.ip().is_loopback() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "身体的私有接入只允许本机回环地址",
        ));
    }
    let connections_stop = shutdown.child_token();
    let _stop_on_drop = connections_stop.clone().drop_guard();
    let occupied = Arc::new(AtomicBool::new(false));
    let mut connections = JoinSet::new();
    let outcome = loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break Ok(()),
            joined = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(error)) = joined {
                    eprintln!("[接入] 连接任务异常退出：{error}");
                }
            }
            accepted = listener.accept() => {
                let (mut stream, _peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => break Err(error),
                };
                if occupied.swap(true, Ordering::SeqCst) {
                    // 拒绝也受停机控制；不给不读取的客户端无限等待。
                    let stop = connections_stop.child_token();
                    connections.spawn(async move {
                        let message = encode(&Message::Occupied {
                            reason: "这具身体已经有一个接入在用；同一时刻只认一个".to_owned(),
                        });
                        tokio::select! {
                            _ = stop.cancelled() => {}
                            _ = tokio::time::timeout(
                                Duration::from_secs(1),
                                stream.write_all(&message),
                            ) => {}
                        }
                    });
                    continue;
                }
                let attachment = Attachment {
                    body: body.clone(),
                    occupied: occupied.clone(),
                };
                let stop = connections_stop.child_token();
                connections.spawn(async move {
                    attachment.body.attached();
                    connection(stream, attachment.body.clone(), stop).await;
                    drop(attachment);
                });
            }
        }
    };
    connections_stop.cancel();
    while let Some(joined) = connections.join_next().await {
        if let Err(error) = joined {
            eprintln!("[接入] 回收连接任务失败：{error}");
        }
    }
    outcome
}

async fn send(
    outgoing: &mpsc::Sender<Message>,
    stop: &CancellationToken,
    message: Message,
) -> bool {
    tokio::select! {
        biased;
        _ = stop.cancelled() => false,
        sent = outgoing.send(message) => sent.is_ok(),
    }
}

async fn connection(stream: TcpStream, body: Arc<dyn Body>, stop: CancellationToken) {
    let _stop_on_drop = stop.clone().drop_guard();
    let (read, mut write) = stream.into_split();
    let (outgoing, mut messages) = mpsc::channel::<Message>(MAX_IN_FLIGHT);
    let writer_stop = stop.clone();
    let writer = tokio::spawn(async move {
        loop {
            let message = tokio::select! {
                biased;
                _ = writer_stop.cancelled() => break,
                message = messages.recv() => match message {
                    Some(message) => message,
                    None => break,
                },
            };
            let bytes = encode(&message);
            let sent = tokio::select! {
                biased;
                _ = writer_stop.cancelled() => break,
                sent = write.write_all(&bytes) => sent,
            };
            if sent.is_err() {
                break;
            }
        }
        writer_stop.cancel();
    });

    let mut lines = BufReader::new(read).lines();
    let mut calls = JoinSet::new();
    let mut active = HashMap::<u64, AbortHandle>::new();
    if send(&outgoing, &stop, Message::Ready).await {
        loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                joined = calls.join_next(), if !calls.is_empty() => {
                    let message = match joined {
                        Some(Ok((id, result))) => {
                            // Cancel 可以在调用完成而回执尚未转发时到达。
                            if active.remove(&id).is_none() {
                                continue;
                            }
                            Message::Result { id, result }
                        }
                        Some(Err(error)) => {
                            let id = active.iter()
                                .find(|(_, handle)| handle.id() == error.id())
                                .map(|(id, _)| *id);
                            let Some(id) = id else { continue };
                            active.remove(&id);
                            if error.is_cancelled() {
                                continue;
                            }
                            Message::Failed {
                                id,
                                reason: "工具调用异常中止；已提交游戏动作的执行结果无法确认".to_owned(),
                            }
                        }
                        None => continue,
                    };
                    if !send(&outgoing, &stop, message).await {
                        break;
                    }
                }
                pending = body.next_nudge() => {
                    if !send(&outgoing, &stop, Message::Nudge { pending }).await {
                        break;
                    }
                }
                line = lines.next_line() => {
                    let line = match line {
                        Ok(Some(line)) => line,
                        Ok(None) | Err(_) => break,
                    };
                    let request = match serde_json::from_str::<Request>(&line) {
                        Ok(request) => request,
                        Err(error) => {
                            eprintln!("[接入] 私有协议消息无效，结束接入：{error}");
                            break;
                        }
                    };
                    match request {
                        Request::Cancel { id } => {
                            if let Some(handle) = active.remove(&id) {
                                handle.abort();
                            }
                        }
                        Request::Tools { id } => {
                            if !send(&outgoing, &stop, Message::Tools {
                                id,
                                tools: body.tools(),
                            }).await {
                                break;
                            }
                        }
                        Request::Call { id, name, arguments } => {
                            if active.contains_key(&id) {
                                // 不执行含歧义的重复请求，也不猜测应取消哪一个。
                                break;
                            }
                            if !body.tools().iter().any(|tool| tool.name.as_str() == name) {
                                if !send(&outgoing, &stop, Message::UnknownTool { id, name }).await {
                                    break;
                                }
                                continue;
                            }
                            if calls.len() >= MAX_IN_FLIGHT {
                                let result = ToolResult::failure(
                                    ToolCallId::new(format!("bridge-{id}")),
                                    "接入的在途调用过多，请等待已有调用结束",
                                );
                                if !send(&outgoing, &stop, Message::Result { id, result }).await {
                                    break;
                                }
                                continue;
                            }
                            let body = body.clone();
                            let handle = calls.spawn(async move {
                                let call = ToolCall::new(format!("bridge-{id}"), name, arguments);
                                (id, body.call(call).await)
                            });
                            active.insert(id, handle);
                        }
                    }
                }
            }
        }
    }
    // 先使写端停止；再中止并回收工具 future，最后才允许下一接入。
    stop.cancel();
    calls.shutdown().await;
    drop(outgoing);
    let _ = writer.await;
}
