use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::atomic::{AtomicBool, Ordering};

use reviewq_app::access::AppError;
use reviewq_app::history::{
    ContentReference, FileContentRef, FilePosition, NativeHistoryPage, NativePosition,
    VisibleEntry, VisiblePart, VisiblePartKind,
};
use reviewq_domain::identity::NativeInputId;
use reviewq_domain::session::HostProvider;
use sha2::{Digest, Sha256};

use super::locator::Source;
use super::records;

mod lines;
pub(super) use lines::Lines;

const BLOCK: usize = 32 * 1024;
pub(super) const MAX_RECORDS: usize = 100_000;
pub(super) const READ_BUDGET: u64 = 512 * 1024 * 1024;

pub(super) fn page(
    source: &Source,
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
    let generation = generation(source)?;
    let had_position = position.file.is_some();
    let initial = match position.file {
        Some(witness) => {
            verify_position(file, &witness, complete)?;
            witness.offset
        }
        None if position.ascending => 0,
        None => complete,
    };
    let mut offset = initial;
    let mut entries = Vec::with_capacity(usize::from(limit));
    let mut tool_proof = ToolProof::new();
    let mut lines = Lines::new(file, cancelled);
    for _ in 0..MAX_RECORDS {
        if cancelled.load(Ordering::Relaxed) {
            return Err(AppError::Unavailable);
        }
        if !position.ascending && offset == 0 || position.ascending && offset == complete {
            break;
        }
        let (start, end) = if position.ascending {
            (offset, lines.next(offset, complete, &mut budget)?)
        } else {
            (lines.previous(offset - 1, &mut budget)?, offset)
        };
        if end == start {
            break;
        }
        if let Some(mut record) = records::project(
            source.provider,
            lines.input(start, end),
            &source.native_id,
            source.root_session.as_ref(),
            0,
            2_052,
        ) {
            tool_proof.redact(source, start, &mut record, cancelled);
            let text = readable_prefix(&record.text)?
                .chars()
                .take(512)
                .collect::<String>();
            let pointer = file_position(file, start, complete)?;
            let parts = visible_parts(record.parts, &generation, &record.id, &pointer)?;
            entries.push(VisibleEntry {
                id: record.id.clone(),
                turn: record.turn,
                at_ms: record.at_ms,
                kind: record.kind,
                text,
                text_bytes: Some(record.bytes),
                parts,
                next_part: record.next_part,
                content: Some(ContentReference::File(FileContentRef {
                    generation: generation.clone(),
                    entry: record.id,
                    part: None,
                    position: pointer,
                })),
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

pub(super) fn parts(
    source: &Source,
    reference: FileContentRef,
    first: u16,
    cancelled: &AtomicBool,
) -> Result<reviewq_app::history::NativeParts, AppError> {
    verify_content(source, &reference)?;
    let start = reference.position.offset;
    let mut budget = READ_BUDGET;
    let end = next_line(
        &source.file,
        start,
        reference.position.high_water,
        &mut budget,
        cancelled,
    )?;
    if end == start || !boundary(&source.file, end)? {
        return Err(AppError::CursorExpired);
    }
    let mut record = records::project_parts(
        source.provider,
        input(&source.file, start, end, cancelled),
        &source.native_id,
        source.root_session.as_ref(),
        first,
    )
    .filter(|record| record.id == reference.entry)
    .ok_or(AppError::CursorExpired)?;
    ToolProof::new().redact(source, start, &mut record, cancelled);
    let parts = visible_parts(
        record.parts,
        &reference.generation,
        &record.id,
        &reference.position,
    )?;
    verify_content(source, &reference)?;
    Ok(reviewq_app::history::NativeParts {
        parts,
        next: record.next_part,
    })
}

fn visible_parts(
    parts: Vec<records::Part>,
    generation: &str,
    id: &NativeInputId,
    position: &FilePosition,
) -> Result<Vec<VisiblePart>, AppError> {
    parts
        .into_iter()
        .map(|part| {
            let text = readable_prefix(&part.body.picked)?.to_owned();
            let content = (part.body.bytes > text.len() as u64).then(|| {
                ContentReference::File(FileContentRef {
                    generation: generation.to_owned(),
                    entry: id.clone(),
                    part: Some(part.index),
                    position: position.clone(),
                })
            });
            Ok(VisiblePart {
                native_index: part.index,
                kind: part.kind,
                text,
                content,
            })
        })
        .collect()
}

pub(super) fn verify_content(source: &Source, reference: &FileContentRef) -> Result<(), AppError> {
    if generation(source)? != reference.generation {
        return Err(AppError::CursorExpired);
    }
    verify_position(
        &source.file,
        &reference.position,
        source.file.metadata().map_err(io_error)?.len(),
    )
}

pub(super) struct ContentSource {
    pub end: u64,
    pub bytes: u64,
    pub field: records::BodyField,
}

pub(super) fn content_source(
    source: &Source,
    reference: &FileContentRef,
    cancelled: &AtomicBool,
) -> Result<ContentSource, AppError> {
    verify_content(source, reference)?;
    let file = &source.file;
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
    let mut record = records::project_window(
        source.provider,
        input(file, start, end, cancelled),
        &source.native_id,
        source.root_session.as_ref(),
        records::Window {
            start: 0,
            limit: 0,
            part: reference.part,
        },
    )
    .filter(|record| record.id == reference.entry)
    .ok_or(AppError::CursorExpired)?;
    ToolProof::new().redact(source, start, &mut record, cancelled);
    let (bytes, field) = if let Some(index) = reference.part {
        let part = record
            .parts
            .into_iter()
            .find(|part| part.index == index)
            .ok_or(AppError::CursorExpired)?;
        let field = match (&part.kind, source.provider) {
            (VisiblePartKind::Text(_), _) => records::BodyField::TextBlock(index),
            (VisiblePartKind::Plan, HostProvider::Codex) => {
                records::BodyField::CombinedText(record.text_blocks)
            }
            (VisiblePartKind::ToolCall { .. }, HostProvider::Codex) => {
                records::BodyField::CodexField(
                    record.argument_field.ok_or(AppError::CursorExpired)?,
                )
            }
            (VisiblePartKind::ToolResult { .. }, HostProvider::Codex) => {
                records::BodyField::CodexField(record.result_field.ok_or(AppError::CursorExpired)?)
            }
            (VisiblePartKind::ToolResult { .. }, HostProvider::Claude) => {
                records::BodyField::ResultBlock(index)
            }
            _ => records::BodyField::InputBlock(index),
        };
        (part.body.bytes, field)
    } else {
        (
            record.bytes,
            records::BodyField::CombinedText(record.text_blocks),
        )
    };
    Ok(ContentSource { end, bytes, field })
}

struct ToolProof {
    parents: HashMap<NativeInputId, Vec<(NativeInputId, String)>>,
    budget: u64,
}

impl ToolProof {
    fn new() -> Self {
        Self {
            parents: HashMap::new(),
            budget: 4 * 1024 * 1024,
        }
    }

    fn redact(
        &mut self,
        source: &Source,
        start: u64,
        record: &mut records::Record,
        cancel: &AtomicBool,
    ) {
        if source.provider != HostProvider::Claude
            || !record
                .parts
                .iter()
                .any(|part| matches!(part.kind, VisiblePartKind::ToolResult { .. }))
        {
            return;
        }
        let calls = record.claude_tool_parent.as_ref().map(|parent| {
            self.parents.entry(parent.clone()).or_insert_with(|| {
                tool_parent(source, start, parent, &mut self.budget, cancel).unwrap_or_default()
            })
        });
        record.parts.retain(|part| {
            let VisiblePartKind::ToolResult { call, .. } = &part.kind else {
                return true;
            };
            calls.as_ref().is_some_and(|calls| {
                let mut matching = calls.iter().filter(|(id, _)| id == call);
                matching
                    .next()
                    .is_some_and(|(_, name)| super::tools::visible_claude_result(name))
                    && matching.next().is_none()
            })
        });
    }
}

fn tool_parent(
    source: &Source,
    mut end: u64,
    parent: &NativeInputId,
    budget: &mut u64,
    cancel: &AtomicBool,
) -> Option<Vec<(NativeInputId, String)>> {
    for _ in 0..128 {
        if end == 0 || *budget == 0 {
            break;
        }
        let start = end_of_line(&source.file, end - 1, budget, cancel).ok()?;
        if let Some(candidate) = records::project(
            HostProvider::Claude,
            input(&source.file, start, end, cancel),
            &source.native_id,
            source.root_session.as_ref(),
            0,
            0,
        ) && &candidate.id == parent
            && candidate.kind == reviewq_app::history::VisibleKind::Assistant
        {
            return Some(candidate.claude_tool_calls);
        }
        end = start;
    }
    None
}

pub(super) fn readable_prefix(raw: &[u8]) -> Result<&str, AppError> {
    match std::str::from_utf8(raw) {
        Ok(text) => Ok(text),
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&raw[..error.valid_up_to()]).map_err(|_| AppError::CursorExpired)
        }
        Err(_) => Err(AppError::CursorExpired),
    }
}

pub(super) fn file_position(
    file: &File,
    offset: u64,
    high_water: u64,
) -> Result<FilePosition, AppError> {
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

pub(super) fn verify_position(
    file: &File,
    position: &FilePosition,
    complete: u64,
) -> Result<(), AppError> {
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

pub(super) fn end_of_line(
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

pub(super) fn boundary(file: &File, value: u64) -> Result<bool, AppError> {
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
