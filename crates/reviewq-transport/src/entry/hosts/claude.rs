use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use reviewq_app::access::{Actor, AppError};
use reviewq_app::history::HistorySession;
use reviewq_app::hosts::{
    ClaudeContext, ClaudeHeartbeat, ClaudeHost, ClaudeOffer, HostActivity, HostPhase,
};
use reviewq_domain::identity::SessionKey;
use reviewq_domain::session::{HostProvider, LeadTarget, NativeBinding, NativeIdentity};
use reviewq_protocol::Success;
use serde::{Deserialize, Serialize};

use super::ClaudeCommand;
use crate::entry::control::companion_proof;
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
    /// Added by the heartbeat helper, not the plugin: the process hosting the session.
    host: Option<Host>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Host {
    pid: u32,
    start_time: u64,
    boot_id: String,
    pid_namespace: String,
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
    let proof = companion_proof(&headers)?.ok_or(AppError::Unauthorized)?;
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
        host: command.host.map(|host| ClaudeHost {
            pid: host.pid,
            start_time: host.start_time,
            boot_id: host.boot_id,
            pid_namespace: host.pid_namespace,
        }),
        proof,
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

/// Admits a request that only the Claude plugin's module can make for `session`: one carrying
/// the proof its companion's generation registered with its first heartbeat. Every request of
/// the module passes here but the registration, the link and the heartbeat, which registers and
/// matches the proof itself. The plugin's command hooks run outside the module and carry none.
pub(in crate::entry) async fn require_companion(
    entry: &Entry,
    headers: &HeaderMap,
    session: &SessionKey,
) -> Result<(), AppError> {
    let proof = companion_proof(headers)?;
    entry
        .store
        .prove_claude_companion(session.clone(), proof, false)
        .await
}

/// Admits a link of the session `binding` names: while its companion is live or unknown, only a
/// link carrying that companion's proof, so another process cannot move the session to another
/// review. A session whose companion is gone, or that has none yet, links as it is.
pub(in crate::entry) async fn require_companion_to_link(
    entry: &Entry,
    headers: &HeaderMap,
    binding: &NativeBinding,
) -> Result<(), AppError> {
    let identity = binding.identity();
    let session = identity
        .provider()
        .session_key(identity.native_id())
        .map_err(|_| AppError::InvalidRequest)?;
    entry
        .store
        .prove_claude_companion(session, companion_proof(headers)?, true)
        .await
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
/// session with the Claude companion: from its transcript, or by where the transcript belongs
/// while Claude Code has not written it. Fails as `registered_lead` does, with `NotFound` for
/// a transcript of another conversation or folder, and with `Forbidden` for a transcript
/// outside the profile's project folders or reached through a symlink.
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
