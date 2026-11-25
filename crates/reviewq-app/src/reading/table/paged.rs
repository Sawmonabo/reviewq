//! Recognize bounded row groups in an already over-budget, settled source.

use std::ops::Range;

use crate::reports::{DisplayBlockKind, DisplayIndex, HtmlRendering, display_index};

use super::{Inline, fragment};

const MAX_CONTEXT_BYTES: usize = 64 * 1024;
const MAX_HEADER_BYTES: usize = 4 * 1024;
const MAX_ROW_BYTES: usize = 16 * 1024;
const MAX_GROUP_BYTES: usize = 32 * 1024;
const MAX_GROUP_ROWS: usize = 48;
const MAX_GROUPS: usize = 1024;
pub const MAX_INDEXED_BYTES: u64 = 32 * 1024 * 1024;

/// Finds the row groups of a source's one long table as its text is pushed in order. The
/// context around the table is proved with the display index for the source's reader, so
/// report text, whose reader shows HTML as text, keeps its row groups around raw HTML.
pub struct Builder {
    position: u64,
    partial: String,
    state: State,
    html: HtmlRendering,
}

#[derive(Default)]
enum State {
    #[default]
    Start,
    Before(String),
    Rows(Rows),
    After(Index, String),
    Declined,
}

struct Rows {
    prefix: String,
    index: Index,
    markdown: String,
    start: u64,
    rows: usize,
}

pub struct Index {
    pub start: u64,
    pub end: u64,
    pub header: String,
    pub widths: Vec<u8>,
    pub rows: u64,
    introduction: bool,
    conclusion: bool,
    min_nodes: u64,
    groups: Vec<Group>,
}

struct Group {
    bytes: Range<u64>,
    row: u64,
}

pub struct TablePage {
    pub header: String,
    pub header_in_source: bool,
    pub widths: Vec<u8>,
    pub first_row: u64,
    pub row_count: u64,
    pub total_rows: u64,
    pub markdown: String,
}

pub struct RangeInfo {
    pub bytes: Range<u64>,
    pub previous: Option<u64>,
    pub next: Option<u64>,
    pub first_row: u64,
    pub rows: u64,
}

pub struct ContextRange {
    pub bytes: Range<u64>,
    pub previous: Option<u64>,
    pub next: Option<u64>,
}

impl Index {
    pub fn report_context(&self, offset: u64, length: u64) -> Option<ContextRange> {
        if self.introduction && offset < self.start {
            return Some(ContextRange {
                bytes: 0..self.start,
                previous: None,
                next: Some(self.start),
            });
        }
        if self.conclusion && offset >= self.end {
            return Some(ContextRange {
                bytes: self.end..length,
                previous: Some(self.groups.last()?.bytes.start),
                next: None,
            });
        }
        None
    }

    pub fn report_group(&self, offset: u64) -> RangeInfo {
        let mut group = self.group(offset);
        if self.introduction && group.previous.is_none() {
            group.previous = Some(0);
        }
        if self.conclusion && group.next.is_none() {
            group.next = Some(self.end);
        }
        group
    }

    pub fn exceeds_node_budget(&self) -> bool {
        self.min_nodes > 60_000
    }

    pub fn page(&self, group: &RangeInfo, markdown: String) -> TablePage {
        TablePage {
            header: self.header.clone(),
            header_in_source: group.bytes.start == self.start,
            widths: self.widths.clone(),
            first_row: group.first_row,
            row_count: group.rows,
            total_rows: self.rows,
            markdown,
        }
    }

    pub fn group(&self, offset: u64) -> RangeInfo {
        let index = self
            .groups
            .partition_point(|group| group.bytes.end <= offset)
            .min(self.groups.len() - 1);
        let group = &self.groups[index];
        let previous = index.checked_sub(1).map(|i| self.groups[i].bytes.start);
        let next = self.groups.get(index + 1);
        RangeInfo {
            bytes: group.bytes.clone(),
            previous,
            next: next.map(|group| group.bytes.start),
            first_row: group.row,
            rows: next.map_or(self.rows, |group| group.row) - group.row,
        }
    }
}

impl Builder {
    /// A builder for a source whose reader renders raw HTML as `html` says.
    pub fn new(html: HtmlRendering) -> Self {
        Self {
            position: 0,
            partial: String::new(),
            state: State::default(),
            html,
        }
    }

    pub fn push(&mut self, source: &str) {
        if matches!(self.state, State::Declined) {
            return;
        }
        for piece in source.split_inclusive('\n') {
            if self.partial.len() + piece.len() > MAX_ROW_BYTES {
                self.state = State::Declined;
                break;
            }
            if !self.partial.is_empty() || !piece.ends_with('\n') {
                self.partial.push_str(piece);
                if !piece.ends_with('\n') {
                    break;
                }
                let line = std::mem::take(&mut self.partial);
                self.line(&line);
            } else {
                self.line(piece);
            }
            if matches!(self.state, State::Declined) {
                break;
            }
        }
    }

    pub fn declined(&self) -> bool {
        matches!(self.state, State::Declined)
    }

    pub fn finish(mut self) -> Option<Index> {
        if !self.partial.is_empty() {
            let line = std::mem::take(&mut self.partial);
            self.line(&line);
        }
        match self.state {
            State::Rows(mut table) => {
                table.flush(self.position, self.html)?;
                Some(table.index)
            }
            State::After(mut index, suffix)
                if suffix.trim().is_empty() || context(&suffix, self.html) =>
            {
                index.conclusion = !suffix.trim().is_empty();
                Some(index)
            }
            _ => None,
        }
        .filter(|table| !table.groups.is_empty())
    }

    fn line(&mut self, line: &str) {
        let at = self.position;
        self.position += line.len() as u64;
        self.state = match std::mem::take(&mut self.state) {
            State::Start => State::Before(line.to_owned()),
            State::Before(mut before) => {
                if let Some(header) = header_start(&before, line) {
                    let byte_start = at - (before.len() - header) as u64;
                    let mut head = before.split_off(header);
                    head.push_str(line);
                    State::Rows(Rows {
                        prefix: before,
                        index: Index {
                            start: byte_start,
                            end: self.position,
                            header: head,
                            widths: Vec::new(),
                            rows: 0,
                            introduction: false,
                            conclusion: false,
                            min_nodes: 0,
                            groups: Vec::new(),
                        },
                        markdown: String::new(),
                        start: self.position,
                        rows: 0,
                    })
                } else if before.len() + line.len() <= MAX_CONTEXT_BYTES {
                    before.push_str(line);
                    State::Before(before)
                } else {
                    State::Declined
                }
            }
            State::Rows(mut table) => {
                if line.starts_with('|') {
                    if table.rows >= MAX_GROUP_ROWS
                        || table.markdown.len() + line.len() > MAX_GROUP_BYTES
                    {
                        if table.flush(at, self.html).is_none() {
                            self.state = State::Declined;
                            return;
                        }
                        table.start = at;
                    }
                    table.markdown.push_str(line);
                    table.rows += 1;
                    State::Rows(table)
                } else if line.trim().is_empty() && table.flush(at, self.html).is_some() {
                    State::After(table.index, line.to_owned())
                } else {
                    State::Declined
                }
            }
            State::After(index, mut suffix) if suffix.len() + line.len() <= MAX_CONTEXT_BYTES => {
                suffix.push_str(line);
                State::After(index, suffix)
            }
            _ => State::Declined,
        };
    }
}

// The parser proves top-level context. These line checks only bound a candidate's
// physical extent before asking for that proof.
fn header_start(before: &str, line: &str) -> Option<usize> {
    if !line.starts_with('|')
        || !line.contains('-')
        || !line
            .bytes()
            .all(|byte| matches!(byte, b'|' | b':' | b'-' | b' ' | b'\t' | b'\r' | b'\n'))
    {
        return None;
    }
    let prior = before.strip_suffix('\n')?;
    let start = prior.rfind('\n').map_or(0, |position| position + 1);
    let header = &before[start..];
    if !header.starts_with('|') || header.len() + line.len() > MAX_HEADER_BYTES {
        return None;
    }
    if start != 0
        && !prior[..start]
            .strip_suffix('\n')?
            .rsplit('\n')
            .next()?
            .trim()
            .is_empty()
    {
        return None;
    }
    Some(start)
}

impl Rows {
    fn flush(&mut self, end: u64, html: HtmlRendering) -> Option<()> {
        if self.rows == 0 {
            return (!self.index.groups.is_empty()).then_some(());
        }
        if self.index.groups.len() == MAX_GROUPS {
            return None;
        }
        let mut joined = String::with_capacity(self.index.header.len() + self.markdown.len());
        joined.push_str(&self.index.header);
        joined.push_str(&self.markdown);
        let packet = fragment(&joined)?;
        if packet.rows.len() != self.rows + 1 {
            return None;
        }
        if self.index.groups.is_empty() {
            let before = self.prefix.len();
            self.index.introduction = !self.prefix.trim().is_empty();
            self.prefix.push_str(&joined);
            let DisplayIndex::Indexed(layout) = display_index(&self.prefix, html) else {
                return None;
            };
            if !layout.blocks.last().is_some_and(|block| {
                block.kind == DisplayBlockKind::Table && block.byte_start as usize == before
            }) {
                return None;
            }
            self.prefix = String::new();
            self.index.widths.resize(packet.columns.len(), 8);
        }
        for (row, cells) in packet.rows.into_iter().enumerate() {
            let count_row = row != 0 || self.index.groups.is_empty();
            if count_row {
                // The row, its cells and inter-cell nodes necessarily exist in
                // the canonical table in addition to supported inline content.
                self.index.min_nodes += 1 + 2 * cells.len() as u64;
            }
            for (cell, width) in cells.into_iter().zip(&mut self.index.widths) {
                if count_row {
                    self.index.min_nodes += cell
                        .iter()
                        .map(|token| match token {
                            Inline::Close => 0,
                            Inline::Code { .. } => 2,
                            _ => 1,
                        })
                        .sum::<u64>();
                }
                let count: u32 = cell
                    .iter()
                    .map(|token| match token {
                        Inline::Text { start, end } => end - start,
                        Inline::Code { text, .. } => text.encode_utf16().count() as u32,
                        _ => 0,
                    })
                    .sum();
                *width = (*width).max((count + 2).clamp(8, 56) as u8);
            }
        }
        let start = if self.index.groups.is_empty() {
            self.index.start
        } else {
            self.start
        };
        self.index.groups.push(Group {
            bytes: start..end,
            row: self.index.rows,
        });
        self.index.rows += self.rows as u64;
        self.index.end = end;
        self.markdown.clear();
        self.rows = 0;
        Some(())
    }
}

fn context(markdown: &str, html: HtmlRendering) -> bool {
    matches!(display_index(markdown, html), DisplayIndex::Indexed(_))
}
