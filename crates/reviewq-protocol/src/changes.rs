//! Revision-bound patch metadata. The raw patch keeps its own HTTP representation.

use serde::Serialize;

use crate::scalar::{
    FileIdDto, GitShaDto, PatchETagDto, PatchRevisionDto, ReviewIdDto, SafeCountDto,
};
use crate::{Page, Success, UtcTimestampDto};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchFileIndex {
    pub revision: PatchRevisionDto,
    pub base_sha: GitShaDto,
    pub start_sha: GitShaDto,
    pub head_sha: GitShaDto,
    #[serde(flatten)]
    pub page: Page<PatchFile>,
}

pub type PatchFileIndexResponse = Success<PatchFileIndex>;

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
    #[serde(rename = "patchETag")]
    pub patch_etag: Option<PatchETagDto>,
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
}

pub type PatchDiscussionsResponse = Success<PatchDiscussions>;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchDiscussion {
    pub discussion_id: String,
    pub anchor: Option<PatchDiscussionAnchor>,
    pub outdated: bool,
    pub notes: Vec<PatchDiscussionNote>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchDiscussionAnchor {
    pub old_path: String,
    pub new_path: String,
    pub old_line: Option<SafeCountDto>,
    pub new_line: Option<SafeCountDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchDiscussionNote {
    pub note_id: String,
    pub author: String,
    pub body: String,
    pub created_at: Option<UtcTimestampDto>,
    pub resolved: bool,
}
