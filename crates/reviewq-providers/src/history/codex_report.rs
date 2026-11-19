use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;
use reviewq_app::history::{FileContentRef, NativeTerminalFailure};
use reviewq_app::workflow::{NativeCall, ReportContent, ReportFailureAddress};
use reviewq_domain::identity::NativeInputId;
use sha2::{Digest, Sha256};
use struson::reader::{JsonReader, JsonStreamReader, ValueType};

use super::records::{ID_BYTES, invalid, object, string};
use super::{Cancel, content, file, locator, proof, reader::NameBudget};

pub(crate) struct CodexTerminalReport {
    pub normal: bool,
    pub response: Option<Arc<dyn ReportContent>>,
}

pub(crate) async fn codex_report_terminal(
    call: NativeCall,
    path: PathBuf,
    include_response: bool,
) -> Result<Option<CodexTerminalReport>, AppError> {
    let cancel = Cancel(Arc::new(AtomicBool::new(false)));
    let stopped = cancel.0.clone();
    let Some((source, normal, response)) = tokio::task::spawn_blocking(move || {
        let source = locator::verify(call.lead.binding(), None, &path, &stopped)?
            .ok_or(AppError::Unavailable)?;
        let outcome = terminal(&source, &call, include_response, &stopped)?;
        Ok::<_, AppError>(outcome.map(|(normal, response)| (source, normal, response)))
    })
    .await
    .map_err(|_| AppError::Unavailable)??
    else {
        return Ok(None);
    };
    let response = match response {
        Some((reference, digest)) => {
            Some(content::report_content(source, reference, digest).await?)
        }
        None => None,
    };
    Ok(Some(CodexTerminalReport { normal, response }))
}

pub(crate) async fn codex_earlier_report_failures(
    call: NativeCall,
    path: PathBuf,
    mut candidates: Vec<ReportFailureAddress>,
) -> Result<Vec<ReportFailureAddress>, AppError> {
    let cancel = Cancel(Arc::new(AtomicBool::new(false)));
    let stopped = cancel.0.clone();
    tokio::task::spawn_blocking(move || {
        let source = locator::verify(call.lead.binding(), None, &path, &stopped)?
            .ok_or(AppError::Unavailable)?;
        if file::generation(&source)? != call.source_generation
            || !file::boundary(&source.file, call.source_position)?
        {
            return Err(AppError::StaleRevision);
        }
        let mut matched = Vec::new();
        let mut offset = call.source_position;
        let mut budget = 64 * 1024 * 1024;
        let mut lines = file::Lines::new(&source.file, &stopped);
        for _ in 0..16_384 {
            if offset == 0 || candidates.is_empty() {
                break;
            }
            let start = match lines.previous(offset - 1, &mut budget) {
                Ok(start) => start,
                Err(AppError::PayloadTooLarge) => break,
                Err(error) => return Err(error),
            };
            let record = event(lines.input(start, offset))?;
            offset = start;
            if record.kind.as_deref() != Some("item_completed")
                || record.thread.as_deref() != Some(source.native_id.as_str())
                || record.item.restricted
                || !(record.item.kind.as_deref() == Some("CommandExecution")
                    || (record.item.kind.as_deref() == Some("AgentMessage")
                        && record.item.phase.as_deref() == Some("final_answer")))
            {
                continue;
            }
            candidates.retain(|candidate| {
                let is_exact = record.turn.as_deref() == Some(candidate.turn.as_str())
                    && record.item.id.as_deref() == Some(candidate.item.as_str());
                if is_exact {
                    matched.push(candidate.clone());
                }
                !is_exact
            });
        }
        Ok(matched)
    })
    .await
    .map_err(|_| AppError::Unavailable)?
}

/// Inspect the last lifecycle boundary in the verified Codex transcript. A
/// more recent start, normal completion or abort supersedes an older failure.
pub(super) fn latest_failure(
    source: &locator::Source,
    high_water: u64,
    cancelled: &AtomicBool,
) -> Option<NativeTerminalFailure> {
    let mut remaining = 8 * 1024 * 1024;
    let mut lines = file::Lines::new(&source.file, cancelled);
    let mut end = high_water;
    for _ in 0..256 {
        if end == 0 {
            break;
        }
        let start = lines.previous(end - 1, &mut remaining).ok()?;
        let item = event(lines.input(start, end)).ok()?;
        end = start;
        if item
            .thread
            .as_deref()
            .is_some_and(|thread| thread != source.native_id.as_str())
        {
            continue;
        }
        match item.kind.as_deref() {
            Some("task_complete" | "turn_complete") => {
                return item.typed_error.then_some(NativeTerminalFailure {
                    turn: item.turn?.parse().ok()?,
                    completed_at_ms: item.completed_at_ms?,
                });
            }
            Some("task_started" | "turn_started" | "turn_aborted") => return None,
            _ => {}
        }
    }
    None
}

type FinalResponse = (FileContentRef, [u8; 32]);

fn terminal(
    source: &locator::Source,
    call: &NativeCall,
    include_response: bool,
    cancelled: &AtomicBool,
) -> Result<Option<(bool, Option<FinalResponse>)>, AppError> {
    let generation = file::generation(source)?;
    let length = source
        .file
        .metadata()
        .map_err(|_| AppError::Unavailable)?
        .len();
    let mut budget = file::READ_BUDGET;
    if generation != call.source_generation
        || call.source_position >= length
        || !file::boundary(&source.file, call.source_position)?
    {
        return Err(AppError::Conflict);
    }
    let mut lines = file::Lines::new(&source.file, cancelled);
    let mut offset = lines.next(call.source_position, length, &mut budget)?;
    if !file::boundary(&source.file, offset)?
        || !proof::opening(
            lines.input(call.source_position, offset),
            &call.call_id,
            None,
        )?
    {
        return Err(AppError::Conflict);
    }
    let mut result_seen = false;
    let mut response = None;
    let turn = call.turn_id.as_str();
    for _ in 0..file::MAX_RECORDS {
        if offset == length {
            return Ok(None);
        }
        let start = offset;
        offset = lines.next(start, length, &mut budget)?;
        if offset == length && !file::boundary(&source.file, offset)? {
            return Ok(None);
        }
        let event = event(lines.input(start, offset))?;
        if event.turn.as_deref() != Some(turn) {
            continue;
        }
        match event.kind.as_deref() {
            Some("task_complete" | "turn_complete" | "turn_aborted") => {
                if let Some(root) = &call.root_turn_id
                    && event.root.as_deref() != Some(root.as_str())
                {
                    return Err(AppError::Conflict);
                }
                let normal = event.kind.as_deref() != Some("turn_aborted") && !event.has_error;
                let response = response
                    .filter(|_| normal)
                    .map(|(entry, start, end)| {
                        let reference = FileContentRef {
                            generation,
                            entry,
                            part: None,
                            position: file::file_position(&source.file, start, end)?,
                        };
                        Ok::<_, AppError>((reference, hash_record(lines.input(start, end))?))
                    })
                    .transpose()?;
                return Ok(Some((normal, response)));
            }
            Some("exec_command_end") if event.call.as_deref() == Some(call.call_id.as_str()) => {
                result_seen = true;
            }
            Some("item_completed")
                if event.thread.as_deref() == Some(source.native_id.as_str()) =>
            {
                match event.item.kind.as_deref() {
                    Some("CommandExecution")
                        if event.item.id.as_deref() == Some(call.call_id.as_str()) =>
                    {
                        result_seen = true;
                    }
                    Some("UserMessage") => return Err(AppError::Conflict),
                    Some("AgentMessage")
                        if include_response
                            && result_seen
                            && event.item.phase.as_deref() == Some("final_answer") =>
                    {
                        response = if !event.item.restricted {
                            event
                                .item
                                .id
                                .and_then(|id| id.parse::<NativeInputId>().ok())
                                .map(|entry| (entry, start, offset))
                        } else {
                            None
                        };
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    // Require the exact terminal event within this native scan budget.
    Err(AppError::PayloadTooLarge)
}

#[derive(Default)]
struct Event {
    kind: Option<String>,
    thread: Option<String>,
    turn: Option<String>,
    root: Option<String>,
    call: Option<String>,
    has_error: bool,
    typed_error: bool,
    completed_at_ms: Option<i64>,
    item: Item,
}

#[derive(Default)]
struct Item {
    kind: Option<String>,
    id: Option<String>,
    phase: Option<String>,
    restricted: bool,
}

fn event(input: impl Read) -> Result<Event, AppError> {
    let mut reader = JsonStreamReader::new(NameBudget::new(input));
    let mut outer = None;
    let mut result = Event::default();
    object(&mut reader, |name, reader| match name {
        "type" => string(reader, ID_BYTES).map(|value| outer = value),
        "payload" if reader.peek().map_err(invalid)? == ValueType::Object => {
            object(reader, |name, reader| match name {
                "type" => string(reader, ID_BYTES).map(|value| result.kind = value),
                "thread_id" => string(reader, ID_BYTES).map(|value| result.thread = value),
                "turn_id" => string(reader, ID_BYTES).map(|value| result.turn = value),
                "root_turn_id" => string(reader, ID_BYTES).map(|value| result.root = value),
                "call_id" => string(reader, ID_BYTES).map(|value| result.call = value),
                "error" => {
                    let kind = reader.peek().map_err(invalid)?;
                    result.has_error = kind != ValueType::Null;
                    result.typed_error = kind == ValueType::Object;
                    reader.skip_value().map_err(invalid)
                }
                "completed_at" if reader.peek().map_err(invalid)? == ValueType::Number => {
                    result.completed_at_ms = reader
                        .next_number_as_string()
                        .map_err(invalid)?
                        .parse::<i64>()
                        .ok()
                        .filter(|time| *time > 0)
                        .and_then(|time| time.checked_mul(1000));
                    Ok(())
                }
                "item" if reader.peek().map_err(invalid)? == ValueType::Object => {
                    object(reader, |name, reader| match name {
                        "type" => string(reader, ID_BYTES).map(|value| result.item.kind = value),
                        "id" => string(reader, ID_BYTES).map(|value| result.item.id = value),
                        "phase" => string(reader, ID_BYTES).map(|value| result.item.phase = value),
                        "moderation" | "delivery" => {
                            non_null(reader).map(|value| result.item.restricted |= value)
                        }
                        _ => reader.skip_value().map_err(invalid),
                    })
                }
                _ => reader.skip_value().map_err(invalid),
            })
        }
        _ => reader.skip_value().map_err(invalid),
    })?;
    reader.consume_trailing_whitespace().map_err(invalid)?;
    if outer.as_deref() != Some("event_msg") {
        result.kind = None;
    }
    Ok(result)
}

fn non_null<R: Read>(reader: &mut JsonStreamReader<R>) -> Result<bool, AppError> {
    let non_null = reader.peek().map_err(invalid)? != ValueType::Null;
    reader.skip_value().map_err(invalid)?;
    Ok(non_null)
}

fn hash_record(mut input: impl Read) -> Result<[u8; 32], AppError> {
    let mut digest = Sha256::new();
    let mut buffer = [0; 32 * 1024];
    loop {
        let used = input.read(&mut buffer).map_err(|_| AppError::Unavailable)?;
        if used == 0 {
            return Ok(digest.finalize().into());
        }
        digest.update(&buffer[..used]);
    }
}
