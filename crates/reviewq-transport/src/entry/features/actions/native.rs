use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::actions::{
    ActionDeclaration, CommentTarget, Intent, declare_publication, reconcile_publication,
};
use reviewq_domain::identity::ReportHash;
use reviewq_domain::session::{HostProvider, LeadTarget};
use reviewq_protocol::Success;
use reviewq_protocol::actions as dto;
use sha2::{Digest, Sha256};

use super::project;
use crate::entry::error::ApiBytes;
use crate::entry::hosts::claude::require_companion;
use crate::entry::pages::{parse, success};
use crate::entry::{ApiError, Entry};

/// Largest declare body, as the route's body limit allows.
pub(in crate::entry::features) const MAX_DECLARE_BODY_BYTES: usize = 80 * 1024;
/// Largest reconcile body, as the API's default body limit allows.
const MAX_RECONCILE_BODY_BYTES: usize = 64 * 1024;

/// A Claude lead's action call comes only from its plugin, which proves it; a Codex lead's
/// comes from ReviewQ's own command in that lead's shell.
async fn from_plugin(
    entry: &Entry,
    headers: &HeaderMap,
    lead: &LeadTarget,
) -> Result<(), AppError> {
    if lead.binding().identity().provider() == HostProvider::Claude {
        require_companion(entry, headers, lead.session_key()).await?;
    }
    Ok(())
}

pub(in crate::entry::features) async fn declare(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<dto::Action>>, ApiError> {
    let command: dto::DeclareCommand =
        entry
            .auth
            .bridge_command(&headers, &bytes, MAX_DECLARE_BODY_BYTES)?;
    let encoded = serde_json::to_vec(&command.declaration).map_err(|_| AppError::Internal)?;
    if encoded.len() > 64 * 1024 {
        return Err(AppError::PayloadTooLarge.into());
    }
    let proof = entry.workflow.proof.as_ref();
    let source = entry.source_reader().await?;
    let lead = entry
        .store
        .run_lead(parse(command.session_key.as_str())?)
        .await?;
    from_plugin(&entry, &headers, &lead).await?;
    let turn = command
        .turn_id
        .as_ref()
        .map(|turn| parse(turn.as_str()))
        .transpose()?;
    let native = proof
        .verify_active_call(
            lead.clone(),
            turn.clone(),
            command.call_id.as_str().to_owned(),
        )
        .await?;
    if native.lead != lead
        || native.call_id != command.call_id.as_str()
        || turn.as_ref().is_some_and(|turn| turn != &native.turn_id)
        || command.request_entry_id.as_ref().is_some_and(|requested| {
            native.request_entry_id.as_ref().map(|id| id.as_str()) != Some(requested.as_str())
        })
    {
        return Err(AppError::NotFound.into());
    }
    let declaration = ActionDeclaration {
        intent: match command.declaration.intent {
            dto::Intent::Approval => Intent::Approval,
            dto::Intent::Comments => Intent::Comments,
            dto::Intent::Both => Intent::Both,
            dto::Intent::Cancel => Intent::Cancel,
        },
        reviewed_sha: parse(command.declaration.reviewed_sha.as_str())?,
        targets: command
            .declaration
            .comments
            .into_iter()
            .map(|target| {
                Ok::<_, AppError>(CommentTarget {
                    hash: parse(target.body_sha256.as_str())?,
                    discussion: target.discussion_id.map(|id| id.as_str().to_owned()),
                })
            })
            .collect::<Result<_, _>>()?,
    };
    let digest = ReportHash::from_sha256(Sha256::digest(encoded).into());
    let stored = declare_publication(
        entry.store.as_ref(),
        proof,
        source.as_ref(),
        native,
        declaration,
        digest,
    )
    .await?;
    Ok(Json(success(project(stored.value)?, stored.cursor)?))
}

pub(in crate::entry::features) async fn reconcile(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<Option<dto::Action>>>, ApiError> {
    let command: dto::ReconcileCommand =
        entry
            .auth
            .bridge_command(&headers, &bytes, MAX_RECONCILE_BODY_BYTES)?;
    let proof = entry.workflow.proof.as_ref();
    let source = entry.source_reader().await?;
    let lead = entry
        .store
        .run_lead(parse(command.session_key.as_str())?)
        .await?;
    from_plugin(&entry, &headers, &lead).await?;
    let verified = proof
        .verify_active_call(lead.clone(), None, command.call_id.as_str().to_owned())
        .await?;
    if verified.lead != lead || verified.call_id != command.call_id.as_str() {
        return Err(AppError::NotFound.into());
    }
    if let Some(stored) =
        reconcile_publication(entry.store.as_ref(), proof, source.as_ref(), verified).await?
    {
        return Ok(Json(success(Some(project(stored.value)?), stored.cursor)?));
    }
    Ok(Json(Success {
        data: None,
        cursor: entry.head().await?,
    }))
}
