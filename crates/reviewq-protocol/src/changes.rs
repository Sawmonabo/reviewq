//! Revision-bound patch metadata. The raw patch keeps its own HTTP representation.

use serde::Serialize;

mod approvals;
mod source;
pub use approvals::{
    ApprovalFacts, ApprovalRule, ApprovalView, ApprovalViewResponse, ApprovalWarning, Approver,
};
pub use source::{PatchSourceCursor, PatchSourcePage, PatchSourceResponse, PatchSourceRow};

use crate::scalar::{
    FileIdDto, GitShaDto, PageCursorDto, PatchETagDto, PatchRevisionDto, ReviewIdDto, RevisionDto,
    SafeCountDto,
};
use crate::{Page, Success, UtcTimestampDto};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchFileIndex {
    pub revision: PatchRevisionDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings_revision: Option<RevisionDto>,
    pub base_sha: GitShaDto,
    pub start_sha: GitShaDto,
    pub head_sha: GitShaDto,
    pub page_number: SafeCountDto,
    pub previous_cursor: Option<PageCursorDto>,
    #[serde(flatten)]
    pub page: Page<PatchFile>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SelectedPatch {
    pub file: PatchFile,
    pub ordinal: SafeCountDto,
    pub settings_revision: RevisionDto,
    pub previous_cursor: Option<PageCursorDto>,
    pub next_cursor: Option<PageCursorDto>,
    /// Exact original UTF-8 when the selected complete patch fits the inline limit.
    #[cfg_attr(feature = "schema", schemars(length(max = 131072)))]
    pub text: Option<String>,
}

pub type SelectedPatchResponse = Success<SelectedPatch>;

pub type PatchFileIndexResponse = Success<PatchFileIndex>;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CurrentPatchRevision {
    pub review_id: ReviewIdDto,
    pub revision: PatchRevisionDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings_revision: Option<RevisionDto>,
}

pub type CurrentPatchRevisionResponse = Success<CurrentPatchRevision>;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchPaths {
    pub review_id: ReviewIdDto,
    pub revision: PatchRevisionDto,
    pub partial: bool,
    #[serde(flatten)]
    pub page: Page<PatchPath>,
}

pub type PatchPathsResponse = Success<PatchPaths>;

pub type PatchSearchResponse = Success<PatchSearch>;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchSearch {
    pub review_id: ReviewIdDto,
    pub revision: PatchRevisionDto,
    pub coverage: PatchSearchCoverage,
    #[serde(flatten)]
    pub page: Page<PatchSearchMatch>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum PatchSearchCoverage {
    Reading,
    Complete,
    Limited,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum PatchSide {
    Old,
    New,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchSearchMatch {
    pub file_id: FileIdDto,
    #[serde(rename = "patchETag")]
    pub patch_etag: PatchETagDto,
    #[cfg_attr(feature = "schema", schemars(length(max = 4096)))]
    pub old_path: String,
    #[cfg_attr(feature = "schema", schemars(length(max = 4096)))]
    pub new_path: String,
    pub side: PatchSide,
    pub line: SafeCountDto,
    pub hunk: SafeCountDto,
    pub source_start: SafeCountDto,
    pub source_end: SafeCountDto,
    #[cfg_attr(feature = "schema", schemars(length(max = 448)))]
    pub snippet: String,
    pub highlight_start: SafeCountDto,
    pub highlight_end: SafeCountDto,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchPath {
    pub old_path: String,
    pub new_path: String,
    /// A lookup hint accepted by the checked patch index, not a verified patch address.
    pub lookup_key: FileIdDto,
}

/// One immutable address even when another revision serves identical bytes.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchFile {
    pub review_id: ReviewIdDto,
    pub revision: PatchRevisionDto,
    pub file_id: FileIdDto,
    pub old_path: String,
    pub new_path: String,
    pub renamed: bool,
    pub created: bool,
    pub deleted: bool,
    pub availability: PatchAvailability,
    pub byte_length: Option<SafeCountDto>,
    pub line_count: Option<SafeCountDto>,
    pub max_line_bytes: Option<SafeCountDto>,
    pub additions: Option<SafeCountDto>,
    pub deletions: Option<SafeCountDto>,
    #[serde(rename = "patchETag")]
    pub patch_etag: Option<PatchETagDto>,
    /// Whole original text from this same verified file-page response.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(length(max = 131072)))]
    pub preview: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum PatchAvailability {
    Complete,
    Incomplete,
    Binary,
    ProviderLimited,
    Missing,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchDiscussions {
    pub review_id: ReviewIdDto,
    pub revision: PatchRevisionDto,
    #[serde(flatten)]
    pub page: Page<PatchDiscussion>,
    /// Optional searchable plain text from notes in this same response.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(length(max = 64)))]
    pub text_projections: Option<Vec<PatchDiscussionText>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchDiscussionText {
    #[cfg_attr(feature = "schema", schemars(length(max = 20)))]
    pub note_id: String,
    #[cfg_attr(feature = "schema", schemars(length(max = 32768)))]
    pub text: String,
}

pub type PatchDiscussionsResponse = Success<PatchDiscussions>;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchDiscussion {
    pub discussion_id: String,
    pub anchor: Option<PatchDiscussionAnchor>,
    pub outdated: bool,
    pub web_url: String,
    pub our_participation: bool,
    pub resolution: DiscussionResolution,
    pub notes: Vec<PatchDiscussionNote>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum DiscussionResolution {
    Open,
    Resolved,
    Comment,
    Unknown,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchDiscussionAnchor {
    pub base_sha: GitShaDto,
    pub start_sha: GitShaDto,
    pub head_sha: GitShaDto,
    pub position_type: DiscussionPosition,
    pub old_path: String,
    pub new_path: String,
    pub old_line: Option<SafeCountDto>,
    pub new_line: Option<SafeCountDto>,
    pub range: Option<PatchDiscussionRange>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum DiscussionPosition {
    Text,
    File,
    Other,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchDiscussionRange {
    pub start: PatchDiscussionLine,
    pub end: PatchDiscussionLine,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchDiscussionLine {
    pub old_line: Option<SafeCountDto>,
    pub new_line: Option<SafeCountDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchDiscussionNote {
    pub note_id: String,
    pub web_url: String,
    pub author: String,
    pub author_id: Option<crate::UpstreamIdDto>,
    pub ours: bool,
    pub body: String,
    pub created_at: Option<UtcTimestampDto>,
    pub resolvable: Option<bool>,
    pub resolved: Option<bool>,
    pub resolved_by: Option<crate::UpstreamIdDto>,
}
