//! Browser projections for accepted report versions and labeled candidates.

use serde::Serialize;

pub use crate::scalar::ReportHashDto;
use crate::{
    GitShaDto, HistoryTurnIdDto, Page, ResourceIdDto, RevisionDto, SafeCountDto, UtcTimestampDto,
};

macro_rules! wire_enum {
    ($name:ident { $($value:ident),+ $(,)? }) => {
        #[derive(Clone, Debug, serde::Deserialize, Serialize)]
        #[serde(rename_all = "snake_case")]
        #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
        pub enum $name { $($value),+ }
    };
}

wire_enum!(Verdict {
    Approve,
    RequestChanges,
    Blocked,
    NotSpecified
});
wire_enum!(Coverage {
    FullMr,
    SelectedThreads
});
wire_enum!(SemanticIndex { Ready, SourceOnly });
wire_enum!(DeliveryState {
    Preparing,
    Saving,
    Failed,
    Saved
});
wire_enum!(Publication {
    Withheld,
    Authorized,
    Published
});
wire_enum!(FindingConclusion {
    Fixed,
    ClarifiedWithdrawn,
    Superseded,
    PartlyAddressed,
    StillPresent,
    Unverified,
});
wire_enum!(OffsetUnit { UnicodeCodepoints });

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ArtifactStats {
    pub byte_length: SafeCountDto,
    pub total_code_points: SafeCountDto,
    pub line_count: SafeCountDto,
    pub max_line_bytes: SafeCountDto,
    pub complete: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReportReference {
    pub receipt_id: ResourceIdDto,
    pub sha256: ReportHashDto,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReportSummary {
    #[serde(flatten)]
    pub reference: ReportReference,
    pub original_sha256: Option<ReportHashDto>,
    pub native_provenance: bool,
    pub parent_receipt_id: Option<ResourceIdDto>,
    pub attention_receipt_id: ResourceIdDto,
    pub reviewed_sha: Option<GitShaDto>,
    pub revision_reason: Option<String>,
    pub full_mr_reviewed_sha: Option<GitShaDto>,
    pub coverage: Coverage,
    pub coverage_source_sha: Option<GitShaDto>,
    pub coverage_target_sha: Option<GitShaDto>,
    pub verdict: Verdict,
    pub semantic_index: SemanticIndex,
    pub finding_count: Option<SafeCountDto>,
    pub change_count: Option<SafeCountDto>,
    /// Active findings carried from an earlier version whose card this report omits.
    pub carried_finding_count: SafeCountDto,
    pub captured_at: UtcTimestampDto,
    #[serde(flatten)]
    pub stats: ArtifactStats,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReportDelivery {
    pub receipt_id: Option<ResourceIdDto>,
    pub state: DeliveryState,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReportView {
    pub report: Option<ReportSummary>,
    pub delivery: Option<ReportDelivery>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<DocumentDisplay>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DrawingHint {
    pub reference: crate::PageCursorDto,
    pub source_sha256: ReportHashDto,
    pub recipe: String,
    pub font: ReportHashDto,
}

/// Optional derivatives of the accompanying verified Markdown.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DocumentDisplay {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "schema", schemars(length(max = 8)))]
    pub drawings: Vec<DrawingHint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<PreparedTable>,
}

mod table;
pub use table::{
    PreparedTable, ReportTable, ReportTableContent, ReportTableRange, ReportTableResponse,
    TableColumn, TableInline, TablePage,
};

wire_enum!(DisplayBlockKind {
    Paragraph,
    Heading,
    BlockQuote,
    Code,
    List,
    Rule,
    Table
});
wire_enum!(DisplayFallback {
    DocumentBudget,
    ParserBudget,
    DocumentContext,
    LayoutBudget,
    UnsupportedSource,
    InvalidBoundary,
    ParserFailed,
    IncompleteSource,
    CacheUnavailable,
});

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DisplayBlock {
    pub byte_start: SafeCountDto,
    pub byte_end: SafeCountDto,
    pub scalar_start: SafeCountDto,
    pub utf16_start: SafeCountDto,
    pub kind: DisplayBlockKind,
    pub heading_level: Option<SafeCountDto>,
    pub markdown: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<DocumentDisplay>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DisplayLayoutBlock {
    pub byte_start: SafeCountDto,
    pub byte_end: SafeCountDto,
    pub scalar_start: SafeCountDto,
    pub utf16_start: SafeCountDto,
    pub kind: DisplayBlockKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading_level: Option<SafeCountDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading_markdown: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DisplayLayout {
    pub byte_length: SafeCountDto,
    pub scalar_length: SafeCountDto,
    pub utf16_length: SafeCountDto,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 512)))]
    pub blocks: Vec<DisplayLayoutBlock>,
}

#[derive(Debug, Serialize)]
#[serde(
    tag = "state",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum DisplayResult {
    Ready {
        total_blocks: SafeCountDto,
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 32)))]
        blocks: Vec<DisplayBlock>,
        previous_cursor: Option<crate::PageCursorDto>,
        next_cursor: Option<crate::PageCursorDto>,
        #[serde(skip_serializing_if = "Option::is_none")]
        layout: Option<DisplayLayout>,
    },
    Building,
    Fallback {
        reason: DisplayFallback,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReportWindow {
    pub report: ReportSummary,
    pub index_format: SafeCountDto,
    pub result: DisplayResult,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReportHistory<I = ReportSummary> {
    #[serde(flatten)]
    pub page: Page<I>,
    pub current_receipt_id: Option<ResourceIdDto>,
    pub delivery: Option<ReportDelivery>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReportSection {
    #[serde(flatten)]
    pub report: ReportReference,
    pub complete: bool,
    pub original_sha256: Option<ReportHashDto>,
    pub text: String,
    pub offset: SafeCountDto,
    pub next_offset: Option<SafeCountDto>,
    pub total_code_points: SafeCountDto,
    pub offset_unit: OffsetUnit,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SourceRange {
    pub offset: SafeCountDto,
    pub length: SafeCountDto,
    pub sha256: ReportHashDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<Box<SourceOrigin>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SourceOrigin {
    pub report: ReportReference,
    pub checked_sha: Option<GitShaDto>,
}

#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Descriptor {
    Finding {
        finding_id: String,
        title: String,
        severity: Option<String>,
        blocking: Option<bool>,
        source: Option<SourceRange>,
        /// The earlier receipt whose card a carried finding keeps.
        #[serde(skip_serializing_if = "Option::is_none")]
        carried_from: Option<Box<SourceOrigin>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Box<FindingDetails>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        disposition: Option<Box<FindingDisposition>>,
    },
    Change {
        finding_id: String,
        conclusion: FindingConclusion,
        reason: String,
        checked_sha: Option<GitShaDto>,
        discussion_ids: Vec<ResourceIdDto>,
    },
    Disposition {
        finding_id: String,
        blocking: Option<bool>,
        publication: Option<Publication>,
        reason: String,
    },
    Section {
        title: String,
        source: SourceRange,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FindingDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<FindingLocation>,
    pub consequence: String,
    pub evidence: FindingBasis,
    pub checked_sha: Option<GitShaDto>,
    pub evidence_sha256: ReportHashDto,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FindingDisposition {
    pub blocking: Option<bool>,
    pub publication: Option<Publication>,
    pub reason: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReportContext<I = Descriptor> {
    pub base_report: Option<ReportReference>,
    pub user_disposition_version: RevisionDto,
    #[serde(flatten)]
    pub page: Page<I>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CandidateReport {
    pub accepted: bool,
    #[serde(flatten)]
    pub section: ReportSection,
    pub stats: ArtifactStats,
    pub turn_id: HistoryTurnIdDto,
    pub call_id: ResourceIdDto,
    pub declaration_sha256: ReportHashDto,
    pub reconciled_by_batch_id: Option<ResourceIdDto>,
    pub reason: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum PublicationChoice {
    Withheld,
    Authorized,
    Published,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DispositionPatch {
    pub blocking: Option<bool>,
    pub publication: Option<PublicationChoice>,
    pub reason: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DispositionRequest {
    pub submission_id: crate::SubmissionIdDto,
    pub expected_version: RevisionDto,
    pub receipt_id: ResourceIdDto,
    pub finding_id: ResourceIdDto,
    pub disposition: DispositionPatch,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DispositionReceipt {
    pub receipt_id: ResourceIdDto,
    pub user_disposition_version: RevisionDto,
    pub finding: Descriptor,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ViewedRequest {
    pub submission_id: crate::SubmissionIdDto,
    pub receipt_id: ResourceIdDto,
    pub rendered_batch_id: Option<ResourceIdDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ViewedReceipt {
    pub receipt_id: ResourceIdDto,
    pub previous_receipt_id: Option<ResourceIdDto>,
    pub rendered_batch_id: Option<ResourceIdDto>,
}

#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum QuoteUnit {
    UnicodeCodepoints,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct QuoteRef {
    pub offset: SafeCountDto,
    pub length: SafeCountDto,
    pub unit: QuoteUnit,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct QuoteResolveRequest {
    pub sha256: ReportHashDto,
    pub text: crate::MessageTextDto,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum QuoteResolution {
    Missing,
    Ambiguous,
    Unique {
        offset: SafeCountDto,
        length: SafeCountDto,
        unit: QuoteUnit,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ResolvedQuote {
    pub receipt_id: ResourceIdDto,
    pub sha256: ReportHashDto,
    pub result: QuoteResolution,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FeedbackReference {
    pub receipt_id: ResourceIdDto,
    pub report_hash: ReportHashDto,
    #[serde(default)]
    pub quote: String,
    pub quote_ref: Option<QuoteRef>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FeedbackRequest {
    pub submission_id: crate::SubmissionIdDto,
    pub receipt_id: ResourceIdDto,
    pub report_hash: ReportHashDto,
    pub quote: String,
    pub quote_ref: Option<QuoteRef>,
    pub text: crate::MessageTextDto,
}

mod handoff;
pub use handoff::{
    AcceptedReference, AcceptedReport, Assessment, CodexCommand, FindingBasis, FindingBasisKind,
    FindingEdit, FindingLocation, Handoff, HandoffCommand, HandoffReceipt, HandoffState,
    NativeContext, NativeReportAddress, REPORT_AMENDMENT_SOURCE_MESSAGE,
    REPORT_HANDOFF_DESCRIPTION, REPORT_HANDOFF_EXAMPLE, REPORT_HANDOFF_SCHEMA,
    ReportHandoffFieldError, ReportHandoffInput, ReportRevision, ReportSource, ReportVerdict,
    StructuredFinding, StructuredReport, limits, report_amendment_source_error,
    validate_report_handoff,
};
