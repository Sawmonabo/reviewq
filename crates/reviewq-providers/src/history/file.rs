use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::atomic::{AtomicBool, Ordering};

use reviewq_app::access::AppError;
use reviewq_app::history::{
    ContentReference, FileContentRef, FilePosition, NativeFinalResponse, NativeHistoryPage,
    NativePosition, VisibleEntry, VisiblePart, VisiblePartKind,
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
    let mut tool_proof = ToolProof::new(file, cancelled);
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
            tool_proof.redact(source, start, &mut record);
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

pub(super) fn final_response(
    source: &Source,
    cancelled: &AtomicBool,
) -> Result<NativeFinalResponse, AppError> {
    let mut budget = READ_BUDGET;
    let length = source.file.metadata().map_err(io_error)?.len();
    let complete = end_of_line(&source.file, length, &mut budget, cancelled)?;
    let generation = generation(source)?;
    let mut lines = Lines::new(&source.file, cancelled);
    let mut offset = complete;
    let mut entry = None;
    for _ in 0..MAX_RECORDS {
        if offset == 0 {
            break;
        }
        let start = lines.previous(offset - 1, &mut budget)?;
        let record = records::project(
            source.provider,
            lines.input(start, offset),
            &source.native_id,
            source.root_session.as_ref(),
            0,
            2_052,
        );
        offset = start;
        if let Some(record) = record.filter(|record| record.final_response && record.bytes > 0) {
            let position = file_position(&source.file, start, complete)?;
            entry = Some(VisibleEntry {
                id: record.id.clone(),
                turn: record.turn,
                at_ms: record.at_ms,
                kind: record.kind,
                text: readable_prefix(&record.text)?.chars().take(512).collect(),
                text_bytes: Some(record.bytes),
                parts: Vec::new(),
                next_part: None,
                content: Some(ContentReference::File(FileContentRef {
                    generation: generation.clone(),
                    entry: record.id,
                    part: None,
                    position,
                })),
            });
            break;
        }
    }
    if entry.is_none() && offset != 0 {
        return Err(AppError::Unavailable);
    }
    let failure = if source.provider == HostProvider::Codex {
        super::codex_report::latest_failure(source, complete, cancelled)
    } else {
        None
    };
    Ok(NativeFinalResponse {
        generation,
        entry,
        failure,
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
    ToolProof::new(&source.file, cancelled).redact(source, start, &mut record);
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
                text_bytes: part.body.bytes,
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
    ToolProof::new(&source.file, cancelled).redact(source, start, &mut record);
    let (bytes, field) = if let Some(index) = reference.part {
        let part = record
            .parts
            .iter()
            .find(|part| part.index == index)
            .ok_or(AppError::CursorExpired)?;
        (part.body.bytes, part_field(source.provider, &record, part)?)
    } else {
        (
            record.bytes,
            records::BodyField::CombinedText(record.text_blocks),
        )
    };
    Ok(ContentSource { end, bytes, field })
}

type DocumentSource = (u64, u64, Vec<(u64, records::NativeValue)>);

pub(super) fn document_source(
    source: &Source,
    reference: &FileContentRef,
    cancelled: &AtomicBool,
) -> Result<DocumentSource, AppError> {
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
    let record = records::project_document(
        source.provider,
        input(&source.file, start, end, cancelled),
        &source.native_id,
        source.root_session.as_ref(),
        reference.part,
    )?
    .filter(|record| record.id == reference.entry)
    .ok_or(AppError::CursorExpired)?;
    let original = |part: records::Part| {
        let native = part
            .body
            .native_value
            .filter(|value| value.string)
            .ok_or(AppError::NotFound)?;
        Ok((part.body.bytes, native))
    };
    let (bytes, fields) = if let Some(index) = reference.part {
        let part = record
            .parts
            .into_iter()
            .find(|part| {
                part.index == index
                    && matches!(part.kind, VisiblePartKind::Text(_) | VisiblePartKind::Plan)
            })
            .ok_or(AppError::NotFound)?;
        let field = original(part)?;
        (field.0, vec![field])
    } else if let Some(field) = record.native_value.filter(|value| value.string) {
        (record.bytes, vec![(record.bytes, field)])
    } else {
        let parts = record
            .parts
            .into_iter()
            .filter(|part| part.body.bytes > 0 && matches!(part.kind, VisiblePartKind::Text(_)))
            .map(original)
            .collect::<Result<Vec<_>, _>>()?;
        (record.bytes, parts)
    };
    Ok((end, bytes, fields))
}

pub(super) fn part_field(
    provider: HostProvider,
    record: &records::Record,
    part: &records::Part,
) -> Result<records::BodyField, AppError> {
    use records::BodyField;
    Ok(match (&part.kind, provider) {
        (VisiblePartKind::Text(_), _) => BodyField::TextBlock(part.index),
        (VisiblePartKind::Plan, HostProvider::Codex) => {
            BodyField::CombinedText(record.text_blocks.clone())
        }
        (VisiblePartKind::ToolCall { .. }, HostProvider::Codex) => BodyField::CodexField(
            record
                .argument_field
                .clone()
                .ok_or(AppError::CursorExpired)?,
        ),
        (VisiblePartKind::ToolResult { .. }, HostProvider::Codex) => {
            BodyField::CodexField(record.result_field.clone().ok_or(AppError::CursorExpired)?)
        }
        (VisiblePartKind::ToolResult { .. }, HostProvider::Claude) => {
            BodyField::ResultBlock(part.index)
        }
        _ => BodyField::InputBlock(part.index),
    })
}

pub(super) struct ToolProof<'a> {
    parents: HashMap<NativeInputId, Vec<(NativeInputId, String)>>,
    budget: u64,
    lines: Lines<'a>,
}

impl<'a> ToolProof<'a> {
    pub(super) fn new(file: &'a File, cancel: &'a AtomicBool) -> Self {
        Self {
            parents: HashMap::new(),
            budget: 4 * 1024 * 1024,
            lines: Lines::new(file, cancel),
        }
    }

    pub(super) fn redact(&mut self, source: &Source, start: u64, record: &mut records::Record) {
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
                tool_parent(source, start, parent, &mut self.budget, &mut self.lines)
                    .unwrap_or_default()
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
    lines: &mut Lines<'_>,
) -> Option<Vec<(NativeInputId, String)>> {
    for _ in 0..128 {
        if end == 0 || *budget == 0 {
            break;
        }
        let start = lines.previous(end - 1, budget).ok()?;
        if let Some(candidate) = records::project(
            HostProvider::Claude,
            lines.input(start, end),
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
    FileInput::new(file, start, end, cancel)
}

struct FileInput<'a> {
    bytes: BufReader<FileRange<'a>>,
    cancel: &'a AtomicBool,
}

impl<'a> FileInput<'a> {
    fn new(file: &'a File, start: u64, end: u64, cancel: &'a AtomicBool) -> Self {
        // Struson does not buffer its underlying reader. Amortize its small reads
        // without buffering the whole native record or passing its signed end.
        let capacity = (end - start).min(8 * 1024) as usize;
        Self {
            bytes: BufReader::with_capacity(
                capacity,
                FileRange {
                    file,
                    offset: start,
                    end,
                },
            ),
            cancel,
        }
    }
}

impl Read for FileInput<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        check_read_cancelled(self.cancel)?;
        self.bytes.read(output)
    }
}

struct FileRange<'a> {
    file: &'a File,
    offset: u64,
    end: u64,
}

impl Read for FileRange<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let limit = (self.end - self.offset).min(output.len() as u64) as usize;
        if limit == 0 {
            return Ok(0);
        }
        let amount = self.file.read_at(&mut output[..limit], self.offset)?;
        if amount == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.offset += amount as u64;
        Ok(amount)
    }
}

fn check_read_cancelled(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(io::Error::other("native history read cancelled"));
    }
    Ok(())
}

fn io_error(error: io::Error) -> AppError {
    match error.kind() {
        io::ErrorKind::NotFound => AppError::NotFound,
        io::ErrorKind::PermissionDenied => AppError::Forbidden,
        io::ErrorKind::UnexpectedEof => AppError::CursorExpired,
        _ => AppError::Unavailable,
    }
}

#[cfg(test)]
mod input_tests {
    use std::io::{Read, Write};

    use super::{AtomicBool, FileInput, Ordering};

    #[test]
    fn stops_at_the_record_end_and_checks_cancel_before_serving_buffered_bytes() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"before record after").unwrap();
        let cancel = AtomicBool::new(false);
        let mut selected = FileInput::new(&file, 7, 13, &cancel);
        let mut first = [0];
        selected.read_exact(&mut first).unwrap();
        assert_eq!(first, [b'r']);
        cancel.store(true, Ordering::Relaxed);
        assert!(selected.read_exact(&mut first).is_err());

        let running = AtomicBool::new(false);
        let mut record = Vec::new();
        FileInput::new(&file, 7, 13, &running)
            .read_to_end(&mut record)
            .unwrap();
        assert_eq!(record, b"record");
    }
}

#[cfg(test)]
mod result_tests {
    use std::io::Write;
    use std::sync::atomic::AtomicBool;

    use reviewq_domain::session::HostProvider;

    use super::{Source, final_response};

    #[test]
    fn selects_only_the_last_proven_native_final_response() {
        let mut source = Source {
            file: tempfile::tempfile().unwrap(),
            provider: HostProvider::Codex,
            native_id: "target".parse().unwrap(),
            fingerprint: [0; 32],
            root_session: None,
            path: "unused".into(),
        };
        let write = |file: &mut std::fs::File, item: serde_json::Value, thread: &str| {
            writeln!(
                file,
                "{}",
                serde_json::json!({"type":"event_msg","payload":{
                    "type":"item_completed","turn_id":"turn","thread_id":thread,"item":item,
                }})
            )
            .unwrap();
        };
        write(
            &mut source.file,
            serde_json::json!({"type":"AgentMessage","id":"earlier","text":"preview","phase":"commentary"}),
            "target",
        );
        let cancel = AtomicBool::new(false);
        assert!(final_response(&source, &cancel).unwrap().entry.is_none());
        write(
            &mut source.file,
            serde_json::json!({"type":"AgentMessage","id":"response","text":"final result","phase":"final_answer"}),
            "target",
        );
        write(
            &mut source.file,
            serde_json::json!({"type":"AgentMessage","id":"later","text":"preview","phase":"commentary"}),
            "target",
        );
        write(
            &mut source.file,
            serde_json::json!({"type":"AgentMessage","id":"async","text":"question","phase":"final_answer","delivery":"async"}),
            "target",
        );
        write(
            &mut source.file,
            serde_json::json!({"type":"AgentMessage","id":"other","text":"wrong session","phase":"final_answer"}),
            "other",
        );
        let result = final_response(&source, &cancel).unwrap().entry.unwrap();
        assert_eq!(result.id.as_str(), "response");
        assert_eq!(result.text, "final result");
    }
}
