//! Browser projections for published report versions.

use serde::Serialize;

pub use crate::scalar::ReportHashDto;
use crate::{
    DiscussionIdDto, FindingIdDto, GitShaDto, Page, ResourceIdDto, RevisionDto, SafeCountDto,
    UtcTimestampDto,
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
wire_enum!(DeliveryState { Failed, Saved });
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
wire_enum!(FindingKind {
    Functional,
    Quality
});
wire_enum!(FindingBasisKind { Observed, Gap });

/// A finding's severity, most severe first.
#[derive(Clone, Debug, serde::Deserialize, Serialize)]
#[serde(rename_all = "UPPERCASE")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Severity {
    P0,
    P1,
    P2,
    P3,
}

/// Longest location path, in characters.
pub const MAX_PATH_CHARS: usize = 512;
/// Longest issue, consequence, evidence text or proposed comment: 8 KiB of UTF-8. The schema
/// states the same number as its character maximum.
pub const MAX_FINDING_TEXT_BYTES: usize = 8 * 1024;
/// Longest outcome or assessment reason, in characters.
pub const MAX_REASON_CHARS: usize = 1024;
/// Most finding IDs one assessment names.
pub const MAX_ASSESSMENT_FINDING_IDS: usize = 100;

/// A file a finding points at, with an optional 1-based line.
#[derive(Debug, serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FindingLocation {
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = MAX_PATH_CHARS)))]
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 4294967295_u32)))]
    pub line: Option<u32>,
}

/// What the agent confirmed (`observed`) or could not verify (`gap`).
#[derive(Debug, serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FindingBasis {
    pub kind: FindingBasisKind,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = MAX_FINDING_TEXT_BYTES)))]
    pub text: String,
}

/// The reviewed revision: `{sha}` with the exact full SHA, or `{sha: null, reason}` when it is
/// unknown.
#[derive(Debug, serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReportRevision {
    /// The full SHA of the reviewed commit, or null when it is unknown.
    #[cfg_attr(
        feature = "schema",
        schemars(required, schema_with = "crate::scalar::nullable_git_sha_schema")
    )]
    pub sha: Option<GitShaDto>,
    /// Why the reviewed commit is unknown. Only with a null sha.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "String", length(min = 1, max = MAX_REASON_CHARS))
    )]
    pub reason: Option<String>,
}

/// The lead's conclusion on one selected discussion of a follow-up batch.
#[derive(Debug, serde::Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Assessment {
    pub discussion_id: DiscussionIdDto,
    pub conclusion: FindingConclusion,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = MAX_REASON_CHARS)))]
    pub reason: String,
    #[cfg_attr(feature = "schema", schemars(length(max = MAX_ASSESSMENT_FINDING_IDS)))]
    pub finding_ids: Vec<FindingIdDto>,
    pub next_action: crate::followups::NextAction,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ArtifactStats {
    pub byte_length: SafeCountDto,
    pub total_code_points: SafeCountDto,
    pub line_count: SafeCountDto,
    pub max_line_bytes: SafeCountDto,
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
    pub parent_receipt_id: Option<ResourceIdDto>,
    pub attention_receipt_id: ResourceIdDto,
    pub reviewed_sha: Option<GitShaDto>,
    pub revision_reason: Option<String>,
    pub full_mr_reviewed_sha: Option<GitShaDto>,
    pub coverage: Coverage,
    pub coverage_source_sha: Option<GitShaDto>,
    pub coverage_target_sha: Option<GitShaDto>,
    pub verdict: Verdict,
    pub finding_count: SafeCountDto,
    pub change_count: SafeCountDto,
    pub published_at: UtcTimestampDto,
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
        severity: Severity,
        blocking: bool,
        source: SourceRange,
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

/// What a finding card in the Findings panel shows beside its title and badges. The rest of
/// the finding is in the report text at the descriptor's `source`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FindingDetails {
    pub locations: Vec<FindingLocation>,
    pub consequence: String,
    pub evidence: FindingBasis,
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
    pub finding_id: FindingIdDto,
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
