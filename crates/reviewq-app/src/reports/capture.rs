use std::sync::Arc;

use reviewq_domain::identity::{GitSha, MrIdentity, ReceiptId, ReportHash, RunId, SessionKey};
use reviewq_domain::session::{AbsolutePath, LeadTarget};
use sha2::{Digest, Sha256};

use crate::access::{Actor, AppError, AppFuture};
use crate::events::Snapshot;
use crate::labels::labels;
use crate::workflow::{
    CompletedReportCall, ExpectedAcknowledgement, NativeCall, NativeWorkflowProof,
    ReportCompletion, ReportContent, ReportFailureAddress,
};

use super::index;
use super::{
    ArtifactStats, Descriptor, FindingConclusion, ReportEdits, ReportReference, StructuredReport,
    Verdict,
};

pub const CHUNK_BYTES: usize = 64 * 1024;
const SEMANTIC_BYTES: u64 = 4 * 1024 * 1024;
const MAX_REPORT_WORK: u16 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Source {
    File {
        path: AbsolutePath,
        hash: Option<ReportHash>,
    },
    Response,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RevisionEvidence {
    Exact(GitSha),
    Unknown(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Assessment {
    pub discussion_id: String,
    pub conclusion: FindingConclusion,
    pub reason: String,
    pub finding_ids: Vec<String>,
    pub next_action: crate::followups::NextAction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Declaration {
    Final {
        source: Source,
        revision: RevisionEvidence,
        verdict: Verdict,
        report: Option<Arc<StructuredReport>>,
        amends: Option<ReportReference>,
        edits: ReportEdits,
        assessments: Vec<Assessment>,
    },
    Unchanged {
        accepted: ReportReference,
        source: Source,
        assessments: Vec<Assessment>,
    },
    NeedsInput {
        reason: String,
        assessments: Vec<Assessment>,
    },
}

impl Declaration {
    pub fn requires_review_refresh(&self) -> bool {
        matches!(
            self,
            Self::Final {
                amends: Some(_),
                ..
            } | Self::Unchanged { .. }
        )
    }

    pub fn assessments(&self) -> &[Assessment] {
        match self {
            Self::Final { assessments, .. }
            | Self::Unchanged { assessments, .. }
            | Self::NeedsInput { assessments, .. } => assessments,
        }
    }

    /// Checks the invariants acceptance relies on. The transport validates the wire
    /// fields and their bounds before projecting a declaration.
    pub fn validate(&self) -> Result<(), AppError> {
        match self {
            Self::Final {
                source: Source::File { hash: None, .. },
                ..
            }
            | Self::Unchanged {
                source: Source::File { hash: None, .. },
                ..
            } => Err(AppError::InvalidRequest),
            Self::Final {
                report,
                amends,
                edits,
                ..
            } if report.is_some() == amends.is_some()
                || (amends.is_some() && !edits.is_empty()) =>
            {
                Err(AppError::InvalidRequest)
            }
            _ => Ok(()),
        }
    }
}

labels!(HandoffState {
    PendingCompletion => "pending_completion", Recorded => "recorded", Failed => "failed",
});

pub struct HandoffReceipt {
    pub receipt_id: ReceiptId,
    pub declaration_sha256: ReportHash,
    pub state: HandoffState,
}

#[derive(Clone)]
pub struct ReportWork {
    pub call: NativeCall,
    pub acknowledgement: ExpectedAcknowledgement,
    pub declaration: Declaration,
    pub fence: u64,
}

pub struct ArtifactChunk {
    pub ordinal: u64,
    pub byte_start: u64,
    pub scalar_start: u64,
    pub bytes: Vec<u8>,
}

pub struct CapturedArtifact {
    pub hash: ReportHash,
    pub stats: ArtifactStats,
    pub index: Option<Vec<Descriptor>>,
}

pub struct AcceptedReport {
    pub reference: ReportReference,
    pub source: Option<Source>,
}

pub trait ReportFileSource: Send + Sync {
    fn open_report(
        &self,
        call: NativeCall,
        path: AbsolutePath,
    ) -> AppFuture<'_, Arc<dyn ReportContent>>;
}

/// Identifies the configured project, GitLab principal and checkout used by a report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportAuthority(pub ReportHash);

pub trait ReportCaptureStore: Send + Sync {
    fn report_lead(&self, session: SessionKey) -> AppFuture<'_, LeadTarget>;

    /// Resolves a host-reported session inside the checkout that presented the grant.
    fn mcp_report_lead(
        &self,
        actor: Actor,
        provider: reviewq_domain::session::HostProvider,
        native_id: reviewq_domain::identity::NativeSessionId,
    ) -> AppFuture<'_, LeadTarget>;

    /// Last occurrence in this native turn and the prior occurrence of an exact retried hook.
    fn report_hook_order(
        &self,
        session: SessionKey,
        turn: reviewq_domain::identity::NativeTurnId,
        call_id: String,
    ) -> AppFuture<'_, (u32, Option<u32>)>;

    fn report_source(&self, lead: LeadTarget) -> AppFuture<'_, (MrIdentity, ReportAuthority)>;

    fn accepted_report(&self, lead: LeadTarget) -> AppFuture<'_, Snapshot<Option<AcceptedReport>>>;

    fn stage_report(
        &self,
        call: NativeCall,
        declaration: Declaration,
        request_sha256: ReportHash,
        declaration_sha256: ReportHash,
        source_authority: Option<ReportAuthority>,
        checkout: Option<Actor>,
    ) -> AppFuture<'_, Snapshot<HandoffReceipt>>;

    fn reserve_report_correction(&self, call: NativeCall) -> AppFuture<'_, bool>;

    fn report_handoff_status(
        &self,
        call: NativeCall,
        digest: ReportHash,
    ) -> AppFuture<'_, Snapshot<HandoffReceipt>>;

    fn claim_report_work(&self, limit: u16) -> AppFuture<'_, Vec<ReportWork>>;

    fn defer_report(&self, work: ReportWork) -> AppFuture<'_, ()>;

    fn begin_artifact(&self, work: ReportWork) -> AppFuture<'_, ()>;

    fn append_report_chunk(&self, work: ReportWork, chunk: ArtifactChunk) -> AppFuture<'_, ()>;

    fn unresolved_report_failures(
        &self,
        call: NativeCall,
    ) -> AppFuture<'_, Vec<ReportFailureAddress>>;

    /// Returns whether a new immutable report version was accepted.
    fn accept_report(
        &self,
        work: ReportWork,
        result_entry: reviewq_domain::identity::NativeInputId,
        artifact: Option<CapturedArtifact>,
        resolved: Vec<ReportFailureAddress>,
    ) -> AppFuture<'_, bool>;

    /// Optionally index the validated capture buffer after committing the accepted version.
    fn index_captured_source(
        &self,
        run: RunId,
        reference: ReportReference,
        source: Option<String>,
    ) -> AppFuture<'_, ()>;

    fn reject_report(
        &self,
        work: ReportWork,
        artifact: Option<CapturedArtifact>,
        reason: &'static str,
        native_result: Option<reviewq_domain::identity::NativeInputId>,
    ) -> AppFuture<'_, ()>;
}

pub async fn hash_report_file(
    files: &dyn ReportFileSource,
    call: NativeCall,
    path: AbsolutePath,
) -> Result<ReportHash, AppError> {
    let source = files.open_report(call, path).await?;
    let total = source.byte_length();
    if total == 0 || total > i64::MAX as u64 {
        return Err(AppError::InvalidRequest);
    }
    let mut digest = Sha256::new();
    let mut offset = 0;
    while offset < total {
        let length = (total - offset).min(CHUNK_BYTES as u64) as usize;
        let bytes = source.read(offset, length).await?;
        if bytes.len() != length {
            return Err(AppError::Conflict);
        }
        digest.update(bytes);
        offset += length as u64;
    }
    source.verify_unchanged().await?;
    Ok(ReportHash::from_sha256(digest.finalize().into()))
}

pub async fn reconcile_reports(
    store: &dyn ReportCaptureStore,
    proof: &dyn NativeWorkflowProof,
    files: &dyn ReportFileSource,
) -> Result<(), AppError> {
    for work in store.claim_report_work(MAX_REPORT_WORK).await? {
        reconcile_one(store, proof, files, work).await?;
    }
    Ok(())
}

async fn reconcile_one(
    store: &dyn ReportCaptureStore,
    proof: &dyn NativeWorkflowProof,
    files: &dyn ReportFileSource,
    work: ReportWork,
) -> Result<(), AppError> {
    let evidence = match proof
        .report_completion(
            work.call.clone(),
            work.acknowledgement.clone(),
            matches!(
                work.declaration,
                Declaration::Final {
                    source: Source::Response,
                    ..
                }
            ),
        )
        .await
    {
        Ok(ReportCompletion::Pending) | Err(AppError::Unavailable | AppError::Busy) => {
            return store.defer_report(work).await;
        }
        Ok(ReportCompletion::Confirmed(evidence))
            if evidence.call == work.call && evidence.acknowledged == work.acknowledgement =>
        {
            evidence
        }
        _ => {
            return store
                .reject_report(work, None, "native_acknowledgement_unproved", None)
                .await;
        }
    };
    let source = match &work.declaration {
        Declaration::Final { source, .. } => Some(source),
        Declaration::Unchanged {
            source: Source::Response,
            ..
        } => None,
        Declaration::Unchanged { source, .. } => Some(source),
        Declaration::NeedsInput { .. } => None,
    };
    if !evidence.normal_turn_completion
        && !matches!(
            work.declaration,
            Declaration::Final {
                source: Source::File { .. },
                ..
            }
        )
    {
        return store
            .reject_report(
                work,
                None,
                "native_turn_did_not_complete",
                Some(evidence.result_entry_id),
            )
            .await;
    }
    let (artifact, display_source) = match source {
        None => (None, None),
        Some(source) => {
            let content = match content(files, &work.call, source, &evidence).await {
                Ok(content) => content,
                Err(AppError::Busy) => return store.defer_report(work).await,
                Err(_) => {
                    return store
                        .reject_report(
                            work,
                            None,
                            "native_report_source_unavailable",
                            Some(evidence.result_entry_id),
                        )
                        .await;
                }
            };
            let install = matches!(work.declaration, Declaration::Final { .. });
            if install {
                store.begin_artifact(work.clone()).await?;
            }
            let (captured, display_source) =
                match read_source(store, &work, content.as_ref(), install).await {
                    Ok(captured) => captured,
                    Err(AppError::Unavailable | AppError::Busy) => {
                        return store.defer_report(work).await;
                    }
                    Err(_) => {
                        return store
                            .reject_report(
                                work,
                                None,
                                "native_report_source_invalid",
                                Some(evidence.result_entry_id),
                            )
                            .await;
                    }
                };
            if let Source::File {
                hash: Some(hash), ..
            } = source
                && hash != &captured.hash
            {
                let candidate = install.then_some(captured);
                return store
                    .reject_report(
                        work,
                        candidate,
                        "native_report_hash_mismatch",
                        Some(evidence.result_entry_id),
                    )
                    .await;
            }
            (Some(captured), display_source)
        }
    };
    let resolved = if matches!(&work.declaration, Declaration::Final {
        revision: RevisionEvidence::Exact(_), assessments, amends: None, ..
    } if assessments.is_empty())
    {
        let candidates = store.unresolved_report_failures(work.call.clone()).await?;
        if candidates.is_empty() {
            Vec::new()
        } else {
            proof
                .earlier_report_failures(work.call.clone(), candidates)
                .await
                .unwrap_or_default()
        }
    } else {
        Vec::new()
    };
    let run = work.call.lead.run_id().clone();
    let reference = artifact.as_ref().map(|captured| ReportReference {
        receipt_id: work.acknowledgement.receipt_id.clone(),
        hash: captured.hash.clone(),
    });
    let accepted = store
        .accept_report(work, evidence.result_entry_id, artifact, resolved)
        .await?;
    if accepted
        && let Some(reference) = reference
        && let Err(error) = store
            .index_captured_source(run, reference, display_source)
            .await
    {
        eprintln!("ReviewQ could not retain the report display index ({error}).");
    }
    Ok(())
}

async fn content(
    files: &dyn ReportFileSource,
    call: &NativeCall,
    source: &Source,
    evidence: &CompletedReportCall,
) -> Result<Arc<dyn ReportContent>, AppError> {
    match source {
        Source::File { path, .. } => files.open_report(call.clone(), path.clone()).await,
        Source::Response => evidence.final_response.clone().ok_or(AppError::Unavailable),
    }
}

async fn read_source(
    store: &dyn ReportCaptureStore,
    work: &ReportWork,
    source: &dyn ReportContent,
    install: bool,
) -> Result<(CapturedArtifact, Option<String>), AppError> {
    let total = source.byte_length();
    if total == 0 || total > i64::MAX as u64 {
        return Err(AppError::InvalidRequest);
    }
    let mut index_source =
        (install && total <= SEMANTIC_BYTES).then(|| String::with_capacity(total as usize));
    let mut hash = Sha256::new();
    let mut count = LineStats::default();
    let mut has_content = false;
    let mut offset = 0;
    let mut persisted = 0;
    let mut scalars = 0;
    let mut ordinal = 0;
    let mut carry = Vec::new();
    while offset < total {
        let length = (total - offset).min((CHUNK_BYTES - 4) as u64) as usize;
        let mut read = source.read(offset, length).await?;
        if read.len() != length {
            return Err(AppError::Conflict);
        }
        hash.update(&read);
        offset += length as u64;
        let mut bytes = if carry.is_empty() {
            read
        } else {
            carry.append(&mut read);
            std::mem::take(&mut carry)
        };
        let valid_len = match std::str::from_utf8(&bytes) {
            Ok(_) => bytes.len(),
            Err(error) if error.error_len().is_none() && offset < total => error.valid_up_to(),
            Err(_) => return Err(AppError::InvalidRequest),
        };
        carry = bytes.split_off(valid_len);
        if bytes.is_empty() {
            continue;
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| AppError::InvalidRequest)?;
        let characters = text.chars().count() as u64;
        has_content |= !text.trim_start_matches('\u{feff}').trim().is_empty();
        if let Some(index) = index_source.as_mut() {
            index.push_str(text);
        }
        count.push(&bytes);
        if install {
            store
                .append_report_chunk(
                    work.clone(),
                    ArtifactChunk {
                        ordinal,
                        byte_start: persisted,
                        scalar_start: scalars,
                        bytes,
                    },
                )
                .await?;
        }
        ordinal += 1;
        persisted += valid_len as u64;
        scalars += characters;
    }
    source.verify_unchanged().await?;
    if persisted != total || !has_content {
        return Err(AppError::InvalidRequest);
    }
    let (line_count, max_line_bytes) = count.finish();
    let stats = ArtifactStats {
        byte_length: total,
        total_code_points: scalars,
        line_count,
        max_line_bytes,
        complete: true,
    };
    let display_admitted = stats.require_markdown_budget().is_ok();
    // A report whose headings cannot be parsed is accepted without a heading index.
    let (index, display_source) = match index_source {
        Some(source) => match tokio::task::spawn_blocking(move || {
            let index = index::parse(&source)?;
            Ok::<_, AppError>((index, display_admitted.then_some(source)))
        })
        .await
        {
            Ok(Ok((index, source))) => (index, source),
            Ok(Err(_)) | Err(_) => (None, None),
        },
        None => (None, None),
    };
    Ok((
        CapturedArtifact {
            hash: ReportHash::from_sha256(hash.finalize().into()),
            stats,
            index,
        },
        display_source,
    ))
}

#[derive(Default)]
struct LineStats {
    line: u64,
    lines: u64,
    max: u64,
    last_cr: bool,
}

impl LineStats {
    fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' {
                self.max = self
                    .max
                    .max(self.line.saturating_sub(u64::from(self.last_cr)));
                self.lines += 1;
                self.line = 0;
            } else {
                self.line += 1;
            }
            self.last_cr = byte == b'\r';
        }
    }

    fn finish(self) -> (u64, u64) {
        (
            self.lines + u64::from(self.line > 0),
            self.max.max(self.line),
        )
    }
}
