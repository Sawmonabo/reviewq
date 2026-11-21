use reviewq_app::changes::LineChanges;

const HUNK_HEADER_PREFIX: usize = 96;

/// Counts complete unified-diff hunks without buffering code or long section headings.
pub(super) struct DiffStats {
    counts: LineChanges,
    remaining: (usize, usize),
    saw_hunk: bool,
    invalid: bool,
    first: Option<u8>,
    header: [u8; HUNK_HEADER_PREFIX],
    header_len: usize,
}

impl DiffStats {
    pub(super) fn new() -> Self {
        Self {
            counts: LineChanges::default(),
            remaining: (0, 0),
            saw_hunk: false,
            invalid: false,
            first: None,
            header: [0; HUNK_HEADER_PREFIX],
            header_len: 0,
        }
    }

    pub(super) fn part(&mut self, bytes: &[u8], start: bool) {
        if start {
            self.first = bytes.first().copied();
        }
        if self.first == Some(b'@') {
            let copied = bytes.len().min(self.header.len() - self.header_len);
            let end = self.header_len + copied;
            self.header[self.header_len..end].copy_from_slice(&bytes[..copied]);
            self.header_len = end;
        }
    }

    pub(super) fn end_line(&mut self) {
        match (self.remaining, self.first) {
            ((0, 0), Some(b'@')) => {
                if let Some(((_, old), (_, new))) = hunk_ranges(&self.header[..self.header_len]) {
                    self.remaining = (old, new);
                    self.saw_hunk = true;
                } else {
                    self.invalid = true;
                }
            }
            ((0, 0), _) if !self.saw_hunk => {}
            ((old, new), Some(b' ')) if old > 0 && new > 0 => self.remaining = (old - 1, new - 1),
            ((old, new), Some(b'+')) if new > 0 => {
                self.remaining = (old, new - 1);
                self.counts.additions += 1;
            }
            ((old, new), Some(b'-')) if old > 0 => {
                self.remaining = (old - 1, new);
                self.counts.deletions += 1;
            }
            (_, Some(b'\\')) => {}
            _ => self.invalid = true,
        }
        self.header_len = 0;
    }

    pub(super) fn finish(self, empty: bool) -> Option<LineChanges> {
        (!self.invalid && self.remaining == (0, 0) && (self.saw_hunk || empty))
            .then_some(self.counts)
    }
}

pub(in crate::gitlab::changes) fn hunk_ranges(
    header: &[u8],
) -> Option<((u32, usize), (u32, usize))> {
    let mut fields = header.splitn(5, |byte| byte.is_ascii_whitespace());
    if fields.next()? != b"@@" {
        return None;
    }
    let old = span(fields.next()?.strip_prefix(b"-")?)?;
    let new = span(fields.next()?.strip_prefix(b"+")?)?;
    (fields.next()? == b"@@").then_some((old, new))
}

fn span(range: &[u8]) -> Option<(u32, usize)> {
    let range = std::str::from_utf8(range).ok()?;
    let (start, count) = range.split_once(',').unwrap_or((range, "1"));
    let (start, count) = (start.parse::<u32>().ok()?, count.parse::<usize>().ok()?);
    let end = u64::from(start).checked_add(u64::try_from(count).ok()?)?;
    (end <= u64::from(u32::MAX) && (start > 0 || count == 0)).then_some((start, count))
}
