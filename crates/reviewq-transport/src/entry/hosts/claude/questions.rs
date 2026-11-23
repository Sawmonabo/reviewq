use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::hosts::{ClaudeQuestionAddress, ClaudeQuestionAnswers, QuestionStatus};
use reviewq_domain::identity::ReportHash;
use reviewq_protocol::Success;
use serde::Deserialize;

use super::{ClaudeCommand, bridge_command, verified_context};
use crate::entry::error::ApiBytes;
use crate::entry::pages::{parse, success};
use crate::entry::{ApiError, Entry};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Address {
    generation: String,
    turn_id: String,
    assistant_id: String,
    call_id: String,
}

impl Address {
    fn checked(self) -> Result<ClaudeQuestionAddress, AppError> {
        if self.generation.len() != 32
            || !self
                .generation
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(AppError::InvalidRequest);
        }
        Ok(ClaudeQuestionAddress {
            generation: self.generation,
            turn: parse(&self.turn_id)?,
            assistant: parse(&self.assistant_id)?,
            call: parse(&self.call_id)?,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Watch {
    context: ClaudeCommand,
    address: Address,
    input: serde_json::Value,
}

pub(super) async fn watch(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<Option<ClaudeQuestionAnswers>>>, ApiError> {
    let command: Watch = bridge_command(&entry, &headers, &bytes, 64 * 1024)?;
    if !entry.sends_allowed {
        return Err(AppError::Unavailable.into());
    }
    let address = command.address.checked()?;
    let (lead, session) = verified_context(&entry, command.context).await?;
    let companion = entry.claude.as_ref().ok_or(AppError::Unavailable)?;
    entry
        .store
        .fence_claude_question(lead.clone(), address.clone(), false)
        .await?;
    let input = serde_json::to_vec(&command.input).map_err(|_| AppError::InvalidRequest)?;
    let receiver = companion.open_question(session.clone(), address.clone(), &input)?;
    if let Err(error) = entry
        .store
        .fence_claude_question(lead.clone(), address.clone(), true)
        .await
    {
        companion.finish_question(&session, &address, QuestionStatus::Unavailable, None);
        return Err(error.into());
    }
    let mut shutdown = entry.shutdown.clone();
    let result = tokio::select! {
        result = receiver => result.ok(),
        _ = shutdown.wait_for(|shutdown| *shutdown) => None,
    };
    if result.is_some() {
        if let Err(error) = entry
            .store
            .fence_claude_question(lead.clone(), address.clone(), false)
            .await
        {
            if companion.finish_question(&session, &address, QuestionStatus::Unavailable, None) {
                let _ = entry.store.fence_claude_question(lead, address, true).await;
            }
            return Err(error.into());
        }
    } else if companion.finish_question(&session, &address, QuestionStatus::Unavailable, None) {
        let _ = entry.store.fence_claude_question(lead, address, true).await;
    }
    Ok(Json(success(result, *entry.commits.borrow())?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Finish {
    context: ClaudeCommand,
    address: Address,
    status: Outcome,
    answer_digest: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Answered,
    Closed,
    Unavailable,
}

pub(super) async fn finish(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<()>>, ApiError> {
    let command: Finish = bridge_command(&entry, &headers, &bytes, 4096)?;
    let address = command.address.checked()?;
    let (lead, session) = verified_context(&entry, command.context).await?;
    let companion = entry.claude.as_ref().ok_or(AppError::Unavailable)?;
    entry
        .store
        .fence_claude_question(lead.clone(), address.clone(), false)
        .await?;
    let status = match command.status {
        Outcome::Answered => QuestionStatus::Answered,
        Outcome::Closed => QuestionStatus::Closed,
        Outcome::Unavailable => QuestionStatus::Unavailable,
    };
    let answer_digest: Option<ReportHash> =
        command.answer_digest.as_deref().map(parse).transpose()?;
    if companion.finish_question(&session, &address, status, answer_digest) {
        entry
            .store
            .fence_claude_question(lead, address, true)
            .await?;
    }
    Ok(Json(success((), *entry.commits.borrow())?))
}
