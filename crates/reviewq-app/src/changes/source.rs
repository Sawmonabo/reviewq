use super::{PatchRevision, PatchSide};

pub const PATCH_SOURCE_ROWS: usize = 128;
pub const PATCH_SOURCE_BYTES: usize = 128 * 1024;

pub struct PatchSourceRequest {
    pub revision: String,
    pub file_id: String,
    pub etag: String,
    pub target: PatchSourceTarget,
}

#[derive(Clone, Copy)]
pub enum PatchSourceTarget {
    Row { row: usize, byte: Option<usize> },
    Byte(usize),
    Line { side: PatchSide, line: u32 },
    Before(usize),
    End,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PatchSourceCursor {
    /// Zero-based physical row.
    pub row: usize,
    /// Absolute UTF-8 byte offset in the original patch.
    pub byte: usize,
}

pub struct PatchSourcePage {
    pub revision: PatchRevision,
    pub file_id: String,
    pub etag: String,
    pub window: PatchSourceWindow,
}

pub struct PatchSourceWindow {
    pub total: PatchSourceCursor,
    pub rows: Vec<PatchSourceRow>,
    pub next: Option<PatchSourceCursor>,
}

pub struct PatchSourceRow {
    pub row: usize,
    /// Source byte of the hunk header, when the row belongs to a hunk.
    pub hunk: Option<usize>,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub row_start: usize,
    pub row_end: usize,
    pub source_start: usize,
    pub source_end: usize,
    pub text: String,
}
