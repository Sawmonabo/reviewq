use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use reviewq_app::access::{Actor, AppError};
use reviewq_app::hosts::{ClaudeHeartbeat, HostActivity, HostPhase};
use reviewq_domain::session::{HostProvider, LeadTarget, NativeBinding, NativeIdentity};
use reviewq_protocol::Success;
use serde::{Deserialize, Serialize};

use super::{ClaudeCommand, bridge_command};
use crate::entry::error::ApiBytes;
use crate::entry::pages::{parse, success};
use crate::entry::{ApiError, Entry};

pub(in crate::entry) fn routes() -> Router<Arc<Entry>> {
    Router::new().route("/native/claude/heartbeat", post(heartbeat))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Heartbeat {
    context: ClaudeCommand,
    generation: String,
    sequence: u64,
    phase: Option<Phase>,
    has_questions: bool,
    turn_id: Option<String>,
    latest_prompt_id: Option<String>,
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
}

async fn heartbeat(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<HeartbeatResponse>>, ApiError> {
    let command: Heartbeat = bridge_command(&entry, &headers, &bytes, 16_384)?;
    let lead = verified_context(&entry, command.context).await?;
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
        generation: command.generation,
        sequence: command.sequence,
        activity,
        turn: command.turn_id.as_deref().map(parse).transpose()?,
        latest_prompt: command.latest_prompt_id.as_deref().map(parse).transpose()?,
        model: command.model,
        effort: command.effort,
    };
    let recorded = entry
        .store
        .record_claude_heartbeat(lead, observation)
        .await?;
    Ok(Json(success(
        HeartbeatResponse {
            protocol_version: 1,
            label: recorded.value.label,
        },
        recorded.cursor,
    )?))
}

async fn verified_context(entry: &Entry, command: ClaudeCommand) -> Result<LeadTarget, AppError> {
    if command.protocol_version != 1 {
        return Err(AppError::PreconditionFailed);
    }
    if command.target.is_some() {
        return Err(AppError::InvalidRequest);
    }
    let mut context = command.context()?;
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
    let history = entry
        .store
        .history_session(
            Actor::native_session(binding.identity().clone()),
            registered.run_id().clone(),
            registered.session_key().clone(),
        )
        .await?;
    if context.transcript.is_none() {
        context.transcript = history.value.source;
    }
    let companion = entry.claude.as_ref().ok_or(AppError::Unavailable)?;
    let verified = companion.prove(context).await?;
    if verified.binding != binding || verified.source.is_none() {
        return Err(AppError::Forbidden);
    }
    Ok(registered)
}
