use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, OriginalUri, Request, State};
use axum::http::{HeaderMap, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reviewq_app::access::{Actor, AppError};
use reviewq_app::actions::{ActionCommandStore, ActionStore};
use reviewq_app::decisions::{Checkpoints, DecisionStore};
use reviewq_app::events::EventLog;
use reviewq_app::followups::{FollowupCommands, FollowupStore};
use reviewq_app::history::{HistoryStore, NativeHistoryReader};
use reviewq_app::hosts::{
    ClaudeCompanion, ClaudeStore, ClaudeWorkflowStore, LeadStore, MessageHistoryStore, NativeHost,
    NativeTerminal,
};
use reviewq_app::launch::{LaunchStore, NativeLaunchers, NativeModels};
use reviewq_app::notifications::InboxStore;
use reviewq_app::queue::{QueueStore, ReviewLookup, TrackingStore};
use reviewq_app::reports::{
    ReportCaptureStore, ReportFileSource, ReportMutationStore, ReportStore,
};
use reviewq_app::settings::{ConnectionSources, NativeSkills, RuntimeSources, SettingsStore};
use reviewq_app::sources::SourceRefreshStore;
use reviewq_app::workflow::NativeWorkflowProof;
use reviewq_app::workspace::WorkspaceStore;
use reviewq_domain::identity::EventCursor;
use reviewq_protocol::queue::QueueSync;
use reviewq_protocol::{EventCursorDto, Success};
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, Semaphore, watch};
use tower_http::services::{ServeDir, ServeFile};

mod auth;
mod changes;
mod checkout;
mod control;
mod control_ops;
mod error;
mod events;
mod features;
mod history;
mod hosts;
mod launch;
mod mcp;
mod messages;
mod pages;
mod questions;
mod refresh;
mod settings;
mod sources;
mod targets;
mod terminal;
mod tracking;

pub use control::{ControlRequest, ServiceEndpoint, ServiceIdentity};

use auth::Auth;
use error::{ApiBytes, ApiError};

/// Shared application ports for one ReviewQ listener pair.
pub trait Store:
    WorkspaceStore
    + QueueStore
    + TrackingStore
    + ReviewLookup
    + EventLog
    + LeadStore
    + ClaudeStore
    + ClaudeWorkflowStore
    + HistoryStore
    + SourceRefreshStore
    + ReportStore
    + ReportMutationStore
    + ReportCaptureStore
    + MessageHistoryStore
    + InboxStore
    + DecisionStore
    + ActionStore
    + ActionCommandStore
    + FollowupStore
    + FollowupCommands
    + SettingsStore
    + LaunchStore
{
}
impl<
    S: WorkspaceStore
        + QueueStore
        + TrackingStore
        + ReviewLookup
        + EventLog
        + LeadStore
        + ClaudeStore
        + ClaudeWorkflowStore
        + HistoryStore
        + SourceRefreshStore
        + ReportStore
        + ReportMutationStore
        + ReportCaptureStore
        + MessageHistoryStore
        + InboxStore
        + DecisionStore
        + ActionStore
        + ActionCommandStore
        + FollowupStore
        + FollowupCommands
        + SettingsStore
        + LaunchStore,
> Store for S
{
}

/// Native proof and guarded artifact reads for the workflow routes.
pub struct WorkflowIntegration {
    pub proof: Arc<dyn NativeWorkflowProof>,
    pub report_files: Arc<dyn ReportFileSource>,
}

pub(super) struct Entry {
    store: Arc<dyn Store>,
    source: Arc<RuntimeSources>,
    settings_sources: Arc<dyn ConnectionSources>,
    skills: NativeSkills,
    models: NativeModels,
    launchers: NativeLaunchers,
    launch_slots: Arc<Semaphore>,
    refresh_wakeup: Notify,
    host: Option<Arc<dyn NativeHost>>,
    claude: Option<Arc<dyn ClaudeCompanion>>,
    file_history: Arc<dyn NativeHistoryReader>,
    history_watches: history::Watchers,
    workflow: WorkflowIntegration,
    terminal: Arc<dyn NativeTerminal>,
    sends_allowed: bool,
    commits: watch::Receiver<EventCursor>,
    shutdown: watch::Receiver<bool>,
    event_slots: Arc<Semaphore>,
    auth: Auth,
    checkpoints: Checkpoints,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", deny_unknown_fields)]
enum SessionRequest {
    Launch,
    Redeem { ticket: String },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Status<'a> {
    instance: &'a str,
    build: &'a str,
    browser: String,
    native: String,
    #[serde(flatten)]
    identity: &'a ServiceIdentity,
    ready: bool,
    configured_projects: usize,
    total_mrs: u64,
    sync: QueueSync,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionResult {
    csrf_token: String,
}

/// Runtime-owned listener identity and commit/shutdown signals.
pub struct ServiceRuntime<'a> {
    pub endpoint: ServiceEndpoint,
    pub web: &'a Path,
    pub sends_allowed: bool,
    pub source_poll_interval: Option<Duration>,
    pub settings_sources: Arc<dyn ConnectionSources>,
    pub skills: NativeSkills,
    pub models: NativeModels,
    pub launchers: NativeLaunchers,
    pub launches_allowed: bool,
    pub claude: Option<Arc<dyn ClaudeCompanion>>,
    pub workflow: WorkflowIntegration,
    pub terminal: Arc<dyn NativeTerminal>,
    pub commits: watch::Receiver<EventCursor>,
    pub shutdown: watch::Receiver<bool>,
}

/// Mounts production assets and the private versioned HTTP routes.
pub fn router(
    store: Arc<dyn Store>,
    source: Arc<RuntimeSources>,
    host: Option<Arc<dyn NativeHost>>,
    file_history: Arc<dyn NativeHistoryReader>,
    runtime: ServiceRuntime<'_>,
) -> Router {
    let entry = Arc::new(Entry {
        store,
        source,
        settings_sources: runtime.settings_sources,
        skills: runtime.skills,
        models: runtime.models,
        launchers: runtime.launchers,
        launch_slots: Arc::new(Semaphore::new(if runtime.launches_allowed { 4 } else { 0 })),
        refresh_wakeup: Notify::new(),
        host,
        claude: runtime.claude,
        file_history,
        history_watches: history::Watchers::default(),
        workflow: runtime.workflow,
        terminal: runtime.terminal,
        sends_allowed: runtime.sends_allowed,
        commits: runtime.commits,
        shutdown: runtime.shutdown,
        event_slots: Arc::new(Semaphore::new(32)),
        auth: Auth::new(runtime.endpoint),
        checkpoints: Checkpoints::default(),
    });
    let web = runtime.web;
    if let Some(interval) = runtime.source_poll_interval {
        tokio::spawn(poll_source(entry.clone(), interval));
    }
    let api = Router::new()
        .route("/status", get(status))
        .route("/control/review", post(control_ops::review))
        .route(
            "/control/ack",
            post(control_ops::ack).layer(DefaultBodyLimit::max(160 * 1024)),
        )
        .route("/settings", get(settings::read).put(settings::save))
        .route("/settings/test", post(settings::test))
        .route("/settings/catalog", post(settings::catalog))
        .route("/settings/disconnect", post(settings::disconnect))
        .route("/checkout/grant", post(checkout::grant))
        .route("/events", get(events::feed))
        .route("/session", get(session).post(start_session))
        .route("/queue", get(pages::queue))
        .route("/queue/sync", get(refresh::sync))
        .route("/queue/refresh", post(refresh::start))
        .route("/queue/refresh/{refresh_id}", get(refresh::status))
        .route("/tracking", post(tracking::track))
        .route("/tickets/{key}", get(tracking::ticket))
        .route("/reviews/resolve", get(pages::resolve))
        .route("/reviews/{review_id}", get(pages::overview))
        .route("/reviews/{review_id}/changes/files", get(changes::files))
        .route("/reviews/{review_id}/changes/patch", get(changes::patch))
        .route(
            "/reviews/{review_id}/changes/discussions",
            get(changes::discussions),
        )
        .route(
            "/reviews/{review_id}/changes/approvals",
            get(changes::approvals),
        )
        .route("/runs/{run_id}", get(pages::run))
        .route("/runs/catalog", post(launch::catalog))
        .route("/runs/prepare", post(launch::prepare))
        .route(
            "/runs/start",
            post(launch::start).layer(DefaultBodyLimit::max(144 * 1024)),
        )
        .route(
            "/reviews/{review_id}/launches/{submission_id}",
            get(launch::receipt),
        )
        .route(
            "/runs/{run_id}/sessions/{session_key}/terminal",
            get(terminal::read),
        )
        .route(
            "/runs/{run_id}/sessions/{session_key}/terminal/open",
            post(terminal::open),
        )
        .route(
            "/runs/{run_id}/sessions/{session_key}/questions",
            get(questions::list),
        )
        .route(
            "/runs/{run_id}/sessions/{session_key}/questions/{request_id}/answer",
            post(questions::answer),
        )
        .route("/native/codex/link", post(hosts::link))
        .route(
            "/native/codex/registration",
            post(hosts::codex_registration),
        )
        .route("/native/claude/link", post(hosts::claude_link))
        .merge(hosts::claude::routes())
        .route(
            "/native/claude/registration",
            post(hosts::claude_registration),
        )
        .merge(features::routes())
        .merge(history::routes())
        .route(
            "/runs/{run_id}/sessions/{session_key}/history/page",
            get(history::page),
        )
        .route(
            "/runs/{run_id}/sessions/{session_key}/history/content",
            get(history::content),
        )
        .route(
            "/runs/{run_id}/sessions/{session_key}/messages",
            get(messages::list).post(messages::send),
        )
        .route(
            "/runs/{run_id}/sessions/{session_key}/messages/{submission_id}",
            get(messages::get),
        )
        .fallback(api_not_found)
        .layer(DefaultBodyLimit::max(64 * 1024));
    Router::new()
        .merge(mcp::mount(entry.clone()))
        .nest("/api/v1", api)
        .fallback_service(
            ServeDir::new(web).not_found_service(ServeFile::new(web.join("index.html"))),
        )
        .layer(middleware::from_fn_with_state(entry.clone(), guard))
        .with_state(entry)
}

async fn poll_source(entry: Arc<Entry>, interval: Duration) {
    let mut shutdown = entry.shutdown.clone();
    while !*shutdown.borrow_and_update() {
        tokio::select! {
            _ = shutdown.changed() => break,
            () = tokio::time::sleep(interval) => {},
            () = entry.refresh_wakeup.notified() => {},
        }
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = refresh::enqueue(&entry) => {},
        }
    }
}

async fn guard(State(entry): State<Arc<Entry>>, request: Request, next: Next) -> Response {
    if !entry.auth.allowed_host(request.headers()) {
        return ApiError(AppError::Forbidden).into_response();
    }
    let private = request.uri().path().starts_with("/api/") || request.uri().path() == "/mcp";
    let mut response = next.run(request).await;
    if private {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    }
    response
}

async fn api_not_found() -> ApiError {
    ApiError(AppError::NotFound)
}

async fn status(
    State(entry): State<Arc<Entry>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let target = uri
        .path_and_query()
        .ok_or(AppError::InvalidRequest)?
        .as_str();
    let proof = entry.auth.control("GET", target, &headers, b"")?;
    let workspace = entry
        .store
        .workspace_context(Actor::local_operator())
        .await?;
    let queue = entry
        .store
        .read_queue(Actor::local_operator(), None, 1)
        .await?;
    let endpoint = &entry.auth.endpoint;
    let response = Success {
        data: Status {
            instance: endpoint.instance(),
            build: env!("CARGO_PKG_VERSION"),
            browser: endpoint.browser().to_string(),
            native: endpoint.native().to_string(),
            identity: endpoint.identity(),
            ready: true,
            configured_projects: workspace.value.projects.len(),
            total_mrs: queue.value.total_mrs,
            sync: pages::queue_sync(&entry.active_source().await?, queue.value.sync),
        },
        cursor: EventCursorDto::new(queue.cursor.get()).ok_or(AppError::Internal)?,
    };
    let body = serde_json::to_vec(&response).map_err(|_| AppError::Internal)?;
    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    endpoint.response_proof(&proof, 200, &mut response_headers, &body)?;
    Ok((response_headers, body).into_response())
}

async fn session(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
) -> Result<Json<Success<SessionResult>>, ApiError> {
    let csrf = URL_SAFE_NO_PAD.encode(entry.auth.csrf(&headers)?);
    let body = SessionResult { csrf_token: csrf };
    Ok(Json(Success {
        data: body,
        cursor: entry.head().await?,
    }))
}

async fn start_session(
    State(entry): State<Arc<Entry>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Response, ApiError> {
    if !headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(';').next() == Some("application/json"))
    {
        return Err(AppError::InvalidRequest.into());
    }
    let request: SessionRequest =
        serde_json::from_slice(&bytes).map_err(|_| AppError::InvalidRequest)?;
    match request {
        SessionRequest::Launch => {
            let target = uri
                .path_and_query()
                .ok_or(AppError::InvalidRequest)?
                .as_str();
            let proof = entry.auth.control("POST", target, &headers, &bytes)?;
            let cursor = entry.head().await?;
            let response = Success {
                data: serde_json::json!({"ticket": entry.auth.launch()?}),
                cursor,
            };
            let body = serde_json::to_vec(&response).map_err(|_| AppError::Internal)?;
            let mut response_headers = HeaderMap::new();
            response_headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
            entry
                .auth
                .endpoint
                .response_proof(&proof, 200, &mut response_headers, &body)?;
            Ok((response_headers, body).into_response())
        }
        SessionRequest::Redeem { ticket } => {
            entry.auth.check_origin(&headers)?;
            let cursor = entry.head().await?;
            let (csrf_token, cookie) = entry.auth.redeem(&ticket)?;
            let mut result = Json(Success {
                data: SessionResult { csrf_token },
                cursor,
            })
            .into_response();
            result.headers_mut().insert(header::SET_COOKIE, cookie);
            Ok(result)
        }
    }
}

impl Entry {
    async fn head(&self) -> Result<EventCursorDto, AppError> {
        let snapshot = self
            .store
            .workspace_context(Actor::local_operator())
            .await?;
        EventCursorDto::new(snapshot.cursor.get()).ok_or(AppError::Internal)
    }
}
