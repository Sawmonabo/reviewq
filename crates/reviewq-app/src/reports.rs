//! Review reports, immutable source reads and finding projections.

mod capture;
mod display;
mod drawings;
mod index;
mod revision;

pub use display::{
    DISPLAY_INDEX_FORMAT, DisplayBlock, DisplayBlockKind, DisplayFallback, DisplayIndex,
    DisplayLayout, DisplayWindow, DisplayWindowBlock, DisplayWindowPosition, DisplayWindowState,
    MAX_DISPLAY_BLOCKS, MAX_DISPLAY_HEADING_BYTES, MAX_DISPLAY_HEADINGS_BYTES, build_display_index,
};
pub use drawings::{
    DRAWING_RECIPE, DrawingOutput, DrawingRenderer, MAX_DRAWING_BYTES, mermaid_sources,
};

pub use revision::{
    Correction, CorrectionKind, EvidenceReference, EvidenceTarget, FindingEdit, ReportEdits,
    SectionEdit,
};

pub use capture::{
    AcceptedReport, ArtifactChunk, Assessment, CHUNK_BYTES, CapturedArtifact, Declaration,
    HandoffReceipt, HandoffState, ReportCaptureStore, ReportFileSource, ReportWork,
    RevisionEvidence, Source, hash_report_file, reconcile_reports,
};

use reviewq_domain::identity::{GitSha, ReceiptId, ReportHash, Revision, RunId};

use crate::access::{Actor, AppError, AppFuture};
use crate::events::Snapshot;
use crate::hosts::MessageBody;

pub const MAX_REPORT_PAGE: u16 = 50;
pub const MAX_SECTION_SCALARS: u32 = 12_000;
const MAX_MARKDOWN_LINE_BYTES: usize = 16 * 1024;

use crate::labels::labels;

labels!(Verdict {
    Approve => "approve",
    RequestChanges => "request_changes",
    Blocked => "blocked",
    NotSpecified => "not_specified",
});
labels!(Coverage {
    FullMr => "full_mr",
    SelectedThreads => "selected_threads",
});
labels!(SemanticIndex {
    Ready => "ready",
    SourceOnly => "source_only",
});
labels!(ReportDeliveryState {
    Preparing => "preparing",
    Saving => "saving",
    Failed => "failed",
    Saved => "saved",
});
labels!(Publication {
    Withheld => "withheld",
    Authorized => "authorized",
    Published => "published",
});
labels!(FindingConclusion {
    Fixed => "fixed",
    ClarifiedWithdrawn => "clarified_withdrawn",
    Superseded => "superseded",
    PartlyAddressed => "partly_addressed",
    StillPresent => "still_present",
    Unverified => "unverified",
});

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactStats {
    pub byte_length: u64,
    pub total_code_points: u64,
    pub line_count: u64,
    pub max_line_bytes: u64,
    pub complete: bool,
}

impl ArtifactStats {
    pub fn require_markdown_budget(&self) -> Result<(), AppError> {
        if self.byte_length > 1024 * 1024
            || self.line_count > 10_000
            || self.max_line_bytes > MAX_MARKDOWN_LINE_BYTES as u64
        {
            return Err(AppError::PayloadTooLarge);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportReference {
    pub receipt_id: ReceiptId,
    pub hash: ReportHash,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportSummary {
    pub reference: ReportReference,
    pub original_sha256: Option<ReportHash>,
    pub native_provenance: bool,
    pub parent_receipt_id: Option<ReceiptId>,
    pub attention_receipt_id: ReceiptId,
    pub reviewed_sha: Option<GitSha>,
    pub revision_reason: Option<String>,
    pub full_mr_reviewed_sha: Option<GitSha>,
    pub coverage: Coverage,
    pub coverage_source_sha: Option<GitSha>,
    pub coverage_target_sha: Option<GitSha>,
    pub verdict: Verdict,
    pub semantic_index: SemanticIndex,
    pub finding_count: Option<u64>,
    pub change_count: Option<u64>,
    pub captured_at_ms: i64,
    pub stats: ArtifactStats,
}

pub struct ReportDelivery {
    pub receipt_id: Option<ReceiptId>,
    pub state: ReportDeliveryState,
    pub reason: Option<String>,
}

pub struct ReportView {
    pub report: Option<ReportSummary>,
    pub delivery: Option<ReportDelivery>,
    pub markdown: Option<String>,
}

#[derive(Clone)]
pub struct ReportPosition {
    pub captured_at_ms: i64,
    pub receipt_id: ReceiptId,
}

pub struct ReportHistory {
    pub items: Vec<ReportSummary>,
    pub next: Option<ReportPosition>,
    pub current_receipt_id: Option<ReceiptId>,
    pub delivery: Option<ReportDelivery>,
}

pub struct ReportSection {
    pub report: ReportReference,
    pub complete: bool,
    pub original_sha256: Option<ReportHash>,
    pub text: String,
    pub offset: u64,
    pub next_offset: Option<u64>,
    pub total_code_points: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Collection {
    #[default]
    Dispositions,
    Findings,
    Changes,
    Sections,
}

#[derive(Clone)]
pub enum ContextKey {
    Identity(String),
    Offset(u64),
}

pub struct ContextPosition {
    pub report: ReportReference,
    pub disposition_version: Revision,
    pub key: ContextKey,
}

pub struct ContextRequest {
    pub receipt: Option<ReceiptId>,
    pub collection: Collection,
    pub after: Option<ContextPosition>,
    pub limit: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRange {
    pub offset: u64,
    pub length: u64,
    pub hash: ReportHash,
}

#[derive(Clone)]
pub enum Descriptor {
    Finding {
        id: String,
        title: String,
        severity: Option<String>,
        blocking: Option<bool>,
        source: Option<SourceRange>,
    },
    Change {
        id: String,
        conclusion: FindingConclusion,
        reason: String,
        checked_sha: Option<GitSha>,
        discussion_ids: Vec<String>,
    },
    Disposition {
        id: String,
        blocking: Option<bool>,
        publication: Option<Publication>,
        reason: String,
    },
    Section {
        title: String,
        source: SourceRange,
    },
}

impl Descriptor {
    pub fn key(&self) -> ContextKey {
        match self {
            Self::Finding { id, .. } | Self::Change { id, .. } | Self::Disposition { id, .. } => {
                ContextKey::Identity(id.clone())
            }
            Self::Section { source, .. } => ContextKey::Offset(source.offset),
        }
    }
}

pub struct ReportContext {
    pub report: Option<ReportReference>,
    pub disposition_version: Revision,
    pub items: Vec<Descriptor>,
    pub next: Option<ContextPosition>,
}

pub struct CandidateReport {
    pub reference: ReportReference,
    pub stats: ArtifactStats,
    pub turn_id: String,
    pub call_id: String,
    pub declaration_sha256: ReportHash,
    pub reconciled_by_batch_id: Option<String>,
    pub reason: String,
    pub section: ReportSection,
}

pub enum QuoteResolution {
    Missing,
    Ambiguous,
    Unique { offset: u64, length: u64 },
}

pub trait ReportStore: Send + Sync {
    fn read_report(
        &self,
        actor: Actor,
        run: RunId,
        receipt: Option<ReceiptId>,
        markdown: bool,
    ) -> AppFuture<'_, Snapshot<ReportView>>;
    fn read_report_window(
        &self,
        actor: Actor,
        run: RunId,
        report: ReportReference,
        position: DisplayWindowPosition,
    ) -> AppFuture<'_, Snapshot<DisplayWindow>>;
    fn resolve_report_quote(
        &self,
        actor: Actor,
        run: RunId,
        report: ReportReference,
        text: MessageBody,
    ) -> AppFuture<'_, Snapshot<QuoteResolution>>;
    fn read_report_history(
        &self,
        actor: Actor,
        run: RunId,
        after: Option<ReportPosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<ReportHistory>>;
    fn read_report_context(
        &self,
        actor: Actor,
        run: RunId,
        request: ContextRequest,
    ) -> AppFuture<'_, Snapshot<ReportContext>>;
    fn read_report_section(
        &self,
        actor: Actor,
        run: RunId,
        receipt: Option<ReceiptId>,
        offset: u64,
        characters: u32,
    ) -> AppFuture<'_, Snapshot<ReportSection>>;
    fn read_report_chunk(
        &self,
        actor: Actor,
        run: RunId,
        report: ReportReference,
        ordinal: u64,
    ) -> AppFuture<'_, Option<Vec<u8>>>;
    fn read_report_candidate(
        &self,
        actor: Actor,
        run: RunId,
        receipt: ReceiptId,
        offset: u64,
        characters: u32,
    ) -> AppFuture<'_, Snapshot<CandidateReport>>;
}

#[derive(Clone)]
pub struct DispositionEdit {
    pub run: RunId,
    pub receipt: ReceiptId,
    pub expected_revision: Revision,
    pub finding_id: String,
    pub submission: reviewq_domain::identity::SubmissionId,
    pub blocking: Option<bool>,
    pub publication: Option<Publication>,
    pub reason: String,
}

pub struct DispositionReceipt {
    pub receipt_id: ReceiptId,
    pub disposition_version: Revision,
    pub finding: Descriptor,
}

pub struct ViewedReport {
    pub run: RunId,
    pub receipt: ReceiptId,
    pub submission: reviewq_domain::identity::SubmissionId,
    pub rendered_batch_id: Option<String>,
}

pub struct ViewReceipt {
    pub receipt_id: ReceiptId,
    pub previous_receipt_id: Option<ReceiptId>,
    pub rendered_batch_id: Option<String>,
}

pub trait ReportMutationStore: Send + Sync {
    fn set_disposition(
        &self,
        actor: Actor,
        edit: DispositionEdit,
    ) -> AppFuture<'_, Snapshot<DispositionReceipt>>;
    fn mark_report_viewed(
        &self,
        actor: Actor,
        view: ViewedReport,
    ) -> AppFuture<'_, Snapshot<ViewReceipt>>;
}

pub async fn send_feedback(
    store: &(impl crate::hosts::LeadStore + crate::queue::QueueStore + ?Sized),
    actor: Actor,
    run: RunId,
    submission_id: reviewq_domain::identity::SubmissionId,
    text: crate::hosts::MessageBody,
    reference: crate::hosts::ReportRef,
) -> Result<Snapshot<crate::hosts::DeliveryReceipt>, AppError> {
    use crate::hosts::SubmitMessage;

    let target = store
        .read_run(actor.clone(), run, None, 1)
        .await?
        .value
        .lead;
    store
        .admit_message(
            actor,
            SubmitMessage::user_input(target, submission_id, text, Some(reference)),
        )
        .await
}
