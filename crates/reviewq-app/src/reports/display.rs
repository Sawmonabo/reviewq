//! Native source spans for a captured report. Browser admission still uses its own validator.

mod cutter;

pub use cutter::{DisplayBlock, DisplayBlockKind, DisplayFallback};

use super::ReportSummary;

pub const DISPLAY_INDEX_FORMAT: u32 = 1;

pub enum DisplayIndex {
    Indexed(Vec<DisplayBlock>),
    Fallback(DisplayFallback),
}

/// Jump by original UTF-8 byte offset, or move on either side of a returned block.
#[derive(Clone, Copy)]
pub enum DisplayWindowPosition {
    AtByte(u64),
    Before(u32),
    After(u32),
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
        Ok(blocks) => DisplayIndex::Indexed(blocks),
        Err(reason) => DisplayIndex::Fallback(reason),
    }
}
