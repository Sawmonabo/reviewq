use reviewq_app::access::AppError;
use reviewq_app::changes::{
    PATCH_SOURCE_BYTES, PATCH_SOURCE_ROWS, PatchSourceCursor, PatchSourceRow, PatchSourceTarget,
    PatchSourceWindow,
};

use super::SourceRead;
use super::index::{Row, SourceIndex};

// Reserve wire space for the response identity, counters and row metadata.
// The text uses the remaining budget according to its JSON escaped size.
const ENVELOPE_BYTES: usize = 1024;
const ROW_BYTES: usize = 192;

impl SourceIndex {
    pub(in crate::gitlab::changes) async fn window(
        &self,
        body: &mut impl SourceRead,
        target: PatchSourceTarget,
    ) -> Result<PatchSourceWindow, AppError> {
        match target {
            PatchSourceTarget::Before(end) => self.before(body, end).await,
            PatchSourceTarget::End => {
                if self.total.byte == 0 {
                    return Ok(self.empty());
                }
                self.before(body, self.total.byte).await
            }
            PatchSourceTarget::Row { row, byte }
                if row == self.total.row && byte.is_none_or(|byte| byte == self.total.byte) =>
            {
                Ok(self.empty())
            }
            PatchSourceTarget::Byte(byte) if byte == self.total.byte => Ok(self.empty()),
            PatchSourceTarget::Byte(byte) => {
                let byte = self.floor(body, byte).await?;
                let row = self.locate(body, PatchSourceTarget::Byte(byte)).await?;
                self.forward(body, row, byte).await
            }
            _ => {
                let row = self.locate(body, target).await?;
                let start = match target {
                    PatchSourceTarget::Row {
                        byte: Some(byte), ..
                    } => {
                        if !(row.at.byte..row.end().byte).contains(&byte)
                            || self.floor(body, byte).await? != byte
                        {
                            return Err(AppError::RangeNotSatisfiable);
                        }
                        byte
                    }
                    _ => row.at.byte,
                };
                self.forward(body, row, start).await
            }
        }
    }

    async fn forward(
        &self,
        body: &mut impl SourceRead,
        row: Row,
        first: usize,
    ) -> Result<PatchSourceWindow, AppError> {
        let end = self.byte_at_or_after_row(row.at.row + PATCH_SOURCE_ROWS);
        let length = (end - first).min(PATCH_SOURCE_BYTES);
        let bytes = body.read(first, length).await?;
        let text = match std::str::from_utf8(&bytes) {
            Ok(text) => text,
            Err(error) if error.error_len().is_none() && first + length < self.total.byte => {
                exact_text(&bytes[..error.valid_up_to()])?
            }
            Err(_) => return Err(AppError::Unavailable),
        };
        self.rows(body, row, first, text).await
    }

    async fn before(
        &self,
        body: &mut impl SourceRead,
        end: usize,
    ) -> Result<PatchSourceWindow, AppError> {
        if end == 0 || end > self.total.byte {
            return Err(AppError::RangeNotSatisfiable);
        }
        let lower = self
            .floor(body, end.saturating_sub(PATCH_SOURCE_BYTES))
            .await?;
        // Include enough surrounding bytes to validate the exact exclusive end.
        let read_end = (end + 1).min(self.total.byte);
        let bytes = body.read(lower, read_end - lower).await?;
        let end = end - lower;
        if continuation(bytes.get(end)) || crlf(&bytes, end) {
            return Err(AppError::RangeNotSatisfiable);
        }
        let text = exact_text(&bytes[..end])?;
        let mut first = reverse_start(text);
        if crlf(&bytes, first) {
            first += 1;
        }
        let row = self
            .locate(body, PatchSourceTarget::Byte(lower + first))
            .await?;
        let page = self.rows(body, row, lower + first, &text[first..]).await?;
        if page.rows.last().map(|row| row.source_end) != Some(lower + end) {
            return Err(AppError::Internal);
        }
        Ok(page)
    }

    async fn rows(
        &self,
        body: &mut impl SourceRead,
        mut row: Row,
        mut start: usize,
        text: &str,
    ) -> Result<PatchSourceWindow, AppError> {
        let mut rows = Vec::new();
        let mut budget = PATCH_SOURCE_BYTES - ENVELOPE_BYTES;
        let mut next = row.at;
        for (index, fragment) in text
            .split_inclusive('\n')
            .take(PATCH_SOURCE_ROWS)
            .enumerate()
        {
            let (length, escaped) = forward_end(fragment, budget.saturating_sub(ROW_BYTES));
            if length == 0 {
                break;
            }
            if index > 0 {
                row = if fragment.ends_with('\n') || start + fragment.len() == self.total.byte {
                    row.next_row(fragment.as_bytes())
                } else {
                    self.locate(
                        body,
                        PatchSourceTarget::Row {
                            row: row.end().row,
                            byte: None,
                        },
                    )
                    .await?
                };
            }
            let end = start + length;
            rows.push(PatchSourceRow {
                row: row.at.row,
                hunk: row.hunk,
                old_line: row.old_line,
                new_line: row.new_line,
                row_start: row.at.byte,
                row_end: row.end().byte,
                source_start: start,
                source_end: end,
                text: fragment[..length].to_owned(),
            });
            next = PatchSourceCursor {
                row: row.at.row + usize::from(end == row.end().byte),
                byte: end,
            };
            if length != fragment.len() {
                break;
            }
            budget -= ROW_BYTES + escaped;
            start = end;
        }
        if rows.is_empty() {
            return Err(AppError::Internal);
        }
        Ok(PatchSourceWindow {
            total: self.total,
            rows,
            next: (next != self.total).then_some(next),
        })
    }

    async fn floor(&self, body: &mut impl SourceRead, byte: usize) -> Result<usize, AppError> {
        if byte > self.total.byte {
            return Err(AppError::RangeNotSatisfiable);
        }
        if byte == 0 || byte == self.total.byte {
            return Ok(byte);
        }
        let from = byte.saturating_sub(3);
        let bytes = body.read(from, byte + 1 - from).await?;
        let mut cut = byte - from;
        while cut > 0 && continuation(bytes.get(cut)) {
            cut -= 1;
        }
        if crlf(&bytes, cut) {
            cut -= 1;
        }
        Ok(from + cut)
    }

    fn empty(&self) -> PatchSourceWindow {
        PatchSourceWindow {
            total: self.total,
            rows: Vec::new(),
            next: None,
        }
    }
}

fn exact_text(bytes: &[u8]) -> Result<&str, AppError> {
    std::str::from_utf8(bytes).map_err(|_| AppError::Unavailable)
}

fn continuation(byte: Option<&u8>) -> bool {
    byte.is_some_and(|byte| byte & 0xc0 == 0x80)
}

fn crlf(bytes: &[u8], at: usize) -> bool {
    at > 0 && bytes[at - 1] == b'\r' && bytes.get(at) == Some(&b'\n')
}

fn wire_bytes(character: char) -> usize {
    match character {
        '\t' | '\n' | '\r' | '\u{8}' | '\u{c}' | '"' | '\\' => 2,
        '\0'..='\u{1f}' => 6,
        _ => character.len_utf8(),
    }
}

fn forward_end(fragment: &str, limit: usize) -> (usize, usize) {
    let mut charged = 0;
    for (at, value) in fragment.char_indices() {
        let cost = wire_bytes(value);
        if charged + cost > limit {
            if crlf(fragment.as_bytes(), at) {
                return (at - 1, charged - 2);
            }
            return (at, charged);
        }
        charged += cost;
    }
    (fragment.len(), charged)
}

fn reverse_start(text: &str) -> usize {
    let mut charged = ENVELOPE_BYTES + ROW_BYTES;
    let mut rows = 1;
    let mut start = text.len();
    for (at, value) in text.char_indices().rev() {
        if value == '\n' && at + 1 != text.len() {
            if rows == PATCH_SOURCE_ROWS {
                break;
            }
            charged += ROW_BYTES;
            rows += 1;
        }
        charged += wire_bytes(value);
        if charged > PATCH_SOURCE_BYTES {
            break;
        }
        start = at;
    }
    start
}
