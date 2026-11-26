use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, OriginalUri, Request, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use reviewq_app::access::{Actor, AppError};
use reviewq_app::actions::{ActionCommandStore, ActionStore};
use reviewq_app::changes::DiscussionEpochStore;
use reviewq_app::decisions::{Checkpoints, DecisionStore};
use reviewq_app::events::EventLog;
use reviewq_app::followups::{FollowupCommands, FollowupStore};
use reviewq_app::history::{HistoryStore, NativeHistoryReader};
use reviewq_app::hosts::{
    ClaudeCompanion, ClaudeStore, ClaudeWorkflowStore, LeadStore, MessageHistoryStore, NativeHost,
    NativeTerminal,
};
use reviewq_app::launch::{LaunchStore, NativeLaunchers, NativeModels};
use reviewq_app::ledger::LedgerStore;
use reviewq_app::notifications::InboxStore;
use reviewq_app::queue::{
    ArchiveStore, QueueStore, ReviewLookup, SprintStore, TrackingStore, WorkspaceQueue,
};
use reviewq_app::reports::{DrawingRenderer, ReportMutationStore, ReportStore};
use reviewq_app::settings::{ConnectionSources, NativeSkills, RuntimeSources, SettingsStore};
use reviewq_app::sources::SourceRefreshStore;
use reviewq_app::workflow::NativeWorkflowProof;
use reviewq_app::workspace::WorkspaceStore;
use reviewq_domain::identity::EventCursor;
use reviewq_protocol::queue::QueueSync;
use reviewq_protocol::{EventCursorDto, Success};
use serde::Serialize;
use tokio::sync::{Notify, Semaphore, watch};
use tower_http::services::{ServeDir, ServeFile};

mod auth;
mod browser_desktop;
mod changes;
mod checkout;
mod control;
mod control_ops;
mod diagrams;
mod document_display;
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
mod tables;
mod targets;
mod terminal;
mod tracking;
mod workspace;

pub use browser_desktop::{BrowserDesktop, BrowserOffer};
pub use control::{BridgeDescriptor, ControlRequest, ServiceEndpoint, ServiceIdentity};
pub use error::{field_error_text, ledger_refusal_text};

use auth::Auth;
use error::ApiError;

/// Shared application ports for one ReviewQ listener pair.
pub trait Store:
    WorkspaceStore
    + QueueStore
    + ArchiveStore
    + SprintStore
    + WorkspaceQueue
    + TrackingStore
    + ReviewLookup
    + EventLog
    + DiscussionEpochStore
    + LeadStore
    + ClaudeStore
    + ClaudeWorkflowStore
    + HistoryStore
    + reviewq_app::hosts::QuestionStore
    + SourceRefreshStore
    + ReportStore
    + ReportMutationStore
    + MessageHistoryStore
    + InboxStore
    + DecisionStore
    + ActionStore
    + ActionCommandStore
    + FollowupStore
    + FollowupCommands
    + SettingsStore
    + LaunchStore
    + LedgerStore
    + reviewq_app::hosts::recovery::RecoveryStore
{
}
impl<
    S: WorkspaceStore
        + QueueStore
        + ArchiveStore
        + SprintStore
        + WorkspaceQueue
        + TrackingStore
        + ReviewLookup
        + EventLog
        + DiscussionEpochStore
        + LeadStore
        + ClaudeStore
        + ClaudeWorkflowStore
        + HistoryStore
        + reviewq_app::hosts::QuestionStore
        + SourceRefreshStore
        + ReportStore
        + ReportMutationStore
        + MessageHistoryStore
        + InboxStore
        + DecisionStore
        + ActionStore
        + ActionCommandStore
        + FollowupStore
        + FollowupCommands
        + SettingsStore
        + LaunchStore
        + LedgerStore
        + reviewq_app::hosts::recovery::RecoveryStore,
> Store for S
{
}

/// Native proof for the workflow routes.
pub struct WorkflowIntegration {
    pub proof: Arc<dyn NativeWorkflowProof>,
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
    sprint_load: tokio::sync::Mutex<()>,
    archive_metadata: workspace::ArchiveCache,
    host: Option<Arc<dyn NativeHost>>,
    claude: Option<Arc<dyn ClaudeCompanion>>,
    claude_interrupt: Option<Arc<dyn reviewq_app::hosts::recovery::NativeJobInterrupt>>,
    file_history: Arc<dyn NativeHistoryReader>,
    history_watches: history::Watchers,
    workflow: WorkflowIntegration,
    drawings: diagrams::Cache,
    tables: tables::Cache,
    terminal: Arc<dyn NativeTerminal>,
    browser_desktop: Option<BrowserDesktop>,
    sends_allowed: bool,
    commits: watch::Receiver<EventCursor>,
    shutdown: watch::Receiver<bool>,
    startup: watch::Receiver<StartupState>,
    event_slots: Arc<Semaphore>,
    auth: Auth,
    checkpoints: Checkpoints,
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
    recovery_complete: bool,
    settings_revision: reviewq_protocol::RevisionDto,
    configured_projects: usize,
    total_mrs: u64,
    sync: QueueSync,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserProof {
    csrf_token: String,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum StartupState {
    Verifying,
    Recovered,
    Operational,
}

/// Runtime-owned listener identity and commit/shutdown signals.
pub struct ServiceRuntime<'a> {
    pub endpoint: ServiceEndpoint,
    pub web: &'a Path,
    pub retained_assets: Option<&'a Path>,
    pub sends_allowed: bool,
    pub source_poll_interval: Option<Duration>,
    pub settings_sources: Arc<dyn ConnectionSources>,
    pub skills: NativeSkills,
    pub models: NativeModels,
    pub launchers: NativeLaunchers,
    pub launches_allowed: bool,
    pub claude: Option<Arc<dyn ClaudeCompanion>>,
    pub claude_interrupt: Option<Arc<dyn reviewq_app::hosts::recovery::NativeJobInterrupt>>,
    pub workflow: WorkflowIntegration,
    pub drawing_renderer: Option<Arc<dyn DrawingRenderer>>,
    pub terminal: Arc<dyn NativeTerminal>,
    pub browser_desktop: Option<BrowserDesktop>,
    pub commits: watch::Receiver<EventCursor>,
    pub shutdown: watch::Receiver<bool>,
    pub startup: watch::Receiver<StartupState>,
}

/// Mounts production assets and the private versioned HTTP routes.
pub fn router(
    store: Arc<dyn Store>,
    source: Arc<RuntimeSources>,
    host: Option<Arc<dyn NativeHost>>,
    file_history: Arc<dyn NativeHistoryReader>,
    runtime: ServiceRuntime<'_>,
) -> Result<Router, AppError> {
    let entry = Arc::new(Entry {
        store,
        source,
        settings_sources: runtime.settings_sources,
        skills: runtime.skills,
        models: runtime.models,
        launchers: runtime.launchers,
        launch_slots: Arc::new(Semaphore::new(if runtime.launches_allowed { 4 } else { 0 })),
        refresh_wakeup: Notify::new(),
        sprint_load: tokio::sync::Mutex::new(()),
        archive_metadata: workspace::ArchiveCache::default(),
        host,
        claude: runtime.claude,
        claude_interrupt: runtime.claude_interrupt,
        file_history,
        history_watches: history::Watchers::default(),
        workflow: runtime.workflow,
        drawings: diagrams::Cache::new(runtime.drawing_renderer, runtime.shutdown.clone()),
        tables: tables::Cache::new(),
        terminal: runtime.terminal,
        browser_desktop: runtime.browser_desktop,
        sends_allowed: runtime.sends_allowed,
        commits: runtime.commits,
        shutdown: runtime.shutdown,
        startup: runtime.startup.clone(),
        event_slots: Arc::new(Semaphore::new(32)),
        auth: Auth::new(runtime.endpoint)?,
        checkpoints: Checkpoints::default(),
    });
    let web = runtime.web;
    if let Some(interval) = runtime.source_poll_interval {
        tokio::spawn(poll_source(
            entry.clone(),
            interval,
            runtime.startup.clone(),
        ));
    }
    let api = Router::new()
        .route("/status", get(status))
        .route("/drawings/{reference}", get(diagrams::read))
        .route("/control/review", post(control_ops::review))
        .route("/control/codex-hooks", get(control_ops::codex_hooks))
        .route(
            "/control/ack",
            post(control_ops::ack).layer(DefaultBodyLimit::max(160 * 1024)),
        )
        .route("/settings", get(settings::read).put(settings::save))
        .route("/settings/test", post(settings::test))
        .route("/settings/catalog", post(settings::catalog))
        .route("/settings/checkout/skills", post(settings::checkout_skills))
        .route("/settings/disconnect", post(settings::disconnect))
        .route("/checkout/grant", post(checkout::grant))
        .route("/events", get(events::feed))
        .route("/session", get(browser_proof))
        .route("/notifications/desktop/channel", get(browser_desktop::open))
        .route("/queue", get(workspace::queue))
        .route("/queue/sync", get(refresh::sync))
        .route("/queue/sprints", post(workspace::load_sprints))
        .route("/queue/refresh", post(refresh::start))
        .route("/queue/refresh/{refresh_id}", get(refresh::status))
        .route("/tracking", post(tracking::track))
        .route("/tickets/{key}", get(tracking::ticket))
        .route("/reviews/resolve", get(pages::resolve))
        .route("/reviews/{review_id}", get(pages::overview))
        .route("/reviews/{review_id}/changes/files", get(changes::files))
        .route(
            "/reviews/{review_id}/changes/revision",
            get(changes::revision),
        )
        .route(
            "/reviews/{review_id}/changes/selected",
            get(changes::selected),
        )
        .route("/reviews/{review_id}/changes/paths", get(changes::paths))
        .route("/reviews/{review_id}/changes/patch", get(changes::patch))
        .route("/reviews/{review_id}/changes/source", get(changes::source))
        .route("/reviews/{review_id}/changes/search", get(changes::search))
        .route(
            "/reviews/{review_id}/changes/notes",
            get(changes::search_notes),
        )
        .route(
            "/reviews/{review_id}/changes/notes/destination",
            get(changes::note_destination),
        )
        .route(
            "/reviews/{review_id}/changes/discussions",
            get(changes::discussions),
        )
        .route(
            "/reviews/{review_id}/changes/approvals",
            get(changes::approvals),
        )
        .route("/runs/{run_id}", get(pages::run))
        .route("/runs", get(workspace::runs))
        .route("/archive", get(workspace::archive))
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
        .route("/sessions/register", post(hosts::register::register))
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
            "/runs/{run_id}/sessions/{session_key}/history/content/window",
            get(history::document),
        )
        .route(
            "/runs/{run_id}/sessions/{session_key}/history/content/markdown",
            get(history::markdown),
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
    let router = Router::new()
        .merge(mcp::mount(entry.clone()))
        .nest("/api/v1", api);
    let assets = ServeDir::new(web.join("assets"));
    let router = if let Some(previous) = runtime.retained_assets {
        router.nest_service("/assets", assets.fallback(ServeDir::new(previous)))
    } else {
        router.nest_service("/assets", assets)
    };
    Ok(router
        .fallback_service(
            ServeDir::new(web).not_found_service(ServeFile::new(web.join("index.html"))),
        )
        .layer(tower_http::timeout::RequestBodyTimeoutLayer::new(
            Duration::from_secs(30),
        ))
        .layer(middleware::from_fn_with_state(entry.clone(), guard))
        .layer(middleware::from_fn_with_state(
            runtime.startup,
            activation_guard,
        ))
        .with_state(entry))
}

async fn activation_guard(
    State(startup): State<watch::Receiver<StartupState>>,
    request: Request,
    next: Next,
) -> Response {
    if *startup.borrow() != StartupState::Operational
        && (request.method() != axum::http::Method::GET || request.uri().path() != "/api/v1/status")
    {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            [(header::CACHE_CONTROL, "no-store")],
            "ReviewQ is starting.",
        )
            .into_response();
    }
    next.run(request).await
}

async fn poll_source(
    entry: Arc<Entry>,
    interval: Duration,
    mut startup: watch::Receiver<StartupState>,
) {
    let mut shutdown = entry.shutdown.clone();
    tokio::select! {
        _ = shutdown.wait_for(|stop| *stop) => return,
        result = startup.wait_for(|state| *state == StartupState::Operational) => {
            if result.is_err() { return; }
        },
    }
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
        return ApiError::from(AppError::Forbidden).into_response();
    }
    let private = request.uri().path().starts_with("/api/") || request.uri().path() == "/mcp";
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static("same-origin"),
    );
    if private {
        headers.insert(
            "cross-origin-resource-policy",
            HeaderValue::from_static("same-origin"),
        );
        headers.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
    }
    if private
        || headers
            .get(header::CONTENT_TYPE)
            .is_some_and(|value| value.as_bytes().starts_with(b"text/html"))
    {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

async fn api_not_found() -> ApiError {
    AppError::NotFound.into()
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
    let queue = entry.store.queue_readiness(Actor::local_operator()).await?;
    let active = entry.active_source().await?;
    if queue.value.settings != active.revision {
        return control_ops::signed(
            &entry,
            &proof,
            axum::http::StatusCode::OK,
            Err::<Success<Status>, _>(AppError::StaleRevision),
        );
    }
    let endpoint = &entry.auth.endpoint;
    let startup = *entry.startup.borrow();
    let response = Success {
        data: Status {
            instance: endpoint.instance(),
            build: env!("CARGO_PKG_VERSION"),
            browser: endpoint.browser().to_string(),
            native: endpoint.native().to_string(),
            identity: endpoint.identity(),
            ready: startup == StartupState::Operational,
            recovery_complete: startup != StartupState::Verifying,
            settings_revision: reviewq_protocol::RevisionDto::new(queue.value.settings.get())
                .ok_or(AppError::Internal)?,
            configured_projects: queue.value.projects,
            total_mrs: queue.value.total_mrs,
            sync: pages::queue_sync(&active, queue.value.sync),
        },
        cursor: EventCursorDto::new(queue.cursor.get()).ok_or(AppError::Internal)?,
    };
    let body = serde_json::to_vec(&response).map_err(|_| AppError::Internal)?;
    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    endpoint.response_proof(&proof, 200, &mut response_headers, &body)?;
    Ok((response_headers, body).into_response())
}

async fn browser_proof(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
) -> Result<Json<Success<BrowserProof>>, ApiError> {
    let body = BrowserProof {
        csrf_token: entry.auth.csrf(&headers)?,
    };
    Ok(Json(pages::success(body, *entry.commits.borrow())?))
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
