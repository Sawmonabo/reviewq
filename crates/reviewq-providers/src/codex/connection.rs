use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use reviewq_app::access::AppError;
use reviewq_app::hosts::DispatchGuard;
use reviewq_app::launch::LaunchProgress;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};
use tokio_tungstenite::{WebSocketStream, client_async_with_config};

const REQUEST_LIMIT: usize = 8;
const DISCOVERY_LIMIT: u64 = 16 * 1024;
const FRAME_LIMIT: usize = 2 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(5);
const IDLE_TIME: Duration = Duration::from_secs(60);

type Socket = WebSocketStream<UnixStream>;

pub(super) struct Connection {
    sender: mpsc::Sender<Rpc>,
}

struct Rpc {
    method: &'static str,
    params: Value,
    guard: Option<Effect>,
    reply: oneshot::Sender<Result<Value, CallError>>,
}

enum Effect {
    Delivery(DispatchGuard),
    Launch(LaunchProgress),
}

impl Effect {
    async fn check(self) -> Result<(), AppError> {
        match self {
            Self::Delivery(guard) => guard.check().await,
            Self::Launch(guard) => guard.check().await,
        }
    }
}

pub(super) enum CallError {
    BeforeSend(AppError),
    Denied(AppError),
    Remote(AppError),
    Uncertain,
}

impl CallError {
    fn app(self) -> AppError {
        match self {
            Self::BeforeSend(error) | Self::Denied(error) | Self::Remote(error) => error,
            Self::Uncertain => AppError::Unavailable,
        }
    }
}
struct Daemon {
    executable: PathBuf,
    profile: PathBuf,
    owner: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Discovery {
    status: String,
    socket_path: PathBuf,
    app_server_version: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Initialized {
    codex_home: PathBuf,
}

impl Connection {
    pub(super) fn new(executable: PathBuf, profile: PathBuf, owner: u32) -> Self {
        let (sender, commands) = mpsc::channel(REQUEST_LIMIT);
        let daemon = Daemon {
            executable,
            profile,
            owner,
        };
        tokio::spawn(daemon.run(commands));
        Self { sender }
    }

    pub(super) async fn read<T: DeserializeOwned>(
        &self,
        method: &'static str,
        params: Value,
    ) -> Result<T, AppError> {
        let response = self
            .call(method, params, None)
            .await
            .map_err(CallError::app)?;
        serde_json::from_value(response).map_err(|_| AppError::Unavailable)
    }

    pub(super) async fn submit(
        &self,
        method: &'static str,
        params: Value,
        guard: DispatchGuard,
    ) -> Result<(), CallError> {
        self.call(method, params, Some(Effect::Delivery(guard)))
            .await
            .map(|_| ())
    }

    pub(super) async fn launch<T: DeserializeOwned>(
        &self,
        method: &'static str,
        params: Value,
        guard: LaunchProgress,
    ) -> Result<T, CallError> {
        let value = self
            .call(method, params, Some(Effect::Launch(guard)))
            .await?;
        serde_json::from_value(value).map_err(|_| CallError::Uncertain)
    }

    async fn call(
        &self,
        method: &'static str,
        params: Value,
        guard: Option<Effect>,
    ) -> Result<Value, CallError> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .try_send(Rpc {
                method,
                params,
                guard,
                reply,
            })
            .map_err(|error| {
                CallError::BeforeSend(match error {
                    mpsc::error::TrySendError::Full(_) => AppError::Busy,
                    mpsc::error::TrySendError::Closed(_) => AppError::Unavailable,
                })
            })?;
        timeout(DEADLINE + DEADLINE, receive)
            .await
            .map_err(|_| CallError::Uncertain)?
            .map_err(|_| CallError::Uncertain)?
    }
}

impl Daemon {
    async fn run(self, mut commands: mpsc::Receiver<Rpc>) {
        let mut socket = None;
        let mut id = 1_u64;
        loop {
            let request = match socket.as_mut() {
                None => commands.recv().await,
                Some(ws) => tokio::select! {
                    request = commands.recv() => request,
                    frame = timeout(IDLE_TIME, receive_frame(ws)) => {
                        if !matches!(frame, Ok(Ok(_))) {
                            socket = None;
                        }
                        continue;
                    }
                },
            };
            let Some(request) = request else { break };
            if request.reply.is_closed() {
                continue;
            }
            if socket.is_none() {
                socket = timeout(DEADLINE, self.connect())
                    .await
                    .ok()
                    .and_then(Result::ok);
            }
            let Some(ws) = socket.as_mut() else {
                let _ = request
                    .reply
                    .send(Err(CallError::BeforeSend(AppError::Unavailable)));
                continue;
            };
            if let Some(guard) = request.guard {
                if let Err(error) = guard.check().await {
                    let _ = request.reply.send(Err(CallError::Denied(error)));
                    continue;
                }
                if request.reply.is_closed() {
                    continue;
                }
            }
            id = id.wrapping_add(1).max(2);
            match timeout(DEADLINE, exchange(ws, id, request.method, request.params)).await {
                Ok(Ok(result)) => {
                    let _ = request.reply.send(result.map_err(CallError::Remote));
                }
                _ => {
                    socket = None;
                    let _ = request.reply.send(Err(CallError::Uncertain));
                }
            }
        }
    }

    async fn connect(&self) -> Result<Socket, AppError> {
        let discovery = self.discover().await?;
        let socket_path = self.verified_socket(&discovery.socket_path).await?;
        let stream = UnixStream::connect(socket_path)
            .await
            .map_err(|_| AppError::Unavailable)?;
        if stream.peer_cred().map_err(|_| AppError::Unavailable)?.uid() != self.owner {
            return Err(AppError::Forbidden);
        }

        let config = WebSocketConfig::default()
            .max_message_size(Some(FRAME_LIMIT))
            .max_frame_size(Some(FRAME_LIMIT))
            .max_write_buffer_size(256 * 1024);
        let (mut ws, _) = client_async_with_config("ws://localhost/", stream, Some(config))
            .await
            .map_err(|_| AppError::Unavailable)?;
        let result = exchange(
            &mut ws,
            1,
            "initialize",
            json!({
                "clientInfo": {"name":"reviewq","version":env!("CARGO_PKG_VERSION")},
                "capabilities": {"experimentalApi":false}
            }),
        )
        .await
        .map_err(|_| AppError::Unavailable)??;
        let initialized: Initialized =
            serde_json::from_value(result).map_err(|_| AppError::Unavailable)?;
        if initialized.codex_home != self.profile {
            return Err(AppError::Forbidden);
        }
        send(&mut ws, json!({"method":"initialized"}))
            .await
            .map_err(|_| AppError::Unavailable)?;
        Ok(ws)
    }

    async fn verified_socket(&self, locator: &Path) -> Result<PathBuf, AppError> {
        let parent = locator.parent().ok_or(AppError::Forbidden)?;
        if !parent.starts_with(&self.profile)
            || tokio::fs::canonicalize(parent).await.ok().as_deref() != Some(parent)
        {
            return Err(AppError::Forbidden);
        }
        let metadata = tokio::fs::symlink_metadata(locator)
            .await
            .map_err(|_| AppError::Unavailable)?;
        if metadata.uid() != self.owner {
            return Err(AppError::Forbidden);
        }
        if metadata.file_type().is_socket() {
            return Ok(locator.into());
        }
        if !metadata.is_symlink() {
            return Err(AppError::Forbidden);
        }
        // Codex uses a private short socket path when it publishes a Unix locator.
        let target = tokio::fs::canonicalize(locator)
            .await
            .map_err(|_| AppError::Unavailable)?;
        let target_parent = target.parent().ok_or(AppError::Forbidden)?;
        let directory = tokio::fs::metadata(target_parent)
            .await
            .map_err(|_| AppError::Unavailable)?;
        let socket = tokio::fs::symlink_metadata(&target)
            .await
            .map_err(|_| AppError::Unavailable)?;
        if directory.uid() != self.owner
            || directory.mode() & 0o777 != 0o700
            || socket.uid() != self.owner
            || !socket.file_type().is_socket()
        {
            return Err(AppError::Forbidden);
        }
        Ok(target)
    }

    async fn discover(&self) -> Result<Discovery, AppError> {
        let mut child = Command::new(&self.executable)
            .args(["app-server", "daemon", "version"])
            .current_dir(Path::new("/"))
            .env("CODEX_HOME", &self.profile)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| AppError::Unavailable)?;
        let stdout = child.stdout.take().ok_or(AppError::Unavailable)?;
        let mut bytes = Vec::new();
        stdout
            .take(DISCOVERY_LIMIT + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| AppError::Unavailable)?;
        if bytes.len() as u64 > DISCOVERY_LIMIT
            || !child
                .wait()
                .await
                .map_err(|_| AppError::Unavailable)?
                .success()
        {
            return Err(AppError::Unavailable);
        }
        let discovery: Discovery =
            serde_json::from_slice(&bytes).map_err(|_| AppError::Unavailable)?;
        if discovery.status != "running"
            || discovery
                .app_server_version
                .as_deref()
                .is_none_or(str::is_empty)
        {
            return Err(AppError::Unavailable);
        }
        Ok(discovery)
    }
}

async fn exchange(
    socket: &mut Socket,
    id: u64,
    method: &str,
    params: Value,
) -> Result<Result<Value, AppError>, ()> {
    send(socket, json!({"id":id,"method":method,"params":params})).await?;
    loop {
        let Some(frame) = receive_frame(socket).await? else {
            continue;
        };
        if frame.get("id").and_then(Value::as_u64) != Some(id) || frame.get("method").is_some() {
            continue;
        }
        if let Some(result) = frame.get("result") {
            return Ok(Ok(result.clone()));
        }
        let error = match frame.pointer("/error/code").and_then(Value::as_i64) {
            Some(-32600 | -32602) => AppError::NotFound,
            _ => AppError::Unavailable,
        };
        return Ok(Err(error));
    }
}

async fn send(socket: &mut Socket, value: Value) -> Result<(), ()> {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .map_err(|_| ())
}

async fn receive_frame(socket: &mut Socket) -> Result<Option<Value>, ()> {
    match socket.next().await.ok_or(())?.map_err(|_| ())? {
        Message::Text(text) => serde_json::from_str(&text).map(Some).map_err(|_| ()),
        Message::Ping(_) => {
            socket.flush().await.map_err(|_| ())?;
            Ok(None)
        }
        Message::Close(_) => Err(()),
        _ => Ok(None),
    }
}
