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
use tokio::time::{Instant, timeout};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};
use tokio_tungstenite::{WebSocketStream, client_async_with_config};

mod multiplex;
mod reach;
mod served;

pub(super) use reach::Reach;
use reach::Sightings;
pub(super) use served::{Reply, Served};

use super::live::Live;
use super::questions::Questions;

const REQUEST_LIMIT: usize = 8;
const DISCOVERY_LIMIT: u64 = 16 * 1024;
pub(super) const FRAME_LIMIT: usize = 2 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(5);
/// How long a call waits for a place in the command queue while a burst fills it. Nothing is
/// sent while it waits, so a call that runs out of it is refused `Busy`, unsent.
const ROOM_WAIT: Duration = Duration::from_secs(10);
/// A bound on the wait for the loop's answer, against a fault only. The loop answers every
/// request it takes, and each step it takes for one ends within `DEADLINE`: connecting,
/// checking the guard and waiting for the reply. A request behind a full queue while every
/// connection attempt times out is answered in about a minute, half of this. Past it the
/// caller cannot tell whether the request was written, so it reads as `Uncertain`.
const ANSWER_LIMIT: Duration = Duration::from_secs(120);
const IDLE_TIME: Duration = Duration::from_secs(60);

type Socket = WebSocketStream<UnixStream>;

pub(super) struct Connection {
    sender: mpsc::Sender<CommandRequest>,
    questions: Arc<Mutex<Questions>>,
    pub(super) live: Arc<Live>,
    reach: Arc<Mutex<Reach>>,
    served: Arc<Mutex<Option<Arc<Served>>>>,
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
    reply: oneshot::Sender<Result<Reply, CallError>>,
}

struct Pending {
    reply: oneshot::Sender<Result<Reply, CallError>>,
    deadline: Instant,
}

enum Effect {
    Delivery(DispatchGuard, Option<PathBuf>),
    FollowupResume(FollowupResume),
    Launch(LaunchProgress),
    /// A guard that passes once its sender fires or is dropped, for tests of the loop.
    #[cfg(test)]
    Wait(oneshot::Receiver<()>),
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
            #[cfg(test)]
            Self::Wait(release) => {
                let _ = release.await;
                Ok(())
            }
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
        let served = Arc::new(Mutex::new(None));
        tokio::spawn(daemon.run(
            commands,
            questions.clone(),
            live.clone(),
            reach.clone(),
            served.clone(),
        ));
        Self {
            sender,
            questions,
            live,
            reach,
            served,
        }
    }

    pub(super) fn reach(&self) -> Reach {
        self.reach.lock().map_or(Reach::Unknown, |reach| *reach)
    }

    /// The daemon session the connection holds now, if any.
    pub(super) fn served(&self) -> Option<Arc<Served>> {
        self.served.lock().ok().and_then(|served| served.clone())
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

    /// Holds a place in the command queue for one answer, waiting up to `ROOM_WAIT` while a
    /// burst fills it. Taken before the answer is reserved, so a queue that stays full
    /// refuses the answer as `Busy` with nothing reserved and nothing to settle.
    pub(super) async fn answer_slot(&self) -> Result<AnswerSlot<'_>, AppError> {
        match timeout(ROOM_WAIT, self.sender.reserve()).await {
            Ok(Ok(permit)) => Ok(AnswerSlot { permit }),
            Ok(Err(_)) => Err(AppError::Unavailable),
            Err(_) => Err(AppError::Busy),
        }
    }

    /// Sends one request and returns its raw reply frame with the daemon session that
    /// answered it, for a caller that reads the error itself or needs that session.
    pub(super) async fn request(
        &self,
        method: &'static str,
        params: Value,
    ) -> Result<Reply, AppError> {
        self.round_trip(method, params, None)
            .await
            .map_err(CallError::app)
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
        let (frame, _) = self.round_trip(method, params, guard).await?;
        rpc_result(frame).map_err(CallError::Remote)
    }

    /// Queues one request, waiting up to `ROOM_WAIT` for room while a burst fills the queue,
    /// and returns its reply. A call that never got room was not sent: `BeforeSend`. The
    /// loop owns the rest: it answers `Denied` for a guard that refused and `BeforeSend` for
    /// a request it never wrote, and waits for the reply only from the moment it wrote the
    /// request, so `Uncertain` means it may have been sent.
    async fn round_trip(
        &self,
        method: &'static str,
        params: Value,
        guard: Option<Effect>,
    ) -> Result<Reply, CallError> {
        let permit = match timeout(ROOM_WAIT, self.sender.reserve()).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => return Err(CallError::BeforeSend(AppError::Unavailable)),
            Err(_) => return Err(CallError::BeforeSend(AppError::Busy)),
        };
        let (reply, receive) = oneshot::channel();
        permit.send(CommandRequest::Rpc(Rpc {
            method,
            params,
            guard,
            reply,
        }));
        timeout(ANSWER_LIMIT, receive)
            .await
            .map_err(|_| CallError::Uncertain)?
            .map_err(|_| CallError::Uncertain)?
    }
}

/// A place held in a connection's command queue for one answer.
pub(super) struct AnswerSlot<'a> {
    permit: mpsc::Permit<'a, CommandRequest>,
}

impl AnswerSlot<'_> {
    /// Sends `answer` in the place held and waits for whether the daemon took it. The loop
    /// answers `BeforeSend` for an answer it never wrote, and `Uncertain` for one whose write
    /// it began; so does a wait past `ANSWER_LIMIT`.
    pub(super) async fn answer(
        self,
        answer: QuestionAnswer,
        form: QuestionForm,
    ) -> Result<QuestionForm, AnswerError> {
        let (reply, result) = oneshot::channel();
        self.permit
            .send(CommandRequest::Answer(answer, form, reply));
        timeout(ANSWER_LIMIT, result)
            .await
            .map_err(|_| AnswerError::Uncertain)?
            .map_err(|_| AnswerError::Uncertain)?
    }
}

impl Daemon {
    async fn run(
        self,
        mut commands: mpsc::Receiver<CommandRequest>,
        questions: Arc<Mutex<Questions>>,
        live: Arc<Live>,
        reach: Arc<Mutex<Reach>>,
        served: Arc<Mutex<Option<Arc<Served>>>>,
    ) {
        let mut id = 1_u64;
        let mut sightings = Sightings::new();
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
                Ok::<_, AppError>((self.handshake(stream).await, bound))
            })
            .await;
            let (reached, connected) = match connected {
                Ok(Ok((socket, bound))) => (
                    Reach::Bound(bound.map(|bound| sightings.age(bound))),
                    socket,
                ),
                Ok(Err(error)) => (Reach::Unknown, Err(error)),
                Err(_) => (Reach::Unknown, Err(AppError::Unavailable)),
            };
            if let Ok(mut current) = reach.lock() {
                *current = reached;
            }
            let Ok((mut socket, home)) = connected else {
                let _ = request
                    .reply
                    .send(Err(CallError::BeforeSend(AppError::Unavailable)));
                continue;
            };
            let session = Arc::new(Served::new(home.join("hooks.json")));
            if let Ok(mut current) = served.lock() {
                *current = Some(session.clone());
            }
            multiplex::run(
                &mut socket,
                request,
                &mut commands,
                &questions,
                &live,
                &session,
                &mut id,
            )
            .await;
            live.disconnect();
            if let Ok(mut current) = served.lock() {
                *current = None;
            }

            if let Ok(mut cache) = questions.lock() {
                cache.disconnect();
            }
        }
    }

    /// Connects to the profile's daemon socket and reads when it was bound.
    async fn open(&self) -> Result<(UnixStream, Option<SystemTime>), AppError> {
        let discovery = self.discover().await?;
        let socket_path = self.verified_socket(&discovery.socket_path).await?;
        let bound = tokio::fs::metadata(&socket_path)
            .await
            .ok()
            .and_then(|socket| socket.modified().ok());
        let stream = UnixStream::connect(socket_path)
            .await
            .map_err(|_| AppError::Unavailable)?;
        Ok((stream, bound))
    }

    /// Opens the session on `stream` and returns it with the Codex home the daemon names.
    async fn handshake(&self, stream: UnixStream) -> Result<(Socket, PathBuf), AppError> {
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
        let result = exchange(&mut ws, 1, "initialize", initialize_params())
            .await
            .map_err(|_| AppError::Unavailable)??;
        let (home, initialized) = accept_initialize(result, &self.profile)?;
        send(&mut ws, initialized)
            .await
            .map_err(|_| AppError::Unavailable)?;
        Ok((ws, home))
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

/// The user whose Codex daemon ReviewQ may talk to for the Codex home `profile`, a real
/// path: its owner, and only for a folder no other user can write to. The one rule for both
/// ways ReviewQ talks to a daemon, the service's connection and a single request.
pub(super) fn home_owner(profile: &Path) -> Result<u32, AppError> {
    let metadata = std::fs::metadata(profile).map_err(|_| AppError::Unavailable)?;
    if !metadata.is_dir() || metadata.mode() & 0o022 != 0 {
        return Err(AppError::Forbidden);
    }
    Ok(metadata.uid())
}

/// Sends one request to the running daemon of the Codex home `profile`, a real path, on a
/// session of its own, and returns the reply. For a caller with no `Connection`, such as the
/// installer. Discovery starts no daemon, so with none running this fails at once.
pub(super) async fn request_once(
    executable: &Path,
    profile: &Path,
    method: &str,
    params: Value,
) -> Result<Value, AppError> {
    let owner = home_owner(profile)?;
    let daemon = Daemon {
        executable: executable.into(),
        profile: profile.into(),
        owner,
    };
    let (stream, _) = daemon.open().await?;
    let (mut socket, _) = daemon.handshake(stream).await?;
    let reply = exchange(&mut socket, 2, method, params)
        .await
        .map_err(|_| AppError::Unavailable)?;
    let _ = socket.close(None).await;
    reply
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

/// The parameters of the `initialize` request that opens every Codex app-server session.
pub(super) fn initialize_params() -> Value {
    json!({
        "clientInfo": {"name":"reviewq","version":env!("CARGO_PKG_VERSION")},
        "capabilities": {"experimentalApi":true}
    })
}

/// Accepts an `initialize` result only from a server whose Codex home leads to the real path
/// `profile`, and returns that home as the server names it with the `initialized`
/// notification to send next.
pub(super) fn accept_initialize(
    result: Value,
    profile: &Path,
) -> Result<(PathBuf, Value), AppError> {
    let initialized: Initialized =
        serde_json::from_value(result).map_err(|_| AppError::Unavailable)?;
    if initialized.codex_home.canonicalize().ok().as_deref() != Some(profile) {
        return Err(AppError::Forbidden);
    }
    Ok((initialized.codex_home, json!({"method":"initialized"})))
}

pub(super) fn rpc_result(mut frame: Value) -> Result<Value, AppError> {
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

#[cfg(test)]
mod tests;
