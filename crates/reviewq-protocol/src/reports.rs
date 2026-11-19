//! Browser projections for accepted report versions and labeled candidates.

use serde::Serialize;

pub use crate::scalar::ReportHashDto;
use crate::{
    GitShaDto, HistoryTurnIdDto, Page, ResourceIdDto, RevisionDto, SafeCountDto, UtcTimestampDto,
};

macro_rules! wire_enum {
    ($name:ident { $($value:ident),+ $(,)? }) => {
        #[derive(Clone, Debug, Serialize)]
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
    pub verdict: Verdict,
    pub semantic_index: SemanticIndex,
    pub finding_count: Option<SafeCountDto>,
    pub change_count: Option<SafeCountDto>,
    pub captured_at: UtcTimestampDto,
    #[serde(flatten)]
    pub stats: ArtifactStats,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReportDelivery {
    pub receipt_id: ResourceIdDto,
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
    },
    Change {
        finding_id: String,
        conclusion: FindingConclusion,
        reason: String,
        checked_sha: GitShaDto,
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

#[derive(Debug, serde::Deserialize)]
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
