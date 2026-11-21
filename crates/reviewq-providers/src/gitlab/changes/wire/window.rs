use std::collections::VecDeque;

use reviewq_app::changes::{PATCH_SOURCE_BYTES, PatchRange, PatchRead, PatchSourceTarget};

use super::super::source::SEEK_BEHIND_BYTES;
use super::{BodyPolicy, Storage};

// The browser includes up to four overlap bytes for UTF-8 window boundaries.
const MAX_BYTES: usize = 64 * 1024 + 8;
const READ_AHEAD_BYTES: usize = 1024 * 1024;
const SOURCE_CAPTURE_BYTES: usize = PATCH_SOURCE_BYTES + 2 * SEEK_BEHIND_BYTES;

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
    pub(super) fn new(read: PatchRead, policy: BodyPolicy) -> Self {
        let limit = if matches!(policy, BodyPolicy::ReadAhead) {
            READ_AHEAD_BYTES
        } else {
            MAX_BYTES
        };
        match read {
            PatchRead::Full => Self::from(0, limit),
            PatchRead::Range(Some(PatchRange::From { start, last })) => {
                let end = last.map_or(usize::MAX, |last| last.saturating_add(1));
                Self::from(start, end.min(start.saturating_add(limit)))
            }
            PatchRead::Range(Some(PatchRange::Suffix(length))) if length > 0 => Self::Tail {
                length: length.min(limit),
                bytes: VecDeque::new(),
            },
            PatchRead::Source(target) => Self::source(target),
            _ => Self::None,
        }
    }

    fn source(target: PatchSourceTarget) -> Self {
        let start = match target {
            PatchSourceTarget::Byte(byte)
            | PatchSourceTarget::Row {
                byte: Some(byte), ..
            } => byte.saturating_sub(SEEK_BEHIND_BYTES),
            PatchSourceTarget::Before(byte) => {
                byte.saturating_sub(PATCH_SOURCE_BYTES + SEEK_BEHIND_BYTES)
            }
            PatchSourceTarget::End => {
                return Self::Tail {
                    length: SOURCE_CAPTURE_BYTES,
                    bytes: VecDeque::new(),
                };
            }
            PatchSourceTarget::Row { row: 0, byte: None } => 0,
            _ => return Self::None,
        };
        Self::from(start, start.saturating_add(SOURCE_CAPTURE_BYTES))
    }

    fn from(start: usize, end: usize) -> Self {
        Self::From {
            start,
            end,
            bytes: Vec::new(),
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

    pub(super) fn finish(self, length: usize) -> Storage {
        let (start, bytes) = match self {
            Self::None => (0, Vec::new()),
            Self::From { start, bytes, .. } => (start, bytes),
            Self::Tail { bytes, .. } => (length - bytes.len(), Vec::from(bytes)),
        };
        Storage::Window { start, bytes }
    }
}
