//! Revision-bound source reads for a selected merge request.

use reviewq_domain::identity::{GitSha, MrIdentity};

use crate::access::AppFuture;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PatchRevision {
    pub id: String,
    pub base: GitSha,
    pub start: GitSha,
    pub head: GitSha,
}

#[derive(Debug)]
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

#[derive(Debug)]
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

#[derive(Debug)]
pub struct PatchMetrics {
    pub byte_length: usize,
    pub line_count: usize,
    pub max_line_bytes: usize,
    pub etag: String,
}

#[derive(Debug)]
pub struct PatchBytes {
    pub revision: PatchRevision,
    pub file: PatchFile,
    pub bytes: Vec<u8>,
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

pub trait ChangeSource: Send + Sync {
    fn files(
        &self,
        source: MrIdentity,
        revision: Option<String>,
        page: u32,
        limit: u16,
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
