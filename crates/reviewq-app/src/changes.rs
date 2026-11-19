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

#[derive(Debug)]
pub struct DiscussionAnchor {
    pub old_path: String,
    pub new_path: String,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
}

#[derive(Debug)]
pub struct DiscussionNote {
    pub id: u64,
    pub author: String,
    pub body: String,
    pub created_at_ms: Option<i64>,
    pub resolved: bool,
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
