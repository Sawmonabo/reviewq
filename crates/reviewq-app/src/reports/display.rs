//! Native source spans for a captured report. Browser admission still uses its own validator.

mod cutter;

pub use cutter::{DisplayBlock, DisplayBlockKind, DisplayFallback};

pub const DISPLAY_INDEX_FORMAT: u32 = 1;

pub enum DisplayIndex {
    Indexed(Vec<DisplayBlock>),
    Fallback(DisplayFallback),
}

pub(super) async fn build(source: Option<String>) -> DisplayIndex {
    let Some(source) = source else {
        return DisplayIndex::Fallback(DisplayFallback::DocumentBudget);
    };
    match tokio::task::spawn_blocking(move || cutter::cut(&source)).await {
        Ok(Ok(blocks)) => DisplayIndex::Indexed(blocks),
        Ok(Err(reason)) => DisplayIndex::Fallback(reason),
        Err(_) => DisplayIndex::Fallback(DisplayFallback::ParserFailed),
    }
}
