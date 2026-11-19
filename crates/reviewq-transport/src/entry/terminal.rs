use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::hosts::{TerminalHandoff, TerminalInstruction};
use reviewq_domain::session::HostProvider;
use reviewq_protocol::{
    HostProviderDto, ResourceIdDto, RunIdDto, SessionKeyDto, TerminalHandoff as HandoffDto,
    TerminalResponse, TerminalResult,
};

use super::error::ApiPath as Path;
use super::pages::{parse, success};
use super::{ApiError, Entry};

pub(super) async fn read(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session)): Path<(String, String)>,
) -> Result<Json<TerminalResponse>, ApiError> {
    resolve(&entry, &headers, run, session, false).await
}

pub(super) async fn open(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session)): Path<(String, String)>,
) -> Result<Json<TerminalResponse>, ApiError> {
    resolve(&entry, &headers, run, session, true).await
}

async fn resolve(
    entry: &Entry,
    headers: &HeaderMap,
    run: String,
    session: String,
    open: bool,
) -> Result<Json<TerminalResponse>, ApiError> {
    let actor = entry.auth.browser_actor(headers, open)?;
    let found = entry
        .store
        .history_session(actor, parse(&run)?, parse(&session)?)
        .await?;
    let identity = found.value.binding.identity();
    let provider = match identity.provider() {
        HostProvider::Codex => HostProviderDto::Codex,
        HostProvider::Claude => HostProviderDto::Claude,
    };
    let native_id = ResourceIdDto::new(identity.native_id().as_str()).ok_or(AppError::Internal)?;
    let handoff = match entry.terminal.terminal(found.value, open).await? {
        TerminalHandoff::Command { command, opened } => HandoffDto::Command { command, opened },
        TerminalHandoff::Instruction(instruction) => match instruction {
            TerminalInstruction::RestoreCheckout => HandoffDto::RestoreCheckout,
            TerminalInstruction::ConfigureProvider => HandoffDto::ConfigureProvider,
            TerminalInstruction::ForegroundClaude => HandoffDto::ForegroundClaude,
            TerminalInstruction::InspectParent => HandoffDto::InspectParent,
        },
    };
    Ok(Json(success(
        TerminalResult {
            run_id: RunIdDto::new(run).ok_or(AppError::Internal)?,
            session_key: SessionKeyDto::new(session).ok_or(AppError::Internal)?,
            provider,
            native_id,
            handoff,
        },
        found.cursor,
    )?))
}
