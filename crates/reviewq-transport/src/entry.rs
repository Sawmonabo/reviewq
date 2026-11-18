use std::path::Path;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, OriginalUri, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reviewq_app::access::{Actor, AppError};
use reviewq_app::events::EventLog;
use reviewq_app::hosts::LeadStore;
use reviewq_app::queue::{QueueStore, ReviewLookup};
use reviewq_app::sources::{ReviewSourceReader, refresh_queue};
use reviewq_app::workspace::WorkspaceStore;
use reviewq_protocol::{EventCursorDto, Success};
use serde::{Deserialize, Serialize};
use tower_http::services::{ServeDir, ServeFile};

mod auth;
mod control;
mod error;
mod pages;

pub use control::{ControlRequest, ServiceEndpoint};

use auth::Auth;
use error::ApiError;

/// Shared application ports for one ReviewQ listener pair.
pub trait Store: WorkspaceStore + QueueStore + ReviewLookup + EventLog + LeadStore {}
impl<S: WorkspaceStore + QueueStore + ReviewLookup + EventLog + LeadStore> Store for S {}

pub(super) struct Entry {
    store: Arc<dyn Store>,
    source: Option<Arc<dyn ReviewSourceReader>>,
    auth: Auth,
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
    store_application_id: u32,
    store_schema: u32,
    ready: bool,
    configured_projects: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionResult {
    csrf_token: String,
}

/// Mounts production assets and the private versioned HTTP routes.
pub fn router(
    store: Arc<dyn Store>,
    source: Option<Arc<dyn ReviewSourceReader>>,
    endpoint: ServiceEndpoint,
    web: &Path,
) -> Router {
    let entry = Arc::new(Entry {
        store,
        source,
        auth: Auth::new(endpoint),
    });
    let api = Router::new()
        .route("/status", get(status))
        .route("/session", get(session).post(start_session))
        .route("/queue", get(pages::queue))
        .route("/queue/refresh", post(refresh))
        .route("/reviews/resolve", get(pages::resolve))
        .route("/reviews/{review_id}", get(pages::overview))
        .route("/runs/{run_id}", get(pages::run))
        .fallback(api_not_found)
        .layer(DefaultBodyLimit::max(64 * 1024));
    Router::new()
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
    let snapshot = entry
        .store
        .workspace_context(Actor::local_operator())
        .await?;
    let endpoint = &entry.auth.endpoint;
    let response = Success {
        data: Status {
            instance: endpoint.instance(),
            build: env!("CARGO_PKG_VERSION"),
            browser: endpoint.browser().to_string(),
            native: endpoint.native().to_string(),
            store_application_id: 0x52565132,
            store_schema: 1,
            ready: true,
            configured_projects: snapshot.value.projects.len(),
        },
        cursor: EventCursorDto::new(snapshot.cursor.get()).ok_or(AppError::Internal)?,
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
    bytes: Bytes,
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
) -> Result<impl IntoResponse, ApiError> {
    let actor = if headers.contains_key("x-reviewq-control") {
        entry.auth.control(
            "POST",
            uri.path_and_query()
                .ok_or(AppError::InvalidRequest)?
                .as_str(),
            &headers,
            b"",
        )?;
        Actor::local_operator()
    } else {
        entry.auth.browser_actor(&headers, true)?
    };
    let count = refresh_queue(
        entry.store.as_ref(),
        entry.source.as_deref().ok_or(AppError::Unavailable)?,
        actor,
    )
    .await?;
    let cursor = entry.head().await?;
    Ok((
        StatusCode::OK,
        Json(Success {
            data: serde_json::json!({"totalRows": count}),
            cursor,
        }),
    ))
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
