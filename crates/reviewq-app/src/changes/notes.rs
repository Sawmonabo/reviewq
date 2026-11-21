use std::sync::Arc;

use reviewq_domain::identity::{EventCursor, ReviewId, Revision};

use crate::access::AppFuture;
use crate::reading::TextMatch;

use super::{Discussion, DiscussionAnchor, PatchRevision};

pub struct DiscussionScope {
    pub revision: String,
    pub epoch: EventCursor,
    pub journal_head: EventCursor,
    pub observation: Option<String>,
}

pub struct DiscussionPageRequest {
    pub scope: DiscussionScope,
    pub page: u32,
    pub limit: u16,
    pub allow_partial: bool,
}

pub struct DiscussionPage {
    pub revision: PatchRevision,
    pub observation: String,
    pub items: Arc<[Discussion]>,
    pub next_page: Option<u32>,
    pub limited: bool,
}

#[derive(Eq, PartialEq)]
pub struct DiscussionDraft {
    pub id: u64,
    pub fingerprint: [u8; 32],
    pub account: u64,
    pub author: String,
    pub body: String,
    pub parent: Option<String>,
    pub anchor: Option<DiscussionAnchor>,
    pub outdated: bool,
}

pub struct DiscussionDrafts {
    pub observation: String,
    pub items: Arc<[DiscussionDraft]>,
    pub limited: bool,
}

pub enum NoteAddress {
    Published(String),
    Draft(u64),
}

pub enum DiscussionTarget {
    Published(Discussion),
    Draft(DiscussionDraft),
}

/// Only normalized observation metadata is retained in the event journal and store.
pub trait DiscussionEpochStore: Send + Sync {
    fn discussion_epoch(
        &self,
        review: ReviewId,
        settings: Revision,
    ) -> AppFuture<'_, DiscussionEpoch>;

    fn invalidate_discussions(
        &self,
        review: ReviewId,
        settings: Revision,
        expected: EventCursor,
        change: DiscussionInvalidation,
    ) -> AppFuture<'_, ()>;
}

pub struct DiscussionEpoch {
    pub cursor: EventCursor,
    pub denied: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum DiscussionInvalidation {
    Changed,
    Denied,
    Restored,
}

pub struct NoteOccurrence {
    pub span: TextMatch,
    pub matched: String,
    pub ordinal: usize,
    pub snippet: String,
    pub highlight_start: usize,
    pub highlight_end: usize,
}

/// Pick the largest bounded page whose first row is this exact provider offset.
pub fn discussion_page_at(offset: u32, maximum: u16) -> (u32, u16) {
    let limit = [40, 20, 10, 5, 1]
        .into_iter()
        .find(|size| *size <= maximum && offset.is_multiple_of(u32::from(*size)))
        .unwrap_or(1);
    (offset / u32::from(limit) + 1, limit)
}

pub fn note_occurrence(text: &str, span: TextMatch) -> NoteOccurrence {
    let start = span.start.byte;
    let end = span.end.byte;
    let matched = &text[start..end];
    let left = text[..start]
        .char_indices()
        .rev()
        .nth(47)
        .map_or(0, |(offset, _)| offset);
    let right = text[end..]
        .char_indices()
        .nth(72)
        .map_or(text.len(), |(offset, _)| end + offset);
    let ordinal = text[..start]
        .char_indices()
        .filter(|(offset, _)| text[*offset..].starts_with(matched))
        .count();
    let highlight_start = text[left..start].encode_utf16().count();
    NoteOccurrence {
        span,
        matched: matched.to_owned(),
        ordinal,
        snippet: text[left..right].to_owned(),
        highlight_start,
        highlight_end: highlight_start + span.end.utf16 - span.start.utf16,
    }
}
