use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;

use super::{Body, RecordWindow, check_cancel};

const BLOCK_BYTES: usize = 32 * 1024;

pub(super) struct NativeString {
    pub(super) key: u16,
    end: u64,
    points: Vec<Point>,
}

struct Point {
    source: u64,
    decoded: u64,
}

impl NativeString {
    pub(super) fn prepare(
        file: &File,
        record: &RecordWindow,
        body: &Body,
        cancel: &AtomicBool,
    ) -> Result<Option<Self>, AppError> {
        let Some(field) = &body.native_string else {
            return Ok(None);
        };
        let first = record.position.offset + field.start;
        let last = record.position.offset + field.end;
        if first + 2 > last || last > record.end {
            return Err(AppError::CursorExpired);
        }
        let mut quote = [0];
        for at in [first, last - 1] {
            file.read_exact_at(&mut quote, at)
                .map_err(|_| AppError::Unavailable)?;
            if quote[0] != b'"' {
                return Err(AppError::CursorExpired);
            }
        }
        let end = last - 1;
        let mut source = first + 1;
        let mut decoded = 0;
        let mut points = Vec::new();
        let mut scratch = Vec::new();
        while source < end {
            check_cancel(cancel)?;
            points.push(Point { source, decoded });
            let (next, text) = decode(file, source, end, &mut scratch)?;
            decoded += text.len() as u64;
            source = next;
        }
        if decoded != body.bytes {
            return Err(AppError::CursorExpired);
        }
        Ok(Some(Self {
            key: body.key(),
            end,
            points,
        }))
    }

    pub(super) fn read(
        &self,
        file: &File,
        from: u64,
        end: u64,
        cancel: &AtomicBool,
        consume: &mut impl FnMut(u64, &str) -> Result<bool, AppError>,
    ) -> Result<(), AppError> {
        let start = self
            .points
            .partition_point(|point| point.decoded <= from)
            .saturating_sub(1);
        let mut scratch = Vec::new();
        for point in self.points[start..]
            .iter()
            .take_while(|point| point.decoded < end)
        {
            check_cancel(cancel)?;
            let (_, text) = decode(file, point.source, self.end, &mut scratch)?;
            if !consume(point.decoded, &text)? {
                break;
            }
        }
        Ok(())
    }
}

/// Decode a bounded segment of an already verified JSON string with the JSON library.
/// The native span and complete escape/scalar boundaries allow independent re-seeks.
fn decode(
    file: &File,
    source: u64,
    end: u64,
    scratch: &mut Vec<u8>,
) -> Result<(u64, String), AppError> {
    let size = (end - source).min(BLOCK_BYTES as u64) as usize;
    scratch.resize(size + 2, 0);
    scratch[0] = b'"';
    file.read_exact_at(&mut scratch[1..=size], source)
        .map_err(|_| AppError::Unavailable)?;
    let mut length = size;
    if size == BLOCK_BYTES && source + (size as u64) < end {
        length = 0;
        let raw = &scratch[1..=size];
        while length < raw.len() {
            let remaining = &raw[length..];
            let count = match remaining[0] {
                b'\\' if remaining.get(1) == Some(&b'u') => {
                    if remaining.len() < 6 {
                        break;
                    }
                    let high = remaining[2].eq_ignore_ascii_case(&b'd')
                        && matches!(remaining[3].to_ascii_lowercase(), b'8'..=b'b');
                    if high { 12 } else { 6 }
                }
                b'\\' => 2,
                0x00..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return Err(AppError::CursorExpired),
            };
            if count > remaining.len() {
                break;
            }
            length += count;
        }
    }
    if length == 0 {
        return Err(AppError::CursorExpired);
    }
    scratch[length + 1] = b'"';
    let text = serde_json::from_slice::<String>(&scratch[..length + 2])
        .map_err(|_| AppError::CursorExpired)?;
    Ok((source + length as u64, text))
}
