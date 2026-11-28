//! Review reports, immutable source reads and finding projections.

mod display;
mod drawings;
mod revision;

pub(crate) use display::display_index;
pub use display::{
    DISPLAY_INDEX_FORMAT, DisplayBlock, DisplayBlockKind, DisplayFallback, DisplayIndex,
    DisplayLayout, DisplayWindow, DisplayWindowBlock, DisplayWindowPosition, DisplayWindowState,
    HtmlRendering, MAX_DISPLAY_BLOCKS, MAX_DISPLAY_HEADING_BYTES, MAX_DISPLAY_HEADINGS_BYTES,
    build_display_index, build_report_display_index,
};
pub use drawings::{
    DRAWING_RECIPE, DrawingOutput, DrawingRenderer, MAX_DRAWING_BYTES, drawing_digest,
    mermaid_sources,
};
pub(crate) use drawings::{Undrawable, diagram_source, is_mermaid, js_space, undrawable};

pub use revision::{FindingEdit, ReportEdits};

use reviewq_domain::identity::{GitSha, ReceiptId, ReportHash, Revision, RunId};
use satteri_pulldown_cmark::Options;

use crate::access::{Actor, AppError, AppFuture};
use crate::events::Snapshot;
use crate::hosts::MessageBody;

pub const MAX_REPORT_PAGE: u16 = 50;
pub const MAX_SECTION_SCALARS: u32 = 12_000;
const MAX_MARKDOWN_LINE_BYTES: usize = 16 * 1024;
/// Largest stored chunk of a report's text.
pub const CHUNK_BYTES: usize = 64 * 1024;
/// The parser options for report text, shared by the display index, the drawing finder and
/// the ledger's field checks, so a field is judged as the reader parses the report.
pub(crate) const REPORT_MARKDOWN: Options = Options::ENABLE_GFM
    .union(Options::ENABLE_TABLES)
    .union(Options::ENABLE_STRIKETHROUGH)
    .union(Options::ENABLE_TASKLISTS)
    .union(Options::ENABLE_FOOTNOTES);

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
labels!(ReportDeliveryState {
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
labels!(Severity {
    P0 => "P0",
    P1 => "P1",
    P2 => "P2",
    P3 => "P3",
});
labels!(FindingKind {
    Functional => "functional",
    Quality => "quality",
});
labels!(FindingBasisKind {
    Observed => "observed",
    Gap => "gap",
});

/// The reviewed revision a version or a closing call names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RevisionEvidence {
    Exact(GitSha),
    Unknown(String),
}

/// The lead's conclusion on one selected discussion of a follow-up batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Assessment {
    pub discussion_id: String,
    pub conclusion: FindingConclusion,
    pub reason: String,
    pub finding_ids: Vec<String>,
    pub next_action: crate::followups::NextAction,
}

/// Identifies the configured project, GitLab principal and checkout used by a report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportAuthority(pub ReportHash);

/// Counts lines and the longest line, in bytes without a line's trailing CR, as text is
/// appended.
#[derive(Default)]
pub(crate) struct LineStats {
    line: u64,
    lines: u64,
    max: u64,
    last_cr: bool,
}

impl LineStats {
    pub(crate) fn push(&mut self, bytes: &[u8]) {
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

    /// The line count and the longest line's length.
    pub(crate) fn finish(self) -> (u64, u64) {
        (
            self.lines + u64::from(self.line > 0),
            self.max.max(self.line),
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactStats {
    pub byte_length: u64,
    pub total_code_points: u64,
    pub line_count: u64,
    pub max_line_bytes: u64,
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
    pub parent_receipt_id: Option<ReceiptId>,
    pub attention_receipt_id: ReceiptId,
    pub reviewed_sha: Option<GitSha>,
    pub revision_reason: Option<String>,
    pub full_mr_reviewed_sha: Option<GitSha>,
    pub coverage: Coverage,
    pub coverage_source_sha: Option<GitSha>,
    pub coverage_target_sha: Option<GitSha>,
    pub verdict: Verdict,
    pub finding_count: u64,
    pub change_count: u64,
    pub published_at_ms: i64,
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
    pub published_at_ms: i64,
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
    /// Which findings a `Findings` read returns; other collections ignore it.
    pub findings: FindingSelection,
    pub after: Option<ContextPosition>,
    pub limit: u16,
}

/// Which findings of a version a `Findings` read returns, and how much of each.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FindingSelection {
    pub state: FindingState,
    pub kind: Option<FindingKind>,
    /// Whether each finding carries its issue, proposed comment and the note that comment
    /// replies to. The Findings panel leaves them out, as it shows them from the report text.
    pub full_text: bool,
}

/// A finding's place in one version, from what the version records. `Open`: the version shows
/// it. `Retired`: the version records an outcome that retires a finding its parent showed. A
/// finding retired by an earlier version is in neither set.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FindingState {
    #[default]
    Open,
    Retired,
    All,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRange {
    pub offset: u64,
    pub length: u64,
    pub hash: ReportHash,
}

/// A file a finding points at, with an optional 1-based line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindingLocation {
    pub path: String,
    pub line: Option<u32>,
}

/// What the agent confirmed (`Observed`) or could not verify (`Gap`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindingBasis {
    pub kind: FindingBasisKind,
    pub text: String,
}

/// A finding record's fields as a card shows them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindingDetails {
    pub locations: Vec<FindingLocation>,
    pub issue: String,
    pub consequence: String,
    pub evidence: FindingBasis,
    pub proposed_comment: Option<String>,
    /// The GitLab note, as its ID's digits, that the proposed comment replies to. It exists
    /// only with a proposed comment and links the finding to no discussion.
    pub reply_to_note: Option<String>,
    pub details: Option<String>,
}

/// A finding's fields at the record revision a version published. The Findings panel shows
/// all but `issue`, `proposed_comment` and `reply_to_note` beside its title and badges, and
/// reads the rest in the report text at the descriptor's `source`; a full-text read fills
/// those three as well.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindingPreview {
    pub kind: FindingKind,
    pub locations: Vec<FindingLocation>,
    pub consequence: String,
    pub evidence: FindingBasis,
    pub issue: Option<String>,
    pub proposed_comment: Option<String>,
    pub reply_to_note: Option<String>,
}

/// The outcome a version records for one of its findings.
#[derive(Clone)]
pub struct FindingChange {
    pub conclusion: FindingConclusion,
    pub reason: String,
    pub checked_sha: Option<GitSha>,
}

#[derive(Clone)]
pub struct FindingDisposition {
    pub blocking: Option<bool>,
    pub publication: Option<Publication>,
    pub reason: String,
}

#[derive(Clone)]
pub enum Descriptor {
    Finding {
        id: String,
        title: String,
        severity: Severity,
        blocking: bool,
        /// The card's range in this version's text; `None` for a retired finding, whose card
        /// is in the parent version.
        source: Option<SourceRange>,
        details: Option<Box<FindingPreview>>,
        disposition: Option<Box<FindingDisposition>>,
        retired: bool,
        outcome: Option<Box<FindingChange>>,
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
        cursor_offset: u64,
    },
}

impl Descriptor {
    pub fn key(&self) -> ContextKey {
        match self {
            Self::Finding { id, .. } | Self::Change { id, .. } | Self::Disposition { id, .. } => {
                ContextKey::Identity(id.clone())
            }
            Self::Section { cursor_offset, .. } => ContextKey::Offset(*cursor_offset),
        }
    }
}

pub struct ReportContext {
    pub report: Option<ReportReference>,
    pub disposition_version: Revision,
    pub items: Vec<Descriptor>,
    pub next: Option<ContextPosition>,
}

pub enum QuoteResolution {
    Missing,
    Ambiguous,
    Unique { offset: u64, length: u64 },
}

pub struct ReportSourceRange {
    pub markdown: String,
    pub scalar_start: u64,
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
    fn read_report_bytes(
        &self,
        actor: Actor,
        run: RunId,
        report: ReportReference,
        bytes: std::ops::Range<u64>,
    ) -> AppFuture<'_, Snapshot<ReportSourceRange>>;
    fn read_report_chunk(
        &self,
        actor: Actor,
        run: RunId,
        report: ReportReference,
        ordinal: u64,
    ) -> AppFuture<'_, Option<Vec<u8>>>;
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
