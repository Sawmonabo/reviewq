//! Native source spans for a published report version. Browser admission still uses its own validator.

mod cards;
mod cutter;

pub use cutter::{DisplayBlock, DisplayBlockKind, DisplayFallback, HtmlRendering};

use super::ReportSummary;

/// Format 3 adds each finding card's leading Markdown to the layout.
pub const DISPLAY_INDEX_FORMAT: u32 = 3;
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

/// Index a complete, verified UTF-8 buffer already admitted by the rich source envelope,
/// for a reader that renders raw HTML as markup, such as a transcript's: any HTML falls back
/// to the whole document. Run this operation on a blocking worker.
pub fn build_display_index(source: &str) -> DisplayIndex {
    display_index(source, HtmlRendering::Markup)
}

/// Index a published report version's text, whose reader shows raw HTML as text, so HTML
/// keeps the windowed reader. Each finding card's card line and Location field also carry
/// their leading Markdown for the findings list. Otherwise as `build_display_index`.
pub fn build_report_display_index(source: &str) -> DisplayIndex {
    let mut index = display_index(source, HtmlRendering::Text);
    if let DisplayIndex::Indexed(layout) = &mut index {
        cards::mark(source, &mut layout.blocks);
    }
    index
}

/// Index `source` for a reader that renders raw HTML as `html` says: `build_display_index`
/// for `Markup` and `build_report_display_index` for `Text`. For a caller that serves both.
pub(crate) fn display_index(source: &str, html: HtmlRendering) -> DisplayIndex {
    match cutter::cut(source, html) {
        Ok(layout) => DisplayIndex::Indexed(layout),
        Err(reason) => DisplayIndex::Fallback(reason),
    }
}
