use regex::bytes::Regex;
use reviewq_app::changes::PatchSide;

use super::super::wire::hunk_ranges;

const WINDOW_BYTES: usize = 8 * 1024;
const CONTEXT_BYTES: usize = 96;
const HUNK_HEADER_BYTES: usize = 96;

pub(in crate::gitlab::changes) struct Occurrence {
    pub side: PatchSide,
    pub line: u32,
    pub hunk: usize,
    pub start: usize,
    pub end: usize,
    pub snippet: String,
    pub highlight_start: usize,
    pub highlight_end: usize,
}

pub(in crate::gitlab::changes) struct Matches {
    pub hits: Vec<Occurrence>,
    pub next_offset: Option<usize>,
}

pub(in crate::gitlab::changes) struct Rows<'a> {
    query: &'a Regex,
    after: usize,
    limit: usize,
    match_bytes: usize,
    matches: Matches,
    offset: usize,
    hunk: usize,
    old: SideRows,
    new: SideRows,
    line: Option<Line>,
    // Reused across code rows. Retain only a block plus query overlap and snippet context.
    window: Vec<u8>,
}

#[derive(Default)]
struct SideRows {
    next: u32,
    remaining: usize,
}

enum Line {
    Header {
        offset: usize,
    },
    Code {
        side: PatchSide,
        line: u32,
        start: usize,
        checked: usize,
    },
    Other,
}

impl<'a> Rows<'a> {
    pub(super) fn new(query: &'a Regex, match_bytes: usize, after: usize, limit: usize) -> Self {
        Self {
            query,
            after,
            limit,
            match_bytes,
            matches: Matches {
                hits: Vec::new(),
                next_offset: None,
            },
            offset: 0,
            hunk: 0,
            old: SideRows::default(),
            new: SideRows::default(),
            line: None,
            window: Vec::new(),
        }
    }

    pub(in crate::gitlab::changes) fn part(&mut self, chunk: &[u8]) {
        if self.matches.next_offset.is_some() {
            return;
        }
        for part in chunk.split_inclusive(|byte| *byte == b'\n') {
            let content = part.strip_suffix(b"\n").unwrap_or(part);
            let content = if self.line.is_none()
                && let Some((&marker, content)) = content.split_first()
            {
                self.line = Some(self.begin(marker));
                if matches!(self.line, Some(Line::Header { .. })) {
                    self.window.push(marker);
                }
                content
            } else {
                content
            };
            self.push(content);
            if part.ends_with(b"\n") {
                self.end(true);
            }
            self.offset += part.len();
            if self.matches.next_offset.is_some() {
                break;
            }
        }
    }

    pub(in crate::gitlab::changes) fn finish(mut self) -> Matches {
        self.end(false);
        self.matches
    }

    fn begin(&mut self, marker: u8) -> Line {
        let start = self.offset + 1;
        let code = |side, line| Line::Code {
            side,
            line,
            start,
            checked: start,
        };
        match marker {
            b'@' => {
                self.old = SideRows::default();
                self.new = SideRows::default();
                Line::Header {
                    offset: self.offset,
                }
            }
            b'-' if self.old.remaining > 0 => code(PatchSide::Old, self.old.take()),
            b'+' if self.new.remaining > 0 => code(PatchSide::New, self.new.take()),
            b' ' if self.old.remaining > 0 && self.new.remaining > 0 => {
                self.old.take();
                code(PatchSide::New, self.new.take())
            }
            b'\\' => Line::Other,
            _ => {
                self.old = SideRows::default();
                self.new = SideRows::default();
                Line::Other
            }
        }
    }

    fn push(&mut self, mut content: &[u8]) {
        match self.line {
            Some(Line::Header { .. }) => {
                self.window.extend_from_slice(
                    &content[..content.len().min(HUNK_HEADER_BYTES - self.window.len())],
                );
            }
            Some(Line::Code { .. }) => {
                let right = self.match_bytes + CONTEXT_BYTES;
                let max = WINDOW_BYTES + right;
                if self.window.capacity() < max {
                    self.window.reserve_exact(max - self.window.len());
                }
                while !content.is_empty() && self.matches.next_offset.is_none() {
                    let take = (max - self.window.len()).min(content.len());
                    self.window.extend_from_slice(&content[..take]);
                    content = &content[take..];
                    if self.window.len() == max {
                        self.search_window(WINDOW_BYTES);
                        let drain = WINDOW_BYTES - CONTEXT_BYTES;
                        self.window.drain(..drain);
                        if let Some(Line::Code { start, .. }) = &mut self.line {
                            *start += drain;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn search_window(&mut self, up_to: usize) {
        let Some(Line::Code {
            side,
            line,
            start,
            checked,
        }) = &mut self.line
        else {
            return;
        };
        let begin = (*checked).max(self.after).saturating_sub(*start);
        if begin >= up_to || self.matches.next_offset.is_some() {
            return;
        }
        let mut seek = begin;
        while let Some(hit) = self.query.find_at(&self.window, seek) {
            let first = hit.start();
            let last = hit.end();
            // A match may overlap the previous one. The Unicode matcher skips partial scalars.
            seek = first + 1;
            if first >= up_to {
                break;
            }
            if self.matches.hits.len() == self.limit {
                self.matches.next_offset = Some(*start + first);
                break;
            }
            let mut lower = first.saturating_sub(CONTEXT_BYTES);
            let mut upper = (last + CONTEXT_BYTES).min(self.window.len());
            while is_continuation(self.window[lower]) {
                lower += 1;
            }
            while upper < self.window.len() && is_continuation(self.window[upper]) {
                upper -= 1;
            }
            // Struson has already validated the UTF-8 in the decoded JSON string.
            let text = std::str::from_utf8(&self.window[lower..upper])
                .expect("the snippet has UTF-8 boundaries");
            let highlight_start = text[..first - lower].encode_utf16().count();
            let highlight_end =
                highlight_start + text[first - lower..last - lower].encode_utf16().count();
            self.matches.hits.push(Occurrence {
                side: *side,
                line: *line,
                hunk: self.hunk,
                start: *start + first,
                end: *start + last,
                snippet: text.to_owned(),
                highlight_start,
                highlight_end,
            });
        }
        *checked = *start + up_to;
    }

    fn end(&mut self, lf: bool) {
        match self.line {
            Some(Line::Header { offset }) => {
                if let Some((old, new)) = hunk_ranges(&self.window) {
                    self.hunk = offset;
                    self.old = SideRows::from(old);
                    self.new = SideRows::from(new);
                }
            }
            Some(Line::Code { .. }) => {
                if lf && self.window.last() == Some(&b'\r') {
                    self.window.pop();
                }
                self.search_window(self.window.len());
            }
            _ => {}
        }
        self.line = None;
        self.window.clear();
    }
}

impl SideRows {
    fn from((next, remaining): (u32, usize)) -> Self {
        Self { next, remaining }
    }

    fn take(&mut self) -> u32 {
        self.remaining -= 1;
        let current = self.next;
        self.next += 1;
        current
    }
}

fn is_continuation(byte: u8) -> bool {
    byte & 0xc0 == 0x80
}

#[cfg(test)]
mod tests {
    use super::{PatchSide, Rows, WINDOW_BYTES};
    use regex::bytes::RegexBuilder;

    #[test]
    fn locates_hunk_bytes_and_unicode_across_chunk_and_result_boundaries() {
        let fragment = "🐍KeEdLe";
        let needle = "🐍keedle";
        let query = RegexBuilder::new(&regex::escape(needle))
            .case_insensitive(true)
            .build()
            .unwrap();
        let prior = "🦀".repeat((WINDOW_BYTES - 4) / 4);
        let patch = format!(
            "--- a/a\r\n+++ b/b\r\n@@ -4,2 +7,2 @@\r\n-🦀{fragment}\r\n+{prior}{fragment}\r\n context {fragment}"
        );
        let expected: Vec<_> = patch
            .match_indices(fragment)
            .map(|(start, _)| start)
            .collect();
        let read = |after, limit| {
            let mut rows = Rows::new(&query, 4 * needle.chars().count(), after, limit);
            for chunk in patch.as_bytes().chunks(9) {
                rows.part(chunk);
            }
            rows.finish()
        };
        let first = read(0, 1);
        assert_eq!(first.next_offset, Some(expected[1]));
        let removed = &first.hits[0];
        assert!(matches!(removed.side, PatchSide::Old));
        assert_eq!(
            (removed.line, removed.start, removed.end),
            (4, expected[0], expected[0] + fragment.len())
        );
        assert_eq!(removed.hunk, patch.find("@@").unwrap());
        assert_eq!((removed.highlight_start, removed.highlight_end), (2, 10));
        assert_eq!(removed.snippet, format!("🦀{fragment}"));
        let next = read(first.next_offset.unwrap(), 2);
        assert!(next.next_offset.is_none());
        for (index, found) in next.hits.iter().enumerate() {
            assert!(matches!(found.side, PatchSide::New));
            assert_eq!(found.line, 7 + index as u32);
            assert_eq!(found.start, expected[index + 1]);
            let highlighted: Vec<_> = found
                .snippet
                .encode_utf16()
                .skip(found.highlight_start)
                .take(found.highlight_end - found.highlight_start)
                .collect();
            assert_eq!(String::from_utf16(&highlighted).unwrap(), fragment);
        }
    }
}
