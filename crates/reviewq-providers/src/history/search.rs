//! Find literal occurrences in the verified native display projection.

use std::collections::VecDeque;
use std::fs::{self, Metadata};
use std::io::{Read, Write};
use std::ops::Range;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use reviewq_app::access::AppError;
use reviewq_app::history::search::{
    HistorySearch, MAX_SEARCH_HITS, SearchPage, SearchPoint, SourceObservation,
};
use reviewq_app::history::{FileContentRef, FilePosition, VisiblePartKind};
use reviewq_app::wait::Budget;
use reviewq_domain::identity::NativeInputId;
use reviewq_domain::session::SessionTarget;
use sha2::{Digest, Sha256};

use super::documents::Documents;
use super::file::{self, Lines, ToolProof};
use super::locator::Source;
use super::records::{self, BodyField, NativeValue, Record};
use super::tools::stream_value;

mod document;
mod index;
mod large;
mod matches;
use super::native_string;
pub(super) use index::Acceleration;
pub(super) use large::LargeRecords;
use matches::Matches;

const RECORDS_PER_PAGE: usize = 64;
const PARTS_PER_PAGE: usize = 64;
const BYTES_PER_PAGE: u64 = 2 * 1024 * 1024;
const DECODE_BLOCK: usize = 16 * 1024;
const BODY_STEP: u64 = 512 * 1024;
const CONTENT_BYTES: u64 = 65_536;

/// A user's search of one session: whose session it is, what it looks for, and the budget
/// its waits for a document spend.
pub(super) struct Request {
    pub(super) owner: SessionTarget,
    pub(super) query: HistorySearch,
    pub(super) waits: Budget,
}

pub(super) fn read(
    source: &Source,
    request: &Request,
    cancel: &AtomicBool,
    large: &mut LargeRecords,
    documents: &Arc<Documents>,
    acceleration: Option<&Acceleration>,
) -> Result<SearchPage, AppError> {
    let Request {
        owner,
        query,
        waits,
    } = request;
    let mut budget = file::READ_BUDGET;
    let metadata = source.file.metadata().map_err(|_| AppError::Unavailable)?;
    let complete = file::end_of_line(&source.file, metadata.len(), &mut budget, cancel)?;
    let observation = observed(source, &metadata, complete);
    let generation = file::generation(source)?;
    if query
        .from
        .as_ref()
        .is_some_and(|cursor| cursor.observation != observation)
    {
        return Err(AppError::CursorExpired);
    }
    let mut lines = Lines::new(&source.file, cancel);
    let mut index = acceleration.and_then(|acceleration| {
        acceleration.use_index(source, owner, &observation, query, documents)
    });
    let mut proof = ToolProof::new(&source.file, cancel);
    let mut next = match &query.from {
        Some(cursor) => Some(cursor.point),
        None if query.reverse && complete > 0 => {
            Some(first(lines.previous(complete - 1, &mut budget)?, true))
        }
        None if complete > 0 => Some(first(0, false)),
        None => None,
    };
    let mut output = SearchPage {
        generation,
        observation,
        hits: Vec::with_capacity(MAX_SEARCH_HITS),
        next: None,
        partial: complete < metadata.len(),
    };
    let mut scanned = 0;
    for _ in 0..RECORDS_PER_PAGE {
        let Some(mut at) = next.take() else { break };
        if let Some(candidates) = index
            .as_ref()
            .filter(|index| at.record < index.covered_through())
        {
            match candidates.next(at.record, query.reverse) {
                Ok(Some(candidate)) if candidate != at.record => {
                    at = first(candidate, query.reverse)
                }
                Ok(Some(_)) => (),
                Ok(None) if query.reverse || candidates.covered_through() == complete => break,
                Ok(None) => at = first(candidates.covered_through(), query.reverse),
                Err(_) => index = None,
            }
        }
        if at.record >= complete || !file::boundary(&source.file, at.record)? {
            return Err(AppError::CursorExpired);
        }
        let prepared = large.take(owner, &output.observation, at.record);
        let end = if let Some(window) = &prepared {
            window.end
        } else {
            lines.next(at.record, complete, &mut budget)?
        };
        let prepared = if prepared.is_some() {
            prepared
        } else {
            match records::project_search(
                source.provider,
                lines.input(at.record, end),
                &source.native_id,
                source.root_session.as_ref(),
            ) {
                Ok(Some(mut record)) => {
                    proof.project(source, at.record..end, complete, &mut record);
                    let position = file::file_position(&source.file, at.record, complete)?;
                    Some(RecordWindow {
                        end,
                        record,
                        position,
                        staged: None,
                        native: None,
                        stage_unavailable: false,
                    })
                }
                Ok(None) => None,
                Err(_) => {
                    check_cancel(cancel)?;
                    output.partial = true;
                    None
                }
            }
        };
        if let Some(mut window) = prepared {
            output.partial |= window.record.next_part.is_some();
            next = match_record(
                MatchSource {
                    source,
                    owner,
                    lines: &lines,
                    cancel,
                    waits,
                    large,
                    documents,
                },
                &mut window,
                at,
                query,
                &mut output,
            )?;
            check_cancel(cancel)?;
            verify_observation(source, &output.observation)?;
            large.remember(owner, &output.observation, window);
            if next.is_some() {
                break;
            }
        }
        next = match query.reverse {
            true if at.record > 0 => Some(first(lines.previous(at.record - 1, &mut budget)?, true)),
            false if end < complete => Some(first(end, false)),
            _ => None,
        };
        scanned += end - at.record;
        // Deliver useful suggestions before scanning through the remaining conversation.
        if output.hits.len() == MAX_SEARCH_HITS
            || scanned >= BYTES_PER_PAGE
            || (!output.hits.is_empty() && scanned >= 128 * 1024)
        {
            break;
        }
    }
    verify_observation(source, &output.observation)?;
    output.next = next;
    Ok(output)
}

pub(super) fn confirm(
    source: &Source,
    observation: &SourceObservation,
    reference: &FileContentRef,
) -> Result<(), AppError> {
    verify_observation(source, observation)?;
    file::verify_content(source, reference)
}

fn first(record: u64, reverse: bool) -> SearchPoint {
    if reverse {
        SearchPoint {
            record,
            part: u16::MAX,
            window: None,
            byte: u64::MAX,
        }
    } else {
        SearchPoint {
            record,
            part: 0,
            window: None,
            byte: 0,
        }
    }
}

pub(super) fn observed(source: &Source, stamp: &Metadata, high_water: u64) -> SourceObservation {
    let mut version = Sha256::new();
    version.update(source.fingerprint);
    version.update(stamp.dev().to_be_bytes());
    version.update(stamp.ino().to_be_bytes());
    version.update(stamp.len().to_be_bytes());
    version.update(stamp.mtime().to_be_bytes());
    version.update(stamp.mtime_nsec().to_be_bytes());
    version.update(stamp.ctime().to_be_bytes());
    version.update(stamp.ctime_nsec().to_be_bytes());
    SourceObservation {
        high_water,
        version: version.finalize().into(),
    }
}

pub(super) fn verify_observation(
    source: &Source,
    expected: &SourceObservation,
) -> Result<(), AppError> {
    let descriptor = source.file.metadata().map_err(|_| AppError::Unavailable)?;
    let path = fs::symlink_metadata(&source.path).map_err(|_| AppError::CursorExpired)?;
    if !path.is_file()
        || observed(source, &descriptor, expected.high_water) != *expected
        || observed(source, &path, expected.high_water) != *expected
    {
        return Err(AppError::CursorExpired);
    }
    Ok(())
}

struct Body {
    field: BodyField,
    part: Option<u16>,
    tool_call: Option<NativeInputId>,
    markdown: bool,
    bytes: u64,
    native_value: Option<NativeValue>,
}

impl Body {
    fn key(&self) -> u16 {
        self.part.map_or(0, |index| index + 1)
    }

    fn document(&self) -> bool {
        self.tool_call.is_none() && self.bytes > CONTENT_BYTES
    }

    fn source_range(&self, record: Range<u64>) -> Result<Option<NativeValue>, AppError> {
        self.native_value
            .as_ref()
            .map(|value| {
                let start = record
                    .start
                    .checked_add(value.span.start)
                    .ok_or(AppError::CursorExpired)?;
                let end = record
                    .start
                    .checked_add(value.span.end)
                    .ok_or(AppError::CursorExpired)?;
                if start >= end || end > record.end {
                    return Err(AppError::CursorExpired);
                }
                Ok(NativeValue {
                    span: start..end,
                    string: value.string,
                })
            })
            .transpose()
    }
}

fn bodies(source: &Source, record: &Record) -> Result<Vec<Body>, AppError> {
    if record.parts.is_empty() {
        return Ok(vec![Body {
            field: BodyField::CombinedText(record.text_blocks.clone()),
            part: None,
            tool_call: None,
            markdown: record.bytes <= CONTENT_BYTES,
            bytes: record.bytes,
            native_value: record.native_value.clone(),
        }]);
    }
    record
        .parts
        .iter()
        .filter(|part| part.body.bytes > 0)
        .map(|part| {
            let tool_call = match &part.kind {
                VisiblePartKind::ToolCall { call, .. }
                | VisiblePartKind::ToolResult { call, .. } => Some(call.clone()),
                _ => None,
            };
            Ok(Body {
                field: file::part_field(source.provider, record, part)?,
                part: Some(part.index),
                markdown: tool_call.is_none() && part.body.bytes <= CONTENT_BYTES,
                tool_call,
                bytes: part.body.bytes,
                native_value: part.body.native_value.clone(),
            })
        })
        .collect()
}

struct RecordWindow {
    end: u64,
    record: Record,
    position: FilePosition,
    staged: Option<large::StagedBody>,
    native: Option<native_string::NativeString>,
    stage_unavailable: bool,
}

struct MatchSource<'a, 'b> {
    source: &'a Source,
    owner: &'a SessionTarget,
    lines: &'a Lines<'b>,
    cancel: &'a AtomicBool,
    waits: &'a Budget,
    large: &'a mut LargeRecords,
    documents: &'a Documents,
}

fn match_record(
    mut reader: MatchSource<'_, '_>,
    window: &mut RecordWindow,
    at: SearchPoint,
    query: &HistorySearch,
    output: &mut SearchPage,
) -> Result<Option<SearchPoint>, AppError> {
    let mut bodies = bodies(reader.source, &window.record)?;
    if query.reverse {
        bodies.reverse();
    }
    let mut repeated_bytes = 0;
    let mut parts = 0;
    for body in bodies {
        let key = body.key();
        if if query.reverse {
            key > at.part
        } else {
            key < at.part
        } {
            continue;
        }
        check_cancel(reader.cancel)?;
        let point = SearchPoint {
            record: at.record,
            part: key,
            window: if at.part == key { at.window } else { None },
            byte: 0,
        };
        let reference = FileContentRef {
            generation: output.generation.clone(),
            entry: window.record.id.clone(),
            part: body.part,
            position: window.position.clone(),
        };
        let boundary = if at.part == key {
            at.byte
        } else if query.reverse {
            u64::MAX
        } else {
            0
        };
        let available = MAX_SEARCH_HITS - output.hits.len();
        if available == 0 || parts == PARTS_PER_PAGE {
            return Ok(Some(SearchPoint {
                byte: boundary,
                ..point
            }));
        }
        parts += 1;
        let span = at.record..window.end;
        let field = body.source_range(span.clone())?;
        let mut matches = Matches {
            query: &query.literal,
            reverse: query.reverse,
            reference,
            body: &body,
            document: None,
            point,
            boundary,
            range: 0..u64::MAX,
            available,
            hits: VecDeque::new(),
            bytes: Vec::new(),
            decoded: 0,
            start_utf16: 0,
            next: None,
            partial: false,
            stopped: false,
        };
        if !body.document() || !document::scan(&mut matches, &reader)? {
            scan_body(
                &mut matches,
                &mut reader,
                window,
                span,
                field,
                &mut repeated_bytes,
            )?;
        }
        if query.reverse && matches.hits.len() == available {
            let earliest = matches.hits.front().ok_or(AppError::Internal)?.point;
            matches.next = Some(earliest);
        }
        output.partial |= matches.partial;
        if query.reverse {
            output.hits.extend(matches.hits.into_iter().rev());
        } else {
            output.hits.extend(matches.hits);
        }
        if matches.next.is_some() {
            return Ok(matches.next);
        }
    }
    Ok(None)
}

fn scan_body(
    matches: &mut Matches<'_>,
    reader: &mut MatchSource<'_, '_>,
    window: &mut RecordWindow,
    span: Range<u64>,
    field: Option<NativeValue>,
    repeated_bytes: &mut u64,
) -> Result<(), AppError> {
    let MatchSource {
        source,
        lines,
        large,
        cancel,
        ..
    } = reader;
    let body = matches.body;
    let key = body.key();
    if body.bytes > CONTENT_BYTES
        && field.as_ref().is_some_and(|field| field.string)
        && window
            .native
            .as_ref()
            .is_none_or(|native| native.key != key)
    {
        window.native = field.as_ref().and_then(|field| {
            native_string::NativeString::prepare(
                &source.file,
                field.span.clone(),
                key,
                body.bytes,
                cancel,
            )
            .ok()
        });
        check_cancel(cancel)?;
    }
    let native = window.native.as_ref().filter(|native| native.key == key);
    if native.is_none()
        && body.bytes > CONTENT_BYTES
        && !window.stage_unavailable
        && window
            .staged
            .as_ref()
            .is_none_or(|staged| staged.key != key)
    {
        window.staged.take();
        window.staged = large
            .stage(source, window, body, field.as_ref(), cancel)
            .ok();
        window.stage_unavailable = window.staged.is_none();
        check_cancel(cancel)?;
    }
    if let Some(native) = native {
        matches.scan_native(&source.file, native, cancel)?;
    } else if let Some(staged) = window.staged.as_ref().filter(|staged| staged.key == key) {
        matches.scan_body(&staged.file, cancel)?;
    } else if let Some(decoded) = field
        .as_ref()
        .filter(|field| field.string && body.bytes <= CONTENT_BYTES)
        .and_then(|field| native_string::small(&source.file, field.span.clone(), body.bytes).ok())
    {
        matches.bytes = decoded.into_bytes();
        matches.complete_body()?;
    } else {
        if *repeated_bytes >= 16 * 1024 * 1024 {
            matches.next = Some(SearchPoint {
                byte: matches.boundary,
                ..matches.point
            });
            return Ok(());
        }
        let input = if let Some(field) = field {
            field.span
        } else {
            span
        };
        *repeated_bytes += input.end - input.start;
        let projected = if body.native_value.is_some() {
            stream_value(lines.input(input.start, input.end), matches)
                .is_ok_and(|bytes| bytes == body.bytes)
        } else {
            project_body(
                source,
                &window.record.id,
                body,
                lines.input(input.start, input.end),
                matches,
            )
        };
        if matches.next.is_none() && !matches.stopped {
            if !projected || matches.decoded + matches.bytes.len() as u64 != body.bytes {
                matches.partial = true;
                return Ok(());
            }
            matches.complete_body()?;
        }
    }
    Ok(())
}

fn project_body(
    source: &Source,
    record: &NativeInputId,
    body: &Body,
    input: impl Read,
    output: &mut dyn Write,
) -> bool {
    records::stream_body(
        source.provider,
        input,
        &source.native_id,
        source.root_session.as_ref(),
        body.field.clone(),
        output,
    )
    .is_some_and(|current| &current.id == record)
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), AppError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(AppError::Unavailable);
    }
    Ok(())
}
