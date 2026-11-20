//! Bounded, whole-line windows and two-block settling adapted from AI Sidekicks.
//! Upstream: crates/sidekicks-session/src/transcript/block_cut.rs at
//! https://github.com/Sawmonabo/ai-sidekicks/commit/85975b3d8475b9d4c3a31d4bc4de4c5baaf3b42d
//!
//! Copyright 2026 AI Sidekicks contributors
//! SPDX-License-Identifier: Apache-2.0
//! The source stays verbatim. ReviewQ uses the pinned parser's native offset events,
//! records block kinds and scalar coordinates, and leaves unsafe cuts to the full reader.

use satteri_pulldown_cmark::{Event, Options, Parser, Tag};

use crate::reports::MAX_MARKDOWN_LINE_BYTES;

const CUT_WINDOW_BYTES: usize = 64 * 1024;
const SETTLE_LAG_BLOCKS: usize = 2;
const MAX_EVENTS: usize = 60_000;
const MAX_DEPTH: usize = 128;

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

/// Half-open original UTF-8 byte range and the matching original Unicode scalar start.
#[derive(Debug, Eq, PartialEq)]
pub struct DisplayBlock {
    pub byte_start: u32,
    pub byte_end: u32,
    pub scalar_start: u32,
    pub kind: DisplayBlockKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayFallback {
    DocumentBudget,
    ParserBudget,
    DocumentContext,
    UnsupportedSource,
    InvalidBoundary,
    ParserFailed,
}

struct BlockStart {
    byte: usize,
    kind: DisplayBlockKind,
    events: usize,
}

pub(super) fn cut(source: &str) -> Result<Vec<DisplayBlock>, DisplayFallback> {
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
    if source.split('\n').any(|line| {
        line.len() > MAX_MARKDOWN_LINE_BYTES
            && line.encode_utf16().count() > MAX_MARKDOWN_LINE_BYTES
    }) {
        return Err(DisplayFallback::DocumentBudget);
    }
    // The caller has applied the report byte, line and line-width envelope.
    // Nonfinal windows defer the last two children while more source is available.
    let mut blocks = Vec::new();
    let mut from = 0;
    let mut scalar_at_from = 0;
    let mut total_events = 0;
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
            let mut scalar = scalar_at_from;
            for (index, start) in candidates[..keep].iter().enumerate() {
                let end = candidates
                    .get(index + 1)
                    .map_or(window.len(), |next| next.byte);
                if start.byte < previous_end || start.byte >= end {
                    return Err(DisplayFallback::InvalidBoundary);
                }
                scalar += window[previous_end..start.byte].chars().count() as u32;
                blocks.push(DisplayBlock {
                    byte_start: (from + start.byte) as u32,
                    byte_end: (from + end) as u32,
                    scalar_start: scalar,
                    kind: start.kind,
                });
                scalar += window[start.byte..end].chars().count() as u32;
                previous_end = end;
                total_events += start.events;
                if total_events > MAX_EVENTS {
                    return Err(DisplayFallback::ParserBudget);
                }
            }
            from += previous_end;
            scalar_at_from = scalar;
            break;
        }
    }
    Ok(blocks)
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
    let parser = Parser::new_ext(
        window,
        Options::ENABLE_GFM
            | Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES,
    );
    if parser.reference_definitions().iter().next().is_some() {
        return Err(DisplayFallback::DocumentContext);
    }
    let mut blocks = Vec::new();
    let mut depth = 0_usize;
    for (event_count, (event, range)) in parser.into_offset_iter().enumerate() {
        if event_count == MAX_EVENTS {
            return Err(DisplayFallback::ParserBudget);
        }
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
            });
        }
        if let Some(block) = blocks.last_mut() {
            block.events += 1;
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
