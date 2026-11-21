use reviewq_app::access::AppError;
use reviewq_app::changes::{PatchSide, PatchSourceCursor, PatchSourceTarget};

use super::super::wire::{HUNK_HEADER_PREFIX, hunk_ranges};
use super::SourceRead;

pub(in crate::gitlab::changes) const CHECKPOINT_BYTES: usize = 64 * 1024;
const MIN_CHECKPOINT_BYTES: usize = 4 * 1024;
const CHECKPOINT_ROWS: usize = 128;

#[derive(Clone, Copy, Default)]
struct Side {
    next: u32,
    remaining: usize,
    seen: u32,
}

impl Side {
    fn set(&mut self, (start, count): (u32, usize)) {
        self.next = start;
        self.remaining = count;
    }

    fn advance(&mut self) -> u32 {
        let line = self.next;
        self.next += 1;
        self.remaining -= 1;
        self.seen = self.seen.max(line);
        line
    }
}

#[derive(Clone, Copy, Default)]
struct Checkpoint {
    at: PatchSourceCursor,
    hunk: Option<usize>,
    old: Side,
    new: Side,
}

#[derive(Clone, Copy)]
pub(super) struct Row {
    pub(super) at: PatchSourceCursor,
    after: Checkpoint,
    pub(super) hunk: Option<usize>,
    pub(super) old_line: Option<u32>,
    pub(super) new_line: Option<u32>,
}

impl Checkpoint {
    fn row(mut self, end: usize, first: Option<u8>, header: &[u8]) -> Row {
        let at = self.at;
        let (mut old_line, mut new_line) = (None, None);
        match first {
            Some(b'@') => {
                if let Some((old, new)) = hunk_ranges(header) {
                    self.hunk = Some(at.byte);
                    self.old.set(old);
                    self.new.set(new);
                }
            }
            Some(b' ') if self.old.remaining > 0 && self.new.remaining > 0 => {
                old_line = Some(self.old.advance());
                new_line = Some(self.new.advance());
            }
            Some(b'-') if self.old.remaining > 0 => old_line = Some(self.old.advance()),
            Some(b'+') if self.new.remaining > 0 => new_line = Some(self.new.advance()),
            _ => {}
        }
        self.at = PatchSourceCursor {
            row: at.row + 1,
            byte: end,
        };
        Row {
            at,
            after: self,
            hunk: self.hunk,
            old_line,
            new_line,
        }
    }
}

impl Row {
    pub(super) fn end(self) -> PatchSourceCursor {
        self.after.at
    }

    pub(super) fn next_row(self, bytes: &[u8]) -> Self {
        self.after.row(
            self.after.at.byte + bytes.len(),
            bytes.first().copied(),
            &bytes[..bytes.len().min(HUNK_HEADER_PREFIX)],
        )
    }

    fn matches(self, target: PatchSourceTarget) -> bool {
        match target {
            PatchSourceTarget::Row { row, .. } => self.at.row == row,
            PatchSourceTarget::Byte(byte) => byte < self.end().byte,
            PatchSourceTarget::Line { side, line } => match side {
                PatchSide::Old => self.old_line == Some(line),
                PatchSide::New => self.new_line == Some(line),
            },
            _ => false,
        }
    }
}

pub(in crate::gitlab::changes) struct SourceIndex {
    pub(super) total: PatchSourceCursor,
    checkpoints: Vec<Checkpoint>,
    long_rows: Vec<Row>,
    old_disordered: bool,
    new_disordered: bool,
}

#[derive(Default)]
pub(in crate::gitlab::changes) struct IndexBuilder {
    at: Checkpoint,
    checkpoints: Vec<Checkpoint>,
    long_rows: Vec<Row>,
    old_disordered: bool,
    new_disordered: bool,
}

impl IndexBuilder {
    pub(in crate::gitlab::changes) fn push(
        &mut self,
        end: usize,
        first: Option<u8>,
        header: &[u8],
    ) {
        let at = self.at;
        if self.checkpoints.last().is_none_or(|previous| {
            // Small physical rows still yield a bounded lookup by bytes and
            // cannot grow the index in proportion to their row count.
            (at.at.row - previous.at.row >= CHECKPOINT_ROWS
                && at.at.byte - previous.at.byte >= MIN_CHECKPOINT_BYTES)
                || end - previous.at.byte > CHECKPOINT_BYTES
        }) {
            self.checkpoints.push(at);
        }
        let row = at.row(end, first, header);
        if end - at.at.byte > CHECKPOINT_BYTES {
            // With checkpoints on either side, no lookup has to walk this line.
            self.long_rows.push(row);
        }
        self.old_disordered |= row.old_line.is_some_and(|line| line <= at.old.seen);
        self.new_disordered |= row.new_line.is_some_and(|line| line <= at.new.seen);
        self.at = row.after;
    }

    pub(in crate::gitlab::changes) fn finish(self) -> SourceIndex {
        SourceIndex {
            total: self.at.at,
            checkpoints: self.checkpoints,
            long_rows: self.long_rows,
            old_disordered: self.old_disordered,
            new_disordered: self.new_disordered,
        }
    }
}

impl SourceIndex {
    pub(in crate::gitlab::changes) fn memory(bytes: &[u8]) -> Self {
        let mut builder = IndexBuilder::default();
        let mut end = 0;
        for row in bytes.split_inclusive(|byte| *byte == b'\n') {
            end += row.len();
            builder.push(
                end,
                row.first().copied(),
                &row[..row.len().min(HUNK_HEADER_PREFIX)],
            );
        }
        builder.finish()
    }

    pub(super) async fn locate(
        &self,
        body: &mut impl SourceRead,
        target: PatchSourceTarget,
    ) -> Result<Row, AppError> {
        let index = match target {
            PatchSourceTarget::Row { row, .. } if row < self.total.row => self
                .checkpoints
                .partition_point(|point| point.at.row <= row),
            PatchSourceTarget::Byte(byte) if byte < self.total.byte => self
                .checkpoints
                .partition_point(|point| point.at.byte <= byte),
            PatchSourceTarget::Line {
                side: PatchSide::Old,
                line,
            } if !self.old_disordered => self.checkpoints.partition_point(|p| p.old.seen < line),
            PatchSourceTarget::Line {
                side: PatchSide::New,
                line,
            } if !self.new_disordered => self.checkpoints.partition_point(|p| p.new.seen < line),
            PatchSourceTarget::Line { .. } => return Err(AppError::Unavailable),
            _ => return Err(AppError::RangeNotSatisfiable),
        };
        let mut mark = self
            .checkpoints
            .get(index.checked_sub(1).ok_or(AppError::NotFound)?)
            .copied()
            .ok_or(AppError::NotFound)?;
        if let Some(row) = self.long_row(mark.at.byte) {
            return row.matches(target).then_some(row).ok_or(AppError::NotFound);
        }
        let end = self
            .checkpoints
            .get(index)
            .map_or(self.total.byte, |next| next.at.byte);
        let bytes = body.read(mark.at.byte, end - mark.at.byte).await?;
        for line in bytes.split_inclusive(|byte| *byte == b'\n') {
            let row = mark.row(
                mark.at.byte + line.len(),
                line.first().copied(),
                &line[..line.len().min(HUNK_HEADER_PREFIX)],
            );
            if row.matches(target) {
                return Ok(row);
            }
            mark = row.after;
        }
        Err(AppError::NotFound)
    }

    pub(super) fn long_row(&self, start: usize) -> Option<Row> {
        self.long_rows
            .binary_search_by_key(&start, |row| row.at.byte)
            .ok()
            .map(|index| self.long_rows[index])
    }

    pub(super) fn byte_at_or_after_row(&self, row: usize) -> usize {
        let next = self.checkpoints.partition_point(|point| point.at.row < row);
        self.checkpoints
            .get(next)
            .map_or(self.total.byte, |point| point.at.byte)
    }
}
