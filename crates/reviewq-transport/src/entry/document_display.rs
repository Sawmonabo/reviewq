use reviewq_app::access::AppError;
use reviewq_app::reports::{DisplayBlockKind, DisplayFallback, DisplayLayout, DisplayWindowBlock};
use reviewq_protocol::reports as dto;

use super::pages::count;

pub(super) const MAX_WINDOW_JSON_BYTES: usize = 640 * 1024;

pub(super) fn remove_tables(blocks: &mut [dto::DisplayBlock]) {
    for block in blocks {
        if let Some(display) = &mut block.display {
            display.table = None;
            if display.drawings.is_empty() {
                block.display = None;
            }
        }
    }
}

pub(super) fn project_table(
    page: reviewq_app::reading::table::paged::TablePage,
) -> Result<dto::TablePage, AppError> {
    Ok(dto::TablePage {
        header: page.header,
        header_in_source: page.header_in_source,
        widths: page.widths,
        first_row: count(page.first_row)?,
        row_count: count(page.row_count)?,
        total_rows: count(page.total_rows)?,
        markdown: page.markdown,
    })
}

pub(super) fn project_block(
    block: DisplayWindowBlock,
    display: Option<dto::DocumentDisplay>,
) -> Result<dto::DisplayBlock, AppError> {
    let (kind, heading_level) = project_kind(block.span.kind)?;
    Ok(dto::DisplayBlock {
        byte_start: count(u64::from(block.span.byte_start))?,
        byte_end: count(u64::from(block.span.byte_end))?,
        scalar_start: count(u64::from(block.span.scalar_start))?,
        utf16_start: count(u64::from(block.span.utf16_start))?,
        kind,
        heading_level,
        markdown: block.markdown,
        display,
    })
}

pub(super) fn project_layout(layout: DisplayLayout) -> Result<dto::DisplayLayout, AppError> {
    Ok(dto::DisplayLayout {
        byte_length: count(u64::from(layout.byte_length))?,
        scalar_length: count(u64::from(layout.scalar_length))?,
        utf16_length: count(u64::from(layout.utf16_length))?,
        blocks: layout
            .blocks
            .into_iter()
            .map(|block| {
                let (kind, heading_level) = project_kind(block.kind)?;
                Ok(dto::DisplayLayoutBlock {
                    byte_start: count(u64::from(block.byte_start))?,
                    byte_end: count(u64::from(block.byte_end))?,
                    scalar_start: count(u64::from(block.scalar_start))?,
                    utf16_start: count(u64::from(block.utf16_start))?,
                    kind,
                    heading_level,
                    heading_markdown: block.heading_markdown,
                })
            })
            .collect::<Result<_, AppError>>()?,
    })
}

fn project_kind(
    kind: DisplayBlockKind,
) -> Result<
    (
        dto::DisplayBlockKind,
        Option<reviewq_protocol::SafeCountDto>,
    ),
    AppError,
> {
    use dto::DisplayBlockKind as Kind;
    let (kind, heading_level) = match kind {
        DisplayBlockKind::Paragraph => (Kind::Paragraph, None),
        DisplayBlockKind::Heading(level) => (Kind::Heading, Some(level)),
        DisplayBlockKind::BlockQuote => (Kind::BlockQuote, None),
        DisplayBlockKind::Code => (Kind::Code, None),
        DisplayBlockKind::List => (Kind::List, None),
        DisplayBlockKind::Rule => (Kind::Rule, None),
        DisplayBlockKind::Table => (Kind::Table, None),
    };
    Ok((
        kind,
        heading_level
            .map(|level| count(u64::from(level)))
            .transpose()?,
    ))
}

pub(super) fn project_fallback(reason: DisplayFallback) -> dto::DisplayFallback {
    use dto::DisplayFallback as Reason;
    match reason {
        DisplayFallback::DocumentBudget => Reason::DocumentBudget,
        DisplayFallback::ParserBudget => Reason::ParserBudget,
        DisplayFallback::DocumentContext => Reason::DocumentContext,
        DisplayFallback::LayoutBudget => Reason::LayoutBudget,
        DisplayFallback::UnsupportedSource => Reason::UnsupportedSource,
        DisplayFallback::InvalidBoundary => Reason::InvalidBoundary,
        DisplayFallback::ParserFailed => Reason::ParserFailed,
        DisplayFallback::IncompleteSource => Reason::IncompleteSource,
        DisplayFallback::CacheUnavailable => Reason::CacheUnavailable,
    }
}
