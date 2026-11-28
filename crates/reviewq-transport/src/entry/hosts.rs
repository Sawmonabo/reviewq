use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use reviewq_app::access::AppError;
use reviewq_app::hosts::{
    ClaudeContext, HookApproval, NativeHost, NativeLink, link_codex, link_proven,
};
use reviewq_app::ledger::readback::RULE;
use reviewq_domain::session::{AbsolutePath, LeadTarget};
use reviewq_protocol::queue::ReviewSelection;
use reviewq_protocol::{EventCursorDto, Failure, ReviewIdDto, RunIdDto, SessionKeyDto, Success};
use serde::{Deserialize, Serialize};

use super::error::ApiBytes;
use super::{ApiError, Entry};

pub(super) mod claude;
pub(super) mod register;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LinkCommand {
    protocol_version: u16,
    thread_id: String,
    target: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CodexAddress {
    protocol_version: u16,
    thread_id: String,
}

#[derive(Serialize)]
struct AmbiguousTarget {
    #[serde(flatten)]
    failure: Failure,
    candidates: Vec<String>,
}

pub(super) async fn link(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Response, ApiError> {
    let command: LinkCommand = entry.auth.bridge_command(&headers, &bytes, 4_096)?;
    protocol(command.protocol_version)?;
    let source = entry.source_reader().await?;
    let host = entry.host.as_deref().ok_or(AppError::Unavailable)?;
    let thread = command
        .thread_id
        .parse()
        .map_err(|_| AppError::InvalidRequest)?;
    let linked = link_codex(
        entry.store.as_ref(),
        source.as_ref(),
        host,
        thread,
        command.target,
    )
    .await?;
    let notice = match linked {
        NativeLink::Linked(_) => codex_hooks_notice(&entry, host).await?,
        NativeLink::Ambiguous(_) => None,
    };
    link_response(linked, notice)
}

pub(super) async fn codex_registration(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<ReviewSelection>>, ApiError> {
    let input: CodexAddress = entry.auth.bridge_command(&headers, &bytes, 4096)?;
    protocol(input.protocol_version)?;
    let host = entry.host.as_deref().ok_or(AppError::Unavailable)?;
    let proof = host
        .prove(
            input
                .thread_id
                .parse()
                .map_err(|_| AppError::InvalidRequest)?,
        )
        .await?;
    let registered = entry.store.lookup_native_lead(proof.binding).await?;
    let lead = registered.value.ok_or(AppError::NotFound)?;
    Ok(Json(super::pages::success(
        selection(&lead)?,
        registered.cursor,
    )?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ClaudeCommand {
    protocol_version: u16,
    session_id: String,
    profile: std::path::PathBuf,
    cwd: std::path::PathBuf,
    transcript: Option<std::path::PathBuf>,
    target: Option<String>,
}

impl ClaudeCommand {
    fn context(&self) -> Result<ClaudeContext, AppError> {
        fn absolute(path: std::path::PathBuf) -> Result<AbsolutePath, AppError> {
            AbsolutePath::from_canonical(path).map_err(|_| AppError::InvalidRequest)
        }
        Ok(ClaudeContext {
            session_id: self
                .session_id
                .parse()
                .map_err(|_| AppError::InvalidRequest)?,
            profile: absolute(self.profile.clone())?,
            cwd: absolute(self.cwd.clone())?,
            transcript: self.transcript.clone().map(absolute).transpose()?,
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeRegistration {
    protocol_version: u16,
    registration: Option<ReviewSelection>,
    pending_launch: bool,
}

pub(super) async fn claude_registration(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Response, ApiError> {
    let command: ClaudeCommand = entry.auth.bridge_command(&headers, &bytes, 16_384)?;
    protocol(command.protocol_version)?;
    if command.target.is_some() {
        return Err(AppError::InvalidRequest.into());
    }
    let context = command.context()?;
    let companion = entry.claude.as_ref().ok_or(AppError::Unavailable)?;
    if !companion.accepts_profile(&context.profile) {
        return Err(AppError::Forbidden.into());
    }
    let registered = match claude::verified_context(&entry, command).await {
        Ok((lead, _)) => Some(
            entry
                .store
                .lookup_native_lead(lead.binding().clone())
                .await?,
        ),
        Err(AppError::NotFound) => None,
        Err(error) => return Err(error.into()),
    };
    let (registration, cursor) = match registered {
        Some(value) => (
            value.value.as_ref().map(selection).transpose()?,
            value.cursor,
        ),
        None => (None, *entry.commits.borrow()),
    };
    let pending_launch =
        registration.is_none() && entry.store.pending_claude_launch(context).await?;
    Ok(Json(Success {
        data: ClaudeRegistration {
            protocol_version: 1,
            registration,
            pending_launch,
        },
        cursor: EventCursorDto::new(cursor.get()).ok_or(AppError::Internal)?,
    })
    .into_response())
}

pub(super) async fn claude_link(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Response, ApiError> {
    let command: ClaudeCommand = entry.auth.bridge_command(&headers, &bytes, 16_384)?;
    protocol(command.protocol_version)?;
    let companion = entry.claude.as_deref().ok_or(AppError::Unavailable)?;
    let source = entry.source_reader().await?;
    let proof = companion.prove(command.context()?).await?;
    let target = command.target.ok_or(AppError::InvalidRequest)?;
    claude::require_companion_to_link(&entry, &headers, &proof.binding).await?;
    let linked = link_proven(entry.store.as_ref(), source.as_ref(), proof, target).await?;
    link_response(linked, None)
}

fn protocol(version: u16) -> Result<(), ApiError> {
    if version != 1 {
        return Err(AppError::PreconditionFailed.into());
    }
    Ok(())
}

/// The reply to a successful link: the linked review, the rule the model reads once linked,
/// and for Codex what to tell the person about its hooks, when anything.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Linked {
    #[serde(flatten)]
    selection: ReviewSelection,
    rule: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    hooks_notice: Option<HooksNotice>,
}

/// What the person needs to know about ReviewQ's Codex hooks after a link.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum HooksNotice {
    /// Codex does not trust them, and could not be made to.
    Pending,
    /// Codex trusts them, but no Codex stop hook has reported on this installation yet, so a
    /// Codex session opened before they were trusted does not run them.
    Restart,
}

/// Answers a link: `Linked` for one review, or `409` with the candidates when the target named
/// several.
fn link_response(
    result: NativeLink,
    hooks_notice: Option<HooksNotice>,
) -> Result<Response, ApiError> {
    let registered = match result {
        NativeLink::Linked(registered) => registered,
        NativeLink::Ambiguous(candidates) => {
            let (_, failure) = super::error::failure(AppError::Conflict);
            return Ok((
                StatusCode::CONFLICT,
                Json(AmbiguousTarget {
                    failure,
                    candidates,
                }),
            )
                .into_response());
        }
    };
    let response = Success {
        data: Linked {
            selection: selection(&registered.value)?,
            rule: RULE,
            hooks_notice,
        },
        cursor: EventCursorDto::new(registered.cursor.get()).ok_or(AppError::Internal)?,
    };
    Ok(Json(response).into_response())
}

/// What a Codex link tells the person about ReviewQ's hooks, after asking `host` to trust
/// them: `Pending` when Codex does not trust them, whatever reported before, and `Restart`
/// when it does but no stop hook has ever reported.
async fn codex_hooks_notice(
    entry: &Entry,
    host: &dyn NativeHost,
) -> Result<Option<HooksNotice>, AppError> {
    let trusted = matches!(
        host.approve_hooks().await,
        Ok(HookApproval::Approved(_) | HookApproval::AlreadyTrusted)
    );
    if !trusted {
        return Ok(Some(HooksNotice::Pending));
    }
    Ok((!entry.store.codex_stop_hook_reported().await?).then_some(HooksNotice::Restart))
}

fn selection(target: &LeadTarget) -> Result<ReviewSelection, AppError> {
    Ok(ReviewSelection::WithLead {
        review_id: ReviewIdDto::new(target.review_id().as_str()).ok_or(AppError::Internal)?,
        run_id: RunIdDto::new(target.run_id().as_str()).ok_or(AppError::Internal)?,
        lead_session_key: SessionKeyDto::new(target.session_key().as_str())
            .ok_or(AppError::Internal)?,
    })
}
