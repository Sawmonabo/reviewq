use std::path::Path;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, OriginalUri, Request, State};
use axum::http::{HeaderMap, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reviewq_app::access::{Actor, AppError};
use reviewq_app::actions::ActionStore;
use reviewq_app::decisions::{Checkpoints, DecisionStore};
use reviewq_app::events::EventLog;
use reviewq_app::followups::FollowupStore;
use reviewq_app::history::{HistoryStore, NativeHistoryReader};
use reviewq_app::hosts::{ClaudeCompanion, LeadStore, MessageHistoryStore, NativeHost};
use reviewq_app::notifications::InboxStore;
use reviewq_app::queue::{QueueStore, ReviewLookup};
use reviewq_app::reports::{ReportCaptureStore, ReportMutationStore, ReportStore};
use reviewq_app::sources::{ReviewSourceReader, SourceRefreshStore, refresh_queue};
use reviewq_app::workflow::NativeWorkflowProof;
use reviewq_app::workspace::WorkspaceStore;
use reviewq_domain::identity::EventCursor;
use reviewq_protocol::queue::QueueSync;
use reviewq_protocol::{EventCursorDto, Success};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Semaphore, watch};
use tower_http::services::{ServeDir, ServeFile};

mod auth;
mod changes;
mod checkout;
mod control;
mod error;
mod events;
mod features;
mod history;
mod hosts;
mod mcp;
mod messages;
mod pages;

pub use control::{ControlRequest, ServiceEndpoint, ServiceIdentity};

use auth::Auth;
use error::{ApiBytes, ApiError};

/// Shared application ports for one ReviewQ listener pair.
pub trait Store:
    WorkspaceStore
    + QueueStore
    + ReviewLookup
    + EventLog
    + LeadStore
    + HistoryStore
    + SourceRefreshStore
    + ReportStore
    + ReportMutationStore
    + ReportCaptureStore
    + MessageHistoryStore
    + InboxStore
    + DecisionStore
    + ActionStore
    + FollowupStore
{
}
impl<
    S: WorkspaceStore
        + QueueStore
        + ReviewLookup
        + EventLog
        + LeadStore
        + HistoryStore
        + SourceRefreshStore
        + ReportStore
        + ReportMutationStore
        + ReportCaptureStore
        + MessageHistoryStore
        + InboxStore
        + DecisionStore
        + ActionStore
        + FollowupStore,
> Store for S
{
}

type RefreshResult = Option<Result<usize, AppError>>;

pub(super) struct Entry {
    store: Arc<dyn Store>,
    source: Option<Arc<dyn ReviewSourceReader>>,
    host: Option<Arc<dyn NativeHost>>,
    claude: Option<Arc<dyn ClaudeCompanion>>,
    file_history: Arc<dyn NativeHistoryReader>,
    workflow: Option<Arc<dyn NativeWorkflowProof>>,
    sends_allowed: bool,
    commits: watch::Receiver<EventCursor>,
    shutdown: watch::Receiver<bool>,
    event_slots: Arc<Semaphore>,
    auth: Auth,
    refresh: Mutex<Option<watch::Receiver<RefreshResult>>>,
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
    pub claude: Option<Arc<dyn ClaudeCompanion>>,
    pub workflow: Option<Arc<dyn NativeWorkflowProof>>,
    pub commits: watch::Receiver<EventCursor>,
    pub shutdown: watch::Receiver<bool>,
}

/// Mounts production assets and the private versioned HTTP routes.
pub fn router(
    store: Arc<dyn Store>,
    source: Option<Arc<dyn ReviewSourceReader>>,
    host: Option<Arc<dyn NativeHost>>,
    file_history: Arc<dyn NativeHistoryReader>,
    runtime: ServiceRuntime<'_>,
) -> Router {
    let entry = Arc::new(Entry {
        store,
        source,
        host,
        claude: runtime.claude,
        file_history,
        workflow: runtime.workflow,
        sends_allowed: runtime.sends_allowed,
        commits: runtime.commits,
        shutdown: runtime.shutdown,
        event_slots: Arc::new(Semaphore::new(32)),
        auth: Auth::new(runtime.endpoint),
        refresh: Mutex::new(None),
        checkpoints: Checkpoints::default(),
    });
    let web = runtime.web;
    let api = Router::new()
        .route("/status", get(status))
        .route("/checkout/grant", post(checkout::grant))
        .route("/events", get(events::feed))
        .route("/session", get(session).post(start_session))
        .route("/queue", get(pages::queue))
        .route("/queue/refresh", post(refresh))
        .route("/reviews/resolve", get(pages::resolve))
        .route("/reviews/{review_id}", get(pages::overview))
        .route("/reviews/{review_id}/changes/files", get(changes::files))
        .route("/reviews/{review_id}/changes/patch", get(changes::patch))
        .route(
            "/reviews/{review_id}/changes/discussions",
            get(changes::discussions),
        )
        .route("/runs/{run_id}", get(pages::run))
        .route("/native/codex/link", post(hosts::link))
        .route("/native/claude/link", post(hosts::claude_link))
        .route(
            "/native/claude/registration",
            post(hosts::claude_registration),
        )
        .merge(features::routes())
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
            total_mrs: queue.value.total,
            sync: pages::queue_sync(entry.source.is_some(), queue.value.sync),
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

async fn refresh(
    State(entry): State<Arc<Entry>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let proof = if headers.contains_key("x-reviewq-control") {
        Some(
            entry.auth.control(
                "POST",
                uri.path_and_query()
                    .ok_or(AppError::InvalidRequest)?
                    .as_str(),
                &headers,
                b"",
            )?,
        )
    } else {
        entry.auth.browser_actor(&headers, true)?;
        None
    };
    let result = entry.refresh().await;
    let (status, body) = match result {
        Ok(count) => (
            200,
            serde_json::to_vec(&Success {
                data: serde_json::json!({"totalRows": count}),
                cursor: entry.head().await?,
            })
            .map_err(|_| AppError::Internal)?,
        ),
        Err(error) => {
            let (status, failure) = error::failure(error);
            (
                status.as_u16(),
                serde_json::to_vec(&failure).map_err(|_| AppError::Internal)?,
            )
        }
    };
    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    if let Some(proof) = proof {
        entry
            .auth
            .endpoint
            .response_proof(&proof, status, &mut response_headers, &body)?;
    }
    Ok((
        axum::http::StatusCode::from_u16(status).map_err(|_| AppError::Internal)?,
        response_headers,
        body,
    )
        .into_response())
}

impl Entry {
    async fn refresh(self: &Arc<Self>) -> Result<usize, AppError> {
        let source = self.source.clone().ok_or(AppError::Unavailable)?;
        let mut receiver = {
            let mut active = self.refresh.lock().await;
            if let Some(receiver) = active
                .as_ref()
                .filter(|receiver| receiver.borrow().is_none())
            {
                receiver.clone()
            } else {
                let (send, receive) = watch::channel(None);
                let store = self.store.clone();
                tokio::spawn(async move {
                    let result =
                        refresh_queue(store.as_ref(), source.as_ref(), Actor::local_operator())
                            .await;
                    send.send_replace(Some(result));
                });
                *active = Some(receive.clone());
                receive
            }
        };
        loop {
            if let Some(result) = *receiver.borrow_and_update() {
                return result;
            }
            receiver.changed().await.map_err(|_| AppError::Internal)?;
        }
    }

    async fn head(&self) -> Result<EventCursorDto, AppError> {
        let snapshot = self
            .store
            .workspace_context(Actor::local_operator())
            .await?;
        EventCursorDto::new(snapshot.cursor.get()).ok_or(AppError::Internal)
    }
}
