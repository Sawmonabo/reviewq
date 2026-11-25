//! Display spans for one complete native Markdown body.

use std::ops::Range;

use reviewq_domain::identity::ReportHash;

use crate::access::AppError;
use crate::reading::table::paged::{self, TablePage};
use crate::reports::{
    ArtifactStats, DisplayFallback, DisplayIndex, DisplayLayout, DisplayWindowBlock, HtmlRendering,
    build_display_index,
};

pub const MAX_INDEX_BYTES: usize = 1024 * 1024;
pub const MAX_WINDOW_BYTES: u64 = 64 * 1024;
const MAX_WINDOW_BLOCKS: usize = 32;

pub struct Query {
    pub position: Position,
    pub expected: Option<ReportHash>,
}

pub enum Position {
    AtByte(u64),
    Canonical,
}

pub struct Window {
    pub hash: ReportHash,
    pub byte_length: u64,
    pub table_start: Option<u64>,
    pub bytes: Range<u64>,
    pub previous: Option<u64>,
    pub next: Option<u64>,
    pub result: WindowResult,
}

pub enum WindowResult {
    Ready(Vec<DisplayWindowBlock>),
    Table(TablePage),
    Canonical(String),
    Source {
        reason: DisplayFallback,
        canonical: bool,
        markdown: String,
    },
}

pub struct Index {
    hash: ReportHash,
    bytes: u64,
    layout: DisplayIndex,
    pages: Vec<Range<usize>>,
    source: SourcePages,
    table: Option<paged::Index>,
    canonical: bool,
}

struct Bounds {
    bytes: Range<u64>,
    previous: Option<u64>,
    next: Option<u64>,
}

#[derive(Default)]
pub struct SourcePages {
    bytes: u64,
    breaks: Vec<u64>,
    table: Option<paged::Builder>,
}

impl SourcePages {
    pub fn for_bytes(bytes: u64) -> Self {
        Self {
            table: (MAX_INDEX_BYTES as u64 + 1..=paged::MAX_INDEXED_BYTES)
                .contains(&bytes)
                .then(|| paged::Builder::new(HtmlRendering::Markup)),
            ..Self::default()
        }
    }

    pub fn push(&mut self, text: &str) {
        let end = self.bytes + text.len() as u64;
        // Start each page on a UTF-8 scalar and reserve the maximum retreat
        // from the nominal boundary so the original byte cap still holds.
        while (self.breaks.len() as u64 + 1) * (MAX_WINDOW_BYTES - 3) < end {
            let offset = (self.breaks.len() as u64 + 1) * (MAX_WINDOW_BYTES - 3);
            let offset = text.floor_char_boundary((offset - self.bytes) as usize);
            self.breaks.push(self.bytes + offset as u64);
        }
        self.bytes = end;
        if let Some(table) = &mut self.table {
            table.push(text);
        }
    }

    fn page(&self, offset: u64) -> Bounds {
        if self.bytes <= MAX_WINDOW_BYTES {
            return Bounds {
                bytes: 0..self.bytes,
                previous: None,
                next: None,
            };
        }
        let page = self.breaks.partition_point(|start| *start <= offset);
        let start = |index: usize| index.checked_sub(1).map_or(0, |i| self.breaks[i]);
        let previous = page.checked_sub(1).map(start);
        let next = self.breaks.get(page).copied();
        Bounds {
            bytes: start(page)..next.unwrap_or(self.bytes),
            previous,
            next,
        }
    }
}

impl Index {
    pub fn new(hash: ReportHash, mut source: SourcePages, text: Option<&str>) -> Self {
        let bytes = source.bytes;
        let over_envelope = text.is_some_and(|text| !admitted(text));
        let mut layout = text
            .filter(|text| !text.is_empty() && !over_envelope)
            .map_or(
                DisplayIndex::Fallback(DisplayFallback::DocumentBudget),
                build_display_index,
            );
        if matches!(&layout, DisplayIndex::Indexed(layout) if layout.blocks.first().is_some_and(|block| u64::from(block.byte_end) > MAX_WINDOW_BYTES))
        {
            layout = DisplayIndex::Fallback(DisplayFallback::LayoutBudget);
        }
        let mut pages = Vec::new();
        if let DisplayIndex::Indexed(layout) = &layout {
            let mut start = 0;
            while let Some(block) = layout.blocks.get(start) {
                let start_byte = if start == 0 { 0 } else { block.byte_start };
                let ceiling = u64::from(start_byte) + MAX_WINDOW_BYTES;
                let mut end = start + 1;
                while end < layout.blocks.len()
                    && end - start < MAX_WINDOW_BLOCKS
                    && u64::from(layout.blocks[end].byte_end) <= ceiling
                {
                    end += 1;
                }
                pages.push(start..end);
                start = end;
            }
        }
        let table = source
            .table
            .take()
            .and_then(paged::Builder::finish)
            .or_else(|| {
                let text = text?;
                if !over_envelope
                    && !matches!(
                        &layout,
                        DisplayIndex::Fallback(
                            DisplayFallback::LayoutBudget | DisplayFallback::ParserBudget
                        )
                    )
                {
                    return None;
                }
                let mut table = paged::Builder::new(HtmlRendering::Markup);
                table.push(text);
                table
                    .finish()
                    .filter(|index| over_envelope || index.exceeds_node_budget())
            });
        let canonical = text.is_some_and(|text| !text.is_empty() && !over_envelope)
            && table.is_none()
            && matches!(layout, DisplayIndex::Fallback(_));
        Self {
            hash,
            bytes,
            layout,
            pages,
            source,
            table,
            canonical,
        }
    }

    pub fn read(
        &self,
        query: Query,
        mut read: impl FnMut(Range<u64>) -> Result<Vec<u8>, AppError>,
    ) -> Result<Window, AppError> {
        let Query { position, expected } = query;
        if expected
            .as_ref()
            .is_some_and(|expected| *expected != self.hash)
        {
            return Err(AppError::CursorExpired);
        }
        let offset = match position {
            Position::AtByte(offset) => offset,
            Position::Canonical => {
                if !self.canonical {
                    return Err(AppError::InvalidRequest);
                }
                let bytes = 0..self.bytes;
                let markdown = decode(read(bytes.clone())?)?;
                return Ok(Window {
                    hash: self.hash.clone(),
                    byte_length: self.bytes,
                    table_start: None,
                    bytes,
                    previous: None,
                    next: None,
                    result: WindowResult::Canonical(markdown),
                });
            }
        };
        if offset > self.bytes {
            return Err(AppError::InvalidRequest);
        }
        let (bounds, result) = match &self.layout {
            DisplayIndex::Indexed(layout) if !self.pages.is_empty() => {
                self.read_page(layout, offset, &mut read)?
            }
            DisplayIndex::Fallback(reason) => self.read_source(offset, *reason, &mut read)?,
            DisplayIndex::Indexed(_) => return Err(AppError::Unavailable),
        };
        Ok(Window {
            hash: self.hash.clone(),
            byte_length: self.bytes,
            table_start: self.table.as_ref().map(|table| table.start),
            bytes: bounds.bytes,
            previous: bounds.previous,
            next: bounds.next,
            result,
        })
    }

    fn read_source(
        &self,
        offset: u64,
        reason: DisplayFallback,
        read: &mut impl FnMut(Range<u64>) -> Result<Vec<u8>, AppError>,
    ) -> Result<(Bounds, WindowResult), AppError> {
        let mut bounds = self.source.page(offset);
        if let Some(table) = &self.table {
            let in_table = table.start <= offset && offset < table.end
                || (offset == self.bytes && table.end == self.bytes);
            if in_table {
                let group = table.group(offset);
                let previous = group.previous.or_else(|| {
                    table
                        .start
                        .checked_sub(1)
                        .map(|before| self.source.page(before).bytes.start)
                });
                let next = group
                    .next
                    .or_else(|| (table.end < self.bytes).then_some(table.end));
                let markdown = decode(read(group.bytes.clone())?)?;
                let result = WindowResult::Table(table.page(&group, markdown));
                return Ok((
                    Bounds {
                        bytes: group.bytes,
                        previous,
                        next,
                    },
                    result,
                ));
            }
            if offset < table.start && bounds.bytes.end > table.start {
                bounds.bytes.end = table.start;
                bounds.next = Some(table.start);
            }
            if offset >= table.end {
                if bounds.bytes.start <= table.end {
                    bounds.bytes.start = table.end;
                    bounds.previous = Some(table.group(table.end).bytes.start);
                } else if bounds.previous.is_some_and(|previous| previous < table.end) {
                    bounds.previous = Some(table.end);
                }
            }
        }
        let markdown = decode(read(bounds.bytes.clone())?)?;
        Ok((
            bounds,
            WindowResult::Source {
                reason,
                markdown,
                canonical: self.canonical,
            },
        ))
    }

    fn read_page(
        &self,
        layout: &DisplayLayout,
        offset: u64,
        read: &mut impl FnMut(Range<u64>) -> Result<Vec<u8>, AppError>,
    ) -> Result<(Bounds, WindowResult), AppError> {
        let page = self
            .pages
            .partition_point(|range| u64::from(layout.blocks[range.end - 1].byte_end) <= offset)
            .min(self.pages.len() - 1);
        let indices = self.pages[page].clone();
        let start = |page: usize| {
            if page == 0 {
                0
            } else {
                u64::from(layout.blocks[self.pages[page].start].byte_start)
            }
        };
        let bytes = start(page)..u64::from(layout.blocks[indices.end - 1].byte_end);
        let piece = decode(read(bytes.clone())?)?;
        let mut blocks = Vec::with_capacity(indices.len());
        for span in &layout.blocks[indices] {
            let from = (u64::from(span.byte_start) - bytes.start) as usize;
            let to = (u64::from(span.byte_end) - bytes.start) as usize;
            let markdown = piece
                .get(from..to)
                .ok_or(AppError::CursorExpired)?
                .to_owned();
            blocks.push(DisplayWindowBlock {
                span: span.clone(),
                markdown,
            });
        }
        // The first block owns the leading blank source, including its coordinates.
        if page == 0
            && let Some(first) = blocks.first_mut()
            && first.span.byte_start != 0
        {
            first.markdown = piece[..first.span.byte_end as usize].to_owned();
            first.span.byte_start = 0;
            first.span.scalar_start = 0;
            first.span.utf16_start = 0;
        }
        let previous = page.checked_sub(1).map(start);
        let next = (page + 1 < self.pages.len()).then(|| start(page + 1));
        Ok((
            Bounds {
                bytes,
                previous,
                next,
            },
            WindowResult::Ready(blocks),
        ))
    }
}

fn admitted(text: &str) -> bool {
    let mut lines = 0;
    let mut maximum = 0;
    for line in text.split_terminator('\n') {
        lines += 1;
        maximum = maximum.max(line.strip_suffix('\r').unwrap_or(line).len());
    }
    ArtifactStats {
        byte_length: text.len() as u64,
        total_code_points: 0,
        line_count: lines,
        max_line_bytes: maximum as u64,
    }
    .require_markdown_budget()
    .is_ok()
}

fn decode(bytes: Vec<u8>) -> Result<String, AppError> {
    String::from_utf8(bytes).map_err(|_| AppError::CursorExpired)
}
