//! Native source spans for a captured report. Browser admission still uses its own validator.

mod cutter;

pub use cutter::{DisplayBlock, DisplayBlockKind, DisplayFallback};

use super::ReportSummary;

pub const DISPLAY_INDEX_FORMAT: u32 = 2;
pub const MAX_DISPLAY_BLOCKS: usize = 512;
pub const MAX_DISPLAY_HEADING_BYTES: usize = 1024;
pub const MAX_DISPLAY_HEADINGS_BYTES: usize = 16 * 1024;

pub enum DisplayIndex {
    Indexed(DisplayLayout),
    Fallback(DisplayFallback),
}

/// Jump by byte offset, move on either side of a returned block, or read an exact span range.
#[derive(Clone, Copy)]
pub enum DisplayWindowPosition {
    AtByte { offset: u64, with_layout: bool },
    Exact { from: u64, to: u64 },
    Before(u32),
    After(u32),
}

pub struct DisplayLayout {
    pub byte_length: u32,
    pub scalar_length: u32,
    pub utf16_length: u32,
    pub blocks: Vec<DisplayBlock>,
}

pub struct DisplayWindow {
    pub report: ReportSummary,
    pub state: DisplayWindowState,
}

pub enum DisplayWindowState {
    Building,
    Fallback(DisplayFallback),
    Ready {
        total_blocks: u32,
        blocks: Vec<DisplayWindowBlock>,
        previous: Option<u32>,
        next: Option<u32>,
        layout: Option<DisplayLayout>,
    },
}

pub struct DisplayWindowBlock {
    pub span: DisplayBlock,
    pub markdown: String,
}

/// Index a complete, verified UTF-8 buffer already admitted by the rich source envelope.
/// Run this operation on a blocking worker.
pub fn build_display_index(source: &str) -> DisplayIndex {
    match cutter::cut(source) {
        Ok(layout) => DisplayIndex::Indexed(layout),
        Err(reason) => DisplayIndex::Fallback(reason),
    }
}
