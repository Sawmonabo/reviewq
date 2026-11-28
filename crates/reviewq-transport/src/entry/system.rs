//! The System page routes: what is running, starting at login, quitting and uninstalling.
//! Each change is a browser mutation; the installed-mode and install-lock rules belong to the
//! `SystemControl` implementation, whose refusals reach the page as `409` with its sentence.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::system::{SystemControl, SystemMode, SystemRefusal};
use reviewq_protocol::UtcTimestampDto;
use reviewq_protocol::system as dto;
use serde::de::DeserializeOwned;

use super::error::ApiBytes;
use super::pages::{count, success};
use super::{ApiError, Entry};

/// The page's one door marker. Only the installed service turns the Start button on.
const DOOR_OFF: &str = "data-door=\"off\"";
const DOOR_ON: &str = "data-door=\"on\"";

pub(super) fn routes() -> Router<Arc<Entry>> {
    Router::new()
        .route("/system", get(facts))
        .route("/system/quit", post(quit))
        .route("/system/login-start", post(login_start))
        .route("/system/uninstall", post(uninstall))
}

async fn facts(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
) -> Result<Json<dto::SystemFactsResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let system = port(&entry)?;
    // A source that cannot load, such as one with a broken saved token, leaves only the
    // last refresh unknown, so Quit and Uninstall stay on the page.
    let sync = async {
        let active = entry.active_source().await.ok()?;
        entry.store.refresh_sync(actor, active.revision).await.ok()
    };
    let (facts, sync) = tokio::join!(system.facts(), sync);
    let facts = facts?;
    let (last_refresh_at, cursor) = match sync {
        Some(sync) => (
            sync.value
                .last_success_at_ms
                .and_then(UtcTimestampDto::from_unix_milliseconds),
            sync.cursor,
        ),
        None => (None, *entry.commits.borrow()),
    };
    let data = dto::SystemFacts {
        version: env!("CARGO_PKG_VERSION").into(),
        mode: match system.mode() {
            SystemMode::Installed => dto::SystemMode::Installed,
            SystemMode::Foreground => dto::SystemMode::Foreground,
            SystemMode::Validation => dto::SystemMode::Validation,
        },
        address: entry.auth.origin.clone(),
        data_folder: facts.data_folder,
        data_bytes: facts.data_bytes.map(count).transpose()?,
        last_refresh_at,
        start_at_login: facts.start_at_login,
    };
    Ok(Json(success(data, cursor)?))
}

async fn quit(State(entry): State<Arc<Entry>>, headers: HeaderMap) -> Result<Response, ApiError> {
    entry.auth.browser_actor(&headers, true)?;
    decided(port(&entry)?.quit()).await?;
    Ok(StatusCode::ACCEPTED.into_response())
}

async fn login_start(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<dto::LoginStartResponse>, ApiError> {
    entry.auth.browser_actor(&headers, true)?;
    let system = port(&entry)?;
    let input: dto::LoginStartRequest = read(&bytes)?;
    let start_at_login = decided(system.set_start_at_login(input.enabled)).await?;
    Ok(Json(success(
        dto::LoginStart { start_at_login },
        *entry.commits.borrow(),
    )?))
}

async fn uninstall(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Response, ApiError> {
    entry.auth.browser_actor(&headers, true)?;
    let system = port(&entry)?;
    let input: dto::UninstallRequest = read(&bytes)?;
    decided(system.uninstall(input.delete_data)).await?;
    Ok(StatusCode::ACCEPTED.into_response())
}

/// `GET /stopped.html`: the page shown while ReviewQ is stopped, read from the build. In the
/// installed service it carries `data-door="on"` in place of its one `data-door="off"`; nothing
/// else in it changes. Without the file, the answer is `404`.
pub(super) async fn stopped_page(State(entry): State<Arc<Entry>>) -> Response {
    let Ok(page) = tokio::fs::read_to_string(&entry.stopped_page).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let installed = entry
        .system
        .as_deref()
        .is_some_and(|system| system.mode() == SystemMode::Installed);
    let page = if installed && page.matches(DOOR_OFF).count() == 1 {
        page.replacen(DOOR_OFF, DOOR_ON, 1)
    } else {
        page
    };
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], page).into_response()
}

/// The wired `SystemControl`, or `Unavailable` when this listener has none.
fn port(entry: &Entry) -> Result<&dyn SystemControl, AppError> {
    entry.system.as_deref().ok_or(AppError::Unavailable)
}

/// The value of a change the port made, or its refusal as the page's `409`.
async fn decided<T>(change: AppFuture<'_, Result<T, SystemRefusal>>) -> Result<T, ApiError> {
    change
        .await?
        .map_err(|refusal| ApiError::Refused(refusal.to_string()))
}

fn read<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, AppError> {
    serde_json::from_slice(bytes).map_err(|_| AppError::InvalidRequest)
}
