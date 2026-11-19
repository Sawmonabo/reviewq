use std::collections::HashSet;
use std::io::Read;
use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;
use reviewq_app::hosts::{
    ObservedAsyncQuestion, Question, QuestionAnchor, QuestionForm, QuestionKey, QuestionMode,
    QuestionOption, QuestionStatus,
};
use reviewq_domain::identity::ReportHash;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::question_wire::{NativeQuestion, project_questions, request_id, visible};

use super::{file, locator::Source};

const MAX_FORMS: usize = 8;
const FORM_BYTES: u64 = 48 * 1024;
const ANSWER_BYTES: u64 = 384 * 1024;
const DISCOVERY_BYTES: u64 = 256 * 1024;
const DISCOVERY_RECORDS: usize = 256;
const PROOF_BYTES: u64 = 64 * 1024 * 1024;
const PROOF_RECORDS: usize = 16_384;
const ANSWER_PREFIX: &str = "Answer to Codex question (";

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: String,
    timestamp: Option<String>,
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
    delivery: Option<String>,
    moderation: Option<serde::de::IgnoredAny>,
    questions: Option<AsyncQuestions>,
    #[serde(default)]
    content: Vec<Content>,
}

#[derive(Deserialize)]
struct Content {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AsyncQuestions {
    Titled(Vec<TitledQuestion>),
    Structured(Vec<NativeQuestion>),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TitledQuestion {
    title: String,
    options: Option<Vec<String>>,
    #[serde(default, alias = "is_secret")]
    is_secret: bool,
}

type Observation = (ObservedAsyncQuestion, QuestionForm);

pub(super) fn read(
    source: &Source,
    known: &[ObservedAsyncQuestion],
    request: Option<&str>,
    cancel: &AtomicBool,
) -> Result<Vec<QuestionForm>, AppError> {
    let generation = file::generation(source)?;
    let complete = end(source, cancel)?;
    let mut lines = file::Lines::new(&source.file, cancel);
    let mut forms = Vec::with_capacity(known.len());
    for evidence in known {
        let form = exact(source, &mut lines, &generation, complete, evidence)?.map_or_else(
            || evidence.form(QuestionStatus::Unavailable),
            |(_, form)| form,
        );
        forms.push(form);
    }
    if forms.len() < MAX_FORMS && (request.is_none() || forms.is_empty()) {
        let new = recent(source, cancel, &generation, complete)?;
        for (evidence, form) in new {
            if forms.len() == MAX_FORMS {
                break;
            }
            if request.is_none_or(|request| form.request == request)
                && !known
                    .iter()
                    .any(|row| row.key.request == evidence.key.request)
            {
                forms.push(QuestionForm {
                    state: if evidence.state == QuestionStatus::Answered {
                        QuestionStatus::Answered
                    } else {
                        QuestionStatus::Unavailable
                    },
                    ..form
                });
            }
        }
    }
    Ok(forms)
}

pub(super) fn observe(
    source: &Source,
    started_at_ms: i64,
    known: &[ObservedAsyncQuestion],
    cancel: &AtomicBool,
) -> Result<Vec<ObservedAsyncQuestion>, AppError> {
    let generation = file::generation(source)?;
    let complete = end(source, cancel)?;
    let mut evidence: Vec<_> = recent(source, cancel, &generation, complete)?
        .into_iter()
        .map(|(evidence, _)| evidence)
        .filter(|evidence| evidence.occurred_at_ms >= started_at_ms)
        .filter(|evidence| {
            !known
                .iter()
                .any(|row| row.key.request == evidence.key.request)
        })
        .collect();
    let mut lines = file::Lines::new(&source.file, cancel);
    for item in known.iter().filter(|item| {
        matches!(
            item.state,
            QuestionStatus::Pending | QuestionStatus::Submitting | QuestionStatus::Uncertain
        )
    }) {
        let Some((mut verified, _)) = exact(source, &mut lines, &generation, complete, item)?
        else {
            continue;
        };
        if answered_after(source, &mut lines, complete, &item.key).unwrap_or(false) {
            verified.state = QuestionStatus::Answered;
            evidence.push(verified);
        }
    }
    Ok(evidence)
}

pub(super) fn prove(
    source: &Source,
    evidence: &ObservedAsyncQuestion,
    cancel: &AtomicBool,
) -> Result<QuestionForm, AppError> {
    let generation = file::generation(source)?;
    let complete = end(source, cancel)?;
    let mut lines = file::Lines::new(&source.file, cancel);
    let (_, mut form) = exact(source, &mut lines, &generation, complete, evidence)?
        .ok_or(AppError::StaleRevision)?;
    if form.state == QuestionStatus::Pending
        && answered_after(source, &mut lines, complete, &evidence.key)?
    {
        form.state = QuestionStatus::Answered;
    }
    Ok(form)
}

fn end(source: &Source, cancel: &AtomicBool) -> Result<u64, AppError> {
    let length = source
        .file
        .metadata()
        .map_err(|_| AppError::Unavailable)?
        .len();
    let mut budget = PROOF_BYTES;
    file::end_of_line(&source.file, length, &mut budget, cancel)
}

fn exact(
    source: &Source,
    lines: &mut file::Lines<'_>,
    generation: &str,
    complete: u64,
    evidence: &ObservedAsyncQuestion,
) -> Result<Option<Observation>, AppError> {
    let start = evidence.key.anchor.opening;
    if evidence.key.anchor.generation != generation
        || start >= complete
        || !file::boundary(&source.file, start)?
    {
        return Ok(None);
    }
    let mut budget = FORM_BYTES;
    let end = match lines.next(start, complete, &mut budget) {
        Ok(end) => end,
        Err(AppError::PayloadTooLarge) => return Ok(None),
        Err(error) => return Err(error),
    };
    let bytes = raw_record(lines, start, end, FORM_BYTES)?;
    let Some((mut observed, mut form)) = project(&bytes, source, generation, start) else {
        return Ok(None);
    };
    if observed.key != evidence.key
        || observed.line_sha256 != evidence.line_sha256
        || observed.occurred_at_ms != evidence.occurred_at_ms
    {
        return Ok(None);
    }
    observed.state = evidence.state;
    form.state = evidence.state;
    Ok(Some((observed, form)))
}

fn recent(
    source: &Source,
    cancel: &AtomicBool,
    generation: &str,
    complete: u64,
) -> Result<Vec<Observation>, AppError> {
    let mut output = Vec::new();
    let mut answers = HashSet::new();
    let mut seen = HashSet::new();
    let mut offset = complete;
    let mut budget = DISCOVERY_BYTES;
    let mut lines = file::Lines::new(&source.file, cancel);
    for _ in 0..DISCOVERY_RECORDS {
        if offset == 0 || output.len() == MAX_FORMS {
            break;
        }
        let start = match lines.previous(offset - 1, &mut budget) {
            Ok(start) => start,
            Err(AppError::PayloadTooLarge) => break,
            Err(error) => return Err(error),
        };
        if offset - start <= ANSWER_BYTES {
            let bytes = raw_record(&lines, start, offset, ANSWER_BYTES)?;
            if let Some(address) = answer_address(&bytes, source) {
                answers.insert(address);
            }
            if offset - start <= FORM_BYTES
                && let Some((mut evidence, mut form)) = project(&bytes, source, generation, start)
                && seen.insert(form.request.clone())
            {
                if answers.contains(&address(&evidence.key)) {
                    evidence.state = QuestionStatus::Answered;
                    form.state = QuestionStatus::Answered;
                }
                output.push((evidence, form));
            }
        }
        offset = start;
    }
    Ok(output)
}

fn answered_after(
    source: &Source,
    lines: &mut file::Lines<'_>,
    complete: u64,
    key: &QuestionKey,
) -> Result<bool, AppError> {
    let address = address(key);
    let mut offset = complete;
    let mut budget = PROOF_BYTES;
    for _ in 0..PROOF_RECORDS {
        if offset <= key.anchor.opening {
            return Ok(false);
        }
        let start = lines.previous(offset - 1, &mut budget)?;
        if start <= key.anchor.opening {
            return Ok(false);
        }
        if offset - start <= ANSWER_BYTES {
            let bytes = raw_record(lines, start, offset, ANSWER_BYTES)?;
            if answer_address(&bytes, source).as_deref() == Some(address.as_str()) {
                return Ok(true);
            }
        }
        offset = start;
    }
    Err(AppError::Unavailable)
}

fn answer_address(bytes: &[u8], source: &Source) -> Option<String> {
    let record = event(bytes, source, b"UserMessage")?;
    if record.payload.item.kind != "UserMessage" {
        return None;
    }
    let [input] = record.payload.item.content.as_slice() else {
        return None;
    };
    if input.kind != "text" {
        return None;
    }
    let (address, body) = input
        .text
        .as_deref()?
        .strip_prefix(ANSWER_PREFIX)?
        .split_once("):\n\n")?;
    (!address.is_empty() && address.len() <= 513 && !body.trim().is_empty())
        .then(|| address.to_owned())
}

fn address(key: &QuestionKey) -> String {
    format!("{}:{}", key.turn, key.item)
}

fn raw_record(
    lines: &file::Lines<'_>,
    start: u64,
    end: u64,
    limit: u64,
) -> Result<Vec<u8>, AppError> {
    let mut bytes = Vec::new();
    lines
        .input(start, end)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| AppError::Unavailable)?;
    if bytes.len() as u64 > limit {
        return Err(AppError::PayloadTooLarge);
    }
    Ok(bytes)
}

fn event(bytes: &[u8], source: &Source, marker: &[u8]) -> Option<Record> {
    if !bytes.windows(marker.len()).any(|bytes| bytes == marker) {
        return None;
    }
    let record: Record = serde_json::from_slice(bytes).ok()?;
    (record.kind == "event_msg"
        && record.payload.kind == "item_completed"
        && record.payload.thread_id == source.native_id.as_str()
        && !record.payload.turn_id.is_empty()
        && record.payload.item.moderation.is_none())
    .then_some(record)
}

fn project(bytes: &[u8], source: &Source, generation: &str, start: u64) -> Option<Observation> {
    let record = event(bytes, source, b"AgentMessage")?;
    let event = record.payload;
    if event.item.kind != "AgentMessage" || event.item.delivery.as_deref() != Some("async") {
        return None;
    }
    let questions = match event.item.questions? {
        AsyncQuestions::Titled(values) => project_titled(values)?,
        AsyncQuestions::Structured(values) => project_questions(values)?,
    };
    let key = QuestionKey {
        request: request_id(&event.thread_id, &event.turn_id, &event.item.id),
        turn: event.turn_id.parse().ok()?,
        item: event.item.id.parse().ok()?,
        anchor: QuestionAnchor {
            generation: generation.to_owned(),
            opening: start,
        },
    };
    let form = QuestionForm {
        request: key.request.clone(),
        turn: key.turn.clone(),
        item: key.item.clone(),
        blocking: false,
        mode: QuestionMode::Async,
        state: QuestionStatus::Pending,
        questions,
    };
    Some((
        ObservedAsyncQuestion {
            key,
            line_sha256: ReportHash::from_sha256(Sha256::digest(bytes).into()),
            occurred_at_ms: super::records::date(record.timestamp)?,
            state: form.state,
        },
        form,
    ))
}

fn project_titled(values: Vec<TitledQuestion>) -> Option<Vec<Question>> {
    if values.is_empty() || values.len() > 16 {
        return None;
    }
    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let options = value.options.unwrap_or_default();
            if !visible(&value.title, 8192, true) || options.len() > 32 {
                return None;
            }
            let mut seen = HashSet::new();
            let options = options
                .into_iter()
                .map(|label| {
                    (visible(&label, 1024, false) && seen.insert(label.clone())).then_some(
                        QuestionOption {
                            label,
                            description: String::new(),
                        },
                    )
                })
                .collect::<Option<Vec<_>>>()?;
            Some(Question {
                id: format!("question-{}", index + 1),
                header: String::new(),
                prompt: value.title,
                allows_other: true,
                secret: value.is_secret,
                options,
            })
        })
        .collect()
}
