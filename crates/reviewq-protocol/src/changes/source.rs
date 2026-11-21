use serde::Serialize;

use crate::{
    FileIdDto, GitShaDto, PatchETagDto, PatchRevisionDto, ReviewIdDto, SafeCountDto, Success,
};

pub type PatchSourceResponse = Success<PatchSourcePage>;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchSourcePage {
    pub review_id: ReviewIdDto,
    pub revision: PatchRevisionDto,
    pub file_id: FileIdDto,
    pub base_sha: GitShaDto,
    pub start_sha: GitShaDto,
    pub head_sha: GitShaDto,
    #[serde(rename = "patchETag")]
    pub patch_etag: PatchETagDto,
    pub total_bytes: SafeCountDto,
    pub total_rows: SafeCountDto,
    #[cfg_attr(feature = "schema", schemars(length(max = 128)))]
    pub rows: Vec<PatchSourceRow>,
    pub next: Option<PatchSourceCursor>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchSourceRow {
    pub row: SafeCountDto,
    pub hunk: Option<SafeCountDto>,
    pub old_line: Option<SafeCountDto>,
    pub new_line: Option<SafeCountDto>,
    /// The complete row's original half-open byte range.
    pub row_start: SafeCountDto,
    pub row_end: SafeCountDto,
    /// The returned fragment's original half-open byte range.
    pub source_start: SafeCountDto,
    pub source_end: SafeCountDto,
    #[cfg_attr(feature = "schema", schemars(length(max = 131072)))]
    pub text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PatchSourceCursor {
    pub row: SafeCountDto,
    pub byte: SafeCountDto,
}
