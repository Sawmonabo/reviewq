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
    let workflow = &entry.workflow;
    let proof = workflow.proof.as_ref();
    let command: dto::HandoffCommand =
        serde_json::from_slice(bytes).map_err(|_| AppError::InvalidRequest)?;
    let (session, turn, call_id, expected, mut handoff) = match command {
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
    let request_sha256 = ReportHash::from_sha256(Sha256::digest(canonical).into());
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
        || turn.as_ref().is_some_and(|turn| &verified.turn_id != turn)
        || verified.tool != WorkflowTool::Report
    {
        return Err(AppError::NotFound.into());
    }
    let existing = entry
        .store
        .report_handoff_status(verified.clone(), request_sha256.clone())
        .await;
    let stored = match existing {
        Ok(stored) => stored,
        Err(AppError::NotFound) if !lookup => {
            if let Some(dto::ReportSource::File { path, sha256 }) = source_mut(&mut handoff)
                && sha256.is_none()
            {
                if expected != HostProvider::Claude {
                    return Err(AppError::InvalidRequest.into());
                }
                let path = absolute_path(path)?;
                let hash_value =
                    app::hash_report_file(workflow.report_files.as_ref(), verified.clone(), path)
                        .await?;
                *sha256 = Some(hash(hash_value.as_str())?);
            }
            let bound = serde_json::to_vec(&handoff).map_err(|_| AppError::Internal)?;
            let declaration_sha256 = ReportHash::from_sha256(Sha256::digest(bound).into());
            let declaration = declaration(handoff, expected)?;
            declaration.validate()?;
            let settings = if declaration.requires_review_refresh() {
                let source = entry.source_reader().await?;
                let (identity, settings) = entry.store.report_source(lead.clone()).await?;
                if source.review(identity.clone()).await?.identity != identity {
                    return Err(AppError::Conflict.into());
                }
                Some(settings)
            } else {
                None
            };
            let current = proof
                .verify_active_call(
                    lead,
                    turn,
                    call_id.as_str().to_owned(),
                    WorkflowTool::Report,
                )
                .await?;
            if current != verified {
                return Err(AppError::Conflict.into());
            }

            entry
                .store
                .stage_report(
                    verified,
                    declaration,
                    request_sha256,
                    declaration_sha256,
                    settings,
                )
                .await?
        }
        Err(error) => return Err(error.into()),
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
            corrects,
            finding_changes,
            section_changes,
            assessments,
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
            assessments: project_assessments(assessments)?,
            edits: project_edits(corrects, finding_changes, section_changes)?,
            amends: amends
                .map(|base| -> Result<_, AppError> {
                    Ok(ReportReference {
                        receipt_id: parse(base.receipt_id.as_str())?,
                        hash: parse(base.sha256.as_str())?,
                    })
                })
                .transpose()?,
        },
        dto::Handoff::Unchanged {
            accepted,
            source,
            assessments,
        } => Declaration::Unchanged {
            assessments: project_assessments(assessments)?,
            accepted: ReportReference {
                receipt_id: parse(accepted.receipt_id.as_str())?,
                hash: parse(accepted.sha256.as_str())?,
            },
            source: project_source(source, provider)?,
        },
        dto::Handoff::NeedsInput {
            reason,
            assessments,
        } => Declaration::NeedsInput {
            reason,
            assessments: project_assessments(assessments)?,
        },
    })
}

fn project_assessments(values: Vec<dto::Assessment>) -> Result<Vec<app::Assessment>, AppError> {
    values
        .into_iter()
        .map(|value| {
            use reviewq_app::followups::NextAction;
            Ok(app::Assessment {
                discussion_id: value.discussion_id.as_str().to_owned(),
                conclusion: project_conclusion(value.conclusion),
                reason: value.reason,
                finding_ids: value
                    .finding_ids
                    .into_iter()
                    .map(|id| id.as_str().to_owned())
                    .collect(),
                next_action: match value.next_action {
                    reviewq_protocol::followups::NextAction::Review => NextAction::Review,
                    reviewq_protocol::followups::NextAction::WaitingAuthor => {
                        NextAction::WaitingAuthor
                    }
                },
            })
        })
        .collect()
}

fn project_conclusion(value: dto::FindingConclusion) -> app::FindingConclusion {
    use app::FindingConclusion;
    match value {
        dto::FindingConclusion::Fixed => FindingConclusion::Fixed,
        dto::FindingConclusion::ClarifiedWithdrawn => FindingConclusion::ClarifiedWithdrawn,
        dto::FindingConclusion::Superseded => FindingConclusion::Superseded,
        dto::FindingConclusion::PartlyAddressed => FindingConclusion::PartlyAddressed,
        dto::FindingConclusion::StillPresent => FindingConclusion::StillPresent,
        dto::FindingConclusion::Unverified => FindingConclusion::Unverified,
    }
}

fn project_edits(
    correction: Option<dto::Correction>,
    findings: Vec<dto::FindingEdit>,
    sections: Vec<dto::SectionEdit>,
) -> Result<app::ReportEdits, AppError> {
    let ids = |values: Vec<reviewq_protocol::ResourceIdDto>| {
        values
            .into_iter()
            .map(|value| value.as_str().to_owned())
            .collect()
    };
    Ok(app::ReportEdits {
        correction: correction
            .map(|value| {
                Ok::<_, AppError>(app::Correction {
                    base: ReportReference {
                        receipt_id: parse(value.receipt_id.as_str())?,
                        hash: parse(value.sha256.as_str())?,
                    },
                    kind: match value.kind {
                        dto::CorrectionKind::Presentation => app::CorrectionKind::Presentation,
                        dto::CorrectionKind::Evidence => app::CorrectionKind::Evidence,
                    },
                    reason: value.reason,
                    finding_ids: ids(value.finding_ids),
                    evidence: value
                        .evidence
                        .into_iter()
                        .map(|reference| {
                            let (target, hash) = match reference {
                                dto::EvidenceReference::Finding(value) => (
                                    app::EvidenceTarget::Finding(
                                        value.finding_id.as_str().to_owned(),
                                    ),
                                    value.sha256,
                                ),
                                dto::EvidenceReference::Section(value) => (
                                    app::EvidenceTarget::Section(value.section_title),
                                    value.sha256,
                                ),
                            };
                            Ok(app::EvidenceReference {
                                target,
                                hash: parse(hash.as_str())?,
                            })
                        })
                        .collect::<Result<_, AppError>>()?,
                })
            })
            .transpose()?,
        findings: findings
            .into_iter()
            .map(|value| {
                Ok(app::FindingEdit {
                    finding_id: value.finding_id.as_str().to_owned(),
                    conclusion: project_conclusion(value.conclusion),
                    reason: value.reason,
                    checked_sha: parse(value.checked_sha.as_str())?,
                    discussion_ids: ids(value.discussion_ids),
                })
            })
            .collect::<Result<_, AppError>>()?,
        sections: sections
            .into_iter()
            .map(|value| {
                Ok(app::SectionEdit {
                    baseline_title: value.baseline_title,
                    baseline_hash: parse(value.baseline_sha256.as_str())?,
                    replacement_title: value.replacement_title,
                    conclusion: project_conclusion(value.conclusion),
                    reason: value.reason,
                    discussion_ids: ids(value.discussion_ids),
                })
            })
            .collect::<Result<_, AppError>>()?,
    })
}

fn source_mut(handoff: &mut dto::Handoff) -> Option<&mut dto::ReportSource> {
    match handoff {
        dto::Handoff::Final { source, .. } | dto::Handoff::Unchanged { source, .. } => Some(source),
        dto::Handoff::NeedsInput { .. } => None,
    }
}

fn absolute_path(path: &str) -> Result<AbsolutePath, AppError> {
    if path.len() > 4096 || path.contains('\0') {
        return Err(AppError::InvalidRequest);
    }
    AbsolutePath::from_canonical(PathBuf::from(path)).map_err(|_| AppError::InvalidRequest)
}

fn project_source(source: dto::ReportSource, provider: HostProvider) -> Result<Source, AppError> {
    Ok(match source {
        dto::ReportSource::Response => Source::Response,
        dto::ReportSource::File { path, sha256 } => {
            if sha256.is_none() {
                return Err(if provider == HostProvider::Codex {
                    AppError::InvalidRequest
                } else {
                    AppError::Conflict
                });
            }
            Source::File {
                path: absolute_path(&path)?,
                hash: sha256.map(|value| parse(value.as_str())).transpose()?,
            }
        }
    })
}
