use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::atomic::{AtomicBool, Ordering};

use reviewq_app::access::AppError;
use reviewq_app::history::{
    FileContentRef, FilePosition, NativeContent, NativeHistoryPage, NativePosition, VisibleEntry,
};
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
    let had_position = position.file.is_some();
    let initial = match position.file {
        Some(witness) => {
            verify_position(file, &witness, complete)?;
            witness.offset
        }
        None => complete,
    };
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
        if let Some(record) = records::project(
            source.provider,
            input(file, start, end, cancelled),
            &source.native_id,
            source.root_session.as_ref(),
            0,
            2_052,
        ) {
            let text = readable_prefix(&record.text)?
                .chars()
                .take(512)
                .collect::<String>();
            let pointer = file_position(file, start, complete)?;
            entries.push(VisibleEntry {
                id: record.id.clone(),
                turn: record.turn,
                at_ms: record.at_ms,
                kind: record.kind,
                text,
                text_bytes: Some(record.bytes),
                content: Some(FileContentRef {
                    generation: generation.clone(),
                    entry: record.id,
                    position: pointer,
                }),
            });
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
    let seek = |offset, ascending| {
        Ok::<_, AppError>(NativePosition {
            file: Some(file_position(file, offset, complete)?),
            ascending,
            ..NativePosition::default()
        })
    };
    Ok(NativeHistoryPage {
        generation,
        entries,
        older: (older_offset > 0)
            .then(|| seek(older_offset, false))
            .transpose()?,
        newer: (had_position && newer_offset < complete)
            .then(|| seek(newer_offset, true))
            .transpose()?,
        within: None,
    })
}

pub(super) fn content(
    source: Source,
    reference: FileContentRef,
    offset: u64,
    limit: u32,
    cancelled: &AtomicBool,
) -> Result<NativeContent, AppError> {
    let generation = generation(&source)?;
    if generation != reference.generation {
        return Err(AppError::CursorExpired);
    }
    let file = &source.file;
    verify_position(
        file,
        &reference.position,
        file.metadata().map_err(io_error)?.len(),
    )?;
    let start = reference.position.offset;
    let mut budget = READ_BUDGET;
    let end = next_line(
        file,
        start,
        reference.position.high_water,
        &mut budget,
        cancelled,
    )?;
    if end == start || !boundary(file, end)? {
        return Err(AppError::CursorExpired);
    }
    let limit = usize::try_from(limit).map_err(|_| AppError::InvalidRequest)?;
    let record = records::project(
        source.provider,
        input(file, start, end, cancelled),
        &source.native_id,
        source.root_session.as_ref(),
        offset,
        limit,
    )
    .filter(|entry| entry.id == reference.entry)
    .ok_or(AppError::CursorExpired)?;
    if offset > record.bytes {
        return Err(AppError::InvalidRequest);
    }
    let text = readable_prefix(&record.text)?.to_owned();
    let next = offset + text.len() as u64;
    // If a stale or hostile cursor landed inside a Unicode scalar, don't loop on the same offset.
    if text.is_empty() && next < record.bytes {
        return Err(AppError::CursorExpired);
    }
    verify_position(
        file,
        &reference.position,
        file.metadata().map_err(io_error)?.len(),
    )?;
    Ok(NativeContent {
        generation,
        text,
        next_offset: (next < record.bytes).then_some(next),
    })
}

fn readable_prefix(raw: &[u8]) -> Result<&str, AppError> {
    match std::str::from_utf8(raw) {
        Ok(text) => Ok(text),
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&raw[..error.valid_up_to()]).map_err(|_| AppError::CursorExpired)
        }
        Err(_) => Err(AppError::CursorExpired),
    }
}

fn file_position(file: &File, offset: u64, high_water: u64) -> Result<FilePosition, AppError> {
    if offset > high_water {
        return Err(AppError::CursorExpired);
    }
    let mut hash = Sha256::new();
    hash.update(offset.to_be_bytes());
    hash.update(high_water.to_be_bytes());
    for point in [offset, high_water] {
        let mut sample = [0; 2_048];
        let first = point.saturating_sub(1024);
        let end = point.saturating_add(1024).min(high_water);
        let length = (end - first) as usize;
        file.read_exact_at(&mut sample[..length], first)
            .map_err(io_error)?;
        hash.update(&sample[..length]);
    }
    Ok(FilePosition {
        offset,
        high_water,
        witness: hash.finalize().into(),
    })
}

fn verify_position(file: &File, position: &FilePosition, complete: u64) -> Result<(), AppError> {
    if position.high_water > complete
        || !boundary(file, position.offset)?
        || file_position(file, position.offset, position.high_water)? != *position
    {
        return Err(AppError::CursorExpired);
    }
    Ok(())
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
            *budget -= index as u64 + 1;
            return Ok(offset + index as u64 + 1);
        }
        *budget -= length as u64;
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
            *budget -= (length - index) as u64;
            return Ok(start + index as u64 + 1);
        }
        *budget -= length as u64;
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
