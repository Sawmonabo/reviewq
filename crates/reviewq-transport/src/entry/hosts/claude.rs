use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use reviewq_app::access::{Actor, AppError};
use reviewq_app::history::HistorySession;
use reviewq_app::hosts::{ClaudeContext, ClaudeHeartbeat, ClaudeOffer, HostActivity, HostPhase};
use reviewq_domain::session::{HostProvider, LeadTarget, NativeBinding, NativeIdentity};
use reviewq_protocol::Success;
use serde::{Deserialize, Serialize};

use super::ClaudeCommand;
use crate::entry::error::ApiBytes;
use crate::entry::pages::{parse, success};
use crate::entry::{ApiError, Entry};

mod delivery;
mod questions;

pub(in crate::entry) fn routes() -> Router<Arc<Entry>> {
    Router::new()
        .route("/native/claude/heartbeat", post(heartbeat))
        .route("/native/claude/delivery-check", post(delivery::check))
        .route("/native/claude/hook", post(delivery::hook))
        .route("/native/claude/question", post(questions::watch))
        .route("/native/claude/question-finished", post(questions::finish))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Heartbeat {
    context: ClaudeCommand,
    generation: String,
    sequence: u64,
    phase: Option<Phase>,
    has_questions: bool,
    question_id: Option<String>,
    turn_id: Option<String>,
    step: Option<u64>,
    latest_prompt_id: Option<String>,
    latest_prompt_turn_id: Option<String>,
    latest_prompt_manual: bool,
    model: Option<String>,
    effort: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Idle,
    Working,
    Blocked,
    Failed,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HeartbeatResponse {
    protocol_version: u16,
    label: String,
    offer: Option<delivery::Offer>,
}

async fn heartbeat(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<HeartbeatResponse>>, ApiError> {
    let command: Heartbeat = entry.auth.bridge_command(&headers, &bytes, 16_384)?;
    let (lead, _) = verified_context(&entry, command.context).await?;
    let activity = command.phase.map(|phase| HostActivity {
        phase: match phase {
            Phase::Idle => HostPhase::Idle,
            Phase::Working => HostPhase::Working,
            Phase::Blocked => HostPhase::Blocked,
            Phase::Failed => HostPhase::Failed,
        },
        has_questions: command.has_questions,
    });
    let observation = ClaudeHeartbeat {
        generation: command.generation.clone(),
        sequence: command.sequence,
        activity,
        turn: command.turn_id.as_deref().map(parse).transpose()?,
        step: command.step,
        question_id: command.question_id.as_deref().map(parse).transpose()?,
        latest_prompt: command.latest_prompt_id.as_deref().map(parse).transpose()?,
        latest_prompt_turn: command
            .latest_prompt_turn_id
            .as_deref()
            .map(parse)
            .transpose()?,
        latest_prompt_manual: command.latest_prompt_manual,
        model: command.model,
        effort: command.effort,
    };
    let recorded = entry
        .store
        .record_claude_heartbeat(lead.clone(), observation)
        .await?;
    let offer = if entry.sends_allowed {
        entry
            .store
            .offer_claude_delivery(lead, command.generation)
            .await?
            .map(delivery::Offer::from_offer)
            .transpose()?
    } else {
        None
    };
    Ok(Json(success(
        HeartbeatResponse {
            protocol_version: 1,
            label: recorded.value.label,
            offer,
        },
        recorded.cursor,
    )?))
}

/// The registered lead a hook's context names, its context and its binding, read from the
/// store alone: no transcript is read. Fails with `NotFound` when the session leads no
/// review, `PreconditionFailed` for another protocol version and `InvalidRequest` for a
/// context that names a target or an invalid path.
pub(super) async fn registered_lead(
    entry: &Entry,
    command: &ClaudeCommand,
) -> Result<(LeadTarget, ClaudeContext, NativeBinding), AppError> {
    if command.protocol_version != 1 {
        return Err(AppError::PreconditionFailed);
    }
    if command.target.is_some() {
        return Err(AppError::InvalidRequest);
    }
    let context = command.context()?;
    let binding = NativeBinding::new(
        NativeIdentity::new(
            HostProvider::Claude,
            context.profile.clone(),
            context.session_id.clone(),
        ),
        context.cwd.clone(),
    );
    let registered = entry
        .store
        .lookup_native_lead(binding.clone())
        .await?
        .value
        .ok_or(AppError::NotFound)?;
    Ok((registered, context, binding))
}

/// The registered lead a hook's context names and its history session, after proving the
/// session's transcript with the Claude companion. Fails as `registered_lead` does, and with
/// `Forbidden` when the proof names another session or no transcript.
pub(super) async fn verified_context(
    entry: &Entry,
    command: ClaudeCommand,
) -> Result<(LeadTarget, HistorySession), AppError> {
    let (registered, mut context, binding) = registered_lead(entry, &command).await?;
    let mut history = entry
        .store
        .history_session(
            Actor::native_session(binding.identity().clone()),
            registered.run_id().clone(),
            registered.session_key().clone(),
        )
        .await?;
    if context.transcript.is_none() {
        context.transcript = history.value.source.clone();
    }
    let companion = entry.claude.as_ref().ok_or(AppError::Unavailable)?;
    let verified = companion.prove(context).await?;
    if verified.binding != binding || verified.source.is_none() {
        return Err(AppError::Forbidden);
    }
    history.value.source = verified.source;
    Ok((registered, history.value))
}
