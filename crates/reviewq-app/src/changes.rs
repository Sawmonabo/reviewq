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
    pub next_page: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PatchAvailability {
    Complete,
    Incomplete,
    Binary,
    ProviderLimited,
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
}

#[derive(Clone, Debug)]
pub struct PatchMetrics {
    pub byte_length: usize,
    pub line_count: usize,
    pub max_line_bytes: usize,
    pub etag: String,
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
    pub old_path: String,
    pub new_path: String,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub range: Option<DiscussionRange>,
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
    pub approved_at_ms: Option<i64>,
}

#[derive(Debug)]
pub struct ApprovalRule {
    pub id: u64,
    pub name: String,
    pub approved: bool,
    pub required: u32,
}

pub trait ChangeSource: Send + Sync {
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
        selected_file: Option<String>,
    ) -> AppFuture<'_, SourcePage<PatchFile>>;

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
