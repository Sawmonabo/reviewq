//! Revision-bound source reads for a selected merge request.

use std::sync::Arc;

use reviewq_domain::identity::{GitSha, MrIdentity};

use crate::access::AppFuture;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PatchRevision {
    pub id: String,
    pub base: GitSha,
    pub start: GitSha,
    pub head: GitSha,
}

#[derive(Clone, Debug)]
pub struct SourcePage<T> {
    pub revision: PatchRevision,
    pub items: Vec<T>,
    pub page: u32,
    pub next_page: Option<u32>,
}

/// One REST-verified selection, with an optional small whole-file projection.
pub struct SelectedPatch {
    pub revision: PatchRevision,
    pub file: PatchFile,
    pub ordinal: u32,
    pub next: Option<u32>,
    pub text: Option<String>,
}

#[derive(Debug)]
pub struct PatchPath {
    pub old_path: String,
    pub new_path: String,
    pub lookup_key: String,
}

#[derive(Debug)]
pub struct PatchPathCursor {
    pub page: u32,
    pub after: String,
    pub partial: bool,
}

#[derive(Debug)]
pub struct PatchPathPage {
    pub revision: PatchRevision,
    pub items: Vec<PatchPath>,
    pub next: Option<PatchPathCursor>,
    pub partial: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PatchFileSelection {
    Exact(String),
    PathHint(String),
}

impl PatchFileSelection {
    pub fn key(&self) -> &str {
        match self {
            Self::Exact(key) | Self::PathHint(key) => key,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PatchAvailability {
    Complete,
    Incomplete,
    Binary,
    ProviderLimited,
}

/// Edge of a verified file page the reader will enter while scrolling.
#[derive(Clone, Copy, Debug)]
pub enum PatchPageEdge {
    Start,
    End,
}

#[derive(Clone, Debug)]
pub struct PatchFile {
    pub id: String,
    pub old_path: String,
    pub new_path: String,
    pub renamed: bool,
    pub created: bool,
    pub deleted: bool,
    pub availability: PatchAvailability,
    pub metrics: Option<PatchMetrics>,
    /// Original UTF-8 projected only for an explicitly requested page edge.
    pub preview: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PatchMetrics {
    pub byte_length: usize,
    pub line_count: usize,
    pub max_line_bytes: usize,
    pub line_changes: Option<LineChanges>,
    pub etag: String,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LineChanges {
    pub additions: usize,
    pub deletions: usize,
}

#[derive(Clone)]
pub struct PatchBytes {
    pub revision: PatchRevision,
    pub file: PatchFile,
    pub body: Arc<dyn PatchContent>,
}

pub trait PatchContent: Send + Sync {
    fn read(&self, offset: usize, length: usize) -> AppFuture<'_, Vec<u8>>;
}

#[derive(Debug)]
pub struct Discussion {
    pub id: String,
    pub anchor: Option<DiscussionAnchor>,
    pub outdated: bool,
    pub notes: Vec<DiscussionNote>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscussionResolution {
    Open,
    Resolved,
    Comment,
    Unknown,
}

impl DiscussionResolution {
    pub fn from_notes(notes: impl IntoIterator<Item = (Option<bool>, Option<bool>)>) -> Self {
        let mut saw_resolved = false;
        let mut saw_comment = false;
        let mut uncertain = false;
        for (resolvable, resolved) in notes {
            match (resolvable, resolved) {
                (Some(true), Some(false)) => return Self::Open,
                (Some(true), Some(true)) => saw_resolved = true,
                (Some(false), _) => saw_comment = true,
                _ => uncertain = true,
            }
        }
        if uncertain {
            Self::Unknown
        } else if saw_resolved {
            Self::Resolved
        } else if saw_comment {
            Self::Comment
        } else {
            Self::Unknown
        }
    }

    pub fn resolved(self) -> Option<bool> {
        match self {
            Self::Open => Some(false),
            Self::Resolved => Some(true),
            Self::Comment | Self::Unknown => None,
        }
    }
}

#[derive(Debug)]
pub struct DiscussionAnchor {
    pub base_sha: GitSha,
    pub start_sha: GitSha,
    pub head_sha: GitSha,
    pub position_type: DiscussionPosition,
    pub old_path: String,
    pub new_path: String,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub range: Option<DiscussionRange>,
}

#[derive(Debug)]
pub enum DiscussionPosition {
    Text,
    File,
    Other,
}

#[derive(Debug)]
pub struct DiscussionRange {
    pub start: DiscussionLine,
    pub end: DiscussionLine,
}

#[derive(Debug)]
pub struct DiscussionLine {
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
}

#[derive(Debug)]
pub struct DiscussionNote {
    pub id: u64,
    pub author: String,
    pub author_id: Option<u64>,
    pub body: String,
    pub created_at_ms: Option<i64>,
    pub resolvable: Option<bool>,
    pub resolved: Option<bool>,
    pub resolved_by: Option<u64>,
}

#[derive(Debug)]
pub struct ApprovalView {
    pub head: GitSha,
    pub observed_at_ms: i64,
    pub personal_approval: Option<bool>,
    pub approvals: Option<ApprovalFacts>,
    pub rules: Option<Vec<ApprovalRule>>,
}

#[derive(Debug)]
pub struct ApprovalFacts {
    pub approved: Option<bool>,
    pub required: Option<u32>,
    pub left: Option<u32>,
    pub approvers: Vec<Approver>,
}

#[derive(Debug)]
pub struct Approver {
    pub id: u64,
    pub username: Option<String>,
    pub name: Option<String>,
    pub approved_at_ms: Option<i64>,
}

#[derive(Debug)]
pub struct ApprovalRule {
    pub id: u64,
    pub name: String,
    pub approved: bool,
    pub required: u32,
    pub approved_count: Option<usize>,
}

pub trait ChangeSource: Send + Sync {
    fn revision(&self, source: MrIdentity) -> AppFuture<'_, PatchRevision>;

    fn approvals(
        &self,
        source: MrIdentity,
        expected_head: Option<GitSha>,
        additional_reviewers: Vec<u64>,
    ) -> AppFuture<'_, ApprovalView>;

    fn files(
        &self,
        source: MrIdentity,
        revision: Option<String>,
        page: u32,
        limit: u16,
        selected_file: Option<PatchFileSelection>,
        edge: Option<PatchPageEdge>,
    ) -> AppFuture<'_, SourcePage<PatchFile>>;

    fn selected(
        &self,
        source: MrIdentity,
        revision: String,
        ordinal: u32,
        hint: Option<String>,
    ) -> AppFuture<'_, SelectedPatch>;

    fn paths(
        &self,
        source: MrIdentity,
        project_path: String,
        revision: String,
        cursor: Option<PatchPathCursor>,
    ) -> AppFuture<'_, PatchPathPage>;

    fn patch(
        &self,
        source: MrIdentity,
        revision: String,
        file: String,
    ) -> AppFuture<'_, PatchBytes>;

    fn discussions(
        &self,
        source: MrIdentity,
        revision: String,
        page: u32,
        limit: u16,
    ) -> AppFuture<'_, SourcePage<Discussion>>;
}
