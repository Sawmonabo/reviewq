use std::io::Read;
use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;
use reviewq_app::hosts::{ObservedAsyncQuestion, QuestionForm, QuestionMode, QuestionStatus};
use reviewq_domain::identity::ReportHash;
use reviewq_domain::session::HostProvider;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::question_wire::{NativeQuestion, project_questions, request_id};

use super::{file, locator::Source};

const MAX_FORMS: usize = 8;
const RECORD_BYTES: u64 = 48 * 1024;
const DISCOVERY_BYTES: u64 = 256 * 1024;
const DISCOVERY_RECORDS: usize = 256;

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: String,
    payload: Event,
}

#[derive(Deserialize)]
struct Event {
    #[serde(rename = "type")]
    kind: String,
    thread_id: String,
    turn_id: String,
    item: Item,
}

#[derive(Deserialize)]
struct Item {
    #[serde(rename = "type")]
    kind: String,
    id: String,
    delivery: String,
    moderation: Option<serde::de::IgnoredAny>,
    questions: Vec<NativeQuestion>,
}

pub(super) fn read(
    source: &Source,
    known: Vec<ObservedAsyncQuestion>,
    request: Option<&str>,
    cancel: &AtomicBool,
) -> Result<Vec<QuestionForm>, AppError> {
    if source.provider != HostProvider::Codex {
        return Ok(Vec::new());
    }
    let generation = file::generation(source)?;
    let length = source
        .file
        .metadata()
        .map_err(|_| AppError::Unavailable)?
        .len();
    let mut budget = DISCOVERY_BYTES;
    let complete = match file::end_of_line(&source.file, length, &mut budget, cancel) {
        Ok(complete) => complete,
        Err(AppError::PayloadTooLarge) => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut lines = file::Lines::new(&source.file, cancel);
    let mut forms = Vec::with_capacity(known.len());
    for evidence in &known {
        let start = evidence.key.anchor.opening;
        if evidence.key.anchor.generation != generation
            || start >= complete
            || !file::boundary(&source.file, start)?
        {
            continue;
        }
        let mut budget = RECORD_BYTES;
        let end = match lines.next(start, complete, &mut budget) {
            Ok(end) => end,
            Err(AppError::PayloadTooLarge) => continue,
            Err(error) => return Err(error),
        };
        let bytes = raw_record(&lines, start, end)?;
        if ReportHash::from_sha256(Sha256::digest(&bytes).into()) != evidence.line_sha256 {
            continue;
        }
        if let Some(mut form) = project(&bytes, source)
            && form.request == evidence.key.request
            && form.turn == evidence.key.turn
            && form.item == evidence.key.item
        {
            form.state = evidence.state;
            forms.push(form);
        }
    }
    // Raw native history proves the question text and identity. Pending state requires
    // independent lifecycle evidence before the projection can show it as pending.
    let mut offset = complete;
    for _ in 0..DISCOVERY_RECORDS {
        if offset == 0 || forms.len() >= MAX_FORMS || (request.is_some() && !forms.is_empty()) {
            break;
        }
        let start = match lines.previous(offset - 1, &mut budget) {
            Ok(start) => start,
            Err(AppError::PayloadTooLarge) => break,
            Err(error) => return Err(error),
        };
        if offset - start <= RECORD_BYTES {
            let bytes = raw_record(&lines, start, offset)?;
            if let Some(form) = project(&bytes, source)
                && request.is_none_or(|request| form.request == request)
                && !known.iter().any(|row| row.key.request == form.request)
                && !forms.iter().any(|prior| prior.request == form.request)
            {
                forms.push(form);
            }
        }
        offset = start;
    }
    Ok(forms)
}

fn raw_record(lines: &file::Lines<'_>, start: u64, end: u64) -> Result<Vec<u8>, AppError> {
    let mut bytes = Vec::new();
    lines
        .input(start, end)
        .take(RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AppError::Unavailable)?;
    Ok(bytes)
}

fn project(bytes: &[u8], source: &Source) -> Option<QuestionForm> {
    if !bytes
        .windows(b"AgentMessage".len())
        .any(|bytes| bytes == b"AgentMessage")
    {
        return None;
    }
    let record: Record = serde_json::from_slice(bytes).ok()?;
    let event = record.payload;
    if record.kind != "event_msg"
        || event.kind != "item_completed"
        || event.thread_id != source.native_id.as_str()
        || event.item.kind != "AgentMessage"
        || event.item.delivery != "async"
        || event.item.moderation.is_some()
    {
        return None;
    }
    Some(QuestionForm {
        request: request_id(&event.thread_id, &event.turn_id, &event.item.id),
        turn: event.turn_id.parse().ok()?,
        item: event.item.id.parse().ok()?,
        blocking: false,
        mode: QuestionMode::Async,
        state: QuestionStatus::Unavailable,
        questions: project_questions(event.item.questions)?,
    })
}
