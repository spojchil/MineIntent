use std::{io, sync::Arc};

use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{oneshot, Mutex},
    task::{JoinError, JoinSet},
};

use super::debug_state::DebugStateStore;

pub const DEBUG_SERVER_HOST: &str = "127.0.0.1";
pub const DEFAULT_DEBUG_PORT: u32 = 3_211;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebugServerAddress {
    pub host: &'static str,
    pub port: u16,
    pub url: String,
}

#[derive(Debug, Error)]
pub enum DebugServerError {
    #[error("debug port {port} must be between 0 and 65535")]
    InvalidPort { port: u32 },
    #[error("debug server is not listening")]
    NotListening,
    #[error("debug server bind failed: {source}")]
    Bind {
        #[source]
        source: io::Error,
    },
    #[error("debug server task failed: {source}")]
    Task {
        #[source]
        source: JoinError,
    },
}

impl DebugServerError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidPort { .. } => "invalid_port",
            Self::NotListening => "not_listening",
            Self::Bind { .. } => "bind_failed",
            Self::Task { .. } => "task_failed",
        }
    }
}

#[derive(Clone)]
pub struct LocalDebugServer {
    inner: Arc<DebugServerInner>,
}

struct DebugServerInner {
    state: DebugStateStore,
    port: u16,
    lifecycle: Mutex<Option<RunningServer>>,
    address: std::sync::RwLock<Option<DebugServerAddress>>,
}

struct RunningServer {
    address: DebugServerAddress,
    shutdown: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl LocalDebugServer {
    /// Construct a server. Binding is intentionally deferred to `start`.
    pub fn new<S>(state: S, port: u32) -> Result<Self, DebugServerError>
    where
        S: Into<DebugStateStore>,
    {
        let port = u16::try_from(port).map_err(|_| DebugServerError::InvalidPort { port })?;
        Ok(Self {
            inner: Arc::new(DebugServerInner {
                state: state.into(),
                port,
                lifecycle: Mutex::new(None),
                address: std::sync::RwLock::new(None),
            }),
        })
    }

    pub fn with_default_port<S>(state: S) -> Result<Self, DebugServerError>
    where
        S: Into<DebugStateStore>,
    {
        Self::new(state, DEFAULT_DEBUG_PORT)
    }

    /// Start once and return the actual bound address. Repeated starts return
    /// the same address and do not create another listener or task.
    pub async fn start(&self) -> Result<DebugServerAddress, DebugServerError> {
        let mut lifecycle = self.inner.lifecycle.lock().await;
        if let Some(running) = lifecycle.as_ref() {
            return Ok(running.address.clone());
        }

        let listener = TcpListener::bind((DEBUG_SERVER_HOST, self.inner.port))
            .await
            .map_err(|source| DebugServerError::Bind { source })?;
        let port = listener
            .local_addr()
            .map_err(|source| DebugServerError::Bind { source })?
            .port();
        let address = DebugServerAddress {
            host: DEBUG_SERVER_HOST,
            port,
            url: format!("http://{DEBUG_SERVER_HOST}:{port}"),
        };
        let (shutdown, shutdown_signal) = oneshot::channel();
        let task = tokio::spawn(serve(listener, self.inner.state.clone(), shutdown_signal));

        *write_recover(&self.inner.address) = Some(address.clone());
        *lifecycle = Some(RunningServer {
            address: address.clone(),
            shutdown,
            task,
        });
        Ok(address)
    }

    /// Return the currently listening address, or a structured not-listening
    /// error before start and after a completed stop.
    pub fn address(&self) -> Result<DebugServerAddress, DebugServerError> {
        read_recover(&self.inner.address)
            .clone()
            .ok_or(DebugServerError::NotListening)
    }

    /// Stop the listener and join its accept/connection task. The lifecycle
    /// mutex remains held until the task is joined, so a concurrent start
    /// cannot bind a replacement listener over a still-closing socket.
    pub async fn stop(&self) -> Result<(), DebugServerError> {
        let mut lifecycle = self.inner.lifecycle.lock().await;
        let Some(running) = lifecycle.take() else {
            *write_recover(&self.inner.address) = None;
            return Ok(());
        };

        let RunningServer {
            address: _,
            shutdown,
            task,
        } = running;
        let _ = shutdown.send(());
        let task_result = task.await;
        *write_recover(&self.inner.address) = None;
        task_result.map_err(|source| DebugServerError::Task { source })
    }
}

impl From<&DebugStateStore> for DebugStateStore {
    fn from(state: &DebugStateStore) -> Self {
        state.clone()
    }
}

impl From<Arc<DebugStateStore>> for DebugStateStore {
    fn from(state: Arc<DebugStateStore>) -> Self {
        state.as_ref().clone()
    }
}

async fn serve(listener: TcpListener, state: DebugStateStore, mut shutdown: oneshot::Receiver<()>) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let Ok((stream, _peer)) = accepted else {
                    break;
                };
                let state = state.clone();
                connections.spawn(async move {
                    handle_connection(stream, state).await;
                });
            }
        }
    }

    connections.abort_all();
    while connections.join_next().await.is_some() {}
}

async fn handle_connection(mut stream: TcpStream, state: DebugStateStore) {
    let request = match read_request(&mut stream).await {
        Ok(Some(request)) => request,
        Ok(None) => return,
        Err(_) => {
            let _ = write_response(
                &mut stream,
                response(400, "Bad Request", br#"{"error":"bad_request"}"#, false),
            )
            .await;
            return;
        }
    };

    let response = route_request(&request, &state);
    let _ = write_response(&mut stream, response).await;
}

const MAX_REQUEST_HEADER_BYTES: usize = 16 * 1024;

async fn read_request(stream: &mut TcpStream) -> io::Result<Option<Vec<u8>>> {
    let mut request = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(None);
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(Some(request));
        }
        if request.len() > MAX_REQUEST_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request headers too large",
            ));
        }
    }
}

struct HttpResponse {
    status: u16,
    reason: &'static str,
    body: Vec<u8>,
    allow_get: bool,
}

fn response(status: u16, reason: &'static str, body: &[u8], allow_get: bool) -> HttpResponse {
    HttpResponse {
        status,
        reason,
        body: body.to_vec(),
        allow_get,
    }
}

fn route_request(request: &[u8], state: &DebugStateStore) -> HttpResponse {
    let request_line = request
        .split(|byte| *byte == b'\n')
        .next()
        .and_then(|line| line.strip_suffix(b"\r"))
        .unwrap_or_default();
    let mut parts = request_line.split(|byte| *byte == b' ' || *byte == b'\t');
    let method = parts.next().filter(|part| !part.is_empty());
    let target = parts.next().filter(|part| !part.is_empty());

    let Some(method) = method else {
        return response(400, "Bad Request", br#"{"error":"bad_request"}"#, false);
    };
    if method != b"GET" {
        return response(405, "Method Not Allowed", br#"{"error":"read_only"}"#, true);
    }

    match target {
        Some(b"/health") => response(200, "OK", br#"{"status":"ok"}"#, false),
        Some(b"/v1/state") => match serde_json::to_vec(state.snapshot().as_ref()) {
            Ok(body) => response(200, "OK", &body, false),
            Err(_) => response(
                500,
                "Internal Server Error",
                br#"{"error":"internal"}"#,
                false,
            ),
        },
        _ => response(404, "Not Found", br#"{"error":"not_found"}"#, false),
    }
}

async fn write_response(stream: &mut TcpStream, response: HttpResponse) -> io::Result<()> {
    let mut headers = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nCache-Control: no-store\r\n",
        response.status, response.reason
    );
    if response.allow_get {
        headers.push_str("Allow: GET\r\n");
    }
    headers.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        response.body.len()
    ));
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(&response.body).await?;
    stream.shutdown().await
}

fn read_recover<T>(lock: &std::sync::RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    match lock.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn write_recover<T>(lock: &std::sync::RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    match lock.write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
