use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use reviewq_app::access::{Actor, AppError};
use reviewq_app::reports::{
    self as app, Declaration, ReportReference, RevisionEvidence, Source, Verdict,
};
use reviewq_app::workflow::{NativeCall, WorkflowTool};
use reviewq_domain::identity::{NativeTurnId, ReportHash};
use reviewq_domain::session::{AbsolutePath, HostProvider};
use reviewq_protocol::reports as dto;
use reviewq_protocol::{ResourceIdDto, SessionKeyDto, Success};
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
    // Both native bodies wrap the same tool input. Field errors return to the agent's open turn.
    let raw = serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|mut body| {
            let handoff = body.as_object_mut()?.remove("handoff")?;
            Some(serde_json::json!({ "handoff": handoff }))
        });
    if !lookup && let Some(raw) = &raw {
        let errors = dto::validate_report_handoff(raw);
        if !errors.is_empty() {
            return Err(ApiError::InvalidHandoff(errors));
        }
    }
    let command = match serde_json::from_slice::<dto::HandoffCommand>(bytes) {
        Ok(command) => command,
        Err(_) if !lookup => return Err(malformed_handoff(entry, bytes).await.into()),
        Err(_) => return Err(AppError::InvalidRequest.into()),
    };
    let (session, turn, call_id, expected, handoff, witness) = match command {
        dto::HandoffCommand::Codex(value) => (
            value.session_key,
            None,
            value.call_id,
            HostProvider::Codex,
            value.handoff,
            None,
        ),
        dto::HandoffCommand::Companion(value) => (
            value.context.session_key,
            Some(parse::<NativeTurnId>(value.context.turn_id.as_str())?),
            value.context.call_id,
            HostProvider::Claude,
            value.handoff,
            value.context.witness_ordinal,
        ),
    };
    if expected == HostProvider::Claude {
        // Claude's native tool hook supplies the session and turn that its HTTP MCP client omits.
        let raw = raw.ok_or(AppError::InvalidRequest)?;
        let arguments = serde_json::to_vec(&raw).map_err(|_| AppError::InvalidRequest)?;
        let lead = entry.store.report_lead(parse(session.as_str())?).await?;
        if lead.binding().identity().provider() != expected {
            return Err(AppError::Forbidden.into());
        }
        let turn = turn.ok_or(AppError::InvalidRequest)?;
        let proof = McpReportProof {
            actor: None,
            arguments,
            enclosing: None,
            witness: witness.map(|ordinal| (turn.clone(), ordinal.get())),
        };
        let verified = proof.verify(entry, lead, call_id.as_str()).await?;
        if turn != verified.turn_id {
            return Err(AppError::Conflict.into());
        }
        return Ok(Json(
            submit_verified(entry, verified, handoff, lookup, Some(proof)).await?,
        ));
    }
    let verified = verify_call(entry, &session, turn, &call_id, expected).await?;
    Ok(Json(
        submit_verified(entry, verified, handoff, lookup, None).await?,
    ))
}

pub(in crate::entry) struct McpReportProof {
    pub(in crate::entry) actor: Option<Actor>,
    pub(in crate::entry) arguments: Vec<u8>,
    pub(in crate::entry) enclosing: Option<String>,
    pub(in crate::entry) witness: Option<(NativeTurnId, u32)>,
}

impl McpReportProof {
    pub(in crate::entry) async fn verify(
        &self,
        entry: &Entry,
        lead: reviewq_domain::session::LeadTarget,
        call_id: &str,
    ) -> Result<NativeCall, AppError> {
        match &self.witness {
            Some((turn, ordinal)) => {
                entry
                    .workflow
                    .proof
                    .verify_witnessed_report_call(
                        lead,
                        turn.clone(),
                        call_id.to_owned(),
                        self.arguments.clone(),
                        *ordinal,
                        self.enclosing.clone(),
                    )
                    .await
            }
            None => {
                entry
                    .workflow
                    .proof
                    .verify_mcp_report_call(
                        lead,
                        call_id.to_owned(),
                        self.arguments.clone(),
                        self.enclosing.clone(),
                    )
                    .await
            }
        }
    }
}

pub(in crate::entry) async fn submit_verified(
    entry: &Entry,
    verified: NativeCall,
    mut handoff: dto::Handoff,
    lookup: bool,
    mcp: Option<McpReportProof>,
) -> Result<Success<dto::HandoffReceipt>, AppError> {
    let workflow = &entry.workflow;
    let proof = workflow.proof.as_ref();
    let expected = verified.lead.binding().identity().provider();
    let canonical = serde_json::to_vec(&handoff).map_err(|_| AppError::Internal)?;
    if canonical.len() > app::METADATA_BYTES {
        return Err(AppError::PayloadTooLarge);
    }
    let request_sha256 = ReportHash::from_sha256(Sha256::digest(canonical).into());
    let lead = verified.lead.clone();
    let existing = entry
        .store
        .report_handoff_status(verified.clone(), request_sha256.clone())
        .await;
    let stored = match existing {
        Ok(stored) => stored,
        Err(AppError::NotFound) if !lookup => {
            // Every file report is hashed before staging, and a file final's headings are
            // checked against its ledger.
            let ledger = matches!(
                &handoff,
                dto::Handoff::Final {
                    report: Some(_),
                    ..
                }
            );
            // An unchanged result or an amendment points at the accepted report's file.
            let accepted_sha256 = match &handoff {
                dto::Handoff::Unchanged { accepted, .. }
                | dto::Handoff::Final {
                    amends: Some(accepted),
                    ..
                } => Some(accepted.sha256.as_str().to_owned()),
                _ => None,
            };
            let mut headings = None;
            if let Some(dto::ReportSource::File { path, sha256 }) = source_mut(&mut handoff) {
                let declared = sha256
                    .as_ref()
                    .map(|value| parse::<ReportHash>(value.as_str()))
                    .transpose()?;
                if declared.is_none() && mcp.is_none() && expected != HostProvider::Claude {
                    return Err(request_correction(entry, &verified).await);
                }
                let path = match absolute_path(path) {
                    Ok(path) => Some(path),
                    // The declaration below reports the path itself.
                    Err(AppError::InvalidRequest) if declared.is_some() => None,
                    Err(AppError::InvalidRequest) => {
                        return Err(if mcp.is_some() {
                            AppError::InvalidRequest
                        } else {
                            request_correction(entry, &verified).await
                        });
                    }
                    Err(error) => return Err(error),
                };
                if let Some(path) = path {
                    let read = app::read_submitted_file(
                        workflow.report_files.as_ref(),
                        verified.clone(),
                        path,
                        declared.as_ref(),
                        ledger,
                    )
                    .await?;
                    match (&declared, &read.hash) {
                        (None, Some(value)) => *sha256 = Some(hash(value.as_str())?),
                        // Only bytes actually read can contradict the declared hash.
                        (Some(declared), Some(actual)) if declared != actual => {
                            let message = mismatch_message(accepted_sha256.as_deref(), actual);
                            return Err(AppError::InvalidHandoff(vec![
                                app::HandoffFieldError::new("/handoff/source/sha256", message),
                            ]));
                        }
                        _ => {}
                    }
                    headings = read.headings;
                }
            }
            if handoff.changes_amended_source() {
                return Err(AppError::ReportAmendmentSourceChanged);
            }
            let bound = serde_json::to_vec(&handoff).map_err(|_| AppError::Internal)?;
            let declaration_sha256 = ReportHash::from_sha256(Sha256::digest(bound).into());
            let declaration = match declaration(handoff, expected).and_then(|declaration| {
                declaration.validate()?;
                Ok(declaration)
            }) {
                Ok(declaration) => declaration,
                Err(AppError::InvalidRequest) => {
                    return Err(if mcp.is_some() {
                        AppError::InvalidRequest
                    } else {
                        request_correction(entry, &verified).await
                    });
                }
                Err(error) => return Err(error),
            };
            let settings = if declaration.requires_review_refresh() {
                let (identity, settings) = entry.store.report_source(lead.clone()).await?;
                let source = entry.source_reader().await?;
                if source.review(identity.clone()).await?.identity != identity {
                    return Err(AppError::Conflict);
                }
                Some(settings)
            } else {
                None
            };
            let current = match &mcp {
                Some(mcp) => mcp.verify(entry, lead, &verified.call_id).await?,
                None => {
                    proof
                        .verify_active_call(
                            lead,
                            Some(verified.turn_id.clone()),
                            verified.call_id.clone(),
                            WorkflowTool::Report,
                        )
                        .await?
                }
            };
            if current != verified {
                return Err(AppError::Conflict);
            }

            entry
                .store
                .stage_report(
                    verified,
                    declaration,
                    request_sha256,
                    declaration_sha256,
                    settings,
                    mcp.and_then(|context| context.actor),
                    headings,
                )
                .await?
        }
        Err(error) => return Err(error),
    };
    success(
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
    )
}

async fn verify_call(
    entry: &Entry,
    session: &SessionKeyDto,
    turn: Option<NativeTurnId>,
    call_id: &ResourceIdDto,
    expected: HostProvider,
) -> Result<NativeCall, AppError> {
    let lead = entry.store.report_lead(parse(session.as_str())?).await?;
    if lead.binding().identity().provider() != expected {
        return Err(AppError::NotFound);
    }
    let call = entry
        .workflow
        .proof
        .verify_active_call(
            lead.clone(),
            turn.clone(),
            call_id.as_str().to_owned(),
            WorkflowTool::Report,
        )
        .await?;
    if call.lead != lead
        || call.call_id != call_id.as_str()
        || call.tool != WorkflowTool::Report
        || turn.as_ref().is_some_and(|turn| &call.turn_id != turn)
    {
        return Err(AppError::NotFound);
    }
    Ok(call)
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum CorrectionAddress {
    Codex(CodexAddress),
    Claude(ClaudeAddress),
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CodexAddress {
    session_key: SessionKeyDto,
    call_id: ResourceIdDto,
    #[serde(default, rename = "handoff")]
    _handoff: Option<serde::de::IgnoredAny>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaudeAddress {
    context: dto::NativeContext,
    #[serde(default, rename = "handoff")]
    _handoff: Option<serde::de::IgnoredAny>,
}

async fn malformed_handoff(entry: &Entry, bytes: &[u8]) -> AppError {
    let verified = match serde_json::from_slice::<CorrectionAddress>(bytes) {
        Ok(CorrectionAddress::Codex(address)) => {
            verify_call(
                entry,
                &address.session_key,
                None,
                &address.call_id,
                HostProvider::Codex,
            )
            .await
        }
        Ok(CorrectionAddress::Claude(address)) => {
            let Ok(turn) = parse(address.context.turn_id.as_str()) else {
                return AppError::InvalidRequest;
            };
            verify_call(
                entry,
                &address.context.session_key,
                Some(turn),
                &address.context.call_id,
                HostProvider::Claude,
            )
            .await
        }
        Err(_) => return AppError::InvalidRequest,
    };
    match verified {
        Ok(call) => request_correction(entry, &call).await,
        Err(error) => error,
    }
}

async fn request_correction(entry: &Entry, call: &NativeCall) -> AppError {
    let current = entry
        .workflow
        .proof
        .verify_active_call(
            call.lead.clone(),
            Some(call.turn_id.clone()),
            call.call_id.clone(),
            WorkflowTool::Report,
        )
        .await;
    match current {
        Ok(current) if &current == call => {}
        Ok(_) => return AppError::Conflict,
        Err(error) => return error,
    }
    match entry.store.reserve_report_correction(call.clone()).await {
        Ok(true) => AppError::ReportCorrectionRequired,
        Ok(false) => AppError::InvalidRequest,
        Err(error) => error,
    }
}

pub(super) fn authenticate(entry: &Entry, headers: &HeaderMap) -> Result<(), AppError> {
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
            report,
            amends,
            finding_changes,
            assessments,
        } => {
            let revision = match revision {
                dto::ReportRevision::Exact { sha } => RevisionEvidence::Exact(parse(sha.as_str())?),
                dto::ReportRevision::Unknown { reason, .. } => RevisionEvidence::Unknown(reason),
            };
            Declaration::Final {
                source: project_source(source, provider)?,
                verdict: match verdict {
                    dto::ReportVerdict::Approve => Verdict::Approve,
                    dto::ReportVerdict::RequestChanges => Verdict::RequestChanges,
                    dto::ReportVerdict::Blocked => Verdict::Blocked,
                    dto::ReportVerdict::NotSpecified => Verdict::NotSpecified,
                },
                report: report.map(|report| Arc::new(structured_report(report))),
                assessments: project_assessments(assessments)?,
                edits: project_edits(finding_changes, &revision),
                revision,
                amends: amends
                    .map(|base| -> Result<_, AppError> {
                        Ok(ReportReference {
                            receipt_id: parse(base.receipt_id.as_str())?,
                            hash: parse(base.sha256.as_str())?,
                        })
                    })
                    .transpose()?,
            }
        }
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

fn structured_report(report: dto::StructuredReport) -> app::StructuredReport {
    app::StructuredReport {
        version: report.version,
        findings: report
            .findings
            .into_iter()
            .map(|finding| app::StructuredFinding {
                id: finding.id,
                title: finding.title,
                severity: finding.severity,
                blocking: finding.blocking,
                details: app::FindingDetails {
                    location: finding.location.map(|location| app::FindingLocation {
                        path: location.path,
                        line: location.line,
                    }),
                    consequence: finding.consequence,
                    evidence: app::FindingBasis {
                        kind: match finding.evidence.kind {
                            dto::FindingBasisKind::Observed => app::FindingBasisKind::Observed,
                            dto::FindingBasisKind::Gap => app::FindingBasisKind::Gap,
                        },
                        text: finding.evidence.text,
                    },
                    checked_sha: None,
                    evidence_hash: None,
                },
            })
            .collect(),
    }
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

/// Outcomes are checked at the exact reviewed revision, which ReviewQ records for each one.
fn project_edits(findings: Vec<dto::FindingEdit>, revision: &RevisionEvidence) -> app::ReportEdits {
    let checked_sha = match revision {
        RevisionEvidence::Exact(sha) => Some(sha.clone()),
        RevisionEvidence::Unknown(_) => None,
    };
    app::ReportEdits {
        findings: findings
            .into_iter()
            .map(|value| app::FindingEdit {
                finding_id: value.finding_id.as_str().to_owned(),
                conclusion: project_conclusion(value.conclusion),
                reason: value.reason,
                checked_sha: checked_sha.clone(),
                discussion_ids: value
                    .discussion_ids
                    .into_iter()
                    .map(|id| id.as_str().to_owned())
                    .collect(),
            })
            .collect(),
    }
}

/// Why a file contradicts its declared hash. An unchanged result or an amendment whose file
/// no longer holds the accepted report's bytes needs a final; otherwise the declared hash
/// is wrong.
fn mismatch_message(accepted_sha256: Option<&str>, actual: &ReportHash) -> &'static str {
    match accepted_sha256 {
        Some(accepted) if accepted != actual.as_str() => {
            "The file at source.path no longer matches the accepted report. Submit a final with its findings instead."
        }
        _ => {
            "The file at source.path does not match source.sha256. Point source at the finished report and compute the hash after its last edit."
        }
    }
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
