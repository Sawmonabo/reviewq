use std::collections::HashMap;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures_util::{SinkExt, StreamExt};
use reviewq_app::access::AppError;
use reviewq_app::hosts::{DispatchGuard, FollowupResume, QuestionAnswer, QuestionForm};
use reviewq_app::launch::LaunchProgress;
use reviewq_domain::identity::NativeSessionId;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep_until, timeout};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};
use tokio_tungstenite::{WebSocketStream, client_async_with_config};

use super::live::Live;
use super::questions::Questions;

const REQUEST_LIMIT: usize = 8;
const DISCOVERY_LIMIT: u64 = 16 * 1024;
const FRAME_LIMIT: usize = 2 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(5);
const IDLE_TIME: Duration = Duration::from_secs(60);

type Socket = WebSocketStream<UnixStream>;

pub(super) struct Connection {
    sender: mpsc::Sender<CommandRequest>,
    questions: Arc<Mutex<Questions>>,
    pub(super) live: Arc<Live>,
    reach: Arc<Mutex<Reach>>,
}

/// What the latest attempt to connect learned about the profile's Codex daemon.
#[derive(Clone, Copy, Default)]
pub(super) enum Reach {
    #[default]
    Unknown,
    /// No daemon answered: discovery found none, or its socket refused the connection.
    Absent,
    /// A daemon accepted the connection on a socket bound at this time.
    Bound(Option<SystemTime>),
}

enum CommandRequest {
    Rpc(Rpc),
    Answer(
        QuestionAnswer,
        QuestionForm,
        oneshot::Sender<Result<QuestionForm, AnswerError>>,
    ),
}

struct Rpc {
    method: &'static str,
    params: Value,
    guard: Option<Effect>,
    reply: oneshot::Sender<Result<Value, CallError>>,
}

struct Pending {
    reply: oneshot::Sender<Result<Value, CallError>>,
    deadline: Instant,
}

enum Effect {
    Delivery(DispatchGuard, Option<PathBuf>),
    FollowupResume(FollowupResume),
    Launch(LaunchProgress),
}

impl Effect {
    async fn check(self, questions: &Mutex<Questions>) -> Result<(), AppError> {
        match self {
            Self::Delivery(guard, question_source) => {
                if let Some(question) = guard.async_question() {
                    let path = question_source.ok_or(AppError::Unavailable)?;
                    if !crate::history::verify_pending_async_question(
                        question.session.clone(),
                        path,
                        question.question.clone(),
                    )
                    .await?
                    {
                        return Err(AppError::PreconditionFailed);
                    }
                }
                let pending = if guard.is_followup() {
                    pending_threads(questions)?
                } else {
                    Vec::new()
                };
                guard.check(pending).await
            }
            Self::FollowupResume(guard) => guard.check(pending_threads(questions)?).await,
            Self::Launch(guard) => guard.check().await,
        }
    }
}

fn pending_threads(questions: &Mutex<Questions>) -> Result<Vec<NativeSessionId>, AppError> {
    Ok(questions
        .lock()
        .map_err(|_| AppError::Internal)?
        .pending_threads())
}

pub(super) enum AnswerError {
    BeforeSend,
    Uncertain,
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
        let questions = Arc::new(Mutex::new(Questions::default()));
        let live = Arc::new(Live::default());
        let reach = Arc::new(Mutex::new(Reach::Unknown));
        tokio::spawn(daemon.run(commands, questions.clone(), live.clone(), reach.clone()));
        Self {
            sender,
            questions,
            live,
            reach,
        }
    }

    pub(super) fn reach(&self) -> Reach {
        self.reach.lock().map_or(Reach::Unknown, |reach| *reach)
    }

    pub(super) fn questions(
        &self,
        thread: &str,
        request: Option<&str>,
    ) -> Result<Vec<QuestionForm>, AppError> {
        Ok(self
            .questions
            .lock()
            .map_err(|_| AppError::Internal)?
            .list(thread, request))
    }

    pub(super) async fn answer(
        &self,
        answer: QuestionAnswer,
        form: QuestionForm,
    ) -> Result<QuestionForm, AnswerError> {
        let (reply, result) = oneshot::channel();
        self.sender
            .try_send(CommandRequest::Answer(answer, form, reply))
            .map_err(|_| AnswerError::BeforeSend)?;
        timeout(DEADLINE + DEADLINE, result)
            .await
            .map_err(|_| AnswerError::Uncertain)?
            .map_err(|_| AnswerError::Uncertain)?
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
        question_source: Option<PathBuf>,
    ) -> Result<(), CallError> {
        self.call(
            method,
            params,
            Some(Effect::Delivery(guard, question_source)),
        )
        .await
        .map(|_| ())
    }

    pub(super) async fn resume_followup<T: DeserializeOwned>(
        &self,
        guard: FollowupResume,
    ) -> Result<T, AppError> {
        let params = json!({"threadId":guard.thread().as_str(),"excludeTurns":true});
        let response = self
            .call("thread/resume", params, Some(Effect::FollowupResume(guard)))
            .await
            .map_err(CallError::app)?;
        serde_json::from_value(response).map_err(|_| AppError::Unavailable)
    }

    pub(super) async fn interrupt(&self, params: Value) -> Result<(), CallError> {
        self.call("turn/interrupt", params, None).await.map(|_| ())
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
            .try_send(CommandRequest::Rpc(Rpc {
                method,
                params,
                guard,
                reply,
            }))
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
    async fn run(
        self,
        mut commands: mpsc::Receiver<CommandRequest>,
        questions: Arc<Mutex<Questions>>,
        live: Arc<Live>,
        reach: Arc<Mutex<Reach>>,
    ) {
        let mut id = 1_u64;
        while let Some(first) = commands.recv().await {
            let request = match first {
                CommandRequest::Rpc(request) => request,
                CommandRequest::Answer(_, _, reply) => {
                    let _ = reply.send(Err(AnswerError::BeforeSend));
                    continue;
                }
            };
            if request.reply.is_closed() {
                continue;
            }
            let connected = timeout(DEADLINE, async {
                let (stream, bound) = self.open().await?;
                Ok((self.handshake(stream).await, bound))
            })
            .await;
            let (reached, connected) = match connected {
                Ok(Ok((socket, bound))) => (Reach::Bound(bound), socket),
                Ok(Err(None)) => (Reach::Absent, Err(AppError::Unavailable)),
                Ok(Err(Some(error))) => (Reach::Unknown, Err(error)),
                Err(_) => (Reach::Unknown, Err(AppError::Unavailable)),
            };
            if let Ok(mut current) = reach.lock() {
                *current = reached;
            }
            let Ok(mut socket) = connected else {
                let _ = request
                    .reply
                    .send(Err(CallError::BeforeSend(AppError::Unavailable)));
                continue;
            };
            multiplex(
                &mut socket,
                request,
                &mut commands,
                &questions,
                &live,
                &mut id,
            )
            .await;
            live.disconnect();

            if let Ok(mut cache) = questions.lock() {
                cache.disconnect();
            }
        }
    }

    /// Connects to the profile's daemon socket and reads when it was bound. `None` means no
    /// daemon answered; any other refusal carries its error.
    async fn open(&self) -> Result<(UnixStream, Option<SystemTime>), Option<AppError>> {
        let absent = |error| match error {
            AppError::Unavailable => None,
            error => Some(error),
        };
        let discovery = self.discover().await.map_err(absent)?;
        let socket_path = self
            .verified_socket(&discovery.socket_path)
            .await
            .map_err(absent)?;
        let bound = tokio::fs::metadata(&socket_path)
            .await
            .ok()
            .and_then(|socket| socket.modified().ok());
        let stream = UnixStream::connect(socket_path).await.map_err(|_| None)?;
        Ok((stream, bound))
    }

    async fn handshake(&self, stream: UnixStream) -> Result<Socket, AppError> {
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
                "capabilities": {"experimentalApi":true}
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

async fn multiplex(
    socket: &mut Socket,
    first: Rpc,
    commands: &mut mpsc::Receiver<CommandRequest>,
    questions: &Mutex<Questions>,
    live: &Live,
    id: &mut u64,
) {
    let mut pending = HashMap::new();
    if dispatch(socket, first, id, &mut pending, questions)
        .await
        .is_err()
    {
        return;
    }
    let mut last_activity = Instant::now();
    loop {
        let (question_active, question_deadline) = questions
            .lock()
            .ok()
            .map(|mut cache| cache.next())
            .unwrap_or((false, None));
        let next_deadline = pending
            .values()
            .map(|item| item.deadline)
            .min()
            .or_else(|| (!question_active).then_some(last_activity + IDLE_TIME));
        let wake = match (next_deadline, question_deadline) {
            (Some(left), Some(right)) => left.min(right),
            (Some(value), None) | (None, Some(value)) => value,
            (None, None) => last_activity + IDLE_TIME,
        };
        tokio::select! {
            work = commands.recv(), if pending.len() < REQUEST_LIMIT => {
                last_activity = Instant::now();
                match work {
                    Some(CommandRequest::Rpc(request)) => {
                        if dispatch(socket, request, id, &mut pending, questions).await.is_err() { break; }
                    }
                    Some(CommandRequest::Answer(answer, form, reply)) => {
                        if reply.is_closed() { continue; }
                        if answer.check().await.is_err() {
                            let _ = reply.send(Err(AnswerError::BeforeSend));
                            continue;
                        }
                        if reply.is_closed() { continue; }
                        let prepared = questions.lock().map_err(|_| AppError::Internal)
                            .and_then(|mut cache| cache.prepare(answer, form));
                        let result = match prepared {
                            Ok((form, Some(outbound))) => {
                                if !matches!(timeout(DEADLINE, send(socket,outbound)).await, Ok(Ok(()))) {
                                    let _ = reply.send(Err(AnswerError::Uncertain));
                                    break;
                                }
                                Ok(form)
                            }
                            Ok((form, None)) => Ok(form),
                            Err(_) => Err(AnswerError::BeforeSend),
                        };
                        let _ = reply.send(result);
                    }
                    None => break,
                }
            }
            event = receive_frame(socket) => {
                last_activity = Instant::now();
                match event {
                    Ok(Some(frame)) if frame.get("method").is_some() => {
                        if let Ok(mut cache) = questions.lock() { cache.frames(&frame); }
                        live.frame(&frame);
                    }
                    Ok(Some(frame)) => {
                        if let Some(id) = frame.get("id").and_then(Value::as_u64)
                            && let Some(active) = pending.remove(&id) {
                            let _ = active.reply.send(rpc_result(frame).map_err(CallError::Remote));
                        }
                    }
                    Ok(None) => (),
                    Err(()) => break,
                }
            }
            () = sleep_until(wake) => {
                let now = Instant::now();
                pending.retain(|_, item| !item.reply.is_closed() && item.deadline > now);
                if pending.is_empty() && !question_active && now >= last_activity + IDLE_TIME {
                    if !live.active() { break; }
                    last_activity = now;
                }
            }
        }
    }
    for (_, item) in pending {
        let _ = item.reply.send(Err(CallError::Uncertain));
    }
}

async fn dispatch(
    socket: &mut Socket,
    request: Rpc,
    id: &mut u64,
    pending: &mut HashMap<u64, Pending>,
    questions: &Mutex<Questions>,
) -> Result<(), ()> {
    if request.reply.is_closed() {
        return Ok(());
    }
    if let Some(guard) = request.guard
        && let Err(error) = guard.check(questions).await
    {
        let _ = request.reply.send(Err(CallError::Denied(error)));
        return Ok(());
    }
    if request.reply.is_closed() {
        return Ok(());
    }
    *id = id.wrapping_add(1).max(2);
    let wire = json!({"id": *id, "method": request.method, "params": request.params});
    if !matches!(timeout(DEADLINE, send(socket, wire)).await, Ok(Ok(()))) {
        let _ = request.reply.send(Err(CallError::Uncertain));
        return Err(());
    }
    pending.insert(
        *id,
        Pending {
            reply: request.reply,
            deadline: Instant::now() + DEADLINE,
        },
    );
    Ok(())
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
        return Ok(rpc_result(frame));
    }
}

fn rpc_result(mut frame: Value) -> Result<Value, AppError> {
    if let Some(result) = frame
        .as_object_mut()
        .and_then(|object| object.remove("result"))
    {
        return Ok(result);
    }
    Err(match frame.pointer("/error/code").and_then(Value::as_i64) {
        Some(-32600 | -32602) => AppError::NotFound,
        _ => AppError::Unavailable,
    })
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
