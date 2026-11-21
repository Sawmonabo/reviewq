//! Display spans for one complete native Markdown body.

use std::ops::Range;

use reviewq_domain::identity::ReportHash;

use crate::access::AppError;
use crate::reports::{
    ArtifactStats, DisplayFallback, DisplayIndex, DisplayLayout, DisplayWindowBlock,
    build_display_index,
};

pub const MAX_INDEX_BYTES: usize = 1024 * 1024;
pub const MAX_WINDOW_BYTES: u64 = 64 * 1024;
const MAX_WINDOW_BLOCKS: usize = 32;

pub struct Query {
    pub offset: u64,
    pub expected: Option<ReportHash>,
}

pub struct Window {
    pub hash: ReportHash,
    pub byte_length: u64,
    pub bytes: Range<u64>,
    pub previous: Option<u64>,
    pub next: Option<u64>,
    pub result: WindowResult,
}

pub enum WindowResult {
    Ready(Vec<DisplayWindowBlock>),
    Source {
        reason: DisplayFallback,
        markdown: String,
    },
}

pub struct Index {
    hash: ReportHash,
    bytes: u64,
    layout: DisplayIndex,
    pages: Vec<Range<usize>>,
    source: SourcePages,
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
}

impl SourcePages {
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
    pub fn new(hash: ReportHash, source: SourcePages, text: Option<&str>) -> Self {
        let bytes = source.bytes;
        let mut layout = text
            .filter(|text| !text.is_empty() && admitted(text))
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
        Self {
            hash,
            bytes,
            layout,
            pages,
            source,
        }
    }

    pub fn read(
        &self,
        query: Query,
        mut read: impl FnMut(Range<u64>) -> Result<Vec<u8>, AppError>,
    ) -> Result<Window, AppError> {
        let Query { offset, expected } = query;
        if expected
            .as_ref()
            .is_some_and(|expected| *expected != self.hash)
        {
            return Err(AppError::CursorExpired);
        }
        if offset > self.bytes {
            return Err(AppError::InvalidRequest);
        }
        let (bounds, result) = match &self.layout {
            DisplayIndex::Indexed(layout) if !self.pages.is_empty() => {
                self.read_page(layout, offset, &mut read)?
            }
            DisplayIndex::Fallback(reason) => {
                let bounds = self.source.page(offset);
                let markdown = String::from_utf8(read(bounds.bytes.clone())?)
                    .map_err(|_| AppError::CursorExpired)?;
                (
                    bounds,
                    WindowResult::Source {
                        reason: *reason,
                        markdown,
                    },
                )
            }
            DisplayIndex::Indexed(_) => return Err(AppError::Unavailable),
        };
        Ok(Window {
            hash: self.hash.clone(),
            byte_length: self.bytes,
            bytes: bounds.bytes,
            previous: bounds.previous,
            next: bounds.next,
            result,
        })
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
        let piece = String::from_utf8(read(bytes.clone())?).map_err(|_| AppError::CursorExpired)?;
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
    for line in text.split('\n') {
        lines += 1;
        maximum = maximum.max(line.len());
    }
    ArtifactStats {
        byte_length: text.len() as u64,
        total_code_points: 0,
        line_count: lines,
        max_line_bytes: maximum as u64,
        complete: true,
    }
    .require_markdown_budget()
    .is_ok()
}
