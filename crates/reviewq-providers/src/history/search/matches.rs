//! The matches of one body being scanned: the window it searches and the hits it collects.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::os::unix::fs::FileExt;
use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;
use reviewq_app::history::FileContentRef;
use reviewq_app::history::search::{DocumentMatch, DocumentMatchMode, SearchHit, SearchPoint};
use reviewq_app::reading::{FoldedText, Literal, TextMatch, displayed_markdown};

use super::{BODY_STEP, Body, CONTENT_BYTES, DECODE_BLOCK, check_cancel, file, native_string};

pub(super) struct Matches<'a> {
    pub(super) query: &'a Literal,
    pub(super) reverse: bool,
    pub(super) reference: FileContentRef,
    pub(super) body: &'a Body,
    pub(super) document: Option<DocumentMatch>,
    pub(super) point: SearchPoint,
    pub(super) boundary: u64,
    pub(super) range: std::ops::Range<u64>,
    pub(super) available: usize,
    pub(super) hits: VecDeque<SearchHit>,
    pub(super) bytes: Vec<u8>,
    pub(super) decoded: u64,
    pub(super) start_utf16: u64,
    pub(super) next: Option<SearchPoint>,
    pub(super) partial: bool,
    pub(super) stopped: bool,
}

impl Matches<'_> {
    pub(super) fn complete_body(&mut self) -> Result<(), AppError> {
        if self.body.markdown {
            let text = file::readable_prefix(&self.bytes)?;
            if let Some(display) = displayed_markdown(text) {
                self.collect(&display, true);
            } else {
                self.partial = true;
            }
        } else {
            self.flush_matches(true)?;
        }
        Ok(())
    }

    fn select_window(&mut self) -> std::ops::Range<u64> {
        let boundary = self.boundary.min(self.body.bytes);
        self.range = if self.reverse {
            boundary.saturating_sub(BODY_STEP)..boundary
        } else {
            boundary..boundary.saturating_add(BODY_STEP).min(self.body.bytes)
        };
        self.range.start.saturating_sub(CONTENT_BYTES)
            ..self
                .range
                .end
                .saturating_add(CONTENT_BYTES)
                .min(self.body.bytes)
    }

    pub(super) fn scan_native(
        &mut self,
        file: &std::fs::File,
        native: &native_string::NativeString,
        cancel: &AtomicBool,
    ) -> Result<(), AppError> {
        let window = self.select_window();
        let mut first = true;
        native.read(
            file,
            window.start,
            window.end,
            cancel,
            &mut |offset, text| {
                if first {
                    self.decoded = offset;
                    first = false;
                }
                self.bytes.extend_from_slice(text.as_bytes());
                self.flush_matches(false)?;
                Ok(self.next.is_none())
            },
        )?;
        self.close_window()
    }

    pub(super) fn scan_body(
        &mut self,
        file: &std::fs::File,
        cancel: &AtomicBool,
    ) -> Result<(), AppError> {
        let window = self.select_window();
        let end = window.end;
        let mut offset = window.start;
        self.decoded = offset;
        let mut scratch = [0; DECODE_BLOCK];
        while offset < end && self.next.is_none() {
            check_cancel(cancel)?;
            let count = (end - offset).min(DECODE_BLOCK as u64) as usize;
            file.read_exact_at(&mut scratch[..count], offset)
                .map_err(|_| AppError::Unavailable)?;
            let skip = if offset == self.decoded && offset > 0 && self.bytes.is_empty() {
                scratch[..count]
                    .iter()
                    .take_while(|byte| **byte & 0xc0 == 0x80)
                    .count()
            } else {
                0
            };
            self.decoded += skip as u64;
            self.bytes.extend_from_slice(&scratch[skip..count]);
            self.flush_matches(false)?;
            offset += count as u64;
        }
        self.close_window()
    }

    fn close_window(&mut self) -> Result<(), AppError> {
        if self.next.is_none() {
            self.flush_matches(true)?;
        }
        if self.next.is_none() {
            let next = if self.reverse {
                self.range.start
            } else {
                self.range.end
            };
            if if self.reverse {
                next > 0
            } else {
                next < self.body.bytes
            } {
                self.next = Some(SearchPoint {
                    byte: next,
                    ..self.point
                });
            }
        }
        Ok(())
    }

    fn flush_matches(&mut self, final_block: bool) -> Result<(), AppError> {
        let bytes = std::mem::take(&mut self.bytes);
        let text = file::readable_prefix(&bytes)?;
        let cut = self.collect(text, final_block);
        self.decoded += cut.byte as u64;
        self.start_utf16 += cut.utf16 as u64;
        self.bytes.extend_from_slice(&bytes[cut.byte..]);
        if self.range.end == u64::MAX
            && !self.reverse
            && self.next.is_none()
            && self.decoded >= self.boundary.saturating_add(BODY_STEP)
        {
            self.next = Some(SearchPoint {
                byte: self.decoded,
                ..self.point
            });
        }
        // A pathological cluster is larger than a display window and has no bounded witness.
        if self.bytes.len() >= CONTENT_BYTES as usize {
            let end = text.len();
            self.partial = true;
            self.bytes.drain(..end);
            self.decoded += end as u64;
            self.start_utf16 += text.encode_utf16().count() as u64;
        }
        Ok(())
    }

    pub(super) fn collect(
        &mut self,
        text: &str,
        final_block: bool,
    ) -> reviewq_app::reading::TextPoint {
        use reviewq_app::reading::TextPoint;
        let folded = FoldedText::new(text);
        let cut = if final_block {
            TextPoint {
                byte: text.len(),
                utf16: text.encode_utf16().count(),
            }
        } else {
            folded.settled_prefix(self.query.folded().len())
        };
        for span in folded
            .occurrences(self.query)
            .take_while(|span| span.start.byte < cut.byte)
        {
            let byte = self.decoded + span.start.byte as u64;
            if !self.range.contains(&byte) {
                continue;
            }
            if if self.reverse {
                byte >= self.boundary
            } else {
                byte < self.boundary
            } {
                continue;
            }
            let point = SearchPoint { byte, ..self.point };
            let matched = &text[span.start.byte..span.end.byte];
            if matched.len() > 1024 {
                self.partial = true;
                continue;
            }
            if self.reverse && self.hits.len() == self.available {
                self.hits.pop_front();
            }
            self.hits.push_back(self.hit(text, span, point, matched));
            if !self.reverse && self.hits.len() == self.available {
                self.next = Some(SearchPoint {
                    byte: byte + 1,
                    ..self.point
                });
                break;
            }
        }
        cut
    }

    pub(super) fn hit(
        &self,
        text: &str,
        span: TextMatch,
        point: SearchPoint,
        matched: &str,
    ) -> SearchHit {
        let markdown = self.body.markdown
            || self
                .document
                .as_ref()
                .is_some_and(|proof| proof.mode != DocumentMatchMode::Source);
        let ranged = self.document.is_none() && !markdown && self.body.bytes > CONTENT_BYTES;
        let before = &text[..span.start.byte];
        let from = before.char_indices().rev().nth(31).map_or(0, |(at, _)| at);
        SearchHit {
            reference: self.reference.clone(),
            point,
            tool_call: self.body.tool_call.clone(),
            before: before[from..].to_owned(),
            matched: matched.to_owned(),
            after: text[span.end.byte..].chars().take(32).collect(),
            output_offset: if ranged { point.byte } else { 0 },
            output_bytes: self.body.bytes,
            start_utf16: if ranged {
                0
            } else {
                self.start_utf16 + span.start.utf16 as u64
            },
            end_utf16: if ranged {
                (span.end.utf16 - span.start.utf16) as u64
            } else {
                self.start_utf16 + span.end.utf16 as u64
            },
            markdown,
            document: self.document.clone(),
            ordinal: if markdown {
                before
                    .char_indices()
                    .filter(|(start, _)| text[*start..].starts_with(matched))
                    .count() as u64
            } else {
                0
            },
        }
    }
}

impl Write for Matches<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(bytes);
        if !self.body.markdown && self.bytes.len() >= DECODE_BLOCK {
            self.flush_matches(false).map_err(io::Error::other)?;
        }
        self.stopped = self.reverse && self.decoded >= self.boundary.saturating_add(CONTENT_BYTES);
        if self.next.is_some() || self.stopped {
            return Err(io::Error::other("search page ready"));
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
