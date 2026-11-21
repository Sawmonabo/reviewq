use reviewq_domain::identity::NativeInputId;

use super::FileContentRef;
use crate::reading::Literal;

pub mod index;

pub const MAX_SEARCH_HITS: usize = 16;

pub struct HistorySearch {
    pub literal: Literal,
    pub from: Option<SearchCursor>,
    pub reverse: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceObservation {
    pub high_water: u64,
    pub version: [u8; 32],
}

/// Scan order within a complete native record. Part zero represents combined prose.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SearchPoint {
    pub record: u64,
    pub part: u16,
    pub byte: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchCursor {
    pub observation: SourceObservation,
    pub point: SearchPoint,
}

#[derive(Debug)]
pub struct SearchHit {
    pub reference: FileContentRef,
    pub point: SearchPoint,
    pub tool_call: Option<NativeInputId>,
    pub before: String,
    pub matched: String,
    pub after: String,
    pub output_offset: u64,
    pub start_utf16: u64,
    pub end_utf16: u64,
    pub markdown: bool,
    pub ordinal: u64,
}

pub struct SearchPage {
    pub generation: String,
    pub observation: SourceObservation,
    pub hits: Vec<SearchHit>,
    pub next: Option<SearchPoint>,
    pub partial: bool,
}
