use serde::Serialize;

use super::{PatchDiscussion, PatchSearchCoverage};
use crate::{Page, PageCursorDto, PatchRevisionDto, ReviewIdDto, SafeCountDto, Success};

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum NoteKind {
    Inline,
    General,
    Outdated,
    Draft,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DiscussionSearch {
    pub review_id: ReviewIdDto,
    pub revision: PatchRevisionDto,
    pub coverage: PatchSearchCoverage,
    pub drafts_unavailable: bool,
    #[serde(flatten)]
    pub page: Page<DiscussionSearchMatch>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DiscussionSearchMatch {
    pub hit: PageCursorDto,
    pub kind: NoteKind,
    pub author: String,
    pub path: Option<String>,
    pub line: Option<SafeCountDto>,
    #[cfg_attr(feature = "schema", schemars(length(max = 400)))]
    pub snippet: String,
    pub highlight_start: SafeCountDto,
    pub highlight_end: SafeCountDto,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DiscussionDestination {
    pub review_id: ReviewIdDto,
    pub revision: PatchRevisionDto,
    pub thread: PatchDiscussion,
    pub note_id: String,
    pub kind: NoteKind,
    pub parent_discussion_id: Option<String>,
    pub matched: String,
    pub start_utf16: SafeCountDto,
    pub end_utf16: SafeCountDto,
    pub ordinal: SafeCountDto,
}

pub type DiscussionSearchResponse = Success<DiscussionSearch>;
pub type DiscussionDestinationResponse = Success<DiscussionDestination>;
