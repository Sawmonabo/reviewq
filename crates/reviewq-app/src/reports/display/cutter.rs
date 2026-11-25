//! Bounded, whole-line windows and two-block settling adapted from AI Sidekicks.
//! Upstream: crates/sidekicks-session/src/transcript/block_cut.rs at
//! https://github.com/Sawmonabo/ai-sidekicks/commit/85975b3d8475b9d4c3a31d4bc4de4c5baaf3b42d
//!
//! Copyright 2026 AI Sidekicks contributors
//! SPDX-License-Identifier: Apache-2.0
//! The source stays verbatim. ReviewQ uses the pinned parser's native offset events,
//! records block kinds and original source coordinates, and leaves unsafe cuts to the full reader.

use satteri_pulldown_cmark::{Event, Parser, Tag};

use super::{
    DisplayLayout, MAX_DISPLAY_BLOCKS, MAX_DISPLAY_HEADING_BYTES, MAX_DISPLAY_HEADINGS_BYTES,
};
use crate::reports::{MAX_MARKDOWN_LINE_BYTES, REPORT_MARKDOWN};

const CUT_WINDOW_BYTES: usize = 64 * 1024;
const SETTLE_LAG_BLOCKS: usize = 2;
const MAX_EVENTS: usize = 12_000;
const MAX_DEPTH: usize = 48;
const MAX_TABLE_ROWS: usize = 128;
const MAX_BROWSER_NODES: usize = 60_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayBlockKind {
    Paragraph,
    Heading(u8),
    BlockQuote,
    Code,
    List,
    Rule,
    Table,
}

/// Half-open original UTF-8 byte range and the matching scalar and UTF-16 starts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayBlock {
    pub byte_start: u32,
    pub byte_end: u32,
    pub scalar_start: u32,
    pub utf16_start: u32,
    pub kind: DisplayBlockKind,
    pub heading_markdown: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayFallback {
    DocumentBudget,
    ParserBudget,
    DocumentContext,
    LayoutBudget,
    UnsupportedSource,
    InvalidBoundary,
    ParserFailed,
    CacheUnavailable,
}

struct BlockStart {
    byte: usize,
    kind: DisplayBlockKind,
    events: usize,
    heading_end: Option<usize>,
    table_rows: usize,
    table_cells: usize,
    expected_cells: usize,
    literal_markers: usize,
}

pub(super) fn cut(source: &str) -> Result<DisplayLayout, DisplayFallback> {
    // The parser strips a leading BOM on each parse. Bare CR and NUL also need
    // special browser handling. Keep those inputs on the existing reader.
    if source.contains(['\u{feff}', '\0'])
        || source
            .as_bytes()
            .iter()
            .enumerate()
            .any(|(index, byte)| *byte == b'\r' && source.as_bytes().get(index + 1) != Some(&b'\n'))
    {
        return Err(DisplayFallback::UnsupportedSource);
    }
    for line in source.split('\n') {
        if line.len() > MAX_MARKDOWN_LINE_BYTES
            && line.encode_utf16().count() > MAX_MARKDOWN_LINE_BYTES
        {
            return Err(DisplayFallback::DocumentBudget);
        }
        // Empty markers can change the interpretation of their own container
        // or its neighbors, including when the block starts the document.
        if matches!(context_sensitive_marker(line), Some(Marker::Empty)) {
            return Err(DisplayFallback::DocumentContext);
        }
    }
    // The caller has applied the report byte, line and line-width envelope.
    // Nonfinal windows defer the last two children while more source is available.
    let mut blocks = Vec::new();
    let mut from = 0;
    let mut coordinate = Coordinate::default();
    let mut heading_bytes = 0;
    let mut total_events = 0;
    let mut browser_nodes = 1;
    while from < source.len() {
        let mut window_bytes = CUT_WINDOW_BYTES;
        loop {
            let to = window_end(source, from, window_bytes)?;
            let window = &source[from..to];
            let candidates = parse_window(window)?;
            let finished = to == source.len();
            let keep = if finished {
                candidates.len()
            } else {
                candidates.len().saturating_sub(SETTLE_LAG_BLOCKS)
            };
            if keep == 0 {
                if finished {
                    return Err(DisplayFallback::InvalidBoundary);
                }
                window_bytes *= 2;
                continue;
            }
            let mut previous_end = 0;
            for (index, start) in candidates[..keep].iter().enumerate() {
                let end = candidates
                    .get(index + 1)
                    .map_or(window.len(), |next| next.byte);
                if start.byte < previous_end || start.byte >= end {
                    return Err(DisplayFallback::InvalidBoundary);
                }
                let bytes = end - start.byte;
                if blocks.len() == MAX_DISPLAY_BLOCKS
                    || bytes > CUT_WINDOW_BYTES
                    || start.table_rows > MAX_TABLE_ROWS
                {
                    return Err(DisplayFallback::LayoutBudget);
                }
                // An ordered marker other than 1 can open a list in an
                // isolated slice even when it could not interrupt a paragraph.
                let first_line = window[start.byte..end].lines().next().unwrap_or_default();
                if !blocks.is_empty() && context_sensitive_marker(first_line).is_some() {
                    return Err(DisplayFallback::DocumentContext);
                }
                let heading_markdown = if let Some(heading_end) = start.heading_end {
                    let heading = window
                        .get(start.byte..heading_end)
                        .filter(|_| heading_end <= end)
                        .ok_or(DisplayFallback::InvalidBoundary)?;
                    heading_bytes += heading.len();
                    if heading.len() > MAX_DISPLAY_HEADING_BYTES
                        || heading_bytes > MAX_DISPLAY_HEADINGS_BYTES
                    {
                        return Err(DisplayFallback::LayoutBudget);
                    }
                    Some(heading.to_owned())
                } else {
                    None
                };
                coordinate.advance(&window[previous_end..start.byte]);
                blocks.push(DisplayBlock {
                    byte_start: (from + start.byte) as u32,
                    byte_end: (from + end) as u32,
                    scalar_start: coordinate.scalar,
                    utf16_start: coordinate.utf16,
                    kind: start.kind,
                    heading_markdown,
                });
                coordinate.advance(&window[start.byte..end]);
                previous_end = end;
                total_events += start.events;
                browser_nodes += start.browser_nodes();
                if total_events > MAX_EVENTS || browser_nodes > MAX_BROWSER_NODES {
                    return Err(DisplayFallback::ParserBudget);
                }
            }
            from += previous_end;
            break;
        }
    }
    if blocks.is_empty() {
        return Err(DisplayFallback::InvalidBoundary);
    }
    Ok(DisplayLayout {
        byte_length: source.len() as u32,
        scalar_length: coordinate.scalar,
        utf16_length: coordinate.utf16,
        blocks,
    })
}

impl BlockStart {
    fn browser_nodes(&self) -> usize {
        // Budget for elements and whitespace introduced when converting Markdown
        // to HTML, potential GFM literal links, and ragged table cell padding.
        let missing_cells = self.expected_cells.saturating_sub(self.table_cells);
        2 + 4 * (self.events + self.literal_markers) + 2 * missing_cells
    }
}

#[derive(Default)]
struct Coordinate {
    scalar: u32,
    utf16: u32,
}

impl Coordinate {
    fn advance(&mut self, source: &str) {
        for ch in source.chars() {
            self.scalar += 1;
            self.utf16 += ch.len_utf16() as u32;
        }
    }
}

enum Marker {
    Empty,
    NoninitialOrdered,
}

fn context_sensitive_marker(mut line: &str) -> Option<Marker> {
    line = line.trim_ascii();
    while let Some(quoted) = line.strip_prefix('>') {
        line = quoted.trim_ascii_start();
    }
    let marker_end = line.find([' ', '\t']).unwrap_or(line.len());
    let (marker, content) = line.split_at(marker_end);
    match marker.as_bytes() {
        [b'*' | b'+' | b'-'] if content.is_empty() => Some(Marker::Empty),
        [number @ .., b'.' | b')']
            if (1..=9).contains(&number.len()) && number.iter().all(u8::is_ascii_digit) =>
        {
            if content.is_empty() {
                Some(Marker::Empty)
            } else if number != b"1" {
                Some(Marker::NoninitialOrdered)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn window_end(source: &str, from: usize, bytes: usize) -> Result<usize, DisplayFallback> {
    let to = from + bytes.min(source.len() - from);
    if to == source.len() {
        return Ok(to);
    }
    source.as_bytes()[from..to]
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map(|index| from + index + 1)
        .ok_or(DisplayFallback::InvalidBoundary)
}

fn parse_window(window: &str) -> Result<Vec<BlockStart>, DisplayFallback> {
    let parser = Parser::new_ext(window, REPORT_MARKDOWN);
    if parser.reference_definitions().iter().next().is_some() {
        return Err(DisplayFallback::DocumentContext);
    }
    let mut blocks = Vec::new();
    let mut depth = 0_usize;
    let mut table_columns = 0;
    for (event_count, (event, range)) in parser.into_offset_iter().enumerate() {
        if event_count == MAX_EVENTS {
            return Err(DisplayFallback::ParserBudget);
        }
        let table_row = matches!(event, Event::Start(Tag::TableRow));
        if let Event::Start(Tag::Table(columns)) = &event {
            table_columns = columns.len();
        }
        let expected_cells = if table_row || matches!(event, Event::Start(Tag::TableHead)) {
            table_columns
        } else {
            0
        };
        let table_cell = matches!(event, Event::Start(Tag::TableCell));
        let literal_markers = if let Event::Text(value) = &event {
            value
                .bytes()
                .filter(|byte| matches!(byte, b'@' | b':' | b'.'))
                .count()
        } else {
            0
        };
        let kind = match event {
            Event::Start(Tag::FootnoteDefinition(_) | Tag::HtmlBlock)
            | Event::FootnoteReference(_)
            | Event::Html(_)
            | Event::InlineHtml(_) => return Err(DisplayFallback::DocumentContext),
            Event::Start(ref tag) => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(DisplayFallback::ParserBudget);
                }
                if depth == 1 {
                    Some(block_kind(tag)?)
                } else {
                    None
                }
            }
            Event::End(_) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or(DisplayFallback::InvalidBoundary)?;
                None
            }
            Event::Rule if depth == 0 => Some(DisplayBlockKind::Rule),
            _ if depth == 0 => return Err(DisplayFallback::InvalidBoundary),
            _ => None,
        };
        if let Some(kind) = kind {
            let prefix = window
                .get(..range.start)
                .ok_or(DisplayFallback::InvalidBoundary)?;
            let byte = prefix.rfind('\n').map_or(0, |index| index + 1);
            if blocks
                .last()
                .is_some_and(|previous: &BlockStart| byte <= previous.byte)
            {
                return Err(DisplayFallback::InvalidBoundary);
            }
            blocks.push(BlockStart {
                byte,
                kind,
                events: 0,
                heading_end: matches!(kind, DisplayBlockKind::Heading(1..=3)).then_some(range.end),
                table_rows: 0,
                table_cells: 0,
                expected_cells: 0,
                literal_markers: 0,
            });
        }
        if let Some(block) = blocks.last_mut() {
            block.events += 1;
            block.table_rows += usize::from(table_row);
            block.table_cells += usize::from(table_cell);
            block.expected_cells += expected_cells;
            block.literal_markers += literal_markers;
        }
    }
    if depth != 0 {
        return Err(DisplayFallback::InvalidBoundary);
    }
    Ok(blocks)
}

fn block_kind(tag: &Tag<'_>) -> Result<DisplayBlockKind, DisplayFallback> {
    Ok(match tag {
        Tag::Paragraph => DisplayBlockKind::Paragraph,
        Tag::Heading { level, .. } => DisplayBlockKind::Heading(*level as u8),
        Tag::BlockQuote(_) => DisplayBlockKind::BlockQuote,
        Tag::CodeBlock(_) => DisplayBlockKind::Code,
        Tag::List(..) => DisplayBlockKind::List,
        Tag::Table(_) => DisplayBlockKind::Table,
        _ => return Err(DisplayFallback::InvalidBoundary),
    })
}
