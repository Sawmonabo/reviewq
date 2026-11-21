use std::collections::VecDeque;

use reviewq_app::changes::{PatchRange, PatchRead};

use super::Body;

// The browser includes up to four overlap bytes for UTF-8 window boundaries.
const MAX_BYTES: usize = 64 * 1024 + 8;

pub(super) enum Capture {
    None,
    From {
        start: usize,
        end: usize,
        bytes: Vec<u8>,
    },
    Tail {
        length: usize,
        bytes: VecDeque<u8>,
    },
}

impl Capture {
    pub(super) fn new(read: PatchRead) -> Self {
        match read {
            PatchRead::Range(Some(PatchRange::From { start, last })) => {
                let end = last.map_or(usize::MAX, |last| last.saturating_add(1));
                Self::From {
                    start,
                    end: end.min(start.saturating_add(MAX_BYTES)),
                    bytes: Vec::new(),
                }
            }
            PatchRead::Range(Some(PatchRange::Suffix(length))) if length > 0 => Self::Tail {
                length: length.min(MAX_BYTES),
                bytes: VecDeque::new(),
            },
            _ => Self::None,
        }
    }

    pub(super) fn inspect(&mut self, offset: usize, chunk: &[u8]) {
        match self {
            Self::None => {}
            Self::From { start, end, bytes } => {
                let first = offset.max(*start);
                let last = (offset + chunk.len()).min(*end);
                if first < last {
                    if bytes.is_empty() {
                        bytes.reserve_exact(*end - *start);
                    }
                    bytes.extend_from_slice(&chunk[first - offset..last - offset]);
                }
            }
            Self::Tail { length, bytes } => {
                let new = &chunk[chunk.len().saturating_sub(*length)..];
                let excess = (bytes.len() + new.len()).saturating_sub(*length);
                bytes.drain(..excess);
                if bytes.capacity() == 0 {
                    bytes.reserve_exact(*length);
                }
                bytes.extend(new);
            }
        }
    }

    pub(super) fn finish(self, length: usize) -> Body {
        let (start, bytes) = match self {
            Self::None => (0, Vec::new()),
            Self::From { start, bytes, .. } => (start, bytes),
            Self::Tail { bytes, .. } => (length - bytes.len(), Vec::from(bytes)),
        };
        Body::Window { start, bytes }
    }
}
