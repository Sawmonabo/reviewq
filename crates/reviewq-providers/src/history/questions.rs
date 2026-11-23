use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;
use reviewq_app::history::SourceAnchor;
use reviewq_app::hosts::{
    ObservedAsyncQuestion, Question, QuestionAnchor, QuestionForm, QuestionKey, QuestionStatus,
};
use reviewq_domain::identity::{NativeInputId, NativeTurnId, ReportHash};
use reviewq_domain::session::HostProvider;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::question_wire::{ASYNC_FORM_BYTES, AsyncQuestions, request_id};

use super::{file, locator::Source, records};

const MAX_FORMS: usize = 8;
const ANSWER_BYTES: u64 = 384 * 1024;
const DISCOVERY_BYTES: u64 = 256 * 1024;
const DISCOVERY_RECORDS: usize = 256;
const NATIVE_LOCATOR_BYTES: u64 = 2 * 1024 * 1024;
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

struct Observation {
    evidence: ObservedAsyncQuestion,
    questions: Vec<Question>,
    end: u64,
}

impl Observation {
    fn located(self, source: &Source, state: QuestionStatus) -> QuestionForm {
        let mut form = self.evidence.form(state);
        let anchor = self.evidence.key.anchor;
        let file = file::file_position(&source.file, anchor.opening, self.end).ok();
        form.locator = file.map(|file| SourceAnchor {
            generation: anchor.generation,
            file,
        });
        form.questions = self.questions;
        form
    }
}

struct ReplyProof {
    opening: u64,
    address: String,
    answered: Option<bool>,
}

impl ReplyProof {
    fn new(key: &QuestionKey) -> Self {
        Self {
            opening: key.anchor.opening,
            address: address(key),
            answered: None,
        }
    }
}

pub(super) fn unverified(evidence: &ObservedAsyncQuestion) -> QuestionForm {
    evidence.form(unverified_state(evidence.state))
}

fn unverified_state(state: QuestionStatus) -> QuestionStatus {
    if state == QuestionStatus::Pending {
        QuestionStatus::Unavailable
    } else {
        state
    }
}

pub(super) fn read(
    source: &Source,
    known: &[ObservedAsyncQuestion],
    request: Option<&str>,
    cancel: &AtomicBool,
) -> Result<Vec<QuestionForm>, AppError> {
    let generation = file::generation(source)?;
    let length = length(source)?;
    let mut lines = file::Lines::new(&source.file, cancel);
    let mut forms = Vec::with_capacity(known.len());
    let mut candidates = Vec::new();
    let mut proofs = Vec::new();
    for evidence in known {
        let form = match exact(source, &mut lines, &generation, length, evidence) {
            Ok(Some(found)) => {
                if matches!(
                    evidence.state,
                    QuestionStatus::Pending
                        | QuestionStatus::Submitting
                        | QuestionStatus::Uncertain
                ) {
                    let can_be_pending = evidence.state == QuestionStatus::Pending
                        && evidence.key.anchor.generation == generation;
                    candidates.push((forms.len(), can_be_pending));
                    proofs.push(ReplyProof::new(&evidence.key));
                }
                found.located(source, unverified_state(evidence.state))
            }
            Ok(None) | Err(_) => unverified(evidence),
        };
        forms.push(form);
    }
    let discover = forms.len() < MAX_FORMS && (request.is_none() || forms.is_empty());
    if !discover && proofs.is_empty() {
        return Ok(forms);
    }
    let complete = match end(source, length, cancel) {
        Ok(complete) => complete,
        Err(_) if !forms.is_empty() => return Ok(forms),
        Err(error) => return Err(error),
    };
    if !proofs.is_empty() {
        let _ = check_replies(source, &mut lines, complete, &mut proofs);
        for ((index, can_be_pending), proof) in candidates.into_iter().zip(proofs) {
            let form = &mut forms[index];
            form.state = match proof.answered {
                Some(true) => QuestionStatus::Answered,
                Some(false) if can_be_pending && complete == length => QuestionStatus::Pending,
                _ => form.state,
            };
        }
    }
    if discover {
        let new = recent(source, cancel, &generation, complete);
        let new = match new {
            Ok(new) => new,
            Err(_) if !forms.is_empty() => return Ok(forms),
            Err(error) => return Err(error),
        };
        for found in new {
            if forms.len() == MAX_FORMS {
                break;
            }
            let key = &found.evidence.key;
            if request.is_none_or(|request| key.request == request)
                && !known.iter().any(|row| row.key.request == key.request)
            {
                let state = unverified_state(found.evidence.state);
                forms.push(found.located(source, state));
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
    let complete = end(source, length(source)?, cancel)?;
    let mut evidence: Vec<_> = recent(source, cancel, &generation, complete)?
        .into_iter()
        .map(|found| found.evidence)
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
        ) && item.key.anchor.generation == generation
    }) {
        let Some(mut found) = exact(source, &mut lines, &generation, complete, item)? else {
            continue;
        };
        if answered_after(source, &mut lines, complete, &item.key).unwrap_or(false) {
            found.evidence.state = QuestionStatus::Answered;
            evidence.push(found.evidence);
        }
    }
    Ok(evidence)
}

pub(super) fn native_locations(
    source: &Source,
    wanted: &[(NativeTurnId, NativeInputId)],
    cancel: &AtomicBool,
) -> Result<HashMap<(NativeTurnId, NativeInputId), SourceAnchor>, AppError> {
    let generation = file::generation(source)?;
    let mut found = HashMap::new();
    let mut budget = NATIVE_LOCATOR_BYTES;
    let mut offset = file::end_of_line(&source.file, length(source)?, &mut budget, cancel)?;
    let mut turn_budget = records::NATIVE_QUESTION_TURN_BYTES;
    let mut lines = file::Lines::new(&source.file, cancel);
    for _ in 0..DISCOVERY_RECORDS {
        if offset == 0 || found.len() == wanted.len() {
            break;
        }
        let Ok(start) = lines.previous(offset - 1, &mut budget) else {
            break;
        };
        if let Some(mut row) = records::project(
            source.provider,
            lines.input(start, offset),
            &source.native_id,
            source.root_session.as_ref(),
            0,
            0,
        ) && row.native_question
            && wanted.iter().any(|(_, item)| *item == row.id)
            && records::bind_native_question(&mut row, source, start, &mut turn_budget, cancel)
            && let Some(turn) = row.turn
            && let Some(key) = wanted.iter().find(|(t, id)| *t == turn && *id == row.id)
            && let Ok(file) = file::file_position(&source.file, start, offset)
        {
            found.entry(key.clone()).or_insert_with(|| SourceAnchor {
                generation: generation.clone(),
                file,
            });
        }
        offset = start;
    }
    Ok(found)
}

pub(super) fn prove(
    source: &Source,
    evidence: &ObservedAsyncQuestion,
    cancel: &AtomicBool,
) -> Result<QuestionForm, AppError> {
    let generation = file::generation(source)?;
    if evidence.state != QuestionStatus::Pending {
        return Err(AppError::PreconditionFailed);
    }
    if evidence.key.anchor.generation != generation {
        return Err(AppError::StaleRevision);
    }
    let complete = length(source)?;
    if !file::boundary(&source.file, complete)? {
        return Err(AppError::Unavailable);
    }
    let mut lines = file::Lines::new(&source.file, cancel);
    let found = exact(source, &mut lines, &generation, complete, evidence)?
        .ok_or(AppError::StaleRevision)?;
    let state = if answered_after(source, &mut lines, complete, &evidence.key)? {
        QuestionStatus::Answered
    } else {
        QuestionStatus::Pending
    };
    Ok(found.located(source, state))
}

fn length(source: &Source) -> Result<u64, AppError> {
    Ok(source
        .file
        .metadata()
        .map_err(|_| AppError::Unavailable)?
        .len())
}

fn end(source: &Source, length: u64, cancel: &AtomicBool) -> Result<u64, AppError> {
    let mut budget = PROOF_BYTES;
    file::end_of_line(&source.file, length, &mut budget, cancel)
}

fn exact(
    source: &Source,
    lines: &mut file::Lines<'_>,
    generation: &str,
    length: u64,
    evidence: &ObservedAsyncQuestion,
) -> Result<Option<Observation>, AppError> {
    let start = evidence.key.anchor.opening;
    if start >= length || !file::boundary(&source.file, start)? {
        return Ok(None);
    }
    let mut budget = ASYNC_FORM_BYTES;
    let end = match lines.next(start, length, &mut budget) {
        Ok(end) => end,
        Err(AppError::PayloadTooLarge) => return Ok(None),
        Err(error) => return Err(error),
    };
    let bytes = raw_record(lines, start, end, ASYNC_FORM_BYTES)?;
    if bytes.len() as u64 != end - start || !bytes.ends_with(b"\n") {
        return Ok(None);
    }
    let Some(found) = project(&bytes, source, generation, start) else {
        return Ok(None);
    };
    // The native opening survives a filesystem move. Compare every persisted
    // property of that opening except the physical generation.
    Ok((found.evidence.key.request == evidence.key.request
        && found.evidence.key.turn == evidence.key.turn
        && found.evidence.key.item == evidence.key.item
        && found.evidence.line_sha256 == evidence.line_sha256
        && found.evidence.occurred_at_ms == evidence.occurred_at_ms)
        .then_some(found))
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
            if offset - start <= ASYNC_FORM_BYTES
                && let Some(mut found) = project(&bytes, source, generation, start)
                && seen.insert(found.evidence.key.request.clone())
            {
                if answers.contains(&address(&found.evidence.key)) {
                    found.evidence.state = QuestionStatus::Answered;
                }
                output.push(found);
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
    let mut proof = ReplyProof::new(key);
    check_replies(source, lines, complete, std::slice::from_mut(&mut proof))?;
    proof.answered.ok_or(AppError::Unavailable)
}

fn check_replies(
    source: &Source,
    lines: &mut file::Lines<'_>,
    complete: u64,
    proofs: &mut [ReplyProof],
) -> Result<(), AppError> {
    let mut offset = complete;
    let mut budget = PROOF_BYTES;
    for _ in 0..PROOF_RECORDS {
        if offset == 0 {
            break;
        }
        let start = lines.previous(offset - 1, &mut budget)?;
        if proofs
            .iter()
            .any(|proof| proof.answered.is_none() && start > proof.opening)
            && offset - start <= ANSWER_BYTES
        {
            let bytes = raw_record(lines, start, offset, ANSWER_BYTES)?;
            if let Some(address) = answer_address(&bytes, source) {
                for proof in proofs.iter_mut() {
                    if proof.answered.is_none() && start > proof.opening && proof.address == address
                    {
                        proof.answered = Some(true);
                    }
                }
            }
        }
        for proof in proofs.iter_mut() {
            if proof.answered.is_none() && start <= proof.opening {
                proof.answered = Some(false);
            }
        }
        if proofs.iter().all(|proof| proof.answered.is_some()) {
            return Ok(());
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
    if source.provider != HostProvider::Codex
        || !bytes.windows(marker.len()).any(|bytes| bytes == marker)
    {
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
    let questions = event.item.questions?.project()?;
    let key = QuestionKey {
        request: request_id(&event.thread_id, &event.turn_id, &event.item.id),
        turn: event.turn_id.parse().ok()?,
        item: event.item.id.parse().ok()?,
        anchor: QuestionAnchor {
            generation: generation.to_owned(),
            opening: start,
        },
    };
    Some(Observation {
        evidence: ObservedAsyncQuestion {
            key,
            line_sha256: ReportHash::from_sha256(Sha256::digest(bytes).into()),
            occurred_at_ms: super::records::date(record.timestamp)?,
            state: QuestionStatus::Pending,
        },
        questions,
        end: start + bytes.len() as u64,
    })
}

#[cfg(test)]
mod tests;
