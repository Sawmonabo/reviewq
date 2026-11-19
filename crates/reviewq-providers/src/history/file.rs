use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::atomic::{AtomicBool, Ordering};

use reviewq_app::access::AppError;
use reviewq_app::history::{NativeHistoryPage, NativePosition};
use sha2::{Digest, Sha256};

use super::locator::Source;
use super::records;

const BLOCK: usize = 32 * 1024;
const MAX_RECORDS: usize = 100_000;
const READ_BUDGET: u64 = 512 * 1024 * 1024;

pub(super) fn page(
    source: Source,
    position: NativePosition,
    limit: u16,
    cancelled: &AtomicBool,
) -> Result<NativeHistoryPage, AppError> {
    if position.cursor.is_some() || position.within.is_some() || position.skip_turn.is_some() {
        return Err(AppError::CursorExpired);
    }
    let file = &source.file;
    let length = file.metadata().map_err(io_error)?.len();
    let mut budget = READ_BUDGET;
    let complete = end_of_line(file, length, &mut budget, cancelled)?;
    let generation = generation(&source)?;
    let had_position = position.file_offset.is_some();
    let initial = position.file_offset.unwrap_or(complete);
    if initial > complete || !boundary(file, initial)? {
        return Err(AppError::CursorExpired);
    }
    let mut offset = initial;
    let mut entries = Vec::with_capacity(usize::from(limit));
    for _ in 0..MAX_RECORDS {
        if cancelled.load(Ordering::Relaxed) {
            return Err(AppError::Unavailable);
        }
        if !position.ascending && offset == 0 || position.ascending && offset == complete {
            break;
        }
        let (start, end) = if position.ascending {
            (
                offset,
                next_line(file, offset, complete, &mut budget, cancelled)?,
            )
        } else {
            (
                end_of_line(file, offset - 1, &mut budget, cancelled)?,
                offset,
            )
        };
        if end == start {
            break;
        }
        let input = input(file, start, end, cancelled);
        if let Some(entry) = records::project(source.provider, input, start, &source.native_id)? {
            entries.push(entry);
        }
        offset = if position.ascending { end } else { start };
        if entries.len() == usize::from(limit) || budget == 0 {
            break;
        }
    }
    if !position.ascending {
        entries.reverse();
    }
    let older_offset = if position.ascending { initial } else { offset };
    let newer_offset = if position.ascending { offset } else { initial };
    let position = |offset, ascending| {
        Some(NativePosition {
            file_offset: Some(offset),
            ascending,
            ..NativePosition::default()
        })
    };
    Ok(NativeHistoryPage {
        generation,
        entries,
        older: (older_offset > 0)
            .then(|| position(older_offset, false))
            .flatten(),
        newer: (had_position && newer_offset < complete)
            .then(|| position(newer_offset, true))
            .flatten(),
        within: None,
    })
}

pub(super) fn generation(source: &Source) -> Result<String, AppError> {
    let metadata = source.file.metadata().map_err(io_error)?;
    let mut digest = Sha256::new();
    digest.update(source.native_id.as_str().as_bytes());
    digest.update(source.fingerprint);
    digest.update(metadata.dev().to_be_bytes());
    digest.update(metadata.ino().to_be_bytes());
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

pub(super) fn next_line(
    file: &File,
    start: u64,
    end: u64,
    budget: &mut u64,
    cancelled: &AtomicBool,
) -> Result<u64, AppError> {
    let mut offset = start;
    let mut block = [0; BLOCK];
    while offset < end {
        let length = block_size(end - offset, budget, cancelled)?;
        let read = &mut block[..length];
        file.read_exact_at(read, offset).map_err(io_error)?;
        if let Some(index) = read.iter().position(|byte| *byte == b'\n') {
            return Ok(offset + index as u64 + 1);
        }
        offset += length as u64;
    }
    Ok(end)
}

fn end_of_line(
    file: &File,
    mut end: u64,
    budget: &mut u64,
    cancelled: &AtomicBool,
) -> Result<u64, AppError> {
    let mut block = [0; BLOCK];
    while end > 0 {
        let length = block_size(end, budget, cancelled)?;
        let start = end - length as u64;
        let read = &mut block[..length];
        file.read_exact_at(read, start).map_err(io_error)?;
        if let Some(index) = read.iter().rposition(|byte| *byte == b'\n') {
            return Ok(start + index as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

fn block_size(remaining: u64, budget: &mut u64, cancelled: &AtomicBool) -> Result<usize, AppError> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(AppError::Unavailable);
    }
    let length = remaining.min(*budget).min(BLOCK as u64);
    if length == 0 {
        return Err(AppError::PayloadTooLarge);
    }
    *budget -= length;
    Ok(length as usize)
}

fn boundary(file: &File, value: u64) -> Result<bool, AppError> {
    if value == 0 {
        return Ok(true);
    }
    let mut byte = [0];
    file.read_exact_at(&mut byte, value - 1).map_err(io_error)?;
    Ok(byte[0] == b'\n')
}

pub(super) fn input<'a>(
    file: &'a File,
    start: u64,
    end: u64,
    cancel: &'a AtomicBool,
) -> impl Read + 'a {
    FileInput {
        file,
        offset: start,
        end,
        cancel,
    }
}

struct FileInput<'a> {
    file: &'a File,
    offset: u64,
    end: u64,
    cancel: &'a AtomicBool,
}

impl Read for FileInput<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other("native history read cancelled"));
        }
        let limit = (self.end - self.offset).min(output.len() as u64) as usize;
        let amount = self.file.read_at(&mut output[..limit], self.offset)?;
        if amount == 0 && limit > 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.offset += amount as u64;
        Ok(amount)
    }
}

fn io_error(error: io::Error) -> AppError {
    match error.kind() {
        io::ErrorKind::NotFound => AppError::NotFound,
        io::ErrorKind::PermissionDenied => AppError::Forbidden,
        io::ErrorKind::UnexpectedEof => AppError::CursorExpired,
        _ => AppError::Unavailable,
    }
}
