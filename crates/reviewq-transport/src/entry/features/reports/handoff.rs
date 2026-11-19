use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use reviewq_app::access::AppError;
use reviewq_app::reports::{
    self as app, Declaration, ReportReference, RevisionEvidence, Source, Verdict,
};
use reviewq_app::workflow::WorkflowTool;
use reviewq_domain::identity::ReportHash;
use reviewq_domain::session::{AbsolutePath, HostProvider};
use reviewq_protocol::Success;
use reviewq_protocol::reports as dto;
use sha2::{Digest, Sha256};

use super::{hash, resource};
use crate::entry::error::ApiBytes;
use crate::entry::pages::{parse, success};
use crate::entry::{ApiError, Entry};

pub(in crate::entry::features) async fn accepted(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<Option<dto::AcceptedReport>>>, ApiError> {
    authenticate(&entry, &headers)?;
    let command: dto::NativeReportAddress =
        serde_json::from_slice(&bytes).map_err(|_| AppError::InvalidRequest)?;
    let lead = entry
        .store
        .report_lead(parse(command.session_key.as_str())?)
        .await?;
    let stored = entry.store.accepted_report(lead).await?;
    let accepted = stored
        .value
        .map(|value| {
            let source = match value.source {
                Some(Source::File {
                    path,
                    hash: declared,
                }) => Some(dto::ReportSource::File {
                    path: path
                        .as_path()
                        .to_str()
                        .ok_or(AppError::Unavailable)?
                        .to_owned(),
                    sha256: declared
                        .as_ref()
                        .map(|value| hash(value.as_str()))
                        .transpose()?,
                }),
                Some(Source::Response) => Some(dto::ReportSource::Response),
                None => None,
            };
            Ok::<_, AppError>(dto::AcceptedReport {
                receipt_id: resource(value.reference.receipt_id.as_str())?,
                sha256: hash(value.reference.hash.as_str())?,
                source,
            })
        })
        .transpose()?;
    Ok(Json(success(accepted, stored.cursor)?))
}

pub(in crate::entry::features) async fn handoff(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<dto::HandoffReceipt>>, ApiError> {
    receive(&entry, &headers, &bytes, false).await
}

pub(in crate::entry::features) async fn handoff_status(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<dto::HandoffReceipt>>, ApiError> {
    receive(&entry, &headers, &bytes, true).await
}

async fn receive(
    entry: &Entry,
    headers: &HeaderMap,
    bytes: &[u8],
    lookup: bool,
) -> Result<Json<Success<dto::HandoffReceipt>>, ApiError> {
    authenticate(entry, headers)?;
    let proof = entry.workflow.as_deref().ok_or(AppError::Unavailable)?;
    let command: dto::HandoffCommand =
        serde_json::from_slice(bytes).map_err(|_| AppError::InvalidRequest)?;
    let (session, turn, call_id, expected, handoff) = match command {
        dto::HandoffCommand::Codex(value) => (
            value.session_key,
            None,
            value.call_id,
            HostProvider::Codex,
            value.handoff,
        ),
        dto::HandoffCommand::Companion(value) => (
            value.context.session_key,
            Some(parse(value.context.turn_id.as_str())?),
            value.context.call_id,
            HostProvider::Claude,
            value.handoff,
        ),
    };
    let canonical = serde_json::to_vec(&handoff).map_err(|_| AppError::Internal)?;
    if canonical.len() > 128 * 1024 {
        return Err(AppError::PayloadTooLarge.into());
    }
    let digest = ReportHash::from_sha256(Sha256::digest(canonical).into());
    let declaration = declaration(handoff, expected)?;
    declaration.validate()?;
    let lead = entry.store.report_lead(parse(session.as_str())?).await?;
    if lead.binding().identity().provider() != expected {
        return Err(AppError::NotFound.into());
    }
    let verified = proof
        .verify_active_call(
            lead.clone(),
            turn.clone(),
            call_id.as_str().to_owned(),
            WorkflowTool::Report,
        )
        .await?;
    if verified.lead != lead
        || verified.call_id != call_id.as_str()
        || turn.is_some_and(|turn| verified.turn_id != turn)
        || verified.tool != WorkflowTool::Report
    {
        return Err(AppError::NotFound.into());
    }
    let stored = if lookup {
        entry.store.report_handoff_status(verified, digest).await?
    } else {
        entry
            .store
            .stage_report(verified, declaration, digest)
            .await?
    };
    Ok(Json(success(
        dto::HandoffReceipt {
            receipt_id: resource(stored.value.receipt_id.as_str())?,
            declaration_sha256: hash(stored.value.declaration_sha256.as_str())?,
            state: match stored.value.state {
                app::HandoffState::PendingCompletion => dto::HandoffState::PendingCompletion,
                app::HandoffState::Recorded => dto::HandoffState::Recorded,
                app::HandoffState::Failed => dto::HandoffState::Failed,
            },
        },
        stored.cursor,
    )?))
}

fn authenticate(entry: &Entry, headers: &HeaderMap) -> Result<(), AppError> {
    entry.auth.endpoint.verify_bridge(headers)?;
    if !headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(';').next() == Some("application/json"))
    {
        return Err(AppError::InvalidRequest);
    }
    Ok(())
}

fn declaration(input: dto::Handoff, provider: HostProvider) -> Result<Declaration, AppError> {
    Ok(match input {
        dto::Handoff::Final {
            source,
            revision,
            verdict,
            amends,
        } => Declaration::Final {
            source: project_source(source, provider)?,
            revision: match (revision.sha, revision.reason) {
                (Some(sha), None) => RevisionEvidence::Exact(parse(sha.as_str())?),
                (None, Some(reason)) => RevisionEvidence::Unknown(reason),
                _ => return Err(AppError::InvalidRequest),
            },
            verdict: match verdict {
                dto::ReportVerdict::Approve => Verdict::Approve,
                dto::ReportVerdict::RequestChanges => Verdict::RequestChanges,
                dto::ReportVerdict::Blocked => Verdict::Blocked,
                dto::ReportVerdict::NotSpecified => Verdict::NotSpecified,
            },
            amends: amends
                .map(|base| -> Result<_, AppError> {
                    Ok(ReportReference {
                        receipt_id: parse(base.receipt_id.as_str())?,
                        hash: parse(base.sha256.as_str())?,
                    })
                })
                .transpose()?,
        },
        dto::Handoff::Unchanged { accepted, source } => Declaration::Unchanged {
            accepted: ReportReference {
                receipt_id: parse(accepted.receipt_id.as_str())?,
                hash: parse(accepted.sha256.as_str())?,
            },
            source: project_source(source, provider)?,
        },
        dto::Handoff::NeedsInput { reason } => Declaration::NeedsInput { reason },
    })
}

fn project_source(source: dto::ReportSource, provider: HostProvider) -> Result<Source, AppError> {
    Ok(match source {
        dto::ReportSource::Response => Source::Response,
        dto::ReportSource::File { path, sha256 } => {
            if path.len() > 4096
                || path.contains('\0')
                || (sha256.is_none() && provider == HostProvider::Codex)
            {
                return Err(AppError::InvalidRequest);
            }
            Source::File {
                path: AbsolutePath::from_canonical(PathBuf::from(path))
                    .map_err(|_| AppError::InvalidRequest)?,
                hash: sha256.map(|value| parse(value.as_str())).transpose()?,
            }
        }
    })
}
