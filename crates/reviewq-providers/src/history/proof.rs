use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use reviewq_app::access::AppError;
use reviewq_app::history::{HistoryParent, HistorySession};
use reviewq_app::hosts::{QuestionAnchor, QuestionKey, QuestionStatus, answer_digest};
use reviewq_domain::identity::NativeTurnId;
use reviewq_domain::session::{HostProvider, NativeBinding};
use serde::Deserialize;
use struson::reader::{JsonReader, JsonStreamReader, ValueType};

use crate::question_wire::NATIVE_QUESTION_TOOL;

use super::reader::NameBudget;
use super::records::{ID_BYTES, invalid, object, string};
use super::{Cancel, file, locator};

const SEARCH_BYTES: u64 = 16 * 1024 * 1024;

/// The original tool invocation's ordered address in the verified rollout.
pub(crate) async fn codex_opening(
    binding: NativeBinding,
    path: PathBuf,
    call_id: String,
    position: Option<u64>,
) -> Result<(String, u64), AppError> {
    codex_call(binding, None, path, call_id, position, None).await
}

pub(crate) async fn codex_question_opening(
    session: HistorySession,
    path: PathBuf,
    call_id: String,
    turn: NativeTurnId,
) -> Result<QuestionAnchor, AppError> {
    let (generation, opening) = codex_call(
        session.binding,
        session.parent,
        path,
        call_id,
        None,
        Some(turn),
    )
    .await?;
    Ok(QuestionAnchor {
        generation,
        opening,
    })
}

async fn codex_call(
    binding: NativeBinding,
    parent: Option<HistoryParent>,
    path: PathBuf,
    call_id: String,
    position: Option<u64>,
    expected_question_turn: Option<NativeTurnId>,
) -> Result<(String, u64), AppError> {
    if binding.identity().provider() != HostProvider::Codex {
        return Err(AppError::Forbidden);
    }
    let cancel = Cancel(Arc::new(AtomicBool::new(false)));
    let stopped = cancel.0.clone();
    tokio::task::spawn_blocking(move || {
        let source = locator::verify(&binding, parent.as_ref(), &path, &stopped)?
            .ok_or(AppError::Unavailable)?;
        let length = source
            .file
            .metadata()
            .map_err(|_| AppError::Unavailable)?
            .len();
        let mut budget = if expected_question_turn.is_some() {
            file::READ_BUDGET
        } else {
            SEARCH_BYTES
        };
        let name = expected_question_turn
            .as_ref()
            .map(|_| NATIVE_QUESTION_TOOL);
        let offset = if let Some(position) = position {
            if position >= length || !file::boundary(&source.file, position)? {
                return Err(AppError::Conflict);
            }
            let end = file::next_line(&source.file, position, length, &mut budget, &stopped)?;
            if !file::boundary(&source.file, end)?
                || !opening(
                    file::input(&source.file, position, end, &stopped),
                    &call_id,
                    name,
                )?
            {
                return Err(AppError::Conflict);
            }
            position
        } else {
            let mut end = file::end_of_line(&source.file, length, &mut budget, &stopped)?;
            loop {
                if end == 0 {
                    return Err(AppError::Unavailable);
                }
                let start = file::end_of_line(&source.file, end - 1, &mut budget, &stopped)?;
                if opening(
                    file::input(&source.file, start, end, &stopped),
                    &call_id,
                    name,
                )
                .unwrap_or(false)
                {
                    break start;
                }
                end = start;
            }
        };
        if let Some(turn) = expected_question_turn {
            let mut turn_budget = file::READ_BUDGET;
            if super::turn_metadata::turn_before(&source, offset, &mut turn_budget, &stopped)?
                .as_deref()
                != Some(turn.as_str())
            {
                return Err(AppError::Conflict);
            }
        }
        Ok((file::generation(&source)?, offset))
    })
    .await
    .map_err(|_| AppError::Unavailable)?
}

pub(super) fn opening(
    input: impl Read,
    expected: &str,
    expected_tool: Option<&str>,
) -> Result<bool, AppError> {
    let Some(row) = response_call(input)? else {
        return Ok(false);
    };
    Ok(row.call.as_deref() == Some(expected)
        && expected_tool.is_none_or(|name| {
            row.kind.as_deref() == Some("function_call") && row.name.as_deref() == Some(name)
        }))
}

#[derive(Default)]
struct ResponseCall {
    kind: Option<String>,
    call: Option<String>,
    item: Option<String>,
    name: Option<String>,
}

impl ResponseCall {
    fn code_cell(&self) -> bool {
        self.kind.as_deref() == Some("custom_tool_call")
            && matches!(self.name.as_deref(), Some("exec" | "functions.exec"))
    }
}

fn response_call(input: impl Read) -> Result<Option<ResponseCall>, AppError> {
    let mut reader = JsonStreamReader::new(NameBudget::new(input));
    let mut outer = None;
    let mut row = ResponseCall::default();
    object(&mut reader, |name, reader| match name {
        "type" => string(reader, ID_BYTES).map(|value| outer = value),
        "payload" if reader.peek().map_err(invalid)? == ValueType::Object => {
            object(reader, |name, reader| match name {
                "type" => string(reader, ID_BYTES).map(|value| row.kind = value),
                "call_id" => string(reader, ID_BYTES).map(|value| row.call = value),
                "id" => string(reader, ID_BYTES).map(|value| row.item = value),
                "name" => string(reader, ID_BYTES).map(|value| row.name = value),
                _ => reader.skip_value().map_err(invalid),
            })
        }
        _ => reader.skip_value().map_err(invalid),
    })?;
    reader.consume_trailing_whitespace().map_err(invalid)?;
    Ok((outer.as_deref() == Some("response_item")
        && matches!(
            row.kind.as_deref(),
            Some("function_call" | "custom_tool_call" | "local_shell_call")
        ))
    .then_some(row))
}

/// A direct tool has its own persisted opening. A nested tool shares the Code Mode
/// cell's opening and is ordered separately by the host's active turn item index.
pub(crate) async fn codex_mcp_opening(
    binding: NativeBinding,
    path: PathBuf,
    turn: NativeTurnId,
    call: String,
    origin_item: Option<String>,
) -> Result<(String, u64, bool), AppError> {
    let cancel = Cancel(Arc::new(AtomicBool::new(false)));
    let stopped = cancel.0.clone();
    tokio::task::spawn_blocking(move || {
        let source =
            locator::verify(&binding, None, &path, &stopped)?.ok_or(AppError::Unavailable)?;
        let length = source
            .file
            .metadata()
            .map_err(|_| AppError::Unavailable)?
            .len();
        let mut budget = SEARCH_BYTES;
        let mut lines = file::Lines::new(&source.file, &stopped);
        let mut end = lines.previous(length, &mut budget)?;
        while end > 0 {
            let start = lines.previous(end - 1, &mut budget)?;
            if let Some(row) = response_call(lines.input(start, end))? {
                let direct = row.kind.as_deref() == Some("function_call")
                    && row.call.as_deref() == Some(&call);
                let nested = row.code_cell() && row.item.is_some() && row.item == origin_item;
                if direct || nested {
                    if super::turn_metadata::turn_before(&source, start, &mut budget, &stopped)?
                        .as_deref()
                        != Some(turn.as_str())
                    {
                        return Err(AppError::Conflict);
                    }
                    return Ok((file::generation(&source)?, start, nested));
                }
            }
            end = start;
        }
        Err(AppError::NotFound)
    })
    .await
    .map_err(|_| AppError::Unavailable)?
}

pub(super) fn code_cell_opening(input: impl Read) -> Result<bool, AppError> {
    Ok(response_call(input)?.is_some_and(|row| row.code_cell()))
}

pub(crate) async fn codex_question_answered(
    session: HistorySession,
    path: PathBuf,
    key: QuestionKey,
    digest: Option<[u8; 32]>,
) -> Result<Option<QuestionStatus>, AppError> {
    if session.binding.identity().provider() != HostProvider::Codex {
        return Err(AppError::Forbidden);
    }
    let cancel = Cancel(Arc::new(AtomicBool::new(false)));
    let stopped = cancel.0.clone();
    tokio::task::spawn_blocking(move || {
        let source = locator::verify(&session.binding, session.parent.as_ref(), &path, &stopped)?
            .ok_or(AppError::Unavailable)?;
        let file = &source.file;
        let length = file.metadata().map_err(|_| AppError::Unavailable)?.len();
        let mut budget = file::READ_BUDGET;
        if file::generation(&source)? != key.anchor.generation
            || key.anchor.opening >= length
            || !file::boundary(file, key.anchor.opening)?
        {
            return Err(AppError::CursorExpired);
        }
        let mut lines = file::Lines::new(file, &stopped);
        let mut start = lines.next(key.anchor.opening, length, &mut budget)?;
        if !file::boundary(file, start)?
            || !opening(
                lines.input(key.anchor.opening, start),
                key.item.as_str(),
                Some(NATIVE_QUESTION_TOOL),
            )?
        {
            return Err(AppError::CursorExpired);
        }
        let mut ended = false;
        let mut remaining_after_end = 256_u16;
        for _ in 0..file::MAX_RECORDS {
            if start == length || remaining_after_end == 0 {
                break;
            }
            if ended {
                remaining_after_end -= 1;
            }
            let end = match lines.next(start, length, &mut budget) {
                Err(AppError::PayloadTooLarge) if ended => break,
                result => result?,
            };
            if end == length && !file::boundary(file, end)? {
                break;
            }
            match answer(lines.input(start, end), &key, digest, !ended) {
                Ok(Some(QuestionStatus::Answered)) => return Ok(Some(QuestionStatus::Answered)),
                Ok(Some(QuestionStatus::Closed)) => ended = true,
                _ => {}
            }
            start = end;
        }
        Ok(ended.then_some(QuestionStatus::Closed))
    })
    .await
    .map_err(|_| AppError::Unavailable)?
}

fn answer(
    input: impl Read,
    key: &QuestionKey,
    expected_digest: Option<[u8; 32]>,
    allow_implicit_turn: bool,
) -> Result<Option<QuestionStatus>, AppError> {
    let mut reader = JsonStreamReader::new(NameBudget::new(input));
    let (mut outer, mut kind, mut call, mut turn, mut output) = (None, None, None, None, None);
    object(&mut reader, |name, reader| match name {
        "type" => string(reader, ID_BYTES).map(|value| outer = value),
        "payload" if reader.peek().map_err(invalid)? == ValueType::Object => {
            object(reader, |name, reader| match name {
                "type" => string(reader, ID_BYTES).map(|value| kind = value),
                "call_id" => string(reader, ID_BYTES).map(|value| call = value),
                "turn_id" => string(reader, ID_BYTES).map(|value| turn = value),
                "output"
                    if expected_digest.is_some()
                        && reader.peek().map_err(invalid)? == ValueType::String =>
                {
                    // Keep only a bounded decoded field. Skipping the rest still verifies
                    // the frame's exact native call/turn ownership.
                    small_output(reader).map(|value| output = value)
                }
                _ => reader.skip_value().map_err(invalid),
            })
        }
        _ => reader.skip_value().map_err(invalid),
    })?;
    reader.consume_trailing_whitespace().map_err(invalid)?;
    if outer.as_deref() == Some("event_msg")
        && match kind.as_deref() {
            Some("task_complete" | "turn_complete" | "turn_aborted") => {
                turn.as_deref() == Some(key.turn.as_str())
            }
            Some("task_started" | "turn_started") => turn
                .as_deref()
                .is_some_and(|turn| turn != key.turn.as_str()),
            _ => false,
        }
    {
        return Ok(Some(QuestionStatus::Closed));
    }
    if outer.as_deref() != Some("response_item")
        || kind.as_deref() != Some("function_call_output")
        || call.as_deref() != Some(key.item.as_str())
        || match turn.as_deref() {
            Some(turn) => turn != key.turn.as_str(),
            None => !allow_implicit_turn,
        }
    {
        return Ok(None);
    }
    #[derive(Deserialize)]
    struct ResultPayload {
        answers: BTreeMap<String, ResponseValues>,
    }
    #[derive(Deserialize)]
    struct ResponseValues {
        answers: Vec<String>,
    }
    let Some(result) = output
        .as_deref()
        .and_then(|value| serde_json::from_slice::<ResultPayload>(value).ok())
    else {
        return Ok(Some(QuestionStatus::Closed));
    };
    let normalized = result
        .answers
        .into_iter()
        .map(|(id, value)| (id, value.answers))
        .collect::<BTreeMap<_, _>>();
    Ok(Some(
        if answer_digest(&normalized).ok() == expected_digest && expected_digest.is_some() {
            QuestionStatus::Answered
        } else {
            QuestionStatus::Closed
        },
    ))
}

fn small_output<R: Read>(reader: &mut JsonStreamReader<R>) -> Result<Option<Vec<u8>>, AppError> {
    const MAX: u64 = 68 * 1024;
    let mut value = reader.next_string_reader().map_err(invalid)?;
    let mut bytes = Vec::new();
    (&mut value)
        .take(MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(invalid)?;
    std::io::copy(&mut value, &mut std::io::sink()).map_err(invalid)?;
    Ok((bytes.len() as u64 <= MAX).then_some(bytes))
}
